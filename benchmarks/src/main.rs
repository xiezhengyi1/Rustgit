//! Reproducible storage benchmark: replays identical histories into Git and rgit.
//!
//!   bench run --workload bigfile --seed 1 --rgit target/release/rgit
//!   bench all --rgit target/release/rgit
//!   bench replay --repo /path/to/real/git/repo --commits 100 --rgit target/release/rgit

mod rng;
mod tools;
mod workloads;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};
use rng::Rng;
use serde::Serialize;
use tools::*;
use workloads::{Kind, Workload};

#[derive(Parser)]
struct Cli {
    /// Scratch directory for benchmark repositories (default: system temp)
    #[arg(long, global = true)]
    tmp: Option<PathBuf>,
    /// rgit delta.max_depth for this run (default 4, the plan's limit)
    #[arg(long, global = true)]
    rgit_delta_depth: Option<u32>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one synthetic workload
    Run {
        #[arg(long)]
        workload: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// 1.0 = full size; smaller values shrink file counts and sizes
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        #[arg(long)]
        rgit: PathBuf,
        #[arg(long, default_value = "benchmarks/results")]
        out: PathBuf,
        #[arg(long)]
        keep: bool,
    },
    /// Run every synthetic workload
    All {
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        #[arg(long)]
        rgit: PathBuf,
        #[arg(long, default_value = "benchmarks/results")]
        out: PathBuf,
    },
    /// Write only the initial files of a workload (for inspection and compression experiments)
    Materialize {
        #[arg(long)]
        workload: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        #[arg(long)]
        dir: PathBuf,
    },
    /// Replay the first-parent history of a real Git repository
    Replay {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value_t = 100)]
        commits: usize,
        #[arg(long)]
        rgit: PathBuf,
        #[arg(long, default_value = "benchmarks/results")]
        out: PathBuf,
    },
}

#[derive(Serialize, Default)]
struct VariantResult {
    size: Size,
    files: BTreeMap<String, Size>,
    maintenance: Usage,
    /// Peak allocated bytes of the repository directory while maintenance ran.
    peak_allocated_during_maintenance: u64,
}

#[derive(Serialize, Default)]
struct ToolResult {
    version: String,
    commits: u64,
    commit_total: Usage,
    variants: BTreeMap<String, VariantResult>,
    switch_to_oldest: Usage,
    switch_back: Usage,
    objects_before_100_branches: Option<u64>,
    objects_after_100_branches: Option<u64>,
    repo_bytes_before_100_branches: u64,
    repo_bytes_after_100_branches: u64,
    verify_ok: Option<bool>,
    /// Transfer bytes per maintenance variant of the sending repository.
    transfer: BTreeMap<String, Transfer>,
}

#[derive(Serialize)]
struct Report {
    workload: String,
    seed: u64,
    scale: f64,
    steps: usize,
    working_dir: Size,
    environment: BTreeMap<String, String>,
    tools: BTreeMap<String, ToolResult>,
    comparison: BTreeMap<String, f64>,
    notes: Vec<String>,
}

static SCRATCH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn scratch_root() -> PathBuf {
    SCRATCH.get().cloned().unwrap_or_else(std::env::temp_dir)
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn environment() -> BTreeMap<String, String> {
    let mut e = BTreeMap::new();
    e.insert("os".into(), sh("uname", &["-srm"]));
    if cfg!(target_os = "macos") {
        e.insert(
            "cpu".into(),
            sh("sysctl", &["-n", "machdep.cpu.brand_string"]),
        );
        e.insert("memory_bytes".into(), sh("sysctl", &["-n", "hw.memsize"]));
        e.insert("macos".into(), sh("sw_vers", &["-productVersion"]));
    }
    e.insert("rustc".into(), sh("rustc", &["--version"]));
    e.insert("date".into(), chrono::Utc::now().to_rfc3339());
    e
}

fn commit_all(tool: &dyn Tool, dir: &Path, msg: &str, total: &mut Usage, n: &mut u64) -> R<()> {
    let u = tool.commit(dir, msg)?;
    total.add(&u);
    *n += 1;
    Ok(())
}

/// Replays `wl` into `tool`, returning per-tool results. Branch/merge cycles run
/// every tenth step with disjoint file partitions so merges never conflict.
fn replay_synthetic(
    tool: &dyn Tool,
    dir: &Path,
    kind: Kind,
    seed: u64,
    scale: f64,
) -> R<(ToolResult, Workload)> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    tool.init(dir)?;
    let mut rng = Rng::new(seed);
    let mut wl = Workload::new(kind, scale);
    wl.setup(dir, &mut rng);
    let mut res = ToolResult {
        version: tool.version(),
        ..Default::default()
    };
    commit_all(
        tool,
        dir,
        "initial",
        &mut res.commit_total,
        &mut res.commits,
    )?;
    tool.branch_create(dir, "first")?;
    let steps = wl.steps();
    let mut branch_no = 0;
    for i in 0..steps {
        if i % 10 == 9 {
            branch_no += 1;
            let b = format!("topic{branch_no}");
            tool.branch_create(dir, &b)?;
            tool.switch(dir, &b)?;
            wl.step(i, dir, &mut rng, Some(1));
            commit_all(
                tool,
                dir,
                &format!("topic work {i}"),
                &mut res.commit_total,
                &mut res.commits,
            )?;
            tool.switch(dir, "main")?;
            wl.step(i, dir, &mut rng, Some(0));
            commit_all(
                tool,
                dir,
                &format!("main work {i}"),
                &mut res.commit_total,
                &mut res.commits,
            )?;
            tool.merge(dir, &b)?;
        } else {
            wl.step(i, dir, &mut rng, None);
            commit_all(
                tool,
                dir,
                &format!("step {i}"),
                &mut res.commit_total,
                &mut res.commits,
            )?;
        }
    }
    Ok((res, wl))
}

/// Measurements that run on the finished repository.
fn measure(tool: &dyn Tool, dir: &Path, res: &mut ToolResult, scratch: &Path) -> R<()> {
    // Restore/checkout time of the oldest commit and back.
    res.switch_to_oldest = tool.switch(dir, "first")?;
    res.switch_back = tool.switch(dir, "main")?;
    // Branch cost.
    res.objects_before_100_branches = tool.object_count(dir)?;
    res.repo_bytes_before_100_branches = dir_size(&dir.join(tool.meta_dir())).apparent;
    for i in 0..100 {
        tool.branch_create(dir, &format!("bulk/b{i}"))?;
    }
    res.objects_after_100_branches = tool.object_count(dir)?;
    res.repo_bytes_after_100_branches = dir_size(&dir.join(tool.meta_dir())).apparent;
    // Maintenance variants, each on a pristine copy of the repository.
    for variant in tool.maintenance_variants() {
        let work = scratch.join(format!("{}-{variant}", tool.name()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        copy_dir(&dir.join(tool.meta_dir()), &work.join(tool.meta_dir()))?;
        // The working tree is needed by some commands; Git/rgit maintenance does not read it,
        // but commands expect the project root to exist.
        let meta = work.join(tool.meta_dir());
        let sampler = PeakSampler::start(meta.clone());
        let usage = tool.maintain(&work, variant)?;
        let peak = sampler.finish();
        let size = dir_size(&meta);
        let mut vr = VariantResult {
            size,
            files: breakdown(&meta),
            maintenance: usage,
            peak_allocated_during_maintenance: peak,
        };
        if variant == "pre" {
            vr.peak_allocated_during_maintenance = size.allocated;
        }
        res.variants.insert(variant.to_string(), vr);
        // Transfer bytes from this variant of the repository (a metadata-only copy).
        match tool.transfer(&work, &scratch.join(format!("t-{}-{variant}", tool.name()))) {
            Ok(t) => {
                res.transfer.insert(variant.to_string(), t);
            }
            Err(e) => eprintln!("[{}] transfer measurement for {variant} failed: {e}", tool.name()),
        }
    }
    Ok(())
}

fn finish(
    name: &str,
    seed: u64,
    scale: f64,
    steps: usize,
    working_dir: Size,
    tools: BTreeMap<String, ToolResult>,
    out: &Path,
) -> R<()> {
    let mut comparison = BTreeMap::new();
    if let (Some(g), Some(r)) = (tools.get("git"), tools.get("rgit")) {
        if let (Some(ga), Some(ra)) = (
            g.variants.get("gc_aggressive"),
            r.variants.get("gc_aggressive"),
        ) {
            comparison.insert(
                "rgit_over_git_gc_aggressive_apparent".to_string(),
                ra.size.apparent as f64 / ga.size.apparent as f64,
            );
            comparison.insert(
                "rgit_over_git_gc_aggressive_allocated".to_string(),
                ra.size.allocated as f64 / ga.size.allocated as f64,
            );
        }
        if let (Some(gd), Some(ra)) = (
            g.variants.get("gc_default"),
            r.variants.get("gc_aggressive"),
        ) {
            comparison.insert(
                "rgit_over_git_gc_default_allocated".to_string(),
                ra.size.allocated as f64 / gd.size.allocated as f64,
            );
        }
        if let (Some(gp), Some(rp)) = (g.variants.get("pre"), r.variants.get("pre")) {
            comparison.insert(
                "rgit_over_git_unmaintained_allocated".to_string(),
                rp.size.allocated as f64 / gp.size.allocated as f64,
            );
        }
        for (gv, gt) in &g.transfer {
            // Wire bytes do not depend on rgit's physical layout, so one variant suffices.
            if let Some(rt) = r.transfer.get("pre") {
                let tag = format!("vs_git_{gv}");
                let ratio = |a: u64, b: u64| a as f64 / (b.max(1)) as f64;
                comparison.insert(
                    format!("transfer_initial_upload_rgit_{tag}"),
                    ratio(rt.initial_upload_bytes, gt.initial_upload_bytes),
                );
                comparison.insert(
                    format!("transfer_initial_download_rgit_{tag}"),
                    ratio(rt.initial_download_bytes, gt.initial_download_bytes),
                );
                comparison.insert(
                    format!("transfer_incremental_upload_rgit_{tag}"),
                    ratio(rt.incremental_upload_bytes, gt.incremental_upload_bytes),
                );
                comparison.insert(
                    format!("transfer_incremental_download_rgit_{tag}"),
                    ratio(rt.incremental_download_bytes, gt.incremental_download_bytes),
                );
            }
        }
        comparison.insert(
            "rgit_commit_cpu_over_git".to_string(),
            r.commit_total.cpu_s / g.commit_total.cpu_s.max(1e-9),
        );
    }
    let report = Report {
        workload: name.to_string(),
        seed,
        scale,
        steps,
        working_dir,
        environment: environment(),
        tools,
        comparison,
        notes: vec![
            "Both tools replay identical file contents, commits, branches and merges.".into(),
            "Sizes count every file in .git / .rgit including indexes, SQLite files and caches.".into(),
            "git gc_aggressive = reflog expire + gc --aggressive --prune=now (window 250, depth 50); rgit gc_aggressive = gc --aggressive.".into(),
            "Transfer: git = `git bundle` size and pack bytes written by clone/fetch over file://; rgit = bytes on the TCP connection of `rgit push` / `rgit fetch` against a local `rgit serve`. Only branch main is transferred; the increment is the last N first-parent commits.".into(),
        ],
    };
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let suffix = tools::delta_depth()
        .map(|d| format!("-depth{d}"))
        .unwrap_or_default();
    let file = out.join(format!("{name}-seed{seed}-scale{scale}{suffix}.json"));
    fs::write(&file, serde_json::to_string_pretty(&report).unwrap()).map_err(|e| e.to_string())?;
    print_table(&report);
    println!("raw results: {}", file.display());
    Ok(())
}

fn mib(b: u64) -> String {
    format!("{:.2}", b as f64 / 1048576.0)
}

fn print_table(r: &Report) {
    println!(
        "\n== {} (seed {}, scale {}, {} steps, working dir {} MiB) ==",
        r.workload,
        r.seed,
        r.scale,
        r.steps,
        mib(r.working_dir.apparent)
    );
    println!(
        "{:<6} {:<14} {:>12} {:>12} {:>10} {:>10} {:>12}",
        "tool", "variant", "apparent MiB", "alloc MiB", "maint s", "maint cpu", "peak MiB"
    );
    for (tn, t) in &r.tools {
        for (vn, v) in &t.variants {
            println!(
                "{:<6} {:<14} {:>12} {:>12} {:>10.2} {:>10.2} {:>12}",
                tn,
                vn,
                mib(v.size.apparent),
                mib(v.size.allocated),
                v.maintenance.wall_s,
                v.maintenance.cpu_s,
                mib(v.peak_allocated_during_maintenance)
            );
        }
        for (vn, x) in &t.transfer {
            println!(
                "{:<6} transfer[{}] KiB: full up {:.1} down {:.1}; last {} commits ({} new) up {:.1} down {:.1}",
                tn,
                vn,
                x.initial_upload_bytes as f64 / 1024.0,
                x.initial_download_bytes as f64 / 1024.0,
                x.incremental_back,
                x.incremental_commits,
                x.incremental_upload_bytes as f64 / 1024.0,
                x.incremental_download_bytes as f64 / 1024.0
            );
        }
        println!(
            "{:<6} commits: {} total {:.2}s wall {:.2}s cpu, peak RSS {} MiB; switch oldest {:.2}s back {:.2}s; 100 branches: objects {:?} -> {:?}, repo bytes {} -> {}",
            tn, t.commits, t.commit_total.wall_s, t.commit_total.cpu_s, mib(t.commit_total.max_rss_bytes),
            t.switch_to_oldest.wall_s, t.switch_back.wall_s, t.objects_before_100_branches, t.objects_after_100_branches,
            t.repo_bytes_before_100_branches, t.repo_bytes_after_100_branches
        );
    }
    for (k, v) in &r.comparison {
        println!("  {k}: {v:.3}");
    }
}

fn run_workload(kind: Kind, seed: u64, scale: f64, rgit: &Path, out: &Path, keep: bool) -> R<()> {
    let base = scratch_root().join(format!(
        "rustgit-bench-{}-{seed}-{}",
        kind.name(),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    let mut tools_res = BTreeMap::new();
    let mut steps = 0;
    let mut working = Size::default();
    let result = run_workload_inner(
        kind,
        seed,
        scale,
        rgit,
        out,
        &base,
        &mut tools_res,
        &mut steps,
        &mut working,
    );
    if !keep || result.is_err() {
        let _ = fs::remove_dir_all(&base);
    }
    result?;
    finish(kind.name(), seed, scale, steps, working, tools_res, out)
}

#[allow(clippy::too_many_arguments)]
fn run_workload_inner(
    kind: Kind,
    seed: u64,
    scale: f64,
    rgit: &Path,
    _out: &Path,
    base: &Path,
    tools_res: &mut BTreeMap<String, ToolResult>,
    steps: &mut usize,
    working: &mut Size,
) -> R<()> {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(Git),
        Box::new(Rgit {
            bin: rgit.to_path_buf(),
        }),
    ];
    for tool in &tools {
        eprintln!("[{}] replaying {} ...", tool.name(), kind.name());
        let dir = base.join(tool.name()).join("project");
        let (mut res, wl) = replay_synthetic(tool.as_ref(), &dir, kind, seed, scale)?;
        *steps = wl.steps();
        *working = {
            let total = dir_size(&dir);
            let meta = dir_size(&dir.join(tool.meta_dir()));
            Size {
                apparent: total.apparent - meta.apparent,
                allocated: total.allocated - meta.allocated,
            }
        };
        eprintln!("[{}] measuring ...", tool.name());
        let scratch = base.join(tool.name()).join("scratch");
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        measure(tool.as_ref(), &dir, &mut res, &scratch)?;
        if tool.name() == "rgit" {
            let ok = Command::new(rgit)
                .arg("-C")
                .arg(&dir)
                .arg("verify")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            res.verify_ok = Some(ok);
        }
        tools_res.insert(tool.name().to_string(), res);
    }
    Ok(())
}

/// Real-history replay: check out successive first-parent revisions of `repo` into a
/// working directory and commit each into both tools.
fn run_replay(repo: &Path, commits: usize, rgit: &Path, out: &Path) -> R<()> {
    let revs_out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--first-parent", "--reverse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    let all: Vec<String> = String::from_utf8_lossy(&revs_out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if all.is_empty() {
        return Err("repository has no commits".into());
    }
    // Evenly spaced revisions from the oldest `commits * stride` window ending at HEAD.
    let n = commits.min(all.len());
    let revs: Vec<&String> = (0..n)
        .map(|i| &all[i * (all.len() - 1) / (n.max(2) - 1).max(1)])
        .collect();
    let base = scratch_root().join(format!("rustgit-bench-replay-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(Git),
        Box::new(Rgit {
            bin: rgit.to_path_buf(),
        }),
    ];
    let mut tools_res = BTreeMap::new();
    let mut working = Size::default();
    for tool in &tools {
        let dir = base.join(tool.name()).join("project");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        tool.init(&dir)?;
        let mut res = ToolResult {
            version: tool.version(),
            ..Default::default()
        };
        for (i, rev) in revs.iter().enumerate() {
            // Clear everything but the repository directory, then extract the revision.
            for e in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
                if e.file_name() == tool.meta_dir() {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    let _ = fs::remove_dir_all(&p);
                } else {
                    let _ = fs::remove_file(&p);
                }
            }
            let st = Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "git -C '{}' archive {} | tar -x -C '{}'",
                    repo.display(),
                    rev,
                    dir.display()
                ))
                .status()
                .map_err(|e| e.to_string())?;
            if !st.success() {
                return Err(format!("git archive {rev} failed"));
            }
            match tool.commit(&dir, &format!("rev {i}: {rev}")) {
                Ok(u) => {
                    res.commit_total.add(&u);
                    res.commits += 1;
                    if res.commits == 1 {
                        tool.branch_create(&dir, "first")?;
                    }
                }
                Err(e) if e.contains("nothing") => {}
                Err(e) => return Err(e),
            }
        }
        working = {
            let total = dir_size(&dir);
            let meta = dir_size(&dir.join(tool.meta_dir()));
            Size {
                apparent: total.apparent - meta.apparent,
                allocated: total.allocated - meta.allocated,
            }
        };
        let scratch = base.join(tool.name()).join("scratch");
        fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        measure(tool.as_ref(), &dir, &mut res, &scratch)?;
        tools_res.insert(tool.name().to_string(), res);
    }
    let name = format!(
        "replay-{}",
        repo.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    finish(&name, 0, 1.0, revs.len(), working, tools_res, out)?;
    let _ = fs::remove_dir_all(&base);
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    if let Some(d) = cli.rgit_delta_depth {
        tools::set_delta_depth(d);
    }
    if let Some(t) = &cli.tmp {
        let _ = fs::create_dir_all(t);
        let _ = SCRATCH.set(t.clone());
    }
    let r = match cli.cmd {
        Cmd::Run {
            workload,
            seed,
            scale,
            rgit,
            out,
            keep,
        } => match Kind::parse(&workload) {
            Some(k) => run_workload(k, seed, scale, &rgit, &out, keep),
            None => Err(format!(
                "unknown workload {workload}; choose from source, bigfile, mixed, compressed, tiny"
            )),
        },
        Cmd::All {
            seed,
            scale,
            rgit,
            out,
        } => Kind::all()
            .iter()
            .try_for_each(|k| run_workload(*k, seed, scale, &rgit, &out, false)),
        Cmd::Replay {
            repo,
            commits,
            rgit,
            out,
        } => run_replay(&repo, commits, &rgit, &out),
        Cmd::Materialize {
            workload,
            seed,
            scale,
            dir,
        } => match Kind::parse(&workload) {
            Some(k) => {
                let _ = fs::create_dir_all(&dir);
                Workload::new(k, scale).setup(&dir, &mut Rng::new(seed));
                Ok(())
            }
            None => Err(format!("unknown workload {workload}")),
        },
    };
    if let Err(e) = r {
        eprintln!("benchmark failed: {e}");
        std::process::exit(1);
    }
}
