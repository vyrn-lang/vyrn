//! Which `std/` modules export the same top-level name, as a reviewed list.
//!
//! A top-level name is program-wide, so two std modules exporting it cannot
//! both be imported flatly; a program must reach one through a namespace
//! import. A reviewed collision stays with its reason; a new one fails here. A
//! std name colliding with a user's name is out of scope: the loader reports
//! it at the point of use.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../std")
        .canonicalize()
        .expect("the std directory")
}

/// Every `export`ed top-level name in `src`, with the line it is on.
///
/// A textual scan, so it keeps working when a module fails to parse.
fn exported_names(src: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let Some(rest) = line.strip_prefix("export ") else {
            continue;
        };
        for kw in ["fn ", "type ", "protocol ", "contract ", "gen fn "] {
            if let Some(after) = rest.strip_prefix(kw) {
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.push((name, i + 1));
                }
                break;
            }
        }
    }
    out
}

/// Collisions that have been reviewed and kept, with why.
const REVIEWED: &[(&str, &str)] = &[
    (
        "map",
        "std/arrays and std/stream — the same operation on two containers",
    ),
    (
        "filter",
        "std/arrays and std/stream — the same operation on two containers",
    ),
    ("cli", "std/args' accessor and std/cli's generator"),
];

#[test]
fn no_two_std_modules_export_the_same_name() {
    let dir = std_dir();
    let mut homes: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read std/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    files.sort();
    assert!(files.len() > 20, "only {} std modules found", files.len());

    for path in &files {
        let module = path.file_stem().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(path).expect("read a std module");
        for (name, line) in exported_names(&src) {
            homes
                .entry(name)
                .or_default()
                .push(format!("std/{module}.vyrn:{line}"));
        }
    }

    let reviewed: std::collections::BTreeSet<&str> = REVIEWED.iter().map(|(n, _)| *n).collect();

    let fresh: Vec<String> = homes
        .iter()
        .filter(|(_, where_)| where_.len() > 1)
        .filter(|(name, _)| !reviewed.contains(name.as_str()))
        .map(|(name, where_)| format!("`{name}` — {}", where_.join(", ")))
        .collect();

    assert!(
        fresh.is_empty(),
        "a NEW top-level name is exported by two std modules, so no program can \
         import both flatly:\n  {}\nRename one, or add it to REVIEWED with the \
         reason sharing the name is right. A name here is program-wide.",
        fresh.join("\n  ")
    );

    let stale: Vec<&str> = REVIEWED
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| homes.get(*n).is_none_or(|w| w.len() < 2))
        .collect();
    assert!(
        stale.is_empty(),
        "REVIEWED names that no longer collide — delete the row(s): {}",
        stale.join(", ")
    );
}
