//! RFC-0077 M6 — the wasm target reclaims what it allocates.
//!
//! Three-way parity cannot see this. Reclaiming memory is not observable in
//! output, which is exactly why an allocator that never freed survived every
//! milestone of RFC-0077 and three separate notes describing one face of it each.
//! `memory.buffer.byteLength` is the only thing that can see it, and reading that
//! means a host that keeps the instance alive across many calls — Node, the same
//! shape as `web/`.
//!
//! **It asserts a relation, not a number.** Memory after N calls equals memory
//! after 4N. A byte count would break on every allocator change; the relation
//! survives them, and it is the property that matters: the steady state is
//! bounded, not merely small. The repo writes tests this way elsewhere —
//! `take(unfold(..), n)` pins `n`, and RFC-0082's quadratic pins compare 40x400
//! against 10x1600.
//!
//! It runs the REAL `web/wasi-min.js`, copied to a `.mjs` so Node reads it as the
//! ES module it is. That is deliberate: the JS half of the fix is there — the
//! caller owns a `String` argument (RFC-0012), so the page has to hand it back
//! through `__vyrn_free`, and a test against a private loader would not check it.
//!
//! Skips, loudly, when `node` is absent. Same posture as the parity harness with
//! wasmtime.
//!
//! ---
//!
//! # The census baseline (RFC-0087 §13, RFC-0089 M0)
//!
//! The second test in this file is a different kind of test. It runs one export
//! per memory scenario the census names and records **what each one does today**
//! — which for most of them is to leak. A leaking row asserts that it leaks.
//!
//! That is deliberate. The memory model is about to change under RFC-0089, and
//! parity is blind to memory: a wrong change can pass every output comparison in
//! the repo while it leaks or double-frees. This table is the only thing that
//! sees the difference. Each later phase flips the rows it fixes from `Leaks` to
//! `Steady`, one edit per row, and the test names which census section moved.
//!
//! | export | census | today | why |
//! |---|---|---|---|
//! | `control` | §2 | steady | a local concat, freed at block exit — the whole model working |
//! | `copyLocal` | U2 | steady | `x.copy()` transfers, so the copy has an owner (RFC-0089 M1b) |
//! | `ifExpr` | §2a | leaks | `transfers` answers `false` for an if-expression |
//! | `selfAppend` | §4 / P1 | leaks | a module-state `String` overwrite never releases the old buffer |
//! | `fieldOverwrite` | §4 | leaks | `r.field = v` never releases the old field |
//! | `returnedString` | §9a | leaks | an exported return hands JS a pointer nobody frees |
//! | `optionString` | §14 | leaks | an aggregate does not own its payload |
//! | `lambdaLoop` | §16 | leaks | a stored closure's capture block is never freed |
//! | `spawnFrame` | §10 | steady | see below — on wasm there is no frame to leak |
//!
//! **§10 does not reach this harness.** The census says a `spawn` frame is
//! malloc'd and never freed. On wasm there are no threads, so the direct backend
//! lowers `spawn f(a)` to `f(a)` at the spawn point and allocates no frame at
//! all. The row is here and steady to record that: §10 is a native-only leak,
//! and this file cannot see it. It still guards the lowering — a wasm `spawn`
//! that starts allocating shows up as a row that moved.
//!
//! Two rows carry a finding the census did not have:
//!
//! - **§16 needs a *stored* closure.** A lambda handed straight to a `fn`
//!   parameter is monomorphized and allocates nothing. The leak needs
//!   `let f: Bump = |x| x + k` — a fn-typed binding, RFC-0037's shape.
//! - **§4's record-field row leaks the OLD field**, not the record.
//!
//! Sizes are chosen so a leak is visible against the 128 KiB the module starts
//! with: the strings are ~900 bytes, which is the size RFC-0077 M6 measured
//! `domdemo` with.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    Command::new(&node).arg("--version").output().ok().filter(|o| o.status.success())?;
    Some(PathBuf::from(node))
}

/// The measurement's shape, from RFC-0077 M6: an exported function taking a
/// `String` the JS caller allocated inside the module, plus a `String` the module
/// allocates and drops on its own.
///
/// Both halves are needed. The argument is the host's leak — `own` keys droppable
/// on `Stmt::Let`, so a parameter is borrowed and the caller frees it — and
/// `echo` is the module's, reclaimed by the block-exit release M6 taught this
/// backend to emit.
const FIXTURE: &str = r#"let mut seen: Int64 = 0

export extern fn absorb(arg: String) {
    let echo = arg + "!"
    seen = seen + Int64(echo.byteLength)
}

fn main() -> Int64 {
    return 0
}
"#;

const DRIVER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const bytes = await readFile(new URL("./mem.wasm", import.meta.url));
const arg = "x".repeat(900);

// A fresh instance per run, so the two answers are two steady states rather than
// one run's tail.
async function after(n) {
  const { exports, memory } = await runVyrn(bytes, {});
  for (let i = 0; i < n; i++) exports.absorb(arg);
  return memory.buffer.byteLength;
}

const n = Number(process.argv[2]);
console.log(await after(n));
console.log(await after(4 * n));
"#;

#[test]
fn the_wasm_heap_reaches_a_steady_state() {
    let Some(node) = find_node() else {
        eprintln!("NOTE: no node — RFC-0077 M6's reclamation is unverified on this machine");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vyrn-m6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mem.vyrn"), FIXTURE).unwrap();
    std::fs::write(dir.join("drive.mjs"), DRIVER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(dir.join("mem.vyrn"))
        .args(["--target", "wasm", "-o"])
        .arg(dir.join("mem.wasm"))
        .output()
        .expect("vyrn build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    // 5000 and 20000 — the RFC's own measurement is the second of the two, where
    // 900-byte arguments grew `domdemo.wasm` from 2 pages to 277.
    let n = 5000;
    let out = Command::new(&node)
        .arg(dir.join("drive.mjs"))
        .arg(n.to_string())
        .output()
        .expect("node");
    assert!(out.status.success(), "node failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let sizes: Vec<u64> =
        text.split_whitespace().map(|s| s.parse().expect("a byte count")).collect();
    assert_eq!(sizes.len(), 2, "expected two byte counts, got {text:?}");

    assert_eq!(
        sizes[0], sizes[1],
        "the wasm heap grew with the call count: {} bytes after {n} calls, {} after {}. \
         Four times the work must cost the same memory — a difference means something \
         allocated is never handed back.",
        sizes[0],
        sizes[1],
        4 * n
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// `vyrn why --memory` (RFC-0087 U1) — the model, made visible.
//
// Driven through the real binary, over the exact three bindings the census
// opens with: one shape, three outcomes, and nothing in the source saying
// which. Asserting on the text a user sees is the point — an in-process API
// could agree with itself while the command says something else.
// ---------------------------------------------------------------------------

/// The census U1 program, plus one binding for every reason the printer names.
const WHY_FIXTURE: &str = r#"fn takes(s: String) -> Int64 {
    return s.byteLength
}

fn make(a: String, b: String) -> String {
    let whole = a + b
    return whole
}

fn borrow(s: String) -> String {
    return s
}

fn main() -> Int64 {
    let a = "a"
    let b = "b"
    let c = true
    let kept = a + b
    let mut grown = a + b
    let branch = if c { a + b } else { a + "c" }
    let owner = a + b
    let alias = owner
    let given = a + b
    let n = takes(given)
    let gone = a + b
    drop gone
    region {
        let arena = a + b
        print(arena)
    }
    print(kept)
    print(grown)
    print(branch)
    print(alias)
    return n
}
"#;

fn why_memory_output() -> String {
    // One directory per caller: these tests run in parallel, and a shared path
    // would have one of them delete another's fixture mid-run.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn-why-mem-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("u1.vyrn");
    std::fs::write(&file, WHY_FIXTURE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .args(["why", "--memory"])
        .arg(&file)
        .output()
        .expect("vyrn why --memory");
    assert!(out.status.success(), "`why` reports; it does not gate");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn why_memory_names_the_reason_each_binding_is_not_reclaimed() {
    let text = why_memory_output();
    let has = |needle: &str| {
        assert!(text.contains(needle), "expected {needle:?} in:\n{text}");
    };
    // The reclaimed one, and how.
    has("kept             reclaimed at block exit — freeing the String buffer");
    // Every reason the census asks the printer to distinguish.
    has("grown            NOT reclaimed — it is `mut`");
    has("branch           NOT reclaimed — an if-expression does not transfer ownership");
    has("owner            NOT reclaimed — another binding aliases it at line");
    has("given            NOT reclaimed — it escapes into the call to `takes` at line");
    has("arena            NOT reclaimed — it is inside a `region`");
    has("c                NOT reclaimed — the type Bool owns no heap");
    // Not leaks, and the report must not call them leaks.
    has("a                static data");
    has("gone             reclaimed by `drop` at line");
    has("whole            moved out by the return at line");
}

#[test]
fn why_memory_says_which_functions_transfer_ownership() {
    let text = why_memory_output();
    assert!(
        text.contains(
            "fn make(a: String, b: String) -> String\n    transfers: yes — the caller owns the \
             result, and releases it by freeing the String buffer"
        ),
        "{text}"
    );
    // A function that returns a parameter hands back something it does not own
    // — RFC-0089 rule 3's error, today's silent downgrade.
    assert!(
        text.contains(
            "fn borrow(s: String) -> String\n    transfers: no — the return type String owns heap"
        ),
        "{text}"
    );
    assert!(
        text.contains("fn takes(s: String) -> Int64\n    transfers: no — the return type Int64 \
                       owns no heap"),
        "{text}"
    );
}

#[test]
fn why_memory_counts_the_whole_file() {
    let text = why_memory_output();
    // The summary is the corpus instrument: one line of totals, then the leaks
    // grouped by reason.
    assert!(text.contains("  summary: "), "{text}");
    assert!(text.contains(" reclaimed, "), "{text}");
    assert!(text.contains("not reclaimed"), "{text}");
    assert!(text.contains("aliased by another binding"), "{text}");
    assert!(text.contains("escaped into a call"), "{text}");
}

// ---------------------------------------------------------------------------
// The census baseline. See the module comment for the table and for why the
// leaking rows assert that they leak.
// ---------------------------------------------------------------------------

/// What a shape does to the heap over four times the calls.
#[derive(PartialEq, Eq, Debug)]
enum Shape {
    /// Four times the work costs the same memory.
    Steady,
    /// Memory grows with the call count.
    Leaks,
}

/// One census scenario: the export that exercises it, where it is written down,
/// and what it does today.
struct Row {
    export: &'static str,
    census: &'static str,
    today: Shape,
    why: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        export: "control",
        census: "§2",
        today: Shape::Steady,
        why: "a local concat is freed at block exit — the control for every row below",
    },
    Row {
        export: "copyLocal",
        census: "U2",
        today: Shape::Steady,
        why: "`x.copy()` (RFC-0089 M1b) transfers, so the copy is reclaimed at block exit — \
              the row exists to prove the new builtin is not a new leak",
    },
    Row {
        export: "ifExpr",
        census: "§2a",
        today: Shape::Leaks,
        why: "`transfers` answers `false` for an if-expression, so nothing owns the result",
    },
    Row {
        export: "selfAppend",
        census: "§4/P1",
        today: Shape::Leaks,
        why: "a module-state String overwrite never releases the old buffer",
    },
    Row {
        export: "fieldOverwrite",
        census: "§4",
        today: Shape::Leaks,
        why: "`r.field = v` stores over the old field and never releases it",
    },
    Row {
        export: "optionString",
        census: "§14",
        today: Shape::Leaks,
        why: "an aggregate does not own its payload, so the String in the Option has no owner",
    },
    Row {
        export: "lambdaLoop",
        census: "§16",
        today: Shape::Leaks,
        why: "a stored closure's capture block is never freed",
    },
    Row {
        export: "returnedString",
        census: "§9a",
        today: Shape::Leaks,
        why: "an exported return hands JS a pointer and nothing frees it",
    },
    Row {
        export: "spawnFrame",
        census: "§10",
        today: Shape::Steady,
        why: "wasm has no threads, so `spawn f(a)` IS `f(a)` and allocates no frame — \
              §10's leak is native-only and this harness cannot see it",
    },
];

/// One export per row. The strings are ~900 bytes so one leaked buffer is
/// visible against the 128 KiB the module starts with.
fn shapes_fixture() -> String {
    let pad = "x".repeat(900);
    format!(
        r#"let mut seen: Int64 = 0

let mut acc: String = ""

type Bump = fn(Int64) -> Int64

type Row = {{ name: String, n: Int64 }}

let mut row: Row = Row {{ name: "", n: 0 }}

/// A ~900-byte literal. It lives in the data segment, so calling this allocates
/// nothing — every allocation below is the concatenation, and only that.
fn tag() -> String {{
    return "{pad}"
}}

/// The recommended fallible style (RFC-0005/0009/0079), which is also the
/// leaking one — census §14.
fn maybe(x: String) -> Option<String> {{
    return Some(x + "!")
}}

export extern fn control() {{
    let s = tag() + "!"
    seen = seen + Int64(s.byteLength)
}}

/// RFC-0089 M1b. The copy is a fresh buffer with one owner, released at block
/// exit exactly as `control`'s concatenation is.
export extern fn copyLocal() {{
    let s = tag() + "!"
    let c = s.copy()
    seen = seen + Int64(c.byteLength)
}}

export extern fn ifExpr() {{
    let c = seen % 2 == 0
    let s = if c {{ tag() + "a" }} else {{ tag() + "b" }}
    seen = seen + Int64(s.byteLength)
}}

export extern fn selfAppend() {{
    acc = acc + "0123456789"
    seen = seen + Int64(acc.byteLength)
}}

export extern fn fieldOverwrite() {{
    row.name = tag() + "x"
    seen = seen + Int64(row.n)
}}

export extern fn optionString() -> Int64 {{
    if let Some(s) = maybe(tag()) {{
        return Int64(s.byteLength)
    }}
    return 0
}}

export extern fn lambdaLoop() {{
    let mut i = 0
    while i < 32 {{
        let k = i
        let f: Bump = |x| x + k
        seen = seen + f(i)
        i = i + 1
    }}
}}

export extern fn returnedString() -> String {{
    return tag() + "!"
}}

fn work(n: Int64) -> Int64 {{
    return n + 1
}}

export extern fn spawnFrame() {{
    let t = spawn work(seen)
    seen = t.join()
}}

fn main() -> Int64 {{
    return 0
}}
"#
    )
}

/// Reads every row's two byte counts, a fresh instance per count so the two
/// answers are two steady states rather than one run's tail.
const SHAPES_DRIVER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const bytes = await readFile(new URL("./shapes.wasm", import.meta.url));
const names = process.argv.slice(3);
const n = Number(process.argv[2]);

async function after(name, calls) {
  const { exports, memory } = await runVyrn(bytes, {});
  for (let i = 0; i < calls; i++) exports[name]();
  return memory.buffer.byteLength;
}

for (const name of names) {
  console.log(name, await after(name, n), await after(name, 4 * n));
}
"#;

#[test]
fn the_census_shapes_hold_their_measured_baseline() {
    let Some(node) = find_node() else {
        eprintln!("NOTE: no node — the census baseline is unmeasured on this machine");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vyrn-shapes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("shapes.vyrn"), shapes_fixture()).unwrap();
    std::fs::write(dir.join("drive.mjs"), SHAPES_DRIVER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(dir.join("shapes.vyrn"))
        .args(["--target", "wasm", "-o"])
        .arg(dir.join("shapes.wasm"))
        .output()
        .expect("vyrn build");
    assert!(build.status.success(), "build failed:\n{}", String::from_utf8_lossy(&build.stderr));

    // 500 and 2000. `selfAppend` is quadratic in the call count (census P1), so
    // a larger N buys nothing and costs minutes.
    let n = 500;
    let mut cmd = Command::new(&node);
    cmd.arg(dir.join("drive.mjs")).arg(n.to_string());
    for r in ROWS {
        cmd.arg(r.export);
    }
    let out = cmd.output().expect("node");
    assert!(out.status.success(), "node failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);

    let mut lines = text.lines();
    for r in ROWS {
        let line = lines.next().unwrap_or_else(|| panic!("no reading for `{}`", r.export));
        let cols: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(cols.len(), 3, "expected `name n 4n`, got {line:?}");
        assert_eq!(cols[0], r.export, "the driver answered out of order");
        let (a, b): (u64, u64) = (cols[1].parse().unwrap(), cols[2].parse().unwrap());
        let seen = if b > a { Shape::Leaks } else { Shape::Steady };
        assert_eq!(
            seen,
            r.today,
            "census {} moved: `{}` now reads {:?}, and this table says {:?}.\n  \
             {} bytes after {n} calls, {} after {}.\n  \
             baseline: {}\n  \
             If a later phase fixed it, flip this row to Shape::Steady. If nothing \
             meant to change it, something regressed.",
            r.census,
            r.export,
            seen,
            r.today,
            a,
            b,
            4 * n,
            r.why
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
