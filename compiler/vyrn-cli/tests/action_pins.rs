//! Every workflow pins an action to the same commit, and says which release it is.
//!
//! Third-party code in CI runs with the repository's token. Pinning by commit
//! rather than by tag is what stops a moved tag becoming a supply-chain change
//! nobody reviewed, and the workflows already do it: 23 `uses:` lines, every one
//! a 40-character SHA.
//!
//! The cost of that is nine copies of the checkout SHA. The census (item 10)
//! proposed two composite actions wrapping the two most-repeated pins. This does
//! not, and `.github/dependabot.yml` is why: the updater reads
//! `uses: owner/repo@sha` and rewrites BOTH the SHA and the `# owner/repo@v4`
//! comment above it, and it cannot see through a composite action. The wrapper
//! would have traded a duplication a test can hold for an update path nothing
//! can. When the census was written there was no updater at all, which made the
//! wrapper look free; adding one is what settled it.
//!
//! So this holds the duplication instead. It catches the failure that having
//! nine copies actually causes — bumping some and not the rest — and the one the
//! comments cause, which is a version label that stops describing its SHA.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows")
        .canonicalize()
        .expect("the workflows directory");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("read the workflows directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p: &PathBuf| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).expect("read a workflow"))
        })
        .collect();
    out.sort();
    assert!(
        out.len() >= 2,
        "expected several workflows, found {}",
        out.len()
    );
    out
}

/// Every `uses:` line, with the comment line above it and where it came from.
fn uses_lines() -> Vec<(String, usize, String, String)> {
    let mut out = Vec::new();
    for (file, src) in workflows() {
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            let Some(rest) = t
                .strip_prefix("- uses: ")
                .or_else(|| t.strip_prefix("uses: "))
            else {
                continue;
            };
            let before = if i > 0 {
                lines[i - 1].trim().to_string()
            } else {
                String::new()
            };
            out.push((file.clone(), i + 1, rest.trim().to_string(), before));
        }
    }
    assert!(
        !out.is_empty(),
        "no `uses:` lines found — the parse shape changed"
    );
    out
}

/// Nothing runs a tag. A tag moves; a commit does not.
#[test]
fn every_action_is_pinned_to_a_commit() {
    let loose: Vec<String> = uses_lines()
        .into_iter()
        .filter(|(_, _, u, _)| {
            let Some((_, rev)) = u.rsplit_once('@') else {
                return true;
            };
            rev.len() != 40 || !rev.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(|(f, n, u, _)| format!("{f}:{n} uses {u}"))
        .collect();
    assert!(
        loose.is_empty(),
        "actions not pinned to a 40-character commit:\n  {}",
        loose.join("\n  ")
    );
}

/// One action, one commit, across every workflow.
///
/// THE FAILURE THIS EXISTS FOR: `actions/checkout` is written nine times. Bumping
/// it means editing nine lines, and editing eight of them leaves a workflow
/// running a version that was retired for a reason — silently, because a
/// workflow that passes says nothing about which commit it ran.
#[test]
fn one_action_is_pinned_to_one_commit_everywhere() {
    let mut by_action: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for (file, line, u, _) in uses_lines() {
        let Some((name, rev)) = u.rsplit_once('@') else {
            continue;
        };
        by_action
            .entry(name.to_string())
            .or_default()
            .entry(rev.to_string())
            .or_default()
            .push(format!("{file}:{line}"));
    }
    let split: Vec<String> = by_action
        .iter()
        .filter(|(_, revs)| revs.len() > 1)
        .map(|(name, revs)| {
            let detail: Vec<String> = revs
                .iter()
                .map(|(rev, whence)| format!("{}… at {}", &rev[..12], whence.join(", ")))
                .collect();
            format!(
                "{name} is pinned to {} different commits: {}",
                revs.len(),
                detail.join("; ")
            )
        })
        .collect();
    assert!(
        split.is_empty(),
        "the same action runs at two commits — bump all of them or none:\n  {}",
        split.join("\n  ")
    );
}

/// A SHA says nothing to a reader, so every pin carries the release it is.
///
/// Not verifiable offline — no network, and that is the point of a pin — so what
/// is checked is that the label EXISTS and names the same action. A comment
/// saying `actions/cache@v4` above a `Swatinem/rust-cache` pin is a wrong label,
/// and a wrong label is worse than none.
#[test]
fn every_pin_says_which_release_it_is() {
    let mut bad = Vec::new();
    for (file, line, u, before) in uses_lines() {
        let Some((name, _)) = u.rsplit_once('@') else {
            continue;
        };
        let comment = before.strip_prefix("# ").unwrap_or("").trim();
        if comment.is_empty() {
            bad.push(format!(
                "{file}:{line} pins {name} with no version comment above it"
            ));
        } else if !comment.starts_with(name) {
            bad.push(format!(
                "{file}:{line} pins {name} under a comment about `{comment}`"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "pins whose version label is missing or names another action:\n  {}",
        bad.join("\n  ")
    );
}
