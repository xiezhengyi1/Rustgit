//! The hosting process: `rgit --json serve`, kept as a child of the (tray-resident) app.
//! It is never installed as a service; it runs as the current user and stops with the app.

use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::rgit::{command, for_each_line, rgit_path};
use crate::state::{AppState, Run};

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HostingConfig {
    pub dir: String,
    pub init: bool,
    pub listen: String,
    pub insecure_listen_any: bool,
    pub relay: Option<String>,
}

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServeStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub dir: Option<String>,
    /// the address reported by the `listening` event
    pub addr: Option<String>,
    pub last_error: Option<String>,
    pub secure: bool,
    pub fingerprint: Option<String>,
    pub iroh_url: Option<String>,
    pub relay_online: Option<bool>,
}

#[derive(Default)]
pub struct ServeInner {
    pub status: ServeStatus,
    /// set by serve_stop so a requested stop is not reported as an error
    stopping: bool,
    /// bumped on every start; lets an old reader thread ignore a newer process
    generation: u64,
}

pub fn is_running(state: &AppState) -> bool {
    state.serve.lock().unwrap().status.running
}

pub fn status(state: &AppState) -> ServeStatus {
    state.serve.lock().unwrap().status.clone()
}

fn publish<R: Runtime>(app: &AppHandle<R>) {
    let st = status(&app.state::<AppState>());
    let _ = app.emit("serve-state", st);
    crate::tray::refresh(app);
}

pub fn start<R: Runtime>(app: &AppHandle<R>, cfg: HostingConfig) -> Result<ServeStatus, String> {
    let state = app.state::<AppState>();
    if cfg.dir.trim().is_empty() {
        return Err("choose a folder for the hosted repository".into());
    }
    if cfg.listen.trim().is_empty() {
        return Err("choose a listen address".into());
    }
    let generation;
    {
        let mut s = state.serve.lock().unwrap();
        if s.status.running {
            return Err("hosting is already running".into());
        }
        s.generation += 1;
        generation = s.generation;
    }

    let mut args: Vec<String> = vec!["serve".into(), cfg.dir.clone(), "--listen".into(), cfg.listen.clone()];
    if cfg.init {
        args.push("--init".into());
    }
    if cfg.insecure_listen_any {
        args.push("--advertise".into());
    }
    if let Some(relay) = &cfg.relay {
        if !relay.trim().is_empty() { args.extend(["--iroh".into(), "--iroh-relay".into(), relay.trim().into()]); }
    }
    let mut child = command(None, &args)?
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", rgit_path().display()))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();

    {
        let mut s = state.serve.lock().unwrap();
        s.stopping = false;
        s.status = ServeStatus {
            running: true,
            pid: Some(pid),
            dir: Some(cfg.dir.clone()),
            addr: None,
            last_error: None,
            ..Default::default()
        };
    }
    state
        .runs
        .lock()
        .unwrap()
        .insert(format!("serve-{generation}"), Run { child, _stdin: stdin });

    let err_thread = {
        let app = app.clone();
        thread::spawn(move || {
            if let Some(err) = stderr {
                for_each_line(err, |line| {
                    if line.is_empty() {
                        return;
                    }
                    {
                        let st = app.state::<AppState>();
                        let mut s = st.serve.lock().unwrap();
                        if s.generation == generation {
                            s.status.last_error = Some(line.clone());
                        }
                    }
                    let _ = app.emit("serve-line", json!({ "event": "stderr", "line": line }));
                });
            }
        })
    };
    {
        let app = app.clone();
        thread::spawn(move || {
            if let Some(out) = stdout {
                for_each_line(out, |line| {
                    if line.is_empty() {
                        return;
                    }
                    let value: Value = serde_json::from_str(&line).unwrap_or_else(|_| json!({ "event": "log", "line": line }));
                    let st = app.state::<AppState>();
                    let mut changed = false;
                    {
                        let mut s = st.serve.lock().unwrap();
                        if s.generation == generation {
                            if value["event"] == "listening" {
                                s.status.addr = value["addr"].as_str().map(str::to_string);
                                s.status.secure = value["secure"].as_bool().unwrap_or(false);
                                s.status.fingerprint = value["fingerprint"].as_str().map(str::to_string);
                                changed = true;
                            } else if value["event"] == "listening_iroh" {
                                s.status.iroh_url = value["url"].as_str().map(str::to_string);
                                s.status.relay_online = value["relay_online"].as_bool();
                                changed = true;
                            } else if value["ok"] == false {
                                s.status.last_error = value["error"]["message"].as_str().map(str::to_string);
                            }
                        }
                    }
                    let _ = app.emit("serve-line", value);
                    if changed {
                        publish(&app);
                    }
                });
            }
            let _ = err_thread.join();
            let st = app.state::<AppState>();
            let removed = st.runs.lock().unwrap().remove(&format!("serve-{generation}"));
            let code = removed.and_then(|mut r| r.child.wait().ok()).and_then(|s| s.code());
            {
                let mut s = st.serve.lock().unwrap();
                if s.generation == generation {
                    let requested = s.stopping;
                    s.status.running = false;
                    s.status.pid = None;
                    s.status.addr = None;
                    if requested {
                        s.status.last_error = None;
                    } else if s.status.last_error.is_none() {
                        s.status.last_error = Some(format!("the host process exited (code {})", code.map_or("none".into(), |c| c.to_string())));
                    }
                }
            }
            publish(&app);
        });
    }
    publish(app);
    Ok(status(&state))
}

pub fn kill(state: &AppState) {
    let mut s = state.serve.lock().unwrap();
    s.stopping = true;
    let generation = s.generation;
    drop(s);
    if let Some(r) = state.runs.lock().unwrap().get_mut(&format!("serve-{generation}")) {
        let _ = r.child.kill();
    }
}

pub fn stop<R: Runtime>(app: &AppHandle<R>) -> ServeStatus {
    let state = app.state::<AppState>();
    kill(&state);
    // Let the reader thread observe the exit so the returned state is final.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && is_running(&state) {
        thread::sleep(Duration::from_millis(20));
    }
    status(&state)
}

#[tauri::command]
pub async fn serve_start(app: AppHandle, config: HostingConfig) -> Result<ServeStatus, String> {
    start(&app, config)
}

#[tauri::command]
pub async fn serve_stop(app: AppHandle) -> ServeStatus {
    stop(&app)
}

#[tauri::command]
pub async fn serve_status(app: AppHandle) -> ServeStatus {
    status(&app.state::<AppState>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    fn real_rgit() -> Option<std::path::PathBuf> {
        let p = std::env::var_os("RGIT_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/release/rgit"));
        let out = std::process::Command::new(&p).args(["--json", "version"]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).contains("json_interface").then_some(p)
    }

    /// Starts and stops a real `rgit serve` through the same code the app uses.
    #[test]
    fn host_lifecycle_start_report_address_stop() {
        let Some(bin) = real_rgit() else {
            eprintln!("skipped: no rgit with --json at target/release (build it or set RGIT_BIN)");
            return;
        };
        std::env::set_var("RGIT_BIN", &bin);
        let app = mock_builder().manage(AppState::default()).build(mock_context(noop_assets())).unwrap();
        let handle = app.handle().clone();
        let dir = tempfile::tempdir().unwrap();
        let cfg = HostingConfig {
            dir: dir.path().join("hosted.rgit").to_string_lossy().into(),
            init: true,
            listen: "127.0.0.1:0".into(),
            insecure_listen_any: false,
            relay: None,
        };
        let started = start(&handle, cfg.clone()).unwrap();
        assert!(started.running);
        assert!(start(&handle, cfg.clone()).is_err(), "a second host must be refused");

        let deadline = Instant::now() + Duration::from_secs(10);
        let st = loop {
            let st = status(&handle.state::<AppState>());
            if st.addr.is_some() || Instant::now() > deadline {
                break st;
            }
            thread::sleep(Duration::from_millis(50));
        };
        assert!(st.addr.as_deref().unwrap_or("").starts_with("127.0.0.1:"), "{st:?}");

        let stopped = stop(&handle);
        assert!(!stopped.running, "{stopped:?}");
        assert!(stopped.last_error.is_none(), "a requested stop is not an error: {stopped:?}");
        assert!(handle.state::<AppState>().runs.lock().unwrap().is_empty());

        // starting on a folder that is not a repository (no --init) must fail visibly, not hang
        let bad = HostingConfig { dir: dir.path().join("missing").to_string_lossy().into(), init: false, ..cfg };
        start(&handle, bad).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while is_running(&handle.state::<AppState>()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let failed = status(&handle.state::<AppState>());
        assert!(!failed.running);
        assert!(failed.last_error.is_some(), "the reason must be reported: {failed:?}");
    }

    /// "Allow other devices": listen on every interface, then actually clone through this
    /// computer's LAN address with the real `rgit`, the way a second device would.
    #[test]
    fn lan_hosting_is_reachable_through_a_lan_address() {
        let Some(bin) = real_rgit() else {
            eprintln!("skipped: no rgit with --json");
            return;
        };
        std::env::set_var("RGIT_BIN", &bin);
        let lan = crate::net::lan_addresses();
        let Some(addr) = lan.first() else {
            eprintln!("skipped: no LAN address on this machine");
            return;
        };
        let app = mock_builder().manage(AppState::default()).build(mock_context(noop_assets())).unwrap();
        let handle = app.handle().clone();
        let dir = tempfile::tempdir().unwrap();
        let cfg = HostingConfig {
            dir: dir.path().join("hosted.rgit").to_string_lossy().into(),
            init: true,
            listen: "0.0.0.0:0".into(),
            insecure_listen_any: true,
            relay: None,
        };
        let hosted_dir = std::path::PathBuf::from(&cfg.dir);
        start(&handle, cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let st = loop {
            let st = status(&handle.state::<AppState>());
            if st.addr.is_some() || Instant::now() > deadline {
                break st;
            }
            thread::sleep(Duration::from_millis(50));
        };
        let reported = st.addr.clone().unwrap_or_default();
        assert!(reported.starts_with("0.0.0.0:"), "host should report the wildcard address: {st:?}");
        let port = reported.rsplit(':').next().unwrap();
        let url = format!("tcp://{}:{}", addr.ip, port);

        // Push a real commit to the host through the LAN address, then clone it back.
        let work = tempfile::tempdir().unwrap();
        let p = work.path().join("proj");
        std::fs::create_dir(&p).unwrap();
        let rgit = |args: &[&str], dir: &std::path::Path| {
            let out = std::process::Command::new(&bin).arg("--json").arg("-C").arg(dir).args(args).output().unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        rgit(&["init"], &p);
        rgit(&["config", "user.name", "T"], &p);
        rgit(&["config", "user.email", "t@example.com"], &p);
        std::fs::write(p.join("a.txt"), "hello\n").unwrap();
        assert!(rgit(&["commit", "-m", "one"], &p).contains("\"ok\":true"));
        assert!(st.secure && st.fingerprint.is_some(), "the host must authenticate by default");
        let invite_output = rgit(&["invite", "create", "--role", "write", "--hint", &url], &hosted_dir);
        let invitation: Value = serde_json::from_str(invite_output.trim()).unwrap();
        let uri = invitation["data"]["uri"].as_str().unwrap();
        let mut child = std::process::Command::new(&bin).args(["--json", "-C"]).arg(&p)
            .args(["join", "-", "--add-remote"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        { use std::io::Write; child.stdin.take().unwrap().write_all(uri.as_bytes()).unwrap(); }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        let pushed = rgit(&["push"], &p);
        assert!(pushed.contains("\"ok\":true"), "push through {url} failed: {pushed}");
        let dest = work.path().join("clone");
        let out = std::process::Command::new(&bin).args(["--json", "clone", &url, "--host-key", st.fingerprint.as_deref().unwrap()]).arg(&dest).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("\"ok\":true"), "clone through {url} failed: {text}");
        assert!(dest.join("a.txt").exists());
        stop(&handle);
    }
}
