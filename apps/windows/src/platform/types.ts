import type { Runner } from "../rgit/runner";
import type { EngineEvent } from "../rgit/types";

export type LanguageSetting = "auto" | "en" | "zh";

export interface HostingConfig {
  /** directory of the hosted (bare) repository; never a working project */
  dir: string;
  /** create it if missing (`rgit serve --init`) */
  init: boolean;
  /** `host:port`; loopback by default */
  listen: string;
  /** Legacy setting name: enables LAN binding and discovery; authentication stays required. */
  insecureListenAny: boolean;
  relay?: string;
}

export interface AppSettings {
  language: LanguageSetting;
  /** keep `rgit serve` running in the system tray when the window is closed */
  keepHostingInTray: boolean;
  /** when the app starts (also at login with --background), start hosting with `hosting` */
  startHostingOnLaunch: boolean;
  hosting: HostingConfig | null;
  lastProject: string | null;
  recentProjects: string[];
}

export const DEFAULT_SETTINGS: AppSettings = {
  language: "auto",
  keepHostingInTray: true,
  startHostingOnLaunch: false,
  hosting: null,
  lastProject: null,
  recentProjects: [],
};

export interface ServeStatus {
  secure?: boolean;
  fingerprint?: string | null;
  irohUrl?: string | null;
  relayOnline?: boolean | null;
  running: boolean;
  pid?: number | null;
  dir?: string | null;
  /** address reported by the `listening` event */
  addr?: string | null;
  /** last error line printed by the host process, if it stopped unexpectedly */
  lastError?: string | null;
}

export interface LanAddress {
  ip: string;
  /** adapter name, e.g. "Wi-Fi" */
  name: string;
  /** RFC 1918 private network address */
  private: boolean;
}

export type Unlisten = () => void;

export interface Platform {
  /** true when running inside the Tauri shell (false in a plain browser, e.g. `vite dev`) */
  readonly native: boolean;
  readonly runner: Runner;
  pickDirectory(title: string, defaultPath?: string): Promise<string | null>;
  getSettings(): Promise<AppSettings>;
  saveSettings(s: AppSettings): Promise<AppSettings>;
  serveStart(cfg: HostingConfig): Promise<ServeStatus>;
  serveStop(): Promise<ServeStatus>;
  serveStatus(): Promise<ServeStatus>;
  onInvitation?(cb: (uri: string) => void): Promise<Unlisten>;
  onServeLine(cb: (e: EngineEvent) => void): Promise<Unlisten>;
  onServeState(cb: (s: ServeStatus) => void): Promise<Unlisten>;
  /** Start-at-login through the per-user registry Run key; false/unsupported elsewhere. */
  autostartSupported(): Promise<boolean>;
  autostartGet(): Promise<boolean>;
  autostartSet(enabled: boolean): Promise<void>;
  revealPath(path: string): Promise<void>;
  /** This computer's usable IPv4 addresses (for the join address of a hosted project). */
  lanAddresses(): Promise<LanAddress[]>;
  /** Tell the shell which language the UI shows (tray menu labels follow it). */
  setUiLanguage(language: "en" | "zh"): Promise<void>;
}
