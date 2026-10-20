import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { RunHandle, RunOutcome, RunRequest, Runner } from "../rgit/runner";
import type { EngineEvent } from "../rgit/types";
import type { AppSettings, HostingConfig, Platform, ServeStatus, Unlisten } from "./types";

interface Pending {
  req: RunRequest;
  stdout: string[];
  stderr: string[];
  cancelled: boolean;
  resolve: (o: RunOutcome) => void;
}

interface LinePayload {
  runId: string;
  line: string;
}
interface ExitPayload {
  runId: string;
  code: number | null;
}

/** Runs `rgit --json` through the Rust side; stdout/stderr lines arrive as Tauri events. */
export class TauriRunner implements Runner {
  private readonly runs = new Map<string, Pending>();
  private readonly ready: Promise<void>;

  constructor() {
    this.ready = Promise.all([
      listen<LinePayload>("rgit-line", (e) => {
        const r = this.runs.get(e.payload.runId);
        if (!r) return;
        r.stdout.push(e.payload.line);
        if (r.stdout.length > 500) r.stdout.shift();
        r.req.onStdoutLine?.(e.payload.line);
      }),
      listen<LinePayload>("rgit-stderr", (e) => {
        const r = this.runs.get(e.payload.runId);
        if (!r) return;
        r.stderr.push(e.payload.line);
        if (r.stderr.length > 500) r.stderr.shift();
        r.req.onStderrLine?.(e.payload.line);
      }),
      listen<ExitPayload>("rgit-exit", (e) => {
        const r = this.runs.get(e.payload.runId);
        if (!r) return;
        this.runs.delete(e.payload.runId);
        r.resolve({ exitCode: e.payload.code, stdout: r.stdout, stderr: r.stderr, cancelled: r.cancelled });
      }),
    ]).then(() => undefined);
  }

  spawn(req: RunRequest): RunHandle {
    const runId = crypto.randomUUID();
    let entry!: Pending;
    const done = new Promise<RunOutcome>((resolve, reject) => {
      entry = { req, stdout: [], stderr: [], cancelled: false, resolve };
      this.ready
        .then(() => {
          this.runs.set(runId, entry);
          return invoke("rgit_spawn", { runId, project: req.project ?? null, args: req.args, input: req.stdin ?? null });
        })
        .then(() => {
          if (entry.cancelled) void invoke("rgit_cancel", { runId });
        })
        .catch((e) => {
          this.runs.delete(runId);
          reject(new Error(typeof e === "string" ? e : String(e)));
        });
    });
    return {
      done,
      cancel: () => {
        entry.cancelled = true;
        if (this.runs.has(runId)) void invoke("rgit_cancel", { runId });
      },
    };
  }
}

export function createTauriPlatform(): Platform {
  const lineListeners = (event: string) => async <T>(cb: (p: T) => void): Promise<Unlisten> => {
    const un = await listen<T>(event, (e) => cb(e.payload));
    return () => un();
  };
  const onLine = lineListeners("serve-line");
  const onState = lineListeners("serve-state");
  return {
    native: true,
    onInvitation: async (cb) => {
      const receive = (urls: string[]) => urls.filter(url => url.startsWith("projectapp://join/")).forEach(cb);
      const stop = await onOpenUrl(receive);
      receive(await getCurrent() ?? []);
      return stop;
    },
    runner: new TauriRunner(),
    async pickDirectory(title, defaultPath) {
      const r = await open({ directory: true, multiple: false, title, defaultPath });
      return typeof r === "string" ? r : null;
    },
    getSettings: () => invoke<AppSettings>("settings_get"),
    saveSettings: (settings) => invoke<AppSettings>("settings_save", { settings }),
    serveStart: (cfg: HostingConfig) => invoke<ServeStatus>("serve_start", { config: cfg }),
    serveStop: () => invoke<ServeStatus>("serve_stop"),
    serveStatus: () => invoke<ServeStatus>("serve_status"),
    onServeLine: (cb) => onLine<EngineEvent>(cb),
    onServeState: (cb) => onState<ServeStatus>(cb),
    autostartSupported: () => invoke<boolean>("autostart_supported"),
    autostartGet: () => invoke<boolean>("autostart_get"),
    autostartSet: (enabled) => invoke<void>("autostart_set", { enabled }),
    revealPath: (path) => invoke<void>("reveal_path", { path }),
    lanAddresses: () => invoke("lan_addresses_cmd"),
    setUiLanguage: (language) => invoke<void>("ui_language_set", { language }),
  };
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
