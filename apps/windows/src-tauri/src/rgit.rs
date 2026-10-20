//! Starting the bundled `rgit` and relaying its output. This file contains no repository
//! logic: it spawns the process, forwards stdout/stderr lines and the exit code to the UI as
//! events, and can terminate the process. The UI parses the NDJSON (see src/rgit/client.ts).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::{AppState, Run};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LinePayload {
    run_id: String,
    line: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ExitPayload {
    run_id: String,
    code: Option<i32>,
}

/// `rgit` next to the app executable (where the bundler puts the sidecar), or `$RGIT_BIN`.
pub fn rgit_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RGIT_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(if cfg!(windows) { "rgit.exe" } else { "rgit" })
}

/// `rgit --json [-C project] args...` with piped stdio and no console window.
pub fn command(project: Option<&str>, args: &[String]) -> Result<Command, String> {
    let mut c = Command::new(rgit_path());
    c.arg("--json");
    if let Some(p) = project {
        if !Path::new(p).is_dir() {
            return Err(format!("project folder not found: {p}"));
        }
        c.arg("-C").arg(p);
    }
    c.args(args);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(c)
}

/// Read a stream line by line, tolerating invalid UTF-8 (paths in messages may be odd).
pub fn for_each_line<R: std::io::Read>(r: R, mut f: impl FnMut(String)) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let s = String::from_utf8_lossy(&buf);
                f(s.trim_end_matches(['\n', '\r']).to_string());
            }
        }
    }
}

/// Start `rgit --json` and relay its output as events. `input`, if given, is written to stdin
/// and stdin is then closed.
pub fn start_run<R: Runtime>(
    app: &AppHandle<R>,
    run_id: String,
    project: Option<String>,
    args: Vec<String>,
    input: Option<String>,
) -> Result<(), String> {
    // Hosting has its own lifecycle (tray, close-to-tray, exit cleanup).
    if args.first().map(String::as_str) == Some("serve") {
        return Err("use serve_start to host".into());
    }
    if args.iter().any(|a| a == "--json") {
        return Err("--json is added by the app".into());
    }
    let mut child = command(project.as_deref(), &args)?
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", rgit_path().display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    // A payload (`--paths-from -`, tens of thousands of paths) is written on its own thread, so a
    // full pipe cannot block this command, and stdin is closed once it has been sent.
    // Without a payload stdin stays open for the child's lifetime (closing it asks the engine to stop).
    let stdin = match (input, stdin) {
        (Some(text), Some(mut pipe)) => {
            thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
            None
        }
        (_, kept) => kept,
    };
    app.state::<AppState>()
        .runs
        .lock()
        .unwrap()
        .insert(run_id.clone(), Run { child, _stdin: stdin });

    let err_thread = {
        let app = app.clone();
        let run_id = run_id.clone();
        thread::spawn(move || {
            if let Some(err) = stderr {
                for_each_line(err, |line| {
                    let _ = app.emit("rgit-stderr", LinePayload { run_id: run_id.clone(), line });
                });
            }
        })
    };
    let app = app.clone();
    thread::spawn(move || {
        if let Some(out) = stdout {
            for_each_line(out, |line| {
                let _ = app.emit("rgit-line", LinePayload { run_id: run_id.clone(), line });
            });
        }
        let _ = err_thread.join();
        let removed = app.state::<AppState>().runs.lock().unwrap().remove(&run_id);
        let code = removed.and_then(|mut r| r.child.wait().ok()).and_then(|s| s.code());
        let _ = app.emit("rgit-exit", ExitPayload { run_id, code });
    });
    Ok(())
}

// Commands are `async` so Tauri runs them on its thread pool instead of the main (UI) thread.
#[tauri::command]
pub async fn rgit_spawn(
    app: AppHandle,
    run_id: String,
    project: Option<String>,
    args: Vec<String>,
    input: Option<String>,
) -> Result<(), String> {
    start_run(&app, run_id, project, args, input)
}

/// Terminate a run. The engine resumes interrupted transfers on the next invocation.
#[tauri::command]
pub async fn rgit_cancel(app: AppHandle, run_id: String) {
    if let Some(r) = app.state::<AppState>().runs.lock().unwrap().get_mut(&run_id) {
        let _ = r.child.kill();
    }
}

pub fn kill_all(state: &AppState) {
    for r in state.runs.lock().unwrap().values_mut() {
        let _ = r.child.kill();
    }
}

/// Show a file or folder in the system file manager.
#[tauri::command]
pub async fn reveal_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("not found: {path}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer wants backslashes; `raw_arg` avoids Rust quoting breaking the /select, form.
        let native = path.replace('/', "\\");
        Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{native}\""))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(&p).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let dir = if p.is_dir() { p.clone() } else { p.parent().map(Path::to_path_buf).unwrap_or(p.clone()) };
        Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_survive_crlf_invalid_utf8_and_missing_final_newline() {
        let data: &[u8] = b"{\"a\":1}\r\n\xff\xfebad\n{\"ok\":true}";
        let mut got = Vec::new();
        for_each_line(data, |l| got.push(l));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "{\"a\":1}");
        assert!(got[1].contains('\u{fffd}'));
        assert_eq!(got[2], "{\"ok\":true}");
    }

    #[test]
    fn command_is_json_first_and_validates_the_project_folder() {
        let dir = std::env::temp_dir();
        let c = command(Some(dir.to_str().unwrap()), &["status".to_string()]).unwrap();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args[0], "--json");
        assert_eq!(args[1], "-C");
        assert_eq!(args.last().unwrap(), "status");
        assert!(command(Some("/definitely/not/a/folder"), &[]).is_err());
        let no_project = command(None, &["clone".to_string()]).unwrap();
        assert!(!no_project.get_args().any(|a| a == "-C"));
    }

    #[test]
    fn a_stdin_payload_reaches_the_child_and_is_closed() {
        use tauri::test::{mock_builder, mock_context, noop_assets};
        let Some(bin) = std::env::var_os("RGIT_BIN")
            .map(PathBuf::from)
            .or_else(|| Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/release/rgit")))
            .filter(|p| {
                Command::new(p)
                    .args(["commit", "--help"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).contains("--except-from"))
                    .unwrap_or(false)
            })
        else {
            eprintln!("skipped: rgit with --except-from not found");
            return;
        };
        std::env::set_var("RGIT_BIN", &bin);
        let app = mock_builder().manage(AppState::default()).build(mock_context(noop_assets())).unwrap();
        let handle = app.handle().clone();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_string_lossy().to_string();
        let run = |args: &[&str]| {
            let out = Command::new(&bin).arg("--json").arg("-C").arg(&p).args(args).output().unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        run(&["init"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@example.com"]);
        for i in 0..2000 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        }
        // commit everything except keep.txt, which arrives on stdin
        std::fs::write(dir.path().join("keep.txt"), "k\n").unwrap();
        start_run(&handle, "t1".into(), Some(p.clone()), vec!["commit".into(), "-m".into(), "bulk".into(), "--except-from".into(), "-".into()], Some("keep.txt\n".into())).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !handle.state::<AppState>().runs.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = run(&["status"]);
        assert!(status.contains("keep.txt"), "{status}");
        assert!(!status.contains("f1999.txt"), "2000 files should be committed: {}", &status[..status.len().min(300)]);
    }
}
