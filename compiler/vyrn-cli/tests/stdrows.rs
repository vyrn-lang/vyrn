//! `vyrn check` serves std's stored rows (`vyrn_lower::stdrows`). These pin
//! that a stale or damaged entry is refused and written again, and that
//! `VYRN_NO_GEN_CACHE` reads and writes none, with the same verdicts.

mod common;

use std::path::{Path, PathBuf};

use common::{scratch, vyrn};

/// A program whose acceptance reads a std row: `sleep` waits only by its
/// frames, and an `await` before a call that does not wait is refused.
const ROOT: &str = "import { sleep } from \"std/task\"\n\
                    import { joinWith } from \"std/strings\"\n\
                    fn nap() {\n    await sleep(5)\n}\n\
                    fn main() -> Int64 {\n    await nap()\n    print(joinWith([\"a\"], \",\"))\n    return 0\n}\n";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create");
    for e in std::fs::read_dir(from).expect("read std").flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        match e.file_type().expect("type").is_dir() {
            true => copy_dir(&src, &dst),
            false => {
                std::fs::copy(&src, &dst).expect("copy");
            }
        }
    }
}

/// A copy of std, a cache directory and the root, under one scratch directory.
struct Tree {
    std: PathBuf,
    cache: PathBuf,
    root: PathBuf,
    _dir: common::Scratch,
}

fn tree(tag: &str) -> Tree {
    let dir = scratch(tag);
    let std = dir.join("std");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    let root = dir.join("main.vyrn");
    std::fs::write(&root, ROOT).expect("write root");
    Tree {
        std,
        cache: dir.join("cache"),
        root,
        _dir: dir,
    }
}

impl Tree {
    /// `vyrn check` of the root: its exit code and standard error, the scratch
    /// path left out.
    fn check(&self, no_cache: bool) -> (Option<i32>, String) {
        let mut cmd = vyrn();
        cmd.arg("check")
            .arg(&self.root)
            .env("VYRN_STD", &self.std)
            .env("VYRN_GEN_CACHE_DIR", &self.cache);
        if no_cache {
            cmd.env("VYRN_NO_GEN_CACHE", "1");
        }
        let out = cmd.output().expect("vyrn check");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        )
    }

    /// Each stored entry's name and text.
    fn entries(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(&self.cache)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("rows-"))
            .map(|n| {
                let text = std::fs::read_to_string(self.cache.join(&n)).expect("read entry");
                (n, text)
            })
            .collect();
        out.sort();
        out
    }
}

#[test]
fn an_entry_whose_import_changed_is_written_again() {
    let t = tree("stdrows-stale");
    let first = t.check(false);
    assert_eq!(first.0, Some(0), "{}", first.1);
    let before = t.entries();
    assert!(!before.is_empty(), "the check wrote no entry");
    // `std/strings` imports `std/strpred`, so its entry records that file.
    let strpred = t.std.join("strpred.vyrn");
    let text = std::fs::read_to_string(&strpred).expect("read strpred");
    std::fs::write(&strpred, format!("{text}\n// an edit\n")).expect("edit strpred");
    assert_eq!(t.check(false), first, "the verdict moved with the entry");
    let after = t.entries();
    let rewritten = (before.iter())
        .filter(|(n, text)| after.iter().any(|(m, t2)| m == n && t2 != text))
        .count();
    assert!(
        rewritten > 0,
        "no entry that records std/strpred was written again"
    );
}

#[test]
fn a_damaged_entry_reads_as_a_miss_and_is_written_again() {
    let t = tree("stdrows-damaged");
    let first = t.check(false);
    let (name, _) = t.entries().into_iter().next().expect("an entry");
    std::fs::write(t.cache.join(&name), "stdrows1 0000\n1\n").expect("damage");
    assert_eq!(t.check(false), first);
    let text = std::fs::read_to_string(t.cache.join(&name)).expect("read");
    assert!(
        text.len() > 32,
        "the damaged entry was not written again:\n{text}"
    );
}

#[test]
fn no_gen_cache_reads_and_writes_no_entry_and_the_verdict_holds() {
    let t = tree("stdrows-off");
    let off = t.check(true);
    assert!(t.entries().is_empty(), "VYRN_NO_GEN_CACHE wrote an entry");
    let on = t.check(false);
    assert_eq!(off, on);
    let (name, _) = t.entries().into_iter().next().expect("an entry");
    std::fs::write(t.cache.join(&name), "damaged").expect("damage");
    assert_eq!(t.check(true), on);
    let text = std::fs::read_to_string(t.cache.join(&name)).expect("read");
    assert_eq!(
        text, "damaged",
        "VYRN_NO_GEN_CACHE read and wrote the entry"
    );
}
