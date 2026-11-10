//! `vyrn check` reads std's stored rows (`vyrn_lower::stdrows`) and
//! interfaces (`loader::interface_parse`). These pin that a stale or damaged
//! entry is refused and written again, and that `VYRN_NO_GEN_CACHE` reads and
//! writes none, with the same verdicts.

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
    tree_of(tag, ROOT)
}

fn tree_of(tag: &str, root_text: &str) -> Tree {
    let dir = scratch(tag);
    let std = dir.join("std");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    let root = dir.join("main.vyrn");
    std::fs::write(&root, root_text).expect("write root");
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

    /// Each stored entry's name and text whose name starts with `kind`.
    fn entries(&self, kind: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(&self.cache)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(kind))
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
    let before = t.entries("rows-");
    assert!(!before.is_empty(), "the check wrote no entry");
    // `std/strings` imports `std/strpred`, so its entry records that file.
    let strpred = t.std.join("strpred.vyrn");
    let text = std::fs::read_to_string(&strpred).expect("read strpred");
    std::fs::write(&strpred, format!("{text}\n// an edit\n")).expect("edit strpred");
    assert_eq!(t.check(false), first, "the verdict moved with the entry");
    let after = t.entries("rows-");
    let rewritten = (before.iter())
        .filter(|(n, text)| after.iter().any(|(m, t2)| m == n && t2 != text))
        .count();
    assert!(
        rewritten > 0,
        "no entry that records std/strpred was written again"
    );
}

#[test]
fn a_changed_std_byte_misses_its_entry_and_the_verdict_holds() {
    let t = tree("stdrows-byte");
    let first = t.check(false);
    assert_eq!(first.0, Some(0), "{}", first.1);
    let before = t.entries("rows-");
    // The root's `sleep` is given: its row comes from `std/task`'s entry.
    let task = t.std.join("task.vyrn");
    let text = std::fs::read_to_string(&task).expect("read task");
    std::fs::write(&task, format!("{text}\n// an edit\n")).expect("edit task");
    assert_eq!(t.check(false), first, "the verdict moved with the edit");
    let after = t.entries("rows-");
    assert!(
        after.iter().any(|e| !before.contains(e)),
        "no entry was written for the edited std/task"
    );
}

/// The entries damaged are those that name `sleep`, `std/task`'s among them,
/// which the root's `sleep` reads: a warm check reads only the entries of
/// modules the root reaches, and an own load may have written others.
#[test]
fn a_damaged_entry_reads_as_a_miss_and_is_written_again() {
    let t = tree("stdrows-damaged");
    let first = t.check(false);
    for (kind, task) in [("rows-", "\u{5}sleep"), ("interface-", " sleep\n")] {
        let names: Vec<String> = (t.entries(kind).into_iter())
            .filter(|(_, text)| text.contains(task))
            .map(|(name, _)| name)
            .collect();
        assert!(!names.is_empty(), "no {kind} entry names sleep");
        for name in &names {
            std::fs::write(t.cache.join(name), "interface1 0000\n1\n").expect("damage");
        }
        assert_eq!(t.check(false), first);
        let again = (names.iter())
            .filter(|name| {
                std::fs::read_to_string(t.cache.join(name))
                    .expect("read")
                    .len()
                    > 32
            })
            .count();
        assert!(again > 0, "no damaged {kind} entry was written again");
    }
}

#[test]
fn no_gen_cache_reads_and_writes_no_entry_and_the_verdict_holds() {
    let t = tree("stdrows-off");
    let off = t.check(true);
    assert!(t.entries("").is_empty(), "VYRN_NO_GEN_CACHE wrote an entry");
    let on = t.check(false);
    assert_eq!(off, on);
    let (name, _) = t.entries("rows-").into_iter().next().expect("an entry");
    std::fs::write(t.cache.join(&name), "damaged").expect("damage");
    assert_eq!(t.check(true), on);
    let text = std::fs::read_to_string(t.cache.join(&name)).expect("read");
    assert_eq!(
        text, "damaged",
        "VYRN_NO_GEN_CACHE read and wrote the entry"
    );
}

/// An entry's body with each input's settled stamp left out: a stamp moves
/// with the clock, so two fills of one tree agree on every other byte.
fn unstamped(text: &str) -> String {
    let mut lines = text.lines().skip(1);
    let inputs: usize = (lines.next())
        .and_then(|n| n.parse().ok())
        .expect("an input count");
    let mut out = vec![inputs.to_string()];
    for l in lines.by_ref().take(inputs) {
        out.push(l.split('\t').take(2).collect::<Vec<_>>().join("\t"));
    }
    out.extend(lines.map(str::to_string));
    out.join("\n")
}

/// One cold fill runs `std/strings`' own load first, the other `std/json`'s,
/// so each writes `std/text` under another stack of writers. The entries they
/// share agree byte for byte: an entry is a function of its key.
#[test]
fn a_cold_fill_writes_each_entry_alike_in_any_order() {
    let strings_first = "import { joinWith } from \"std/strings\"\n\
                         fn main() -> Int64 {\n    print(joinWith([\"a\"], \",\"))\n    return 0\n}\n";
    let json_first = "import { Json, emit } from \"std/json\"\n\
                      import { joinWith } from \"std/strings\"\n\
                      fn main() -> Int64 {\n    print(emit(JNull))\n    print(joinWith([\"a\"], \",\"))\n    return 0\n}\n";
    let a = tree_of("stdrows-order-a", strings_first);
    let mut b = tree_of("stdrows-order-b", json_first);
    b.std = a.std.clone();
    for t in [&a, &b] {
        let out = t.check(false);
        assert_eq!(out.0, Some(0), "{}", out.1);
    }
    let (a, b) = (a.entries("rows-"), b.entries("rows-"));
    let shared: Vec<_> = (a.iter())
        .filter_map(|(n, text)| Some((n, text, &b.iter().find(|(m, _)| m == n)?.1)))
        .collect();
    assert!(
        shared.len() >= 4,
        "the fills share {} entries",
        shared.len()
    );
    for (n, x, y) in shared {
        assert_eq!(unstamped(x), unstamped(y), "entry {n} differs");
    }
}
