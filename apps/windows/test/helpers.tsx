import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "../src/App";
import { createBrowserPlatform } from "../src/platform/browser";
import { DEFAULT_SETTINGS, type AppSettings, type Platform, type ServeStatus } from "../src/platform/types";
import type { Runner } from "../src/rgit/runner";
import type { EngineEvent } from "../src/rgit/types";
import { AppProvider } from "../src/state/app";
import { fixturesWith, nodeRunner, readCalls, type LoggedCall, type Override } from "./node-runner";

export interface Harness {
  user: ReturnType<typeof userEvent.setup>;
  calls: () => LoggedCall[];
  platform: Platform;
  settings: () => AppSettings;
  emitServeLine: (e: EngineEvent) => void;
  emitServeState: (s: ServeStatus) => void;
  picked: { value: string | null };
}

export function renderApp(
  o: { overrides?: Override[]; settings?: Partial<AppSettings>; platform?: Partial<Platform>; wrapRunner?: (r: Runner) => Runner } = {},
): Harness {
  const logFile = join(mkdtempSync(join(tmpdir(), "rgit-ui-")), "calls.ndjson");
  let settings: AppSettings = { ...DEFAULT_SETTINGS, language: "en", lastProject: "/work/demo", ...o.settings };
  const lineCbs: ((e: EngineEvent) => void)[] = [];
  const stateCbs: ((s: ServeStatus) => void)[] = [];
  const picked = { value: null as string | null };
  const platform = createBrowserPlatform({
    native: true,
    runner: (o.wrapRunner ?? ((r: Runner) => r))(nodeRunner({ fixtures: fixturesWith(o.overrides ?? []), logFile })),
    getSettings: async () => settings,
    saveSettings: async (s) => (settings = s),
    pickDirectory: async () => picked.value,
    onServeLine: async (cb) => {
      lineCbs.push(cb);
      return () => {};
    },
    onServeState: async (cb) => {
      stateCbs.push(cb);
      return () => {};
    },
    autostartSupported: async () => true,
    autostartGet: async () => false,
    autostartSet: async () => {},
    ...o.platform,
  });
  render(
    <AppProvider platform={platform}>
      <App />
    </AppProvider>,
  );
  return {
    user: userEvent.setup(),
    calls: () => readCalls(logFile),
    platform,
    settings: () => settings,
    emitServeLine: (e) => lineCbs.forEach((cb) => cb(e)),
    emitServeState: (s) => stateCbs.forEach((cb) => cb(s)),
    picked,
  };
}
