use std::fs;
use std::path::Path;

use rustgit_core::diff::ChangeKind;
use rustgit_core::ops::ResolveWith;
use rustgit_core::{Error, Repo};
use tempfile::{tempdir, TempDir};

fn pseudo(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn setup() -> (TempDir, Repo) {
    let d = tempdir().unwrap();
    let repo = Repo::init(d.path(), "main").unwrap();
    repo.set_config("user.name", "Tester").unwrap();
    repo.set_config("user.email", "t@example.com").unwrap();
    (d, repo)
}

fn write(root: &Path, rel: &str, data: impl AsRef<[u8]>) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn read(root: &Path, rel: &str) -> Vec<u8> {
    fs::read(root.join(rel)).unwrap()
}

#[test]
fn commit_status_log_and_exact_restore_through_adds_deletes_modifies() {
    let (d, mut r) = setup();
    let root = d.path().to_path_buf();
    assert!(r.status().unwrap().changes.is_empty());
    write(&root, "a.txt", "one\n");
    write(&root, "dir/b.txt", "bee\n");
    write(&root, "empty.bin", "");
    let st = r.status().unwrap();
    assert_eq!(st.changes.len(), 3);
    let c1 = r.commit("first", &[]).unwrap();
    assert!(r.status().unwrap().changes.is_empty());
    assert!(matches!(
        r.commit("again", &[]),
        Err(Error::NothingToCommit)
    ));

    write(&root, "a.txt", "one\ntwo\n");
    fs::remove_file(root.join("dir/b.txt")).unwrap();
    write(&root, "c.txt", "see\n");
    let c2 = r.commit("second", &[]).unwrap();
    fs::rename(root.join("c.txt"), root.join("renamed.txt")).unwrap();
    let st = r.status().unwrap();
    assert!(st
        .changes
        .iter()
        .any(|c| matches!(&c.kind, ChangeKind::Renamed(f) if f == "c.txt")));
    let c3 = r.commit("third", &[]).unwrap();

    assert_eq!(r.file_at(&c1.to_hex(), "a.txt").unwrap(), b"one\n");
    assert_eq!(r.file_at(&c1.to_hex(), "dir/b.txt").unwrap(), b"bee\n");
    assert_eq!(r.file_at(&c1.to_hex(), "empty.bin").unwrap(), b"");
    assert_eq!(r.file_at(&c2.to_hex(), "a.txt").unwrap(), b"one\ntwo\n");
    assert!(r.file_at(&c2.to_hex(), "dir/b.txt").is_err());
    assert_eq!(r.file_at(&c3.to_hex(), "renamed.txt").unwrap(), b"see\n");
    assert_eq!(r.file_at("HEAD~2", "a.txt").unwrap(), b"one\n");
    let log = r.history(&c3, 10).unwrap();
    assert_eq!(
        log.iter()
            .map(|(_, c)| c.message.as_str())
            .collect::<Vec<_>>(),
        ["third", "second", "first"]
    );
    assert!(r.verify().unwrap().problems.is_empty());
}

#[test]
fn selective_commit_only_takes_chosen_paths() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "keep.txt", "k");
    write(root, "src/x.rs", "x");
    r.commit("base", &[]).unwrap();
    write(root, "keep.txt", "k2");
    write(root, "src/x.rs", "x2");
    write(root, "src/y.rs", "y");
    r.commit("only src", &["src".to_string()]).unwrap();
    let st = r.status().unwrap();
    assert_eq!(st.changes.len(), 1);
    assert_eq!(st.changes[0].path, "keep.txt");
    assert_eq!(r.file_at("HEAD", "src/y.rs").unwrap(), b"y");
    assert_eq!(r.file_at("HEAD", "keep.txt").unwrap(), b"k");
}

#[test]
fn ignore_file_is_honoured_and_dot_rgit_never_tracked() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, ".rgitignore", "*.log\ntarget/\n");
    write(root, "a.log", "x");
    write(root, "target/out", "x");
    write(root, "src/a.rs", "x");
    r.commit("init", &[]).unwrap();
    let flat = r.head_flat().unwrap();
    assert!(flat.contains_key("src/a.rs") && flat.contains_key(".rgitignore"));
    assert!(!flat.contains_key("a.log") && !flat.contains_key("target/out"));
    assert!(flat.keys().all(|k| !k.starts_with(".rgit/")));
}

#[test]
fn branches_share_content_and_cost_only_references() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "big.bin", pseudo(1, 3_000_000));
    write(root, "a.txt", "hello");
    r.commit("base", &[]).unwrap();
    let before = r.store.stats().unwrap();
    for i in 0..100 {
        r.branch_create(&format!("feature/b{i}"), None).unwrap();
    }
    let after = r.store.stats().unwrap();
    assert_eq!(before.objects, after.objects);
    assert_eq!(before.stored_bytes, after.stored_bytes);
    assert_eq!(r.branches().unwrap().len(), 101);
    assert!(matches!(
        r.branch_create("feature/b0", None),
        Err(Error::BranchExists(_))
    ));
    assert!(matches!(
        r.branch_create("bad name", None),
        Err(Error::InvalidName(_))
    ));
}

#[test]
fn switch_changes_files_and_protects_uncommitted_work() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "main version");
    r.commit("base", &[]).unwrap();
    r.switch("dev", true).unwrap();
    write(root, "a.txt", "dev version");
    write(root, "dev-only.txt", "d");
    r.commit("dev work", &[]).unwrap();
    r.switch("main", false).unwrap();
    assert_eq!(read(root, "a.txt"), b"main version");
    assert!(!root.join("dev-only.txt").exists());
    // Uncommitted edit blocks the switch and is left untouched.
    write(root, "a.txt", "precious edit");
    match r.switch("dev", false) {
        Err(Error::DirtyWorkingTree(p)) => assert_eq!(p, vec!["a.txt"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(root, "a.txt"), b"precious edit");
    assert_eq!(r.current_branch().unwrap(), "main");
    // An untracked file in the way is also protected.
    r.restore(None, &["a.txt".into()], true).unwrap();
    write(root, "dev-only.txt", "my own file");
    assert!(matches!(
        r.switch("dev", false),
        Err(Error::DirtyWorkingTree(_))
    ));
    assert_eq!(read(root, "dev-only.txt"), b"my own file");
}

#[test]
fn restore_force_keeps_recovery_snapshot() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "v1");
    r.commit("c1", &[]).unwrap();
    write(root, "a.txt", "unsaved work");
    assert!(matches!(
        r.restore(None, &["a.txt".into()], false),
        Err(Error::DirtyWorkingTree(_))
    ));
    r.restore(None, &["a.txt".into()], true).unwrap();
    assert_eq!(read(root, "a.txt"), b"v1");
    let snaps = r.list_refs("recovery/").unwrap();
    assert_eq!(snaps.len(), 1);
    let flat = r.commit_flat(&snaps[0].1).unwrap();
    assert_eq!(r.read_file_bytes(&flat["a.txt"]).unwrap(), b"unsaved work");
}

#[test]
fn restore_old_version_of_file_and_deleted_file() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "v1");
    write(root, "gone.txt", "will be deleted");
    r.commit("c1", &[]).unwrap();
    write(root, "a.txt", "v2");
    fs::remove_file(root.join("gone.txt")).unwrap();
    r.commit("c2", &[]).unwrap();
    r.restore(Some("HEAD~1"), &["a.txt".into(), "gone.txt".into()], false)
        .unwrap();
    assert_eq!(read(root, "a.txt"), b"v1");
    assert_eq!(read(root, "gone.txt"), b"will be deleted");
    let c3 = r.commit("restore old", &[]).unwrap();
    assert_eq!(
        r.file_at(&c3.to_hex(), "gone.txt").unwrap(),
        b"will be deleted"
    );
}

#[test]
fn shelve_and_unshelve_round_trip() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "line1\nline2\nline3\n");
    r.commit("base", &[]).unwrap();
    write(root, "a.txt", "line1\nline2 edited\nline3\n");
    write(root, "new.txt", "brand new");
    let name = r.shelve("wip").unwrap();
    assert_eq!(name, "shelf/1");
    assert!(r.status().unwrap().changes.is_empty());
    assert_eq!(read(root, "a.txt"), b"line1\nline2\nline3\n");
    assert!(!root.join("new.txt").exists());
    let res = r.unshelve("1").unwrap();
    assert!(res.conflicts.is_empty());
    assert_eq!(read(root, "a.txt"), b"line1\nline2 edited\nline3\n");
    assert_eq!(read(root, "new.txt"), b"brand new");
}

#[test]
fn fast_forward_and_clean_text_merge() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "f.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n");
    r.commit("base", &[]).unwrap();
    r.switch("feature", true).unwrap();
    write(root, "f.txt", "1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    write(root, "feature.txt", "f");
    let feat = r.commit("feature change", &[]).unwrap();
    r.switch("main", false).unwrap();
    let res = r.merge("feature").unwrap();
    assert!(res.fast_forward);
    assert_eq!(r.head_commit().unwrap(), Some(feat));

    r.branch_create("other", Some("HEAD~1")).unwrap();
    r.switch("other", false).unwrap();
    write(root, "f.txt", "one\n2\n3\n4\n5\n6\n7\n8\n9\n");
    r.commit("other change", &[]).unwrap();
    let res = r.merge("main").unwrap();
    let merged = res.commit.unwrap();
    assert!(res.conflicts.is_empty() && !res.fast_forward);
    assert_eq!(read(root, "f.txt"), b"one\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    assert_eq!(read(root, "feature.txt"), b"f");
    assert_eq!(r.read_commit(&merged).unwrap().parents.len(), 2);
    assert!(r.merge("main").unwrap().already_up_to_date);
    assert!(r.verify().unwrap().problems.is_empty());
}

#[test]
fn text_conflict_requires_explicit_resolution() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "f.txt", "a\nb\nc\n");
    r.commit("base", &[]).unwrap();
    r.switch("x", true).unwrap();
    write(root, "f.txt", "a\nX\nc\n");
    r.commit("x", &[]).unwrap();
    r.switch("main", false).unwrap();
    write(root, "f.txt", "a\nY\nc\n");
    let main_tip = r.commit("y", &[]).unwrap();
    let res = r.merge("x").unwrap();
    assert_eq!(res.conflicts, vec!["f.txt"]);
    let text = String::from_utf8(read(root, "f.txt")).unwrap();
    assert!(
        text.contains("<<<<<<<") && text.contains("X") && text.contains("Y"),
        "{text}"
    );
    assert!(matches!(
        r.commit("nope", &[]),
        Err(Error::UnresolvedConflicts(1))
    ));
    assert!(matches!(r.switch("x", false), Err(Error::MergeInProgress)));
    // Cannot mark resolved with markers present.
    assert!(r.resolve("f.txt", ResolveWith::Manual).is_err());
    write(root, "f.txt", "a\nX and Y\nc\n");
    r.resolve("f.txt", ResolveWith::Manual).unwrap();
    let id = r.commit("", &[]).unwrap();
    let c = r.read_commit(&id).unwrap();
    assert_eq!(c.parents.len(), 2);
    assert_eq!(c.parents[0], main_tip);
    assert_eq!(r.file_at("HEAD", "f.txt").unwrap(), b"a\nX and Y\nc\n");
    assert!(r.merge_state().unwrap().is_none());
}

#[test]
fn binary_conflict_keeps_both_versions_until_user_chooses() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "img.bin", pseudo(1, 2000));
    r.commit("base", &[]).unwrap();
    r.switch("x", true).unwrap();
    let theirs = pseudo(2, 2000);
    write(root, "img.bin", &theirs);
    r.commit("x", &[]).unwrap();
    r.switch("main", false).unwrap();
    let ours = pseudo(3, 2000);
    write(root, "img.bin", &ours);
    r.commit("main", &[]).unwrap();
    let res = r.merge("x").unwrap();
    assert_eq!(res.conflicts, vec!["img.bin"]);
    assert_eq!(read(root, "img.bin"), ours);
    assert_eq!(read(root, "img.bin.rgit-theirs"), theirs);
    assert!(matches!(
        r.commit("m", &[]),
        Err(Error::UnresolvedConflicts(1))
    ));
    r.resolve("img.bin", ResolveWith::Theirs).unwrap();
    assert_eq!(read(root, "img.bin"), theirs);
    assert!(!root.join("img.bin.rgit-theirs").exists());
    r.commit("", &[]).unwrap();
    assert_eq!(r.file_at("HEAD", "img.bin").unwrap(), theirs);
}

#[test]
fn delete_modify_conflict_and_abort() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "f.txt", "a\n");
    write(root, "g.txt", "g\n");
    r.commit("base", &[]).unwrap();
    r.switch("x", true).unwrap();
    fs::remove_file(root.join("f.txt")).unwrap();
    r.commit("delete f", &[]).unwrap();
    r.switch("main", false).unwrap();
    write(root, "f.txt", "a modified\n");
    r.commit("modify f", &[]).unwrap();
    let res = r.merge("x").unwrap();
    assert_eq!(res.conflicts, vec!["f.txt"]);
    r.merge_abort().unwrap();
    assert_eq!(read(root, "f.txt"), b"a modified\n");
    assert!(!root.join("f.txt.rgit-theirs").exists());
    assert!(r.merge_state().unwrap().is_none());
    assert!(r.status().unwrap().changes.is_empty());
}

#[test]
fn revert_creates_inverse_commit() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "1\n2\n3\n");
    r.commit("base", &[]).unwrap();
    write(root, "a.txt", "1\n2\n3\n4\n");
    write(root, "b.txt", "new");
    let bad = r.commit("oops", &[]).unwrap();
    write(root, "c.txt", "later");
    r.commit("later", &[]).unwrap();
    let res = r.revert(&bad.to_hex()).unwrap();
    assert!(res.commit.is_some() && res.conflicts.is_empty());
    assert_eq!(read(root, "a.txt"), b"1\n2\n3\n");
    assert!(!root.join("b.txt").exists());
    assert_eq!(read(root, "c.txt"), b"later");
    // Both the original and the reverted-to history stay recoverable.
    assert_eq!(r.file_at(&bad.to_hex(), "b.txt").unwrap(), b"new");
}

#[test]
fn large_file_local_edit_reuses_chunks_and_shrinks_with_deltas() {
    let (d, mut r) = setup();
    let root = d.path();
    let mut data = pseudo(5, 4_000_000);
    write(root, "big.bin", &data);
    r.commit("v0", &[]).unwrap();
    let base = r.store.stats().unwrap();
    for i in 0..5 {
        data[2_000_000 + i * 10] ^= 0xff;
        write(root, "big.bin", &data);
        r.commit(&format!("v{}", i + 1), &[]).unwrap();
    }
    let after = r.store.stats().unwrap();
    // Each edit rewrites one ~256 KiB chunk, stored as a tiny delta.
    let growth = after.stored_bytes - base.stored_bytes;
    assert!(
        growth < 400_000,
        "growth {growth} for 5 localized edits of a 4 MB file"
    );
    assert!(after.delta_objects >= 1);
    for i in 0..=5usize {
        let rev = format!("HEAD~{}", 5 - i);
        let got = r.file_at(&rev, "big.bin").unwrap();
        let mut want = pseudo(5, 4_000_000);
        for k in 0..i {
            want[2_000_000 + k * 10] ^= 0xff;
        }
        assert_eq!(got, want, "version {i}");
    }
    assert!(r.verify().unwrap().problems.is_empty());
}

#[test]
fn case_collisions_and_windows_names_are_rejected_before_commit() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "Readme.md", "x");
    write(root, "src/lib.rs", "x");
    r.commit("ok", &[]).unwrap();
    write(root, "README.md", "y");
    if fs::read(root.join("Readme.md")).unwrap() == b"x" {
        // case-sensitive filesystem: both files exist
        match r.commit("collide", &[]) {
            Err(Error::Unportable(p)) => assert!(p[0].contains("differ only by case"), "{p:?}"),
            other => panic!("{other:?}"),
        }
        fs::remove_file(root.join("README.md")).unwrap();
    }
    write(root, "docs/con.txt", "x");
    match r.commit("reserved", &[]) {
        Err(Error::Unportable(p)) => assert!(p[0].contains("reserved"), "{p:?}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unportable_tree_from_a_peer_is_refused_without_touching_the_working_dir() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "ok.txt", "mine");
    r.commit("base", &[]).unwrap();
    // Build a commit directly (as a peer might have) containing a Windows-reserved name.
    let mut flat = r.head_flat().unwrap();
    let e = flat["ok.txt"].clone();
    flat.insert("aux.txt".into(), e);
    flat.insert("added-by-peer.txt".into(), flat["ok.txt"].clone());
    let tree = r.write_tree(&flat, None).unwrap();
    let c = rustgit_core::objects::Commit {
        parents: vec![r.head_commit().unwrap().unwrap()],
        tree,
        author_name: "p".into(),
        author_email: "p@x".into(),
        timestamp: 1,
        tz_offset_min: 0,
        message: "peer".into(),
    };
    let id = r
        .store
        .put(rustgit_core::ObjectType::Commit, &c.encode(), None)
        .unwrap();
    r.flush().unwrap();
    r.cas_ref("heads/peer", None, Some(id)).unwrap();
    match r.switch("peer", false) {
        Err(Error::Unportable(p)) => assert!(p.iter().any(|x| x.contains("aux")), "{p:?}"),
        other => panic!("{other:?}"),
    }
    assert!(!root.join("added-by-peer.txt").exists());
    assert_eq!(r.current_branch().unwrap(), "main");
}

#[cfg(unix)]
#[test]
fn symlinks_are_stored_and_unsafe_targets_refused() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "target.txt", "t");
    std::os::unix::fs::symlink("target.txt", root.join("link")).unwrap();
    r.commit("with link", &[]).unwrap();
    fs::remove_file(root.join("link")).unwrap();
    r.restore(None, &["link".into()], false).unwrap();
    assert_eq!(
        fs::read_link(root.join("link")).unwrap().to_str().unwrap(),
        "target.txt"
    );
    // A malicious tree: symlink escaping the project.
    let evil = r
        .store
        .put(rustgit_core::ObjectType::Chunk, b"../../etc/passwd", None)
        .unwrap();
    let mut flat = r.head_flat().unwrap();
    flat.insert(
        "evil".into(),
        rustgit_core::FlatEntry {
            kind: rustgit_core::objects::EntryKind::Symlink,
            exec: false,
            id: evil,
        },
    );
    let cur = r.head_flat().unwrap();
    match r.checkout_flat(&cur, &flat, false) {
        Err(Error::Unportable(p)) => assert!(p[0].contains("leaves the project"), "{p:?}"),
        other => panic!("{other:?}"),
    }
    assert!(!root.join("evil").exists());
}

#[cfg(unix)]
#[test]
fn executable_bit_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "run.sh", "#!/bin/sh\n");
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    r.commit("exec", &[]).unwrap();
    fs::remove_file(root.join("run.sh")).unwrap();
    r.restore(None, &["run.sh".into()], false).unwrap();
    assert_eq!(
        fs::metadata(root.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
}

#[test]
fn ref_updates_use_compare_and_swap() {
    let (d, mut r) = setup();
    write(d.path(), "a", "a");
    let c1 = r.commit("c1", &[]).unwrap();
    write(d.path(), "a", "b");
    let c2 = r.commit("c2", &[]).unwrap();
    let before = r.get_ref("heads/main").unwrap().unwrap();
    // A stale writer that still believes the tip is c1 must lose.
    assert!(matches!(
        r.cas_ref("heads/main", Some(c1), Some(c1)),
        Err(Error::RefConflict { .. })
    ));
    let after = r.get_ref("heads/main").unwrap().unwrap();
    assert_eq!(before, after);
    assert_eq!(after.commit, Some(c2));
    assert_eq!(after.generation, 2);
}

#[test]
fn gc_drops_unreachable_history_but_keeps_everything_reachable() {
    let (d, mut r) = setup();
    let root = d.path();
    let mut data = pseudo(8, 1_500_000);
    write(root, "big.bin", &data);
    r.commit("base", &[]).unwrap();
    r.switch("doomed", true).unwrap();
    write(root, "secret.bin", pseudo(9, 700_000));
    r.commit("only on doomed", &[]).unwrap();
    r.switch("main", false).unwrap();
    for i in 0..3 {
        data[100_000 + i] ^= 1;
        write(root, "big.bin", &data);
        r.commit(&format!("edit {i}"), &[]).unwrap();
    }
    let before = r.store.stats().unwrap();
    r.branch_delete("doomed", true).unwrap();
    // Recent unreachable data is protected by the grace period.
    let g = r.gc(3600, 0, false).unwrap();
    assert_eq!(g.repack.dropped, 0);
    let g = r.gc(0, 0, false).unwrap();
    assert!(g.repack.dropped >= 2, "{g:?}");
    let after = r.store.stats().unwrap();
    assert!(after.stored_bytes < before.stored_bytes);
    let rep = r.verify().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    for i in 0..=3usize {
        let got = r.file_at(&format!("HEAD~{}", 3 - i), "big.bin").unwrap();
        let mut want = pseudo(8, 1_500_000);
        for k in 0..i {
            want[100_000 + k] ^= 1;
        }
        assert_eq!(got, want);
    }
    // Survives reopen.
    let r2 = Repo::open(root).unwrap();
    assert!(r2.verify().unwrap().problems.is_empty());
}

#[test]
fn damaged_index_is_rebuilt_without_losing_history() {
    let (d, mut r) = setup();
    let root = d.path().to_path_buf();
    write(&root, "a.txt", "alpha");
    r.commit("c1", &[]).unwrap();
    write(&root, "b.bin", pseudo(4, 900_000));
    r.commit("c2", &[]).unwrap();
    drop(r);
    fs::remove_file(root.join(".rgit/index.sqlite")).unwrap();
    let r = Repo::open(&root).unwrap();
    assert!(r.verify().unwrap().problems.is_empty());
    assert_eq!(r.file_at("HEAD", "a.txt").unwrap(), b"alpha");
    assert!(r.store.warnings().iter().any(|w| w.contains("rebuilt")));
}

#[test]
fn corrupted_pack_is_reported_by_verify() {
    let (d, mut r) = setup();
    let root = d.path().to_path_buf();
    write(&root, "b.bin", pseudo(4, 50_000));
    r.commit("c1", &[]).unwrap();
    drop(r);
    let pack = fs::read_dir(root.join(".rgit/objects"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().map(|e| e == "rgpk").unwrap_or(false))
        .unwrap();
    let mut b = fs::read(&pack).unwrap();
    b[100] ^= 0xff;
    fs::write(&pack, b).unwrap();
    let r = Repo::open(&root).unwrap();
    assert!(!r.verify().unwrap().problems.is_empty());
}

#[test]
fn commits_survive_reopen_and_unflushed_ref_updates_never_dangle() {
    let (d, mut r) = setup();
    let root = d.path().to_path_buf();
    write(&root, "a.txt", "x");
    let c = r.commit("c", &[]).unwrap();
    drop(r);
    let r = Repo::open(&root).unwrap();
    assert_eq!(r.head_commit().unwrap(), Some(c));
    assert!(r.store.has(&c).unwrap());
}

#[test]
fn commit_except_takes_everything_but_the_listed_paths() {
    let (d, mut r) = setup();
    let root = d.path();
    write(root, "a.txt", "a");
    write(root, "dir/b.txt", "b");
    write(root, "dir/c.txt", "c");
    write(root, "keep.txt", "k");
    r.commit_except("most", &["keep.txt".to_string(), "dir/c.txt".to_string()]).unwrap();
    let flat = r.head_flat().unwrap();
    assert!(flat.contains_key("a.txt") && flat.contains_key("dir/b.txt"));
    assert!(!flat.contains_key("keep.txt") && !flat.contains_key("dir/c.txt"));
    let st = r.status().unwrap();
    assert_eq!(st.changes.len(), 2);
    // Excluding a directory excludes everything beneath it.
    write(root, "dir/b.txt", "b2");
    write(root, "other.txt", "o");
    r.commit_except("no dir", &["dir".to_string()]).unwrap();
    assert_eq!(r.file_at("HEAD", "dir/b.txt").unwrap(), b"b");
    assert!(r.head_flat().unwrap().contains_key("other.txt"));
}

#[test]
fn committing_tens_of_thousands_of_selected_paths_is_fast() {
    let (d, mut r) = setup();
    let root = d.path();
    let mut paths = Vec::new();
    for i in 0..20_000 {
        let rel = format!("d{:03}/f{i}.txt", i % 100);
        write(root, &rel, format!("{i}"));
        paths.push(rel);
    }
    let t = std::time::Instant::now();
    r.commit("many", &paths).unwrap();
    let took = t.elapsed();
    assert!(took.as_secs() < 30, "committing 20,000 selected paths took {took:?}");
    assert_eq!(r.head_flat().unwrap().len(), 20_000);
}

#[test]
fn project_gitignore_works_without_git_and_nested_rules_and_rgit_overrides() {
    let outside = tempdir().unwrap();
    write(outside.path(), ".gitignore", "outside.txt\n");
    let root = outside.path().join("project");
    let mut repo = Repo::init(&root, "main").unwrap();
    write(&root, ".gitignore", "*.log\ntarget/\nnode_modules/\nsecret.txt\n!keep.log\n");
    write(&root, ".rgitignore", "!notes.log\nprivate.txt\n");
    write(&root, "src/.gitignore", "generated/\n*.tmp\n!keep.tmp\n");
    for name in ["build.log", "keep.log", "notes.log", "secret.txt", "private.txt", "target/out", "node_modules/pkg/a.js", "src/generated/out", "src/a.tmp", "src/keep.tmp", "src/main.rs", "outside.txt", ".git/objects/not-content"] {
        write(&root, name, name);
    }
    let paths: Vec<String> = repo.status().unwrap().changes.into_iter().map(|c| c.path).collect();
    for name in ["build.log", "secret.txt", "private.txt", "target/out", "node_modules/pkg/a.js", "src/generated/out", "src/a.tmp", ".git/objects/not-content"] {
        assert!(!paths.iter().any(|p| p == name), "must ignore {name}");
    }
    for name in ["keep.log", "notes.log", "src/keep.tmp", "src/main.rs", "outside.txt"] {
        assert!(paths.iter().any(|p| p == name), "must include {name}");
    }
    // The .git metadata above is not required to read project ignore rules.
    fs::remove_dir_all(root.join(".git")).unwrap();
    repo.commit("respect ignore files", &[]).unwrap();
    assert!(!repo.head_flat().unwrap().contains_key("target/out"));
}

#[test]
fn ignore_rules_never_turn_tracked_files_into_deletions() {
    let (d, mut repo) = setup();
    let root = d.path();
    write(root, "cache/tracked.txt", "first");
    write(root, "cache/deleted.txt", "delete me");
    repo.commit("tracked before ignore", &[]).unwrap();
    write(root, ".gitignore", "cache/\n");
    write(root, "cache/tracked.txt", "second");
    write(root, "cache/untracked.txt", "must remain local");
    fs::remove_file(root.join("cache/deleted.txt")).unwrap();
    let status = repo.status().unwrap();
    assert!(status.changes.iter().any(|c| c.path == "cache/tracked.txt" && c.kind == ChangeKind::Modified));
    assert!(status.changes.iter().any(|c| c.path == "cache/deleted.txt" && c.kind == ChangeKind::Deleted));
    assert!(!status.changes.iter().any(|c| c.path == "cache/untracked.txt"));
    repo.commit("tracked changes still count", &[]).unwrap();
    assert_eq!(repo.file_at("HEAD", "cache/tracked.txt").unwrap(), b"second");
    assert!(repo.file_at("HEAD", "cache/deleted.txt").is_err());
    assert!(!repo.head_flat().unwrap().contains_key("cache/untracked.txt"));
}

#[test]
fn restore_entire_root_version_preserves_history_and_untracked_files() {
    let (d, mut repo) = setup();
    let root = d.path();
    write(root, "a.txt", "original");
    let first = repo.commit("first version", &[]).unwrap();
    assert!(repo.restore(Some(&first.to_hex()), &[".".into()], false).unwrap().is_empty());
    write(root, "a.txt", "newer");
    write(root, "later.txt", "added later");
    let second = repo.commit("second version", &[]).unwrap();
    write(root, "local.txt", "untracked local data");
    let restored = repo.restore(Some(&first.to_hex()), &[".".into()], false).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(read(root, "a.txt"), b"original");
    assert!(!root.join("later.txt").exists());
    assert_eq!(read(root, "local.txt"), b"untracked local data");
    assert_eq!(repo.head_commit().unwrap(), Some(second));
    assert_eq!(repo.file_at(&second.to_hex(), "a.txt").unwrap(), b"newer");
}

#[test]
fn whole_version_restore_requires_confirmation_and_keeps_a_recovery_copy() {
    let (d, mut repo) = setup();
    let root = d.path();
    write(root, "a.txt", "original");
    let first = repo.commit("first version", &[]).unwrap();
    write(root, "a.txt", "unsaved edits");
    write(root, "local.txt", "leave me alone");
    assert!(matches!(repo.restore(Some(&first.to_hex()), &[".".into()], false), Err(Error::DirtyWorkingTree(_))));
    assert_eq!(read(root, "a.txt"), b"unsaved edits");
    repo.restore(Some(&first.to_hex()), &[".".into()], true).unwrap();
    assert_eq!(read(root, "a.txt"), b"original");
    assert_eq!(read(root, "local.txt"), b"leave me alone");
    let recovery = repo.list_refs("recovery/").unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(repo.file_at(&recovery[0].1.to_hex(), "a.txt").unwrap(), b"unsaved edits");
}

#[test]
fn ignored_local_file_in_the_way_of_restore_is_protected_and_recoverable() {
    let (d, mut repo) = setup();
    let root = d.path();
    write(root, "private.txt", "old tracked version");
    let original = repo.commit("original", &[]).unwrap();
    fs::remove_file(root.join("private.txt")).unwrap();
    write(root, ".gitignore", "private.txt\n");
    repo.commit("stop tracking private file", &[]).unwrap();
    write(root, "private.txt", "new ignored local data");
    assert!(matches!(repo.restore(Some(&original.to_hex()), &[".".into()], false), Err(Error::DirtyWorkingTree(_))));
    assert_eq!(read(root, "private.txt"), b"new ignored local data");
    repo.restore(Some(&original.to_hex()), &[".".into()], true).unwrap();
    assert_eq!(read(root, "private.txt"), b"old tracked version");
    let recovery = repo.list_refs("recovery/").unwrap();
    assert_eq!(repo.file_at(&recovery[0].1.to_hex(), "private.txt").unwrap(), b"new ignored local data");
}
