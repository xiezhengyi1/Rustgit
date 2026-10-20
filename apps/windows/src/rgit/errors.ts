import type { ErrorBody } from "./types";

/** Error reported by the engine (`{"ok":false,"error":{...}}`) or by the client itself. */
export class RgitError extends Error {
  readonly code: string;
  readonly paths: string[];
  readonly stderr: string;

  constructor(code: string, message: string, paths: string[] = [], stderr = "") {
    super(message);
    this.name = "RgitError";
    this.code = code;
    this.paths = paths;
    this.stderr = stderr;
  }

  static fromBody(body: ErrorBody, stderr = ""): RgitError {
    return new RgitError(body.code, body.message, body.paths ?? [], stderr);
  }
}

export class RgitCancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "RgitCancelled";
  }
}

/** Client-side error codes (never produced by the engine). */
export const CLIENT_ERROR = {
  /** stdout held no result line */
  protocol: "protocol_error",
  /** `json_interface` differs from the version this app understands */
  interfaceMismatch: "interface_mismatch",
  /** the rgit executable could not be started */
  spawnFailed: "spawn_failed",
} as const;

export function isRgitError(e: unknown, code?: string): e is RgitError {
  return e instanceof RgitError && (code === undefined || e.code === code);
}
