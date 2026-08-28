//! The harness both corpus test tiers share: where the examples are, how a
//! backend process is run (cwd, stdin fixture, argv fixture, fixed clock/seed),
//! how its output is normalized for comparison, and which examples do not
//! participate.
//!
//! It lived here rather than in `parity.rs` because RFC-0077's burndown ladder
//! was a second tier (`directwasm.rs`) making the same comparison against the
//! same corpus, and a second copy of these conventions would have drifted. The
//! ladder reached 87 of 87 and M5 folded the tier into `parity.rs`, so there is
//! one caller again — but the conventions stay here, because the reason they
//! were worth separating is that a tier disagreeing about what "the same run"
//! means is how a number stops being about the thing it names.

#![allow(dead_code)] // each tier uses a subset; the other's half is not dead.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Examples currently expected to diverge, with the reason. Shrink this list —
/// never grow it silently. (Empty since trap unification: every trap prints
/// the same `error: ...` bytes to stderr in both backends.)
pub const KNOWN_DIVERGENT: &[(&str, &str)] = &[];

/// Examples that are INTENTIONAL compile errors — they demonstrate a diagnostic
/// (e.g. compile-time validation of a provably-invalid constant) and never
/// build, so they can't participate in run-time parity. They are excluded from
/// the parity loop and instead asserted to fail `vyrn check` by
/// [`expected_check_failures_do_fail`]. This is distinct from KNOWN_DIVERGENT
/// (which is about interp/native divergence of programs that DO run).
///
/// The third field is a substring the diagnostic must contain. It exists because
/// the assertion used to hard-code `does not satisfy` — fine while every entry
/// was a validation failure, and a silent hole the moment a second kind of
/// refusal was pinned here.
pub const EXPECTED_CHECK_FAILURE: &[(&str, &str, &str)] = &[
    (
        "copyfromowned.vyrn",
        "RFC-0115: `copyFrom` overwrites by bytes, so an owning element that is          overwritten would never be released",
        "overwrites the receiver's elements by bytes",
    ),
    (
        "appendowned.vyrn",
        "RFC-0115: `append` is a byte copy, so an element type that owns heap          is refused — copying one by bytes gives two arrays one buffer",
        "copies its source's elements by bytes",
    ),
    (
        "floatkey.vyrn",
        "RFC-0117: float keys are refused by name — NaN != NaN breaks the          reflexivity a key needs",
        "does neither well",
    ),
    (
        "heapkey.vyrn",
        "RFC-0117 M1: a Map key is String or Int64 today; the other Hashable          scalars and user types are M2's",
        "is `String` or `Int64` today",
    ),
    (
        "blockvalarm.vyrn",
        "RFC-0118: a match used as a value keeps single-expression arms; a          block arm exists only in statement position",
        "a block arm needs statement position",
    ),
    // ---- RFC-0114's proof appendix, §45: the four load-bearing assumptions,
    // pinned as refusals. Each row is one assumption of the release-algorithm
    // proofs (rfcs/proofs/release-algorithm.md); the needle is the checker's
    // own wording, quoted in §45. If any of these ever COMPILES, a theorem's
    // hypothesis just became false and the proof document is the first thing
    // to reread.
    (
        "a1_afterjoin.vyrn",
        "A1's reach: a read AFTER the join of a conditional move (Theorem 4's          first case). The branch-disjoint read is accepted; this is not",
        "used again here",
    ),
    (
        "a2_capture.vyrn",
        "A2's capture clause: a closure over a `read` parameter (Lemma 3's          bracketing dies if this compiles)",
        "may not be captured by a closure",
    ),
    (
        "a2_capture_escape.vyrn",
        "A2, the escaping form: the capturing closure is returned",
        "may not be captured by a closure",
    ),
    (
        "a6_reassign.vyrn",
        "A6: parameters are not reassignable — borrows are path-invariant          because nothing can overwrite one",
        "cannot assign to",
    ),
    (
        "excl_alias.vyrn",
        "A7: a `modify` borrow is exclusive — one variable as `modify` and          `read` in one call is refused",
        "borrow is exclusive",
    ),
    (
        "validate_compile.vyrn",
        "compile-time rejection of a provably-invalid constant",
        "does not satisfy",
    ),
    (
        "polyrecursion.vyrn",
        "polymorphic recursion — a generic that calls itself with a bigger type has no \
         monomorphization fixed point (audit A5.2). `check` used to say `ok` and `build` \
         then ran forever printing nothing",
        "past the instantiation limit",
    ),
    (
        "protocol_overlap.vyrn",
        "two impls of one protocol for one type constructor (RFC-0080 M1)",
        "collides with `impl<T> Show for Option<T>` (line",
    ),
    (
        "assoctype_unbound.vyrn",
        "an impl that omits an associated type the protocol declares, and one that \
         binds a name it does not (RFC-0080 M2)",
        "does not bind the associated type `Output`",
    ),
    (
        "protocol_conformance.vyrn",
        "an impl whose methods do not have the signatures the protocol declared — a \
         wrong return type and a wrong parameter type (RFC-0002 §5)",
        "it declares `fn area(self) -> Int64`, this provides `fn area(self) -> Bool`",
    ),
    (
        "protocol_incomplete.vyrn",
        "an impl missing a method its protocol declares (RFC-0002 §5) — `vyrn check` \
         used to pass this and the mangled `Shape__Sq__name` surfaced at run time",
        "does not provide `fn name(self) -> String`",
    ),
    (
        "protocol_extra.vyrn",
        "an impl providing methods its protocol does not declare (RFC-0002 §5) — a \
         typo beside the real method, and a plainly extra one; both used to compile \
         into a mangled symbol nothing could ever dispatch to",
        "provides `fn aera(self) -> Int64`, which protocol `Shape` does not declare",
    ),
    (
        "protocol_scalar.vyrn",
        "a protocol implemented for a validated scalar — the half of the old refusal \
         RFC-0084 M1 kept, and now the only one it names",
        "erases to `Int64` at run time",
    ),
    (
        "stream_abandoned.vyrn",
        "a `Stream` acquired and abandoned (RFC-0075 M1) — the `trpc#6193` shape",
        "`events` is a `Stream<Int64>` and is never disposed",
    ),
    (
        "stream_combinator_abandoned.vyrn",
        "an abandoned std/stream combinator result (RFC-0075 M2) — the obligation \
         must not launder through `map`",
        "`mapped` is a `Stream` and is never disposed",
    ),
    (
        "streammove_after.vyrn",
        "an array read after `fromArray` took it (RFC-0092 M5) — the frame used to \
         release a buffer the stream had already freed, and the native binary \
         corrupted its heap",
        "`xs` was moved here into `fromArray(..)`",
    ),
    (
        "mapkeyborrowed.vyrn",
        "a borrowed array element handed to `m[k] = v` — the map takes the key, so the \
         array and the map both released one buffer and the native binary exited 127; \
         the map LITERAL refused the same borrow all along, which made it one fact with \
         two verdicts. mapkeyowned.vyrn is the shape that runs",
        "`ks[i]` may not be stored into `m`",
    ),
    (
        "consume_borrowed.vyrn",
        "a `read` parameter handed to a `consume` parameter — the frame gave away \
         what it does not own, and the native binary exited 0xC0000374",
        "`ys` may not be passed to a `consume` parameter via `take(..)`",
    ),
    (
        "region_consume.vyrn",
        "a value a `region` allocated, handed to a `consume` parameter (RFC-0004 §4) — \
         the escape guard watched named stores, so `kept.push(s)` inside the region was \
         refused and `keep(s)`, which stores one frame down, was not; the native binary \
         printed freed memory that changed from run to run",
        "cannot hand a heap value to argument 1 of `keep`, which is `consume`, inside a \
         `region`",
    ),
    (
        "task_abandoned.vyrn",
        "a `Task` acquired and abandoned, one joined twice, and one discharged on a \
         single branch (RFC-0095 M1) — the three refusals a `Stream` gets, over the \
         type that leaks an operating-system handle rather than bytes",
        "`t` is a `Task` and is never disposed",
    ),
    (
        "mustuse_abandoned.vyrn",
        "a USER type's must-use obligation, abandoned (RFC-0086 M3) — the same three \
         rejections `Stream` gets, reached through `impl MustUse for Txn` and naming \
         the user's type",
        "`a` is a `Txn` and is never disposed",
    ),
    (
        "gentablefail.vyrn",
        "a GENERATOR's own error, at the line and column of the input file it read \
         (RFC-0099) — the rule is `lib/gen_table.vyrn`'s, in Vyrn, and the compiler \
         knows nothing about tables",
        "data/dupe.tbl:4:1: column `id` is declared twice",
    ),
];

/// Examples whose behavior is HOST-PROVIDED (RFC-0012 `extern`): only a browser
/// page supplies the `vyrn` import namespace, so three-way output parity cannot
/// apply — wasmtime provides WASI, not `vyrn`. Excluded from the parity loop;
/// instead [`wasm_only_examples_trap_identically`] asserts the decided
/// non-wasm semantics: interp and native both produce the canonical
/// `error: extern `name` is not available on this target` trap, byte-identical
/// to each other. The real browser behavior is exercised by `web/externdemo.html`.
/// KNOWN_DIVERGENT stays empty — this list is about *hosts*, not divergence.
///
/// The cost of that exclusion is on record: because nothing here ever *built* one
/// of these to wasm either, the direct backend reached 87 of 87 with no lowering
/// for an `extern` import at all. The build is pinned now, in
/// [`the_rfc_0012_host_boundary_is_named_in_the_module`], which is what an
/// exclusion from the run comparison should have cost all along.
pub const WASM_ONLY: &[(&str, &str)] = &[(
    "externdemo.vyrn",
    "calls `extern` fns; only the browser provides the `vyrn` namespace",
)];

pub fn examples_dir() -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .unwrap()
}

pub fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// A scratch directory named for this PROCESS, removed again when the test that
/// asked for it passes.
///
/// It was `%TEMP%/vyrn-<tag>` for every run of every checkout, so two parity runs
/// at once wrote each other's `fib.vyrn.exe` and the loser reported a divergence
/// it had not produced. The process id makes concurrent runs disjoint; keeping the
/// tree on failure keeps the artifact the failure is about.
pub fn scratch(tag: &str) -> Scratch {
    // The counter is for calls WITHIN a run: two tests reaching one helper with
    // the same tag would otherwise share a path again, and this one deletes.
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("vyrn-{tag}-{}-{nth}", std::process::id()));
    // A previous run of THIS pid (they are reused) left a tree that is not this
    // run's; a stale `.exe` here is a result attributed to the wrong build.
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    Scratch(path)
}

/// The directory [`scratch`] hands out. Derefs to a `Path`, so a call site reads
/// as it did when it held a `PathBuf`.
pub struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failing test panics, and its build artifacts are the evidence — so
        // the cleanup is on the passing path only.
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Where two engines' output for one stream first differs — a line number, the
/// two lines, and the lines around them. `None` when the two are byte-identical,
/// which is the only thing that passes.
///
/// The comparison stays on the bytes: this only ever composes the MESSAGE. A
/// difference the line view cannot show (a trailing newline, a stray CR that
/// `lines()` eats) is still a failure, and says so with the byte offset instead
/// — the shape `reproducible.rs` reports a differing artifact with.
///
/// What it replaces: two whole program outputs `{:?}`-escaped onto one line
/// each. The corpus's largest example is 944 lines, so finding the divergence
/// meant reading two walls of `\n`-spelled text and comparing them by eye.
pub fn first_diff(stream: &str, a_name: &str, a: &str, b_name: &str, b: &str) -> Option<String> {
    if a == b {
        return None;
    }
    let (al, bl): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let counts = format!("({a_name} {} lines, {b_name} {} lines)", al.len(), bl.len());
    let n = (0..al.len().max(bl.len())).find(|&i| al.get(i) != bl.get(i));
    let Some(n) = n else {
        // Every line is equal and the bytes are not: a trailing newline, or a CR
        // `lines()` stripped. Byte-identical INCLUDING those is the invariant.
        let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y);
        let at = at.unwrap_or(a.len().min(b.len()));
        // The byte itself, not a slice from `at`: an offset inside a multi-byte
        // character is not a `str` boundary, and this has to print either way.
        let byte = |s: &str| match s.as_bytes().get(at) {
            Some(x) => format!("{x:#04x}"),
            None => "<end of output>".to_string(),
        };
        return Some(format!(
            "  {stream}: the {} lines are equal, the bytes are not — first differs at byte {at} {counts}\n    \
             {a_name}: {}\n    {b_name}: {}\n",
            al.len(),
            byte(a),
            byte(b),
        ));
    };
    let missing = "<no such line>";
    let mut out = format!("  {stream}: first differs at line {} {counts}\n", n + 1);
    // Everything before line `n` is equal in both, so it is printed once.
    for i in n.saturating_sub(2)..n {
        out.push_str(&format!("     same {:>5} | {}\n", i + 1, al[i]));
    }
    // From here the two are their own; each engine's next two lines follow its
    // own, because a shared "context" after a divergence is a fiction.
    for (who, lines) in [(a_name, &al), (b_name, &bl)] {
        for i in n..(n + 3).min(lines.len().max(n + 1)) {
            out.push_str(&format!(
                "  {who:>7} {:>5} | {}\n",
                i + 1,
                lines.get(i).unwrap_or(&missing)
            ));
        }
    }
    Some(out)
}

pub fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// A run's stderr with COMPILE-TIME diagnostics removed (RFC-0071 M2b).
///
/// `vyrn run` compiles and runs in one process, so a load WARNING lands on the
/// same stream as the program's own output. The native and wasm columns execute
/// an artifact that was already built — they never compile, so they never warn.
/// That asymmetry is structural and it is not a parity failure: the invariant is
/// that the *program* behaves identically on all three backends, and a warning is
/// about the compile, not about the program.
///
/// Compile ERRORS need no such treatment: an example that fails to compile never
/// reaches a comparison (it is in `EXPECTED_CHECK_FAILURE`), and a trap at
/// runtime is program output, which is compared and must stay identical.
pub fn runtime_err(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut in_warning = false;
    for line in norm(bytes).split_inclusive('\n') {
        if line.contains(": warning: ") {
            in_warning = true;
            continue;
        }
        // A warning's `  note:` continuation belongs to it.
        if in_warning && line.starts_with("  note: ") {
            continue;
        }
        in_warning = false;
        out.push_str(line);
    }
    out
}

/// The fixed clock and seed the harness injects (RFC-0043) so a time/random
/// example is a byte-identical three-way parity citizen: `now()` returns exactly
/// these epoch millis and `randomSeed()` this seed, in interp/native/wasm alike
/// (each backend's shim honors the same env). `1_700_000_000_000` ms is
/// 2023-11-14T22:13:20Z.
pub const FIXED_TIME: &str = "1700000000000";
pub const FIXED_SEED: &str = "424242";

/// Run `cmd` with the RFC-0014 I/O conventions: cwd = `examples/` (so relative
/// paths in examples resolve identically under every backend) and stdin piped
/// from `examples/<name>.stdin` when that fixture exists, else closed (EOF) —
/// never inherited, so a `readLine()` example can't hang the harness. The
/// RFC-0043 fixed clock/seed are set for every backend process (native + interp
/// read them directly; the wasm run additionally forwards them into the guest —
/// see the `--env` args on the wasmtime command).
pub fn run_io(mut cmd: Command, dir: &Path, stdin_fixture: &Path) -> std::process::Output {
    cmd.current_dir(dir);
    cmd.env("VYRN_FIXED_TIME", FIXED_TIME);
    cmd.env("VYRN_FIXED_SEED", FIXED_SEED);
    if stdin_fixture.exists() {
        cmd.stdin(std::fs::File::open(stdin_fixture).expect("open stdin fixture"));
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.output().expect("run backend")
}

/// Program arguments for an example (RFC-0061): the tokens in `examples/<name>.args`,
/// ONE per line (so a token may contain spaces), trailing newline ignored. These
/// are forwarded identically to all three backends — `vyrn run <file> <args>`,
/// the native `<exe> <args>`, and `wasmtime run ... <module> <args>` — so an argv
/// example is a byte-identical parity citizen. No fixture ⇒ empty argv.
pub fn read_args(args_fixture: &Path) -> Vec<String> {
    if !args_fixture.exists() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(args_fixture).expect("read args fixture");
    text.lines().map(|l| l.to_string()).collect()
}

/// A `wasmtime` executable to run a module under, and since RFC-0077 M5 the ONLY
/// thing the wasm column depends on: `--target wasm` emits the module itself, so
/// there is no clang, no wasi sysroot and no builtins archive to discover. The
/// `wasi_sysroot`/`wasm_toolchain` pair that used to live here went with the LLVM
/// wasm path; `vyrn-codegen`'s own clang comparisons find their sysroot through
/// `toolchain::`, which the generator engine still needs.
///
/// Which wasmtime is the resolver's answer, not this harness's (RFC-0102 M4).
/// What stood here was a literal `tools/wasmtime-v<version>-x86_64-windows/
/// wasmtime.exe` — a version, an architecture and an operating system baked into
/// a test file, resolving on exactly one developer's machine and dead
/// everywhere else. The repository root now pins the version in `vyrn.json`, so the same three steps
/// every other consumer takes answer here too: `$VYRN_WASMTIME`, then the pin,
/// then the `tools/` walk.
pub fn wasmtime() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let found = vyrn_codegen::toolchain::find_wasmtime_from(&root);
    require_tools("wasmtime", "VYRN_WASMTIME", found)
}

/// Turn a missing tool from a SKIP into a failure when `VYRN_REQUIRE_TOOLS` is
/// set — [`vyrn_codegen::toolchain::require_tools`], re-exported so this
/// harness's callers keep reading `require_tools(..)`.
///
/// It moved into `vyrn-codegen` when RFC-0077's own integration tests needed the
/// same rule: `vyrn-codegen/tests/` is a second harness, and a rule with two
/// copies is a rule with two answers.
pub use vyrn_codegen::toolchain::require_tools;
