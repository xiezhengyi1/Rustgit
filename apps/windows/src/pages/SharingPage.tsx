import { useEffect, useState } from "react";
import { Button, EmptyNote, Field, Section } from "../components/ui";
import { useI18n } from "../i18n";
import { useApp } from "../state/app";
import { copyText, joinAddresses } from "../rgit/hosting";
import { RgitError } from "../rgit/errors";
import { errorText } from "../state/errors";
import type { DeviceIdentity, Invitation, EnrolledDevice, DiscoveredHost } from "../rgit/types";

export function SharingPage() {
  const { t } = useI18n();
  const { state, actions } = useApp();
  const [name, setName] = useState("origin");
  const [url, setUrl] = useState("");
  const [hostKey, setHostKey] = useState("");
  const [identity, setIdentity] = useState<DeviceIdentity>();
  const [invitations, setInvitations] = useState<Invitation[]>([]);
  const [devices, setDevices] = useState<EnrolledDevice[]>([]);
  const [nearby, setNearby] = useState<DiscoveredHost[]>([]);
  const [uri, setUri] = useState("");
  const [role, setRole] = useState("read");
  const [expires, setExpires] = useState("24h");
  const [label, setLabel] = useState("");
  const [singleUse, setSingleUse] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [revoke, setRevoke] = useState<EnrolledDevice>();
  const host = state.serve.dir;
  const refresh = async () => {
    if (host) {
      const [i, d] = await Promise.all([actions.client.invitations(host), actions.client.devices(host)]);
      setInvitations(i.invitations); setDevices(d.devices);
    }
  };
  const perform = async (work: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true); setError("");
    try { await work(); await refresh(); }
    catch (e) { setError(errorText({ t }, e)); }
    finally { setBusy(false); }
  };
  useEffect(() => {
    let active = true;
    actions.client.deviceIdentity().then(v => { if (active) setIdentity(v); }).catch(e => { if (active) setError(errorText({ t }, e)); });
    if (host) void refresh().catch(e => { if (active) setError(errorText({ t }, e)); });
    return () => { active = false; };
  }, [host]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    const controller = new AbortController();
    void actions.client.discover({ signal: controller.signal, onEvent: event => {
      if (controller.signal.aborted) return;
      if (event.event === "host" && event.host) {
        const h = event.host as DiscoveredHost;
        setNearby(old => [...old.filter(v => v.instance !== h.instance), h]);
      } else if (event.event === "lost") setNearby(old => old.filter(v => v.instance !== event.instance));
    } }).catch(e => { if (!controller.signal.aborted) setError(errorText({ t }, e)); });
    return () => controller.abort();
  }, [actions.client]);
  const roleOptions = <><option value="read">{t("access.read")}</option><option value="write">{t("access.write")}</option><option value="admin">{t("access.admin")}</option></>;
  return <div className="page">
    <h1>{t("sharing.title")}</h1>
    {error && <p role="alert" className="error-text">{error}</p>}
    <Section title={t("sharing.remotes")}>
      {!state.project ? <EmptyNote>{t("changes.noProject")}</EmptyNote> : <>
        <ul className="rows">{state.remotes.map(r => <li key={r.name} className="row"><div className="row-main">
          <strong>{r.name}</strong> <code>{r.url}</code>
          <p>{t(r.secure ? "access.encrypted" : "access.unpinned")}</p><code>{r.host_key}</code>
        </div></li>)}</ul>
        {state.connection && <p role="status">{t(`access.${state.connection.state}`)} {state.connection.error}</p>}
        <form onSubmit={e => { e.preventDefault(); void perform(async () => {
          const result = await actions.act((c, p) => c.addRemote(p, name.trim(), url.trim(), hostKey.trim() || undefined));
          if (!result.ok) throw result.error;
          setUrl("");
        }); }}>
          <Field label={t("sharing.remoteName")}>{p => <input {...p} value={name} onChange={e => setName(e.target.value)} />}</Field>
          <Field label={t("sharing.remoteUrl")} hint={t("projects.cloneAddressHint")}>{p => <input {...p} value={url} onChange={e => setUrl(e.target.value)} />}</Field>
          <Field label={t("access.fingerprint")}>{p => <input {...p} value={hostKey} onChange={e => setHostKey(e.target.value)} />}</Field>
          <Button type="submit" disabled={busy || state.busy || !name.trim() || !url.trim()}>{t("sharing.addRemote")}</Button>
        </form>
      </>}
    </Section>
    <Section title={t("access.identity")}>
      {identity && <><code>{identity.fingerprint}</code><p>{identity.key_storage}</p></>}
      <p>{t("access.security")}</p>
    </Section>
    <Section title={t("sharing.invitations")}>
      {!host ? <p>{t("access.hostOnly")}</p> : <>
        <p>{t("access.secret")}</p>
        <Field label={t("access.role")}>{p => <select {...p} value={role} onChange={e => setRole(e.target.value)}>{roleOptions}</select>}</Field>
        <Field label={t("access.expiry")}>{p => <input {...p} value={expires} onChange={e => setExpires(e.target.value)} />}</Field>
        <Field label={t("access.label")}>{p => <input {...p} value={label} onChange={e => setLabel(e.target.value)} />}</Field>
        <label><input type="checkbox" checked={singleUse} onChange={e => setSingleUse(e.target.checked)} /> {t("access.singleUse")}</label>
        <Button disabled={busy || !state.serve.running || !state.serve.addr} onClick={() => void perform(async () => {
          const addresses = await actions.platform.lanAddresses();
          const hints = joinAddresses(state.serve.addr, addresses).list.map(v => v.url);
          if (state.serve.irohUrl) hints.unshift(state.serve.irohUrl);
          const result = await actions.client.createInvitation(host, role, expires, singleUse, label, hints);
          setUri(result.uri);
        })}>{t("access.create")}</Button>
        {uri && <div><p className="path">{uri}</p><Button onClick={() => void copyText(uri)}>{t("access.copy")}</Button><Button onClick={() => setUri("")}>{t("access.hide")}</Button></div>}
        <ul className="rows">{invitations.map(i => <li key={i.id} className="row"><div className="row-main">
          <strong>{i.label || i.id.slice(0, 12)}</strong><p>{i.role} · {i.status} · {i.uses}/{i.max_uses} · {new Date(i.expires * 1000).toLocaleString()}</p>
        </div>{i.status === "active" && <Button disabled={busy} onClick={() => void perform(async () => { await actions.client.revokeInvitation(host, i.id); setUri(""); })}>{t("access.revokeInvite")}</Button>}</li>)}</ul>
        <h3>{t("sharing.devices")}</h3>
        <ul className="rows">{devices.map(d => <li key={d.fingerprint} className="row"><div className="row-main">
          <strong>{d.name || d.fingerprint.slice(0, 16)}</strong><p className="path">{d.fingerprint}</p><p>{d.status} · {d.last_seen ? new Date(d.last_seen * 1000).toLocaleString() : "—"}</p>
          {d.status === "active" && <><select aria-label={t("access.role")} disabled={busy} value={d.role} onChange={e => void perform(() => actions.client.setDeviceRole(host, d.fingerprint, e.target.value))}>{roleOptions}</select><Button disabled={busy} variant="danger" onClick={() => setRevoke(d)}>{t("sharing.revocation")}</Button></>}
        </div></li>)}</ul>
        {revoke && <div role="alertdialog" aria-label={t("sharing.revocation")}><p>{t("access.revokeWarning")}</p><code>{revoke.fingerprint}</code><Button onClick={() => setRevoke(undefined)}>{t("common.cancel")}</Button><Button variant="danger" onClick={() => { void perform(() => actions.client.revokeDevice(host, revoke.fingerprint)); setRevoke(undefined); }}>{t("sharing.revocation")}</Button></div>}
        <Button disabled={busy} onClick={() => void perform(async () => {})}>{t("access.refresh")}</Button>
      </>}
    </Section>
    <Section title={t("access.nearby")}>
      <p>{t("access.hints")}</p>
      <ul className="rows">{nearby.map(h => <li key={h.instance}>{h.urls.map(address => <p key={address}><code>{address}</code><Button onClick={() => setUrl(address)}>{t("access.useAddress")}</Button></p>)}</li>)}</ul>
    </Section>
    <Section title={t("sharing.hosting")}>
      <code>{state.serve.fingerprint}</code><p>{t("status.sleepNote")}</p>
      {state.project && host && state.serve.running && state.serve.addr && <Button disabled={busy || state.busy} onClick={() => void perform(async () => {
        const port = state.serve.addr!.split(":").at(-1)!;
        const address = `tcp://127.0.0.1:${port}`;
        const existing = state.remotes.find(remote => remote.url === address);
        if (existing?.host_key && existing.host_key !== state.serve.fingerprint) throw new RgitError("host_key_mismatch", "Host identity mismatch");
        let remote = existing?.name ?? "origin";
        if (!existing) {
          let n = 1;
          while (state.remotes.some(r => r.name === remote)) remote = `hosted-${n++}`;
        }
        const result = await actions.act(async (c, p) => {
          if (!existing?.host_key) {
            const invitation = await c.createInvitation(host, "admin", "24h", true, "Host owner", [address]);
            await c.enroll(p, invitation.uri, remote);
          }
          await c.call(["push", remote], { project: p });
        });
        if (!result.ok) throw result.error;
      })}>{t("access.publishHosted")}</Button>}

      {state.serve.irohUrl && <><code>{state.serve.irohUrl}</code><Button onClick={() => void copyText(state.serve.irohUrl!)}>{t("access.copyAddress")}</Button>{state.serve.relayOnline === false && <p>{t("access.relayOffline")}</p>}</>}
      <Button onClick={() => actions.setPage("projects")}>{t("nav.projects")}</Button>
    </Section>
  </div>;
}
