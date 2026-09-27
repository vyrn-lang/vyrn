//! Every workflow pins an action to the same commit, and says which release it is.
//!
//! Third-party code in CI runs with the repository's token, so a pin is a commit,
//! never a tag: a moved tag would be an unreviewed supply-chain change. The pins
//! repeat across workflows instead of living in a composite action because
//! `.github/dependabot.yml` rewrites both the SHA and the `# owner/repo@vN`
//! comment above it, and cannot see through a composite action. These tests hold
//! the duplication: some copies bumped and not the rest, or a version label that
//! stops describing its SHA.

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

/// Every `uses:` line as (file, 1-based line, action@rev, the trimmed line above).
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

/// Guards a partial bump: a workflow left on a retired commit still passes and
/// says nothing about which commit it ran.
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

/// The label's version cannot be checked offline, so the test checks only that
/// the label exists and names the same action.
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
