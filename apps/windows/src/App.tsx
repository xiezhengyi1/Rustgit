import { useEffect, useState } from "react";
import { SettingsDialog } from "./components/SettingsDialog";
import { StatusBar } from "./components/StatusBar";
import { Toasts } from "./components/Toasts";
import { Button } from "./components/ui";
import { useI18n, type MessageKey } from "./i18n";
import { ChangesPage } from "./pages/ChangesPage";
import { HistoryPage } from "./pages/HistoryPage";
import { ProjectsPage } from "./pages/ProjectsPage";
import { SharingPage } from "./pages/SharingPage";
import { StoragePage } from "./pages/StoragePage";
import { useApp, type PageId } from "./state/app";

const PAGES: { id: PageId; label: MessageKey; icon: string }[] = [
  { id: "projects", label: "nav.projects", icon: "▤" },
  { id: "changes", label: "nav.changes", icon: "✎" },
  { id: "history", label: "nav.history", icon: "⑂" },
  { id: "sharing", label: "nav.sharing", icon: "⇄" },
  { id: "storage", label: "nav.storage", icon: "▥" },
];

export function App() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const [settingsOpen, setSettingsOpen] = useState(false);

  // Ctrl+1..5 jump between areas; Ctrl+, opens settings.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.shiftKey) return;
      const n = Number(e.key);
      if (n >= 1 && n <= PAGES.length) {
        e.preventDefault();
        actions.setPage(PAGES[n - 1]!.id);
      } else if (e.key === ",") {
        e.preventDefault();
        setSettingsOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [actions]);

  if (state.boot === "checking") {
    return (
      <div className="boot" role="status">
        {t("boot.checking")}
      </div>
    );
  }
  if (state.boot === "failed") {
    const code = state.bootError?.code;
    return (
      <div className="boot" role="alert">
        <h1>{t("boot.failed")}</h1>
        <p>{state.bootError?.message}</p>
        <p className="muted">
          {!actions.platform.native
            ? t("boot.notNative")
            : code === "interface_mismatch"
              ? t("boot.hintMismatch")
              : t("boot.hintMissing")}
        </p>
      </div>
    );
  }

  const Page = { projects: ProjectsPage, changes: ChangesPage, history: HistoryPage, sharing: SharingPage, storage: StoragePage }[state.page];
  return (
    <div className="shell">
      <a className="skip" href="#main" onClick={(e) => { e.preventDefault(); document.getElementById("main")?.focus(); }}>
        {t("app.skip")}
      </a>
      <nav className="nav" aria-label={t("nav.label")}>
        <div className="brand">{t("app.name")}</div>
        <ul>
          {PAGES.map((p, i) => (
            <li key={p.id}>
              <button
                type="button"
                className={`nav-item ${state.page === p.id ? "active" : ""}`}
                aria-current={state.page === p.id ? "page" : undefined}
                aria-keyshortcuts={`Control+${i + 1}`}
                onClick={() => actions.setPage(p.id)}
              >
                <span aria-hidden="true" className="nav-icon">{p.icon}</span>
                {t(p.label)}
              </button>
            </li>
          ))}
        </ul>
        <Button variant="quiet" className="nav-settings" onClick={() => setSettingsOpen(true)} aria-keyshortcuts="Control+,">
          ⚙ {t("nav.settings")}
        </Button>
      </nav>
      <div className="content">
        <StatusBar />
        <main id="main" tabIndex={-1}>
          <Page />
        </main>
      </div>
      <Toasts />
      {settingsOpen ? <SettingsDialog onClose={() => setSettingsOpen(false)} /> : null}
    </div>
  );
}
