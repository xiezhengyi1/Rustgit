import { useCallback, useEffect, useMemo, useState, type ReactElement } from "react";
import { ConflictPanel } from "../components/ConflictPanel";
import { DiffView } from "../components/DiffView";
import { Button, ConfirmDialog, Dialog, EmptyNote, Field, Section, useRovingList } from "../components/ui";
import { useI18n, type MessageKey } from "../i18n";
import { diffForPath, parseUnifiedDiff } from "../rgit/diff";
import { firstLine, formatCommitTime } from "../rgit/format";
import { layoutGraph, type GraphRow } from "../rgit/graph";
import { isRgitError } from "../rgit/errors";
import type { Commit, MergeLikeData, ShowData } from "../rgit/types";
import { errorText } from "../state/errors";
import { useApp } from "../state/app";

const LANE_W = 14;
const ROW_H = 46;
const COLORS = ["#4f7cff", "#e0883a", "#3aa675", "#c44fb0", "#8a6bd1", "#c9a227"];

function GraphCell({ row }: { row: GraphRow }) {
  const w = Math.max(1, row.width) * LANE_W + 6;
  const x = (lane: number) => lane * LANE_W + LANE_W / 2 + 2;
  const mid = ROW_H / 2;
  const color = (lane: number) => COLORS[lane % COLORS.length]!;
  const parts: ReactElement[] = [];
  row.before.forEach((id, lane) => {
    if (id === null) return;
    if (id === row.commit.id) {
      parts.push(<line key={`in${lane}`} x1={x(lane)} y1={0} x2={x(row.lane)} y2={mid} stroke={color(lane)} strokeWidth={2} />);
    } else if (row.after[lane] === id) {
      parts.push(<line key={`pass${lane}`} x1={x(lane)} y1={0} x2={x(lane)} y2={ROW_H} stroke={color(lane)} strokeWidth={2} />);
    } else {
      parts.push(<line key={`end${lane}`} x1={x(lane)} y1={0} x2={x(lane)} y2={mid} stroke={color(lane)} strokeWidth={2} />);
    }
  });
  row.parentLanes.forEach((target, i) => {
    parts.push(<line key={`out${i}`} x1={x(row.lane)} y1={mid} x2={x(target)} y2={ROW_H} stroke={color(target)} strokeWidth={2} />);
  });
  const isMerge = row.commit.parents.length > 1;
  return (
    <svg width={w} height={ROW_H} aria-hidden="true" focusable="false" className="graph">
      {parts}
      <circle cx={x(row.lane)} cy={mid} r={isMerge ? 5 : 4} fill={isMerge ? "var(--bg)" : color(row.lane)} stroke={color(row.lane)} strokeWidth={2} />
    </svg>
  );
}

export function HistoryPage() {
  const { t, intlLocale } = useI18n();
  const { state, actions } = useApp();
  const project = state.project;
  const [commits, setCommits] = useState<Commit[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const roving = useRovingList();

  const head = state.status?.head ?? null;
  const loadLog = useCallback(async () => {
    if (!project) return;
    const r = await actions.act((c, p) => c.log(p, 200), { refresh: false, busy: false });
    if (r.ok) setCommits(r.value.commits);
    setLoaded(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project]);

  useEffect(() => {
    setLoaded(false);
    void loadLog();
  }, [loadLog, head, state.status?.branch]);

  const rows = useMemo(() => layoutGraph(commits), [commits]);
  const selected = commits.find((c) => c.id === selectedId) ?? null;

  if (!project) return <div className="page"><h1>{t("nav.history")}</h1><EmptyNote>{t("changes.noProject")}</EmptyNote></div>;

  return (
    <div className="page">
      <h1>{t("nav.history")}</h1>
      <ConflictPanel showGoChanges />
      <div className="two-col">
        <Section
          title={t("history.commits")}
          actions={
            <Button variant="quiet" onClick={() => void loadLog()}>
              {t("history.refreshLog")}
            </Button>
          }
        >
          {loaded && rows.length === 0 ? (
            <EmptyNote>{t("history.none")}</EmptyNote>
          ) : (
            <ul className="commits" role="listbox" aria-label={t("history.graph")} ref={roving.ref} onKeyDown={roving.onKeyDown}>
              {rows.map((row) => (
                <li
                  key={row.commit.id}
                  role="option"
                  aria-selected={row.commit.id === selectedId}
                  tabIndex={0}
                  data-row
                  className={`commit ${row.commit.id === selectedId ? "row-active" : ""}`}
                  onClick={() => setSelectedId(row.commit.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setSelectedId(row.commit.id);
                    }
                  }}
                >
                  <GraphCell row={row} />
                  <span className="commit-text">
                    <span className="commit-line">
                      <span className="commit-msg">{firstLine(row.commit.message) || "—"}</span>
                      {row.commit.branches.map((b) => (
                        <span key={b} className={`chip ${b === state.status?.branch ? "chip-current" : ""}`}>
                          {b}
                        </span>
                      ))}
                      {row.commit.parents.length > 1 ? <span className="chip">{t("history.merge")}</span> : null}
                    </span>
                    <span className="muted small">
                      {row.commit.short} · {row.commit.author_name} · {formatCommitTime(row.commit.timestamp, row.commit.tz_offset_min, intlLocale)}
                    </span>
                  </span>
                </li>
              ))}
            </ul>
          )}
        </Section>
        <div className="stack">
          <CommitDetails commit={selected} onChanged={loadLog} />
          <BranchSection commits={commits} onChanged={loadLog} />
        </div>
      </div>
    </div>
  );
}

function CommitDetails({ commit, onChanged }: { commit: Commit | null; onChanged: () => Promise<void> }) {
  const { t, intlLocale } = useI18n();
  const { state, actions } = useApp();
  const [show, setShow] = useState<ShowData | null>(null);
  const [file, setFile] = useState<string | null>(null);
  const [restoring, setRestoring] = useState<string | null>(null);
  const [forceRestore, setForceRestore] = useState<string | null>(null);
  const [reverting, setReverting] = useState(false);
  const [branchFrom, setBranchFrom] = useState(false);
  const id = commit?.id ?? null;

  useEffect(() => {
    setShow(null);
    setFile(null);
    if (!id) return;
    let live = true;
    void actions.act((c, p) => c.show(p, id), { refresh: false, busy: false }).then((r) => {
      if (live && r.ok) {
        setShow(r.value);
        setFile(r.value.changes[0]?.path ?? null);
      }
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const files = useMemo(() => parseUnifiedDiff(show?.diff ?? ""), [show]);

  if (!commit) {
    return (
      <Section title={t("history.details")}>
        <EmptyNote>{t("history.pick")}</EmptyNote>
      </Section>
    );
  }

  const doRestore = async (path: string, force: boolean) => {
    const r = await actions.act((c, p) => c.restore(p, [path], commit.id, force), { notify: false });
    if (r.ok) {
      actions.notify("success", t("history.restored", { n: r.value.restored?.length ?? 1 }));
      return;
    }
    if (!force && isRgitError(r.error, "dirty_working_tree")) {
      setForceRestore(path);
      return;
    }
    actions.notify("error", errorText({ t }, r.error));
  };

  const notifyMerge = (r: MergeLikeData) => {
    if (r.conflicts.length > 0) actions.notify("info", t("history.mergeConflicts", { n: r.conflicts.length }));
  };

  return (
    <Section
      title={t("history.details")}
      actions={
        <>
          <Button onClick={() => setBranchFrom(true)} disabled={state.busy}>
            {t("history.branchHere")}
          </Button>
          <Button onClick={() => setReverting(true)} disabled={state.busy || state.status?.merge != null}>
            {t("history.revert")}
          </Button>
        </>
      }
    >
      <h3>{firstLine(commit.message)}</h3>
      {commit.message.includes("\n") ? <pre className="message">{commit.message.slice(commit.message.indexOf("\n")).trim()}</pre> : null}
      <p className="muted">
        {t("history.by", { name: commit.author_name, when: formatCommitTime(commit.timestamp, commit.tz_offset_min, intlLocale) })} · <code>{commit.short}</code>
      </p>
      <h4>{t("history.filesChanged")}</h4>
      {show === null ? (
        <p className="muted">{t("common.loading")}</p>
      ) : (
        <ul className="rows">
          {show.changes.map((c) => (
            <li key={c.path} className={`row ${file === c.path ? "row-active" : ""}`}>
              <button type="button" className="row-button" aria-current={file === c.path} onClick={() => setFile(c.path)}>
                <span className={`kind kind-${c.kind}`}>{t(`kind.${c.kind}` as MessageKey)}</span>
                <span className="path">{c.path}</span>
              </button>
              {c.kind !== "deleted" ? (
                <Button variant="quiet" disabled={state.busy} onClick={() => setRestoring(c.path)} aria-label={`${t("history.restoreFile")}: ${c.path}`}>
                  {t("history.restoreFile")}
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
      {file ? <DiffView file={diffForPath(files, file)} /> : null}

      {restoring ? (
        <ConfirmDialog
          title={t("history.restoreFile")}
          body={t("history.restoreBody", { path: restoring, short: commit.short })}
          confirmLabel={t("history.restoreFile")}
          onCancel={() => setRestoring(null)}
          onConfirm={() => {
            const p = restoring;
            setRestoring(null);
            void doRestore(p, false);
          }}
        />
      ) : null}
      {forceRestore ? (
        <ConfirmDialog
          title={t("history.restoreFile")}
          body={t("history.restoreForce", { path: forceRestore })}
          confirmLabel={t("history.restoreFile")}
          danger
          onCancel={() => setForceRestore(null)}
          onConfirm={() => {
            const p = forceRestore;
            setForceRestore(null);
            void doRestore(p, true);
          }}
        />
      ) : null}
      {reverting ? (
        <ConfirmDialog
          title={t("history.revert")}
          body={t("history.revertBody", { short: commit.short })}
          confirmLabel={t("history.revert")}
          onCancel={() => setReverting(false)}
          onConfirm={async () => {
            setReverting(false);
            const r = await actions.act((c, p) => c.revert(p, commit.id));
            if (r.ok) {
              notifyMerge(r.value);
              if (r.value.conflicts.length === 0) actions.notify("success", t("history.reverted", { short: commit.short }));
              await onChanged();
            }
          }}
        />
      ) : null}
      {branchFrom ? <NewBranchDialog from={commit.id} onClose={() => setBranchFrom(false)} onDone={onChanged} /> : null}
    </Section>
  );
}

function NewBranchDialog({ from, onClose, onDone }: { from?: string; onClose: () => void; onDone: () => Promise<void> }) {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const [name, setName] = useState("");
  const [start, setStart] = useState(from ?? "");
  const submit = async () => {
    const r = await actions.act((c, p) => c.createBranch(p, name.trim(), start || undefined));
    if (r.ok) {
      actions.notify("success", t("history.created", { name: r.value.name }));
      onClose();
      await onDone();
    }
  };
  return (
    <Dialog
      title={t("history.branchNew")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={name.trim() === "" || state.busy} onClick={() => void submit()}>
            {t("common.create")}
          </Button>
        </>
      }
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) void submit();
        }}
      >
        <Field label={t("history.branchName")}>{(p) => <input {...p} value={name} onChange={(e) => setName(e.target.value)} />}</Field>
        {!from ? (
          <Field label={t("history.branchFrom")}>
            {(p) => (
              <select {...p} value={start} onChange={(e) => setStart(e.target.value)}>
                <option value="">{`${t("history.branchCurrent")} (${state.status?.branch ?? ""})`}</option>
                {state.branches?.branches.map((b) => (
                  <option key={b.name} value={b.name}>
                    {b.name}
                  </option>
                ))}
              </select>
            )}
          </Field>
        ) : (
          <p className="muted">
            {t("history.branchFrom")}: <code>{from.slice(0, 10)}</code>
          </p>
        )}
      </form>
    </Dialog>
  );
}

function BranchSection({ onChanged }: { commits: Commit[]; onChanged: () => Promise<void> }) {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [dirty, setDirty] = useState<{ name: string; paths: string[] } | null>(null);
  const branches = state.branches;

  const doSwitch = async (name: string) => {
    const r = await actions.act((c, p) => c.switchBranch(p, name), { notify: false });
    if (r.ok) {
      actions.notify("success", t("history.switched", { name }));
      await onChanged();
    } else if (isRgitError(r.error, "dirty_working_tree")) {
      setDirty({ name, paths: r.error.paths });
    } else {
      actions.notify("error", errorText({ t }, r.error));
    }
  };

  const doMerge = async (name: string) => {
    const r = await actions.act((c, p) => c.merge(p, name), { notify: true });
    if (!r.ok) return;
    const v = r.value;
    if (v.conflicts.length > 0) actions.notify("info", t("history.mergeConflicts", { n: v.conflicts.length }));
    else if (v.already_up_to_date) actions.notify("info", t("history.mergeUpToDate", { name }));
    else if (v.fast_forward) actions.notify("success", t("history.mergeFast", { name }));
    else actions.notify("success", t("history.merged", { name }));
    await onChanged();
  };

  return (
    <Section
      title={t("history.branches")}
      actions={
        <Button onClick={() => setCreating(true)} disabled={state.busy}>
          {t("history.branchNew")}
        </Button>
      }
    >
      {!branches ? (
        <p className="muted">{t("common.loading")}</p>
      ) : (
        <ul className="rows" aria-label={t("history.branches")}>
          {branches.branches.map((b) => {
            const current = b.name === branches.current;
            return (
              <li key={b.name} className="row">
                <div className="row-main">
                  <span className="path">{b.name}</span>
                  {current ? <span className="chip chip-current">{t("history.branchCurrent")}</span> : null}
                  <code className="muted">{b.short}</code>
                </div>
                {!current ? (
                  <div className="row-actions">
                    <Button disabled={state.busy} onClick={() => void doSwitch(b.name)} aria-label={`${t("history.switch")}: ${b.name}`}>
                      {t("history.switch")}
                    </Button>
                    <Button disabled={state.busy || state.status?.merge != null} onClick={() => void doMerge(b.name)} aria-label={`${t("history.merge2")}: ${b.name}`}>
                      {t("history.merge2")}
                    </Button>
                    <Button variant="quiet" disabled={state.busy} onClick={() => setDeleting(b.name)} aria-label={`${t("history.delete")}: ${b.name}`}>
                      {t("history.delete")}
                    </Button>
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {creating ? <NewBranchDialog onClose={() => setCreating(false)} onDone={onChanged} /> : null}
      {deleting ? (
        <ConfirmDialog
          title={t("history.delete")}
          body={t("history.deleteBody", { name: deleting })}
          confirmLabel={t("history.delete")}
          danger
          onCancel={() => setDeleting(null)}
          onConfirm={async () => {
            const name = deleting;
            setDeleting(null);
            const r = await actions.act((c, p) => c.deleteBranch(p, name));
            if (r.ok) {
              actions.notify("success", t("history.deleted", { name }));
              await onChanged();
            }
          }}
        />
      ) : null}
      {dirty ? (
        <Dialog
          title={t("history.switchDirty")}
          onClose={() => setDirty(null)}
          footer={
            <>
              <Button onClick={() => setDirty(null)}>{t("common.cancel")}</Button>
              <Button
                variant="primary"
                onClick={async () => {
                  const name = dirty.name;
                  setDirty(null);
                  const r = await actions.act(async (c, p) => {
                    await c.shelve(p, `Before switching to ${name}`);
                    return c.switchBranch(p, name);
                  });
                  if (r.ok) {
                    actions.notify("success", t("history.switched", { name }));
                    await onChanged();
                  }
                }}
              >
                {t("history.switchShelve")}
              </Button>
            </>
          }
        >
          <p>{t("history.switchDirtyBody", { paths: dirty.paths.join(", ") })}</p>
        </Dialog>
      ) : null}
    </Section>
  );
}
