//! Deterministic workload generators. Every workload is a pure function of
//! (kind, scale, seed): the same inputs always produce the same bytes.

use std::fs;
use std::path::Path;

use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Source,
    BigFile,
    Mixed,
    Compressed,
    Tiny,
    /// Large database-like files committed as successive full snapshots.
    Snapshots,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "source" => Kind::Source,
            "bigfile" => Kind::BigFile,
            "mixed" => Kind::Mixed,
            "compressed" => Kind::Compressed,
            "tiny" => Kind::Tiny,
            "snapshots" => Kind::Snapshots,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Source => "source",
            Kind::BigFile => "bigfile",
            Kind::Mixed => "mixed",
            Kind::Compressed => "compressed",
            Kind::Tiny => "tiny",
            Kind::Snapshots => "snapshots",
        }
    }
    pub fn all() -> [Kind; 6] {
        [
            Kind::Source,
            Kind::BigFile,
            Kind::Mixed,
            Kind::Compressed,
            Kind::Tiny,
            Kind::Snapshots,
        ]
    }
}

const WORDS: &[&str] = &[
    "buffer", "index", "offset", "length", "chunk", "commit", "branch", "result", "error", "value",
    "state", "config", "handle", "stream", "reader", "writer", "parse", "encode", "decode",
    "verify", "update", "insert", "remove", "lookup", "cache", "queue", "limit", "count", "total",
    "start", "end", "next", "prev", "head", "tail", "node", "tree", "entry", "name", "path",
    "size", "mode", "flags", "token", "event", "timer", "frame", "block", "table", "field",
];

fn word(r: &mut Rng) -> &'static str {
    r.pick(WORDS)
}

fn gen_line(r: &mut Rng) -> String {
    match r.range(0, 8) {
        0 => format!(
            "    let {}_{} = self.{}.{}({});\n",
            word(r),
            word(r),
            word(r),
            word(r),
            r.range(0, 4096)
        ),
        1 => format!(
            "    if {}.{} > {} {{ return Err(Error::{}({:?})); }}\n",
            word(r),
            word(r),
            r.range(0, 1 << 16),
            word(r),
            word(r)
        ),
        2 => format!(
            "    // {} the {} before the {} is {}\n",
            word(r),
            word(r),
            word(r),
            word(r)
        ),
        3 => format!(
            "    for {} in {}.iter().{}() {{ total += {}; }}\n",
            word(r),
            word(r),
            word(r),
            r.range(1, 99)
        ),
        4 => format!(
            "pub fn {}_{}(a: &{}, b: u{}) -> Result<{}, Error> {{\n",
            word(r),
            word(r),
            word(r),
            8 << r.range(0, 4),
            word(r)
        ),
        5 => "}\n\n".to_string(),
        6 => format!(
            "    {}.{}({}, {})?;\n",
            word(r),
            word(r),
            word(r),
            r.range(0, 1000)
        ),
        _ => format!(
            "    const {}_{}: usize = {};\n",
            word(r).to_uppercase(),
            word(r).to_uppercase(),
            r.range(0, 1 << 20)
        ),
    }
}

pub fn gen_source(r: &mut Rng, lines: usize) -> String {
    let mut s = String::new();
    for _ in 0..lines {
        s.push_str(&gen_line(r));
    }
    s
}

/// Structured, repetitive text records (log/CSV-like) of exactly `n` bytes.
pub fn gen_records(r: &mut Rng, n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 128);
    let mut id = r.range(0, 1 << 20);
    while v.len() < n {
        id += r.range(1, 4);
        let line = format!(
            "{},2026-01-{:02}T{:02}:{:02}:{:02}Z,{},{},{:.3},{}\n",
            id,
            r.range(1, 29),
            r.range(0, 24),
            r.range(0, 60),
            r.range(0, 60),
            word(r),
            word(r),
            (r.range(0, 100000) as f64) / 1000.0,
            if r.range(0, 4) == 0 { "ERROR" } else { "OK" }
        );
        v.extend_from_slice(line.as_bytes());
    }
    v.truncate(n);
    v
}

struct FileRec {
    path: String,
    /// 0 = owned by main-line edits, 1 = owned by branch edits (so merges never conflict).
    part: u8,
}

pub struct Workload {
    pub kind: Kind,
    scale: f64,
    files: Vec<FileRec>,
    next_id: usize,
    snapshot: Vec<u8>,
}

fn write(dir: &Path, rel: &str, data: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

impl Workload {
    pub fn new(kind: Kind, scale: f64) -> Self {
        Workload {
            kind,
            scale,
            files: Vec::new(),
            next_id: 0,
            snapshot: Vec::new(),
        }
    }

    fn n(&self, base: usize) -> usize {
        ((base as f64 * self.scale).round() as usize).max(2)
    }

    pub fn steps(&self) -> usize {
        match self.kind {
            Kind::Source => self.n(60),
            Kind::BigFile => self.n(40),
            Kind::Mixed => self.n(40),
            Kind::Compressed => self.n(30),
            Kind::Tiny => self.n(25),
            Kind::Snapshots => self.n(20),
        }
    }

    fn add_file(&mut self, dir: &Path, rel: String, data: &[u8]) {
        write(dir, &rel, data);
        let part = (self.next_id % 2) as u8;
        self.next_id += 1;
        self.files.push(FileRec { path: rel, part });
    }

    pub fn setup(&mut self, dir: &Path, r: &mut Rng) {
        match self.kind {
            Kind::Source => {
                for i in 0..self.n(300) {
                    let lines = r.range(30, 400);
                    let s = gen_source(r, lines);
                    self.add_file(
                        dir,
                        format!("src/mod{:02}/file{:04}.rs", i % 20, i),
                        s.as_bytes(),
                    );
                }
            }
            Kind::BigFile => {
                for i in 0..6 {
                    let size =
                        ((16usize << 20) as f64 * self.scale).max(2.0 * 1024.0 * 1024.0) as usize;
                    let d = gen_records(r, size);
                    self.add_file(dir, format!("data/table{i}.csv"), &d);
                }
            }
            Kind::Mixed => {
                for i in 0..self.n(100) {
                    let lines = r.range(30, 300);
                    let s = gen_source(r, lines);
                    self.add_file(dir, format!("src/file{:03}.rs", i), s.as_bytes());
                }
                for i in 0..self.n(40) {
                    let size = r.range(20_000, 300_000);
                    let d = r.bytes(size);
                    self.add_file(dir, format!("assets/img{:03}.png", i), &d);
                }
                for i in 0..2 {
                    let size = (4usize << 20) / (if self.scale < 1.0 { 4 } else { 1 });
                    let d = gen_records(r, size);
                    self.add_file(dir, format!("data/big{i}.log"), &d);
                }
            }
            Kind::Compressed => {
                for i in 0..self.n(24) {
                    let d = r.bytes(1 << 20);
                    self.add_file(dir, format!("blobs/archive{:02}.bin", i), &d);
                }
            }
            Kind::Snapshots => {
                let size = ((6usize << 20) as f64 * self.scale).max(1_048_576.0) as usize;
                self.snapshot = gen_records(r, size);
                let snap = self.snapshot.clone();
                self.add_file(dir, "snap/db-000.bin".to_string(), &snap);
            }
            Kind::Tiny => {
                for i in 0..self.n(20000) {
                    let size = r.range(50, 500);
                    let d = gen_records(r, size);
                    self.add_file(dir, format!("d{:03}/f{:05}.txt", i % 200, i), &d);
                }
            }
        }
    }

    fn candidates(&self, part: Option<u8>, pred: impl Fn(&str) -> bool) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, f)| part.map(|p| f.part == p).unwrap_or(true) && pred(&f.path))
            .map(|(i, _)| i)
            .collect()
    }

    fn edit_text(&self, dir: &Path, idx: usize, r: &mut Rng) {
        let path = dir.join(&self.files[idx].path);
        let text = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        match r.range(0, 3) {
            0 => {
                for _ in 0..r.range(1, 6) {
                    let i = r.range(0, lines.len());
                    lines[i] = gen_line(r);
                }
            }
            1 => {
                let at = r.range(0, lines.len());
                let block: Vec<String> = (0..r.range(3, 11)).map(|_| gen_line(r)).collect();
                lines.splice(at..at, block);
            }
            _ => {
                let at = r.range(0, lines.len());
                let n = r.range(1, 6).min(lines.len() - at);
                lines.drain(at..at + n);
            }
        }
        fs::write(path, lines.concat()).unwrap();
    }

    fn overwrite_regions(&self, dir: &Path, idx: usize, r: &mut Rng) {
        let path = dir.join(&self.files[idx].path);
        let mut data = fs::read(&path).unwrap();
        for _ in 0..r.range(3, 9) {
            let len = r.range(200, 4000).min(data.len() / 2);
            let at = r.range(0, data.len() - len);
            let patch = gen_records(r, len);
            data[at..at + len].copy_from_slice(&patch);
        }
        fs::write(path, data).unwrap();
    }

    /// Applies step `i`. `part` restricts which files may be edited.
    pub fn step(&mut self, i: usize, dir: &Path, r: &mut Rng, part: Option<u8>) {
        match self.kind {
            Kind::Source => {
                let c = self.candidates(part, |_| true);
                for _ in 0..6 {
                    let idx = *r.pick(&c);
                    self.edit_text(dir, idx, r);
                }
                if i % 7 == 3 && part.is_none() {
                    for _ in 0..2 {
                        let lines = r.range(30, 300);
                        let s = gen_source(r, lines);
                        let id = self.next_id;
                        self.add_file(
                            dir,
                            format!("src/mod{:02}/new{:04}.rs", id % 20, id),
                            s.as_bytes(),
                        );
                    }
                }
                if i % 11 == 5 && part.is_none() {
                    let c = self.candidates(None, |_| true);
                    let idx = *r.pick(&c);
                    fs::remove_file(dir.join(&self.files[idx].path)).unwrap();
                    self.files.remove(idx);
                }
                if i % 13 == 6 && part.is_none() {
                    let c = self.candidates(None, |_| true);
                    let idx = *r.pick(&c);
                    let new = format!("src/moved/r{:04}.rs", self.next_id);
                    self.next_id += 1;
                    fs::create_dir_all(dir.join("src/moved")).unwrap();
                    fs::rename(dir.join(&self.files[idx].path), dir.join(&new)).unwrap();
                    self.files[idx].path = new;
                }
            }
            Kind::BigFile => {
                let c = self.candidates(part, |_| true);
                for _ in 0..2 {
                    let idx = *r.pick(&c);
                    self.overwrite_regions(dir, idx, r);
                }
                if i % 5 == 4 {
                    let idx = *r.pick(&c);
                    let path = dir.join(&self.files[idx].path);
                    let mut d = fs::read(&path).unwrap();
                    let at = r.range(1000, d.len() - 1000);
                    let n = r.range(1000, 3000);
                    let ins = gen_records(r, n);
                    d.splice(at..at, ins);
                    fs::write(path, d).unwrap();
                }
                if i % 8 == 7 {
                    let idx = *r.pick(&c);
                    let path = dir.join(&self.files[idx].path);
                    let mut d = fs::read(&path).unwrap();
                    d.extend(gen_records(r, 100_000));
                    fs::write(path, d).unwrap();
                }
            }
            Kind::Mixed => {
                let src = self.candidates(part, |p| p.starts_with("src/"));
                for _ in 0..3 {
                    let idx = *r.pick(&src);
                    self.edit_text(dir, idx, r);
                }
                if i % 6 == 2 {
                    let a = self.candidates(part, |p| p.starts_with("assets/"));
                    if !a.is_empty() {
                        let idx = *r.pick(&a);
                        let size = r.range(20_000, 300_000);
                        let d = r.bytes(size);
                        fs::write(dir.join(&self.files[idx].path), d).unwrap();
                    }
                }
                if i % 4 == 1 {
                    let a = self.candidates(part, |p| p.starts_with("assets/"));
                    if !a.is_empty() {
                        let idx = *r.pick(&a);
                        let path = dir.join(&self.files[idx].path);
                        let mut d = fs::read(&path).unwrap();
                        let h = r.bytes(32);
                        d[..32].copy_from_slice(&h);
                        fs::write(path, d).unwrap();
                    }
                }
                let big = self.candidates(part, |p| p.starts_with("data/"));
                if !big.is_empty() {
                    let idx = *r.pick(&big);
                    self.overwrite_regions(dir, idx, r);
                }
            }
            Kind::Compressed => {
                let c = self.candidates(part, |_| true);
                for _ in 0..2 {
                    let idx = *r.pick(&c);
                    let d = r.bytes(1 << 20);
                    fs::write(dir.join(&self.files[idx].path), d).unwrap();
                }
                for _ in 0..3 {
                    let idx = *r.pick(&c);
                    let path = dir.join(&self.files[idx].path);
                    let mut d = fs::read(&path).unwrap();
                    let h = r.bytes(16);
                    d[..16].copy_from_slice(&h);
                    fs::write(path, d).unwrap();
                }
            }
            Kind::Snapshots => {
                // Replace ~1.5% of the 4 KiB pages, then commit the result as a new file.
                const PAGE: usize = 4096;
                let pages = self.snapshot.len() / PAGE;
                for _ in 0..(pages * 15 / 1000).max(1) {
                    let p = r.range(0, pages);
                    let patch = gen_records(r, PAGE);
                    self.snapshot[p * PAGE..(p + 1) * PAGE].copy_from_slice(&patch);
                }
                let snap = self.snapshot.clone();
                let id = self.next_id;
                self.add_file(dir, format!("snap/db-{:03}.bin", id), &snap);
                if i % 6 == 5 && part.is_none() {
                    let idx = 0;
                    fs::remove_file(dir.join(&self.files[idx].path)).unwrap();
                    self.files.remove(idx);
                }
            }
            Kind::Tiny => {
                let c = self.candidates(part, |_| true);
                let touch = (self.files.len() / 50).max(2);
                for _ in 0..touch {
                    let idx = *r.pick(&c);
                    let path = dir.join(&self.files[idx].path);
                    let mut d = fs::read(&path).unwrap();
                    let at = r.range(0, d.len());
                    let n = r.range(5, 40);
                    d.splice(at..at, gen_records(r, n));
                    fs::write(path, d).unwrap();
                }
                if part.is_none() {
                    for _ in 0..touch / 2 {
                        let id = self.next_id;
                        let size = r.range(50, 500);
                        let d = gen_records(r, size);
                        self.add_file(dir, format!("d{:03}/n{:05}.txt", id % 200, id), &d);
                    }
                    for _ in 0..touch / 2 {
                        let c = self.candidates(None, |_| true);
                        let idx = *r.pick(&c);
                        fs::remove_file(dir.join(&self.files[idx].path)).unwrap();
                        self.files.remove(idx);
                    }
                }
            }
        }
    }
}
