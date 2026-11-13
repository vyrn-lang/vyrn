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

    /// `vyrn build` of the root to wasm: its exit code and standard error.
    fn build(&self) -> (Option<i32>, String) {
        let out = vyrn()
            .arg("build")
            .arg(&self.root)
            .arg("-o")
            .arg(self.root.with_extension("wasm"))
            .args(["--target", "wasm"])
            .env("VYRN_STD", &self.std)
            .env("VYRN_GEN_CACHE_DIR", &self.cache)
            .output()
            .expect("vyrn build");
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

/// A std module whose bodies hold a borrow of module state across a call
/// through a function value, and one that only reads immutable state.
const STATE_MODULE: &str = "let mut names: Array<String> = [\"a\", \"b\"]\n\n\
                            let fixed: Array<String> = [\"x\", \"y\"]\n\n\
                            export fn bump() {\n    names.push(\"c\")\n}\n\n\
                            export fn walk(cb: fn() -> Unit) -> Int64 {\n    let s = names[0]\n    cb()\n    return s.byteLength\n}\n\n\
                            export fn viaModify(xs: modify Array<String>, cb: fn() -> Unit) -> Int64 {\n    let s = xs[0]\n    cb()\n    return s.byteLength\n}\n\n\
                            export fn viaOwn(cb: fn() -> Unit) -> Int64 {\n    return viaModify(names, cb)\n}\n\n\
                            export fn viaFixed(cb: fn() -> Unit) -> Int64 {\n    let s = fixed[0]\n    cb()\n    return s.byteLength\n}\n";

/// A root that calls `std/sgstate`'s `entry` with a callback whose body is
/// `callback`.
fn state_root(entry: &str, callback: &str) -> String {
    format!(
        "import {{ {entry}, bump }} from \"std/sgstate\"\n\n\
         fn quiet() {{\n    {callback}\n}}\n\n\
         fn main() -> Int64 {{\n    return {entry}(quiet)\n}}\n"
    )
}

/// A tree whose std holds `std/sgstate`, rooted at `root_text`.
fn state_tree(tag: &str, root_text: &str) -> Tree {
    let t = tree_of(tag, root_text);
    std::fs::write(t.std.join("sgstate.vyrn"), STATE_MODULE).expect("write sgstate");
    t
}

/// `vyrn check` warm, `vyrn check` under `VYRN_NO_GEN_CACHE` and `vyrn build`
/// of `t`'s root, each as its exit code and first line of standard error.
fn verdicts(t: &Tree) -> [(Option<i32>, String); 3] {
    let first =
        |(code, err): (Option<i32>, String)| (code, err.lines().next().unwrap_or("").into());
    t.check(false);
    [t.check(false), t.check(true), t.build()].map(first)
}

/// A body that borrows its module's state across a call through a function
/// value is built in each program, so the callback the program passes decides
/// its verdict, under check as under build (a given body kept the verdict of
/// its module's own load, where no callback writes the state).
#[test]
fn a_std_body_naming_its_module_state_is_judged_with_the_programs_callbacks() {
    for entry in ["walk", "viaOwn"] {
        let quiet = state_tree("stdrows-state-quiet", &state_root(entry, "print(\"q\")"));
        for (code, err) in verdicts(&quiet) {
            assert_eq!(code, Some(0), "{entry}: {err}");
        }
        let bumps = state_tree("stdrows-state-bumps", &state_root(entry, "bump()"));
        for (code, err) in verdicts(&bumps) {
            assert_eq!(code, Some(1), "{entry} was accepted");
            let refused = err.contains("`names` is written here while");
            assert!(refused, "{entry}: {err}");
        }
    }
}

/// Immutable module state cannot be written, so no answer ends a borrow of
/// it: the body stays given, and every callback is accepted.
#[test]
fn a_std_body_reading_immutable_state_stays_given() {
    let t = state_tree("stdrows-state-fixed", &state_root("viaFixed", "bump()"));
    for (code, err) in verdicts(&t) {
        assert_eq!(code, Some(0), "{err}");
    }
    let spells: Vec<String> = (t.entries("interface-").into_iter())
        .filter(|(_, text)| text.contains("viaFixed"))
        .map(|(_, text)| text)
        .collect();
    assert_eq!(spells.len(), 1, "one interface entry names viaFixed");
    assert!(spells[0].contains("\nviaFixed "), "viaFixed is not given");
    assert!(!spells[0].contains("\nwalk "), "walk is given");
}
