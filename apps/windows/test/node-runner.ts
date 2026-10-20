import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { RunHandle, RunOutcome, RunRequest, Runner } from "../src/rgit/runner";

const here = dirname(fileURLToPath(import.meta.url));
export const FIXTURES = join(here, "fixtures");
const FAKE = join(here, "fake-rgit.mjs");

export interface Override {
  argv: string[];
  file: string;
  exit?: number;
  hang?: boolean;
  delayMs?: number;
}

/** Build a fixtures dir whose manifest puts `overrides` in front of the shared recordings. */
export function fixturesWith(overrides: Override[]): string {
  const base = JSON.parse(readFileSync(join(FIXTURES, "manifest.json"), "utf8")) as { recordings: Override[] };
  const abs = (r: Override) => ({ ...r, file: resolve(FIXTURES, r.file) });
  const tmpRoot = join(here, ".tmp");
  mkdirSync(tmpRoot, { recursive: true });
  const dir = mkdtempSync(join(tmpRoot, "fx-"));
  writeFileSync(
    join(dir, "manifest.json"),
    JSON.stringify({ recordings: [...overrides.map(abs), ...base.recordings.map(abs)] }),
  );
  return dir;
}

export interface NodeRunnerOptions {
  /** use this executable (e.g. the real rgit) instead of the fake */
  realBinary?: string;
  environment?: Record<string, string>;
  fixtures?: string;
  logFile?: string;
}

export interface LoggedCall {
  project: string | null;
  argv: string[];
  stdinLines?: number;
  stdinHead?: string[];
}

/** Runs the fake rgit (or a real one) as a child process, like the Tauri side does. */
export function nodeRunner(opts: NodeRunnerOptions = {}): Runner {
  return {
    spawn(req: RunRequest): RunHandle {
      const args = ["--json", ...(req.project ? ["-C", req.project] : []), ...req.args];
      const [cmd, argv] = opts.realBinary ? [opts.realBinary, args] : [process.execPath, [FAKE, ...args]];
      const env = {
        ...process.env,
        ...opts.environment,
        RGIT_FIXTURES: opts.fixtures ?? FIXTURES,
        ...(opts.logFile ? { RGIT_FAKE_LOG: opts.logFile } : {}),
      };
      const child = spawn(cmd, argv, { env, stdio: ["pipe", "pipe", "pipe"] });
      if (req.stdin !== undefined) child.stdin.end(req.stdin);
      let cancelled = false;
      const stdout: string[] = [];
      const stderr: string[] = [];
      const pump = (stream: NodeJS.ReadableStream, sink: string[], cb?: (l: string) => void) => {
        let buf = "";
        stream.setEncoding?.("utf8");
        stream.on("data", (chunk: string) => {
          buf += chunk;
          let i;
          while ((i = buf.indexOf("\n")) !== -1) {
            const line = buf.slice(0, i);
            buf = buf.slice(i + 1);
            sink.push(line);
            cb?.(line);
          }
        });
        stream.on("end", () => {
          if (buf) {
            sink.push(buf);
            cb?.(buf);
          }
        });
      };
      pump(child.stdout, stdout, req.onStdoutLine);
      pump(child.stderr, stderr, req.onStderrLine);
      const done = new Promise<RunOutcome>((res, rej) => {
        child.on("error", rej);
        child.on("close", (code) => res({ exitCode: code, stdout, stderr, cancelled }));
      });
      return {
        done,
        cancel() {
          cancelled = true;
          child.kill("SIGTERM");
        },
      };
    },
  };
}

export function readCalls(file: string): LoggedCall[] {
  try {
    return readFileSync(file, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((l) => JSON.parse(l) as LoggedCall);
  } catch {
    return [];
  }
}

/** A client-ready fixtures dir holding exactly one recording from `dir`'s manifest, by file name. */
export function singleRecording(dir: string, file: string): string {
  const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8")) as { recordings: Override[] };
  const rec = manifest.recordings.find((r) => r.file === file);
  if (!rec) throw new Error(`no recording ${file} in ${dir}`);
  const tmpRoot = join(here, ".tmp");
  mkdirSync(tmpRoot, { recursive: true });
  const out = mkdtempSync(join(tmpRoot, "one-"));
  writeFileSync(join(out, "manifest.json"), JSON.stringify({ recordings: [{ ...rec, file: resolve(dir, rec.file) }] }));
  return out;
}
