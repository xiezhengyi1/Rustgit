use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use std::collections::HashMap;

use rustgit_core::diff::{diff_flat, Change, ChangeKind};
use rustgit_core::objects::Commit;
use rustgit_core::ops::MergeResult;
use rustgit_core::ObjectId;
use serde_json::{json, Value};
use rustgit_core::ops::ResolveWith;
use rustgit_core::{Error, Flat, Repo};

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

mod auth;
mod iroh;
mod net;
mod out;
mod sync;

#[derive(Parser)]
#[command(
    name = "rgit",
    version,
    about = "Peer-hostable version control with a chunked, deduplicating store"
)]
struct Cli {
    /// Run as if started in this directory
    #[arg(short = 'C', long = "dir", global = true, default_value = ".")]
    dir: PathBuf,
    /// Print one JSON object (see docs/gui-interface.md) instead of human text
    #[arg(long, global = true)]
    json: bool,
    /// Keep private keys in a plain 0600 file instead of the OS key store (macOS Keychain,
    /// Windows Credential Manager). Explicit opt-in: the file is NOT protected by the OS.
    #[arg(long, global = true, env = "RGIT_INSECURE_FILE_KEYS", value_parser = clap::builder::BoolishValueParser::new())]
    insecure_file_keys: bool,
    /// Directory for --insecure-file-keys (default: <config dir>/keys)
    #[arg(long, global = true, env = "RGIT_KEY_DIR")]
    key_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print version information (use with --json)
    Version,
    /// Create a project in the directory
    Init {
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Show or set configuration (user.name, user.email, chunk.avg, ...)
    Config { key: String, value: Option<String> },
    /// Show uncommitted changes
    Status,
    /// Commit all changes, or only the listed files/directories
    Commit {
        #[arg(short, long, default_value = "")]
        message: String,
        paths: Vec<String>,
        /// Read the paths to commit from this file (one per line; `-` is stdin)
        #[arg(long)]
        paths_from: Option<String>,
        /// Commit everything except the paths listed in this file (one per line; `-` is stdin)
        #[arg(long, conflicts_with_all = ["paths", "paths_from"])]
        except_from: Option<String>,
    },
    /// Show history
    Log {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Branch or revision to start from
        rev: Option<String>,
    },
    /// Show one commit and its changes
    Show { rev: String },
    /// Show changes: working tree vs HEAD, or between two revisions
    Diff {
        a: Option<String>,
        b: Option<String>,
    },
    /// List, create or delete branches
    Branch {
        name: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(short, long)]
        delete: bool,
        #[arg(long)]
        force: bool,
    },
    /// Switch branches (refuses to overwrite uncommitted work)
    Switch {
        name: String,
        #[arg(short = 'c', long)]
        create: bool,
    },
    /// Restore files from HEAD or another revision
    Restore {
        #[arg(long)]
        from: Option<String>,
        /// Overwrite uncommitted edits (a recovery snapshot is kept)
        #[arg(long)]
        force: bool,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Create a commit that undoes an earlier commit
    Revert { rev: String },
    /// Merge a branch into the current branch
    Merge {
        branch: Option<String>,
        #[arg(long)]
        abort: bool,
    },
    /// Mark a conflicted path resolved
    Resolve {
        path: String,
        #[arg(long, conflicts_with = "theirs")]
        ours: bool,
        #[arg(long)]
        theirs: bool,
    },
    /// Save uncommitted work aside and reset to HEAD
    Shelve {
        #[arg(short, long, default_value = "")]
        message: String,
    },
    /// Re-apply a shelf
    Unshelve { id: String },
    /// List shelves and recovery snapshots
    Shelves,
    /// Delete a shelf
    ShelfDrop { id: String },
    /// Repack and drop unreachable history older than the grace period
    Gc {
        #[arg(long, default_value_t = 3600)]
        grace: u64,
        #[arg(long, default_value_t = 86400)]
        prune_grace: u64,
        /// Same as --grace 0 --prune-grace 0, plus maximum compression (slow)
        #[arg(long)]
        aggressive: bool,
    },
    /// Verify every object and all reachable history
    Verify,
    /// Rebuild the object index from pack files
    Reindex,
    /// Repository size and deduplication statistics
    Stats,
    /// Host a bare project repository over TCP (loopback unless told otherwise)
    Serve {
        /// Directory of the hosted repository (default: --dir)
        dir: Option<PathBuf>,
        /// Create the hosted repository if it does not exist
        #[arg(long)]
        init: bool,
        /// Address to listen on; port 0 picks a free one
        #[arg(long, default_value = "127.0.0.1:7878")]
        listen: String,
        /// Write the bound address to this file once listening
        #[arg(long)]
        port_file: Option<PathBuf>,
        /// No longer needed: the default (authenticated, encrypted) mode may listen anywhere.
        /// Accepted for old scripts; it does NOT make --insecure-open-access listen beyond loopback.
        #[arg(long, hide = true)]
        insecure_listen_any: bool,
        /// Serve with NO authentication and NO encryption: anyone who can connect can read and
        /// write the project. Loopback addresses only; for local development and tests
        #[arg(long)]
        insecure_open_access: bool,
        /// Accept force pushes (history replacement). Off by default.
        #[arg(long)]
        allow_force: bool,
        /// Accept branch deletion. Off by default.
        #[arg(long)]
        allow_delete: bool,
        /// Quota for one upload session's staged bytes
        #[arg(long)]
        max_session_bytes: Option<u64>,
        /// Refuse uploads once the repository exceeds this many bytes
        #[arg(long)]
        max_repo_bytes: Option<u64>,
        /// Log connections and rejected requests to stderr
        #[arg(long)]
        log: bool,
        /// Serve exactly one session over stdin/stdout and exit (for `ssh host rgit serve --stdio`,
        /// an sshd forced command or subsystem). stdout carries the protocol; cannot be used with --json.
        #[arg(long)]
        stdio: bool,
        /// Advertise this host on the local network with mDNS (`_rustgit._tcp`): only an opaque
        /// project hash, the protocol version and the port are published, never the project name
        #[arg(long)]
        advertise: bool,
        #[command(flatten)]
        iroh: crate::iroh::ServeArgs,
    },
    /// Find rgit hosts advertised on the local network (mDNS). Results are hints, not identities
    Discover {
        /// Seconds to listen
        #[arg(long, default_value_t = 3.0)]
        timeout: f64,
        /// Keep listening and report hosts as they appear and disappear (until terminated)
        #[arg(long)]
        watch: bool,
        /// Mark hosts whose hint matches this project UUID
        #[arg(long)]
        project: Option<String>,
        /// Like --project, with the UUID pinned for this remote of the current project
        #[arg(long)]
        remote: Option<String>,
        /// Also report loopback addresses (this machine's own hosts)
        #[arg(long)]
        include_loopback: bool,
        /// Do not try a plain TCP connect to each host to see whether it still answers
        #[arg(long)]
        no_probe: bool,
    },
    /// Copy a hosted project into a new working directory
    Clone {
        url: String,
        dest: Option<PathBuf>,
        /// Pin the host's identity (64-digit fingerprint) and connect over the authenticated,
        /// encrypted channel. `rgit join` does this for you
        #[arg(long)]
        host_key: Option<String>,
    },
    /// Download new work from the host into remotes/<name>/... (never touches your files)
    Fetch {
        #[arg(default_value = "origin")]
        remote: String,
        /// Keep running and fetch whenever the host's branches change
        #[arg(long)]
        watch: bool,
    },
    /// Upload a branch to the host (fast-forward only; fetch and merge if rejected)
    Push {
        #[arg(default_value = "origin")]
        remote: String,
        /// Local branch (default: current)
        branch: Option<String>,
        /// Branch name on the host (default: same as local)
        #[arg(long = "as")]
        remote_branch: Option<String>,
        /// Ask the host to replace history (hosts refuse unless configured to allow it)
        #[arg(long)]
        force: bool,
        /// Delete this branch on the host instead of pushing
        #[arg(long)]
        delete: Option<String>,
    },
    /// Fetch, then merge the host's version of the current branch
    Pull {
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// List, add or remove remotes
    Remote {
        action: Option<String>,
        name: Option<String>,
        url: Option<String>,
        /// With `add`: pin the host's identity fingerprint (authenticated, encrypted channel)
        #[arg(long)]
        host_key: Option<String>,
    },
    /// This device's identity (Ed25519 key kept in the OS key store)
    Device {
        #[command(subcommand)]
        action: auth::DeviceCmd,
    },
    /// Create, list and revoke invitation links (run in the hosted project)
    Invite {
        #[command(subcommand)]
        action: auth::InviteCmd,
    },
    /// Join a project with an invitation link: enroll this device, then clone
    Join(auth::JoinArgs),
    /// List and manage devices authorized on a hosted project
    Devices {
        #[command(subcommand)]
        action: auth::DevicesCmd,
    },
    /// The hosted project's own identity key
    Host {
        #[command(subcommand)]
        action: auth::HostCmd,
    },
}

fn short_err(e: &Error) -> String {
    e.to_string()
}

/// Serializes a change list straight to the output (no intermediate JSON tree: a 30,000-file
/// status used to build ~18 MiB of `Value` nodes).
struct Changes<'a>(&'a [Change]);

impl serde::Serialize for Changes<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        #[derive(serde::Serialize)]
        struct One<'a> {
            path: &'a str,
            kind: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            from: Option<&'a str>,
        }
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for c in self.0 {
            let (kind, from) = match &c.kind {
                ChangeKind::Added => ("added", None),
                ChangeKind::Modified => ("modified", None),
                ChangeKind::Deleted => ("deleted", None),
                ChangeKind::Renamed(f) => ("renamed", Some(f.as_str())),
            };
            seq.serialize_element(&One { path: &c.path, kind, from })?;
        }
        seq.end()
    }
}

#[derive(serde::Serialize)]
struct StatusOut<'a> {
    branch: &'a str,
    head: Option<String>,
    changes: Changes<'a>,
    skipped: Vec<serde_json::Value>,
    merge: Option<serde_json::Value>,
    conflicts: Vec<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct DiffOut<'a> {
    changes: Changes<'a>,
    diff: &'a str,
}

#[derive(serde::Serialize)]
struct ShowOut<'a> {
    commit: serde_json::Value,
    changes: Changes<'a>,
    diff: &'a str,
}

#[allow(dead_code)]
fn changes_json(changes: &[Change]) -> Value {
    Value::Array(
        changes
            .iter()
            .map(|c| match &c.kind {
                ChangeKind::Added => json!({"path": c.path, "kind": "added"}),
                ChangeKind::Modified => json!({"path": c.path, "kind": "modified"}),
                ChangeKind::Deleted => json!({"path": c.path, "kind": "deleted"}),
                ChangeKind::Renamed(f) => json!({"path": c.path, "kind": "renamed", "from": f}),
            })
            .collect(),
    )
}

fn commit_json(id: &ObjectId, c: &Commit, tips: &HashMap<ObjectId, Vec<String>>) -> Value {
    json!({
        "id": id.to_hex(),
        "short": id.short(),
        "parents": c.parents.iter().map(|p| p.to_hex()).collect::<Vec<_>>(),
        "author_name": c.author_name,
        "author_email": c.author_email,
        "timestamp": c.timestamp,
        "tz_offset_min": c.tz_offset_min,
        "message": c.message,
        "branches": tips.get(id).cloned().unwrap_or_default(),
    })
}

fn branch_tips(repo: &Repo) -> Result<HashMap<ObjectId, Vec<String>>, Error> {
    let mut m: HashMap<ObjectId, Vec<String>> = HashMap::new();
    for (n, id) in repo.branches()? {
        m.entry(id).or_default().push(n);
    }
    Ok(m)
}

pub(crate) fn merge_json(r: &MergeResult, restored: Option<Vec<String>>) -> Value {
    let mut v = json!({
        "commit": r.commit.map(|c| c.to_hex()),
        "fast_forward": r.fast_forward,
        "already_up_to_date": r.already_up_to_date,
        "conflicts": r.conflicts,
    });
    if let Some(p) = restored {
        v["restored"] = json!(p);
    }
    v
}

fn run(cli: Cli) -> Result<u8, Error> {
    out::set_json(cli.json);
    auth::init_key_opts(cli.insecure_file_keys, cli.key_dir.clone());
    if let Cmd::Version = &cli.cmd {
        crate::result!(json!({
            "cli": env!("CARGO_PKG_VERSION"),
            "json_interface": out::JSON_INTERFACE,
            "format": rustgit_core::repo::SUPPORTED_FORMAT,
        }));
        say!("rgit {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if let Cmd::Init { branch } = &cli.cmd {
        std::fs::create_dir_all(&cli.dir)?;
        let r = Repo::init(&std::fs::canonicalize(&cli.dir)?, branch)?;
        say!("Initialized project {} in {}", r.uuid()?, r.root.display());
        crate::result!(json!({"uuid": r.uuid()?, "root": r.root.to_string_lossy()}));
        return Ok(0);
    }
    match &cli.cmd {
        Cmd::Serve { .. } => return sync::serve(cli),
        Cmd::Discover { .. } => return net::discover(cli),
        Cmd::Clone { .. } => return sync::clone(cli),
        Cmd::Device { .. } | Cmd::Invite { .. } | Cmd::Join(_) | Cmd::Devices { .. } | Cmd::Host { .. } => {
            return auth::run(cli)
        }
        _ => {}
    }
    let mut repo = Repo::discover(&cli.dir)?;
    let _maintenance_lock = sync::guard_hosted(&repo, &cli.cmd)?;
    if !cli.json {
        for w in repo.store.warnings() {
            eprintln!("warning: {w}");
        }
    }
    match cli.cmd {
        Cmd::Version | Cmd::Init { .. } => unreachable!(),
        Cmd::Config { key, value } => match value {
            Some(v) => {
                repo.set_user_config(&key, &v)?;
                crate::result!(json!({"key": key, "value": v}));
            }
            None => {
                let v = repo.get_config(&key)?.unwrap_or_default();
                say!("{v}");
                crate::result!(json!({"key": key, "value": v}));
            }
        },
        Cmd::Status => {
            let st = repo.status()?;
            say!(
                "On branch {}{}",
                st.branch,
                if st.head.is_none() { " (no commits yet)" } else { "" }
            );
            if let Some(m) = &st.merge {
                say!("{} in progress ({}):", m.kind, m.label);
                for c in &st.conflicts {
                    say!(
                        "  {} {} [{}]",
                        if c.resolved { "resolved  " } else { "CONFLICT  " },
                        c.path,
                        c.kind
                    );
                }
            }
            if st.changes.is_empty() {
                say!("Nothing to commit, working tree clean");
            }
            for c in &st.changes {
                match &c.kind {
                    ChangeKind::Added => say!("  added     {}", c.path),
                    ChangeKind::Modified => say!("  modified  {}", c.path),
                    ChangeKind::Deleted => say!("  deleted   {}", c.path),
                    ChangeKind::Renamed(f) => say!("  renamed   {f} -> {}", c.path),
                }
            }
            for (p, why) in &st.skipped {
                say!("  skipped   {p}: {why}");
            }
            crate::result!(StatusOut {
                branch: &st.branch,
                head: st.head.map(|h| h.to_hex()),
                changes: Changes(&st.changes),
                skipped: st.skipped.iter().map(|(p, r)| json!({"path": p, "reason": r})).collect(),
                merge: st.merge.as_ref().map(|m| json!({"kind": m.kind, "label": m.label})),
                conflicts: st.conflicts.iter().map(|c| json!({"path": c.path, "kind": c.kind, "resolved": c.resolved})).collect(),
            });
        }
        Cmd::Commit { message, mut paths, paths_from, except_from } => {
            let read_list = |f: &str| -> Result<Vec<String>, Error> {
                let text = if f == "-" {
                    let mut t = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
                    t
                } else {
                    std::fs::read_to_string(f)?
                };
                Ok(text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
            };
            let id = if let Some(f) = except_from {
                repo.commit_except(&message, &read_list(&f)?)?
            } else {
                if let Some(f) = paths_from {
                    paths.extend(read_list(&f)?);
                }
                repo.commit(&message, &paths)?
            };
            say!("[{} {}] committed", repo.current_branch()?, id.short());
            crate::result!(json!({"id": id.to_hex(), "short": id.short(), "branch": repo.current_branch()?}));
        }
        Cmd::Log { limit, rev } => {
            let start = match rev {
                Some(r) => Some(repo.resolve_rev(&r)?),
                None => repo.head_commit()?,
            };
            let mut commits = Vec::new();
            if let Some(start) = start {
                let tips = branch_tips(&repo)?;
                for (id, c) in repo.history(&start, limit)? {
                    let when = chrono::DateTime::from_timestamp(c.timestamp, 0)
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_default();
                    let merge = if c.parents.len() > 1 { " (merge)" } else { "" };
                    say!("{}{} {} <{}> {}", id.short(), merge, c.author_name, c.author_email, when);
                    for l in c.message.lines() {
                        say!("    {l}");
                    }
                    commits.push(commit_json(&id, &c, &tips));
                }
            } else if !cli.json {
                return Err(Error::Other("no commits yet".into()));
            }
            crate::result!(json!({ "commits": commits }));
        }
        Cmd::Show { rev } => {
            let id = repo.resolve_rev(&rev)?;
            let (c, changes) = repo.show_commit(&id)?;
            say!(
                "commit {id}\nAuthor: {} <{}>\nParents: {}\n\n    {}\n",
                c.author_name,
                c.author_email,
                c.parents.iter().map(|p| p.short()).collect::<Vec<_>>().join(" "),
                c.message.replace('\n', "\n    ")
            );
            let diff = repo.render_diff(&changes)?;
            say_raw!("{diff}");
            let tips = branch_tips(&repo)?;
            crate::result!(ShowOut { commit: commit_json(&id, &c, &tips), changes: Changes(&changes), diff: &diff });
        }
        Cmd::Diff { a, b } => {
            let from_work = b.is_none();
            let (old, new): (Flat, Flat) = match (a, b) {
                (None, _) => {
                    let head = repo.head_flat()?;
                    let scan = repo.scan()?;
                    let work = repo.work_flat(&scan, false, &head, None)?;
                    (head, work)
                }
                (Some(a), None) => {
                    let ida = repo.resolve_rev(&a)?;
                    let old = repo.commit_flat(&ida)?;
                    let scan = repo.scan()?;
                    let work = repo.work_flat(&scan, false, &old, None)?;
                    (old, work)
                }
                (Some(a), Some(b)) => {
                    let (ia, ib) = (repo.resolve_rev(&a)?, repo.resolve_rev(&b)?);
                    (repo.commit_flat(&ia)?, repo.commit_flat(&ib)?)
                }
            };
            let changes = diff_flat(&old, &new);
            let diff = if from_work { repo.render_diff_work(&changes)? } else { repo.render_diff(&changes)? };
            say_raw!("{diff}");
            crate::result!(DiffOut { changes: Changes(&changes), diff: &diff });
        }
        Cmd::Branch { name, from, delete, force } => match (name, delete) {
            (None, _) => {
                let cur = repo.current_branch()?;
                let mut list = Vec::new();
                for (n, id) in repo.branches()? {
                    say!("{} {n}  {}", if n == cur { "*" } else { " " }, id.short());
                    list.push(json!({"name": n, "id": id.to_hex(), "short": id.short()}));
                }
                crate::result!(json!({"current": cur, "branches": list}));
            }
            (Some(n), true) => {
                let tip = repo.branch_delete(&n, force)?;
                say!("Deleted branch {n} (was {})", tip.short());
                crate::result!(json!({"name": n, "id": tip.to_hex()}));
            }
            (Some(n), false) => {
                let id = repo.branch_create(&n, from.as_deref())?;
                say!("Created branch {n} at {}", id.short());
                crate::result!(json!({"name": n, "id": id.to_hex()}));
            }
        },
        Cmd::Switch { name, create } => {
            repo.switch(&name, create)?;
            say!("Switched to branch {name}");
            crate::result!(json!({"branch": name}));
        }
        Cmd::Restore { from, force, paths } => {
            let done = repo.restore(from.as_deref(), &paths, force)?;
            for p in &done {
                say!("restored {p}");
            }
            crate::result!(merge_json(
                &MergeResult { commit: None, fast_forward: false, already_up_to_date: false, conflicts: vec![] },
                Some(done),
            ));
        }
        Cmd::Revert { rev } => {
            let r = repo.revert(&rev)?;
            report_merge(&r);
            crate::result!(merge_json(&r, None));
        }
        Cmd::Merge { branch, abort } => {
            if abort {
                repo.merge_abort()?;
                say!("Merge aborted");
                crate::result!(json!({"aborted": true}));
            } else {
                let b = branch.ok_or_else(|| Error::Other("name the branch to merge".into()))?;
                let r = repo.merge(&b)?;
                report_merge(&r);
                crate::result!(merge_json(&r, None));
                if !r.conflicts.is_empty() {
                    return Ok(1);
                }
            }
        }
        Cmd::Resolve { path, ours, theirs } => {
            let how = if ours {
                ResolveWith::Ours
            } else if theirs {
                ResolveWith::Theirs
            } else {
                ResolveWith::Manual
            };
            repo.resolve(&path, how)?;
            say!("Resolved {path}");
            crate::result!(json!({"path": path}));
        }
        Cmd::Shelve { message } => {
            let name = repo.shelve(&message)?;
            say!("Saved {name}");
            crate::result!(json!({"name": name}));
        }
        Cmd::Unshelve { id } => {
            let r = repo.unshelve(&id)?;
            report_merge(&r);
            crate::result!(merge_json(&r, None));
        }
        Cmd::Shelves => {
            let mut shelves = Vec::new();
            let mut recovery = Vec::new();
            for prefix in ["shelf/", "recovery/"] {
                for (n, id) in repo.list_refs(prefix)? {
                    let c = repo.read_commit(&id)?;
                    let msg = c.message.lines().next().unwrap_or("").to_string();
                    say!("{prefix}{n}  {}  {msg}", id.short());
                    let e = json!({"ref": format!("{prefix}{n}"), "name": n, "id": id.to_hex(), "message": msg});
                    if prefix == "shelf/" { shelves.push(e) } else { recovery.push(e) }
                }
            }
            crate::result!(json!({"shelves": shelves, "recovery": recovery}));
        }
        Cmd::ShelfDrop { id } => {
            repo.drop_shelf(&id)?;
            crate::result!(json!({}));
        }
        Cmd::Gc { grace, prune_grace, aggressive } => {
            let (g, p) = if aggressive { (0, 0) } else { (grace, prune_grace) };
            let before = repo.store.disk_usage()?;
            let r = repo.gc(g, p, aggressive)?;
            let after = repo.store.disk_usage()?;
            say!(
                "Repacked: {} objects kept, {} dropped ({} bytes logical); {} bytes freed; repository {} -> {} bytes",
                r.repack.moved, r.repack.dropped, r.repack.dropped_logical_bytes, r.freed_bytes, before, after
            );
            crate::result!(json!({
                "moved": r.repack.moved, "dropped": r.repack.dropped, "freed_bytes": r.freed_bytes,
                "repo_bytes_before": before, "repo_bytes_after": after,
            }));
        }
        Cmd::Verify => {
            let r = repo.verify()?;
            say!("Checked {} objects and {} commits", r.objects_checked, r.commits);
            crate::result!(json!({"objects_checked": r.objects_checked, "commits": r.commits, "problems": r.problems}));
            if r.problems.is_empty() {
                say!("OK: all reachable history reconstructs");
            } else {
                for p in &r.problems {
                    say!("PROBLEM: {p}");
                }
                say!("If only the index is damaged run `rgit reindex`; missing objects need another replica or backup.");
                return Ok(2);
            }
        }
        Cmd::Reindex => {
            let r = repo.reindex()?;
            say!("Reindexed {} objects from {} packs", r.objects, r.packs);
            for p in &r.problems {
                say!("PROBLEM: {p}");
            }
            crate::result!(json!({"objects": r.objects, "packs": r.packs, "problems": r.problems}));
        }
        Cmd::Fetch { remote, watch } => return sync::fetch(&mut repo, &remote, watch),
        Cmd::Push { remote, branch, remote_branch, force, delete } => {
            return sync::push(&mut repo, &remote, branch, remote_branch, force, delete)
        }
        Cmd::Pull { remote } => return sync::pull(&mut repo, &remote),
        Cmd::Remote { action, name, url, host_key } => {
            return sync::remote(&mut repo, action, name, url, host_key)
        }
        Cmd::Serve { .. }
        | Cmd::Clone { .. }
        | Cmd::Discover { .. }
        | Cmd::Device { .. }
        | Cmd::Invite { .. }
        | Cmd::Join(_)
        | Cmd::Devices { .. }
        | Cmd::Host { .. } => unreachable!(),
        Cmd::Stats => {
            let s = repo.stats()?;
            say!("commits:           {}", s.commits);
            say!("branches:          {}", s.branches);
            say!("objects:           {}", s.store.objects);
            say!("logical bytes:     {}", s.store.logical_bytes);
            say!("stored bytes:      {}", s.store.stored_bytes);
            say!("delta objects:     {}", s.store.delta_objects);
            say!("pack files:        {} ({} bytes)", s.store.packs, s.store.pack_file_bytes);
            say!("index (sqlite):    {} bytes", s.store.index_file_bytes);
            say!("total in .rgit:    {} bytes", s.repo_dir_bytes);
            crate::result!(json!({
                "commits": s.commits, "branches": s.branches, "objects": s.store.objects,
                "logical_bytes": s.store.logical_bytes, "stored_bytes": s.store.stored_bytes,
                "delta_objects": s.store.delta_objects, "pack_files": s.store.packs,
                "pack_bytes": s.store.pack_file_bytes, "index_bytes": s.store.index_file_bytes,
                "repo_bytes": s.repo_dir_bytes,
            }));
        }
    }
    Ok(0)
}

pub(crate) fn report_merge(r: &MergeResult) {
    if r.already_up_to_date {
        say!("Already up to date");
    } else if !r.conflicts.is_empty() {
        say!("Conflicts in {} file(s):", r.conflicts.len());
        for c in &r.conflicts {
            say!("  {c}");
        }
        say!("Fix them, then `rgit resolve <path>` and `rgit commit`; or `rgit merge --abort`.");
    } else if let Some(c) = r.commit {
        say!("{} {}", if r.fast_forward { "Fast-forwarded to" } else { "Created" }, c.short());
    } else {
        say!("Applied cleanly");
    }
}

fn main() -> ExitCode {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();
    let cli = Cli::parse();
    let json_mode = cli.json;
    match run(cli) {
        Ok(code) => {
            if json_mode {
                println!("{{\"ok\":true,\"data\":{}}}", out::take_result_raw());
            }
            if code == 0 { ExitCode::SUCCESS } else { ExitCode::from(code) }
        }
        Err(e) => {
            if json_mode {
                println!("{}", out::error_json(&e));
            } else {
                eprintln!("error: {}", short_err(&e));
            }
            ExitCode::from(if matches!(e, Error::RefConflict { .. }) { 3 } else { 1 })
        }
    }
}
