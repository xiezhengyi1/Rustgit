import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import type { LanguageSetting } from "../platform/types";
import { errorText } from "../state/errors";
import { useApp } from "../state/app";
import { Dialog, Field } from "./ui";

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const i18n = useI18n();
  const { t } = i18n;
  const { state, actions } = useApp();
  const [supported, setSupported] = useState(false);
  const [autostart, setAutostart] = useState(false);

  useEffect(() => {
    let live = true;
    void actions.platform.autostartSupported().then(async (s) => {
      if (!live) return;
      setSupported(s);
      if (s) setAutostart(await actions.platform.autostartGet().catch(() => false));
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Dialog title={t("settings.title")} onClose={onClose}>
      <Field label={t("settings.language")}>
        {(p) => (
          <select {...p} value={state.settings.language} onChange={(e) => void actions.saveSettings({ language: e.target.value as LanguageSetting })}>
            <option value="auto">{t("settings.langAuto")}</option>
            <option value="en">{t("settings.langEn")}</option>
            <option value="zh">{t("settings.langZh")}</option>
          </select>
        )}
      </Field>
      <label className="check">
        <input
          type="checkbox"
          checked={state.settings.keepHostingInTray}
          onChange={(e) => void actions.saveSettings({ keepHostingInTray: e.target.checked })}
        />
        {t("settings.keepTray")}
      </label>
      <p className="hint">{t("settings.keepTrayHelp")}</p>
      <label className="check">
        <input
          type="checkbox"
          checked={autostart}
          disabled={!supported}
          onChange={async (e) => {
            const want = e.target.checked;
            try {
              await actions.platform.autostartSet(want);
              setAutostart(want);
            } catch (err) {
              actions.notify("error", errorText(i18n, err));
            }
          }}
        />
        {t("settings.startLogin")}
      </label>
      <p className="hint">{supported ? t("settings.startLoginHelp") : t("settings.startLoginUnsupported")}</p>
    </Dialog>
  );
}
