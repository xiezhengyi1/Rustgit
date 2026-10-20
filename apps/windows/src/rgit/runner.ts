// The only thing the UI needs from the platform to talk to the engine: start the bundled
// `rgit --json` process, receive its stdout lines, learn the exit code, and cancel.
// The Tauri implementation lives in platform/tauri.ts; tests use a Node child process.

export interface RunRequest {
  /** Subcommand and arguments, without `--json` and `-C` (the runner adds them). */
  args: string[];
  /** Project directory passed as `-C`. Omit for commands that do not need one (clone, serve). */
  project?: string | null;
  /** Text written to the process's stdin and then closed (for `--paths-from -`). */
  stdin?: string;
  onStdoutLine?: (line: string) => void;
  onStderrLine?: (line: string) => void;
}

export interface RunOutcome {
  /** null when the process was terminated by a signal / cancelled */
  exitCode: number | null;
  stdout: string[];
  stderr: string[];
  cancelled: boolean;
}

export interface RunHandle {
  readonly done: Promise<RunOutcome>;
  /** Terminate the process. The engine resumes interrupted transfers on the next run. */
  cancel(): void;
}

export interface Runner {
  spawn(req: RunRequest): RunHandle;
}
