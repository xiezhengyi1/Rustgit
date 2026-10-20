import { useI18n } from "../i18n";
import { useApp } from "../state/app";
import { Button } from "./ui";

export function Toasts() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  return (
    <div className="toasts" role="region" aria-label={t("toast.region")}>
      {state.notices.map((n) => (
        <div key={n.id} className={`toast toast-${n.kind}`} role={n.kind === "error" ? "alert" : "status"}>
          <span>{n.text}</span>
          <Button variant="quiet" onClick={() => actions.dismissNotice(n.id)} aria-label={t("common.dismiss")}>
            ×
          </Button>
        </div>
      ))}
    </div>
  );
}
