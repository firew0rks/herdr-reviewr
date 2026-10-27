//! Component attribution for the perceived-latency work: times the individual blocking
//! calls (git spawns, diff builds, highlights) that make up a switch, against a real repo.
//! `scripts/bench_tui.py` is the acceptance instrument — it measures keypress to painted
//! frame in the real binary. Reach for this tool to find out *where* a slow number in that
//! harness comes from.
//!
//! Usage: `cargo run --release --example bench_latency -- <repo-path> [label]`

use std::path::PathBuf;
use std::time::Instant;

use herdr_reviewr::diff::DiffCache;
use herdr_reviewr::git;
use herdr_reviewr::highlight::Highlighter;
use herdr_reviewr::model::Scope;
use herdr_reviewr::theme;

fn ms(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// Run `f` `n` times, return (first, min, median) in ms.
fn sample(n: usize, mut f: impl FnMut()) -> (f64, f64, f64) {
    let mut times: Vec<f64> = (0..n).map(|_| ms(&mut f)).collect();
    let first = times[0];
    times.sort_by(f64::total_cmp);
    (first, times[0], times[times.len() / 2])
}

fn row(name: &str, (first, min, med): (f64, f64, f64)) {
    println!("{name:<46} first {first:>8.1}ms   min {min:>8.1}ms   median {med:>8.1}ms");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(args.next().expect("usage: bench_latency <repo> [label]"));
    let label = args.next().unwrap_or_else(|| repo.display().to_string());
    assert!(git::is_repo(&repo), "not a git repo: {}", repo.display());
    let hl = Highlighter::new(theme::resolve(None).syntax);
    println!("== {label} ==");

    // --- Components of reload() -------------------------------------------------
    let changed = git::changed_files(&repo, Scope::Uncommitted, None).unwrap();
    row(
        "changed_files (uncommitted)",
        sample(5, || {
            git::changed_files(&repo, Scope::Uncommitted, None).unwrap();
        }),
    );
    row(
        "changed_files (branch, incl. resolve)",
        sample(5, || {
            let base = git::resolve_base(&repo, None).ok().and_then(|r| r.status.winner);
            git::changed_files(&repo, Scope::Branch, base.map(|w| w.oid).as_deref()).unwrap();
        }),
    );
    row(
        "snapshot_worktree (poll during turn)",
        sample(3, || {
            git::snapshot_worktree(&repo).unwrap();
        }),
    );

    // Changes tab: set_diff = git show HEAD:path + fs read + two-side highlight + diff.
    if let Some(cf) = changed.first() {
        let path = cf.path.clone();
        row(
            &format!("diff open, Changes COLD ({path})"),
            sample(3, || {
                let mut cache = DiffCache::new();
                let old = git::file_content(&repo, "HEAD", &path);
                let new = std::fs::read_to_string(repo.join(&path)).unwrap_or_default();
                cache.get(path.clone(), None, &old, &new, &hl);
            }),
        );
        let mut warm = DiffCache::new();
        let old0 = git::file_content(&repo, "HEAD", &path);
        let new0 = std::fs::read_to_string(repo.join(&path)).unwrap_or_default();
        warm.get(path.clone(), None, &old0, &new0, &hl);
        row(
            "diff open, Changes WARM (same file re-poll)",
            sample(5, || {
                let old = git::file_content(&repo, "HEAD", &path);
                let new = std::fs::read_to_string(repo.join(&path)).unwrap_or_default();
                warm.get(path.clone(), None, &old, &new, &hl);
            }),
        );
    } else {
        println!("no uncommitted changes; skipping diff opens");
    }
    println!();
}
