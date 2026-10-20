use std::fs::{self, OpenOptions};
use std::io::Write;

use rustgit_store::{ObjectId, ObjectType, RepackOptions, Store, StoreOptions};

fn ro(grace: u64) -> RepackOptions {
    RepackOptions {
        grace_secs: grace,
        ..RepackOptions::default()
    }
}
use tempfile::tempdir;

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

fn text(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    let mut i = 0;
    while v.len() < n {
        v.extend_from_slice(
            format!("line {i}: the quick brown fox jumps over the lazy dog\n").as_bytes(),
        );
        i += 1;
    }
    v.truncate(n);
    v
}

#[test]
fn put_get_dedup_and_compression() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let data = text(100_000);
    let a = s.put(ObjectType::Chunk, &data, None).unwrap();
    let b = s.put(ObjectType::Chunk, &data, None).unwrap();
    assert_eq!(a, b);
    s.flush().unwrap();
    let st = s.stats().unwrap();
    assert_eq!(st.objects, 1);
    assert!(
        st.stored_bytes < 20_000,
        "text should compress: {}",
        st.stored_bytes
    );
    let (ty, got) = s.get(&a).unwrap();
    assert_eq!(ty, ObjectType::Chunk);
    assert_eq!(got, data);
    // The same bytes under another type are a different object.
    let c = s.put(ObjectType::Tree, &data, None).unwrap();
    assert_ne!(a, c);
    // Incompressible data is stored raw, never expanded.
    let rnd = pseudo(5, 50_000);
    let r = s.put(ObjectType::Chunk, &rnd, None).unwrap();
    s.flush().unwrap();
    assert_eq!(s.get(&r).unwrap().1, rnd);
    assert!(s.stats().unwrap().stored_bytes <= st.stored_bytes + 20_000 + 50_000);
    // Empty object.
    let e = s.put(ObjectType::Chunk, b"", None).unwrap();
    assert_eq!(s.get(&e).unwrap().1, b"");
}

#[test]
fn delta_encoding_is_small_bounded_and_exact() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let mut cur = pseudo(11, 200_000);
    let mut prev = s.put(ObjectType::Chunk, &cur, None).unwrap();
    let mut versions = vec![(prev, cur.clone())];
    for i in 0..8 {
        cur[1000 + i * 77] ^= 0xff;
        let id = s.put(ObjectType::Chunk, &cur, Some(&prev)).unwrap();
        versions.push((id, cur.clone()));
        prev = id;
    }
    s.flush().unwrap();
    for (id, data) in &versions {
        assert_eq!(&s.get(id).unwrap().1, data);
    }
    let st = s.stats().unwrap();
    assert!(st.delta_objects >= 4, "deltas: {}", st.delta_objects);
    // 9 random 200 KB versions would be 1.8 MB without deltas.
    assert!(st.stored_bytes < 700_000, "stored {}", st.stored_bytes);
    assert!(s.verify_physical().unwrap().is_empty());
}

#[test]
fn survives_reopen_and_detects_missing_object() {
    let d = tempdir().unwrap();
    let id;
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        id = s.put(ObjectType::Commit, b"hello commit", None).unwrap();
        s.flush().unwrap();
    }
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert_eq!(s.get(&id).unwrap().1, b"hello commit");
    assert!(s.get(&ObjectId([9; 32])).is_err());
}

#[test]
fn unflushed_writes_are_not_acknowledged() {
    let d = tempdir().unwrap();
    let id;
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        id = s.put(ObjectType::Chunk, b"never flushed", None).unwrap();
    }
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert!(!s.has(&id).unwrap());
}

fn pack_file(d: &std::path::Path) -> std::path::PathBuf {
    let mut v: Vec<_> = fs::read_dir(d.join("objects"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e == "rgpk").unwrap_or(false))
        .collect();
    v.sort();
    v.pop().unwrap()
}

#[test]
fn torn_append_is_quarantined_and_earlier_data_kept() {
    let d = tempdir().unwrap();
    let id;
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        id = s.put(ObjectType::Chunk, &text(5000), None).unwrap();
        s.flush().unwrap();
    }
    // Simulate a crash in the middle of the next append.
    let mut f = OpenOptions::new()
        .append(true)
        .open(pack_file(d.path()))
        .unwrap();
    f.write_all(&[0, 0, 0xc8, 0x01, 0xc8, 0x01, 1, 2, 3])
        .unwrap();
    drop(f);
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert_eq!(s.get(&id).unwrap().1, text(5000));
    assert!(
        s.warnings().iter().any(|w| w.contains("torn")),
        "{:?}",
        s.warnings()
    );
    assert!(s.verify_physical().unwrap().is_empty());
}

#[test]
fn durable_pack_records_missing_from_index_are_recovered() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    s.put(ObjectType::Chunk, b"first", None).unwrap();
    s.flush().unwrap();
    drop(s);
    // Snapshot the index, write more, then restore the old index: the state after a crash
    // between pack fsync and index commit.
    let idx = d.path().join("index.sqlite");
    let snap = fs::read(&idx).unwrap();
    let mut s = Store::open(d.path(), StoreOptions::default()).unwrap();
    let id2 = s.put(ObjectType::Chunk, b"second", None).unwrap();
    s.flush().unwrap();
    drop(s);
    fs::write(&idx, snap).unwrap();
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert_eq!(s.get(&id2).unwrap().1, b"second");
}

#[test]
fn reindex_rebuilds_deleted_index_including_deltas() {
    let d = tempdir().unwrap();
    let mut ids = Vec::new();
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        let mut cur = pseudo(2, 100_000);
        let mut prev = s.put(ObjectType::Chunk, &cur, None).unwrap();
        ids.push((prev, cur.clone()));
        for i in 0..3 {
            cur[500 + i] ^= 1;
            let id = s.put(ObjectType::Chunk, &cur, Some(&prev)).unwrap();
            ids.push((id, cur.clone()));
            prev = id;
        }
        s.flush().unwrap();
    }
    fs::remove_file(d.path().join("index.sqlite")).unwrap();
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    for (id, data) in &ids {
        assert_eq!(&s.get(id).unwrap().1, data);
    }
    assert!(s.stats().unwrap().delta_objects > 0);
}

#[test]
fn corruption_is_detected_not_returned() {
    let d = tempdir().unwrap();
    let id;
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        id = s.put(ObjectType::Chunk, &pseudo(3, 10_000), None).unwrap();
        s.flush().unwrap();
    }
    let p = pack_file(d.path());
    let mut bytes = fs::read(&p).unwrap();
    bytes[200] ^= 0x55;
    fs::write(&p, bytes).unwrap();
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert!(s.get(&id).is_err());
    assert!(!s.verify_physical().unwrap().is_empty());
}

#[test]
fn repack_drops_dead_objects_and_keeps_live_content_exact() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let base_data = pseudo(21, 120_000);
    let base = s.put(ObjectType::Chunk, &base_data, None).unwrap();
    let mut v2 = base_data.clone();
    v2[100] ^= 1;
    let live = s.put(ObjectType::Chunk, &v2, Some(&base)).unwrap();
    let dead = s.put(ObjectType::Chunk, &pseudo(22, 50_000), None).unwrap();
    s.flush().unwrap();
    // `live` was a delta against `base`; the repack re-encodes it, so `base` may go.
    let st = s.repack(&[(live, None)], &ro(0)).unwrap();
    assert!(st.dropped >= 1);
    assert!(!s.has(&dead).unwrap());
    assert_eq!(s.get(&live).unwrap().1, v2);
    s.prune(0).unwrap();
    let after = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert_eq!(after.get(&live).unwrap().1, v2);
    assert!(after.verify_physical().unwrap().is_empty());
    assert_eq!(after.stats().unwrap().packs, 1);
}

#[test]
fn repack_keeps_a_base_that_a_surviving_delta_still_needs() {
    let d = tempdir().unwrap();
    let base_data = pseudo(41, 120_000);
    let mut v2 = base_data.clone();
    v2[500] ^= 7;
    let (base, live);
    {
        let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
        base = s.put(ObjectType::Chunk, &base_data, None).unwrap();
        s.flush().unwrap();
        s.roll().unwrap();
        live = s.put(ObjectType::Chunk, &v2, Some(&base)).unwrap();
        s.flush().unwrap();
        assert_eq!(s.stats().unwrap().delta_objects, 1);
    }
    // Make the first pack look old and the second recent.
    let db = rusqlite::Connection::open(d.path().join("index.sqlite")).unwrap();
    db.execute("UPDATE packs SET last_write = 0 WHERE id = 1", [])
        .unwrap();
    drop(db);
    let mut s = Store::open(d.path(), StoreOptions::default()).unwrap();
    // The base is not in the live list, but a delta in a pack we are not touching needs it.
    s.repack(&[], &ro(3600)).unwrap();
    assert_eq!(s.get(&live).unwrap().1, v2);
    assert_eq!(s.get(&base).unwrap().1, base_data);
    s.prune(0).unwrap();
    let after = Store::open(d.path(), StoreOptions::default()).unwrap();
    assert_eq!(after.get(&live).unwrap().1, v2);
    assert!(after.verify_physical().unwrap().is_empty());
}

#[test]
fn small_objects_are_grouped_smaller_and_still_exact() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let mut items = Vec::new();
    let mut want = Vec::new();
    for i in 0..400 {
        let body = format!("fn handler_{i}(req: &Request) -> Response {{\n    let id = req.param(\"id\");\n    log::info!(\"handling {{}}\", id);\n    Response::ok(format!(\"item {i}\"))\n}}\n");
        let id = s.put(ObjectType::Chunk, body.as_bytes(), None).unwrap();
        items.push((id, None));
        want.push((id, body));
    }
    s.flush().unwrap();
    let before = s.stats().unwrap().stored_bytes;
    let st = s.repack(&items, &ro(0)).unwrap();
    assert!(st.groups >= 1 && st.grouped_objects >= 300, "{st:?}");
    s.prune(0).unwrap();
    let after = s.stats().unwrap();
    assert!(
        after.stored_bytes * 2 < before,
        "grouped {} vs individual {before}",
        after.stored_bytes
    );
    for (id, body) in &want {
        assert_eq!(s.get(id).unwrap().1, body.as_bytes());
    }
    assert!(s.verify_physical().unwrap().is_empty());
    drop(s);
    // The index is rebuildable from groups too.
    fs::remove_file(d.path().join("index.sqlite")).unwrap();
    let s = Store::open(d.path(), StoreOptions::default()).unwrap();
    for (id, body) in &want {
        assert_eq!(s.get(id).unwrap().1, body.as_bytes());
    }
}

#[test]
fn delta_chain_past_depth_limit_picks_an_older_base_instead_of_a_full_copy() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let mut cur = pseudo(51, 150_000);
    let mut prev = s.put(ObjectType::Chunk, &cur, None).unwrap();
    let mut all = vec![(prev, cur.clone())];
    for i in 0..12 {
        cur[2_000 + i * 100] ^= 0x5a;
        let id = s.put(ObjectType::Chunk, &cur, Some(&prev)).unwrap();
        all.push((id, cur.clone()));
        prev = id;
    }
    s.flush().unwrap();
    let st = s.stats().unwrap();
    // Without fallback bases every fifth version would be a full 150 KB copy (>450 KB total).
    assert!(st.stored_bytes < 300_000, "stored {}", st.stored_bytes);
    for (id, data) in &all {
        assert_eq!(&s.get(id).unwrap().1, data);
    }
    assert!(s.verify_physical().unwrap().is_empty());
}

#[test]
fn repack_respects_grace_period_for_recent_packs() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let recent = s
        .put(ObjectType::Chunk, b"recent unreferenced", None)
        .unwrap();
    s.flush().unwrap();
    let st = s.repack(&[], &ro(3600)).unwrap();
    assert_eq!(st.dropped, 0);
    assert!(s.has(&recent).unwrap());
}

#[test]
fn repack_creates_deltas_when_hints_are_given() {
    let d = tempdir().unwrap();
    let mut s = Store::create(d.path(), StoreOptions::default()).unwrap();
    let a_data = pseudo(31, 150_000);
    let mut b_data = a_data.clone();
    b_data[7_000] ^= 9;
    let a = s.put(ObjectType::Chunk, &a_data, None).unwrap();
    let b = s.put(ObjectType::Chunk, &b_data, None).unwrap();
    s.flush().unwrap();
    assert_eq!(s.stats().unwrap().delta_objects, 0);
    let st = s.repack(&[(a, None), (b, Some(a))], &ro(0)).unwrap();
    assert_eq!(st.new_deltas, 1);
    s.prune(0).unwrap();
    assert_eq!(s.get(&b).unwrap().1, b_data);
    assert!(s.stats().unwrap().pack_file_bytes < 200_000);
}

#[test]
fn rolls_to_new_pack_at_limit() {
    let d = tempdir().unwrap();
    let opts = StoreOptions {
        pack_max: 100_000,
        ..StoreOptions::default()
    };
    let mut s = Store::create(d.path(), opts.clone()).unwrap();
    let mut ids = Vec::new();
    for i in 0..6 {
        let data = pseudo(100 + i, 60_000);
        ids.push((s.put(ObjectType::Chunk, &data, None).unwrap(), data));
        s.flush().unwrap();
    }
    assert!(s.stats().unwrap().packs >= 3);
    for (id, data) in &ids {
        assert_eq!(&s.get(id).unwrap().1, data);
    }
}
