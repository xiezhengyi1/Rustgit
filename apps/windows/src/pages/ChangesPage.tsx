import { useEffect, useMemo, useState } from "react";
import { ConflictPanel } from "../components/ConflictPanel";
import { DiffView } from "../components/DiffView";
import { Button, Dialog, EmptyNote, Field, Section } from "../components/ui";
import { VirtualList } from "../components/VirtualList";
import { useI18n, type MessageKey } from "../i18n";
import { diffForPath, parseUnifiedDiff } from "../rgit/diff";
import { planSelection } from "../rgit/selection";
import { useApp } from "../state/app";
import { useSelection } from "../state/selection";
import { errorText } from "../state/errors";

/** Above this many changed files the combined diff is not fetched automatically (`diff` has no path filter). */
export const AUTO_DIFF_LIMIT = 300;
const ROW_HEIGHT = 34;
const SKIPPED_SHOWN = 200;

export function ChangesPage() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const status = state.status;
  const changes = useMemo(() => status?.changes ?? [], [status]);
  const selection = useSelection(changes);
  const [message, setMessage] = useState("");
  const [diffText, setDiffText] = useState("");
  const [loadBigDiff, setLoadBigDiff] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [shelving, setShelving] = useState(false);
  const [pendingShare, setPendingShare] = useState<string | null>(null);

  const big = changes.length > AUTO_DIFF_LIMIT;
  const wantDiff = changes.length > 0 && (!big || loadBigDiff);
  useEffect(() => {
    if (!state.project || !wantDiff) {
      setDiffText("");
      return;
    }
    let live = true;
    actions.client
      .diff(state.project)
      .then((d) => live && setDiffText(d.diff))
      .catch(() => live && setDiffText(""));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.project, changes, wantDiff]);
  useEffect(() => setLoadBigDiff(false), [state.project]);

  const files = useMemo(() => parseUnifiedDiff(diffText), [diffText]);
  const index = Math.min(activeIndex, Math.max(0, changes.length - 1));
  const current = changes[index]?.path ?? null;

  if (!state.project || !status) return <div className="page"><h1>{t("nav.changes")}</h1><EmptyNote>{t("changes.noProject")}</EmptyNote></div>;

  const merging = status.merge !== null;
  const unresolved = status.conflicts.some((c) => !c.resolved);
  const count = merging ? changes.length : selection.count;
  const plan = merging ? ({ kind: "all" } as const) : planSelection(changes, selection.set);
  const remote = state.remotes.find((r) => r.name === "origin")?.name ?? state.remotes[0]?.name;
  const canCommit = (message.trim() !== "" || merging) && count > 0 && !unresolved && !state.busy && (!remote || !state.op);
  const label = remote ? t("changes.commitShare") : plan.kind !== "all" ? t("changes.commit", { n: count }) : t("changes.commitAll");

  const retryShare = async () => {
    const result = await actions.act(async (_c, p) => {
      const sent = await actions.runOp("push", (c, o) => c.push(p, { ...o, remote }), p);
      if (!sent.ok) throw sent.error;
      return sent.value;
    }, { notify: false });
    if (result.ok) {
      setPendingShare(null);
      actions.notify("success", t("changes.shared"));
    } else {
      actions.notify("error", t("changes.shareFailed", { reason: errorText({ t }, result.error) }));
    }
  };

  const commit = async () => {
    let saved = false;
    const r = await actions.act(async (c, p) => {
      const committed = await c.commitSelection(p, message.trim(), plan);
      saved = true;
      setMessage("");
      if (remote) {
        setPendingShare(p);
        const sent = await actions.runOp("push", (sender, o) => sender.push(p, { ...o, remote }), p);
        if (!sent.ok) throw sent.error;
        setPendingShare(null);
      }
      return committed;
    }, { notify: false });
    if (r.ok) {
      actions.notify("success", remote ? t("changes.shared") : t("changes.committed", { short: r.value.short, branch: r.value.branch }));
    } else {
      actions.notify("error", saved && remote ? t("changes.shareFailed", { reason: errorText({ t }, r.error) }) : errorText({ t }, r.error));
    }
  };

  return (
    <div className="page">
      <h1>{t("nav.changes")}</h1>
      <ConflictPanel />
      {unresolved ? <p className="warn" role="note">{t("changes.conflictsFirst")}</p> : null}
      <div className="two-col">
        <Section
          title={t("changes.list")}
          actions={
            <Button variant="quiet" disabled={changes.length === 0 || state.busy} onClick={() => setShelving(true)}>
              {t("changes.shelve")}
            </Button>
          }
        >
          {changes.length === 0 ? (
            <EmptyNote>{t("changes.none")}</EmptyNote>
          ) : (
            <>
              <label className="check">
                <input
                  type="checkbox"
                  disabled={merging}
                  checked={merging || count === changes.length}
                  ref={(el) => {
                    if (el) el.indeterminate = !merging && count > 0 && count < changes.length;
                  }}
                  onChange={(e) => (e.target.checked ? selection.selectAll() : selection.clear())}
                />
                {t("changes.selectAll")} <span className="muted">({t("changes.selected", { n: count, total: changes.length })})</span>
              </label>
              {merging ? <p className="muted">{t("changes.mergeNeedsAll")}</p> : null}
              <VirtualList
                items={changes}
                rowHeight={ROW_HEIGHT}
                label={t("changes.list")}
                activeIndex={index}
                onActiveIndexChange={setActiveIndex}
                keyOf={(c) => c.path}
                renderRow={(c, i, ctx) => (
                  <>
                    <input
                      type="checkbox"
                      aria-label={t("changes.file", { path: c.path })}
                      checked={merging || selection.has(c.path)}
                      disabled={merging}
                      onChange={() => selection.toggle(c.path)}
                    />
                    <button
                      type="button"
                      data-row
                      tabIndex={ctx.tabIndex}
                      className="row-button"
                      aria-current={ctx.active}
                      onClick={() => setActiveIndex(i)}
                    >
                      <span className={`kind kind-${c.kind}`}>{t(`kind.${c.kind}` as MessageKey)}</span>
                      <span className="path">{c.path}</span>
                      {c.from ? <span className="muted">{t("changes.renamedFrom", { from: c.from })}</span> : null}
                    </button>
                  </>
                )}
              />
            </>
          )}
          {status.skipped.length > 0 ? (
            <details>
              <summary>{t("changes.skipped", { n: status.skipped.length })}</summary>
              <ul className="log">
                {status.skipped.slice(0, SKIPPED_SHOWN).map((s) => (
                  <li key={s.path}>
                    <span className="path">{s.path}</span> <span className="muted">{s.reason}</span>
                  </li>
                ))}
                {status.skipped.length > SKIPPED_SHOWN ? <li className="muted">{t("changes.skippedMore", { n: status.skipped.length - SKIPPED_SHOWN })}</li> : null}
              </ul>
            </details>
          ) : null}
          <form
            className="commit-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (canCommit) void commit();
            }}
          >
            {pendingShare === state.project ? (
              <div className="warn" role="status">
                <p>{t("changes.pendingShare")}</p>
                <Button onClick={() => void retryShare()} disabled={state.busy || !!state.op}>{t("changes.retryShare")}</Button>
              </div>
            ) : null}
            <Field label={t("changes.message")} hint={remote ? t("changes.messageShareHint") : t("changes.messageHint")}>
              {(p) => (
                <textarea
                  {...p}
                  rows={3}
                  value={message}
                  onChange={(e) => setMessage(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && canCommit) {
                      e.preventDefault();
                      void commit();
                    }
                  }}
                />
              )}
            </Field>
            <Button variant="primary" type="submit" disabled={!canCommit}>
              {label}
            </Button>
          </form>
        </Section>
        <Section title={current ? `${t("changes.diffTitle")}: ${current}` : t("changes.diffTitle")}>
          {!current ? (
            <EmptyNote>{t("changes.pickFile")}</EmptyNote>
          ) : big && !loadBigDiff ? (
            <>
              <EmptyNote>{t("changes.bigNoDiff", { n: changes.length })}</EmptyNote>
              <Button onClick={() => setLoadBigDiff(true)}>{t("changes.bigLoadDiff")}</Button>
            </>
          ) : (
            <DiffView file={diffForPath(files, current)} />
          )}
        </Section>
      </div>
      {shelving ? <ShelveDialog onClose={() => setShelving(false)} /> : null}
    </div>
  );
}

function ShelveDialog({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const { actions } = useApp();
  const [note, setNote] = useState("");
  return (
    <Dialog
      title={t("changes.shelve")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            onClick={async () => {
              const r = await actions.act((c, p) => c.shelve(p, note.trim()));
              if (r.ok) actions.notify("success", t("changes.shelved"));
              onClose();
            }}
          >
            {t("dialog.shelveOk")}
          </Button>
        </>
      }
    >
      <p>{t("changes.shelveBody")}</p>
      <Field label={t("changes.shelveName")}>{(p) => <input {...p} value={note} onChange={(e) => setNote(e.target.value)} />}</Field>
    </Dialog>
  );
}
