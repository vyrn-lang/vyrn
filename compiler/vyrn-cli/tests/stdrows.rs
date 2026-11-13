//! `vyrn check` reads std's stored rows (`vyrn_lower::stdrows`), closed
//! entries (`loader::read_closed`) and interfaces (`loader::interface_parse`).
//! These pin that a stale or damaged entry is refused and written again, that
//! `VYRN_NO_GEN_CACHE` reads and writes none, with the same verdicts, and
//! which std bodies naming module state a warm check gives.

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

    /// `vyrn emit-lowered` of the root, a host that builds every body: its
    /// exit code and standard error.
    fn report(&self) -> (Option<i32>, String) {
        let out = vyrn()
            .arg("emit-lowered")
            .arg(&self.root)
            .env("VYRN_STD", &self.std)
            .env("VYRN_GEN_CACHE_DIR", &self.cache)
            .output()
            .expect("vyrn emit-lowered");
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

/// A std module with one body per kind of call a borrow of its module state
/// can be live across, and one that only reads immutable state.
const STATE_MODULE: &str = r#"let mut names: Array<String> = ["a", "b"]

let mut hooks: Array<fn() -> Unit> = []

let fixed: Array<String> = ["x", "y"]

export type Box = { n: Int64 }

export protocol Visit {
    fn visit(self) -> Int64
}

impl Visit for Box {
    fn visit(self) -> Int64 {
        return self.n
    }
}

export fn bump() {
    names.push("c")
}

export fn run(cb: fn() -> Unit) {
    cb()
}

export fn runAt(f: fn(Int64) -> Unit) {
    f(1)
}

fn nop() {
}

fn nopAt(x: Int64) {
}

fn helper(cb: fn() -> Unit) {
    cb()
}

fn idle<T>(x: T) -> Int64 {
    return 1
}

fn applyG<T>(x: T, f: fn(T) -> Unit) {
    f(x)
}

export fn walk(cb: fn() -> Unit) -> Int64 {
    let s = names[0]
    cb()
    return s.byteLength
}

export fn viaModify(xs: modify Array<String>, cb: fn() -> Unit) -> Int64 {
    let s = xs[0]
    cb()
    return s.byteLength
}

export fn viaOwn(cb: fn() -> Unit) -> Int64 {
    return viaModify(names, cb)
}

export fn viaHelper(cb: fn() -> Unit) -> Int64 {
    let s = names[0]
    helper(cb)
    return s.byteLength
}

export fn viaLocal(cb: fn() -> Unit) -> Int64 {
    run(cb)
    let g: fn() -> Unit = nop
    let s = names[0]
    g()
    return s.byteLength
}

export fn viaHooks(cb: consume fn() -> Unit) -> Int64 {
    hooks.push(cb)
    let s = names[0]
    let mut i = 0
    while i < hooks.length {
        let h = hooks[i]
        h()
        i = i + 1
    }
    return s.byteLength
}

export fn viaGenericFn(cb: fn() -> Unit) -> Int64 {
    let s = names[0]
    applyG(3, nopAt)
    return s.byteLength
}

export fn viaProto(cb: fn() -> Unit) -> Int64 {
    let b = Box { n: 2 }
    let s = names[0]
    let k = b.visit()
    return s.byteLength + k
}

export fn viaGeneric(cb: fn() -> Unit) -> Int64 {
    let s = names[0]
    let k = idle(3)
    return s.byteLength + k
}

export fn walkEnded(cb: fn() -> Unit) -> Int64 {
    let n = names[0].byteLength
    cb()
    return n
}

export fn scalarAcross(cb: fn() -> Unit) -> Int64 {
    let n = names.length
    cb()
    return n
}

export fn viaFixed(cb: fn() -> Unit) -> Int64 {
    let s = fixed[0]
    cb()
    return s.byteLength
}
"#;

/// A root that calls `std/sgstate`'s `entry` with a callback whose body is
/// `callback`. `runAt(hurt)` makes `hurt` a source of `fn(Int64) -> Unit`.
fn state_root(entry: &str, callback: &str) -> String {
    format!(
        "import {{ {entry}, bump, runAt }} from \"std/sgstate\"\n\n\
         fn quiet() {{\n    {callback}\n}}\n\n\
         fn hurt(x: Int64) {{\n    {callback}\n}}\n\n\
         fn main() -> Int64 {{\n    runAt(hurt)\n    return {entry}(quiet)\n}}\n"
    )
}

/// A tree whose std holds `std/sgstate`, rooted at `root_text`.
fn state_tree(tag: &str, root_text: &str) -> Tree {
    let t = tree_of(tag, root_text);
    std::fs::write(t.std.join("sgstate.vyrn"), STATE_MODULE).expect("write sgstate");
    t
}

/// `t`'s root under `vyrn check` cold, warm and under `VYRN_NO_GEN_CACHE`,
/// `vyrn build`, and `vyrn emit-lowered`, a host that builds every body: each
/// as its exit code and standard error. The warm check gives what the cold one
/// wrote a closed entry for.
fn verdicts(t: &Tree) -> [(Option<i32>, String); 5] {
    let _ = std::fs::remove_dir_all(&t.cache);
    let cold = t.check(false);
    [cold, t.check(false), t.check(true), t.build(), t.report()]
}

/// The functions of `std/sgstate` its closed entry gives ([`loader::read_closed`]).
fn closed(t: &Tree) -> Vec<String> {
    let entry = (t.entries("closed-").into_iter()).find(|(_, text)| text.contains("sgstate.vyrn "));
    let (_, text) = entry.expect("a closed entry for std/sgstate");
    (text.lines())
        .filter_map(|l| l.rsplit_once(' '))
        .filter(|(m, _)| m.ends_with("sgstate.vyrn"))
        .map(|(_, n)| n.to_string())
        .collect()
}

/// Asserts that every host prints one verdict for `t`'s root, `code`.
fn agree(t: &Tree, entry: &str, code: i32) -> String {
    let [first, rest @ ..] = verdicts(t);
    assert_eq!(first.0, Some(code), "{entry}: {}", first.1);
    for v in rest {
        assert_eq!(v, first, "{entry}: a host printed another verdict");
    }
    first.1
}

/// A body that borrows its module's state across a call that may run a body
/// the program supplies is built in each program, so the program decides its
/// verdict under every host. A given body would keep its own load's verdict,
/// where no callback writes the state.
#[test]
fn a_std_body_whose_calls_open_is_judged_with_the_programs_callbacks() {
    for entry in [
        "walk",
        "viaOwn",
        "viaHelper",
        "viaLocal",
        "viaHooks",
        "viaGenericFn",
    ] {
        let quiet = state_tree("stdrows-open-quiet", &state_root(entry, "print(\"q\")"));
        agree(&quiet, entry, 0);
        let bumps = state_tree("stdrows-open-bumps", &state_root(entry, "bump()"));
        let err = agree(&bumps, entry, 1);
        assert!(
            err.contains("`names` is written here while"),
            "{entry}: {err}"
        );
        assert!(
            !closed(&bumps).contains(&entry.to_string()),
            "{entry} is given"
        );
    }
}

/// A body whose calls all have one answer in every program keeps its verdict
/// in every program, so a warm check gives it.
#[test]
fn a_std_body_whose_calls_close_is_given() {
    for entry in ["viaProto", "viaGeneric"] {
        let t = state_tree("stdrows-closed", &state_root(entry, "bump()"));
        agree(&t, entry, 0);
        assert!(
            closed(&t).contains(&entry.to_string()),
            "{entry} is not given"
        );
    }
}

/// The test reads calls, not borrows: a body with an open call is built in
/// each program even where no borrow is live across the call.
#[test]
fn a_std_body_with_an_open_call_is_built_though_no_borrow_spans_it() {
    for entry in ["walkEnded", "scalarAcross"] {
        let t = state_tree("stdrows-ended", &state_root(entry, "bump()"));
        agree(&t, entry, 0);
        assert!(!closed(&t).contains(&entry.to_string()), "{entry} is given");
    }
}

/// Immutable module state cannot be written, so no answer ends a borrow of
/// it: the body is given whatever it calls.
#[test]
fn a_std_body_reading_immutable_state_stays_given() {
    let t = state_tree("stdrows-state-fixed", &state_root("viaFixed", "bump()"));
    agree(&t, "viaFixed", 0);
    let spells: Vec<String> = (t.entries("interface-").into_iter())
        .filter(|(_, text)| text.contains("viaFixed"))
        .map(|(_, text)| text)
        .collect();
    assert_eq!(spells.len(), 1, "one interface entry names viaFixed");
    assert!(spells[0].contains("\nviaFixed "), "viaFixed is not given");
}

/// A program refused for a protocol method named like a function a std body
/// calls prints the collision alone, cold, warm, without the cache and under
/// build: no body is typed after it, given or not.
#[test]
fn a_name_collision_prints_one_text_under_every_host() {
    let root = "import { cursorGet, Cursor } from \"std/stream\"\n\n\
                protocol Hd {\n    fn handleOf(read self) -> Int64\n}\n\n\
                impl Hd for Cursor {\n    fn handleOf(read self) -> Int64 {\n        return 0\n    }\n}\n\n\
                fn main() -> Int64 {\n    let c = Cursor { slot: 0, gen: 0 }\n    return cursorGet(c)\n}\n";
    let t = tree_of("stdrows-collision", root);
    let err = agree(&t, "cursorGet", 1);
    assert_eq!(err.lines().count(), 1, "{err}");
}

/// `std/sgstate`'s closed entry: its name and text.
fn closed_entry(t: &Tree) -> (String, String) {
    let entry = (t.entries("closed-").into_iter()).find(|(_, text)| text.contains("sgstate.vyrn "));
    entry.expect("a closed entry for std/sgstate")
}

/// `text`, a closed entry's, with `walk` added to the functions it gives, and
/// its tag line made under `key`; `None` keeps the entry's own tag.
fn forged(text: &str, key: Option<&str>) -> String {
    let (head, body) = text.split_once('\n').expect("a tag line");
    let module = (body.lines())
        .filter_map(|l| l.rsplit_once(' '))
        .find(|(m, _)| m.ends_with("sgstate.vyrn"))
        .map(|(m, _)| m)
        .expect("a function of std/sgstate");
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    let count = (lines.iter().rposition(|l| l.parse::<usize>().is_ok())).expect("a count");
    let n: usize = lines[count].parse().expect("a count");
    lines[count] = (n + 1).to_string();
    lines.push(format!("{module} walk"));
    let body = lines.join("\n") + "\n";
    let head = match key {
        Some(k) => format!(
            "closed1 {}",
            vyrn_frontend::loader::gen_cache_tag(k, body.as_bytes())
        ),
        None => head.to_string(),
    };
    format!("{head}\n{body}")
}

/// A closed entry that does not read whole, under its own key, as the
/// module's own load wrote it, gives nothing: the program's callback still
/// decides `walk`'s verdict.
#[test]
fn a_damaged_closed_entry_gives_nothing() {
    let t = state_tree("stdrows-closed-damaged", &state_root("walk", "bump()"));
    let refused = agree(&t, "walk", 1);
    let (name, text) = closed_entry(&t);
    let path = t.cache.join(&name);
    let damages = [
        ("truncated", text[..text.len() / 2].to_string()),
        ("a wrong tag", forged(&text, None)),
        (
            "another key's tag",
            forged(&text, Some("closed-of-another-compiler")),
        ),
    ];
    for (why, damaged) in damages {
        std::fs::write(&path, damaged).expect("damage");
        assert_eq!(t.check(false), (Some(1), refused.clone()), "{why}");
        let again = std::fs::read_to_string(&path).expect("read");
        assert_eq!(again, text, "{why}: the entry was not written again");
    }
}

/// An entry written for an older text of the module, or of a file its own
/// load read, gives nothing: the text hash keys it and its inputs stamp it.
#[test]
fn a_closed_entry_for_older_std_text_gives_nothing() {
    let t = state_tree("stdrows-closed-stale", &state_root("walk", "bump()"));
    let refused = agree(&t, "walk", 1);
    let (name, text) = closed_entry(&t);
    let module = t.std.join("sgstate.vyrn");
    std::fs::write(&module, format!("{STATE_MODULE}\n// an edit\n")).expect("edit");
    // The edited text has another key; the old entry, made to give `walk`
    // under its own key, records the old text.
    std::fs::write(t.cache.join(&name), forged(&text, Some(&name))).expect("forge");
    assert_eq!(t.check(false), (Some(1), refused.clone()), "older own text");
    std::fs::write(&module, STATE_MODULE).expect("restore");
    // The entry records each file the own load read; one other than the
    // module, edited, makes the entry stale.
    let input = (text.lines().skip(2))
        .filter_map(|l| l.split('\t').next())
        .find(|p| p.ends_with(".vyrn") && !p.ends_with("sgstate.vyrn"))
        .expect("an input besides std/sgstate");
    let s = std::fs::read_to_string(input).expect("read input");
    std::fs::write(input, format!("{s}\n// an edit\n")).expect("edit input");
    std::fs::write(t.cache.join(&name), forged(&text, Some(&name))).expect("forge");
    assert_eq!(t.check(false), (Some(1), refused), "older input text");
}
