import type { DeviceIdentity, Invitation, EnrolledDevice, DiscoveredHost, JoinData, ConnectionInfo } from "./types";
import { CLIENT_ERROR, RgitCancelled, RgitError } from "./errors";
import type { RunHandle, Runner } from "./runner";
import type {
  BranchListData,
  BranchRefData,
  CommitData,
  DiffData,
  CloneData,
  EngineEvent,
  FetchData,
  GcData,
  InitData,
  LogData,
  MergeLikeData,
  ProgressEvent,
  PullData,
  PushData,
  ReindexData,
  RemoteData,
  ResolveData,
  ShelvesData,
  ShowData,
  StatsData,
  StatusData,
  SwitchData,
  VerifyData,
  VersionData,
} from "./types";
import { JSON_INTERFACE_VERSION } from "./types";
import { commitArgs, type CommitSelection } from "./selection";

export interface CallOptions {
  project?: string | null;
  /** Receives every NDJSON line that has an "event" key (progress lines and others). */
  onEvent?: (e: EngineEvent) => void;
  onProgress?: (p: ProgressEvent) => void;
  signal?: AbortSignal;
  /** Text for stdin (then closed). */
  stdin?: string;
  /** Called with the handle as soon as the process is started (for cancel buttons). */
  onStart?: (handle: RunHandle) => void;
  /**
   * Some results are `ok:true` with a non-zero exit code by design (merge conflicts exit 1,
   * verify problems exit 2). Return true to accept that for this `data`. Any other non-zero
   * exit with `ok:true` is treated as a failure: the engine's own words are in stderr.
   */
  allowExit?: (data: never, exitCode: number) => boolean;
  /** Error code to report when the process exits with a code and `ok:true` but no data. */
  exitCodeErrors?: Record<number, string>;
}

const hasConflicts = (d: { conflicts?: unknown[] }) => (d.conflicts?.length ?? 0) > 0;

type Parsed =
  | { kind: "event"; value: EngineEvent }
  | { kind: "result"; value: { ok: boolean; data?: unknown; error?: { code: string; message: string; paths?: string[]; connection?: ConnectionInfo } } }
  | { kind: "garbage" };

export function parseLine(line: string): Parsed {
  const trimmed = line.trim();
  if (!trimmed.startsWith("{")) return { kind: "garbage" };
  let v: unknown;
  try {
    v = JSON.parse(trimmed);
  } catch {
    return { kind: "garbage" };
  }
  if (typeof v !== "object" || v === null) return { kind: "garbage" };
  const o = v as Record<string, unknown>;
  if (typeof o.event === "string") return { kind: "event", value: o as EngineEvent };
  if (typeof o.ok === "boolean") return { kind: "result", value: o as never };
  return { kind: "garbage" };
}

/**
 * Typed wrapper over `rgit --json`. It parses NDJSON only; it never looks at human-readable
 * output and holds no repository logic.
 */
export class RgitClient {
  constructor(private readonly runner: Runner) {}

  /**
   * Run one command and return its `data`. The last result line decides: `ok:true` returns
   * `data` whatever the exit code (merge conflicts exit 1 and verify problems exit 2 with
   * `ok:true`); `ok:false` throws RgitError.
   */
  async call<T>(args: string[], opts: CallOptions = {}): Promise<T> {
    if (opts.signal?.aborted) throw new RgitCancelled();
    let result: Extract<Parsed, { kind: "result" }>["value"] | null = null;
    const handle = this.runner.spawn({
      args,
      project: opts.project ?? null,
      stdin: opts.stdin,
      onStdoutLine: (line) => {
        const p = parseLine(line);
        if (p.kind === "event") {
          opts.onEvent?.(p.value);
          if (p.value.event === "progress") opts.onProgress?.(p.value as unknown as ProgressEvent);
        } else if (p.kind === "result") {
          result = p.value;
        }
      },
    });
    opts.onStart?.(handle);
    const onAbort = () => handle.cancel();
    opts.signal?.addEventListener("abort", onAbort, { once: true });
    if (opts.signal?.aborted) handle.cancel();
    let outcome;
    try {
      outcome = await handle.done;
    } catch (e) {
      throw new RgitError(CLIENT_ERROR.spawnFailed, e instanceof Error ? e.message : String(e));
    } finally {
      opts.signal?.removeEventListener("abort", onAbort);
    }
    const stderr = outcome.stderr.join("\n");
    if (outcome.cancelled && result === null) throw new RgitCancelled();
    // Re-narrow: `result` is assigned inside a callback, TypeScript cannot see that.
    const final = result as Extract<Parsed, { kind: "result" }>["value"] | null;
    if (final === null) {
      throw new RgitError(
        CLIENT_ERROR.protocol,
        stderr.trim() || `rgit exited with code ${outcome.exitCode ?? "none"} without a result`,
        [],
        stderr,
      );
    }
    if (!final.ok) {
      const err = final.error ?? { code: "other", message: "unknown error" };
      if (err.connection) opts.onEvent?.({ event: "connection", ...err.connection });
      throw RgitError.fromBody(err, stderr);
    }
    const exit = outcome.exitCode ?? 0;
    if (exit !== 0) {
      const data = (final.data ?? {}) as never;
      if (!opts.allowExit?.(data, exit)) {
        const code = opts.exitCodeErrors?.[exit] ?? CLIENT_ERROR.protocol;
        throw new RgitError(code, stderr.trim() || `rgit exited with code ${exit}`, [], stderr);
      }
    }
    return (final.data ?? {}) as T;
  }

  deviceIdentity(): Promise<DeviceIdentity> { return this.call(["device", "id"]); }
  invitations(project: string): Promise<{ invitations: Invitation[] }> { return this.call(["invite", "list"], { project }); }
  createInvitation(project: string, role: string, expires: string, singleUse: boolean, label: string, hints: string[]): Promise<{ uri: string; id: string }> {
    return this.call(["invite", "create", "--role", role, "--expires", expires, `--single-use=${singleUse}`, "--label", label, ...hints.flatMap(h => ["--hint", h])], { project });
  }
  revokeInvitation(project: string, id: string): Promise<unknown> { return this.call(["invite", "revoke", id], { project }); }
  devices(project: string): Promise<{ devices: EnrolledDevice[] }> { return this.call(["devices", "list"], { project }); }
  revokeDevice(project: string, fingerprint: string): Promise<unknown> { return this.call(["devices", "revoke", fingerprint], { project }); }
  setDeviceRole(project: string, fingerprint: string, role: string): Promise<unknown> { return this.call(["devices", "role", fingerprint, role], { project }); }
  join(invitation: string, dest: string, o: CallOptions = {}): Promise<JoinData> {
    return this.call(["join", "-", dest], { ...o, project: null, stdin: invitation.trim() + "\n" });
  }
  enroll(project: string, invitation: string, remote = "origin"): Promise<JoinData> {
    return this.call(["join", "-", "--add-remote", "--remote", remote], { project, stdin: invitation.trim() + "\n" });
  }
  discover(o: CallOptions = {}): Promise<{ hosts: DiscoveredHost[] }> {
    return this.call(["discover", "--watch"], { ...o, project: null });
  }

  // ----- version ---------------------------------------------------------------------

  version(): Promise<VersionData> {
    return this.call<VersionData>(["version"]);
  }

  /** Throws interface_mismatch unless the bundled rgit speaks the JSON interface we know. */
  async ensureCompatible(): Promise<VersionData> {
    const v = await this.version();
    if (v.json_interface !== JSON_INTERFACE_VERSION) {
      throw new RgitError(
        CLIENT_ERROR.interfaceMismatch,
        `rgit speaks JSON interface ${v.json_interface}, this app needs ${JSON_INTERFACE_VERSION}`,
      );
    }
    return v;
  }

  // ----- project / working tree ------------------------------------------------------

  init(project: string, branch?: string): Promise<InitData> {
    return this.call<InitData>(["init", ...(branch ? ["--branch", branch] : [])], { project });
  }

  status(project: string): Promise<StatusData> {
    return this.call<StatusData>(["status"], { project });
  }

  commit(project: string, message: string, paths: string[] = []): Promise<CommitData> {
    return this.commitSelection(project, message, paths.length ? { kind: "only", paths } : { kind: "all" });
  }

  /** Commit a selection of any size: long lists go through stdin, never the command line. */
  commitSelection(project: string, message: string, selection: CommitSelection): Promise<CommitData> {
    const { args, stdin } = commitArgs(selection);
    return this.call<CommitData>(["commit", "-m", message, ...args], { project, stdin });
  }

  diff(project: string, a?: string, b?: string): Promise<DiffData> {
    return this.call<DiffData>(["diff", ...(a ? [a] : []), ...(b ? [b] : [])], { project });
  }

  log(project: string, limit = 200, rev?: string): Promise<LogData> {
    return this.call<LogData>(["log", "-n", String(limit), ...(rev ? [rev] : [])], { project });
  }

  show(project: string, rev: string): Promise<ShowData> {
    return this.call<ShowData>(["show", rev], { project });
  }

  // ----- branches --------------------------------------------------------------------

  branches(project: string): Promise<BranchListData> {
    return this.call<BranchListData>(["branch"], { project });
  }

  createBranch(project: string, name: string, from?: string): Promise<BranchRefData> {
    return this.call<BranchRefData>(["branch", name, ...(from ? ["--from", from] : [])], { project });
  }

  deleteBranch(project: string, name: string): Promise<BranchRefData> {
    return this.call<BranchRefData>(["branch", "-d", name], { project });
  }

  switchBranch(project: string, name: string, create = false): Promise<SwitchData> {
    return this.call<SwitchData>(["switch", ...(create ? ["-c"] : []), name], { project });
  }

  // ----- restore / revert / merge ----------------------------------------------------

  restore(project: string, paths: string[], from?: string, force = false): Promise<MergeLikeData> {
    return this.call<MergeLikeData>(
      ["restore", ...(from ? ["--from", from] : []), ...(force ? ["--force"] : []), ...paths],
      { project, allowExit: hasConflicts },
    );
  }

  revert(project: string, rev: string): Promise<MergeLikeData> {
    return this.call<MergeLikeData>(["revert", rev], { project, allowExit: hasConflicts });
  }

  merge(project: string, branch: string): Promise<MergeLikeData> {
    return this.call<MergeLikeData>(["merge", branch], { project, allowExit: hasConflicts });
  }

  mergeAbort(project: string): Promise<MergeLikeData> {
    return this.call<MergeLikeData>(["merge", "--abort"], { project, allowExit: hasConflicts });
  }

  /** `how: "edit"` marks the file as resolved as it is on disk (the user edited it). */
  resolve(project: string, path: string, how: "ours" | "theirs" | "edit"): Promise<ResolveData> {
    return this.call<ResolveData>(["resolve", path, ...(how === "edit" ? [] : [`--${how}`])], { project });
  }

  shelve(project: string, message: string): Promise<{ name: string }> {
    return this.call(["shelve", "-m", message], { project });
  }

  shelves(project: string): Promise<ShelvesData> {
    return this.call<ShelvesData>(["shelves"], { project });
  }

  unshelve(project: string, id: string): Promise<MergeLikeData> {
    return this.call<MergeLikeData>(["unshelve", id], { project, allowExit: hasConflicts });
  }

  dropShelf(project: string, id: string): Promise<Record<string, never>> {
    return this.call(["shelf-drop", id], { project });
  }

  // ----- maintenance -----------------------------------------------------------------

  stats(project: string): Promise<StatsData> {
    return this.call<StatsData>(["stats"], { project });
  }

  verify(project: string, o: CallOptions = {}): Promise<VerifyData> {
    return this.call<VerifyData>(["verify"], {
      ...o,
      project,
      allowExit: (d: VerifyData) => d.problems.length > 0,
    });
  }

  gc(project: string, aggressive = false, o: CallOptions = {}): Promise<GcData> {
    return this.call<GcData>(["gc", ...(aggressive ? ["--aggressive"] : [])], { ...o, project });
  }

  reindex(project: string): Promise<ReindexData> {
    return this.call<ReindexData>(["reindex"], { project });
  }

  // ----- remotes and sync ------------------------------------------------------------

  remotes(project: string): Promise<RemoteData> {
    return this.call<RemoteData>(["remote"], { project });
  }

  addRemote(project: string, name: string, url: string, hostKey?: string): Promise<unknown> {
    return this.call(["remote", "add", name, url, ...(hostKey ? ["--host-key", hostKey] : [])], { project });
  }

  clone(url: string, dest: string, o: CallOptions & { hostKey?: string } = {}): Promise<CloneData> {
    return this.call<CloneData>(["clone", url, dest, ...(o.hostKey ? ["--host-key", o.hostKey] : [])], o);
  }

  fetch(project: string, o: CallOptions = {}): Promise<FetchData> {
    return this.call<FetchData>(["fetch"], { ...o, project });
  }

  /**
   * The engine (as of json_interface 1) answers a rejected push with `ok:true`, empty data and
   * exit code 3 instead of a `ref_conflict` error; map that so a rejection is never shown as success.
   */
  push(project: string, o: CallOptions & { remote?: string } = {}): Promise<PushData> {
    return this.call<PushData>(["push"], { ...o, project, exitCodeErrors: { 3: "ref_conflict" } });
  }

  pull(project: string, o: CallOptions = {}): Promise<PullData> {
    return this.call<PullData>(["pull"], {
      ...o,
      project,
      allowExit: (d: PullData) => hasConflicts(d.merge ?? {}),
    });
  }

  /**
   * Where this branch stands against a remote, from two `log` calls. Used when `status` carries
   * no ahead/behind. `neverPushed` is true when the remote has no copy of the branch yet.
   */
  async syncPosition(
    project: string,
    branch: string,
    remote: string,
    limit = 500,
  ): Promise<{ ahead: number; behind: number; neverPushed: boolean }> {
    const local = await this.log(project, limit, branch);
    const theirs = await this.log(project, limit, `${remote}/${branch}`).catch((e: unknown) => {
      if (e instanceof RgitError && e.code !== CLIENT_ERROR.protocol) return null;
      throw e;
    });
    const mine = new Set(local.commits.map((c) => c.id));
    if (!theirs) return { ahead: local.commits.length, behind: 0, neverPushed: true };
    const remoteIds = new Set(theirs.commits.map((c) => c.id));
    return {
      ahead: local.commits.filter((c) => !remoteIds.has(c.id)).length,
      behind: theirs.commits.filter((c) => !mine.has(c.id)).length,
      neverPushed: false,
    };
  }
}
