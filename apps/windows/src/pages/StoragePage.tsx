import { useCallback, useEffect, useState } from "react";
import { Button, ConfirmDialog, EmptyNote, Section } from "../components/ui";
import { useI18n } from "../i18n";
import { dedupRatio, formatBytes } from "../rgit/format";
import type { GcData, ReindexData, ShelvesData, StatsData, VerifyData } from "../rgit/types";
import { RgitCancelled } from "../rgit/errors";
import { errorText } from "../state/errors";
import { useApp } from "../state/app";

export function StoragePage() {
  const i18n = useI18n();
  const { t, intlLocale } = i18n;
  const { state, actions } = useApp();
  const project = state.project;
  const [stats, setStats] = useState<StatsData | null>(null);
  const [shelves, setShelves] = useState<ShelvesData | null>(null);
  const [verify, setVerify] = useState<VerifyData | null>(null);
  const [gc, setGc] = useState<GcData | null>(null);
  const [reindex, setReindex] = useState<ReindexData | null>(null);
  const [aggressive, setAggressive] = useState(false);
  const [confirmGc, setConfirmGc] = useState(false);
  const [dropShelf, setDropShelf] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!project) return;
    const [s, sh] = await Promise.all([
      actions.act((c, p) => c.stats(p), { refresh: false, notify: false, busy: false }),
      actions.act((c, p) => c.shelves(p), { refresh: false, notify: false, busy: false }),
    ]);
    if (s.ok) setStats(s.value);
    if (sh.ok) setShelves(sh.value);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project]);

  useEffect(() => {
    setStats(null);
    setShelves(null);
    setVerify(null);
    setGc(null);
    setReindex(null);
    void load();
  }, [load]);

  if (!project) return <div className="page"><h1>{t("nav.storage")}</h1><EmptyNote>{t("storage.noProject")}</EmptyNote></div>;

  const busy = state.busy || state.op !== null;
  const fail = (e: unknown) => {
    if (!(e instanceof RgitCancelled)) actions.notify("error", errorText(i18n, e));
  };
  const ratio = stats ? dedupRatio(stats.logical_bytes, stats.stored_bytes) : null;
  const num = (n: number) => new Intl.NumberFormat(intlLocale).format(n);

  const runVerify = async () => {
    setVerify(null);
    const r = await actions.runOp("verify", (c, o) => c.verify(project, o));
    if (r.ok) setVerify(r.value);
    else fail(r.error);
  };
  const runGc = async () => {
    setGc(null);
    const r = await actions.runOp("gc", (c, o) => c.gc(project, aggressive, o));
    if (r.ok) {
      setGc(r.value);
      await load();
    } else fail(r.error);
  };
  const runReindex = async () => {
    const r = await actions.act((c, p) => c.reindex(p));
    if (r.ok) setReindex(r.value);
  };
  const problemCount = (p: ReindexData["problems"]) => (Array.isArray(p) ? p.length : p);

  return (
    <div className="page">
      <h1>{t("storage.title")}</h1>
      <Section title={t("storage.stats")}>
        {!stats ? (
          <p className="muted">{t("common.loading")}</p>
        ) : (
          <dl className="stats">
            <div><dt>{t("storage.commits")}</dt><dd>{num(stats.commits)}</dd></div>
            <div><dt>{t("storage.branches")}</dt><dd>{num(stats.branches)}</dd></div>
            <div><dt>{t("storage.objects")}</dt><dd>{num(stats.objects)}</dd></div>
            <div><dt>{t("storage.logical")}</dt><dd>{formatBytes(stats.logical_bytes, intlLocale)}</dd></div>
            <div><dt>{t("storage.stored")}</dt><dd>{formatBytes(stats.stored_bytes, intlLocale)}</dd></div>
            <div><dt>{t("storage.ratio")}</dt><dd>{ratio ? `${ratio.toFixed(1)}×` : "–"}</dd></div>
            <div><dt>{t("storage.repo")}</dt><dd>{formatBytes(stats.repo_bytes, intlLocale)}</dd></div>
            <div><dt>{t("storage.packs")}</dt><dd>{num(stats.pack_files)}</dd></div>
            <div><dt>{t("storage.deltas")}</dt><dd>{num(stats.delta_objects)}</dd></div>
          </dl>
        )}
      </Section>

      <Section title={t("storage.maintenance")}>
        <div className="maint">
          <div>
            <Button onClick={() => void runVerify()} disabled={busy}>{t("storage.verify")}</Button>
            <p className="hint">{t("storage.verifyHelp")}</p>
            {verify ? (
              verify.problems.length === 0 ? (
                <p role="status" className="ok-text">{t("storage.verifyOk", { objects: num(verify.objects_checked), commits: num(verify.commits) })}</p>
              ) : (
                <div role="alert">
                  <p className="error-text">{t("storage.verifyProblems", { n: verify.problems.length })}</p>
                  <ul className="log">{verify.problems.map((p, i) => <li key={i}>{p}</li>)}</ul>
                </div>
              )
            ) : null}
          </div>
          <div>
            <Button onClick={() => setConfirmGc(true)} disabled={busy}>{t("storage.gc")}</Button>
            <label className="check">
              <input type="checkbox" checked={aggressive} onChange={(e) => setAggressive(e.target.checked)} />
              {t("storage.gcAggressive")}
            </label>
            <p className="hint">{t("storage.gcHelp")}</p>
            {gc ? (
              <p role="status" className="ok-text">
                {t("storage.gcDone", { freed: formatBytes(Math.max(0, gc.freed_bytes), intlLocale), after: formatBytes(gc.repo_bytes_after, intlLocale) })}
              </p>
            ) : null}
          </div>
          <div>
            <Button onClick={() => void runReindex()} disabled={busy}>{t("storage.reindex")}</Button>
            <p className="hint">{t("storage.reindexHelp")}</p>
            {reindex ? (
              <p role="status" className={problemCount(reindex.problems) ? "error-text" : "ok-text"}>
                {t("storage.reindexDone", { objects: num(reindex.objects), packs: num(reindex.packs) })}
              </p>
            ) : null}
          </div>
        </div>
      </Section>

      <Section title={t("storage.shelves")}>
        {!shelves || shelves.shelves.length === 0 ? (
          <EmptyNote>{t("storage.shelvesNone")}</EmptyNote>
        ) : (
          <ul className="rows">
            {shelves.shelves.map((s) => (
              <li key={s.ref} className="row">
                <div className="row-main"><strong>{s.ref}</strong> <span className="muted">{s.message}</span></div>
                <div className="row-actions">
                  <Button
                    disabled={busy}
                    aria-label={`${t("storage.unshelve")}: ${s.ref}`}
                    onClick={async () => {
                      const r = await actions.act((c, p) => c.unshelve(p, s.name));
                      if (r.ok) {
                        actions.notify("success", t("storage.unshelved"));
                        await load();
                      }
                    }}
                  >
                    {t("storage.unshelve")}
                  </Button>
                  <Button variant="quiet" disabled={busy} aria-label={`${t("storage.dropShelf")}: ${s.ref}`} onClick={() => setDropShelf(s.name)}>
                    {t("storage.dropShelf")}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
        {shelves && shelves.recovery.length > 0 ? <p className="muted">{t("storage.recoveryCount", { n: shelves.recovery.length })}</p> : null}
      </Section>

      <Section title={t("storage.guidance")}>
        <ul>
          <li>{t("storage.guidance1")}</li>
          <li>{t("storage.guidance2")}</li>
          <li>{t("storage.guidance3")}</li>
        </ul>
      </Section>

      {confirmGc ? (
        <ConfirmDialog
          title={t("storage.gc")}
          body={t("storage.gcHelp")}
          confirmLabel={t("storage.gc")}
          onCancel={() => setConfirmGc(false)}
          onConfirm={() => {
            setConfirmGc(false);
            void runGc();
          }}
        />
      ) : null}
      {dropShelf ? (
        <ConfirmDialog
          title={t("storage.dropShelf")}
          body={dropShelf}
          confirmLabel={t("storage.dropShelf")}
          danger
          onCancel={() => setDropShelf(null)}
          onConfirm={async () => {
            const id = dropShelf;
            setDropShelf(null);
            const r = await actions.act((c, p) => c.dropShelf(p, id));
            if (r.ok) await load();
          }}
        />
      ) : null}
    </div>
  );
}
