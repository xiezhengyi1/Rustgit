import { useState } from "react";
import { useI18n } from "../i18n";
import { useApp } from "../state/app";
import { Button, ConfirmDialog, Section } from "./ui";

/** Merge conflict resolution: keep mine / keep theirs / edit yourself, or abort. */
export function ConflictPanel({ showGoChanges = false }: { showGoChanges?: boolean }) {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const status = state.status;
  const [confirmAbort, setConfirmAbort] = useState(false);
  if (!status?.merge) return null;
  const project = state.project!;
  const unresolved = status.conflicts.filter((c) => !c.resolved).length;

  const resolve = (path: string, how: "ours" | "theirs" | "edit") =>
    actions.act((c, p) => c.resolve(p, path, how));

  return (
    <Section title={t("conflicts.title", { label: status.merge.label })} id="conflicts">
      <p>{status.conflicts.length === 0 ? t("conflicts.finish") : unresolved === 0 ? t("conflicts.allDone") : t("conflicts.intro")}</p>
      {status.conflicts.length > 0 ? (
        <ul className="rows" aria-label={t("conflicts.list")}>
          {status.conflicts.map((c) => (
            <li key={c.path} className="row conflict-row">
              <div className="row-main">
                <span className="path">{c.path}</span>
                <span className={c.resolved ? "chip chip-ok" : "chip chip-warn"}>
                  {c.resolved ? t("conflicts.resolved") : t("conflicts.unresolved")}
                </span>
                <span className="muted">{t("conflicts.kind", { kind: c.kind })}</span>
              </div>
              <div className="row-actions">
                <Button disabled={state.busy} onClick={() => void resolve(c.path, "ours")} aria-label={`${t("conflicts.ours")}: ${c.path}`}>
                  {t("conflicts.ours")}
                </Button>
                <Button disabled={state.busy} onClick={() => void resolve(c.path, "theirs")} aria-label={`${t("conflicts.theirs")}: ${c.path}`}>
                  {t("conflicts.theirs")}
                </Button>
                {c.kind !== "binary" ? (
                  <>
                    <Button
                      variant="quiet"
                      onClick={() => void actions.platform.revealPath(`${project}/${c.path}`)}
                      aria-label={`${t("conflicts.reveal")}: ${c.path}`}
                    >
                      {t("conflicts.reveal")}
                    </Button>
                    <Button disabled={state.busy} onClick={() => void resolve(c.path, "edit")} aria-label={`${t("conflicts.edit")}: ${c.path}`}>
                      {t("conflicts.edit")}
                    </Button>
                  </>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="button-row">
        {showGoChanges ? (
          <Button variant="primary" onClick={() => actions.setPage("changes")}>
            {t("conflicts.goChanges")}
          </Button>
        ) : null}
        <Button variant="danger" disabled={state.busy} onClick={() => setConfirmAbort(true)}>
          {t("conflicts.abort")}
        </Button>
      </div>
      {confirmAbort ? (
        <ConfirmDialog
          title={t("conflicts.abort")}
          body={t("conflicts.abortBody")}
          confirmLabel={t("conflicts.abort")}
          danger
          onCancel={() => setConfirmAbort(false)}
          onConfirm={() => {
            setConfirmAbort(false);
            void actions.act((c, p) => c.mergeAbort(p));
          }}
        />
      ) : null}
    </Section>
  );
}
