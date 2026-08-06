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
//! | `ifExpr` | §2a | steady | Phase 4c: the TYPE says the result owns a buffer, so the binding does |
//! | `mutString` | §2c | steady | Phase 4c: rule 1 governs reassignment, so `mut` is trackable |
//! | `selfAppend` | §4 / P1 | steady | Phase 5: a store releases the old value, and a module-state accumulator grows in place |
//! | `fieldOverwrite` | §4 | steady | Phase 5: `r.field = v` releases the old field |
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
const WHY_FIXTURE: &str = r#"type Sizer = fn(Int64) -> Int64

fn takes(s: String) -> Int64 {
    let named = s
    return named.byteLength
}

fn make(a: String, b: String) -> String {
    let whole = a + b
    return whole
}

fn borrow(s: String) -> String {
    return s.copy()
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
    let cellA = cell(1)
    let cellB = cellA
    let mut cellC = cell(2)
    let held = a + b
    let f: Sizer = |x| x + held.byteLength
    region {
        let arena = a + b
        print(arena)
    }
    print(kept)
    print(grown)
    print(branch)
    print(alias)
    print(given)
    return n + get(cellB) + get(cellC) + f(1)
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
    // The reclaimed ones, and how. Three of these were leaks until Phase 4c:
    // `grown` because it is `mut`, `branch` because an if-expression was not on
    // a list of expression forms, and `given` because any call might retain its
    // argument. Rules 1 to 3 answer all three, so the rule answers them here.
    has("kept             reclaimed at block exit — freeing the String buffer");
    has("grown            reclaimed at block exit — freeing the String buffer");
    has("branch           reclaimed at block exit — freeing the String buffer");
    has("given            reclaimed at block exit — freeing the String buffer");
    has("alias            reclaimed at block exit — freeing the String buffer");
    // Every reason the printer can still name.
    has("arena            NOT reclaimed — it is inside a `region`");
    has("c                NOT reclaimed — the type Bool owns no heap");
    has("cellA            NOT reclaimed — another binding aliases it at line");
    has("cellB            NOT reclaimed — it is a second name for a value it did not take");
    has("cellC            NOT reclaimed — it is `mut`, and its release is observable");
    has("held             NOT reclaimed — a lambda or a spawn captures it at line");
    has("named            NOT reclaimed — it is a borrow of somebody else's value");
    // Not leaks, and the report must not call them leaks.
    has("a                static data");
    has("gone             reclaimed by `drop` at line");
    has("whole            moved at line");
    has("owner            moved at line");
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
    // This function returned its parameter until Phase 4b, and the printer
    // downgraded it to "transfers: no". Rule 3 refuses that program, so the
    // fixture writes `s.copy()` and the answer is yes. **No compiling program
    // can print "transfers: no" for a heap-owning return type any more** — that
    // branch of the printer is unreachable, which is rule 3 stated as a property
    // of the report rather than as a diagnostic.
    assert!(
        text.contains(
            "fn borrow(s: String) -> String\n    transfers: yes — the caller owns the result, \
             and releases it by freeing the String buffer"
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
    assert!(text.contains("captured by a lambda or a spawn"), "{text}");
    assert!(text.contains("it names somebody else's value"), "{text}");
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
        today: Shape::Steady,
        why: "Phase 4c: reclamation follows the type, and an if-expression that yields a \
              String yields a String — the expression's FORM stopped deciding",
    },
    Row {
        export: "mutString",
        census: "§2c",
        today: Shape::Steady,
        why: "Phase 4c: `mut` used to mean nobody could say who owned the value after a \
              reassignment. Rule 1 says: the binding does, and it owns whatever it holds \
              last. Releasing the OLD value on a store is Phase 5, so this row does \
              not reassign. An in-place append (`s = s + \"x\"`) is a second leak of \
              its own: the accumulator shadow starts every `let` unowned, so the \
              first append abandons the initializer's buffer",
    },
    Row {
        export: "selfAppend",
        census: "§4/P1",
        today: Shape::Steady,
        why: "Phase 5: a store releases what the place held, and a module-state accumulator \
              grows in place. The reset hands back the last call's buffer and the eight \
              appends reallocate one",
    },
    Row {
        export: "fieldOverwrite",
        census: "§4",
        today: Shape::Steady,
        why: "Phase 5: `r.field = v` releases the old field after the new value is built",
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

/// Census §2c. A `mut` String. The keyword alone used to disqualify a binding
/// from reclamation; now the binding owns its buffer and the block frees it.
///
/// It does not reassign. A store that overwrites an owning place must release
/// what was there, and that is Phase 5 — see `selfAppend` below, which is the
/// same hole through module state.
export extern fn mutString() {{
    let mut s = tag() + "!"
    seen = seen + Int64(s.byteLength)
}}

/// Census §4/P1, and the shape a server has: module state reset and then grown.
/// `examples/bin` and `examples/shelf` both rebuild module state per request.
///
/// Both halves of Phase 5 are here. The reset releases the buffer the last call
/// built, and the self-append grows the new one IN PLACE rather than building a
/// fresh buffer per turn and abandoning the old — which is what a module-state
/// accumulator did until Phase 5, because the in-place whitelist read one body
/// and a global is reachable from all of them.
///
/// It resets, and it has to. An accumulator that only ever grows has no bounded
/// steady state to assert: its memory IS the string it built. What that costs is
/// P1's question and `examples/membench.vyrn` answers it.
export extern fn selfAppend() {{
    acc = ""
    let mut i = 0
    while i < 8 {{
        acc = acc + "0123456789"
        i = i + 1
    }}
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
