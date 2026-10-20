import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { createBrowserPlatform } from "./platform/browser";
import { createTauriPlatform, isTauri } from "./platform/tauri";
import { AppProvider } from "./state/app";
import "./styles.css";

const platform = isTauri() ? createTauriPlatform() : createBrowserPlatform();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppProvider platform={platform}>
      <App />
    </AppProvider>
  </StrictMode>,
);
