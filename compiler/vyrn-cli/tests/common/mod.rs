//! The harness both corpus test tiers share: where the examples are, how a
//! backend process is run (cwd, stdin fixture, argv fixture, fixed clock/seed),
//! how its output is normalized for comparison, and which examples do not
//! participate.
//!
//! It lives here rather than in `parity.rs` because RFC-0077's burndown ladder
//! (`directwasm.rs`) makes exactly the same comparison against exactly the same
//! corpus — a second copy of these conventions would drift, and the two tiers
//! disagreeing about what "the same run" means is the one way the ladder could
//! report a number that is not about the backend.

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
        "validate_compile.vyrn",
        "compile-time rejection of a provably-invalid constant",
        "does not satisfy",
    ),
    (
        "protocol_overlap.vyrn",
        "two impls of one protocol for one type constructor (RFC-0080 M1)",
        "collides with `impl<T> Show for Option<T>` (line",
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
pub const WASM_ONLY: &[(&str, &str)] =
    &[("externdemo.vyrn", "calls `extern` fns; only the browser provides the `vyrn` namespace")];

pub fn examples_dir() -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").canonicalize().unwrap()
}

pub fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
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
pub fn wasmtime() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::var("VYRN_WASMTIME")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| {
            Some(root.join("tools/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe"))
                .filter(|p| p.exists())
        })
}
