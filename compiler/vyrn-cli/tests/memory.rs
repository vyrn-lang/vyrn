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
//! | `returnedString` | §9a | steady | Phase 6: a return is owned, so the wrapper frees it after decoding |
//! | `optionString` | §14 | steady | Phase 10a: an `if let` over a temporary gets the reclamation row a `let` gets, so the arms' escapes are recorded against it |
//! | `lambdaLoop` | §16 | steady | Phase 10b: a stored closure owns its capture block, and the copy rule 1 demands is derived over RFC-0037's defunctionalized enum |
//! | `elementLeak` | U4 | steady | RFC-0092 M2: an array owns its elements, because M1's rule proves every route into one is a store |
//! | `recordFields` | RFC-0089 rule 4 | steady | RFC-0092 M3: an aggregate releases its places, so a record hands its two Strings back with it |
//! | `takenField` | RFC-0093 M2 | steady | the walk skips the place a `consume` took, so N turns allocate N and free N — not 2N, which is the double free, and not 0, which is the leak M1 shipped |
//! | `slotsContainer` | U4 / RFC-0090 M1 | steady | a DECLARED container gives its elements back — Phase 8b |
//! | `keysLoop` | U4's price | steady | RFC-0092 M5: a `for` over a temporary owns the snapshot, so it releases it — Phase 10a's row, at the second statement that walks one |
//! | `mapRepeatKey` | U4's price, the other half | steady | a map takes its key, so it releases the key it does not keep — the hit path used to drop it, which is a leak per repeat in every histogram loop |
//! | `mapReplaceValue` | RFC-0028, the value half | steady | a map takes the VALUE too, so a store over an existing key releases the value it replaces — the half missed one line from the key's |
//! | `mapRemoveEntry` | RFC-0028, both halves | steady | `remove` gives up the whole entry, so the key AND the value go back — the runtime shim shifts bytes and is handed no types, so only the call site can |
//! | `spawnFrame` | §10 | steady | RFC-0095 M1: a task is linear, and both discharges give its storage back |
//! | `consumingLoop` | U4's price, one keyword over | steady | RFC-0092 M5's row for `for x in consume xs`: the loop is the buffer's last owner, so it releases it at every exit |
//! | `selfReferring` | RFC-0096 | steady | a type that reaches ITSELF is released by DECLARATION — the walk emits a call at the `impl Owned`, which is the bottom it lacked, and every type above it gets its structural row back |
//! | `injectedJson` | RFC-0096 M2 | steady | the declaration is in an INJECTED module, so the type key is the linker's renamed spelling — and a `Json` that declares `Copy` as well is released once as the original and once as the copy |
//! | `exprTemporary` | RFC-0096 M3 | steady | a String an EXPRESSION allocated has no binding, so the CONSUMER releases it — `@concat`, a String `+`, `@str` and the in-place append each free an operand the expression itself built |
//! | `argsBlock` | RFC-0014, the wasm half | steady | `args()` handed back a data pointer four bytes past a block, which `free` reads as a refusal — so `drop xs` reclaimed nothing on wasm where native has always reclaimed |
//! | `bytesRejected` | RFC-0014 M2 | steady | a REJECTED `stringFromBytes` gives its buffer back: the buffer is allocated before the scan, and both refusals used to leave with the message and without it |
//! | `keptForever` | the detector itself | **leaks, on purpose** | the canary: every other row asserts `steady`, and so does a measurement that stopped measuring |
//! | `localAccumulator` | RFC-0096 M3, defect 3 | steady | the static-data rule read the INITIALIZER, so `let mut acc = ""` answered `Static` for the buffer the loop grew; it asks whether the binding can CHANGE now |
//! | `callArgument` | census-call-arguments | steady | a value the ARGUMENT built has no binding either, so the CALLER releases it after a call that keeps nothing — the proof was never missing, only the place to write it down. A String `+` is `@concat` written as an operator, so its operands are call arguments too and `"n" + label(i)` takes the same verdict |
//!
//! **Twenty-one rows, twenty-one steady.** RFC-0092 M5 closed the last leaking
//! one, and the row beside it — the same statement with `consume` written on it
//! — was not in the census at all. RFC-0096 closed the last CLASS: the corpus
//! reading "the type has no release rule" fell from 63 to 0 on two `impl`s, and
//! M2 took the linked reading from 33 to 0 on two more. M3 closed the one shape
//! M2 measured out of its own row: an operand with no owner. The call-argument
//! census closed the class next door, and by the same reading one level down: an
//! ARGUMENT with no owner.
//!
//! **§10 reaches this harness in half.** A task owns a frame, a task record and
//! an operating-system handle. On wasm there are no threads, so the direct
//! backend runs the thunk at the spawn point and the whole task is one heap box:
//! the frame, with no record and no handle beside it. RFC-0095 M1 frees that box
//! at the join and at the `drop`, and the row is sized so a missed release shows
//! — a `Task<String>` carrying ~900 bytes, dropped rather than joined, where the
//! old row leaked 8 bytes a call and hid inside a page.
//!
//! **The handle is native-only, and it is the part that matters.** Bytes are a
//! leak a program can live with; one operating-system handle per spawn is a
//! server that meets a per-process ceiling and stops.
//! [`the_spawn_handles_go_back_natively`] is that measurement, beside this table
//! rather than in it, because it needs clang and a real process.
//!
//! **Audit finding C2.3 is native-only too, and for the opposite reason.** An
//! empty String built at run time was never freed, because `cap == 0` named both
//! "static literal" and "empty heap buffer" and the native `free` could not tell
//! them apart. The wasm `free` discriminates on the ADDRESS, so this table would
//! have read the row steady before the fix and steady after it, and seen
//! nothing. [`an_empty_string_built_at_run_time_goes_back_natively`] is that
//! measurement — the same relation, against a one-byte control instead of
//! against a larger N.
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// The Node this file measures the heap through, or `None` when there is none.
///
/// Both tests below SKIP without it, and a skip here is the loudest hole in the
/// repo's regression cover: three-way parity cannot see memory at all, so when
/// Node is absent nothing in the build checks that the model reclaims anything.
/// `VYRN_REQUIRE_TOOLS=1` (which CI exports, and which `tests/common/mod.rs`
/// reads for `wasmtime`) turns that skip into a failure, so an environment that
/// meant to measure cannot quietly stop.
fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    let found = Command::new(&node)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from(&node));
    if found.is_none() && std::env::var_os("VYRN_REQUIRE_TOOLS").is_some() {
        panic!(
            "VYRN_REQUIRE_TOOLS is set and `node` was not found — this run would have \
             silently skipped the memory census, the only thing here that sees a leak. \
             Point `VYRN_NODE` at the binary, or unset VYRN_REQUIRE_TOOLS."
        );
    }
    found
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
    assert!(
        out.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let sizes: Vec<u64> = text
        .split_whitespace()
        .map(|s| s.parse().expect("a byte count"))
        .collect();
    assert_eq!(sizes.len(), 2, "expected two byte counts, got {text:?}");

    assert_eq!(
        sizes[0],
        sizes[1],
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

// A type that DECLARES what it owns (RFC-0086 M1), holding no heap of its own.
// Phase 8e's replacement for the two `cell` bindings this fixture used to carry:
// a value rule 1 does not move, so a second name for one is an alias and neither
// name may release it. `Ref<T>` was the built-in case and is gone; a declared
// container is the live one, and it is census U4's own shape.
type Ticket = { id: Int64 }

impl Owned for Ticket {
    fn release(self) {
        print("released")
    }
}

fn mint(n: Int64) -> Ticket {
    return Ticket { id: n }
}

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

fn score(n: Int64) -> Int64 {
    return n * 2
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
    let ticket = mint(1)
    let second = ticket
    let held = a + b
    let sent = a + b
    let job = spawn takes(sent)
    let doubled = job.join()
    let f: Sizer = x -> x + held.byteLength
    region {
        let arena = a + b
        print(arena)
    }
    print(kept)
    print(grown)
    print(branch)
    print(alias)
    print(given)
    return n + second.id + f(1) + doubled
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
    // Round fifty-seven: a LAMBDA's capture is a deep snapshot, so the
    // captured binding reclaims; only a `spawn`'s capture stays a leak.
    has("held             reclaimed at block exit — freeing the String buffer");
    has("sent             NOT reclaimed — a lambda or a spawn captures it at line");
    has("named            NOT reclaimed — it is a borrow of somebody else's value");
    has("ticket           NOT reclaimed — another binding aliases it at line");
    has("second           NOT reclaimed — it is a second name for a value it did not take");
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
        text.contains(
            "fn takes(s: String) -> Int64\n    transfers: no — the return type Int64 \
                       owns no heap"
        ),
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
    // `rfcs/census-regions.md` defect 2: a task is discharged where it is
    // joined, so the report may not call it a leak. It has its own column.
    assert!(text.contains(" discharged, "), "{text}");
    assert!(
        text.contains("discharged, not leaked — a task is joined, forwarded or dropped"),
        "{text}"
    );
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
        export: "temporaryCall",
        census: "RFC-0114 R1′",
        today: Shape::Steady,
        why: "RFC-0114 R1′: the unnamed String receiver of `.byteLength` is freed right \
              after the header read — the read was its last observer. The field exists \
              only on String, which is the type proof; the producer must transfer \
              ownership (`owned_fns`, lenders filtered by `facts`), which is the \
              ownership proof. A heap field of a temp RECORD stays out: extraction lends",
    },
    Row {
        export: "escapingAccumulator",
        census: "RFC-0114 M2",
        today: Shape::Steady,
        why: "RFC-0114 M2: ownedness is per STORE, not per binding. `fold_store_owned` \
              proves each reassignment's old value has no other holder — the final \
              consume takes only the last value, so every store before it releases. \
              The old `slot_owns` gate abandoned the whole binding for that one consume",
    },
    Row {
        export: "conditionalMove",
        census: "RFC-0114 Rule N",
        today: Shape::Steady,
        why: "RFC-0114 Rule N: the still-owning edge releases what the other branch \
              consumed. The union at the merge says consumed, so block exit stays out of \
              it — the release sits ON THE EDGE, which is the one point where the path \
              that kept the value is known. 215.3 MB native before; one buffer after",
    },
    Row {
        export: "conditionalMoveMatch",
        census: "RFC-0114 Rule N",
        today: Shape::Steady,
        why: "Rule N at a match join: one arm consumes, another only reads, the release \
              sits on the untouched arm's edge (its source index). The guards fail toward \
              the leak — a binder shadow, a scrutinee mention, or an arm value that could \
              alias the binding all refuse; a Binary value never can, so it does not",
    },
    Row {
        export: "conditionalMoveIfExpr",
        census: "RFC-0114 Rule N",
        today: Shape::Steady,
        why: "Rule N at an if-expression join, the third join shape. The release sits \
              under the untouched branch's value — stack-neutral in the wasm lowering, \
              before the branch to the phi in the textual one — with the match's value \
              guard: the branch value must not be able to alias what the edge frees",
    },
    Row {
        export: "revivedBinding",
        census: "RFC-0114 untake",
        today: Shape::Steady,
        why: "The take is real, but the binding is provably re-established — the last \
              event is an owning write at the let's own loop set and branch path, every \
              take precedes it, and no early exit sits between — so block exit releases \
              the FINAL value. A conditional revive, or a return/break/`?` in the \
              window, refuses toward the leak",
    },
    Row {
        export: "consumedParamRead",
        census: "RFC-0114 param",
        today: Shape::Steady,
        why: "A `consume` parameter is a value the frame OWNS, and it was the one owned \
              value with no row — released only if the body wrote `drop v`. It is keyed \
              by its `Param` node now, exactly as a `let` is keyed by its statement; a \
              param that is moved on, dropped, or returned still releases nothing, and \
              a declared release runs at the callee's exit in all three engines",
    },
    Row {
        export: "temporaryArrayLength",
        census: "RFC-0114 R1′",
        today: Shape::Steady,
        why: "The container counterpart of `temporaryCall`: `.length` on an unnamed \
              Array the frame owns frees the receiver — buffer and elements — right \
              after the count is read. The producer's return KIND is the filter \
              (FreeArr/FreeSmallArr/FreeMap join FreeStr); a declared release never \
              enters the set, so the free is always silent",
    },
    Row {
        export: "temporaryRecordField",
        census: "RFC-0114 R1′",
        today: Shape::Steady,
        why: "A heap field of a temporary record: `names_a_place` called every field \
              read a borrow, but a field of a value NOBODY owns has no owner to borrow \
              from — the binding owns the extracted buffer now, and its block exit \
              releases it. The record aggregate itself is by value, so for a record \
              whose only heap is the extracted field, the transfer is complete",
    },
    Row {
        export: "temporaryRecordScalar",
        census: "RFC-0114 R1′",
        today: Shape::Steady,
        why: "A scalar field of a temporary record: the read is the record's last \
              observer, so the record is freed whole right after it — deep, so its \
              heap fields go too. A heap or `lazy` field, or an aggregate one (an \
              address INTO the record), stays out; a `Deep` producer is admitted \
              only while the program declares no `impl Owned` anywhere",
    },
    Row {
        export: "temporaryChainedField",
        census: "RFC-0114 R1′",
        today: Shape::Steady,
        why: "`makeTag(..).label.byteLength` — the receiver is a heap field of a record               temporary. The `@fieldof:` marker carries the producer through the lender               filter, and `own` admits it without the Deep gate: what the edge frees is               the FIELD it read, a String or container, silent either way",
    },
    Row {
        export: "prependLoop",
        census: "§4",
        today: Shape::Steady,
        why: "A `+` always allocates, so the store releases what it replaced. The guard was               \"does the new value mention the place\", which is right for `a = @push(a, i)`               and wrong for a concat; the append spine hid it, because the shape that reaches               the general store is a PREPEND and nobody writes one in a hot loop until they do",
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
        today: Shape::Steady,
        why: "Phase 10a: an `if let` whose scrutinee is a TEMPORARY gets the reclamation row \
              a `let` gets, keyed by the statement. Phase 5 built the release and took it out \
              again because the payload escapes the arm and nothing recorded that; recording \
              it is the whole fix. The binders bind to the statement's row, so every `return`, \
              store, capture and handover this pass already writes lands on it, and a row with \
              nothing written is a value the arms did not hand on. The release runs on a drop \
              frame of its own, so an arm that returns early releases it too",
    },
    Row {
        export: "lambdaLoop",
        census: "§16",
        today: Shape::Steady,
        why: "Phase 10b: a stored closure owns its capture block, so the block is freed at \
              block exit and the loop stops allocating one per turn. The copy rule 1 then \
              demands is DERIVED over RFC-0037's defunctionalized enum — one function per \
              module, a switch from tag to block size, then one malloc and one memcpy — \
              because a copy site cannot measure a size the tag decides at run time. Phase 5 \
              said the fix menu's `.copy()` could not be written and 7b said `Copy` as a \
              protocol was not the mechanism; both were right about the type key and wrong \
              about the dead end. Copy and release are both SHALLOW: two lambdas over one \
              String build two blocks holding one pointer, so a deep release would free it \
              twice. A captured String therefore still leaks, and `Gone::Captured` already \
              says why nothing else releases it either",
    },
    Row {
        export: "elementLeak",
        census: "U4",
        today: Shape::Steady,
        why: "RFC-0092 M2: an `Array<T>` releases its ELEMENTS as well as its buffer. What was \
              missing was never a declaration site — built-in rows are seeded — but the PROOF \
              that the elements are the array's own, and M1's rule supplies it: every route \
              into an element is a store, and a store of a borrow or of a projection is \
              refused. What was left was the compiler's own back doors, and there were three, \
              not the one the census named: `m.keys()` and `sa.toArray()` handed back a fresh \
              buffer holding somebody else's element WORDS, and `xs.toArray()` on a plain \
              `Array` handed back the receiver's triple unchanged. All three copy now. An \
              element is released the way its own type is, so an `Array<Record>` followed \
              on its own the day M3 gave a record its row",
    },
    Row {
        export: "recordFields",
        census: "RFC-0089 rule 4",
        today: Shape::Steady,
        why: "RFC-0092 M3: an aggregate releases its PLACES. Phase 5 wrote the rule and left \
              the row out, because a record hands its insides out as projections and rule 3 \
              recorded a returned one as a lend rather than refusing it — three parity runs \
              said so within a minute of each other. M1 refuses all three spellings, so the \
              row is sayable, and a record built per call gives its two Strings back with \
              it. A type that reaches ITSELF is still left alone: the walk is structural and \
              has no bottom, which is the guard the `Array` row already carried",
    },
    Row {
        export: "takenField",
        census: "RFC-0093 M2",
        today: Shape::Steady,
        why: "RFC-0093 M2: a take moves a field out of a record and leaves a hole, and the \
              release walk is the TYPE — which does not know the field left. M1 left the whole \
              binding unreclaimed, because a leak is a task and a double free is a bug; M2 \
              carries the hole set from `movecheck` to `own` to both walks, so the record is \
              reclaimed MINUS the place it gave away. The row is the arithmetic: N turns \
              allocate two Strings and free two — N, not 2N and not 0. Where the walk cannot be \
              told, the binding still leaks whole: a declared `release` is a user function, a \
              path that is not a chain of record fields is not a place a static walk reaches, \
              and a write that fills the hole already released what the take gave away",
    },
    Row {
        export: "slotsContainer",
        census: "U4 / RFC-0090 M1",
        today: Shape::Steady,
        why: "Phase 8b: `std/slots` declares `impl<T> Owned for Slots<T>`, and the release gives \
              back every element and then the five buffers. Two refusals had to move for it — \
              a `mut` binding may take a declared release (the interpreter reads the slot now), \
              and a generic impl carries a row (the drop site solves the type arguments and asks \
              for the instance). The third — `drop v` where `v: T` — moved back: the release \
              drops its ARRAYS now, never a bare `T`, and the Param pass was laundering the \
              record rule (RFC-0118 M2), so it is refused again. U4 opens for a container that \
              knows what it owns, and stays open for one that cannot",
    },
    Row {
        export: "keysLoop",
        census: "U4's price",
        today: Shape::Steady,
        why: "RFC-0092 M5: a `for` over a TEMPORARY owns the snapshot, so it releases it — Phase \
              10a's row for an `if let` over a temporary, at the second statement that can walk \
              one, with the same `names_a_place` guard. The loop VARIABLE is bound to that row, so \
              every way an element can leave the body writes on it: a store \
              (`fs.push(Field { key: k, .. })`, which `httpInput` does), a `return`, a `drop`, a \
              capture. A row that says the value left is reclaimed from not at all — the elements \
              the body kept stay allocated and the buffer with them, which is a leak and not a \
              double free. One rule had to be added for it: a map takes its KEY, because both \
              backends write the key pointer into `keys[len]` and copy nothing, so \
              `for k in base.keys() { hs[k] = .. }` (`httpHeaders`) is a move nothing recorded. \
              Measured native, 2000 turns over 100 65-byte keys: 6 MB peak before M2, 24 MB after \
              it, 4 MB now — and 4 MB again at four times the turns",
    },
    Row {
        export: "mapRepeatKey",
        census: "U4's price, the other half",
        today: Shape::Steady,
        why: "A map takes its key, so the map releases the key it does not keep. `movecheck` \
              refuses a BORROWED key now — the rule RFC-0092 M5 named and left, whose absence \
              let `m[ks[i]] = v` give one buffer two owners while the map LITERAL refused the \
              same borrow — and what arrives at `map_set` is therefore always a value the map \
              may own. The hit path used to drop that value on the floor, so every repeat in a \
              histogram loop leaked a key, and the `.copy()` the new rule requires would have \
              paid for it once per turn. Measured native, 200 thousand inserts of one 3-byte \
              key: 10.29 MB peak before, 4.09 MB after",
    },
    Row {
        export: "mapReplaceValue",
        census: "RFC-0028, the value half",
        today: Shape::Steady,
        why: "the same rule as `mapRepeatKey`, for the other thing the map took. A store over \
              a key the map already holds put the new value in the slot and released nothing, \
              so `Map<String, String>` leaked the previous String on every repeat — the value \
              half was missed one line from where the key half was fixed. It reaches any \
              owning value type: a String, an `Array<T>`, a record with String fields. \
              Measured native, 200 thousand stores over one key with ~200-byte values: 12.99 \
              MB peak before, 3.26 MB after",
    },
    Row {
        export: "mapRemoveEntry",
        census: "RFC-0028, both halves",
        today: Shape::Steady,
        why: "`remove` gives up the WHOLE entry, so the key and the value both go back. \
              Neither did: the runtime's `map_remove_at` shifts the survivors down over two \
              strides and is handed no types, so it cannot release either — at that ABI the \
              value is `esz` anonymous bytes. The obligation belongs to the call site, where \
              the two types are known, and the entry is read out of its slots BEFORE the \
              shift moves the survivors over them. An insert-then-remove loop leaked one key \
              String plus the value's heap a turn, unbounded, which is every cache eviction. \
              Measured native, 200 thousand insert-and-remove turns with ~200-byte values: \
              19.48 MB peak before, 3.26 MB after",
    },
    Row {
        export: "argsBlock",
        census: "RFC-0014, the wasm half",
        today: Shape::Steady,
        why: "`args()` handed back an array whose data pointer was `ptrs + 4` — an address \
              four bytes past a block rather than a block. `free` reads the class word at \
              `p - HDR`, which there is the allocation's own header slack: always zero, \
              below `MIN_CLASS`, so the free was refused without a word and `drop xs` \
              reclaimed nothing. Native `__vyrn_args` hands back a fresh `malloc` and has \
              always been freeable, so this was one backend alone. Copying `argv[1..]` down \
              into slot 0 as the elements are built buys back an allocation base, and drops \
              two more per-call blocks with it: the copy of the program name that the `+ 4` \
              left stranded, and the host's staging blob, which native does not have at all \
              because `main` stashes the argv it was handed. Measured on this harness before \
              the fix, a hundred `args()` a call: 1,703,936 bytes after 500 calls and \
              6,488,064 after 2,000; 131,072 at both after it",
    },
    Row {
        export: "bytesRejected",
        census: "RFC-0014 M2",
        today: Shape::Steady,
        why: "`stringFromBytes` allocates the String's buffer before it scans, and both \
              refusals — an embedded NUL, and invalid UTF-8 — left with the message and \
              without the buffer. Rejecting input in a loop is what a parser does with \
              anything it did not write, so this leaked one block a turn for as long as the \
              buffer has been allocated up front. One free at the join covers both exits; \
              the block is not the arena's on any path, because a region records only the \
              `str_temporary` shapes and this call's type is a `Result`. Measured here \
              before the fix over 900 rejected bytes a call: 589,824 bytes after 500 calls \
              and 2,162,688 after 2,000; 131,072 at both after it. The row is on the direct \
              backend only — the textual one frees the buffer on both exits already",
    },
    Row {
        export: "returnedString",
        census: "§9a",
        today: Shape::Steady,
        why: "Phase 6: rule 3 makes a return the caller's, and across this boundary the               caller is `wasi-min.js` — it decodes the String, then hands the block back               through `__vyrn_free`. An export that would lend one no longer compiles",
    },
    Row {
        export: "spawnFrame",
        census: "§10",
        today: Shape::Steady,
        why: "RFC-0095 M1: a `Task<T>` is linear, so `t.join()` consumes it and `drop t` \
              discharges it without taking the result, and both give the task's storage \
              back. wasm has no threads, so `spawn f(a)` runs at the spawn point and the \
              whole task IS one heap box — the frame with no record and no handle beside \
              it. The row used to say this harness could not see §10, and it was right \
              about the reason and wrong about the fix: the box was allocated per call \
              and never freed, 8 bytes at a time, which hides inside one 64 KiB page at \
              500 calls. It is a `Task<String>` now, dropped rather than joined, so a \
              missed release is ~900 bytes a call and the row moves. What this harness \
              still cannot see is the OPERATING-SYSTEM HANDLE, which is the part of §10 \
              that matters and exists only natively — \
              `the_spawn_handles_go_back_natively` below is the measurement beside it",
    },
    Row {
        export: "selfReferring",
        census: "RFC-0096",
        today: Shape::Steady,
        why: "RFC-0096: a type that reaches ITSELF is released by DECLARATION. The release walk \
              is structural, so `type Twig = | Fork(String, Array<Twig>)` has no bottom to stop \
              at, and `release_kind` answered `None` — for the type, for `Array<Twig>`, and for \
              every record that merely REACHES one. That was 63 of the corpus's unreclaimed \
              bindings, all of them in `std/vyx` and `std/graphql`. A declared `release` IS the \
              bottom: the walk emits a call there rather than expanding, so the guard now asks \
              whether the cycle has a declaration ON it rather than whether a cycle exists, and \
              the rows above the declaration come back with it. Two `impl`s closed all 63. The \
              row is four ~900-byte Strings a call in a tree, under a record that only reaches \
              one: removing the `impl` makes it grow, and so does removing the declared stop \
              from the guard — verified by removing each. The depth is the language's own: a \
              release of a chain 10,000 deep is 10,000 native frames, measured to overflow the \
              default 1 MiB Windows stack at 11,000, where an ordinary recursive Vyrn walk over \
              the same chain overflows at 20,000",
    },
    Row {
        export: "injectedJson",
        census: "RFC-0096 M2",
        today: Shape::Steady,
        why: "RFC-0096 M2: the declaration is in an INJECTED module. `std/json` is linked by \
              the `toJson` desugar rather than by an import, and the linker renames its every \
              declaration by prefix — so the type key this row's bindings carry is \
              `json$Json`, and the declared row has to be keyed by the renamed spelling too. \
              It is, and by the patch RFC-0092 M3 landed for `impl Copy for Json`: the impl \
              method follows its TYPE's rename, and `rewrite_module_refs` rewrites the impl \
              HEAD, so one link has one key. The row also runs the composition RFC-0092 M3 \
              and this milestone each half of: `Json` declares `Copy` as well, and a copy \
              shares nothing, so the tree and its copy are released once each. It builds one \
              object of two ~900-byte Strings a turn, copies it, and emits both — a leak \
              grows it, and a double free traps rather than reading steady, which is why \
              `examples/copy.vyrn` runs the same shape on three engines",
    },
    Row {
        export: "exprTemporary",
        census: "RFC-0096 M3",
        today: Shape::Steady,
        why: "RFC-0096 M3: a String an EXPRESSION allocated has no binding, so `own` — which \
              keys every release on a `let` — had nothing to write a row against. `\"n\" + \
              i.toString()` leaked the `@str` result at every turn of a loop, and so did \
              every hole of an interpolation, because `\"a\\{x}b\\{y}\"` folds left into \
              nested `@concat`s and only the outermost result ever reaches a name. The \
              consumer is the only place that knows the temporary exists and knows it is \
              finished with, so the release goes there: `@concat`, a String `+`, `@str` \
              and the in-place append each free an operand the expression itself \
              allocated. Safe because all four COPY out of their operands, and because \
              `@str` and `@concat` cannot be shadowed — the lexer produces no leading \
              `@`, which is the argument `ban_append_expr` already stands on. Inside a \
              `region` the buffer is the arena's and this stands aside, the way the \
              block-exit release does. En route it settled a DIVERGENCE: `@str` of a \
              String was the identity on the direct backend and a strdup on the textual \
              one, so a lone hole — `let t = \"\\{s}\"`, no literal piece and therefore \
              no `@concat` above it — released one buffer twice on wasm and copied on \
              native. Both copy now, which is what lets one rule answer for both. \
              Measured native before the fix, `\"n\" + i.toString()` in a loop: 19.9 MB \
              peak at 250,000 turns and 54.1 MB at four times that; 4.06 MB at both \
              after it. Removing any one of the four frees makes this row grow",
    },
    Row {
        export: "localAccumulator",
        census: "RFC-0096 M3, defect 3",
        today: Shape::Steady,
        why: "RFC-0096 M3 defect 3: `own`'s static-data rule read the INITIALIZER. `let mut \
              acc = \"\"` starts at a data-segment literal, so the whole binding answered \
              `Fate::Static` and the heap buffer the last `acc = acc + …` left was never \
              freed — the opening line of every accumulator in this language. The rule asks \
              whether the binding can CHANGE now, and a `mut` one is released by its slot's \
              final value like any other. A slot that still holds the literal — a loop that \
              never runs, a branch that assigns another literal — frees nothing, because \
              `@__vyrn_str_free` reads a `cap` of 0 as static and returns. Measured on the \
              direct backend before the fix, over M3's smaller shape: 851,968 bytes after \
              500 calls and 3,211,264 after 2,000; this row's own ~900-byte turns read \
              8,323,072 against 32,899,072. It waited on the row below this one — \
              releasing a reassigned accumulator is what made a `String` returned out of a \
              `region` reachable, and that shape corrupted the native heap until the arena \
              stopped handing out a pointer 8 bytes inside its block",
    },
    Row {
        export: "callArgument",
        census: "census-call-arguments",
        today: Shape::Steady,
        why: "the call argument: a value the ARGUMENT EXPRESSION built has no binding either, \
              so `own` had nothing to write a row against and `width(tagged(seen))` leaked \
              every turn where `let s = tagged(seen)` on the line above did not. The name was \
              the whole difference — the proof was never missing, only the place to write it \
              down. `movecheck` records the argument's node address and the callee's verdict; \
              a `read` parameter that keeps nothing is released by the caller after the call, \
              and rules 2 and 3 are what make `read` mean that. The row runs BOTH halves, \
              because either alone is a bug: `wrap(tagged(seen))` hands its temporary to a \
              position `note_retention` recorded, nothing is freed at the call, and the `Twig` \
              gives it back once at block exit — free it here too and the row does not grow, \
              it double frees. Measured native over `width(label(i))` in a loop before the \
              rule: 14.62 MB at 250,000 turns and 49.12 MB at four times that, which is 48.2 \
              bytes a turn — the String header and its buffer. After it: 3.94 MB and 4.26 MB. \
              Make the retention question answer `Unknown` everywhere and this row leaks \
              again, which is the negative test. The row carries the class NEXT DOOR too: \
              `\"n\" + tagged(seen)` feeds a call result to the OPERATOR lowering rather \
              than to a call, so it was in neither this census's 1505 nor RFC-0096 M3's \
              operand class — M3 frees an operand that allocated its own value, and a call \
              result is one whose CALLEE decides. A String `+` is `@concat` written as an \
              operator, so its operands take the same verdict and the same guards. Measured \
              native over `\"n\" + label(i)` in a loop before the rule: 15.74 MB at 250,000 \
              turns and 50.25 MB at four times that; 3.95 MB and 3.57 MB after it",
    },
    Row {
        export: "regionArena",
        census: "RFC-0004 §4",
        today: Shape::Steady,
        why: "the arena. `own` answers `Leak::Region` for every dynamic String bound inside a \
              `region`, on every backend, because the arena is supposed to own it — and this \
              backend had no arena. `region_exit` bumped a counter and reclaimed nothing, on \
              the recorded argument that `malloc` here never freed either, which stopped \
              being true at M6. So the one construct built for bounded memory was the one \
              construct that made this target unbounded: an audit measured 13.4 MB native \
              against 3,664.5 MB and `out of memory` under wasmtime, for 20,000 turns of a \
              concatenation loop inside a region — and after the arena, 27.7 MB and a clean \
              exit. `region_keep` records what a lexically-inside-a-region expression \
              allocated, `rt.region_free` hands the frame's blocks back at the closing brace, \
              and `rt.region_pop` leaves them alone on the one edge that carries one out. \
              Lexical routing, like the textual backend's: routing on the RUNTIME depth would \
              put a callee's String in a caller's arena, where the escape guard never looked. \
              Take `region_keep` out and this row leaks",
    },
    Row {
        export: "regionCopy",
        census: "RFC-0004 §4, the routing",
        today: Shape::Steady,
        why: "the arena's SET. The row above proves the arena reclaims; this one proves the two \
              backends put the same blocks in it. The textual backend routes at the ALLOCATION \
              — every `Gen::str_alloc` made while a region is open draws from the arena — and \
              this one routed at the EXPRESSION, keeping the value of a node `own::str_temporary` \
              said yes to. A `copy` is not one of those nodes and its buffer is not the node's \
              value, it is one level down, so `let t = s.copy()` inside a region was the arena's \
              natively and nobody's here: `own` answers `Leak::Region` for `t`, so the walk \
              stands off, and nothing recorded it. 400,000 turns read 17.5 MB against native's \
              3.6 MB. The routing is at the allocation on both backends now (`Fn_::str_owned`, \
              at the sites `Gen::str_alloc` is called from), and the same key partitions the \
              release side (`Fn_::rel_at`'s `Str` arm), so a block under a container is the \
              arena's at every depth rather than the arena's and the walk's at once",
    },
    Row {
        export: "regionRebind",
        census: "RFC-0004 §4, the routing's price",
        today: Shape::Leaks,
        why: "the other half of the row above, and the one place this rule is deliberately \
              inexact. A store inside a region takes no snapshot at all — it cannot, because \
              a `String` the place holds is the arena's and the snapshot would free it a \
              second time — but an `Array` buffer is never the arena's, so a container \
              reassigned inside a region hands its old buffer to nobody. BOTH backends leak \
              it identically, which is the point: a leak both engines share is a parity \
              citizen, and it was the trade for a double free. Making it exact means \
              filtering the `String` entry out of `Fn_::store_bufs` rather than refusing the \
              snapshot, on both backends at once; do that and this row flips to Steady",
    },
    Row {
        export: "consumingLoop",
        census: "U4's price, one keyword over",
        today: Shape::Steady,
        why: "RFC-0095 M3: `for x in consume xs` takes the buffer, and the row the PLACE has \
              says `Moved` — which is the truth about the place and was the end of the matter, \
              so nothing freed the buffer at all. The loop is its last owner and releases it at \
              every exit, the way M5's loop over a temporary releases its snapshot. `check_take` \
              has already refused a borrowed root and refused module state, so the value is this \
              frame's; the take is what stops the `let` from releasing it too. An early `break` \
              is safe for the reason the whole row is: the loop variable binds to this row, so a \
              body that hands ONE element on marks the row gone and the container leaks whole — \
              a row that survives to the exit is a body that kept nothing, and the release then \
              gives back the visited and the unvisited elements alike, each exactly once",
    },
    Row {
        export: "matchTemporary",
        census: "§14, one construct over",
        today: Shape::Steady,
        why: "a `match` whose scrutinee is a TEMPORARY owns what it holds, so the match \
              releases it — Phase 10a's `if let` row at the third construct that walks one. \
              An `if let` and a `for` each got a statement row and a `match` got none, \
              because a match is an EXPRESSION and there was no statement to key on; the row \
              is keyed by the match expression's own node address instead, and `movecheck` \
              writes on it whenever an arm hands the payload out. A row nothing wrote on is a \
              scrutinee the arms did not keep, and releasing it is what closes the row. \
              `match makeResult(i) { Ok(s) => s.byteLength, .. }` leaked one `Option`'s heap \
              per turn on both compiling backends and the identical `if let` did not — \
              measured native at 3,000,000 turns, 141.7 MB before and 3.6 MB after. Inside a \
              `region` the row is not written at all: the arena owns what the region \
              allocated and the exit hands it back",
    },
    Row {
        export: "keptForever",
        census: "the detector itself",
        today: Shape::Leaks,
        why: "the canary. Every row above says `Steady`, and so does a measurement that \
              stopped measuring — a driver calling nothing, exports that vanished from the \
              module, a `byteLength` read that no longer moves. This export keeps every \
              buffer it makes in module state, on purpose, so the `Leaks` arm of the \
              comparison is exercised by something. If this row ever reads `Steady`, the \
              table is not looking at the heap and none of the verdicts above mean anything. \
              It is not a defect and no phase will fix it: an array that is never emptied is \
              supposed to hold what it was given",
    },
];

/// One export per row. The strings are ~900 bytes so one leaked buffer is
/// visible against the 128 KiB the module starts with.
fn shapes_fixture() -> String {
    let pad = "x".repeat(900);
    format!(
        r#"import {{ Slots, newSlots, insert, count }} from "std/slots"
import {{ Json, JsonField, emit }} from "std/json"

let mut seen: Int64 = 0

let mut acc: String = ""

type Bump = fn(Int64) -> Int64

type Row = {{ name: String, n: Int64 }}

type Doc = {{ title: String, body: String }}

/// A type that reaches ITSELF — `Twig` holds `Array<Twig>`. The shape the
/// release walk refused to enter until RFC-0096, and the shape `std/vyx`'s
/// `VyxNode` and `std/graphql`'s `GqlSel` have.
type Twig =
    | Tip(String)
    | Fork(String, Array<Twig>)

/// A record that merely REACHES the self-referring type. It had no row either,
/// for the same missing bottom, and gets its structural one back with the
/// declaration below.
type Bough = {{ root: Twig, label: String }}

/// The declaration that IS the bottom. The walk emits a call here rather than
/// expanding, and this function makes the recursion the walk cannot. Block
/// arms (RFC-0118): `drop` stands in the arm where a generic `give` used to
/// trampoline it — and `drop` on a bare `T` is refused now, for laundering
/// the record rule.
impl Owned for Twig {{
    fn release(consume self) {{
        match consume self {{
            Tip(s) => {{
                drop s
            }}
            Fork(s, kids) => {{
                drop s
                drop kids
            }}
        }}
    }}
}}

let mut row: Row = Row {{ name: "", n: 0 }}

let mut keyed: Map<String, Int64> = [:]

/// Bytes that are not UTF-8, filled on the first call: a module-state
/// initializer may call nothing, and the point of holding them here is that a
/// `stringFromBytes` turn allocates only what the call itself allocates.
let mut bad: Array<UInt8> = []

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

/// A PREPEND in a loop, which no in-place append can serve: the old buffer is
/// not a prefix of the new one, so `s = "x" + s` must allocate and the store
/// must release what it replaced.
///
/// THE LEAK THIS PINS. The store's release was skipped whenever the new value
/// mentioned the place at all — right for `a = @push(a, i)`, which grows the old
/// buffer and hands it back, and wrong for a `+`, because `__vyrn_str_concat`
/// always calls `__vyrn_str_new` and memcpy's both operands. It stayed invisible
/// because `s = s + x` is caught by the append spine and never reaches the
/// general store. Measured before the fix: 9.9 GB over 50,000 calls of a
/// 200-iteration loop, where the append form used 4.2 MB.
export extern fn prependLoop() {{
    let mut s = ""
    let mut i = 0
    while i < 200 {{
        s = "abcdefghij" + s
        i = i + 1
    }}
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
        let f: Bump = x -> x + k
        seen = seen + f(i)
        i = i + 1
    }}
}}

/// Census U4: a heap value inside a container. Both the array's buffer and the
/// String in it are reclaimed at block exit since RFC-0092 M2.
///
/// Phase 5 left it because a release that walked elements would free the same
/// pointers twice wherever a shallow view exists — `m.keys()` handed back a
/// FRESH buffer holding the map's OWN key pointers. M2 removed the views: the
/// three builtins that manufactured one copy their elements now, so the only
/// route into an element is a store, and rule 2 refuses storing a borrow.
export extern fn elementLeak() {{
    let mut xs: Array<String> = []
    xs.push(tag() + "!")
    seen = seen + Int64(xs.length)
}}

/// Census U4 through a container that DECLARES what it owns (RFC-0090 M1).
///
/// The same heap element as `elementLeak`, in a `Slots<String>` instead of a bare
/// `Array<String>`. The slab is `mut` — `insert` takes `modify self` — so this
/// row also proves the `mut` half: before Phase 8b a `mut` binding could not take
/// a declared release at all, and every one of the five buffers stayed out.
/// RFC-0089 rule 4 through a RECORD, which is RFC-0092 M3's own row.
///
/// A record built per call, holding two ~900-byte Strings and never handed on.
/// Nothing released it until M3: `release_kind(Record)` answered `None`, so both
/// buffers leaked once per call while the identical `Option<Doc>` one line over
/// released both — one type, two verdicts, an `Option` apart.
export extern fn recordFields() {{
    let d = Doc {{ title: tag() + "t", body: tag() + "b" }}
    seen = seen + Int64(d.title.byteLength) + Int64(d.body.byteLength)
}}

/// RFC-0093 M2. `consume d.title` moves the field out; the record's release
/// walk skips it and hands back `body` alone, so the loop allocates two Strings
/// a turn and frees two. Under M1 `d` was left unreclaimed and `body` leaked
/// once per turn; under a walk that did not skip, `title` would be freed twice
/// — which this harness cannot see and parity can.
export extern fn takenField() {{
    let mut i = 0
    while i < 4 {{
        let d = Doc {{ title: tag() + "t", body: tag() + "b" }}
        let t = consume d.title
        seen = seen + Int64(t.byteLength) + Int64(d.body.byteLength)
        i = i + 1
    }}
}}

export extern fn slotsContainer() {{
    let mut s: Slots<String> = newSlots()
    let h = insert(s, tag() + "!")
    seen = seen + count(s)
}}

/// RFC-0092 M2's price, paid by M5. `m.keys()` copies its keys, and the snapshot
/// a `for` walks is a temporary — which now owns what it holds, so the loop gives
/// back one buffer and one String per key. The map is built once and kept in
/// module state, so the only allocation per call is the snapshot.
export extern fn keysLoop() {{
    if keyed.length == 0 {{
        keyed[tag() + "a"] = 1
        keyed[tag() + "b"] = 2
    }}
    for k in keyed.keys() {{
        seen = seen + Int64(k.byteLength)
    }}
}}

/// The other half of "a map takes its key": the map releases the key it does not
/// keep. Every call hands the map a freshly built key it already holds, so the
/// hit path runs every time and the map keeps nothing new. Before the release
/// the surplus key was dropped on the floor — one ~900-byte String a call, which
/// is what a histogram loop over repeated words leaks.
export extern fn mapRepeatKey() {{
    let mut m: Map<String, Int64> = [:]
    m[tag() + "r"] = 1
    m[tag() + "r"] = 2
    m[tag() + "r"] = 3
    seen = seen + m.length
    drop m
}}

/// The value half of the same rule. The map holds one key throughout and the
/// value under it is replaced twice a call, so the two ~900-byte Strings the
/// stores displace are the only allocation this can leak — and it did, because
/// the hit path stored over the old value and released nothing.
export extern fn mapReplaceValue() {{
    let mut m: Map<String, String> = [:]
    m["k" + "ey"] = tag() + "1"
    m["k" + "ey"] = tag() + "2"
    m["k" + "ey"] = tag() + "3"
    seen = seen + m.length
    drop m
}}

/// Both halves at once, through the other way a map gives an entry up. Every
/// call inserts a built key with a ~900-byte value and removes it again, so the
/// map is empty at the `drop` and the entry it dropped is the whole allocation.
export extern fn mapRemoveEntry() {{
    let mut m: Map<String, String> = [:]
    m["k" + "ey"] = tag() + "v"
    if m.remove("key") {{
        seen = seen + 1
    }}
    seen = seen + m.length
    drop m
}}

/// `args()` gives back a block a `drop` can reach. The array is empty here — a
/// page has no argv — so a hundred turns a call is a hundred pointer blocks and a
/// hundred staging blobs, and nothing else: exactly the two allocations the call
/// makes for itself.
export extern fn argsBlock() {{
    let mut i = 0
    while i < 100 {{
        let xs = args()
        seen = seen + xs.length
        i = i + 1
    }}
}}

/// A REJECTED `stringFromBytes` gives its buffer back. The bytes are module state
/// and the `Err` payload is an interned message, so the buffer the call allocates
/// for the copy is the only thing a turn can leak.
export extern fn bytesRejected() {{
    if bad.length == 0 {{
        let mut i = 0
        while i < 900 {{
            bad.push(255)
            i = i + 1
        }}
    }}
    let r = match stringFromBytes(bad) {{
        Ok(s) => s.byteLength,
        Err(e) => e.byteLength,
    }}
    seen = seen + r
}}

/// RFC-0095 M3, and the row `keysLoop` is one keyword away from. The loop TAKES
/// the array, so the binding's row says it moved and nothing else will ever free
/// it. Two ~900-byte elements and one buffer per call, and the loop hands all
/// three back at its exit.
export extern fn consumingLoop() {{
    let mut xs: Array<String> = []
    xs.push(tag() + "a")
    xs.push(tag() + "b")
    for x in consume xs {{
        seen = seen + Int64(x.byteLength)
    }}
}}

/// RFC-0004 §4. Three ~900-byte Strings a call, all of them the arena's: the
/// binding's own row says `Leak::Region`, so the closing brace is the only thing
/// that can free them. It did not, on this backend, until `region_keep` and
/// `rt.region_free` — and the numbers that measured the difference are on the
/// `regionArena` row above.
export extern fn regionArena() {{
    region {{
        let a = tag() + "a"
        let b = tag() + a
        let c = b + "!"
        seen = seen + Int64(c.byteLength)
    }}
}}

/// The same arena, asked about the block a `copy` makes rather than the one a
/// `+` makes. `own` answers `Leak::Region` for both bindings, so the walk frees
/// neither and the arena is the only owner either can have — which means a
/// routing rule that misses the copy is a leak, not a second owner. It missed
/// it: the expression-level rule read the NODE, and a copy allocates one level
/// under its node.
export extern fn regionCopy() {{
    region {{
        let s = tag() + "!"
        let t = s.copy()
        seen = seen + Int64(t.byteLength)
    }}
}}

/// The price of the same routing, paid in the other direction. Inside a region
/// `Fn_::place_owns` and `Gen::slot_owns` refuse the store snapshot outright,
/// because a `String` block is the arena's and the snapshot would free it twice.
/// The refusal is blunt: an `Array` buffer is NEVER the arena's
/// (`Gen::array_n_to_heap`), so reassigning a container inside a region hands
/// its old buffer to nobody. Both backends leak it, which is why this row is a
/// parity citizen rather than a divergence — and it is measured here so that the
/// day someone makes the refusal exact, by filtering `Fn_::store_bufs`'s `String`
/// entry instead of dropping the whole snapshot, this row flips and says so.
export extern fn regionRebind() {{
    region {{
        let mut xs: Array<Int64> = []
        let mut i = 0
        while i < 100 {{
            xs.push(i)
            i = i + 1
        }}
        xs = []
        seen = seen + xs.length
    }}
}}

/// RFC-0096. Four ~900-byte Strings a call, in a tree that refers to itself,
/// under a record that only reaches one.
///
/// Nothing released any of them until the declaration: `release_kind` answered
/// `None` for `Twig` because a structural walk of a self-referring type has no
/// bottom, and `None` for `Bough` and for `Array<Twig>` for the same reason one
/// hop away. `impl Owned for Twig` supplies the bottom — the walk emits a CALL
/// there — and the three rows above it come back with it. Removing the `impl`
/// makes this row grow; so does removing the declared stop from the guard, and
/// then only `label` is reclaimed.
export extern fn selfReferring() {{
    let mut kids: Array<Twig> = []
    kids.push(Tip(tag() + "a"))
    kids.push(Tip(tag() + "b"))
    let b = Bough {{ root: Fork(tag() + "r", kids), label: tag() + "l" }}
    seen = seen + Int64(b.label.byteLength)
}}

/// RFC-0096 M2. The declared release of a type declared in an INJECTED module,
/// reached through the reserved spelling the linker renames it to.
///
/// `toJson` below is what injects `std/json`, and the import beside it is a HAND
/// import of the same module — both link modes in one program, which is the
/// arrangement that had to have one type key rather than two. It also runs the
/// composition: `Json` declares `Copy` too, so `doc` and `mirror` are two trees
/// and each is released exactly once.
/// What INJECTS `std/json`. The loader links the module because this function
/// MENTIONS `toJson`; nothing calls it, so the row below measures the tree and
/// not the encoder.
fn jsonAnchor() -> String {{
    return toJson(Doc {{ title: "", body: "" }})
}}

/// RFC-0096 M3. A String an EXPRESSION allocated, which no binding names.
///
/// `tag()` hands back a data-segment literal and allocates nothing, so every
/// byte here is a concatenation: the two halves of `joined`, the hole that
/// renders `joined + "y"`, the copy `@str` makes of it, and the inner join of
/// the interpolation spine. Only the outermost result of each statement reaches
/// a name. The last shape is the in-place append, whose operand the fast path
/// copies in and must then release.
///
/// Removing the free in `@concat`, in the `+` lowering, or in the append makes
/// this row grow.
export extern fn exprTemporary() {{
    let joined = (tag() + "a") + (tag() + "b")
    let held = "x\{{joined + "y"}}z"
    acc = ""
    acc = acc + (tag() + "u")
    seen = seen + Int64(held.byteLength) + Int64(acc.byteLength)
}}

/// RFC-0096 M3, defect 3. A LOCAL String accumulator, which is the shape every
/// generator in `std/` is written in.
///
/// The buffer is the one the loop grew, not the literal the binding opened with,
/// and the release runs on the slot's FINAL value. Ten turns of ~900 bytes so
/// one missed release is a page rather than a rounding error. Reverting the
/// `mut` clause in `own::fate` makes this row read 8,323,072 bytes at 500 calls
/// against 32,899,072 at 2,000 — four times the calls, four times the memory,
/// which is the leak stated as the relation this file asserts.
export extern fn localAccumulator() {{
    let mut out = ""
    let mut i = 0
    while i < 10 {{
        out = out + tag()
        i = i + 1
    }}
    seen = seen + Int64(out.byteLength)
}}

export extern fn injectedJson() {{
    let mut fs: Array<JsonField> = []
    fs.push(JsonField {{ key: tag() + "k", value: JStr(tag() + "v") }})
    // Sixteen short nodes beside the two ~900-byte ones. A `Json` payload is
    // WIDE, so it travels in a heap block no Vyrn surface names, and a declared
    // release leaks one block per node unless `free_declared_boxes` runs after
    // the call (RFC-0096 defect 1). Two nodes would be 32 bytes a call and hide
    // inside a page; thirty-six are a kilobyte a call and do not.
    let mut xs: Array<Json> = []
    let mut i = 0
    while i < 16 {{
        xs.push(JStr("node"))
        i = i + 1
    }}
    fs.push(JsonField {{ key: "kids", value: JArr(xs) }})
    let doc: Json = JObj(fs)
    let mirror = doc.copy()
    seen = seen + 1
}}

/// Census §14 at the third construct that walks a temporary. Nothing leaves the
/// arms — both hand back a number — so this `match` is the scrutinee's last
/// owner and releases it. Until the row existed nothing did, and the identical
/// `optionString` one screen up did: a `match` is an expression and had no
/// statement to key a row on.
export extern fn matchTemporary() {{
    let n = match maybe(tag()) {{
        Some(s) => s.byteLength,
        None => 0,
    }}
    seen = seen + Int64(n)
}}

export extern fn returnedString() -> String {{
    return tag() + "!"
}}

fn work(n: Int64) -> Int64 {{
    return n + 1
}}

/// A task result that OWNS heap, so a dropped task has something to release
/// besides the box it sits in — and something this harness can SEE, which 8
/// bytes of box per call was not.
fn tagged(n: Int64) -> String {{
    if n < 0 {{
        return "-"
    }}
    return tag() + "!"
}}

/// A `read` parameter that keeps nothing — rules 2 and 3 refuse every way it
/// could. It reads its argument and answers a number, which is what makes the
/// caller the temporary's only owner.
fn width(s: String) -> Int64 {{
    return Int64(s.byteLength)
}}

/// The call-argument row (`rfcs/census-call-arguments.md`). The temporary
/// `tagged(seen)` builds has no binding, and `width` keeps nothing, so the
/// CALLER releases it after the call.
///
/// The other half of the rule — a position that KEEPS what it is given — is not
/// measurable here and has its own test (`the_retained_argument_is_not_freed_at_the_call`
/// in `tests/parity.rs`). A builder that puts a `read` argument into a value it
/// returns LENDS that value, so the caller may not release the value either: the
/// shape leaks whatever this rule does, which the census records as its own
/// finding and 78 sites of this corpus. A leaking shape cannot be a steady row;
/// a double free is what that test is for, and a double free is not a leak.
export extern fn callArgument() {{
    seen = seen + width(tagged(seen))
    // The class next door, and the same 48 bytes: a call result fed to a `+`.
    // It reaches the OPERATOR lowering rather than a call, so it was in neither
    // the census's 1505 nor RFC-0096 M3's operand class — M3 frees an operand
    // that ALLOCATED its own value, and `tagged(seen)` is a call whose callee
    // decides. `s` names the concatenation, so a leak here is the operand's.
    let s = "n" + tagged(seen)
    seen = seen + Int64(s.byteLength)
}}

/// Census §10, both discharges (RFC-0095 M1). The join takes the result and
/// gives the frame back; the `drop` waits, releases the result BY ITS TYPE, and
/// gives the frame back. The second one is why the row is worth ~900 bytes a
/// call: a `Task<String>` the program drops holds a String the task allocated
/// and nothing else will ever free.
export extern fn spawnFrame() {{
    let t = spawn work(seen)
    seen = t.join()
    // The FRAME on its own is 8 bytes here, which hides inside a 64 KiB page at
    // one per call — which is exactly how the old row read steady while leaking.
    // Sixty-four a call is 512 bytes a call, and the row sees that.
    let mut i = 0
    while i < 64 {{
        let u = spawn work(i)
        seen = seen + u.join()
        i = i + 1
    }}
    let d = spawn tagged(seen)
    drop d
}}

/// The CANARY, and the only row here that is meant to grow.
///
/// Every other row asserts `Steady`, which is the state a broken measurement
/// also reports: a driver that called nothing, a build whose exports vanished,
/// or a `byteLength` read that stopped moving would leave the whole table green
/// while checking nothing. So one export keeps every buffer it makes, on
/// purpose, and its row says `Leaks`. It fails if the detector stops detecting,
/// which is the half the other twenty rows structurally cannot cover.
///
/// It is not a defect and there is nothing to fix: a module-state array that is
/// never emptied is SUPPOSED to hold what it was given. That is what makes it a
/// safe canary — no later phase will ever flip this row.
let mut kept: Array<String> = []

export extern fn keptForever() {{
    kept.push(tag() + "!")
    seen = seen + Int64(kept.length)
}}

/// RFC-0114 M1: a TEMPORARY. `fresh()` returns a String the caller owns, it is
/// read for its length, and nothing binds it — so `drop_slots`, which is keyed
/// on `let`, has no row for it and nothing releases it.
///
/// The interpreter reclaims this one for free: `Val::Str` is an `Rc<String>`.
/// The compiled backends carry no refcount and rely on an analysis that is not
/// asked. Measured on the same program, 20,000 rounds: 8.5 MB interpreted
/// against 313.9 MB native.
export extern fn temporaryCall() {{
    seen = seen + Int64(fresh().byteLength)
}}

fn fresh() -> String {{
    return tag() + "!"
}}

/// RFC-0114 M2: an accumulator whose LAST value escapes. `Gen::slot_owns` asks
/// whether the binding is in `drop_slots` — the set released at block exit — and
/// one consumed into a record is not, so no assignment in its life releases what
/// it replaced. Every intermediate leaks; only the last one is given back, by
/// `b` at the end of the block.
///
/// The same loop with `acc` merely RETURNED is steady, which is what makes this
/// a defect rather than a cost: 4.2 MB against 9.9 GB over 50,000 calls.
type Held = {{ s: String }}

export extern fn escapingAccumulator() {{
    let mut acc = ""
    let mut i = 0
    while i < 8 {{
        acc = acc + tag()
        i = i + 1
    }}
    let b = Held {{ s: consume acc }}
    seen = seen + Int64(b.s.byteLength)
}}

/// RFC-0114 Rule N: a CONDITIONAL move. One branch gives the value away, the
/// other only reads it, both continue to the join — so the move checker's
/// union says "consumed" and block exit releases nothing, on the path where
/// nothing consumed anything. 215.3 MB native over 200,000 rounds of a
/// 1,000-byte value, taking the non-moving branch every time.
fn takeIt(v: consume String) -> Int64 {{
    return v.byteLength
}}

export extern fn conditionalMove() {{
    let s = tag() + tag()
    if seen < 0 {{
        seen = seen + takeIt(consume s)
    }} else {{
        seen = seen + Int64(s.byteLength)
    }}
}}

/// RFC-0114 Rule N at a MATCH join: the same asymmetry, one arm consuming and
/// one only reading, with the release on the untouched arm's edge — the arm's
/// source index instead of then/else.
export extern fn conditionalMoveMatch() {{
    let s = tag() + tag()
    seen = seen + match pick(seen) {{
        Ok(v) => takeIt(consume s) + v,
        Err(v) => Int64(s.byteLength) - v + v,
    }}
}}

fn pick(n: Int64) -> Result<Int64, Int64> {{
    if n < 0 {{ return Ok(n) }}
    return Err(n)
}}

/// RFC-0114 consume-param release: a `consume` parameter the body only READS.
/// The callee owns it; until this landed, only an explicit `drop v` released
/// it, and a body without one leaked its argument every call.
fn readOnly(v: consume String) -> Int64 {{
    return Int64(v.byteLength)
}}

export extern fn consumedParamRead() {{
    let s = tag() + tag()
    seen = seen + readOnly(consume s)
}}

/// RFC-0114 R1′ for containers: `.length` on an unnamed Array the frame owns.
fn makeNums(n: Int64) -> Array<Int64> {{
    let mut a: Array<Int64> = []
    let mut i = 0
    while i < n {{
        a.push(i)
        i = i + 1
    }}
    return a
}}

export extern fn temporaryArrayLength() {{
    seen = seen + makeNums(64).length
}}

/// RFC-0114, the last receiver case: a field of a TEMPORARY record. A heap
/// field is read out of a value NOBODY owns, so the binding takes ownership
/// (`names_a_place` stopped calling it a borrow); a scalar field is the
/// record's last observer, so the record is freed whole after the read.
type Tag = {{ label: String, n: Int64 }}

fn makeTag(i: Int64) -> Tag {{
    return Tag {{ label: tag(), n: i }}
}}

export extern fn temporaryRecordField() {{
    let x = makeTag(seen).label
    seen = seen + Int64(x.byteLength)
}}

export extern fn temporaryRecordScalar() {{
    seen = seen + makeTag(seen).n
}}

/// The CHAINED projection: the receiver of `.byteLength` is itself a heap
/// field of a record temporary, and freeing it frees only the field it read.
export extern fn temporaryChainedField() {{
    seen = seen + Int64(makeTag(seen).label.byteLength)
}}

/// RFC-0114 untake: the value is taken, the binding is provably re-established,
/// and block exit releases the FINAL value — the taken one is the callee's,
/// which `drop`s it (the language's contract for a read-only `consume`).
fn takeDrop(v: consume String) -> Int64 {{
    let n = Int64(v.byteLength)
    drop v
    return n
}}

export extern fn revivedBinding() {{
    let mut s = tag() + tag()
    seen = seen + takeDrop(consume s)
    s = tag() + tag()
    seen = seen + Int64(s.byteLength)
}}

/// RFC-0114 Rule N at an `if`-EXPRESSION join — the third join shape, same
/// asymmetry, the release under the untouched branch's value.
export extern fn conditionalMoveIfExpr() {{
    let s = tag() + tag()
    seen = seen + (if seen < 0 {{ takeIt(consume s) }} else {{ Int64(s.byteLength) }})
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

// `returnedString` is the §9a row. Nothing here names it: the module's own
// `vyrn:exports` section says the result is a String (RFC-0012 M3), so the
// wrapper decodes it — and, since RFC-0089 M3b, releases it. This row is
// therefore also the section's end-to-end test: without it the pointer comes
// back as a number and the buffer leaks.
async function after(name, calls) {
  const { exports, memory } = await runVyrn(bytes);
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
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    // 500 and 2000. `selfAppend` is quadratic in the call count (census P1), so
    // a larger N buys nothing and costs minutes.
    let n = 500;
    let mut cmd = Command::new(&node);
    cmd.arg(dir.join("drive.mjs")).arg(n.to_string());
    for r in ROWS {
        cmd.arg(r.export);
    }
    let out = cmd.output().expect("node");
    assert!(
        out.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);

    let mut lines = text.lines();
    for r in ROWS {
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("no reading for `{}`", r.export));
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

// ---------------------------------------------------------------------------
// RFC-0095 M1 / census §10 — the native half, which is where the handle lives.
// ---------------------------------------------------------------------------

// How many handles a live process holds, and how much memory one ever held. The
// two Win32 calls this file makes, because each names the resource its census
// row is about — §10 is handles, and C2.3 is bytes.
//
// `K32GetProcessMemoryInfo` answers for a process that has already exited, as
// long as the handle is still open: `Child` holds it until it is dropped. So the
// String row needs no park and no polling — run it, wait, read the peak.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetProcessHandleCount(process: *mut std::ffi::c_void, count: *mut u32) -> i32;
    fn K32GetProcessMemoryInfo(
        process: *mut std::ffi::c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

/// `PROCESS_MEMORY_COUNTERS`, in declaration order. Only `peak_working_set` is
/// read; the rest are here because the struct's size is the argument.
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged_pool: usize,
    quota_paged_pool: usize,
    quota_peak_nonpaged_pool: usize,
    quota_nonpaged_pool: usize,
    pagefile: usize,
    peak_pagefile: usize,
}

/// One program that parks TWICE: after `first` spawns, and again once `total`
/// have run. Both handle counts are then read from ONE process, so everything
/// the count holds that is not a task — the standard streams, the loader's, the
/// machine's virus scanner reading a freshly built image — is the same number
/// on both sides of the comparison and cancels out of it. Two processes cannot
/// promise that: 30 launches of this very program, sampled 40 times each,
/// answered a rock-steady 68 for a warm image and 74 for a cold one — six
/// handles of difference that has nothing to do with a task.
///
/// Each park announces itself by WRITING A FILE rather than by printing: a
/// piped stdout is block-buffered, so a line printed before the park does not
/// arrive until the process ends, and waiting for it would deadlock against the
/// process waiting for stdin. A line on stdin releases each park.
#[cfg(windows)]
fn spawn_loop_source(first: usize, total: usize, park_a: &str, park_b: &str) -> String {
    format!(
        r#"fn work(n: Int64) -> Int64 {{
    return n + 1
}}

fn park(path: String, acc: Int64) -> Int64 {{
    let parked = match writeFile(path, "spawned \{{acc}}") {{
        Ok(b) => b,
        Err(e) => false,
    }}
    if parked {{
        if let Some(line) = readLine() {{
            print(line)
        }}
    }}
    return 0
}}

fn main() -> Int64 {{
    let mut i = 0
    let mut acc = 0
    while i < {first} {{
        let t = spawn work(i)
        acc = acc + t.join()
        i = i + 1
    }}
    acc = acc + park("{park_a}", acc)
    while i < {total} {{
        let t = spawn work(i)
        acc = acc + t.join()
        i = i + 1
    }}
    acc = acc + park("{park_b}", acc)
    return 0
}}
"#
    )
}

/// The handles a parked process holds STEADILY.
///
/// A transient only ever ADDS a handle — the marker file the runtime has
/// created and not yet closed, a worker thread the last `join` released that
/// the operating system has not finished tearing down — so the smallest count
/// over a short window is the steady state, and the poll that spots the marker
/// cannot race the write that made it.
#[cfg(windows)]
fn steady_handle_count(child: &std::process::Child) -> u32 {
    use std::os::windows::io::AsRawHandle;
    (0..10)
        .map(|k| {
            if k > 0 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let mut n: u32 = 0;
            let ok = unsafe { GetProcessHandleCount(child.as_raw_handle(), &mut n) };
            assert_ne!(ok, 0, "GetProcessHandleCount failed");
            n
        })
        .min()
        .expect("ten samples")
}

/// The measurement the table above cannot make (RFC-0095 M1).
///
/// A task owns three things: a frame, a task record, and an operating-system
/// handle — a Win32 event object, or a pthread mutex and condition variable. On
/// wasm the first is all there is, so `spawnFrame` measures that one. Here the
/// handle is measured, and it is the reason the milestone was worth building:
/// RFC-0087 §10 recorded 81 bytes AND one handle per spawn, and bytes are a leak
/// a program can live with while a per-process handle ceiling is a server that
/// stops.
///
/// **A relation, not a number**, exactly as the table above asserts one: the
/// handle count does not scale with the spawn count. Before M1 it was 20,076
/// handles at 20,000 spawns and 200,076 at 200,000 — one per spawn, on the
/// nose.
///
/// The two counts come from ONE process, at two parks 18,000 spawns apart. That
/// is what makes the relation measurable rather than merely tolerable: the
/// ambient half of the count — the standard streams, the loader's handles, a
/// virus scanner's read of a freshly built image — is one number here, and it
/// subtracts. The row used to run two processes and demand their counts be
/// EQUAL, which is more precision than two launches can give: it flaked at a
/// difference of one, four times in a week, and taught its readers to re-run a
/// red gate. Against a signal of 18,000 that precision bought nothing.
///
/// Skips, loudly, without clang — the same posture this file takes for node —
/// and compiles only on Windows, because `GetProcessHandleCount` is what names
/// the resource. A pthread task leaks a mutex and a condition variable, which
/// are memory rather than a handle, and the wasm row sees the shape of that.
#[cfg(windows)]
#[test]
fn the_spawn_handles_go_back_natively() {
    use std::io::Write;
    use std::process::Stdio;

    if vyrn_codegen::toolchain::find_clang().is_none() {
        eprintln!("NOTE: no clang — RFC-0095 M1's handle release is unverified on this machine");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vyrn-spawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 2,000 spawns, then 18,000 more. An order of magnitude between the two
    // parks, and the whole program still runs in about two seconds.
    let (first, total) = (2000usize, 20_000usize);
    let parks = [dir.join("park1.txt"), dir.join("park2.txt")];
    let at = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
    let src = dir.join("spawn.vyrn");
    std::fs::write(
        &src,
        spawn_loop_source(first, total, &at(&parks[0]), &at(&parks[1])),
    )
    .unwrap();
    let exe = dir.join("spawn.exe");
    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("vyrn build");
    assert!(
        build.status.success(),
        "native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .spawn()
        .expect("run the spawn loop");
    let mut counts = Vec::new();
    for (k, marker) in parks.iter().enumerate() {
        // Wait for the park. Sixty seconds is a ceiling, not a timing
        // assumption: 20,000 spawns take about two.
        let start = std::time::Instant::now();
        while !marker.exists() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(60),
                "the spawn loop never reached park {}",
                k + 1
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        counts.push(steady_handle_count(&child));
        if k == 0 {
            // Release the first park; the second is released by closing stdin.
            let stdin = child.stdin.as_mut().expect("piped stdin");
            stdin.write_all(b"go\n").expect("release the first park");
            stdin.flush().expect("flush");
        }
    }
    drop(child.stdin.take());
    let status = child.wait().expect("wait");
    assert!(status.success(), "the spawn loop exited {status}");

    // The tolerance, and the argument for the number. The defect is one handle
    // per spawn — census §10 measured 200,076 at 200,000 — so between the two
    // parks, 18,000 spawns apart, that leak shows a difference of 18,000. This
    // gate fires at 16, which is a leak of one handle per 1,125 spawns: it
    // still catches a defect a thousand times smaller than the one it was
    // built for. And it is far above the noise it must ignore, because the
    // ambient count is shared by the two samples and cancels — 40 runs across
    // both profiles, under parallel load, moved this difference by 0 every
    // time. A wider window would weaken the gate; the single process is what
    // removed the jitter, not the 16.
    let slack = 16;
    assert!(
        counts[1] <= counts[0] + slack,
        "the handle count grew with the spawn count: {} handles at the {first}-spawn park and \
         {} at {total}, {} more for {} further spawns. A task owns an operating-system handle, \
         and RFC-0095 M1 gives it back at the one join or at the `drop` — one per spawn is \
         census §10, which measured 200,076 handles at 200,000 spawns.",
        counts[0],
        counts[1],
        counts[1] - counts[0],
        total - first
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Audit C2.3 — the native half again, and for the same reason: the wasm free
// discriminates on the ADDRESS, so this row is steady there whatever the header
// says, and only the textual backend reads a capacity to answer the question.
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn empty_string_loop_source(turns: usize, len: usize) -> String {
    let bytes = "y".repeat(len);
    format!(
        r#"fn blank() -> String {{
    return "{bytes}"
}}

fn main() -> Int64 {{
    let mut i = 0
    while i < {turns} {{
        let a = blank()
        let b = blank()
        let c = a + b
        i = i + 1
    }}
    return 0
}}
"#
    )
}

/// The peak working set a finished process ever held.
#[cfg(windows)]
fn peak_bytes(child: &std::process::Child) -> usize {
    use std::os::windows::io::AsRawHandle;
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    let mut c = ProcessMemoryCounters {
        cb: size,
        ..Default::default()
    };
    let ok = unsafe { K32GetProcessMemoryInfo(child.as_raw_handle(), &mut c, size) };
    assert_ne!(ok, 0, "K32GetProcessMemoryInfo failed");
    c.peak_working_set
}

/// An empty String built at run time is given back (audit finding C2.3).
///
/// `cap == 0` was the header's word for "static literal, never free me", and it
/// is also the capacity every empty String gets from `@__vyrn_str_new(0, 0)` —
/// an empty `join`, a `slice` to nothing, a concat of two empties. So `free`
/// read every one of them as a literal and returned. Three million empty concats
/// peaked at 88.4 MB where the same program with one-byte strings peaked at 3.2.
/// The sentinel is all ones now, which no allocation can return.
///
/// **A relation, not a number**, as every row in this file is: the loop with
/// EMPTY strings must peak where the same loop with one-byte strings peaks. That
/// is the comparison the audit made, and it is what makes the row negative — put
/// the `0` back in `static_str_global` and the empty column grows by tens of
/// megabytes while the control column does not move.
///
/// A second pair at four times the turns says it the other way: a leak scales
/// with the turn count and a steady state does not.
///
/// Windows-only and clang-only, the same posture as the handle row above. It
/// needs a real process, and `K32GetProcessMemoryInfo` is what names the bytes.
#[cfg(windows)]
#[test]
fn an_empty_string_built_at_run_time_goes_back_natively() {
    if vyrn_codegen::toolchain::find_clang().is_none() {
        eprintln!("NOTE: no clang — audit C2.3's empty-String release is unverified here");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vyrn-emptystr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 250,000 and 1,000,000. The leak was 17 bytes plus allocator overhead per
    // turn, so the smaller run already shows megabytes and neither takes a
    // second. `len` 0 is the shape under test; `len` 1 is the control, and the
    // only difference between the two programs.
    let mut peaks = Vec::new();
    for turns in [250_000usize, 1_000_000] {
        for len in [0usize, 1] {
            let stem = format!("s{turns}_{len}");
            let src = dir.join(format!("{stem}.vyrn"));
            std::fs::write(&src, empty_string_loop_source(turns, len)).unwrap();
            let exe = dir.join(format!("{stem}.exe"));
            let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
                .arg("build")
                .arg(&src)
                .arg("-o")
                .arg(&exe)
                .output()
                .expect("vyrn build");
            assert!(
                build.status.success(),
                "native build failed:\n{}",
                String::from_utf8_lossy(&build.stderr)
            );
            let mut child = Command::new(&exe).spawn().expect("run the concat loop");
            let status = child.wait().expect("wait");
            assert!(status.success(), "the concat loop exited {status}");
            peaks.push(peak_bytes(&child));
        }
    }
    let (empty_small, one_small, empty_big, one_big) = (peaks[0], peaks[1], peaks[2], peaks[3]);

    // A megabyte of slack over the control: the two programs differ by one byte
    // of string literal, so anything larger is storage that was not handed back.
    let slack = 1 << 20;
    for (turns, empty, one) in [
        (250_000, empty_small, one_small),
        (1_000_000, empty_big, one_big),
    ] {
        assert!(
            empty <= one + slack,
            "an empty String is not being freed: {turns} turns peaked at {empty} bytes with \
             `\"\"` and {one} with `\"y\"`. `cap == 0` meant `static literal` AND `empty heap \
             buffer`, and `@__vyrn_str_free` read the second as the first — audit C2.3, which \
             measured 88.4 MB against 3.2 MB at three million turns."
        );
    }
    assert!(
        empty_big <= empty_small + slack,
        "the empty-String peak grew with the turn count: {empty_small} bytes at 250,000 turns \
         and {empty_big} at 1,000,000. A steady state does not scale with the loop."
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Census §14 at a `match` — the textual backend's half of the `matchTemporary`
// row above, for the reason every native row here exists: the two backends emit
// their own release, and one of them being right proves nothing about the other.
// ---------------------------------------------------------------------------

/// A loop whose body is a statement-position `match` over a heap temporary, or
/// the identical `if let` — the control, because the `if let` has had the row
/// since Phase 10a and the `match` had none.
#[cfg(windows)]
fn match_loop_source(turns: usize, if_let: bool) -> String {
    let body = if if_let {
        "        if let Ok(s) = makeResult(i) {\n            c = c + Int64(s.byteLength)\n        }\n"
    } else {
        "        let d = match makeResult(i) {\n            Ok(s) => s.byteLength,\n            \
         Err(e) => e.byteLength,\n        }\n        c = c + Int64(d)\n"
    };
    format!(
        r#"fn makeResult(n: Int64) -> Result<String, String> {{
    if n % 2 == 0 {{
        return Ok("ok-\{{n}}")
    }}
    return Err("er-\{{n}}")
}}

fn main() -> Int64 {{
    let mut i = 0
    let mut c = 0
    while i < {turns} {{
{body}        i = i + 1
    }}
    print(c)
    return 0
}}
"#
    )
}

/// A `match` whose scrutinee is a temporary releases it (census §14, at the
/// third construct that walks one).
///
/// `own` wrote a statement row for `Stmt::IfLet` and for `Stmt::ForIn` and none
/// for `Expr::Match`, because a match is an EXPRESSION and there was no
/// statement to key on. So the two spellings of one loop had two verdicts: the
/// `if let` form freed its scrutinee every turn and the `match` form freed it
/// never. The row is keyed by the match expression's own node address now, and
/// both compiling backends release it where nothing else took it.
///
/// **A relation, not a number**, as every row in this file is — twice over. The
/// peak at four times the turns must be the peak at N, and the `match` peak must
/// be the `if let` peak, which is the comparison that names the defect. Measured
/// at 3,000,000 turns before the fix: 141.7 MB for the `match` against 3.4 MB
/// for the `if let`.
///
/// Windows-only and clang-only, the same posture as the rows above.
#[cfg(windows)]
#[test]
fn a_match_over_a_temporary_gives_the_scrutinee_back_natively() {
    if vyrn_codegen::toolchain::find_clang().is_none() {
        eprintln!("NOTE: no clang — census §14's `match` release is unverified on this machine");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vyrn-matchloop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 250,000 and 1,000,000. The leak was one `Result<String, String>`'s heap
    // per turn, so the smaller run already shows tens of megabytes.
    let slack = 1 << 20;
    let mut peaks = Vec::new();
    for if_let in [false, true] {
        for turns in [250_000usize, 1_000_000] {
            let stem = format!("ml{turns}_{if_let}");
            let src = dir.join(format!("{stem}.vyrn"));
            std::fs::write(&src, match_loop_source(turns, if_let)).unwrap();
            let exe = dir.join(format!("{stem}.exe"));
            let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
                .arg("build")
                .arg(&src)
                .arg("-o")
                .arg(&exe)
                .output()
                .expect("vyrn build");
            assert!(
                build.status.success(),
                "native build failed:\n{}",
                String::from_utf8_lossy(&build.stderr)
            );
            let mut child = Command::new(&exe)
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("run the match loop");
            let status = child.wait().expect("wait");
            assert!(status.success(), "the match loop exited {status}");
            peaks.push(peak_bytes(&child));
        }
    }
    let (m_small, m_big, i_small, i_big) = (peaks[0], peaks[1], peaks[2], peaks[3]);

    assert!(
        m_big <= m_small + slack,
        "the `match` peak grew with the turn count: {m_small} bytes at 250,000 turns and \
         {m_big} at 1,000,000. A `match` over a temporary is that value's last owner, so it \
         releases it — a steady state does not scale with the loop."
    );
    assert!(
        m_small <= i_small + slack,
        "the `match` form peaked at {m_small} bytes where the identical `if let` form peaked \
         at {i_small}. One loop, two spellings, and only one of them freed its scrutinee — \
         census §14, which `own` answered for `Stmt::IfLet` and `Stmt::ForIn` and not for \
         `Expr::Match`."
    );
    assert!(
        i_big <= i_small + slack,
        "the `if let` control itself grew: {i_small} bytes at 250,000 turns and {i_big} at \
         1,000,000. The control is what makes the comparison above mean anything."
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// RFC-0028 — a map entry the map gives up, on the textual backend. The wasm
// half is two rows in the table above; this is the same two defects measured
// where `mapRepeatKey` cannot reach, because the two backends emit their own
// release and one of them being right proves nothing about the other.
// ---------------------------------------------------------------------------

/// A loop that replaces the value under ONE key, and optionally removes the
/// entry each turn. The key is built rather than written (`"k" + "ey"` is a heap
/// String; a literal lives in the data segment and is never freed), so the entry
/// this map gives up owns two buffers, not one.
#[cfg(windows)]
fn map_churn_source(turns: usize, remove: bool) -> String {
    let pad = "z".repeat(200);
    let rm = if remove {
        "        m.remove(\"key\")\n"
    } else {
        ""
    };
    format!(
        r#"fn main() -> Int64 {{
    let mut m: Map<String, String> = [:]
    let mut i = 0
    while i < {turns} {{
        m["k" + "ey"] = "{pad}\{{i}}"
{rm}        i = i + 1
    }}
    print(m.length)
    return 0
}}
"#
    )
}

/// A map hands back the entry it gives up — the value a store replaces, and the
/// key AND the value a `remove` drops (RFC-0028).
///
/// Two defects, one shape. `m[k] = v` over a key the map already holds stored
/// the new value over the old one and released nothing: the key half of that
/// rule was fixed one line below and the value half was missed, so
/// `Map<String, String>` leaked the previous String on every repeat.
/// `m.remove(k)` released neither half — `__vyrn_map_remove_at` is handed two
/// strides and no types, so it can only shift pointers, and the call site never
/// picked the obligation up.
///
/// **A relation, not a number**, as every row in this file is: the peak at
/// 800,000 turns must be the peak at 200,000. Both loops keep exactly one entry
/// (or none), so nothing about them scales except what is not handed back.
///
/// The measured numbers at 200,000 turns, before and after: 12.99 MB → 3.26 MB
/// for the store, 19.48 MB → 3.26 MB for the store-and-remove. Unbounded either
/// way — a histogram loop and a cache eviction loop are both this program.
///
/// Windows-only and clang-only, the same posture as the two rows above.
#[cfg(windows)]
#[test]
fn a_map_entry_the_map_gives_up_goes_back_natively() {
    if vyrn_codegen::toolchain::find_clang().is_none() {
        eprintln!("NOTE: no clang — RFC-0028's entry release is unverified on this machine");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vyrn-mapchurn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 200,000 and 800,000. The leak was one ~200-byte value per turn (plus the
    // key on the remove path), so the smaller run already shows megabytes and
    // neither takes a second.
    let slack = 1 << 20;
    for (what, remove) in [("a store over an existing key", false), ("a remove", true)] {
        let mut peaks = Vec::new();
        for turns in [200_000usize, 800_000] {
            let stem = format!("m{turns}_{remove}");
            let src = dir.join(format!("{stem}.vyrn"));
            std::fs::write(&src, map_churn_source(turns, remove)).unwrap();
            let exe = dir.join(format!("{stem}.exe"));
            let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
                .arg("build")
                .arg(&src)
                .arg("-o")
                .arg(&exe)
                .output()
                .expect("vyrn build");
            assert!(
                build.status.success(),
                "native build failed:\n{}",
                String::from_utf8_lossy(&build.stderr)
            );
            let mut child = Command::new(&exe)
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("run the map churn loop");
            let status = child.wait().expect("wait");
            assert!(status.success(), "the map churn loop exited {status}");
            peaks.push(peak_bytes(&child));
        }
        assert!(
            peaks[1] <= peaks[0] + slack,
            "the peak of {what} grew with the turn count: {} bytes at 200,000 turns and {} at \
             800,000. The map takes the key and the value, so the map hands both back when it \
             gives the entry up — a store releases the value it replaces and a `remove` \
             releases the key and the value it drops. A steady state does not scale with the \
             loop; this one held one entry throughout.",
            peaks[0],
            peaks[1]
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
