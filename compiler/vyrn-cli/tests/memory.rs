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
//! | `spawnFrame` | §10 | steady | RFC-0095 M1: a task is linear, and both discharges give its storage back |
//! | `consumingLoop` | U4's price, one keyword over | steady | RFC-0092 M5's row for `for x in consume xs`: the loop is the buffer's last owner, so it releases it at every exit |
//! | `selfReferring` | RFC-0096 | steady | a type that reaches ITSELF is released by DECLARATION — the walk emits a call at the `impl Owned`, which is the bottom it lacked, and every type above it gets its structural row back |
//!
//! **Seventeen rows, seventeen steady.** RFC-0092 M5 closed the last leaking
//! one, and the row beside it — the same statement with `consume` written on it
//! — was not in the census at all. RFC-0096 closed the last CLASS: the corpus
//! reading "the type has no release rule" fell from 63 to 0 on two `impl`s.
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

fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    Command::new(&node)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
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
    let job = spawn score(2)
    let doubled = job.join()
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
    has("held             NOT reclaimed — a lambda or a spawn captures it at line");
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
              back every element and then the five buffers. Three refusals had to move for it — \
              a `mut` binding may take a declared release (the interpreter reads the slot now), \
              a generic impl carries a row (the drop site solves the type arguments and asks for \
              the instance), and `drop v` where `v: T` checks, because the instance decides. U4 \
              opens for a container that knows what it owns, and stays open for one that cannot",
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
];

/// One export per row. The strings are ~900 bytes so one leaked buffer is
/// visible against the 128 KiB the module starts with.
fn shapes_fixture() -> String {
    let pad = "x".repeat(900);
    format!(
        r#"import {{ Slots, newSlots, insert, count }} from "std/slots"

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

/// One helper for every payload the release hands back, because a `match` arm
/// is an expression and `drop` is a statement. `drop v` reads the row the
/// INSTANCE has, so this one function releases a `String` and an `Array<Twig>`.
fn give<T>(v: consume T) -> Int64 {{
    drop v
    return 0
}}

/// The declaration that IS the bottom. The walk emits a call here rather than
/// expanding, and this function makes the recursion the walk cannot.
impl Owned for Twig {{
    fn release(consume self) {{
        let given = match consume self {{
            Tip(s) => give(s),
            Fork(s, kids) => give(s) + give(kids),
        }}
    }}
}}

let mut row: Row = Row {{ name: "", n: 0 }}

let mut keyed: Map<String, Int64> = [:]

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

// How many handles a live process holds. The one Win32 call this file makes,
// because it names the resource census §10 is about.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetProcessHandleCount(process: *mut std::ffi::c_void, count: *mut u32) -> i32;
}

#[cfg(windows)]
fn spawn_loop_source(turns: usize, marker: &str) -> String {
    format!(
        r#"fn work(n: Int64) -> Int64 {{
    return n + 1
}}

fn main() -> Int64 {{
    let mut i = 0
    let mut acc = 0
    while i < {turns} {{
        let t = spawn work(i)
        acc = acc + t.join()
        i = i + 1
    }}
    let parked = match writeFile("{marker}", "spawned \{{acc}}") {{
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
"#
    )
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
/// handle count after 8,000 spawns equals the count after 2,000. Before M1 it
/// was 20,076 handles at 20,000 spawns and 200,076 at 200,000 — one per spawn,
/// on the nose.
///
/// The program parks on `readLine()` after its loop, so the count is read at a
/// synchronisation point rather than by polling a process that may already have
/// exited. Closing its stdin lets it finish. It announces the park by WRITING A
/// FILE rather than by printing: a piped stdout is block-buffered, so a line
/// printed before the park does not arrive until the process ends, and waiting
/// for it would deadlock against the process waiting for stdin.
///
/// Skips, loudly, without clang — the same posture this file takes for node —
/// and compiles only on Windows, because `GetProcessHandleCount` is what names
/// the resource. A pthread task leaks a mutex and a condition variable, which
/// are memory rather than a handle, and the wasm row sees the shape of that.
#[cfg(windows)]
#[test]
fn the_spawn_handles_go_back_natively() {
    use std::os::windows::io::AsRawHandle;
    use std::process::Stdio;

    if vyrn_codegen::toolchain::find_clang().is_none() {
        eprintln!("NOTE: no clang — RFC-0095 M1's handle release is unverified on this machine");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vyrn-spawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 2,000 and 8,000. The leak was one handle per spawn, so one order of
    // magnitude between the two runs is unmistakable and neither takes a second.
    let mut counts = Vec::new();
    for turns in [2000usize, 8000] {
        let marker = dir.join(format!("ready{turns}.txt"));
        let src = dir.join(format!("spawn{turns}.vyrn"));
        std::fs::write(
            &src,
            spawn_loop_source(turns, &marker.display().to_string().replace('\\', "/")),
        )
        .unwrap();
        let exe = dir.join(format!("spawn{turns}.exe"));
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
        // Wait for the park. Sixty seconds is a ceiling, not a timing
        // assumption: 8,000 spawns take well under a second.
        let start = std::time::Instant::now();
        while !marker.exists() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(60),
                "the spawn loop never reached its park at {turns} turns"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let mut n: u32 = 0;
        let ok = unsafe { GetProcessHandleCount(child.as_raw_handle(), &mut n) };
        assert_ne!(ok, 0, "GetProcessHandleCount failed");
        counts.push(n);

        drop(child.stdin.take());
        let status = child.wait().expect("wait");
        assert!(status.success(), "the spawn loop exited {status}");
    }

    assert_eq!(
        counts[0], counts[1],
        "the handle count grew with the spawn count: {} handles after 2,000 spawns, {} after \
         8,000. A task owns an operating-system handle, and RFC-0095 M1 gives it back at the \
         one join or at the `drop` — one per spawn is census §10, which measured 200,076 \
         handles at 200,000 spawns.",
        counts[0], counts[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
