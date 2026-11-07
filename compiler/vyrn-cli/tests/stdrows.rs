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

/// The entries damaged are `std/task`'s, which the root's `sleep` reads: a
/// warm check reads only the entries of modules the root reaches, and an own
/// load may have written others.
#[test]
fn a_damaged_entry_reads_as_a_miss_and_is_written_again() {
    let t = tree("stdrows-damaged");
    let first = t.check(false);
    for (kind, task) in [("rows-", "\nsleep\t"), ("interface-", "fn sleep(")] {
        let (name, _) = (t.entries(kind).into_iter())
            .find(|(_, text)| text.contains(task))
            .expect("std/task's entry");
        std::fs::write(t.cache.join(&name), "interface1 0000\n1\n").expect("damage");
        assert_eq!(t.check(false), first);
        let text = std::fs::read_to_string(t.cache.join(&name)).expect("read");
        assert!(
            text.len() > 32,
            "the damaged entry was not written again:\n{text}"
        );
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

/// The calls the stored row of `name` makes: the callee of each call line of
/// each of its frames, read from whichever `rows-` entry holds it.
fn row_calls(t: &Tree, name: &str) -> Vec<String> {
    // A row's header: its name, its module, then its counts of frames, facts
    // and calls, and the fence's impure reason.
    let header = |l: &str| -> Option<usize> {
        let f: Vec<&str> = l.split('\t').collect();
        (f.len() == 6 && f[0] == name).then(|| f[2].parse().ok())?
    };
    for (_, text) in t.entries("rows-") {
        let lines: Vec<&str> = text.lines().collect();
        let Some((at, frames)) =
            (lines.iter().enumerate()).find_map(|(i, l)| Some((i, header(l)?)))
        else {
            continue;
        };
        let mut out = Vec::new();
        let mut i = at + 1;
        for _ in 0..frames {
            let calls: usize = (lines[i].split('\t').nth(4))
                .and_then(|n| n.parse().ok())
                .expect("a frame line");
            for c in &lines[i + 1..i + 1 + calls] {
                out.push(c.split('\t').next().unwrap_or_default().to_string());
            }
            // The frame line, its calls and its answer line.
            i += calls + 2;
        }
        return out;
    }
    panic!("no entry stores a row for `{name}`");
}

/// The own load expands a projection as an importer's does: `std/stream`'s
/// `cells[h]` reads through `std/slots`' `at`, which checks the handle is alive.
#[test]
fn a_stored_row_judges_a_projection_as_the_importer_runs_it() {
    let root = "import { cursorGet } from \"std/stream\"\n\
                fn main() -> Int64 {\n    return 0\n}\n";
    let t = tree_of("stdrows-projection", root);
    let out = t.check(false);
    assert_eq!(out.0, Some(0), "{}", out.1);
    let calls = row_calls(&t, "cursorGet");
    assert!(
        calls.iter().any(|c| c == "alive"),
        "cursorGet calls {calls:?}"
    );
}

/// The own load names its sites as an importer's does: the runtime's `panic`
/// is its fixed trap, never stamped with a site.
#[test]
fn a_stored_row_names_its_sites_as_the_importer_does() {
    let t = tree("stdrows-sites");
    let out = t.check(false);
    assert_eq!(out.0, Some(0), "{}", out.1);
    let calls = row_calls(&t, "intStr");
    assert!(calls.iter().any(|c| c == "panic"), "intStr calls {calls:?}");
    assert!(
        !calls.iter().any(|c| c.contains("panicAt")),
        "intStr calls {calls:?}"
    );
}
