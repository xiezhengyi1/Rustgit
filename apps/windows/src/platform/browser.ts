import type { Runner } from "../rgit/runner";
import { DEFAULT_SETTINGS } from "./types";
import type { AppSettings, Platform, ServeStatus } from "./types";

const NOT_NATIVE = "This page is running outside the desktop app, so it cannot start rgit.";

const stopped: ServeStatus = { running: false };

/**
 * Used by `vite dev` in a normal browser and as the base of test platforms. It cannot run
 * rgit: every spawn fails with an explanatory error rather than pretending to work.
 */
export function createBrowserPlatform(overrides: Partial<Platform> = {}): Platform {
  const runner: Runner = {
    spawn() {
      return { done: Promise.reject(new Error(NOT_NATIVE)), cancel() {} };
    },
  };
  let settings: AppSettings = { ...DEFAULT_SETTINGS };
  return {
    native: false,
    runner,
    pickDirectory: async () => null,
    getSettings: async () => settings,
    saveSettings: async (s) => (settings = s),
    serveStart: async () => {
      throw new Error(NOT_NATIVE);
    },
    serveStop: async () => stopped,
    serveStatus: async () => stopped,
    onServeLine: async () => () => {},
    onServeState: async () => () => {},
    autostartSupported: async () => false,
    autostartGet: async () => false,
    autostartSet: async () => {
      throw new Error(NOT_NATIVE);
    },
    revealPath: async () => {},
    lanAddresses: async () => [],
    setUiLanguage: async () => {},
    ...overrides,
  };
}
