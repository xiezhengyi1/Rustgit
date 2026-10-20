import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  type ReactNode,
} from "react";
import { I18nProvider, resolveLocale, useI18n } from "../i18n";
import type { AppSettings, HostingConfig, Platform, ServeStatus } from "../platform/types";
import { DEFAULT_SETTINGS } from "../platform/types";
import { RgitClient, type CallOptions } from "../rgit/client";
import { RgitCancelled, RgitError, isRgitError } from "../rgit/errors";
import type { RunHandle } from "../rgit/runner";
import type {
  BranchListData,
  EngineEvent,
  FetchData,
  ProgressEvent,
  PullData,
  PushData,
  RemoteData,
  StatusData,
} from "../rgit/types";

export type SyncResult = FetchData | PushData | PullData;
import { errorText } from "./errors";

export type PageId = "projects" | "changes" | "history" | "sharing" | "storage";
export type OpKind = "fetch" | "push" | "pull" | "clone" | "verify" | "gc";

export interface ActiveOp {
  id: number;
  kind: OpKind;
  phase: string | null;
  done: number;
  total: number;
  bytes?: number;
  handle: RunHandle | null;
}

export interface LastSync {
  kind: "fetch" | "push" | "pull";
  at: number;
  ok: boolean;
  errorCode?: string;
  message?: string;
}

export interface Notice {
  id: number;
  kind: "error" | "success" | "info";
  text: string;
}

export interface AppState {
  pendingInvitation?: string;
  connection?: import("../rgit/types").ConnectionInfo;
  boot: "checking" | "ok" | "failed";
  bootError: { code: string; message: string } | null;
  settings: AppSettings;
  page: PageId;
  project: string | null;
  status: StatusData | null;
  branches: BranchListData | null;
  remotes: RemoteData["remotes"];
  /** a status scan is running (a 30k-file project takes seconds) */
  scanning: boolean;
  /** true when the server has no copy of the current branch yet */
  neverPushed: boolean;
  /** a user-visible operation that locks other mutations */
  busy: boolean;
  op: ActiveOp | null;
  lastSync: LastSync | null;
  serve: ServeStatus;
  hostLog: EngineEvent[];
  notices: Notice[];
  /** folder the user picked that is not a project yet */
  pendingInit: string | null;
}

const initialState: AppState = {
  boot: "checking",
  bootError: null,
  settings: DEFAULT_SETTINGS,
  page: "projects",
  project: null,
  status: null,
  branches: null,
  remotes: [],
  neverPushed: false,
  scanning: false,
  busy: false,
  op: null,
  lastSync: null,
  serve: { running: false },
  hostLog: [],
  notices: [],
  pendingInit: null,
};

type Action =
  | { type: "patch"; patch: Partial<AppState> }
  | { type: "notice"; notice: Notice }
  | { type: "dismiss"; id: number }
  | { type: "hostLine"; e: EngineEvent }
  | { type: "progress"; p: ProgressEvent }
  | { type: "attachHandle"; id: number; handle: RunHandle };

function reducer(s: AppState, a: Action): AppState {
  switch (a.type) {
    case "patch":
      return { ...s, ...a.patch };
    case "notice":
      return { ...s, notices: [...s.notices.slice(-3), a.notice] };
    case "dismiss":
      return { ...s, notices: s.notices.filter((n) => n.id !== a.id) };
    case "hostLine":
      return { ...s, hostLog: [...s.hostLog.slice(-199), a.e] };
    case "attachHandle":
      return s.op && s.op.id === a.id ? { ...s, op: { ...s.op, handle: a.handle } } : s;
    case "progress":
      return s.op
        ? { ...s, op: { ...s.op, phase: a.p.phase, done: a.p.done, total: a.p.total, bytes: a.p.bytes } }
        : s;
  }
}

// Reads through a function so TypeScript does not narrow the ref's value across awaits.
const currentOp = (ref: { current: AppState }): ActiveOp | null => ref.current.op;

export type ActResult<T> = { ok: true; value: T } | { ok: false; error: unknown };

export interface Actions {
  client: RgitClient;
  platform: Platform;
  setPage(p: PageId): void;
  notify(kind: Notice["kind"], text: string): void;
  dismissNotice(id: number): void;
  openProject(path: string): Promise<boolean>;
  createProject(path: string, branch: string): Promise<boolean>;
  closeProject(): void;
  cancelInit(): void;
  refresh(): Promise<void>;
  /**
   * Run a client call that changes (or reads) the project. Sets `busy`, refreshes status
   * afterwards, and reports failures as a notice unless `notify:false` (the caller handles it).
   */
  act<T>(
    fn: (c: RgitClient, project: string) => Promise<T>,
    opts?: { notify?: boolean; refresh?: boolean; /** false for pure reads: do not lock other actions */ busy?: boolean },
  ): Promise<ActResult<T>>;
  /** Run a long operation with progress and cancel. */
  runOp<T>(kind: OpKind, fn: (c: RgitClient, o: CallOptions) => Promise<T>, project?: string | null): Promise<ActResult<T>>;
  cancelOp(): void;
  sync(kind: "fetch" | "push" | "pull"): Promise<ActResult<SyncResult>>;
  clone(url: string, dest: string, hostKey?: string): Promise<ActResult<unknown>>;
  startHosting(cfg: HostingConfig): Promise<boolean>;
  stopHosting(): Promise<void>;
  saveSettings(patch: Partial<AppSettings>): Promise<void>;
}

interface Ctx {
  state: AppState;
  actions: Actions;
}

const AppContext = createContext<Ctx | null>(null);

export function useApp(): Ctx {
  const c = useContext(AppContext);
  if (!c) throw new Error("useApp outside AppProvider");
  return c;
}

export function AppProvider({ platform, children }: { platform: Platform; children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, initialState);
  const client = useMemo(() => new RgitClient(platform.runner), [platform]);
  const stateRef = useRef(state);
  stateRef.current = state;
  const noticeId = useRef(0);
  const locale = resolveLocale(state.settings.language);

  return (
    <I18nProvider locale={locale}>
      <Inner client={client} platform={platform} state={state} dispatch={dispatch} stateRef={stateRef} noticeId={noticeId}>
        {children}
      </Inner>
    </I18nProvider>
  );
}

function Inner({
  client,
  platform,
  state,
  dispatch,
  stateRef,
  noticeId,
  children,
}: {
  client: RgitClient;
  platform: Platform;
  state: AppState;
  dispatch: React.Dispatch<Action>;
  stateRef: React.MutableRefObject<AppState>;
  noticeId: React.MutableRefObject<number>;
  children: ReactNode;
}) {
  const i18n = useI18n();
  const i18nRef = useRef(i18n);
  i18nRef.current = i18n;

  const patch = useCallback((p: Partial<AppState>) => dispatch({ type: "patch", patch: p }), [dispatch]);

  const notify = useCallback(
    (kind: Notice["kind"], text: string) => {
      const id = ++noticeId.current;
      dispatch({ type: "notice", notice: { id, kind, text } });
      if (kind !== "error") setTimeout(() => dispatch({ type: "dismiss", id }), 6000);
    },
    [dispatch, noticeId],
  );

  const reportError = useCallback(
    (e: unknown) => {
      if (e instanceof RgitCancelled) return;
      notify("error", errorText(i18nRef.current, e));
    },
    [notify],
  );

  const loadProjectDataInner = useCallback(
    async (project: string) => {
      const [status, branches, remotes] = await Promise.all([
        client.status(project),
        client.branches(project).catch(() => null),
        client.remotes(project).catch(() => ({ remotes: [] })),
      ]);
      // The engine's status may not carry ahead/behind; derive them from the log when a server exists.
      let shown = status;
      let neverPushed = false;
      const remote = remotes.remotes.find((r) => r.name === "origin") ?? remotes.remotes[0];
      if (remote && status.ahead === undefined && status.head) {
        try {
          const pos = await client.syncPosition(project, status.branch, remote.name);
          shown = { ...status, ahead: pos.ahead, behind: pos.behind };
          neverPushed = pos.neverPushed;
        } catch {
          /* leave the sync summary at "unknown" */
        }
      }
      if (stateRef.current.project !== project) return;
      patch({ status: shown, branches, remotes: remotes.remotes, neverPushed });
    },
    [client, patch, stateRef],
  );

  const scans = useRef(0);
  const loadProjectData = useCallback(
    async (project: string) => {
      scans.current++;
      patch({ scanning: true });
      try {
        await loadProjectDataInner(project);
      } finally {
        scans.current--;
        if (scans.current === 0) patch({ scanning: false });
      }
    },
    [loadProjectDataInner, patch],
  );

  const refresh = useCallback(async () => {
    const project = stateRef.current.project;
    if (!project) return;
    try {
      await loadProjectData(project);
    } catch (e) {
      reportError(e);
    }
  }, [loadProjectData, reportError, stateRef]);

  const rememberProject = useCallback(
    async (path: string) => {
      const s = stateRef.current.settings;
      const next: AppSettings = {
        ...s,
        lastProject: path,
        recentProjects: [path, ...s.recentProjects.filter((p) => p !== path)].slice(0, 10),
      };
      patch({ settings: next });
      try {
        await platform.saveSettings(next);
      } catch {
        /* remembering recents is best effort */
      }
    },
    [patch, platform, stateRef],
  );

  const openProject = useCallback(
    async (path: string): Promise<boolean> => {
      try {
        // Know whether this project is shared before exposing its commit action.
        const [status, remotes] = await Promise.all([client.status(path), client.remotes(path)]);
        patch({ project: path, status, pendingInit: null, branches: null, remotes: remotes.remotes, neverPushed: false, lastSync: null });
        void rememberProject(path);
        void loadProjectData(path).catch(reportError);
        return true;
      } catch (e) {
        if (isRgitError(e, "not_a_repository")) {
          patch({ pendingInit: path });
        } else {
          reportError(e);
        }
        return false;
      }
    },
    [client, loadProjectData, patch, rememberProject, reportError],
  );

  const createProject = useCallback(
    async (path: string, branch: string): Promise<boolean> => {
      try {
        await client.init(path, branch || undefined);
        return await openProject(path);
      } catch (e) {
        reportError(e);
        return false;
      }
    },
    [client, openProject, reportError],
  );

  // `busy` means "a project-changing call is in flight". It is a counter, released in `finally`,
  // so overlapping calls cannot clear each other and a failed call cannot leave it stuck.
  const pending = useRef(0);
  const bump = useCallback(
    (d: number) => {
      pending.current = Math.max(0, pending.current + d);
      patch({ busy: pending.current > 0 });
    },
    [patch],
  );

  const act: Actions["act"] = useCallback(
    async (fn, opts) => {
      const project = stateRef.current.project;
      if (!project) return { ok: false, error: new Error("no project") };
      const lock = opts?.busy !== false;
      if (lock) bump(1);
      let result: ActResult<Awaited<ReturnType<typeof fn>>>;
      try {
        result = { ok: true, value: await fn(client, project) };
      } catch (e) {
        if (opts?.notify !== false) reportError(e);
        result = { ok: false, error: e };
      } finally {
        if (lock) bump(-1);
      }
      // Refresh after the lock is released: a slow status call must never keep buttons disabled.
      if (opts?.refresh !== false) await loadProjectData(project).catch(result.ok ? reportError : () => undefined);
      return result;
    },
    [bump, client, loadProjectData, reportError, stateRef],
  );

  // Long operations (sync, clone, verify, gc) are tracked as `op`, not `busy`: they must not
  // disable unrelated actions such as committing.
  const opSeq = useRef(0);
  const cancelledOps = useRef(new Set<number>());
  const cancelWaiters = useRef(new Map<number, () => void>());

  const runOp: Actions["runOp"] = useCallback(
    async (kind, fn, project) => {
      if (stateRef.current.op) return { ok: false, error: new Error("busy") };
      const id = ++opSeq.current;
      let cancelledByUser = false;
      patch({ op: { id, kind, phase: null, done: 0, total: 0, handle: null } });
      try {
        const cancelled = new Promise<never>((_resolve, reject) => {
          cancelWaiters.current.set(id, () => { cancelledByUser = true; reject(new RgitCancelled()); });
        });
        const value = await Promise.race([fn(client, {
          project: project ?? stateRef.current.project,
          onProgress: (p) => {
            if (currentOp(stateRef)?.id === id) dispatch({ type: "progress", p });
          },
          onEvent: (event) => {
            if (event.event === "connection" && currentOp(stateRef)?.id === id) patch({ connection: event as unknown as import("../rgit/types").ConnectionInfo });
          },
          onStart: (h) => {
            if (cancelledByUser || cancelledOps.current.has(id)) h.cancel();
            else dispatch({ type: "attachHandle", id, handle: h });
          },
        }), cancelled]);
        if (cancelledOps.current.has(id)) return { ok: false, error: new RgitCancelled() };
        return { ok: true, value };
      } catch (e) {
        return { ok: false, error: cancelledOps.current.has(id) ? new RgitCancelled() : e };
      } finally {
        cancelWaiters.current.delete(id);
        cancelledOps.current.delete(id);
        // Only clear our own op: after a cancel the UI is already idle and may be running a new one.
        if (currentOp(stateRef)?.id === id) patch({ op: null });
      }
    },
    [client, dispatch, patch, stateRef],
  );

  /** Returns the UI to idle at once; the child process is terminated in the background. */
  const cancelOp = useCallback(() => {
    const op = stateRef.current.op;
    if (!op) return;
    cancelledOps.current.add(op.id);
    patch({ op: null });
    cancelWaiters.current.get(op.id)?.();
    op.handle?.cancel();
  }, [patch, stateRef]);

  const sync: Actions["sync"] = useCallback(
    async (kind) => {
      const project = stateRef.current.project;
      if (!project) return { ok: false, error: new Error("no project") };
      const r = await runOp<SyncResult>(kind, (c, o): Promise<SyncResult> => c[kind](project, o), project);
      const at = Date.now();
      if (r.ok) {
        patch({ lastSync: { kind, at, ok: true } });
        const merged = kind === "pull" ? (r.value as PullData).merge : undefined;
        if (merged && merged.conflicts.length > 0) {
          notify("info", i18nRef.current.t("history.mergeConflicts", { n: merged.conflicts.length }));
        }
        if (kind === "fetch" && (r.value as FetchData).state === "diverged") {
          notify("info", i18nRef.current.t("sync.diverged"));
        }
      } else if (!(r.error instanceof RgitCancelled)) {
        patch({
          lastSync: {
            kind,
            at,
            ok: false,
            errorCode: r.error instanceof RgitError ? r.error.code : undefined,
            message: errorText(i18nRef.current, r.error),
          },
        });
        reportError(r.error);
      }
      await loadProjectData(project).catch(() => undefined);
      return r;
    },
    [loadProjectData, notify, patch, reportError, runOp, stateRef],
  );

  const clone: Actions["clone"] = useCallback(
    async (url, dest, hostKey) => {
      const r = await runOp<unknown>("clone", (c, o) => url.startsWith("projectapp://") ? c.join(url, dest, o) : c.clone(url, dest, { ...o, hostKey }), null);
      if (r.ok) {
        patch({ pendingInvitation: undefined });
        notify("success", i18nRef.current.t("projects.cloneDone", { path: dest }));
        await openProject(dest);
      } else {
        reportError(r.error);
      }
      return r;
    },
    [notify, openProject, patch, reportError, runOp],
  );

  const startHosting = useCallback(
    async (cfg: HostingConfig) => {
      try {
        const s = await platform.serveStart(cfg);
        patch({ serve: s, hostLog: [] });
        await saveSettingsRef.current({ hosting: cfg });
        return true;
      } catch (e) {
        reportError(e);
        return false;
      }
    },
    [patch, platform, reportError],
  );

  const stopHosting = useCallback(async () => {
    try {
      patch({ serve: await platform.serveStop() });
    } catch (e) {
      reportError(e);
    }
  }, [patch, platform, reportError]);

  const saveSettings = useCallback(
    async (p: Partial<AppSettings>) => {
      const next = { ...stateRef.current.settings, ...p };
      patch({ settings: next });
      try {
        patch({ settings: await platform.saveSettings(next) });
      } catch (e) {
        reportError(e);
      }
    },
    [patch, platform, reportError, stateRef],
  );
  const saveSettingsRef = useRef(saveSettings);
  saveSettingsRef.current = saveSettings;

  // ----- start-up ---------------------------------------------------------------------
  useEffect(() => {
    let cancelled = false;
    const unlisten: (() => void)[] = [];
    (async () => {
      try {
        const settings = await platform.getSettings();
        if (cancelled) return;
        patch({ settings });
        unlisten.push(await platform.onServeLine((e) => dispatch({ type: "hostLine", e })));
        if (platform.onInvitation) unlisten.push(await platform.onInvitation((uri) => patch({ pendingInvitation: uri, page: "projects" })));
        unlisten.push(await platform.onServeState((serve) => patch({ serve })));
        patch({ serve: await platform.serveStatus() });
        await client.ensureCompatible();
        if (cancelled) return;
        patch({ boot: "ok" });
        if (settings.lastProject) {
          const ok = await openProject(settings.lastProject);
          if (ok) patch({ page: "changes" });
        }
      } catch (e) {
        if (cancelled) return;
        patch({
          boot: "failed",
          bootError: {
            code: e instanceof RgitError ? e.code : "spawn_failed",
            message: e instanceof Error ? e.message : String(e),
          },
        });
      }
    })();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);

  useEffect(() => {
    void platform.setUiLanguage(i18n.locale).catch(() => undefined);
  }, [i18n.locale, platform]);

  // Refresh when the window regains focus: files may have changed outside the app.
  useEffect(() => {
    const onFocus = () => {
      if (!stateRef.current.op && !stateRef.current.busy && !stateRef.current.scanning) void refresh();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh, stateRef]);

  const actions = useMemo<Actions>(
    () => ({
      client,
      platform,
      setPage: (page) => patch({ page }),
      notify,
      dismissNotice: (id) => dispatch({ type: "dismiss", id }),
      openProject,
      createProject,
      closeProject: () => patch({ project: null, status: null, branches: null, remotes: [], neverPushed: false, lastSync: null }),
      cancelInit: () => patch({ pendingInit: null }),
      refresh,
      act,
      runOp,
      cancelOp,
      sync,
      clone,
      startHosting,
      stopHosting,
      saveSettings,
    }),
    [act, cancelOp, client, clone, createProject, dispatch, notify, openProject, patch, platform, refresh, runOp, saveSettings, startHosting, stopHosting, sync],
  );

  const value = useMemo(() => ({ state, actions }), [state, actions]);
  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}
