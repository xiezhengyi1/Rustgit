//! The `--json` contract used by the desktop apps (docs/gui-interface.md).

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn rgit(dir: &Path, args: &[&str]) -> (Value, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--json")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let last = stdout.lines().last().unwrap_or_else(|| panic!("no output for {args:?}"));
    (serde_json::from_str(last).unwrap_or_else(|e| panic!("bad json {last:?}: {e}")), out.status.code().unwrap())
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let (v, code) = rgit(dir, args);
    assert_eq!(v["ok"], true, "{args:?}: {v}");
    assert_eq!(code, 0, "{args:?}");
    v["data"].clone()
}

#[test]
fn version_reports_the_interface_number() {
    let d = tempfile::tempdir().unwrap();
    let v = ok(d.path(), &["version"]);
    assert_eq!(v["json_interface"], 1);
    assert_eq!(v["format"], 1);
}

#[test]
fn local_commands_have_stable_shapes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    ok(p, &["init"]);
    ok(p, &["config", "user.name", "Ann"]);
    std::fs::write(p.join("a.txt"), "one\n").unwrap();
    let st = ok(p, &["status"]);
    assert_eq!(st["branch"], "main");
    assert_eq!(st["head"], Value::Null);
    assert_eq!(st["changes"][0]["kind"], "added");
    assert_eq!(st["changes"][0]["path"], "a.txt");
    let c = ok(p, &["commit", "-m", "first"]);
    assert_eq!(c["id"].as_str().unwrap().len(), 64);
    let log = ok(p, &["log"]);
    assert_eq!(log["commits"][0]["message"], "first");
    assert_eq!(log["commits"][0]["branches"][0], "main");
    ok(p, &["branch", "feature"]);
    let b = ok(p, &["branch"]);
    assert_eq!(b["current"], "main");
    assert_eq!(b["branches"].as_array().unwrap().len(), 2);
    std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
    let diff = ok(p, &["diff"]);
    assert!(diff["diff"].as_str().unwrap().contains("+two"));
    assert_eq!(diff["changes"][0]["kind"], "modified");
    let stats = ok(p, &["stats"]);
    assert!(stats["objects"].as_u64().unwrap() > 0);
    let ver = ok(p, &["verify"]);
    assert_eq!(ver["problems"].as_array().unwrap().len(), 0);
}

#[test]
fn errors_are_json_with_codes_and_paths() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let (v, code) = rgit(p, &["status"]);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "not_a_repository");
    assert_eq!(code, 1);
    ok(p, &["init"]);
    std::fs::write(p.join("a.txt"), "x").unwrap();
    ok(p, &["commit", "-m", "c"]);
    ok(p, &["switch", "-c", "dev"]);
    std::fs::write(p.join("a.txt"), "dev").unwrap();
    ok(p, &["commit", "-m", "dev"]);
    ok(p, &["switch", "main"]);
    std::fs::write(p.join("a.txt"), "uncommitted").unwrap();
    let (v, _) = rgit(p, &["switch", "dev"]);
    assert_eq!(v["error"]["code"], "dirty_working_tree");
    assert_eq!(v["error"]["paths"][0], "a.txt");
    let (v, _) = rgit(p, &["commit", "-m", ""]);
    assert_eq!(v["error"]["code"], "other");
}

#[test]
fn merge_conflicts_are_reported_in_data_with_exit_code_1() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    ok(p, &["init"]);
    std::fs::write(p.join("f.txt"), "a\nb\nc\n").unwrap();
    ok(p, &["commit", "-m", "base"]);
    ok(p, &["switch", "-c", "x"]);
    std::fs::write(p.join("f.txt"), "a\nX\nc\n").unwrap();
    ok(p, &["commit", "-m", "x"]);
    ok(p, &["switch", "main"]);
    std::fs::write(p.join("f.txt"), "a\nY\nc\n").unwrap();
    ok(p, &["commit", "-m", "y"]);
    let (v, code) = rgit(p, &["merge", "x"]);
    assert_eq!(code, 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["conflicts"][0], "f.txt");
    let st = ok(p, &["status"]);
    assert_eq!(st["conflicts"][0]["resolved"], false);
    ok(p, &["resolve", "f.txt", "--theirs"]);
    ok(p, &["commit", "-m", "merged"]);
}

#[test]
fn push_streams_progress_lines_then_a_result() {
    let d = tempfile::tempdir().unwrap();
    let host = d.path().join("host");
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let port_file = d.path().join("port");
    let mut serve = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["serve", "--init"])
        .arg(&host)
        .args(["--insecure-open-access", "--listen", "127.0.0.1:0", "--port-file"])
        .arg(&port_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if port_file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let addr = std::fs::read_to_string(&port_file).unwrap();
    ok(&work, &["init"]);
    std::fs::write(work.join("a.txt"), "hello").unwrap();
    ok(&work, &["commit", "-m", "c"]);
    ok(&work, &["remote", "add", "origin", &format!("tcp://{addr}")]);
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["--json", "-C"])
        .arg(&work)
        .arg("push")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines.len() >= 2, "{text}");
    assert!(lines[..lines.len() - 1].iter().all(|l| l.get("event").is_some()));
    let last = lines.last().unwrap();
    assert_eq!(last["ok"], true);
    assert_eq!(last["data"]["updated"][0]["ref"], "origin/main");
    let _ = serve.kill();
}

#[test]
fn rejected_push_is_a_ref_conflict_error_with_exit_3() {
    let d = tempfile::tempdir().unwrap();
    let host = d.path().join("host");
    let port_file = d.path().join("port");
    let mut serve = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["serve", "--init"])
        .arg(&host)
        .args(["--insecure-open-access", "--listen", "127.0.0.1:0", "--port-file"])
        .arg(&port_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if port_file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let url = format!("tcp://{}", std::fs::read_to_string(&port_file).unwrap());
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    for w in [&a, &b] {
        std::fs::create_dir_all(w).unwrap();
        ok(w, &["init"]);
        ok(w, &["remote", "add", "origin", &url]);
    }
    std::fs::write(a.join("f"), "a").unwrap();
    ok(&a, &["commit", "-m", "a"]);
    ok(&a, &["push"]);
    std::fs::write(b.join("g"), "b").unwrap();
    ok(&b, &["commit", "-m", "b"]);
    let (v, code) = rgit(&b, &["push"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ref_conflict");
    // Local commits are untouched.
    assert_eq!(ok(&b, &["log"])["commits"][0]["message"], "b");
    let _ = serve.kill();
}

#[test]
fn commit_can_read_its_selection_from_a_file_so_large_selections_fit() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    ok(p, &["init"]);
    for n in ["a", "b", "c"] {
        std::fs::write(p.join(n), n).unwrap();
    }
    let list = d.path().join("..").join(format!("{}-except.txt", d.path().file_name().unwrap().to_string_lossy()));
    std::fs::write(&list, "b\n").unwrap();
    ok(p, &["commit", "-m", "except b", "--except-from", list.to_str().unwrap()]);
    let st = ok(p, &["status"]);
    assert_eq!(st["changes"].as_array().unwrap().len(), 1);
    assert_eq!(st["changes"][0]["path"], "b");
    std::fs::write(&list, "b\n").unwrap();
    ok(p, &["commit", "-m", "only b", "--paths-from", list.to_str().unwrap()]);
    assert!(ok(p, &["status"])["changes"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_file(list);
}
