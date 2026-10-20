// Development-only page: renders the real UI in a normal browser, with a runner that replays the
// recorded rgit output from test/fixtures (the same data the jsdom tests use). It is not part of the
// production build (only /index.html is) and exists so layouts can be looked at without Windows.
// Open http://localhost:1420/test/visual/index.html?lang=zh&page=history during `npm run dev`.
import { createRoot } from "react-dom/client";
import { App } from "../../src/App";
import { createBrowserPlatform } from "../../src/platform/browser";
import { DEFAULT_SETTINGS } from "../../src/platform/types";
import type { RunHandle, Runner } from "../../src/rgit/runner";
import { AppProvider } from "../../src/state/app";
import "../../src/styles.css";
import manifestText from "../fixtures/manifest.json?raw";

const files = import.meta.glob("../fixtures/*", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const manifest = JSON.parse(manifestText) as { recordings: { argv: string[]; file: string; exit?: number }[] };
const params = new URLSearchParams(location.search);

const text = (file: string): string => {
  const key = Object.keys(files).find((k) => k.endsWith("/" + file.replace(/^.*\//, "")));
  if (file.startsWith("../fixtures-real/")) return realFiles[Object.keys(realFiles).find((k) => k.endsWith("/" + file.replace(/^.*\//, "")))!] ?? "";
  return key ? files[key]! : "";
};
const realFiles = import.meta.glob("../fixtures-real/*", { query: "?raw", import: "default", eager: true }) as Record<string, string>;

const runner: Runner = {
  spawn(req): RunHandle {
    const argv = req.args;
    const rec = manifest.recordings.find((r) => {
      const rest = r.argv[r.argv.length - 1] === "**";
      const fixed = rest ? r.argv.slice(0, -1) : r.argv;
      if (rest ? argv.length < fixed.length : argv.length !== fixed.length) return false;
      return fixed.every((a, i) => a === "*" || a === argv[i]);
    });
    const lines = (rec ? text(rec.file) : "").split("\n").filter(Boolean);
    return {
      done: new Promise((resolve) =>
        setTimeout(() => {
          lines.forEach((l) => req.onStdoutLine?.(l));
          resolve({ exitCode: rec ? (rec.exit ?? 0) : 64, stdout: lines, stderr: rec ? [] : ["no recording"], cancelled: false });
        }, 30),
      ),
      cancel() {},
    };
  },
};

const platform = createBrowserPlatform({
  native: true,
  runner,
  getSettings: async () => ({
    ...DEFAULT_SETTINGS,
    language: (params.get("lang") as "en" | "zh") ?? "en",
    lastProject: params.get("project") === "none" ? null : "C:\\Users\\ann\\Documents\\demo",
    recentProjects: ["C:\\Users\\ann\\Documents\\demo", "D:\\work\\site"],
  }),
  serveStatus: async () => (params.has("host") ? { running: true, addr: "127.0.0.1:7878", dir: "C:\\srv\\demo.rgit" } : { running: false }),
  autostartSupported: async () => true,
});

createRoot(document.getElementById("root")!).render(
  <AppProvider platform={platform}>
    <App />
  </AppProvider>,
);

const page = params.get("page");
if (page) {
  const idx = ["projects", "changes", "history", "sharing", "storage"].indexOf(page);
  setTimeout(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: String(idx + 1), ctrlKey: true })), 600);
}
