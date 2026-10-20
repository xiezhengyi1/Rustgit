import { useEffect, useState } from "react";
import { Button, Dialog, EmptyNote, Field, ProgressBar, Section } from "../components/ui";
import { useI18n } from "../i18n";
import type { HostingConfig, LanAddress } from "../platform/types";
import { DEFAULT_PORT, copyText, isValidPort, joinAddresses, listenFor, portOf } from "../rgit/hosting";
import { baseName } from "../rgit/format";
import { useApp } from "../state/app";

function joinPath(parent: string, name: string): string {
  const sep = parent.includes("\\") && !parent.includes("/") ? "\\" : "/";
  return parent.replace(/[\\/]+$/, "") + sep + name;
}

export function ProjectsPage() {
  const { t } = useI18n();
  const { state, actions } = useApp();

  return (
    <div className="page">
      <h1>{t("projects.title")}</h1>
      <CurrentProject />
      <CloneSection />
      <HostSection />
      {state.pendingInit ? <InitDialog path={state.pendingInit} /> : null}
      <Section title={t("projects.recent")}>
        {state.settings.recentProjects.length === 0 ? (
          <EmptyNote>{t("projects.recentNone")}</EmptyNote>
        ) : (
          <ul className="rows">
            {state.settings.recentProjects.map((p) => (
              <li key={p} className="row">
                <div className="row-main">
                  <strong>{baseName(p)}</strong> <span className="muted path">{p}</span>
                </div>
                <div className="row-actions">
                  <Button
                    onClick={async () => {
                      if (await actions.openProject(p)) actions.setPage("changes");
                    }}
                    aria-label={`${t("projects.openThis")}: ${p}`}
                  >
                    {t("projects.openThis")}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function CurrentProject() {
  const { t } = useI18n();
  const { state, actions } = useApp();

  const pick = async (title: string) => {
    const dir = await actions.platform.pickDirectory(title);
    if (!dir) return;
    if (await actions.openProject(dir)) actions.setPage("changes");
  };

  return (
    <Section title={t("projects.current")}>
      {state.project ? (
        <p>
          <strong>{baseName(state.project)}</strong> <span className="muted path">{state.project}</span>
        </p>
      ) : (
        <EmptyNote>{t("projects.none")}</EmptyNote>
      )}
      <div className="button-row">
        <Button variant="primary" onClick={() => void pick(t("projects.pickCreate"))} disabled={state.busy || !!state.op}>
          {t("projects.create")}
        </Button>
        <Button onClick={() => void pick(t("projects.pickOpen"))} disabled={state.busy}>
          {t("projects.open")}
        </Button>
      </div>
    </Section>
  );
}

function InitDialog({ path }: { path: string }) {
  const { t } = useI18n();
  const { actions } = useApp();
  const [branch, setBranch] = useState("main");
  return (
    <Dialog
      title={t("projects.initTitle")}
      onClose={actions.cancelInit}
      footer={
        <>
          <Button onClick={actions.cancelInit}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            onClick={async () => {
              if (await actions.createProject(path, branch.trim())) actions.setPage("changes");
            }}
          >
            {t("common.create")}
          </Button>
        </>
      }
    >
      <p>{t("projects.initBody", { path })}</p>
      <Field label={t("projects.initBranch")}>
        {(p) => <input {...p} value={branch} onChange={(e) => setBranch(e.target.value)} />}
      </Field>
    </Dialog>
  );
}

function CloneSection() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const [url, setUrl] = useState("");
  const [hostKey, setHostKey] = useState("");
  useEffect(() => { if (state.pendingInvitation) setUrl(state.pendingInvitation); }, [state.pendingInvitation]);
  const [parent, setParent] = useState("");
  const [name, setName] = useState("");
  const cloning = state.op?.kind === "clone";
  const ready = url.trim() !== "" && parent !== "" && name.trim() !== "";

  return (
    <Section title={t("projects.cloneTitle")}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!ready || state.busy) return;
          void actions.clone(url.trim(), joinPath(parent, name.trim()), hostKey.trim() || undefined).then((r) => {
            if (r.ok) actions.setPage("changes");
          });
        }}
      >
        <Field label={t("projects.cloneAddress")} hint={t("projects.cloneAddressHint")}>
          {(p) => <input {...p} value={url} onChange={(e) => setUrl(e.target.value)} placeholder="tcp://host:7878" />}
        </Field>
        {!url.startsWith("projectapp://") && <Field label={t("access.fingerprint")}>{p => <input {...p} value={hostKey} onChange={e => setHostKey(e.target.value)} />}</Field>}
        <Field label={t("projects.cloneDest")} hint={t("projects.cloneDestHint")}>
          {(p) => (
            <div className="input-row">
              <input {...p} value={parent} onChange={(e) => setParent(e.target.value)} />
              <Button
                onClick={async () => {
                  const d = await actions.platform.pickDirectory(t("projects.cloneDest"));
                  if (d) setParent(d);
                }}
              >
                {t("common.browse")}
              </Button>
            </div>
          )}
        </Field>
        <Field label={t("projects.cloneName")}>
          {(p) => <input {...p} value={name} onChange={(e) => setName(e.target.value)} />}
        </Field>
        <div className="button-row">
          <Button variant="primary" type="submit" disabled={!ready || state.busy}>
            {t("projects.cloneStart")}
          </Button>
          {cloning ? (
            <Button onClick={actions.cancelOp}>
              {t("common.cancel")}
            </Button>
          ) : null}
        </div>
        {cloning && state.op ? (
          <div aria-live="polite">
            <ProgressBar done={state.op.done} total={state.op.total} label={t("op.clone")} />
            <p className="muted">{t("projects.cloneRunning", { phase: state.op.phase ?? "" })}</p>
          </div>
        ) : null}
      </form>
    </Section>
  );
}

function HostSection() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const saved = state.settings.hosting;
  const [dir, setDir] = useState(saved?.dir ?? "");
  const [init, setInit] = useState(saved?.init ?? true);
  const [port, setPort] = useState(portOf(saved?.listen) ?? String(DEFAULT_PORT));
  const [relay, setRelay] = useState(saved?.relay ?? "");
  const [lan, setLan] = useState(saved?.insecureListenAny ?? false);
  const [addresses, setAddresses] = useState<LanAddress[]>([]);
  useEffect(() => {
    if (saved) {
      setDir(saved.dir);
      setInit(saved.init);
      setPort(portOf(saved.listen) ?? String(DEFAULT_PORT));
      setLan(saved.insecureListenAny);
    }
  }, [saved?.dir]); // eslint-disable-line react-hooks/exhaustive-deps
  const serve = state.serve;
  const portOk = isValidPort(port);
  const cfg: HostingConfig = { dir: dir.trim(), init, listen: listenFor(lan, port), insecureListenAny: lan, relay: relay.trim() || undefined };

  const loadAddresses = () => void actions.platform.lanAddresses().then(setAddresses).catch(() => setAddresses([]));
  const reachable = serve.running && serve.addr && !joinAddresses(serve.addr, []).local;
  useEffect(() => {
    if (reachable) loadAddresses();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reachable, serve.addr]);
  const join = serve.running ? joinAddresses(serve.addr, addresses) : null;

  const copy = async (text: string) => {
    if (await copyText(text)) actions.notify("success", t("projects.hostCopied"));
  };

  return (
    <Section title={t("projects.hostTitle")}>
      <p>{t("projects.hostIntro")}</p>
      <p className="muted">{t("status.sleepNote")}</p>
      {serve.running ? (
        <div role="status">
          <p>
            <span className="dot dot-on" aria-hidden="true" />{" "}
            {t("projects.hostRunning", { dir: serve.dir ?? dir, addr: serve.addr ?? "…" })}
          </p>
        </div>
      ) : serve.lastError ? (
        <p className="error-text" role="alert">
          {t("status.serverError", { error: serve.lastError })}
        </p>
      ) : null}
      {join ? (
        <div className="join" role="group" aria-label={t("projects.hostJoin")}>
          <h3>{t("projects.hostJoin")}</h3>
          {join.local ? <p className="muted">{t("projects.hostLocalOnly")}</p> : null}
          {!join.local && join.list.length === 0 ? <p className="warn">{t("projects.hostNoAddresses")}</p> : null}
          <ul className="rows">
            {join.list.map((a) => (
              <li key={a.url} className="row join-row">
                <div className="row-main">
                  <code data-testid="join-url">{a.url}</code>
                  {a.label ? <span className="muted">({a.label})</span> : null}
                  <span className="muted small">{t("projects.hostCloneCmd", { cmd: a.clone })}</span>
                </div>
                <div className="row-actions">
                  <Button onClick={() => void copy(a.url)} aria-label={t("projects.hostJoinCopyLabel", { text: a.url })}>
                    {t("projects.hostJoinCopy")}
                  </Button>
                  <Button variant="quiet" onClick={() => void copy(a.clone)} aria-label={t("projects.hostJoinCopyLabel", { text: a.clone })}>
                    {t("projects.hostJoinCopy")} {t("projects.hostCloneShort")}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
          {!join.local ? (
            <>
              <p className="muted">{t("projects.hostJoinHow")}</p>
              <p className="muted">{t("projects.hostSleep")}</p>
              <p className="muted">{t("projects.hostFirewall")}</p>
              <Button variant="quiet" onClick={loadAddresses}>
                {t("projects.hostRefreshAddresses")}
              </Button>
            </>
          ) : null}
        </div>
      ) : null}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!serve.running && cfg.dir && portOk) void actions.startHosting(cfg);
        }}
      >
        <Field label={t("projects.hostDir")}>
          {(p) => (
            <div className="input-row">
              <input {...p} value={dir} onChange={(e) => setDir(e.target.value)} disabled={serve.running} />
              <Button
                disabled={serve.running}
                onClick={async () => {
                  const d = await actions.platform.pickDirectory(t("projects.hostDir"));
                  if (d) setDir(d);
                }}
              >
                {t("common.browse")}
              </Button>
            </div>
          )}
        </Field>
        <label className="check">
          <input type="checkbox" checked={init} onChange={(e) => setInit(e.target.checked)} disabled={serve.running} />
          {t("projects.hostInit")}
        </label>
        <Field label={t("projects.hostListen")} hint={t("projects.hostListenHint")}>
          {(p) => (
            <input
              {...p}
              inputMode="numeric"
              value={port}
              aria-invalid={!portOk}
              onChange={(e) => setPort(e.target.value.trim())}
              disabled={serve.running}
            />
          )}
        </Field>
        <label className="check">
          <input type="checkbox" checked={lan} onChange={(e) => setLan(e.target.checked)} disabled={serve.running} />
          {t("projects.hostLan")}
        </label>
        {lan ? (
          <p className="warn" role="note">
            {t("projects.hostLanWarn")}
          </p>
        ) : null}
        <Field label={t("access.relay")} hint={t("access.relayHint")}>{p => <input {...p} value={relay} onChange={e => setRelay(e.target.value)} disabled={serve.running} />}</Field>
        <label className="check">
          <input
            type="checkbox"
            checked={state.settings.startHostingOnLaunch}
            onChange={(e) => void actions.saveSettings({ startHostingOnLaunch: e.target.checked, hosting: saved ?? cfg })}
          />
          {t("projects.hostStartLogin")}
        </label>
        <div className="button-row">
          {serve.running ? (
            <Button variant="danger" onClick={() => void actions.stopHosting()}>
              {t("projects.hostStop")}
            </Button>
          ) : (
            <Button variant="primary" type="submit" disabled={!cfg.dir || !portOk}>
              {t("projects.hostStart")}
            </Button>
          )}
        </div>
      </form>
      <h3>{t("projects.hostActivity")}</h3>
      {state.hostLog.length === 0 ? (
        <EmptyNote>{t("projects.hostNoActivity")}</EmptyNote>
      ) : (
        <ul className="log" aria-label={t("projects.hostActivity")}>
          {state.hostLog.slice(-30).map((e, i) => (
            <li key={i}>
              <code>{e.event}</code> <span className="muted">{Object.entries(e).filter(([k]) => k !== "event").map(([k, v]) => `${k}=${String(v)}`).join(" ")}</span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
