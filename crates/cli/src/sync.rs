//! `serve`, `clone`, `fetch`, `push`, `pull` and `remote`.

use std::fs::File;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustgit_core::{Error, Repo};
use rustgit_host::{Authorizer, DeviceAuthorizer, Host, HostConfig, OpenAccess};
// ssh:// and tcp:// remotes (net.rs is the one place URL schemes become transports)
use crate::net::transport_for as transport_from_url;
use rustgit_protocol::{
    clone_into, with_retries, CancelToken, Client, ClientConfig, ErrorCode, PushOptions,
    PushOutcome, TcpListenerAdapter, TransferStats,
};

use serde_json::json;

use crate::{out, say, Cli, Cmd};

thread_local! {
    static RELATION: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
}

fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

fn proto(e: rustgit_protocol::Error) -> Error {
    if let Some(tagged) = crate::auth::tag_proto(&e) {
        return tagged;
    }
    match e {
        rustgit_protocol::Error::Remote(w) => Error::Other(w.message),
        rustgit_protocol::Error::Core(c) => c,
        // failures of the ssh client already read as full sentences
        rustgit_protocol::Error::Io(e) if e.to_string().starts_with("ssh ") => Error::Other(e.to_string()),
        other_err => Error::Other(other_err.to_string()),
    }
}

/// A hosted repository has no working directory; most commands make no sense there.
/// Returns the maintenance lock to hold for `gc` / `reindex`.
pub fn guard_hosted(repo: &Repo, cmd: &Cmd) -> Result<Option<File>, Error> {
    if !Host::is_hosted(&repo.root) {
        return Ok(None);
    }
    match cmd {
        Cmd::Log { .. } | Cmd::Show { .. } | Cmd::Verify | Cmd::Stats | Cmd::Config { .. } => {
            Ok(None)
        }
        Cmd::Branch { name: None, .. } => Ok(None),
        Cmd::Gc { .. } | Cmd::Reindex => host_lock(repo),
        _ => Err(Error::Other(
            "this is a hosted repository without a working directory; use `rgit clone` to work on it"
                .into(),
        )),
    }
}

fn host_lock(repo: &Repo) -> Result<Option<File>, Error> {
    rustgit_host::maintenance_lock(&repo.root)
        .map(Some)
        .map_err(|e| match e {
            rustgit_host::Error::Locked(_) => Error::Other(
                "this repository is being served; stop `rgit serve` before running maintenance"
                    .into(),
            ),
            e => other(e),
        })
}

pub fn serve(cli: Cli) -> Result<u8, Error> {
    let Cmd::Serve {
        dir,
        init,
        listen,
        port_file,
        insecure_listen_any,
        insecure_open_access,
        allow_force,
        allow_delete,
        max_session_bytes,
        max_repo_bytes,
        log,
        stdio,
        advertise,
        iroh,
    } = cli.cmd
    else {
        unreachable!()
    };
    let dir = dir.unwrap_or(cli.dir);
    if init && !Host::is_hosted(&dir) {
        if dir.join(".rgit").exists() {
            return Err(Error::Other(format!(
                "{} already holds a working project; hosting is only done from a separate bare repository",
                dir.display()
            )));
        }
        Host::init(&dir).map_err(other)?;
        eprintln!("Created hosted repository in {}", dir.display());
    }
    let addrs: Vec<_> = listen.to_socket_addrs().map_err(other)?.collect();
    if addrs.is_empty() {
        return Err(Error::Other(format!("cannot resolve {listen}")));
    }
    if insecure_open_access && addrs.iter().any(|a| !a.ip().is_loopback()) {
        return Err(Error::Other(format!(
            "refusing to listen on {listen} with --insecure-open-access: open access (no authentication, \
             no encryption) is only allowed on loopback. Drop the flag to serve with device authentication \
             and encryption, then `rgit invite create` to let other devices join."
        )));
    }
    if insecure_listen_any && !stdio {
        eprintln!("note: --insecure-listen-any is no longer needed and has no effect");
    }
    let mut cfg = HostConfig {
        allow_force,
        allow_delete,
        log,
        ..HostConfig::default()
    };
    if let Some(n) = max_session_bytes {
        cfg.max_session_bytes = n;
    }
    cfg.max_repo_bytes = max_repo_bytes;
    // Device-authenticated, encrypted serving is the default; plaintext open access needs the flag.
    let mut host_fp: Option<String> = None;
    let auth: Arc<dyn Authorizer> = if insecure_open_access {
        eprintln!(
            "warning: --insecure-open-access: NO authentication and NO encryption; anyone who can \
             connect to this loopback port can read and change the project"
        );
        Arc::new(OpenAccess)
    } else {
        let uuid = Repo::open(&dir)?.uuid()?;
        let store = Arc::new(rustgit_host::AuthStore::open(&dir).map_err(other)?);
        let (key, created) = rustgit_host::ensure_host_identity(&uuid, &crate::auth::key_storage(), &store)
            .map_err(|e| crate::auth::host_key_error(e))?;
        if created {
            eprintln!("Created the host identity key for this project.");
        }
        let chain = store.successions().map_err(other)?;
        host_fp = Some(key.fingerprint().to_hex());
        Arc::new(DeviceAuthorizer::new(store, key, chain))
    };
    let host = Host::open(&dir, cfg, auth).map_err(|e| match e {
        rustgit_host::Error::Locked(p) => Error::Other(format!(
            "{} is already being served by another process",
            p.display()
        )),
        e => other(e),
    })?;
    if stdio {
        return crate::net::run_stdio(host);
    }
    let listener = TcpListenerAdapter::bind(&listen).map_err(other)?;
    let bound = listener.socket_addr().map_err(other)?;
    say!("Serving project {} from {} on {}", host.uuid(), dir.display(), bound);
    match &host_fp {
        Some(fp) => say!("Host identity {fp}; devices join with `rgit invite create` links"),
        None => say!("OPEN ACCESS: no authentication, no encryption (loopback only)"),
    }
    if host_fp.is_some() && !bound.ip().is_unspecified() {
        if let Ok(store) = rustgit_host::AuthStore::open(&dir) {
            let _ = store.set_meta("listen", &bound.to_string());
        }
    }
    out::event(json!({
        "event": "listening",
        "addr": bound.to_string(),
        "project": host.uuid(),
        "secure": host_fp.is_some(),
        "fingerprint": host_fp,
    }));
    if let Some(pf) = port_file {
        let tmp = pf.with_extension("tmp");
        std::fs::write(&tmp, bound.to_string())?;
        std::fs::rename(&tmp, &pf)?;
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    if advertise {
        crate::net::advertise(&host, bound)?;
    }
    let listener = crate::iroh::extend_listener(Box::new(listener), &iroh, &dir, insecure_open_access)?;
    rustgit_host::run(host, listener);
    Ok(0)
}

fn device_name(repo: Option<&Repo>) -> String {
    if let Some(r) = repo {
        if let Ok(Some(n)) = r.get_config("device.name") {
            if !n.is_empty() {
                return n;
            }
        }
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into());
    format!("{user}@{host}")
}

fn remote_url(repo: &Repo, name: &str) -> Result<String, Error> {
    match repo.get_config(&format!("remote.{name}.url"))? {
        Some(u) if !u.is_empty() => Ok(u),
        _ => Err(Error::Other(format!(
            "no remote named '{name}' (add one with `rgit remote add {name} tcp://host:port`)"
        ))),
    }
}

fn client_cfg(repo: &Repo, name: &str) -> Result<ClientConfig, Error> {
    let mut cfg = ClientConfig::new(device_name(Some(repo)));
    cfg.project = repo
        .get_config(&format!("remote.{name}.project"))?
        .filter(|p| !p.is_empty());
    let host_key = repo.get_config(&format!("remote.{name}.host_key"))?;
    crate::auth::secure_client(cfg, host_key.as_deref())
}

fn pin_project(repo: &Repo, name: &str, c: &Client) -> Result<(), Error> {
    let key = format!("remote.{name}.project");
    if repo.get_config(&key)?.unwrap_or_default().is_empty() {
        repo.set_config(&key, &c.hello.repo_uuid)?;
    }
    // The host rotated its key and proved the succession from the pinned one: follow it.
    if let Some(a) = c.host_auth.as_ref().filter(|a| a.rotated) {
        let fp = a.host_key.fingerprint();
        repo.set_config(&format!("remote.{name}.host_key"), &fp.to_hex())?;
        eprintln!("note: the host rotated its identity key (signed by the previous one); now pinned to {}", fp.short());
    }
    Ok(())
}

fn stats_json(s: &TransferStats) -> serde_json::Value {
    json!({
        "objects_transferred": s.objects_transferred,
        "objects_already_complete": s.objects_already_complete,
        "objects_resumed": s.objects_already_staged,
        "logical_bytes": s.logical_bytes,
        "wire_bytes_sent": s.wire_bytes_sent,
        "wire_bytes_received": s.wire_bytes_received,
    })
}

fn hook_progress(c: &mut Client) {
    if out::is_json() {
        c.on_progress(|p| {
            out::event(json!({"event": "progress", "phase": p.phase, "done": p.done, "total": p.total}))
        });
    }
}

fn print_stats(verb: &str, s: &TransferStats) {
    say!(
        "{verb}: {} object(s) ({} logical bytes); already complete: {}; resumed from staged: {}; wire bytes sent: {}, received: {}",
        s.objects_transferred,
        s.logical_bytes,
        s.objects_already_complete,
        s.objects_already_staged,
        s.wire_bytes_sent,
        s.wire_bytes_received
    );
}

pub fn clone(cli: Cli) -> Result<u8, Error> {
    let Cmd::Clone { url, dest, host_key } = cli.cmd else {
        unreachable!()
    };
    let t = transport_from_url(&url).map_err(Error::Other)?;
    let dest: PathBuf = match dest {
        Some(d) => d,
        // An iroh:// URL is an endpoint id plus hints; none of it is a sensible directory name.
        None if url.starts_with("iroh://") => PathBuf::from("project"),
        None => PathBuf::from(
            url.rsplit(['/', ':'])
                .find(|s| !s.is_empty() && s.parse::<u16>().is_err())
                .unwrap_or("project"),
        ),
    };
    let cfg = crate::auth::secure_client(ClientConfig::new(device_name(None)), host_key.as_deref())?;
    let cancel = CancelToken::new();
    let repo = with_retries(&cfg, &cancel, || {
        clone_into(&*t, &url, &cfg, &dest, "origin", cancel.clone())
    })
    .map_err(proto)?;
    let head = repo.head_commit()?;
    crate::result!(json!({
        "dest": dest.to_string_lossy(),
        "branch": repo.current_branch()?,
        "head": head.map(|h| h.to_hex()),
    }));
    match head {
        Some(h) => say!(
            "Cloned into {} (branch {}, {})",
            dest.display(),
            repo.current_branch()?,
            h.short()
        ),
        None => say!("Cloned an empty project into {}", dest.display()),
    }
    Ok(0)
}

fn relation(repo: &Repo, remote: &str) -> Result<(), Error> {
    let cur = repo.current_branch()?;
    let Some(rc) = repo
        .get_ref(&format!("remotes/{remote}/{cur}"))?
        .and_then(|r| r.commit)
    else {
        return Ok(());
    };
    let state = match repo.head_commit()? {
        None => "no_local_commits",
        Some(h) if h == rc => "up_to_date",
        Some(h) if repo.is_ancestor(&h, &rc)? => "behind",
        Some(h) if repo.is_ancestor(&rc, &h)? => "ahead",
        Some(_) => "diverged",
    };
    out::event(json!({"event": "relation", "branch": cur, "state": state}));
    RELATION.with(|r| *r.borrow_mut() = Some(state));
    match repo.head_commit()? {
        None => say!("Branch {cur} has no commits; `rgit merge {remote}/{cur}` to adopt the host's."),
        Some(h) if h == rc => say!("Branch {cur} is up to date with {remote}/{cur}."),
        Some(h) if repo.is_ancestor(&h, &rc)? => say!(
            "Branch {cur} is behind {remote}/{cur}; `rgit merge {remote}/{cur}` fast-forwards it."
        ),
        Some(h) if repo.is_ancestor(&rc, &h)? => {
            say!("Branch {cur} is ahead of {remote}/{cur}; `rgit push` uploads it.")
        }
        Some(_) => say!(
            "Branch {cur} and {remote}/{cur} have diverged; `rgit merge {remote}/{cur}` combines them, then push."
        ),
    }
    Ok(())
}

fn fetch_once(repo: &mut Repo, remote: &str, cfg: &ClientConfig, t: &dyn rustgit_protocol::Transport, cancel: &CancelToken) -> Result<(), Error> {
    let (out, stats) = with_retries(cfg, cancel, || {
        let mut c = Client::connect(t, cfg, cancel.clone())?;
        hook_progress(&mut c);
        pin_project(repo, remote, &c)?;
        let out = c.fetch(repo, remote)?;
        let stats = c.stats.clone();
        c.close();
        Ok((out, stats))
    })
    .map_err(proto)?;
    crate::result!(json!({
        "up_to_date": out.up_to_date,
        "updated": out.updated.iter().map(|f| json!({
            "ref": format!("{}/{}", remote, f.branch),
            "old": f.old.map(|i| i.to_hex()),
            "new": f.new.map(|i| i.to_hex()),
        })).collect::<Vec<_>>(),
        "stats": stats_json(&stats),
    }));
    if out.up_to_date {
        say!("Already up to date");
    } else {
        for f in &out.updated {
            let show = |o: &Option<rustgit_store::ObjectId>| {
                o.map(|i| i.short()).unwrap_or_else(|| "-".into())
            };
            say!("  {}/{}: {} -> {}", remote, f.branch, show(&f.old), show(&f.new));
        }
        print_stats("Fetched", &stats);
    }
    relation(repo, remote)?;
    if let Some(st) = RELATION.with(|r| r.borrow_mut().take()) {
        let mut v = out_result_take();
        v["state"] = json!(st);
        crate::result!(v);
    }
    Ok(())
}

fn out_result_take() -> serde_json::Value {
    out::take_result()
}

pub fn fetch(repo: &mut Repo, remote: &str, watch: bool) -> Result<u8, Error> {
    let url = remote_url(repo, remote)?;
    let t = transport_from_url(&url).map_err(Error::Other)?;
    let cfg = client_cfg(repo, remote)?;
    let cancel = CancelToken::new();
    fetch_once(repo, remote, &cfg, &*t, &cancel)?;
    if !watch {
        return Ok(0);
    }
    // Watch loop: ask the host to tell us when any ref changes, then fetch. Fetch only adds
    // objects and moves remote-tracking refs, so this never touches the working directory.
    loop {
        let mut c = match Client::connect(&*t, &cfg, cancel.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("host unavailable ({e}); retrying in 5s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let mut seq = c.hello.ref_seq;
        let changed = loop {
            match c.watch(seq, Duration::from_secs(30)) {
                Ok((s, true)) => break Some(s),
                Ok((s, false)) => seq = s,
                Err(e) => {
                    eprintln!("watch ended ({e}); reconnecting");
                    break None;
                }
            }
        };
        c.close();
        if changed.is_some() {
            if let Err(e) = fetch_once(repo, remote, &cfg, &*t, &cancel) {
                eprintln!("fetch failed: {e}");
            }
        } else {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

pub fn push(
    repo: &mut Repo,
    remote: &str,
    branch: Option<String>,
    remote_branch: Option<String>,
    force: bool,
    delete: Option<String>,
) -> Result<u8, Error> {
    let url = remote_url(repo, remote)?;
    let t = transport_from_url(&url).map_err(Error::Other)?;
    let cfg = client_cfg(repo, remote)?;
    let cancel = CancelToken::new();
    if let Some(b) = delete {
        let gen = with_retries(&cfg, &cancel, || {
            let mut c = Client::connect(&*t, &cfg, cancel.clone())?;
            pin_project(repo, remote, &c)?;
            let g = c.delete_remote_branch(repo, remote, &b)?;
            c.close();
            Ok(g)
        })
        .map_err(proto)?;
        say!("Deleted {remote}/{b} (generation {gen})");
        crate::result!(json!({"deleted": format!("{remote}/{b}"), "generation": gen}));
        return Ok(0);
    }
    let branch = match branch {
        Some(b) => b,
        None => repo.current_branch()?,
    };
    let opts = PushOptions {
        remote_branch: remote_branch.unwrap_or_else(|| branch.clone()),
        branch,
        remote: remote.to_string(),
        force,
    };
    let res = with_retries(&cfg, &cancel, || {
        let mut c = Client::connect(&*t, &cfg, cancel.clone())?;
        hook_progress(&mut c);
        pin_project(repo, remote, &c)?;
        let o = c.push(repo, &opts);
        let stats = c.stats.clone();
        c.close();
        o.map(|o| (o, stats))
    });
    match res {
        Ok((PushOutcome::UpToDate, _)) => {
            say!("Everything up to date");
            crate::result!(json!({"up_to_date": true, "updated": []}));
        }
        Ok((
            PushOutcome::Pushed {
                new_head,
                generation,
                already_applied,
            },
            stats,
        )) => {
            say!(
                "Pushed {} -> {}/{} at {} (generation {}){}",
                opts.branch,
                remote,
                opts.remote_branch,
                new_head.short(),
                generation,
                if already_applied { " [host had already applied this update]" } else { "" }
            );
            print_stats("Sent", &stats);
            say!("objects sent: {}", stats.objects_transferred);
            crate::result!(json!({
                "up_to_date": false,
                "updated": [{"ref": format!("{}/{}", remote, opts.remote_branch), "new": new_head.to_hex(), "generation": generation}],
                "already_applied": already_applied,
                "stats": stats_json(&stats),
            }));
        }
        Err(e) => {
            if let Some(code) = e.code() {
                if matches!(code, ErrorCode::StaleRef | ErrorCode::NonFastForward) {
                    if out::is_json() {
                        return Err(Error::RefConflict {
                            name: format!("{remote}/{}", opts.remote_branch),
                        });
                    }
                    eprintln!("rejected: {}", proto(e));
                    eprintln!("Your local commits are unchanged. Run `rgit fetch`, then `rgit merge {remote}/{}`, then push again.", opts.remote_branch);
                    return Ok(3);
                }
            }
            return Err(proto(e));
        }
    }
    Ok(0)
}

pub fn pull(repo: &mut Repo, remote: &str) -> Result<u8, Error> {
    fetch(repo, remote, false)?;
    let fetched = out::take_result();
    let cur = repo.current_branch()?;
    if repo
        .get_ref(&format!("remotes/{remote}/{cur}"))?
        .and_then(|r| r.commit)
        .is_none()
    {
        say!("The host has no branch '{cur}'");
        crate::result!(json!({"fetch": fetched, "merge": null}));
        return Ok(0);
    }
    let r = repo.merge(&format!("{remote}/{cur}"))?;
    crate::report_merge(&r);
    crate::result!(json!({"fetch": fetched, "merge": crate::merge_json(&r, None)}));
    Ok(if r.conflicts.is_empty() { 0 } else { 1 })
}

pub fn remote(
    repo: &mut Repo,
    action: Option<String>,
    name: Option<String>,
    url: Option<String>,
    host_key: Option<String>,
) -> Result<u8, Error> {
    match action.as_deref() {
        None | Some("list") => {
            let mut list = Vec::new();
            for (k, u) in repo.config_with_prefix("remote.")? {
                if let Some(name) = k.strip_prefix("remote.").and_then(|r| r.strip_suffix(".url")) {
                    if !u.is_empty() {
                        let hk = repo
                            .get_config(&format!("remote.{name}.host_key"))?
                            .filter(|k| !k.is_empty());
                        say!("{name}\t{u}{}", if hk.is_some() { "\t(encrypted, host pinned)" } else { "\t(PLAINTEXT)" });
                        list.push(json!({"name": name, "url": u, "secure": hk.is_some(), "host_key": hk}));
                    }
                }
            }
            crate::result!(json!({"remotes": list}));
        }
        Some("add") => {
            let (Some(name), Some(url)) = (name, url) else {
                return Err(Error::Other("usage: rgit remote add NAME URL".into()));
            };
            transport_from_url(&url).map_err(Error::Other)?;
            if name.is_empty() || name.contains('/') {
                return Err(Error::InvalidName(name));
            }
            let hk = match host_key {
                Some(h) => h
                    .parse::<rustgit_protocol::Fingerprint>()
                    .map_err(|e| Error::Invalid(format!("--host-key: {e}")))?
                    .to_hex(),
                None => String::new(),
            };
            repo.set_config(&format!("remote.{name}.url"), &url)?;
            repo.set_config(&format!("remote.{name}.project"), "")?;
            repo.set_config(&format!("remote.{name}.host_key"), &hk)?;
            if hk.is_empty() {
                eprintln!("warning: no --host-key: this remote uses plaintext, unauthenticated TCP (only a host started with --insecure-open-access accepts that)");
            }
        }
        Some("remove") => {
            let Some(name) = name else {
                return Err(Error::Other("usage: rgit remote remove NAME".into()));
            };
            repo.set_config(&format!("remote.{name}.url"), "")?;
            repo.set_config(&format!("remote.{name}.project"), "")?;
            repo.set_config(&format!("remote.{name}.host_key"), "")?;
        }
        Some(a) => return Err(Error::Other(format!("unknown remote action '{a}'"))),
    }
    Ok(0)
}
