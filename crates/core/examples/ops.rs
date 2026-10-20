//! Minimal driver around the engine API for memory measurements that do not need the CLI:
//!   cargo build --release -p rustgit-core --example ops
//!   /usr/bin/time -l target/release/examples/ops <project> status|init|commit|gc|gc-aggressive|verify|log
use std::path::Path;

use rustgit_core::Repo;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (dir, op) = (Path::new(&args[1]), args[2].as_str());
    if op == "init" {
        Repo::init(dir, "main").unwrap();
        return;
    }
    let mut repo = Repo::open(dir).unwrap();
    match op {
        "status" => println!("{} changes", repo.status().unwrap().changes.len()),
        "commit" => println!("{}", repo.commit("measure", &[]).unwrap().short()),
        "gc" => println!("{:?}", repo.gc(0, 0, false).unwrap().repack),
        "gc-aggressive" => println!("{:?}", repo.gc(0, 0, true).unwrap().repack),
        "verify" => println!("{} problems", repo.verify().unwrap().problems.len()),
        "log" => {
            let h = repo.head_commit().unwrap().unwrap();
            println!("{}", repo.history(&h, 5).unwrap().len());
        }
        _ => panic!("unknown op"),
    }
}
