//! Which `std/` modules export the same top-level name, as a reviewed list.
//!
//! A top-level name is PROGRAM-WIDE. Two std modules exporting the same one
//! cannot both be imported flatly, and a program that tries is told to reach one
//! of them through a namespace import instead — which is a real answer, and
//! sometimes the right one.
//!
//! So this is NOT a prohibition. It is the same shape as the parity suite's
//! known-divergent list: a collision that has been looked at and judged fine
//! stays, with its reason beside it; a NEW one fails here and gets looked at.
//!
//! THE FAILURE THAT PROMPTED IT: `std/hints` exported a record called `Policy`
//! and `std/http` exports a protocol called `Policy`. Nothing said so. It
//! surfaced as `site/markup.vyrn`, a whole second program that existed only
//! because the site's export links `std/http` while its markup rules reach
//! `std/hints`. The workaround was written down and correct, and it was still a
//! second entry point for a name collision nobody had reviewed. `std/hints`
//! calls its record `HintPolicy` now.
//!
//! It found three more the moment it ran, and one of them was being added in
//! the same branch: `std/regex` called its scanner `count`, which `std/slots`
//! already had. It is `countMatches` now — a better name, and one fewer
//! namespace import for a caller who wants both.
//!
//! What this does NOT check: a std name colliding with a USER's. That is
//! ordinary, and the loader reports it at the point of use — a program is
//! allowed to call its own function `hint`.

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
/// A textual scan, deliberately: this asks what a READER of `std/` would see,
/// and it must keep working when a module fails to parse for some other reason.
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

/// Collisions that have been looked at and left, with why.
///
/// Both entries are the SAME operation on two containers, which is a case for
/// sharing a name rather than against it: `map` over an array and `map` over a
/// stream mean the same thing to a reader. The loader's own diagnostic points at
/// the answer — `import * as stream` reaches them as `stream.map` — and
/// `std/stream`'s module doc says the pull model is the difference.
///
/// `cli` is the third: `std/args`' is a function answering the parsed
/// arguments, `std/cli`'s is the generator that builds a parser. Different
/// enough to confuse, close enough that renaming either would be worse.
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

    // And the list does not rot: an entry whose collision was resolved is a line
    // claiming a constraint that is gone.
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
