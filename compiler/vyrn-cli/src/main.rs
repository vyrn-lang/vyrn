//! `vyrn` — the Vyrn driver.
//!
//! Usage:
//!   vyrn run     [file.vyrn]            Type-check and interpret; process exits with main's value.
//!   vyrn check   [file.vyrn]            Type-check only; print "ok" or every diagnostic.
//!   vyrn emit-ir [file.vyrn]            Print textual LLVM IR to stdout.
//!   vyrn emit-wat [file.vyrn]           Print the direct wasm backend's module as WAT to stdout.
//!   vyrn emit-lowered [file.vyrn]       Print the lowered form of the root module (RFC-0101).
//!   vyrn emit-gen [file.vyrn] [--maps]  Print every synthesized generator module (RFC-0021),
//!                                       or its RFC-0073 symbol map as JSON.
//!   vyrn build   [file.vyrn] [-o out] [--target wasm] [--route wasm2c]
//!                                        Compile to a native executable (or wasm) via clang.
//!   vyrn test    [file.vyrn] [--name <substring>]
//!                                        Run the root file's `test` blocks under the interpreter.
//!   vyrn bench   [file.vyrn] [--name <substring>] [--check | --json | --compare <baseline.json> [--threshold <factor>]]
//!                                        Compile the root file's `bench` blocks NATIVE and time them
//!                                        (divan-simplified). `--check` runs each once under the
//!                                        interpreter (deterministic, no timing) — the CI face.
//!                                        `--json` emits the machine-readable report (RFC-0063).
//!                                        `--compare` runs, then flags regressions vs a baseline
//!                                        (min > baselineMin * threshold, default 1.5; exit 1 on any).
//!   vyrn serve   [file.vyrn] [--port N] [--workers N]
//!                                        Run `fn handle(req: Request) -> Response` as an HTTP host.
//!                                        `--workers N` (RFC-0025) serves in parallel — refused when
//!                                        `handle` touches module state (the isolation gate).
//!   vyrn dev     [--port N] [--workers N]
//!                                        Fullstack (RFC-0019): build the client to wasm, serve the
//!                                        server root + static assets + the browser runtimes.
//!   vyrn doc     [file|dir] [-o <dir>] [--std] [--verify]
//!                                        Generate GitHub-flavored Markdown API docs (RFC-0065):
//!                                        one `.md` per module + `index.md` (default `docs/api/`).
//!                                        `--std` documents the std library; `--verify` exits 1 on drift.
//!   vyrn new     <name>                 Scaffold a project (vyrn.json + src/main.vyrn).
//!   vyrn deps    [artifact]             Print the resolved module graph of every artifact the
//!                                        manifest declares (`artifacts`, or the `main`/`server`/
//!                                        `client` keys that are sugar for it), or of the one
//!                                        named; then a `toolchain:` section: one row per tool
//!                                        (clang, wasmtime, wasi-sysroot, wasi-builtins) with the
//!                                        path that would be used, its version, and why that path
//!                                        was chosen (RFC-0102 M3). A manifest that declares no
//!                                        artifacts prints the toolchain and says so.
//!   vyrn why     --contract <file>      Print the contract governing a module (RFC-0071)
//!   vyrn fix     [file.vyrn]            Apply the `.copy()` fixes the move diagnostics name.
//!   vyrn why     --memory <file>        Print what is reclaimed, and why not (RFC-0087 U1)
//!   vyrn why     --capability <cap> <artifact>
//!                                        Print every import chain that pulls a capability
//!                                        into an artifact's closure (RFC-0103 M3).
//!                                        and every export's status against it.
//!
//! `--deny-warnings` (or `VYRN_DENY_WARNINGS=1`) turns any load warning into a
//! failure — the switch CI opts into so a build that compiled with something
//! left to say cannot quietly pass. Without it warnings are printed and nothing
//! else changes: never an exit code, never a byte of the program's own output.
//!
//! The file argument is optional whenever a `vyrn.json` manifest (found by
//! walking up from the current directory) declares a `"main"`. The manifest's
//! `"dependencies"` map bare import specifiers to real ones.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

// The C shim and clang discovery live in `vyrn-codegen`, which both this driver
// and the excluded `vyrn-genwasm` can see (RFC-0076 M4). The wasi-sysroot and
// builtins lookups are still there, but this driver no longer needs them to
// BUILD: after RFC-0077 M5 nothing here compiles C for wasm — the generator
// engine does. `vyrn deps` reads all four to REPORT them (RFC-0102 M3).
use vyrn_codegen::toolchain::{extern_trap_stubs, find_clang, runtime_shim};

mod remote;
/// RFC-0125 M5: the WASI host `--engine wasm` runs a program's wasm under.
mod wasmrun;

/// What executes a program under `run`, `test` and `bench --check` (RFC-0125
/// §2.5). The interpreter is the default in M5's first slice; `wasm` compiles
/// through the direct backend and runs the module in the embedded wasmtime.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    Interp,
    Wasm,
}

const USAGE: &str = "usage: vyrn <run|check|fix|emit-ir|emit-wat|emit-lowered|emit-gen|build|test|bench|serve|fmt> [file.vyrn] [-o out] [--target wasm] [--native-target v1|v2|v3|v4|native] [--offline] [--deny-warnings]\n       vyrn build [file.vyrn] [-o out] [--route wasm2c]   (RFC-0125 §2.5: the same wasm `--target wasm` writes, through wasm2c and clang to a native executable; needs wabt and simde under tools/, or $VYRN_WASM2C and $VYRN_SIMDE)\n       vyrn run [file.vyrn] [args...]   (trailing args reach the program's args())\n       vyrn run --profile [file.vyrn] [args...]   (where the interpreted run spent its time, to stderr; the flag counts only BEFORE the file, so a program can have one of its own)\n       vyrn run|test|bench --check --engine interp|wasm [file.vyrn]   (RFC-0125 M5: `wasm` compiles the program with the direct backend and runs it in the embedded wasmtime; `interp` is the default. Counts only BEFORE the file, like --profile)\n       vyrn check --profile [file.vyrn]   (the same, for generation alone: `check` runs every `gen fn` and stops. Needs a cold generator cache to mean anything)\n       vyrn test [file.vyrn] [--name <substring>]\n       vyrn bench [file.vyrn] [--name <substring>] [--check | --json | --compare <baseline.json> [--threshold <factor>]]   (native timing; --check runs each once under the interpreter; --json machine-readable; --compare flags regressions)\n       vyrn serve [file.vyrn] [--port N] [--workers N]   (HTTP host; needs `fn handle(req: Request) -> Response`)\n       vyrn dev [--port N] [--workers N]   (fullstack: build client to wasm + serve server root, static, runtimes)\n       vyrn fmt [file.vyrn ...] [--check]   (canonical formatter; no files = project main + local imports)\n       vyrn fmt --from-json <file.json> [--as <Type>] [--from <module>]   (print the JSON file as VON; RFC-0097)\n       vyrn doc [file|dir] [-o <dir>] [--std] [--verify]   (Markdown API docs; default docs/api/; --verify is the drift gate)\n       vyrn fix [file.vyrn]   (apply the `.copy()` a move diagnostic names, in the file given; every other fix on the menu is a decision and is refused)
       vyrn why <file>   (a module's audience, the path segment that decided it, and every import chain that reaches it)\n       vyrn why --contract <file>   (which module contract governs a file, and every export's status against it)\n       vyrn why --memory <file>   (per binding: whether it is reclaimed, how, and the reason when it is not)\n       vyrn why --capability <fs|stdin|args|extern> <entry-or-artifact-name>   (every import chain that pulls that capability into the artifact's closure)\n       vyrn routes [file.vyrn] [--json]   (the resolved wire table: every derived, pinned, hand-written and page path the router mounts, with its source; --json attaches each route's declaration from the RFC-0073 symbol map)\n       vyrn emit-gen [file.vyrn] [--maps]   (--maps prints each generated module's RFC-0073 symbol map as JSON, one per line)\n\
       vyrn new <name> | vyrn add <specifier> [--name alias] | vyrn update [--locked] [alias] | vyrn vendor [--check] | vyrn deps [artifact]   (deps: every declared artifact's module graph, then the toolchain)\n       vyrn --version   (also -V)";

/// `--offline` flag or `VYRN_OFFLINE=1`: never touch the network; a lock+cache
/// miss is a hard error instead.
fn offline(args: &[String]) -> bool {
    args.iter().any(|a| a == "--offline") || std::env::var("VYRN_OFFLINE").is_ok()
}

/// Whether `--version` / `-V` names THIS program: only when it appears among
/// the leading options, before the subcommand or file argument. After that it
/// belongs to whatever is being run — `vyrn run app.vyrn --version` asks the
/// app's version, and trailing args reach the program's `args()`.
fn wants_version(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.starts_with('-'))
        .any(|a| a == "--version" || a == "-V")
}

/// Whether the environment forbids the network. `--offline` normalizes into
/// `VYRN_OFFLINE` before any command runs, so the env var alone is the whole
/// question — the same answer [`crate::make_resolver`] builds resolvers with.
fn env_offline() -> bool {
    std::env::var("VYRN_OFFLINE").is_ok()
}

/// `--deny-warnings` flag or `VYRN_DENY_WARNINGS=1` (RFC-0071 M2b): a load that
/// produced warnings fails instead of proceeding — the switch CI opts into so a
/// build that compiled with something left to say cannot quietly pass.
///
/// Spelled and stripped exactly like `--offline`: a global flag, normalized into
/// the environment so every nested construction sees it, and removed from the
/// argument vector before any command parses its own options.
fn deny_warnings() -> bool {
    std::env::var("VYRN_DENY_WARNINGS").is_ok()
}

/// The microarchitecture a native build is compiled for.
///
/// A curated set instead of a passthrough `-march` string, for two reasons. A
/// typo in a passthrough reaches the user as a clang error they have to decode;
/// and an arbitrary `-march` can turn on FMA behind our back, which is the one
/// thing a native build must not do silently (see `add_native_clang_flags`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NativeTarget {
    /// The bare `x86-64` baseline: SSE2 and nothing later. What every build
    /// before this flag existed got.
    V1,
    /// SSE3/SSSE3/SSE4.1/SSE4.2/POPCNT — Nehalem, 2009. The default.
    V2,
    /// AVX/AVX2/BMI/**FMA**.
    V3,
    /// AVX-512 (F/BW/CD/DQ/VL), and everything v3 has.
    V4,
    /// `-march=native`: everything *this* machine has, which on any recent CPU
    /// includes FMA. The artifact is then only guaranteed to run here.
    Native,
}

/// The values `--native-target` and `vyrn.json`'s `nativeTarget` accept, for
/// diagnostics. One list, so the error can't drift from `NativeTarget::parse`.
const NATIVE_TARGETS: &str = "v1, v2, v3, v4, native";

impl NativeTarget {
    fn parse(s: &str) -> Option<NativeTarget> {
        Some(match s {
            "v1" => NativeTarget::V1,
            "v2" => NativeTarget::V2,
            "v3" => NativeTarget::V3,
            "v4" => NativeTarget::V4,
            "native" => NativeTarget::Native,
            _ => return None,
        })
    }

    /// The `-march=` value, or `None` when there is nothing to pass.
    ///
    /// The `x86-64-vN` vocabulary is x86-64's alone: `-march=x86-64-v2` is an
    /// error on aarch64, and Apple Silicon and ARM servers are real build
    /// hosts. So the whole knob is guarded on the *target* arch, not on
    /// whether a value was written. `native` is x86-64-only here too — clang
    /// spells the AArch64 equivalent `-mcpu=native` and support has moved
    /// across releases, and this branch has no ARM host to verify on. Off
    /// x86-64 every value is inert rather than fatal, because a manifest is
    /// shared across machines: a project that pins v3 for its x86 CI still has
    /// to build on a maintainer's Mac.
    fn march(self) -> Option<&'static str> {
        if !cfg!(target_arch = "x86_64") {
            return None;
        }
        Some(match self {
            NativeTarget::V1 => "x86-64",
            NativeTarget::V2 => "x86-64-v2",
            NativeTarget::V3 => "x86-64-v3",
            NativeTarget::V4 => "x86-64-v4",
            NativeTarget::Native => "native",
        })
    }
}

/// The default when nothing selects otherwise.
///
/// v2 rather than the bare baseline because of RFC-0083's weakest census row:
/// without SSE4.1, `llvm.trunc.v4f32` has no instruction and scalarizes to four
/// `truncf` calls, leaving `F32x4.trunc` at 0.43x of C — *slower* than the Vyrn
/// it replaced. SSE4.1's `roundps` is one instruction, and the same loop goes
/// 77 ms -> 36 ms (2.1x). v2 is Nehalem, 2009; nothing that can run this
/// compiler is older.
const DEFAULT_NATIVE_TARGET: NativeTarget = NativeTarget::V2;

/// Resolve the native target for a build rooted at `root`.
///
/// `--native-target` (normalized into the environment by `real_main`, like
/// `--offline`) wins over `vyrn.json`'s `nativeTarget`, which wins over the
/// default — a one-off build must not need a manifest edit.
///
/// The manifest is looked up from the root file's directory, the same start
/// point `load_options` uses, so `vyrn build sub/proj/main.vyrn` reads the
/// manifest that governs that file rather than the one above the cwd.
fn native_target_for(root: &str) -> Result<NativeTarget, String> {
    if let Ok(v) = std::env::var("VYRN_NATIVE_TARGET") {
        // Already validated in `real_main`; a bad value here came from the
        // environment directly, so it still gets a diagnostic rather than a
        // silent fallback to a different target than the one asked for.
        return NativeTarget::parse(&v).ok_or_else(|| {
            format!("unknown VYRN_NATIVE_TARGET `{v}` (expected one of: {NATIVE_TARGETS})")
        });
    }
    let start = Path::new(root)
        .parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok());
    let Some(m) = start.and_then(|d| nearest_manifest(&d)) else {
        return Ok(DEFAULT_NATIVE_TARGET);
    };
    let Some(v) = m.native_target else {
        return Ok(DEFAULT_NATIVE_TARGET);
    };
    // Cites the key and the file, the shape RFC-0072's audience diagnostics
    // set. A misspelled target must not fall back to the default: the user
    // would ship a binary built for something other than what they wrote.
    NativeTarget::parse(&v).ok_or_else(|| {
        format!(
            "unknown `nativeTarget` `{v}` in {}/vyrn.json (expected one of: {NATIVE_TARGETS})",
            m.dir
        )
    })
}

/// Every flag a native clang invocation needs, in one place.
///
/// There are two clang invocations in this file — `bench_native` builds a
/// temporary and times it, `build` writes the artifact the user ships — and
/// they have drifted twice. First `-lm`, added to one, and CI kept failing with
/// `undefined reference to ceilf` from the other because the parity harness
/// uses the second. Then `-O2`, which only `bench_native` passed, so every
/// number RFC-0083 recorded described an optimized binary that `vyrn build`
/// never emitted. This helper now owns the codegen flags too, not just the link
/// flags, so there is nothing left at a call site to copy wrong — and
/// `bench_native` measures the target `build` ships by construction.
///
/// - `-O2`: clang's default is `-O0`.
/// - `-march`: see `NativeTarget`. Absent off x86-64.
/// - `-ffp-contract=off`: **the parity flag.** From v3 up, and on `native` on
///   any recent CPU, the machine has FMA, and clang's default `-ffp-contract=on`
///   permits fusing `a*b+c` into one instruction — which rounds once instead of
///   twice, so it is *more* accurate and therefore a different number from the
///   one the tree-walking interpreter computes. Byte-identical output across
///   interpreter, native and wasm is this project's whole invariant, so the
///   more accurate answer is still the wrong answer. Today it is belt and
///   braces: our input is textual IR carrying no `contract` fast-math flags and
///   the C shim does no float arithmetic at all, so nothing fuses even at
///   `-march=native` (verified: zero `vfmadd` in the emitted assembly at every
///   level). The flag is what keeps that true if either of those changes.
///   It costs nothing measurable — at v2 the emitted assembly is byte-identical
///   with and without it — and it is passed unconditionally rather than only
///   above v2, because aarch64's *baseline* has FMA and there is no `-march`
///   there to hang the condition on.
/// - `-Wno-override-module`: our IR carries no target triple; clang supplies
///   the target's, and we don't want the warning.
/// - `-pthread`: worker threads (RFC-0025). Win32 threads need no flag.
/// - `-lm`: RFC-0083's roundings. Below SSE4.1,
///   `llvm.ceil/floor/trunc/rint.v4f32` scalarize to `ceilf`/`floorf`/`truncf`/
///   `rintf`, which live in libm on Unix and in the UCRT — linked by default —
///   on Windows. A Windows-only check structurally cannot see this missing.
fn add_native_clang_flags(cmd: &mut Command, target: NativeTarget) {
    cmd.arg("-O2")
        .arg("-ffp-contract=off")
        .arg("-Wno-override-module");
    // VYRN_DEBUG_SYMBOLS=1: keep debug info so `llvm-symbolizer --obj=<exe>`
    // can name the rva offsets `VYRN_LEAK_CHECK=3` prints — the leak triage
    // instrument (exit-residue round thirty-one). Off by default; -g changes
    // no codegen under -O2, only the artifact's size.
    if std::env::var_os("VYRN_DEBUG_SYMBOLS").is_some() {
        cmd.arg("-g");
    }
    if let Some(march) = target.march() {
        cmd.arg(format!("-march={march}"));
    }
    if !cfg!(windows) {
        cmd.arg("-pthread");
        cmd.arg("-lm");
    }
}

fn main() -> ExitCode {
    // The loader runs generators (RFC-0021) by invoking the tree-walking
    // interpreter recursively, nested deep inside the load/parse/check call
    // chain. On Windows the default ~1 MB main-thread stack overflows on a
    // realistic generator (e.g. std/i18n compiling ICU messages). Run the whole
    // CLI on a worker thread with the interpreter's own reserve, so generation
    // has the same headroom a run does.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::interp::INTERP_STACK_BYTES)
        .spawn(|| {
            let code = real_main();
            // RFC-0125 §3 M4: the build-phase table, on the worker thread that
            // did the build — the phases are thread-local for the reason the
            // run profile's rows are. Stderr, so a piped stdout is unchanged.
            eprint!("{}", vyrn_frontend::prof::phase_table());
            code
        })
        .expect("failed to spawn the vyrn worker thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

fn real_main() -> ExitCode {
    // RFC-0076. Installed before anything can load a module, since generation
    // happens deep inside the load. `VYRN_NO_WASM_GEN=1` forces the
    // interpreter — the configuration the acceptance criteria compare against.
    #[cfg(feature = "wasm-gen")]
    if std::env::var("VYRN_NO_WASM_GEN").is_err() {
        vyrn_genwasm::install();
    }
    // RFC-0125 M3: the placer over the named core, into every plan this
    // process makes. `VYRN_NO_PLACER=1` compiles with the plan as the
    // ownership analysis alone leaves it — the configuration the probes
    // under `rfcs/probes-0125/` were measured against.
    if std::env::var("VYRN_NO_PLACER").is_err() {
        vyrn_lower::install();
    }
    let mut args: Vec<String> = std::env::args().collect();
    let is_offline = offline(&args);
    if is_offline {
        // Normalized so every later resolver construction sees it.
        std::env::set_var("VYRN_OFFLINE", "1");
    }
    args.retain(|a| a != "--offline");
    if args.iter().any(|a| a == "--deny-warnings") {
        std::env::set_var("VYRN_DENY_WARNINGS", "1");
    }
    args.retain(|a| a != "--deny-warnings");
    // `--native-target <v>`: same treatment as `--offline` — a global flag,
    // validated once here so a typo is one clear error rather than a clang
    // error, normalized into the environment, and stripped before any command
    // parses its own options.
    if let Some(i) = args.iter().position(|a| a == "--native-target") {
        let Some(v) = args.get(i + 1).cloned() else {
            eprintln!("error: --native-target needs a value (one of: {NATIVE_TARGETS})");
            return ExitCode::from(2);
        };
        if NativeTarget::parse(&v).is_none() {
            eprintln!("error: unknown --native-target `{v}` (expected one of: {NATIVE_TARGETS})");
            return ExitCode::from(2);
        }
        std::env::set_var("VYRN_NATIVE_TARGET", &v);
        args.drain(i..=i + 1);
    }
    // `emit-gen --maps` (RFC-0073 M1) — drained here, like `--offline`, so the
    // "this command takes no extra arguments" check below still holds. Never
    // out of `run`'s tail: everything past the subcommand and file is the
    // program's own `args()` (RFC-0014), and `--maps` may be one of them.
    let want_maps = args.iter().any(|a| a == "--maps");
    if args.get(1).map(|a| a.as_str()) != Some("run") {
        args.retain(|a| a != "--maps");
    }
    // `--profile` is the CLI's and not the program's, so it counts only BEFORE
    // the file — the same line `--version` draws one comment down, and for the
    // same reason: `vyrn run app.vyrn --profile` is a flag for `app.vyrn`.
    let head = args
        .iter()
        .skip(2)
        .position(|a| !a.starts_with('-'))
        .map_or(args.len(), |i| i + 2)
        .max(2.min(args.len()));
    let at = args
        .get(2.min(args.len())..head)
        .and_then(|h| h.iter().position(|a| a == "--profile"));
    let want_profile = at.is_some();
    // Removed once, so a program's own `--profile` further along survives.
    if let Some(i) = at {
        args.remove(i + 2);
    }
    // `--engine <name>` (RFC-0125 M5) counts only BEFORE the file, for the same
    // reason `--profile` does. Read after `--profile` is removed, since the
    // head is the same span and one removal shifts it.
    let head = args
        .iter()
        .skip(2)
        .position(|a| !a.starts_with('-'))
        .map_or(args.len(), |i| i + 2)
        .max(2.min(args.len()));
    let at = args
        .get(2.min(args.len())..head)
        .and_then(|h| h.iter().position(|a| a == "--engine"));
    let mut engine = Engine::Interp;
    if let Some(i) = at {
        engine = match args.get(i + 3).map(String::as_str) {
            Some("interp") => Engine::Interp,
            Some("wasm") => Engine::Wasm,
            other => {
                eprintln!(
                    "error: --engine needs `interp` or `wasm`, got {}",
                    other.map_or("nothing".to_string(), |o| format!("`{o}`"))
                );
                return ExitCode::from(2);
            }
        };
        args.drain(i + 2..i + 4);
    }
    // `--version` / `-V`, before the usage screen: the published alpha printed
    // usage and exited 2 for both, which is what a package manager reads as a
    // broken install. The string is the crate's own `version`, so the binary and
    // the release archive's `VERSION` file cannot drift — the release workflow
    // checks the tag against this line before it builds anything.
    //
    // Only BEFORE the subcommand or file argument: `vyrn run app.vyrn
    // --version` hands the flag to the program being run, whose version it is,
    // not ours.
    if wants_version(&args) {
        println!("vyrn {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let cmd = args[1].as_str();

    if cmd == "new" {
        let Some(name) = args.get(2) else {
            eprintln!("usage: vyrn new <name>");
            return ExitCode::from(2);
        };
        return scaffold(name);
    }
    if cmd == "deps" {
        return deps(args.get(2).map(|s| s.as_str()));
    }
    if cmd == "why" {
        return why_cmd(&args[2..]);
    }
    if cmd == "add" {
        return add(&args[2..], is_offline);
    }
    if cmd == "update" {
        let locked = args[2..].iter().any(|a| a == "--locked");
        let alias = args[2..].iter().find(|a| !a.starts_with('-'));
        return update(alias.map(|s| s.as_str()), locked);
    }
    if cmd == "vendor" {
        return vendor(args.get(2).is_some_and(|a| a == "--check"));
    }
    if cmd == "fmt" {
        return fmt_cmd(&args[2..]);
    }
    if cmd == "doc" {
        return doc_cmd(&args[2..]);
    }
    if cmd == "dev" {
        return dev_cmd(&args[2..]);
    }
    if cmd == "routes" {
        let json = args[2..].iter().any(|a| a == "--json");
        // The file is the FIRST positional anywhere after the subcommand, not
        // only slot 2: `vyrn routes --json app.vyrn` names a file too.
        let file = args[2..]
            .iter()
            .find(|a| !a.starts_with('-'))
            .map(|s| s.as_str());
        return routes_cmd(file, json);
    }

    // The remaining commands take an optional file; without one, the manifest
    // supplies `main`.
    let (path, rest) = match args.get(2).filter(|a| !a.starts_with('-')) {
        Some(p) => (p.clone(), &args[3..]),
        None => match manifest_main() {
            Some(p) => (p, &args[2..]),
            None => {
                eprintln!("error: no input file, and no vyrn.json with a `main` found");
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        },
    };

    if cmd == "build" {
        return build(&path, rest);
    }
    if cmd == "test" {
        // Armed before the load for the reason `run` and `check` are: a `gen fn`
        // executes while the program loads, and a `test` block is the third
        // thing in this project that only ever runs interpreted.
        if want_profile {
            vyrn_frontend::prof::start();
        }
        let code = test_cmd(&path, rest, engine);
        if want_profile {
            let rows = vyrn_frontend::prof::take();
            eprint!("{}", vyrn_frontend::prof::table(&rows, 25));
        }
        return code;
    }
    if cmd == "bench" {
        return bench_cmd(&path, rest, engine);
    }
    if cmd == "serve" {
        return serve_cmd(&path, rest);
    }
    // `run` forwards any trailing arguments to the program as `args()`
    // (RFC-0014); the other commands take no extra arguments.
    if !rest.is_empty() && cmd != "run" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let prog_args = rest.to_vec();
    let path = path.as_str();

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    // `check` profiles too, and it is the more useful of the two on a program
    // whose weight is generators: `check` runs every `gen fn` and then stops,
    // so what it reports is generation and nothing else. The gen cache has to be
    // cold for that to mean anything — a warm one makes generation free, which
    // is the point of it.
    if want_profile && cmd == "check" {
        vyrn_frontend::prof::start();
    }
    let profile_now = |code: ExitCode| -> ExitCode {
        if want_profile {
            let rows = vyrn_frontend::prof::take();
            eprint!("{}", vyrn_frontend::prof::table(&rows, 25));
        }
        code
    };

    match cmd {
        "fix" => fix_cmd(path, &source),
        // `check` has to predict `build` about the one thing `build` can fail to
        // FINISH (audit A5.2). Monomorphization is only visible while emitting,
        // so `check` emits and throws the code away, and reports the depth
        // refusal alone — every other codegen error stays `build`'s.
        "check" => profile_now(match load_program(path, &source) {
            Ok(program) => {
                let _memo = shared_desugars(&program);
                if let Err(code) = kernel_refuses(&program, path) {
                    return code;
                }
                match vyrn_codegen::check_instantiations(&program) {
                    Ok(()) => {
                        println!("ok");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
            Err(code) => code,
        }),
        "run" => {
            // Armed BEFORE the load, because the load is where generators run.
            // A `gen fn` is ordinary Vyrn executed by this same interpreter at
            // compile time, and on a generator-heavy program it is most of the
            // work — profiling only what happens after `load_program` would miss
            // it and say nothing was slow.
            if want_profile {
                vyrn_frontend::prof::start();
            }
            let program = match load_program(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            // What `check` refuses, `run` refuses, under either engine: a
            // polymorphic recursion has no finite set of instances, and the
            // interpreter running it anyway (audit A5.2) was one program with
            // two answers (RFC-0125 §3 M5). The sentence is `check`'s.
            if let Err(e) = vyrn_codegen::check_instantiations(&program) {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            // And what `check` refuses, `run` refuses: one program has one
            // answer, whichever engine runs it (RFC-0125 §3 M3, the default
            // slice). `run_wasm` asks again on its own route.
            if engine != Engine::Wasm {
                if let Err(code) = kernel_refuses(&program, path) {
                    return code;
                }
            }
            if engine == Engine::Wasm {
                return run_wasm(path, &program, &prog_args);
            }
            let out = vyrn_frontend::interp::run_with_args(&program, &prog_args);
            // The table goes to STDERR, and on the failing path too. A profile is
            // not the program's output — a run whose stdout is piped somewhere
            // must pipe the same bytes with the flag as without it — and the run
            // worth profiling is often the one that traps.
            if want_profile {
                let rows = vyrn_frontend::prof::take();
                eprint!("{}", vyrn_frontend::prof::table(&rows, 25));
            }
            match out {
                Ok(code) => {
                    // main's return value becomes the process exit code (0..=255).
                    ExitCode::from((code & 0xff) as u8)
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "emit-ir" => {
            let program = match load_program(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            match vyrn_codegen::emit(&program) {
                Ok(ir) => {
                    print!("{ir}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        // The other compiled backend's text form (RFC-0077). `emit-ir` prints
        // what `build` hands clang; this prints what `build --target wasm`
        // writes, so a property no program output can show is readable on both.
        "emit-wat" => {
            let program = match load_program(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            match vyrn_codegen::direct::wat(&program) {
                Ok(wat) => {
                    print!("{wat}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        // The form BOTH compiled backends will read (RFC-0101). `emit-ir` and
        // `emit-wat` print what one engine made of a program; this prints what
        // was decided before either of them saw it, for the root module only —
        // `why --memory`'s rule, because a linked program's imports are another
        // file's answer.
        "emit-lowered" => {
            let program = match load_program(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            let lowered = vyrn_lower::lower(&program);
            print!("{}", vyrn_lower::render(&lowered, path));
            ExitCode::SUCCESS
        }
        "emit-gen" => emit_gen(path, &source, want_maps),
        other => {
            eprintln!("unknown command `{other}` (expected run, check, fix, emit-ir, emit-wat, emit-lowered, emit-gen, build, test, bench, or serve)");
            ExitCode::from(2)
        }
    }
}

/// `vyrn emit-gen [file] [--maps]` (RFC-0021) — run every generator import the
/// file reaches and print the synthesized module source, each under a banner
/// naming its generator call site. Nothing is printed for a file with no
/// generators.
///
/// `--maps` prints each module's RFC-0073 symbol map instead of its source, one
/// compact JSON document per line, banners on stderr — so a third-party tool
/// reads JSON without reading Vyrn, and `> api.map.json` produces the sibling
/// file the RFC asks for without this command inventing a name for it. There is
/// no second artifact to invalidate: the map is an export of the module, so this
/// prints what the generator cache already holds.
fn emit_gen(path: &str, source: &str, maps: bool) -> ExitCode {
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let result = vyrn_frontend::loader::generated_modules(source, &root_key, &opts, &resolver);
    // Pins are kept even when the run itself fails — fetched is pinned. But a
    // pin the disk refused fails the command: fetched remotes must land in
    // vyrn.lock or nothing here was reproducible.
    if let Err(code) = save_lock(&resolver) {
        return code;
    }
    match result {
        Ok(mods) => {
            if mods.is_empty() {
                eprintln!("(no generator imports in {root_key})");
            }
            if maps {
                let mut any = false;
                for (banner, src) in mods {
                    if let Some(json) = vyrn_frontend::symbolmap::json_of(&src) {
                        eprintln!("// ==== {banner} ====");
                        println!("{json}");
                        any = true;
                    }
                }
                if !any {
                    eprintln!("(no generated module in {root_key} carries a symbol map)");
                }
                return ExitCode::SUCCESS;
            }
            for (banner, src) in mods {
                println!("// ==== {banner} ====");
                print!("{src}");
                if !src.ends_with('\n') {
                    println!();
                }
                println!();
            }
            ExitCode::SUCCESS
        }
        Err(diags) => {
            for d in &diags {
                let file = d.file.as_deref().unwrap_or(&root_key);
                eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
                if let Some(note) = &d.note {
                    eprintln!("  note: {note}");
                }
            }
            ExitCode::FAILURE
        }
    }
}

/// Filesystem module resolver for multi-file programs (RFC-0010): resolved
/// specifiers are normalized slash-paths relative to the root file.
struct FsResolver;

impl vyrn_frontend::loader::ModuleResolver for FsResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        remote::list_dir(resolved)
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        remote::list_dir_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        remote::gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        remote::gen_cache_put(key, value)
    }
}

/// The project context — the manifest, the lock, the caches, and the two roots a
/// toolchain binary walks up to find — is [`vyrn_frontend::manifest`]. It used
/// to live here, and the language server kept a second copy of it because a
/// binary crate is not linkable. The copy drifted: it served a cached module
/// without verifying its hash, and it accepted a `vyrn.lock` this reader
/// refuses. There is one reader now, and both programs are consumers.
use vyrn_frontend::manifest::{find as find_manifest, real_path, std_root, web_root, Manifest};

/// [`find_manifest`], with the CLI's answer to an unreadable one: say which file
/// and why, and stop. Every command reads the manifest, and none of them can do
/// the right thing without it — so the policy lives here once rather than in
/// eleven call sites that would each have to remember.
fn nearest_manifest(start: &Path) -> Option<Manifest> {
    match find_manifest(start) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

/// The manifest's `main`, resolved relative to the manifest's directory.
fn manifest_main() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let m = nearest_manifest(&cwd)?;
    let main = m.main?;
    Some(format!("{}/{main}", m.dir))
}

/// LoadOptions for a root file: std root + the nearest manifest's aliases.
fn load_options(root: &str) -> vyrn_frontend::loader::LoadOptions {
    let mut opts = vyrn_frontend::loader::LoadOptions {
        std_root: std_root(),
        ..Default::default()
    };
    let start = Path::new(root)
        .parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok());
    if let Some(m) = start.and_then(|d| nearest_manifest(&d)) {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir;
        opts.audience = m.audience;
        opts.artifacts = m.artifacts;
    }
    opts
}

/// `vyrn new <name>` — scaffold vyrn.json + src/main.vyrn + .gitignore.
fn scaffold(name: &str) -> ExitCode {
    // The name is interpolated raw into vyrn.json and src/main.vyrn; a quote,
    // a backslash or a control character would write a manifest no later
    // command can parse, with `vyrn new` long gone from the picture.
    if name.contains('"') || name.contains('\\') || name.chars().any(char::is_control) {
        eprintln!("error: project name cannot contain `\"`, `\\`, or control characters");
        return ExitCode::FAILURE;
    }
    let root = Path::new(name);
    if root.exists() {
        eprintln!("error: `{name}` already exists");
        return ExitCode::FAILURE;
    }
    let manifest = format!(
        "{{\n    \"name\": \"{name}\",\n    \"main\": \"src/main.vyrn\",\n    \"dependencies\": {{}}\n}}\n"
    );
    let main_vyrn =
        format!("fn main() -> Int64 {{\n    print(\"hello from {name}\")\n    return 0\n}}\n");
    let files: &[(&str, &str)] = &[
        ("vyrn.json", &manifest),
        ("src/main.vyrn", &main_vyrn),
        (".gitignore", "*.exe\n*.ll\n*.wasm\n*.shim.c\n"),
    ];
    for (rel, content) in files {
        let path = root.join(rel);
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("error: cannot create {}: {e}", dir.display());
                return ExitCode::FAILURE;
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    println!("created {name}/ (vyrn.json, src/main.vyrn) — try: cd {name} && vyrn run");
    ExitCode::SUCCESS
}

/// `vyrn why --contract <file>` (RFC-0071 M4) — which contract governs a module,
/// and what it has to say about every one of that module's exports.
///
/// The point of a declared convention over a scanned one is that you can ask.
/// This is that question at the command line: the role that attached the
/// contract, the contract's own site, and one line per member and per export —
/// satisfied (at which of the declared shapes), missing, defaulted, the wrong
/// shape, or unknown with a did-you-mean.
///
/// Every answer comes from `vyrn_frontend::contracts`, the same module the LSP
/// asks; the CLI only prints. Exits 1 when the file is in no role — "no contract
/// governs this" is a real answer, but not the one you asked for.
fn why_cmd(args: &[String]) -> ExitCode {
    const USAGE: &str = "usage: vyrn why <file> | vyrn why --contract <file> | \
         vyrn why --memory <file> | vyrn why --capability <fs|stdin|args|extern> <entry-or-artifact-name>";
    let mut file: Option<String> = None;
    let mut contract = false;
    let mut memory = false;
    let mut capability: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--contract" => {
                contract = true;
                i += 1;
            }
            "--memory" => {
                memory = true;
                i += 1;
            }
            "--capability" => {
                let Some(cap) = args.get(i + 1) else {
                    eprintln!(
                        "error: `--capability` needs a capability (one of: {})",
                        vyrn_frontend::floor::CAPABILITIES
                    );
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                };
                capability = Some(cap.clone());
                i += 2;
            }
            other if !other.starts_with('-') => {
                file = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("error: unknown `vyrn why` option `{other}`");
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(file) = file else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if let Some(cap) = capability {
        return why_capability(&cap, &file);
    }
    if memory {
        return why_memory(&file);
    }
    if !contract {
        return why_audience(&file);
    }
    let path = match Path::new(&file).canonicalize() {
        Ok(p) => p
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/"),
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(dir) = Path::new(&path).parent().map(|p| p.to_path_buf()) else {
        eprintln!("error: {file} has no directory");
        return ExitCode::from(2);
    };
    let opts = load_options(&path);

    // The app root: the nearest `vyrn.json` upward, else the file's own
    // directory. Roles hang off a project, so a loose file simply has none.
    let manifest = nearest_manifest(&dir);
    let app_dir = manifest
        .as_ref()
        .map(|m| PathBuf::from(&m.dir))
        .unwrap_or_else(|| dir.clone());
    let roots = contract_roots(&app_dir, manifest.as_ref());
    // The declared roles come from the manifest this command already read, not
    // from a second read of the same file: two readers of one file are two
    // policies whenever one of them fails.
    let roles = match manifest
        .as_ref()
        .map(|m| vyrn_frontend::contracts::roles_from_manifest(&m.doc))
        .filter(|r| !r.is_empty())
    {
        Some(declared) => declared,
        None => vyrn_frontend::contracts::discovered_roles(&roots, &opts, &FsResolver),
    };
    let Some(role) = vyrn_frontend::contracts::role_for(&path, &roles) else {
        println!("{path}");
        println!("  no contract: this file is in no role");
        if vyrn_frontend::contracts::is_projection(&path) {
            println!(
                "  (its stem is dotted: a projection written OVER the modules beside it \
                 (RFC-0074), which the generator scanning that directory skips)"
            );
        }
        if roles.is_empty() {
            println!("  (the project declares no `roles` in vyrn.json, and no generator call site names a directory containing it)");
        } else {
            for r in &roles {
                println!("  role: {} -> {}:{}", r.scope, r.module, r.contract);
            }
        }
        return ExitCode::FAILURE;
    };
    let manifest = app_dir
        .join("vyrn.json")
        .to_string_lossy()
        .replace('\\', "/");
    let Some(view) =
        vyrn_frontend::contracts::load_role_contract(role, &manifest, &opts, &FsResolver)
    else {
        eprintln!(
            "error: cannot resolve contract `{}:{}`",
            role.module, role.contract
        );
        return ExitCode::FAILURE;
    };

    // A `.vyx` is not a module; its `<script>` is. Everything downstream is the
    // same question over the same ordinary Vyrn — except its `<template>`, which
    // becomes an export of the compiled module and is in no `<script>`.
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let synthesized = vyrn_frontend::contracts::synthesized_members(&view, &path, &raw);
    let source = if path.ends_with(".vyx") {
        vyx_script_body(&raw).unwrap_or_default()
    } else {
        raw
    };

    println!("{path}");
    println!("  role: {}", role.scope);
    println!("  contract: {} ({})", view.name, view.module);
    println!("  declared in: {}", view.file);
    let mut objections = 0;
    for e in vyrn_frontend::contracts::contract_status(&view, &source, &synthesized) {
        use vyrn_frontend::contracts::MemberStatus::*;
        let line = match &e.status {
            // Not absent and not defaulted: the file's FORM writes it. A `.vyx`
            // has no other way to declare a view, so "absent, optional" was a
            // claim about every `.vyx` in the corpus.
            Synthesized => {
                format!(
                    "ok        {}: the `<template>` compiles to it — {}",
                    e.name, e.want
                )
            }
            Satisfied { shape } => {
                let of = view.member(&e.name).map(|m| m.shapes.len()).unwrap_or(1);
                format!(
                    "ok        {}: shape {} of {} — {}",
                    e.name,
                    shape + 1,
                    of,
                    e.want
                )
            }
            Defaulted => format!("default   {}: absent, optional — {}", e.name, e.want),
            Missing => {
                objections += 1;
                format!("MISSING   {}: required — {}", e.name, e.want)
            }
            Mismatched { found } => {
                objections += 1;
                format!("MISMATCH  {}: wanted {}, found `{found}`", e.name, e.want)
            }
            Unknown {
                did_you_mean: Some(near),
            } => {
                objections += 1;
                format!(
                    "UNKNOWN   {}: not named by the contract — did you mean `{near}`?",
                    e.name
                )
            }
            Unknown { did_you_mean: None } => {
                objections += 1;
                format!(
                    "UNKNOWN   {}: not named by the contract (it is closed)",
                    e.name
                )
            }
            OpenMatched => format!("ok        {}: matches the open rule — {}", e.name, e.want),
            OpenMismatched { found } => {
                objections += 1;
                format!(
                    "MISMATCH  {}: the open rule wants {}, found `{found}`",
                    e.name, e.want
                )
            }
        };
        println!("  {line}");
    }
    if objections > 0 {
        // `why` answers a question; it is not a gate. The generator that
        // consumes the module runs the same check at load time and FAILS on it
        // — which is why a `.vyrn` page listing its router entry point here
        // (`page`/`respond`, which `Page` does not name) still builds today.
        println!(
            "  — {objections} objection(s); the generator that consumes this module is the gate"
        );
    }
    ExitCode::SUCCESS
}

/// `vyrn routes [file]` (RFC-0072 M3) — the resolved wire table, with where each
/// path came from.
///
/// A derived path is not written down anywhere, which is the point and also the
/// risk: a rule you cannot inspect is a rule you have to simulate in your head.
/// So the generator that MOUNTS the surface also emits one `//@route` directive
/// per route, and this command reads them back. There is exactly one producer of
/// the table — the same division RFC-0071 M4 drew between the LSP and
/// `vyrn_frontend::contracts` — so the printed table and the mounted router
/// cannot disagree.
///
/// The `source` column reads `convention` or `override`, so drift shows up in
/// review instead of in production.
///
/// `--json` (RFC-0073 M4) is the same table plus the thing the directive cannot
/// carry: WHERE each route's procedure is declared. That comes from M1's symbol
/// map, read with `vyrn_frontend::symbolmap` — the very reader the LSP's hover
/// and route lenses use, which is what makes the RFC's "`vyrn routes --json` and
/// the LSP agree" true by construction rather than by care.
///
/// The two channels are UNIONED rather than one replacing the other. They come
/// from the same generator over the same route list today, so the union is the
/// same set; it costs nothing and it keeps the command honest about a future
/// generator that emits only one of them.
///
/// A THIRD channel (this fix) covers what no generator can: a hand-written
/// projection's `Route`/`Live`/`Socket` list (RFC-0074). Its paths are written,
/// not derived, so the generator that mounts the derived surface never sees them
/// — the table printed three of `examples/bin`'s eight wire rows and called
/// itself "every". They are read from the values themselves, by evaluating the
/// arguments of the program's `mount(..)` call — see
/// [`vyrn_frontend::interp::mounted_routes`] for why that is the arguments and
/// not a naming convention, and what it costs.
///
/// The one-producer property SURVIVES: no channel re-derives a path. The first
/// two read what a generator wrote while mounting; the third reads the values
/// `mount` is handed. Nothing here computes a route, so no two of them can
/// disagree about one.
///
/// PAGE routes arrive on the FIRST channel, which is the whole of this fix here:
/// `std/ui` now emits a `//@route` per page and a `routes()` group `mount` takes,
/// so nothing in this command knows a page from a procedure. Two things that were
/// believed to block it turned out not to exist. A page router "always answers",
/// but its PAGES do not — only the tree's 404 does, and that is the composition
/// root's fallback, not a route. And the prefix a tree is served under was never
/// unknown: a page router matches `req.path` whole against its own segments, so a
/// tree cannot be re-hung and its patterns are already absolute — `examples/bin`'s
/// `startsWith("/raw/")` was a hand-written dispatch guard standing in front of a
/// tree that literally contains `raw/[id].vyrn`.
fn routes_cmd(file: Option<&str>, json: bool) -> ExitCode {
    let path = match file.map(|s| s.to_string()).or_else(manifest_main) {
        Some(p) => p,
        None => {
            eprintln!("error: no input file, and no vyrn.json with a `main` found");
            eprintln!("usage: vyrn routes [file]");
            return ExitCode::from(2);
        }
    };
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let source = match std::fs::read_to_string(&root_key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {root_key}: {e}");
            return ExitCode::from(2);
        }
    };
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let result = vyrn_frontend::loader::generated_modules(&source, &root_key, &opts, &resolver);
    // Same policy as load_program: pins survive a failed run, and a pin the
    // disk refuses fails this command rather than leaving the remote unpinned.
    if let Err(code) = save_lock(&resolver) {
        return code;
    }
    let mods = match result {
        Ok(m) => m,
        Err(diags) => {
            for d in &diags {
                let f = d.file.as_deref().unwrap_or(&root_key);
                eprintln!("{}:{}:{}: {}", f, d.line, d.col, d.message);
            }
            return ExitCode::FAILURE;
        }
    };
    // `(method, path, procedure, source)`, de-duplicated: a page or api
    // directory reached through two roots generates the same table twice.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for (_, src) in &mods {
        // The third scan over generated text, and it reads its control lines the
        // way the other two do: a `//@route` counts where the LEXER says a comment
        // begins. A generator copies its input through verbatim, so a string
        // literal a component author wrote could otherwise add a route to this
        // table that no procedure serves.
        let comments = vyrn_frontend::origin::comment_lines(src);
        for (i, line) in src.lines().enumerate() {
            if comments.as_ref().is_some_and(|c| !c.contains(&(i + 1))) {
                continue;
            }
            let Some(rest) = line.strip_prefix("//@route ") else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            let row = (
                f[0].to_string(),
                f[1].to_string(),
                f[2].to_string(),
                f[3].to_string(),
            );
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    // The hand-written channel. A failure here is reported and survived: the
    // derived rows above are still true, and a table that is short and says so
    // beats one that needs the program to start.
    match vyrn_frontend::load(&source, &root_key, &opts, &resolver)
        .map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default())
        .and_then(|p| vyrn_frontend::interp::mounted_routes(&p))
    {
        Ok(mounted) => {
            for r in mounted {
                // A `surface(..)` stands for a whole subsystem; the directives
                // above already list its members, one row each.
                if r.prefix {
                    continue;
                }
                let row = (r.method, r.path, r.procedure, "explicit".to_string());
                if !rows.iter().any(|x| x.0 == row.0 && x.1 == row.1) {
                    rows.push(row);
                }
            }
        }
        Err(e) => eprintln!(
            "note: only derived routes are listed — the mounted router could not be read: {e}"
        ),
    }
    if json {
        return routes_json(&mods, rows);
    }
    if rows.is_empty() {
        println!("(no derived routes in {root_key})");
        return ExitCode::SUCCESS;
    }
    // By path then method: a projection puts two methods on one path, so path
    // alone no longer orders the table.
    rows.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    let w0 = rows
        .iter()
        .map(|r| r.0.len())
        .max()
        .unwrap_or(6)
        .max("method".len());
    let w1 = rows
        .iter()
        .map(|r| r.1.len())
        .max()
        .unwrap_or(4)
        .max("path".len());
    let w2 = rows
        .iter()
        .map(|r| r.2.len())
        .max()
        .unwrap_or(9)
        .max("procedure".len());
    println!(
        "{:w0$}  {:w1$}  {:w2$}  source",
        "method", "path", "procedure"
    );
    for (method, path, proc, src) in &rows {
        println!("{method:w0$}  {path:w1$}  {proc:w2$}  {src}");
    }
    ExitCode::SUCCESS
}

/// `vyrn routes --json` (RFC-0073 M4) — the merged wire table for external
/// tooling: every route the mounting generator declared, each carrying the
/// declaration its symbol map names.
///
/// `origin` is `null` for a route whose generator emits directives but no map.
/// That is a real state and not a defect to hide — the JSON says which routes a
/// tool can follow back to a declaration and which it cannot.
fn routes_json(
    mods: &[(String, String)],
    directives: Vec<(String, String, String, String)>,
) -> ExitCode {
    /// `(method, path, procedure, source, origin)`.
    type Row = (
        String,
        String,
        String,
        String,
        Option<vyrn_frontend::symbolmap::MappedSymbol>,
    );
    let mut rows: Vec<Row> = directives
        .into_iter()
        .map(|(method, path, proc, src)| (method, path, proc, src, None))
        .collect();
    for (_, src) in mods {
        for m in vyrn_frontend::symbolmap::read(src) {
            let Some(path) = m.derived("path") else {
                continue;
            };
            let method = m.derived("method").unwrap_or("POST").to_string();
            let source = m.derived("source").unwrap_or("convention").to_string();
            match rows.iter_mut().find(|r| r.0 == method && r.1 == path) {
                // A procedure is mapped once per generator that mounts it, and
                // all of them name the same declaration at the same place — the
                // agreement M1 put under test — so the first one settles it.
                Some(row) => {
                    if row.4.is_none() {
                        row.4 = Some(m);
                    }
                }
                None => {
                    let path = path.to_string();
                    let proc = m.decl.clone();
                    rows.push((method, path, proc, source, Some(m)));
                }
            }
        }
    }
    rows.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    println!("[");
    for (i, (method, path, proc, source, origin)) in rows.iter().enumerate() {
        let comma = if i + 1 == rows.len() { "" } else { "," };
        let origin = match origin {
            Some(m) => format!(
                "{{ \"file\": {}, \"line\": {}, \"col\": {}, \"name\": {} }}",
                json_str(&m.file),
                m.line,
                m.col,
                json_str(&m.decl)
            ),
            None => "null".to_string(),
        };
        println!(
            "  {{ \"method\": {}, \"path\": {}, \"procedure\": {}, \"source\": {}, \"origin\": {} }}{comma}",
            json_str(method),
            json_str(path),
            json_str(proc),
            json_str(source),
            origin
        );
    }
    println!("]");
    ExitCode::SUCCESS
}

/// A JSON string literal. Paths reach here as the loader keyed them, which on
/// Windows can still hold a backslash, so escaping is not optional.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `vyrn why --memory <file>` (RFC-0087 U1, RFC-0089 M0) — what the ownership
/// analysis decided about every binding in a module, and why.
///
/// Three bindings of one shape have opposite outcomes today, and nothing in the
/// source says which is which. The compiler holds the exact answer and shows it
/// to nobody. This is that answer at the shell.
///
/// It is a **printer**. Every word comes out of `own::Ownership`, recorded by
/// the walker that decided — never re-derived here. A second walk over the tree
/// could disagree with the first, and the census records that defect three times.
///
/// It reports; it does not gate. Exit 0 whenever it could answer.
fn why_memory(file: &str) -> ExitCode {
    let path = match Path::new(file).canonicalize() {
        Ok(p) => p
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/"),
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    // A `.vyx` is not a module; its `<script>` is — the same split `--contract`
    // makes.
    let source = if path.ends_with(".vyx") {
        vyx_script_body(&raw).unwrap_or_default()
    } else {
        raw
    };
    let program = match load_program(&path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let own = vyrn_frontend::own::analyze(&program);

    println!("{path}");
    println!("  memory: every binding, whether it is reclaimed, and the reason when it is not");

    let mut bindings = 0usize;
    let mut reclaimed = 0usize;
    let mut moved = 0usize;
    let mut dropped = 0usize;
    let mut statics = 0usize;
    let mut discharged = 0usize;
    // Reason -> count, kept in first-seen order so the report is stable.
    let mut leaked: Vec<(&'static str, usize)> = Vec::new();

    // Only the file asked about. A linked program carries every import's
    // functions, and they are another file's answer.
    for f in program
        .functions
        .iter()
        .filter(|f| f.module.is_none() && !f.is_extern)
    {
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect();
        println!();
        println!("  fn {}({}) -> {}", f.name, params.join(", "), f.ret);
        match own.owned_fns.get(&f.name) {
            Some(kind) => println!(
                "    transfers: yes — the caller owns the result, and releases it by {}",
                kind.words()
            ),
            // RFC-0089 rule 3: a return is owned. A heap return type always
            // transfers, so the only other answer is that the type owns nothing.
            None => println!("    transfers: no — the return type {} owns no heap", f.ret),
        }
        let notes = match own.notes.get(&f.name) {
            Some(n) if !n.is_empty() => n,
            _ => {
                println!("    (no bindings)");
                continue;
            }
        };
        for n in notes {
            use vyrn_frontend::own::Fate;
            bindings += 1;
            match &n.fate {
                Fate::Reclaimed(..) => reclaimed += 1,
                Fate::Moved { .. } => moved += 1,
                Fate::Dropped { .. } => dropped += 1,
                Fate::Static => statics += 1,
                Fate::Discharged(_) => discharged += 1,
                Fate::Leaked(reason) => {
                    let key = reason.kind();
                    match leaked.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, c)) => *c += 1,
                        None => leaked.push((key, 1)),
                    }
                }
            }
            println!("    line {:<5} {:<16} {}", n.line, n.name, n.fate.words());
        }
    }

    let leaks: usize = leaked.iter().map(|(_, c)| c).sum();
    println!();
    println!(
        "  summary: {bindings} bindings — {reclaimed} reclaimed, {moved} moved out, \
         {dropped} dropped, {discharged} discharged, {statics} static, {leaks} not reclaimed"
    );
    leaked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (reason, count) in &leaked {
        println!("    {count:>5}  {reason}");
    }
    ExitCode::SUCCESS
}

/// `vyrn why <file>` (RFC-0072 M1) — the audience of a module, the path segment
/// that decided it, and every import chain that reaches it.
///
/// "What is bundled where" is the question the audience rule exists to answer,
/// and a rule you can only observe by building is a convention you have to
/// trust. This is that answer at the shell, from the same
/// `vyrn_frontend::audience` the loader enforces with — not a second reading of
/// the tree that could drift from it.
///
/// It REPORTS; it does not gate (the RFC-0071 M4 convention). Exit 0 whenever it
/// could answer, 2 only when the file cannot be read.
fn why_audience(file: &str) -> ExitCode {
    let Some(path) = real_path(file) else {
        eprintln!("error: cannot read {file}");
        return ExitCode::from(2);
    };
    let Some(dir) = Path::new(&path).parent().map(|p| p.to_path_buf()) else {
        eprintln!("error: {file} has no directory");
        return ExitCode::from(2);
    };
    let manifest = nearest_manifest(&dir);
    let app_dir = manifest
        .as_ref()
        .map(|m| PathBuf::from(&m.dir))
        .unwrap_or_else(|| dir.clone());
    let app_slash = app_dir.to_string_lossy().replace('\\', "/");
    let map = manifest.as_ref().and_then(|m| m.audience.clone());

    println!("{path}");
    // PLAN-0125-runtime §3.2: the two modules whose audience the compiler
    // declares. Decided by path identity against the std root, the way the
    // loader's fence decides it, and before the manifest is consulted because
    // no manifest has a say.
    let fenced = std_root().and_then(|root| {
        use vyrn_frontend::loader::{MEM_SPEC, RUNTIME_SPEC};
        let is = |spec: &str| {
            real_path(&format!("{root}/{}.vyrn", &spec["std/".len()..])).as_deref() == Some(&*path)
        };
        if is(MEM_SPEC) {
            Some(format!(
                "`{RUNTIME_SPEC}`, declared by the compiler (RFC-0125 §2.4)"
            ))
        } else if is(RUNTIME_SPEC) {
            Some("the compiler, which links it into every program (RFC-0125 §2.4)".to_string())
        } else {
            None
        }
    });
    match (&fenced, &map) {
        (Some(who), _) => println!("  audience: {who}"),
        (None, Some(map)) => {
            let v = vyrn_frontend::audience::audience_of(&path, map);
            println!("  audience: {} — {}", v.audience.phrase(), v.because());
        }
        (None, None) => {
            println!(
                "  audience: universal — this project declares no `audience` in vyrn.json, \
                 so every module is universal and no import is rejected"
            );
        }
    }

    // The import graph, read straight off the sources: no load, no generators,
    // no build. A file whose audience you are asking about may well be the one
    // that does not compile.
    let edges = project_imports(&app_dir);
    let chains = import_chains(&path, &edges);
    if chains.is_empty() {
        println!("  imported by: nothing in this project reaches it");
    } else {
        println!("  imported by:");
        for chain in &chains {
            let pretty: Vec<String> = chain.iter().map(|p| rel_to(p, &app_slash)).collect();
            println!("    {}", pretty.join(" -> "));
        }
    }
    ExitCode::SUCCESS
}

/// `vyrn why --capability <cap> <entry-or-artifact-name>` (RFC-0103 M3) — every
/// import chain that pulls a capability into one artifact's closure.
///
/// The floor's refusal shows ONE chain, the shortest, because a refusal has to
/// be read in a hurry. This is the other question — "where does `fs` come from
/// at all?" — and it answers with EVERY chain, because deleting a hop off the
/// shortest path removes nothing if a second path also reaches the module.
///
/// It walks the LINKED graph — `loader::capability_graph`, the same triples the
/// check refuses over. M3 read the project's files instead, so that the command
/// could answer for an artifact that does not compile, and M4 found the price:
/// a generated module is produced by the loader and is on nobody's disk, so
/// `shelf`'s client carried the `vyrnRpcCall` `extern` through its rpc stub, the
/// floor said so, and the report said `nothing … needs 'extern'`. That is the
/// one class of capability nobody can find by reading their own source, so the
/// graph is now the load's and the vocabulary, the carriers and the closure all
/// come from one place.
///
/// The load runs with the fence and the floor DISARMED. Both are refusals over
/// this graph rather than facts about it, and leaving them armed would leave the
/// command unable to answer for the tree anyone actually asks about — the one
/// that was just refused.
///
/// It REPORTS; it does not gate. Exit 0 whenever it could answer, 2 when it
/// could not — an unknown capability, or an argument that names no artifact.
fn why_capability(cap: &str, name: &str) -> ExitCode {
    use vyrn_frontend::floor::{self, Capability};
    let Some(cap) = Capability::parse(cap) else {
        eprintln!(
            "error: unknown capability `{cap}` (expected one of: {})",
            floor::CAPABILITIES
        );
        return ExitCode::from(2);
    };
    // The argument is an artifact ENTRY's path or an artifact's NAME, resolved
    // the way the floor resolves a root: file identity first, so two spellings
    // of one file name one artifact.
    let path = real_path(name);
    let start = path
        .as_deref()
        .and_then(|p| Path::new(p).parent().map(|d| d.to_path_buf()))
        .or_else(|| std::env::current_dir().ok());
    let manifest = start.as_deref().and_then(nearest_manifest);
    let Some(manifest) = manifest else {
        eprintln!("error: no vyrn.json found upward from `{name}`");
        return ExitCode::from(2);
    };
    let Some(map) = manifest.artifacts.as_ref() else {
        eprintln!(
            "error: {}/vyrn.json declares no artifacts, so nothing in this project has a target",
            manifest.dir
        );
        return ExitCode::from(2);
    };
    let artifact = path
        .as_deref()
        .and_then(|p| map.artifact_for(p))
        .or_else(|| map.list.iter().find(|a| a.name == name));
    let Some(artifact) = artifact else {
        let declared: Vec<&str> = map.list.iter().map(|a| a.name.as_str()).collect();
        eprintln!(
            "error: `{name}` is neither an artifact entry point nor an artifact name in \
             {}/vyrn.json (declared: {})",
            manifest.dir,
            declared.join(", ")
        );
        return ExitCode::from(2);
    };

    println!("{}", artifact.entry);
    let has = floor::capabilities(artifact.target).contains(&cap);
    println!(
        "  artifact: `{}` ({}) — target `{}` {}",
        artifact.name,
        artifact.target,
        artifact.target,
        if has {
            format!("has `{}`", cap.name())
        } else {
            format!("has {}", cap.absence())
        }
    );

    // The load's own graph, with both policies disarmed: a report about the
    // refused tree is the only report anyone wants.
    let mut opts = load_options(&artifact.entry);
    opts.audience = None;
    opts.artifacts = None;
    let source = match std::fs::read_to_string(&artifact.entry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", artifact.entry);
            return ExitCode::from(2);
        }
    };
    let (graph, root_key) =
        match vyrn_frontend::loader::capability_graph(&source, &artifact.entry, &opts, &FsResolver)
        {
            Ok(g) => g,
            Err(diags) => {
                eprintln!("error: cannot link artifact `{}`", artifact.name);
                for d in diags.iter().take(3) {
                    eprintln!(
                        "  {}: {}",
                        d.file.as_deref().unwrap_or(&artifact.entry),
                        d.message
                    );
                }
                return ExitCode::from(2);
            }
        };
    let edges: Vec<(String, String)> = graph
        .iter()
        .flat_map(|(k, imports, _)| imports.iter().map(|t| (k.clone(), t.clone())))
        .collect();

    let mut found = false;
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (module, _, carried) in &graph {
        for c in carried.iter().filter(|c| c.cap == cap) {
            let key = (module.as_str(), c.carrier.as_str());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            found = true;
            println!(
                "  `{}` needs `{}` — {}:{}",
                c.carrier,
                cap.name(),
                map.display_path(module),
                c.line
            );
            // Every module in the graph is in the closure by construction. One
            // the loader injected has no importer, and the floor's own rule for
            // its chain is the entry and it.
            let mut chains = chains_from(&root_key, module, &edges);
            if chains.is_empty() && module != &root_key {
                chains = vec![vec![root_key.clone(), module.clone()]];
            }
            for chain in chains {
                let pretty: Vec<String> = chain.iter().map(|p| map.display_path(p)).collect();
                println!("    {}", pretty.join(" -> "));
            }
        }
    }
    if !found {
        println!(
            "  nothing in artifact `{}`'s closure needs `{}`",
            artifact.name,
            cap.name()
        );
    }
    ExitCode::SUCCESS
}

/// Every simple path forward from `entry` to `target` in the import graph, the
/// entry first. Empty when `target` is not in the artifact's closure at all.
///
/// Bounded the way [`import_chains`] is bounded, and for the same reason: an
/// exhaustive enumeration of an interesting graph does not return, and a report
/// that hangs is worse than one that stops at two dozen answers.
fn chains_from(entry: &str, target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {
    const MAX_CHAINS: usize = 24;
    const MAX_DEPTH: usize = 12;
    fn walk(
        node: &str,
        target: &str,
        edges: &[(String, String)],
        seen: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        if out.len() >= MAX_CHAINS || seen.len() > MAX_DEPTH {
            return;
        }
        if node == target {
            out.push(seen.clone());
            return;
        }
        for (from, to) in edges {
            if from != node || seen.iter().any(|s| s == to) {
                continue;
            }
            seen.push(to.clone());
            walk(to, target, edges, seen, out);
            seen.pop();
        }
    }
    let mut out = Vec::new();
    walk(entry, target, edges, &mut vec![entry.to_string()], &mut out);
    out
}

/// `path` relative to `base`, for printing.
fn rel_to(path: &str, base: &str) -> String {
    path.strip_prefix(base)
        .map(|r| r.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Every `importer -> imported` edge in a project, resolved with the loader's
/// own `resolve_spec`.
///
/// A GENERATOR import contributes edges too: `pages("./routes")` is how a page
/// reaches the bundle, and a chain that stopped at the generator call would miss
/// the only edge anybody is actually asking about. A call naming ONE FILE
/// (`vyxPage("../server/pages/Leak.vyx")`) is resolved by
/// `audience::generator_input` — the same function that decides that module's
/// audience, so the report and the checker cannot disagree about which file a
/// generator was pointed at. Resolving it as an import specifier instead used to
/// append `.vyrn`, so a `.vyx` mount matched nothing and `why` answered "nothing
/// in this project reaches it" about a file the client root demonstrably mounts.
/// A call naming a DIRECTORY still reaches every source under it.
fn project_imports(app_dir: &Path) -> Vec<(String, String)> {
    let files = project_sources(app_dir);
    let mut out: Vec<(String, String)> = Vec::new();
    for (path, source) in &files {
        let opts = load_options(path);
        let body = if path.ends_with(".vyx") {
            vyx_script_body(source).unwrap_or_default()
        } else {
            source.clone()
        };
        let Ok(tokens) = vyrn_frontend::lexer::lex(&body) else {
            continue;
        };
        let (program, _) = vyrn_frontend::parser::parse_accum(tokens);
        for imp in &program.imports {
            use vyrn_frontend::ast::{Expr, ImportSource};
            let spec = match &imp.source {
                ImportSource::Path(s) => s.clone(),
                ImportSource::Generator { args, .. } => match args.first() {
                    Some(Expr::Str(s)) => {
                        if let Some(input) = vyrn_frontend::audience::generator_input(path, s) {
                            out.push((path.clone(), input));
                            continue;
                        }
                        s.clone()
                    }
                    _ => continue,
                },
            };
            let Ok(resolved) = vyrn_frontend::loader::resolve_spec(&spec, path, &opts) else {
                continue;
            };
            let stripped = resolved
                .strip_suffix(".vyrn")
                .unwrap_or(&resolved)
                .to_string();
            if Path::new(&stripped).is_dir() {
                // A generator pointed at a directory reaches every source in it.
                for (kid, _) in &files {
                    if kid.starts_with(&format!("{stripped}/")) {
                        out.push((path.clone(), kid.clone()));
                    }
                }
            } else {
                out.push((path.clone(), resolved));
            }
        }
    }
    // Both ends of every edge as the FILESYSTEM names them, because that is what
    // decides audience (`real_path`) and what the queried path was resolved to.
    // An import spelled `../Server/store` on Windows names the same file as
    // `../server/store`, and a graph keyed on the spelling would report that
    // nothing reaches it while the checker refuses that very edge.
    for (from, to) in out.iter_mut() {
        if let Some(p) = real_path(from) {
            *from = p;
        }
        if let Some(p) = real_path(to) {
            *to = p;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every `.vyrn` / `.vyx` source under `app_dir`, as `(slash path, text)`.
/// Build output and vendored trees are not the project.
fn project_sources(app_dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name.starts_with('.') || name == "vendor" || name == "node_modules" {
                    continue;
                }
                walk(&p, out);
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("vyrn") | Some("vyx")
            ) {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let key = p.to_string_lossy().replace('\\', "/");
                    let key = key.trim_start_matches("//?/").to_string();
                    out.push((key, text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(app_dir, &mut out);
    out.sort();
    out
}

/// Every import chain reaching `target`, each starting at a module nothing else
/// imports (a composition root). Walks the edges BACKWARD from the target, so
/// the answer is "how does anything get here", not "what does this reach".
fn import_chains(target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {
    const MAX_CHAINS: usize = 24;
    const MAX_DEPTH: usize = 12;
    let mut out: Vec<Vec<String>> = Vec::new();
    // Depth-first backward walk, carrying the path built so far (target-last).
    fn back(
        node: &str,
        edges: &[(String, String)],
        seen: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        if out.len() >= MAX_CHAINS || seen.len() >= MAX_DEPTH {
            return;
        }
        let importers: Vec<&String> = edges
            .iter()
            .filter(|(_, to)| to == node)
            .map(|(from, _)| from)
            .filter(|from| !seen.iter().any(|s| s == *from))
            .collect();
        if importers.is_empty() {
            let mut chain = seen.clone();
            chain.reverse();
            out.push(chain);
            return;
        }
        for from in importers {
            seen.push(from.clone());
            back(from, edges, seen, out);
            seen.pop();
        }
    }
    let mut seen = vec![target.to_string()];
    back(target, edges, &mut seen, &mut out);
    // A target nothing imports yields the one-element chain of itself, which is
    // not an answer to "imported by".
    out.retain(|c| c.len() > 1);
    out
}

/// The `.vyrn` modules role discovery reads: the manifest's entry points plus
/// every `.vyrn` directly in the app directory. Generator imports live in an
/// app's ROOT modules by construction, so this stays a shallow scan.
fn contract_roots(app_dir: &Path, manifest: Option<&Manifest>) -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = Vec::new();
    // The manifest the caller already read. Re-reading and re-parsing it here is
    // how a second reader silently answers "this project declares no entry
    // points" to a file the first reader refused.
    if let Some(m) = manifest {
        for key in ["main", "server", "client"] {
            if let Some(vyrn_frontend::schema::Json::Str(p)) = m.doc.get(key) {
                paths.push(app_dir.join(p));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(app_dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("vyrn") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|p| {
            let src = std::fs::read_to_string(&p).ok()?;
            Some((p.to_string_lossy().replace('\\', "/"), src))
        })
        .collect()
}

/// The `<script> … </script>` body of a `.vyx`, which is ordinary Vyrn. The
/// section's bounds are `vyrn_frontend::vyx`'s — the same rule `std/vyx` applies
/// when it compiles the component, so `why` and `deps` describe the file the
/// compiler sees rather than one truncated at a `</script>` inside a string.
fn vyx_script_body(text: &str) -> Option<String> {
    let (start, end) = vyrn_frontend::vyx::script_body(text)?;
    Some(text[start..end].to_string())
}

/// One `toolchain:` row: the tool, the path that would be used, its version,
/// and why that path was chosen.
type ToolRow = (String, String, String, String);

/// A row for a tool resolved by one of `vyrn-codegen`'s `*_from` resolvers,
/// which answer with the path AND the reason (RFC-0102 M1/M2).
///
/// The version column is the pin for a pinned tool, because that is where the
/// version is written down, and `unknown` otherwise: an override and a `tools/`
/// walk both name a path nobody declared a version for, and running each tool to
/// ask would spend four spawns on a report. clang is the exception, and it is
/// the exception because its probe was already being run and thrown away.
fn tool_row(
    name: &str,
    found: Result<Option<(std::path::PathBuf, &'static str)>, String>,
    pin: Option<&str>,
    consulted: &str,
) -> ToolRow {
    let unknown = || vyrn_codegen::toolchain::UNKNOWN_VERSION.to_string();
    match found {
        Ok(Some((path, why))) => {
            let version = match why {
                "pinned" => pin
                    .unwrap_or(vyrn_codegen::toolchain::UNKNOWN_VERSION)
                    .into(),
                _ => unknown(),
            };
            (name.into(), show_path(&path), version, why.into())
        }
        Ok(None) => (
            name.into(),
            "not found".into(),
            unknown(),
            format!("not found: {consulted}"),
        ),
        // A pin that cannot be resolved is a refusal everywhere else, and a
        // report that omitted it would be the silent fallback this RFC exists to
        // forbid. It prints, with the refusal's own words on one line.
        Err(e) => (
            name.into(),
            "unresolved".into(),
            pin.unwrap_or(vyrn_codegen::toolchain::UNKNOWN_VERSION)
                .into(),
            format!(
                "pinned, unresolved: {}",
                e.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        ),
    }
}

/// A path as this repository writes paths: forward slashes, on every host.
fn show_path(p: &Path) -> String {
    p.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

/// The `toolchain:` section of `vyrn deps` (RFC-0102 M3): one row per tool, with
/// the path that would be used, its version, and WHY that path was chosen.
///
/// This is the report the RFC designs, and it lives under `vyrn deps` rather
/// than under a `vyrn doctor` because the RFC's whole thesis is that a tool is
/// one of the things a build depends on — a second command would contradict the
/// title. The third column is the one that matters: an override prints as an
/// override, so a machine that disagrees with CI says which line it disagreed
/// on.
///
/// Nothing here touches the network: a pin resolves through vendor and the
/// content-addressed cache, and an unresolved one prints as unresolved.
fn print_toolchain(start: &Path) {
    let pins = nearest_manifest(start)
        .map(|m| m.toolchain)
        .unwrap_or_default();
    let pin = |tool: &str| {
        pins.iter()
            .find(|(n, _)| n == tool)
            .map(|(_, v)| v.to_string())
    };
    let mut rows: Vec<ToolRow> = Vec::new();
    // clang is discovered, never pinned — RFC-0102 says why — so it is the one
    // row whose version is a probe rather than a declaration.
    rows.push(match vyrn_codegen::toolchain::clang_from() {
        Some((path, version, why)) => ("clang".into(), show_path(&path), version, why.into()),
        None => tool_row("clang", Ok(None), None, "$CLANG, PATH"),
    });
    rows.push(tool_row(
        "wasmtime",
        vyrn_codegen::toolchain::wasmtime_from(start),
        pin("wasmtime").as_deref(),
        "$VYRN_WASMTIME, tools/",
    ));
    let sysroot = vyrn_codegen::toolchain::wasi_sysroot_from(start);
    let sysroot_path = match &sysroot {
        Ok(Some((p, _))) => p.clone(),
        _ => PathBuf::new(),
    };
    rows.push(tool_row(
        "wasi-sysroot",
        sysroot,
        pin("wasi-sysroot").as_deref(),
        "$WASI_SYSROOT, tools/",
    ));
    rows.push(tool_row(
        "wasi-builtins",
        vyrn_codegen::toolchain::wasi_builtins_from(start, &sysroot_path),
        pin("wasi-builtins").as_deref(),
        "$WASI_BUILTINS, beside the sysroot",
    ));
    // The wasm2c route's two tools (RFC-0125 §2.5): discovered like clang, so
    // wasm2c's row is a probe too, and simde's is a path with no version to ask.
    rows.push(match vyrn_codegen::toolchain::wasm2c_from(start) {
        Ok(Some(t)) => ("wasm2c".into(), show_path(&t.exe), t.version, t.why.into()),
        other => tool_row(
            "wasm2c",
            other.map(|o| o.map(|t| (t.exe, t.why))),
            None,
            "$VYRN_WASM2C, tools/",
        ),
    });
    rows.push(tool_row(
        "simde",
        Ok(vyrn_codegen::toolchain::simde_from(start)),
        None,
        "$VYRN_SIMDE, tools/",
    ));

    // The name and path columns are padded; the version column is not. A clang
    // version line runs past a hundred characters on an upstream build, and
    // padding every other row out to it would spend a screen of spaces to align
    // one parenthesis.
    let width = |col: fn(&ToolRow) -> &String| {
        rows.iter()
            .map(|r| col(r).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(|r| &r.0), width(|r| &r.1));
    println!("toolchain:");
    for (name, path, version, why) in &rows {
        println!("  {name:w0$}  {path:w1$}  {version}  ({why})");
    }
}

/// `vyrn deps [artifact]` — print the resolved module graph of every artifact
/// this project declares, then the toolchain that would build them (RFC-0102 M3).
///
/// The entry used to be the manifest's `main`, and no example project in this
/// repository declares one: they declare `artifacts`, or the `server`/`client`
/// keys RFC-0103 M1 made sugar for it. So the command that answers "what does
/// this build depend on" could not answer for the projects that are built. It
/// reads the artifact map now, which is the same declaration the floor reads —
/// one reader, so `deps` cannot report a graph the build does not have.
///
/// A project declaring only `main` prints exactly what it printed before: one
/// graph, no header. The header exists to say WHICH artifact a graph belongs to,
/// and with one nameless-by-convention artifact there is nothing to disambiguate.
///
/// A manifest that declares no artifacts at all — the repository root's own,
/// which pins a toolchain and nothing else — is not an error. It prints the
/// toolchain and says there is no graph, because that is the true answer to the
/// question asked.
fn deps(name: Option<&str>) -> ExitCode {
    let Some(cwd) = std::env::current_dir().ok() else {
        eprintln!("error: cannot read the current directory");
        return ExitCode::FAILURE;
    };
    let Some(manifest) = nearest_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found upward from here");
        return ExitCode::FAILURE;
    };
    let dir = PathBuf::from(&manifest.dir);
    let artifacts = manifest.artifacts.as_ref();
    let list: Vec<&vyrn_frontend::artifacts::Artifact> = match (artifacts, name) {
        // Naming an artifact scopes the report to it — the argument the CLI
        // already had a slot for, resolved by name exactly as
        // `vyrn why --capability` resolves it. An entry PATH is not accepted
        // here: `why` needs it because a capability question starts from a file,
        // and this one starts from the manifest.
        (Some(map), Some(want)) => match map.list.iter().find(|a| a.name == want) {
            Some(a) => vec![a],
            None => {
                let declared: Vec<&str> = map.list.iter().map(|a| a.name.as_str()).collect();
                eprintln!(
                    "error: {}/vyrn.json declares no artifact `{want}` (declared: {})",
                    manifest.dir,
                    declared.join(", ")
                );
                return ExitCode::from(2);
            }
        },
        (Some(map), None) => map.list.iter().collect(),
        (None, Some(want)) => {
            eprintln!(
                "error: {}/vyrn.json declares no artifacts, so it declares no `{want}`",
                manifest.dir
            );
            return ExitCode::from(2);
        }
        (None, None) => Vec::new(),
    };
    if list.is_empty() {
        println!(
            "{}/vyrn.json declares no artifacts, so there is no module graph to report",
            manifest.dir
        );
        print_toolchain(&dir);
        return ExitCode::SUCCESS;
    }

    // One artifact called `main` is what every project declared before RFC-0103
    // named the concept, and its report is unchanged.
    let bare = list.len() == 1 && list[0].name == "main";
    let mut failed = false;
    for (i, artifact) in list.iter().enumerate() {
        if !bare {
            if i > 0 {
                println!();
            }
            println!(
                "artifact `{}` ({}) — {}",
                artifact.name,
                artifact.target,
                artifacts.map_or_else(
                    || artifact.entry.clone(),
                    |m| m.display_path(&artifact.entry)
                )
            );
        }
        let root_key = &artifact.entry;
        let source = match std::fs::read_to_string(root_key) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {root_key}: {e}");
                failed = true;
                continue;
            }
        };
        let opts = load_options(root_key);
        match vyrn_frontend::loader::module_graph(&source, root_key, &opts, &FsResolver) {
            Ok(graph) => {
                for (module, imports) in graph {
                    println!("{module}");
                    for i in imports {
                        println!("  -> {i}");
                    }
                }
            }
            Err(diags) => {
                for d in &diags {
                    let file = d.file.as_deref().unwrap_or(root_key);
                    eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
                    if let Some(note) = &d.note {
                        eprintln!("  note: {note}");
                    }
                }
                failed = true;
            }
        }
    }
    print_toolchain(&dir);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `vyrn fmt [file ...] [--check]` (RFC-0017) — the canonical formatter.
///
/// With explicit files, formats each in place. With no files, formats the
/// project `main` plus its LOCAL (non-remote) imports, discovered through the
/// module graph. `--check` writes nothing: it lists the files that would change
/// and exits 1 if any do (0 otherwise) — the CI gate.
///
/// fmt requires only *lexable* input (a parse error still formats). A lex error
/// leaves that file untouched and is reported; the command still processes the
/// other files but exits non-zero.
fn fmt_cmd(rest: &[String]) -> ExitCode {
    // `--from-json` is a converter, not a formatter run: it prints and writes
    // nothing (RFC-0097 M1).
    if let Some(i) = rest.iter().position(|a| a == "--from-json") {
        let Some(path) = rest.get(i + 1).filter(|a| !a.starts_with('-')) else {
            eprintln!("error: --from-json needs a .json file");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        let flag = |name: &str, fallback: &str| -> String {
            rest.iter()
                .position(|a| a == name)
                .and_then(|k| rest.get(k + 1))
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };
        return from_json_cmd(
            path,
            &flag("--as", "Config"),
            &flag("--from", "./config.vyrn"),
        );
    }
    let check = rest.iter().any(|a| a == "--check");
    let files: Vec<String> = rest
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();

    // Resolve the set of files to format.
    let targets: Vec<String> = if files.is_empty() {
        match fmt_project_files() {
            Ok(t) => t,
            Err(code) => return code,
        }
    } else {
        files
    };
    if targets.is_empty() {
        eprintln!("error: no input files, and no vyrn.json with a `main` found");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let mut would_change: Vec<String> = Vec::new();
    let mut had_error = false;
    let mut written = 0usize;
    for path in &targets {
        // A `.vyx` file is a template with a Vyrn `<script>` inside it, not a Vyrn
        // module. The formatter lexes the whole file as Vyrn, and the template
        // survives that as a token soup — so it round-trips through the safety
        // invariant and comes out with spaces inside every tag and every sentence.
        // Silently destroying source is worse than refusing, so `.vyx` is skipped
        // until the formatter learns to print a template.
        if path.ends_with(".vyx") {
            eprintln!("note: skipping {path}: `vyrn fmt` cannot format a .vyx template yet");
            continue;
        }
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {path}: {e}");
                had_error = true;
                continue;
            }
        };
        // CRLF policy (RFC-0017): the formatter decides the whitespace *between*
        // tokens, never the platform newline convention. A file's existing
        // line-ending style is preserved — a CRLF (Windows-authored) file
        // round-trips to CRLF, an LF file to LF — so a canonically-formatted CRLF
        // file is NOT a spurious diff under `--check`, and `fmt` never rewrites a
        // whole file just to flip its newlines. We normalize to LF for the
        // formatter (whose safety invariant re-lexes LF), then re-apply CRLF if
        // the source used it. (A file that mixes styles canonicalizes to CRLF
        // when any CRLF is present — a deliberate, idempotent choice.)
        let uses_crlf = source.contains("\r\n");
        let normalized = source.replace("\r\n", "\n");
        match vyrn_frontend::fmt(&normalized) {
            Ok(formatted) => {
                let formatted = if uses_crlf {
                    formatted.replace('\n', "\r\n")
                } else {
                    formatted
                };
                if formatted != source {
                    if check {
                        would_change.push(path.clone());
                    } else if let Err(e) = std::fs::write(path, &formatted) {
                        eprintln!("error: cannot write {path}: {e}");
                        had_error = true;
                    } else {
                        written += 1;
                    }
                }
            }
            Err(d) => {
                // A lex error (or the internal safety-invariant tripwire) — leave
                // the file untouched.
                eprintln!("{path}:{}: {}", d.line, d.message);
                had_error = true;
            }
        }
    }

    if check {
        for f in &would_change {
            println!("{f}");
        }
        if !would_change.is_empty() || had_error {
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if written > 0 {
        println!(
            "formatted {written} file{}",
            if written == 1 { "" } else { "s" }
        );
    } else if !had_error {
        println!("already formatted");
    }
    if had_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The converter `vyrn fmt --from-json` runs, written in Vyrn.
///
/// The CLI carries bytes and nothing else: `std/jsonread` does the reading (it
/// already rejects duplicate keys and trailing commas — RFC-0059) and
/// `std/von`'s `jsonToVon` does the writing. There is no second JSON reader and
/// no second VON writer in Rust, which is the point — a converter that
/// disagreed with the reader would be worse than no converter.
///
/// `toVon` ends its text with a newline and `print` adds one, so the last byte
/// is dropped here: the output redirected to a `.von` file is then exactly what
/// `vyrn fmt` leaves behind.
const FROM_JSON_SRC: &str = r#"import { parseJson } from "std/jsonread"
import { jsonToVon } from "std/von"
import { substring } from "std/strings"

fn convert(src: String, name: String, module: String) -> Result<String, String> {
    let j = parseJson(src)?
    return jsonToVon(j, name, module)
}

fn put(s: String) -> Int64 {
    print(substring(s, 0, s.byteLength - 1))
    return 0
}

fn main() -> Int64 {
    let a = args()
    return match convert(a[0], a[1], a[2]) {
        Ok(text) => put(text),
        Err(e) => panic(e),
    }
}
"#;

/// `vyrn fmt --from-json <file.json> [--as <Type>] [--from <module>]` (RFC-0097
/// M1) — print a JSON file as VON, headed by an `import type` line.
///
/// The result is a starting point, not an answer: JSON says nothing about types,
/// so every nested object arrives as a `Map` and the author, who has the type,
/// promotes what should be a record. Nothing is written to disk.
fn from_json_cmd(path: &str, type_name: &str, module: &str) -> ExitCode {
    let json = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    // A key beside the input file, so `std/` resolves the same way it would for
    // a program written there. Nothing reads it: the source is the constant above.
    let norm = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let key = match norm.rfind('/') {
        Some(i) => format!("{}/from-json.vyrn", &norm[..i]),
        None => "from-json.vyrn".to_string(),
    };
    let program = match load_program(&key, FROM_JSON_SRC) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let args = vec![json, type_name.to_string(), module.to_string()];
    match vyrn_frontend::interp::run_with_args(&program, &args) {
        Ok(code) => ExitCode::from((code & 0xff) as u8),
        Err(e) => {
            // The trap carries the position of the `panic` in the converter
            // above — a module the user never wrote and cannot open. The message
            // is the whole answer, so the internal location is dropped and the
            // INPUT file's name takes its place.
            let msg = e
                .split_once(" (from-json.vyrn:")
                .map(|(m, _)| m)
                .unwrap_or(e.as_str());
            eprintln!("error: {path}: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The project's `main` plus its local (non-remote) imports, as file paths — the
/// default target set for a bare `vyrn fmt`. Remote imports (github:/gist:/https:)
/// are pinned artifacts, never formatted in place.
fn fmt_project_files() -> Result<Vec<String>, ExitCode> {
    let Some(main) = manifest_main() else {
        return Ok(Vec::new());
    };
    let source = match std::fs::read_to_string(&main) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {main}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let root_key = main.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    match vyrn_frontend::loader::module_graph(&source, &root_key, &opts, &resolver) {
        Ok(graph) => {
            // Module keys are the local modules' file paths (and remote specifiers,
            // which we exclude). De-duplicate while preserving order.
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for (module, _imports) in graph {
                if vyrn_frontend::loader::is_remote(&module) {
                    continue;
                }
                if seen.insert(module.clone()) {
                    out.push(module);
                }
            }
            Ok(out)
        }
        Err(diags) => {
            // A graph error (e.g. an unresolvable import) — fall back to just the
            // main file so `fmt` is still useful on a partly-broken project.
            for d in &diags {
                let file = d.file.as_deref().unwrap_or(&root_key);
                eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
                if let Some(note) = &d.note {
                    eprintln!("  note: {note}");
                }
            }
            Ok(vec![root_key])
        }
    }
}

// ---------------------------------------------------------------------------
// `vyrn doc` (RFC-0065) — Markdown API docs
// ---------------------------------------------------------------------------

/// A module to document: its stable name (`std/json`, `store`, `routes/home`)
/// and its source text. The name is both the page heading and, with `.md`, the
/// output file path (so `/` becomes a subdirectory).
struct DocModule {
    name: String,
    source: String,
}

/// `vyrn doc [file|dir] [-o <dir>] [--std] [--verify]` (RFC-0065) — emit
/// GitHub-flavored Markdown API docs: one `.md` per module plus `index.md`. The
/// `///` blocks pass through verbatim, so a ` ```mermaid ` fence renders natively
/// on GitHub with zero bundled JavaScript. Output is deterministic and byte-stable
/// (every list is sorted, newlines are LF) so generated docs diff cleanly in git.
///
/// `--verify` writes nothing: it regenerates in memory and exits 1 if the output
/// directory differs from what would be generated (the CI drift gate).
fn doc_cmd(rest: &[String]) -> ExitCode {
    let with_std = rest.iter().any(|a| a == "--std");
    let verify = rest.iter().any(|a| a == "--verify");
    let out_dir = match rest.iter().position(|a| a == "-o") {
        Some(i) => match rest.get(i + 1) {
            Some(d) => d.clone(),
            None => {
                eprintln!("error: -o needs a directory");
                return ExitCode::from(2);
            }
        },
        None => "docs/api".to_string(),
    };
    // The one positional (a file or directory); flags and the `-o` value excluded.
    let target = rest
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with('-') && !(*i > 0 && rest[*i - 1] == "-o"))
        .map(|(_, a)| a.clone())
        .next();

    let modules = match discover_doc_modules(target.as_deref(), with_std) {
        Ok(m) => m,
        Err(code) => return code,
    };
    if modules.is_empty() {
        eprintln!("error: no modules to document");
        return ExitCode::from(2);
    }

    // Render every page into a (relative path -> content) set, deterministically.
    let mut files: Vec<(String, String)> = Vec::new();
    files.push(("index.md".to_string(), render_doc_index(&modules)));
    for m in &modules {
        let doc = vyrn_frontend::module_doc(&m.source);
        files.push((format!("{}.md", m.name), render_doc_page(&m.name, &doc)));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    if verify {
        return verify_doc_dir(&out_dir, &files);
    }
    write_doc_dir(&out_dir, &files)
}

/// Resolve the set of modules to document (RFC-0065):
/// - a **file** argument → that file's local-import closure (`--std` adds the
///   std modules it reaches);
/// - a **directory** argument → every `.vyrn` under it, named relative to it;
/// - **no argument** with a `vyrn.json` main → the project's local-import closure
///   (`--std` adds reached std modules);
/// - **no argument** with `--std` → the whole std library.
fn discover_doc_modules(target: Option<&str>, with_std: bool) -> Result<Vec<DocModule>, ExitCode> {
    match target {
        Some(t) if Path::new(t).is_dir() => scan_doc_dir(t, ""),
        Some(t) => closure_doc_modules(t, with_std),
        None => {
            if let Some(main) = manifest_main() {
                closure_doc_modules(&main, with_std)
            } else if with_std {
                match std_root() {
                    Some(root) => scan_doc_dir(&root, "std/"),
                    None => {
                        eprintln!("error: --std given but no std library found (set VYRN_STD)");
                        Err(ExitCode::FAILURE)
                    }
                }
            } else {
                eprintln!(
                    "error: no input file or directory, and no vyrn.json with a `main` found"
                );
                eprintln!("{USAGE}");
                Err(ExitCode::from(2))
            }
        }
    }
}

/// Every `.vyrn` file under `dir` (recursively), each a module named `<prefix>`
/// plus its path relative to `dir` (no extension, `/`-separated). Sorted by name.
fn scan_doc_dir(dir: &str, prefix: &str) -> Result<Vec<DocModule>, ExitCode> {
    let base = normalize_slashes(dir);
    let mut paths: Vec<String> = Vec::new();
    collect_vyrn_files(Path::new(dir), &mut paths);
    let mut out = Vec::new();
    for p in paths {
        let rel = rel_name(&p, &base);
        // A fenced module has no reader outside the compiler (RFC-0125 §2.4).
        if vyrn_frontend::loader::is_fenced(&format!("{prefix}{rel}")) {
            continue;
        }
        let source = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {p}: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        out.push(DocModule {
            name: format!("{prefix}{rel}"),
            source,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Recursively collect `.vyrn` files under `dir` into `out` (unsorted; the caller
/// sorts by module name). Directories are visited in a stable, sorted order.
fn collect_vyrn_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    items.sort();
    for path in items {
        if path.is_dir() {
            collect_vyrn_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("vyrn") {
            out.push(normalize_slashes(&path.to_string_lossy()));
        }
    }
}

/// The modules of `root_file`'s local-import closure (RFC-0010 module graph):
/// every LOCAL module reached, named relative to the project. `with_std` also
/// keeps the std modules the closure reaches (named `std/<rel>`). Remote and
/// generated modules are never documented.
fn closure_doc_modules(root_file: &str, with_std: bool) -> Result<Vec<DocModule>, ExitCode> {
    let source = match std::fs::read_to_string(root_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {root_file}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let root_key = normalize_slashes(root_file);
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let std_root = opts.std_root.as_deref().map(normalize_slashes);
    // The project base for local module names: the manifest dir, else the root
    // file's own directory.
    let base = nearest_manifest(Path::new(&root_key).parent().unwrap_or(Path::new(".")))
        .map(|m| m.dir)
        .unwrap_or_else(|| {
            root_key
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default()
        });

    let result =
        vyrn_frontend::loader::module_graph_with_sources(&source, &root_key, &opts, &resolver);
    // Pins are kept even when the run itself fails — fetched is pinned. But a
    // pin the disk refused fails the command rather than leaving the remote
    // unpinned under a quiet exit.
    save_lock(&resolver)?;
    let graph = match result {
        Ok(g) => g,
        Err(diags) => {
            for d in &diags {
                let file = d.file.as_deref().unwrap_or(&root_key);
                eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
            }
            return Err(ExitCode::FAILURE);
        }
    };

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (key, _imports, gen_source) in graph {
        if gen_source.is_some() || vyrn_frontend::loader::is_remote(&key) {
            continue; // generated + remote modules are out of scope
        }
        let is_std = std_root
            .as_deref()
            .is_some_and(|r| key.starts_with(&format!("{r}/")));
        let name = if is_std {
            if !with_std {
                continue;
            }
            format!("std/{}", rel_name(&key, std_root.as_deref().unwrap_or("")))
        } else {
            rel_name(&key, &base)
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        let source = match std::fs::read_to_string(&key) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {key}: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        out.push(DocModule { name, source });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A slash-normalized path with the Windows verbatim prefix stripped.
fn normalize_slashes(p: &str) -> String {
    p.trim_start_matches(r"\\?\").replace('\\', "/")
}

/// A module path relative to `base`, without its `.vyrn` extension — the module
/// name. Falls back to the file stem when `path` is not under `base`.
fn rel_name(path: &str, base: &str) -> String {
    let path = normalize_slashes(path);
    let stripped = if base.is_empty() {
        path.as_str()
    } else {
        path.strip_prefix(&format!("{}/", base.trim_end_matches('/')))
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path))
    };
    stripped
        .strip_suffix(".vyrn")
        .unwrap_or(stripped)
        .to_string()
}

/// Render `index.md`: a title and a sorted list of modules, each linking to its
/// page with the first line of its header doc as a one-line description.
fn render_doc_index(modules: &[DocModule]) -> String {
    let mut lines = vec!["# API Reference".to_string(), String::new()];
    for m in modules {
        let doc = vyrn_frontend::module_doc(&m.source);
        let summary = doc
            .header_doc
            .as_deref()
            .and_then(|h| h.lines().next())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match summary {
            Some(s) => lines.push(format!("- [{}]({}.md) — {}", m.name, m.name, s)),
            None => lines.push(format!("- [{}]({}.md)", m.name, m.name)),
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Render one module page (RFC-0065): the `# name` title, the header doc block,
/// then every export as a `## name` heading, a ` ```vyrn ` signature fence, and
/// its `///` block verbatim — followed, for a protocol, by a `###` section per
/// DOCUMENTED method. Blocks are separated by a single blank line; the page ends
/// in exactly one newline.
fn render_doc_page(name: &str, doc: &vyrn_frontend::ModuleDoc) -> String {
    let mut blocks: Vec<String> = vec![format!("# {name}")];
    if let Some(h) = &doc.header_doc {
        blocks.push(h.clone());
    }
    if doc.exports.is_empty() {
        blocks.push("_No exported declarations._".to_string());
    }
    for e in &doc.exports {
        let mut parts = vec![
            format!("## {}", e.name),
            format!("```vyrn\n{}\n```", e.signature),
        ];
        if let Some(d) = &e.doc {
            parts.push(d.clone());
        }
        // A documented protocol method gets its own `###` section under the
        // protocol, so its prose sits with the signature it describes.
        for (sig, d) in &e.members {
            parts.push(format!("### `{sig}`"));
            parts.push(d.clone());
        }
        blocks.push(parts.join("\n\n"));
    }
    let mut page = blocks.join("\n\n");
    page.push('\n');
    page
}

/// Write the rendered `files` under `out_dir` (creating subdirectories), then
/// prune any stale `.md` files not in the set — so a regenerate always converges
/// with `--verify`. Every file is written with LF newlines.
fn write_doc_dir(out_dir: &str, files: &[(String, String)]) -> ExitCode {
    let wanted: std::collections::HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();
    for (rel, content) in files {
        let path = Path::new(out_dir).join(rel);
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("error: cannot create {}: {e}", dir.display());
                return ExitCode::FAILURE;
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    for existing in existing_md_files(out_dir) {
        if !wanted.contains(&existing) {
            let _ = std::fs::remove_file(Path::new(out_dir).join(&existing));
        }
    }
    println!(
        "wrote {} file{} to {out_dir}",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    ExitCode::SUCCESS
}

/// `--verify`: exit 1 if `out_dir`'s `.md` files differ in any way (missing,
/// extra, or content) from the freshly generated `files`. The CI drift gate.
fn verify_doc_dir(out_dir: &str, files: &[(String, String)]) -> ExitCode {
    let wanted: std::collections::HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let existing: std::collections::HashSet<String> =
        existing_md_files(out_dir).into_iter().collect();
    // A stale page on disk that we no longer generate is drift.
    let mut extra: Vec<String> = existing.difference(&wanted).cloned().collect();
    extra.sort();
    if let Some(f) = extra.first() {
        eprintln!("doc drift: {out_dir}/{f} is not generated (stale) — run `vyrn doc` to update");
        return ExitCode::FAILURE;
    }
    for (rel, content) in files {
        let path = Path::new(out_dir).join(rel);
        match std::fs::read_to_string(&path) {
            Ok(on_disk) if normalize_slashes_content(&on_disk) == *content => {}
            Ok(_) => {
                eprintln!("doc drift: {out_dir}/{rel} is out of date — run `vyrn doc` to update");
                return ExitCode::FAILURE;
            }
            Err(_) => {
                eprintln!("doc drift: {out_dir}/{rel} is missing — run `vyrn doc` to update");
                return ExitCode::FAILURE;
            }
        }
    }
    println!("docs up to date ({} files)", files.len());
    ExitCode::SUCCESS
}

/// Normalize a file's newlines to LF before comparison, so a CRLF checkout of a
/// generated doc is not reported as drift (the tool always emits LF).
fn normalize_slashes_content(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Every `.md` file under `dir` (recursively), as paths relative to `dir` with
/// `/` separators — the set `--verify` compares and `write` prunes against.
fn existing_md_files(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_md_files(Path::new(dir), dir, &mut out);
    out.sort();
    out
}

fn collect_md_files(dir: &Path, base: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    items.sort();
    for path in items {
        if path.is_dir() {
            collect_md_files(&path, base, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let full = normalize_slashes(&path.to_string_lossy());
            let base = normalize_slashes(base);
            let rel = full
                .strip_prefix(&format!("{}/", base.trim_end_matches('/')))
                .unwrap_or(&full)
                .to_string();
            out.push(rel);
        }
    }
}

/// The lockfile location + project dir for a root file: next to the manifest
/// when there is one, else next to the root file.
fn lock_home(root_key: &str) -> (PathBuf, Option<String>) {
    let start = Path::new(root_key)
        .parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok());
    if let Some(m) = start.clone().and_then(|d| nearest_manifest(&d)) {
        return (Path::new(&m.dir).join("vyrn.lock"), Some(m.dir));
    }
    let dir = start.unwrap_or_else(|| PathBuf::from("."));
    (dir.join("vyrn.lock"), None)
}

/// Build the CLI resolver (fs + lock/cache/network remote handling).
///
/// A lock file that will not parse stops the command here. Continuing would mean
/// building against whatever the network serves now, and re-pinning to it.
fn make_resolver(root_key: &str) -> remote::RemoteResolver {
    let (lock_path, project_dir) = lock_home(root_key);
    remote::RemoteResolver {
        lock: std::cell::RefCell::new(load_lock(lock_path)),
        project_dir,
        offline: env_offline(),
    }
}

/// [`remote::Lock::load`], with the CLI's answer to a damaged one: name the line
/// and stop. The same policy as an unreadable manifest, for the same reason —
/// a pin the compiler cannot read is not the absence of a pin.
fn load_lock(path: PathBuf) -> remote::Lock {
    match remote::Lock::load(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

/// Persist any new pins the load produced. Failures to write the lock are
/// loud: an unpinned build is not reproducible.
fn save_lock(resolver: &remote::RemoteResolver) -> Result<(), ExitCode> {
    let lock = resolver.lock.borrow();
    if lock.dirty {
        if let Err(e) = lock.save() {
            eprintln!("error: cannot write {}: {e}", lock.path.display());
            return Err(ExitCode::FAILURE);
        }
        eprintln!("pinned new remote imports in {}", lock.path.display());
    }
    Ok(())
}

/// Load + check a root file through the module loader, printing diagnostics
/// (with their originating file) on failure and WARNINGS on success.
///
/// This is the toolchain's single load site — every command that *builds a
/// program* arrives here (`check`, `run`, `emit-ir`, `build`, `test`, `bench`,
/// `serve`, `dev`), so warnings need exactly one print site to reach all of
/// them. The three that do not are the three that never call `load`: `fmt` is a
/// token-stream rewriter, `doc` renders `module_doc` over sources, and
/// `emit-gen` prints the generated text itself — where a `//@warning` directive
/// is already visible verbatim.
///
/// They print BEFORE the caller does anything else, which puts them ahead of the
/// `serving …` / `dev: serving …` banner and ahead of the program's own output.
/// That is deliberate: a warning is about the compile, so it belongs with the
/// compile, and printing it first means it can never interleave with a served
/// request's log lines or be scrolled off by a long-running host. They go to
/// stderr, so `vyrn run`'s stdout stays exactly the program's.
///
/// Stderr is a different matter and worth stating plainly: `vyrn run` compiles
/// and runs in ONE process, so a warning shares the stream with the program's
/// own stderr, while a native or wasm run executes an already-built artifact and
/// never compiles. The parity harness therefore compares the program's stderr
/// with compile-time diagnostics filtered out (`parity::runtime_err`) — the
/// invariant is that the PROGRAM behaves identically on all three backends, and a
/// warning is about the compile.
/// `vyrn fix [file]` (RFC-0087 U2) — apply the `.copy()` a move diagnostic
/// names, and refuse everything else.
///
/// Since Phase 4b every rule-1/2/3 error is a menu: the offending line, then one
/// `fix:` per way out. Phase 4b migrated 65 sites by hand and 4b-2 another 262,
/// and 299 of those 327 were the same edit — put `.copy()` on the path. This is
/// that edit, made by the compiler that named it.
///
/// **It applies one fix and one only: `.copy()`.** The other two entries on the
/// menu are decisions, not edits. `consume` on a parameter changes the
/// signature, so it changes what every caller may do with its argument;
/// `for x in consume xs` gives the loop the container, so it decides that
/// nothing after the loop wants it. The compiler knows both are legal where it
/// offers them and neither is what the author meant. A fix that guesses is worse
/// than no fix, so this refuses them by name.
///
/// **It edits the file it was given and no other.** A diagnostic in an imported
/// module is reported, not fixed: `std/` and a vendored remote are not the
/// caller's to rewrite, and a file the caller does own is one more `vyrn fix`.
///
/// **The compiler verifies every round.** A round applies at most one edit per
/// line, re-loads, and keeps the result only if the diagnostic count went down.
/// The tool therefore cannot leave a file that compiles worse than it found it.
fn fix_cmd(path: &str, source: &str) -> ExitCode {
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let mut text = source.to_string();
    let mut rounds = 0usize;
    let mut applied: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();

    loop {
        let diags = fix_diagnostics(&root_key, &text);
        let mine: Vec<&vyrn_frontend::diagnostics::Diagnostic> = diags
            .iter()
            .filter(|d| d.stage == "movecheck" && d.file.is_none())
            .collect();
        // One edit per line: two fixes on one line are two searches over text
        // that the first edit already moved.
        let mut edits: Vec<(usize, String)> = Vec::new();
        let mut seen_lines: Vec<usize> = Vec::new();
        for d in &mine {
            if seen_lines.contains(&d.line) {
                continue;
            }
            match copy_path(&d.message) {
                Some(p) => {
                    seen_lines.push(d.line);
                    edits.push((d.line, p));
                }
                None => {
                    let first = d.message.lines().next().unwrap_or_default();
                    let note = format!("{}:{}: {first}", root_key, d.line);
                    if !refused.contains(&note) {
                        refused.push(note);
                    }
                }
            }
        }
        if edits.is_empty() {
            for d in &diags {
                if d.file.is_some() {
                    let first = d.message.lines().next().unwrap_or_default();
                    let where_ = d.file.as_deref().unwrap_or(&root_key);
                    let note = format!("{where_}:{}: {first} (another file)", d.line);
                    if !refused.contains(&note) {
                        refused.push(note);
                    }
                }
            }
            break;
        }
        let mut next = text.clone();
        let mut this_round: Vec<String> = Vec::new();
        for (line, p) in &edits {
            match insert_copy(&next, *line, p) {
                Ok(t) => {
                    next = t;
                    this_round.push(format!("{root_key}:{line}: `{p}` -> `{p}.copy()`"));
                }
                Err(why) => {
                    let note = format!("{root_key}:{line}: {why}");
                    if !refused.contains(&note) {
                        refused.push(note);
                    }
                }
            }
        }
        if this_round.is_empty() {
            break;
        }
        // The compiler is the check. An edit that does not reduce the problem
        // count is discarded whole, because a fix that trades one error for
        // another is a guess with extra steps.
        if fix_diagnostics(&root_key, &next).len() >= diags.len() {
            refused.push(format!(
                "{root_key}: {} edit(s) rolled back — they did not reduce the diagnostics",
                this_round.len()
            ));
            break;
        }
        text = next;
        applied.extend(this_round);
        rounds += 1;
        // A round always reduces the count, so this bound is only reached by a
        // file with hundreds of sites — and then it is worth running again.
        if rounds >= 100 {
            break;
        }
    }

    if text != source {
        if let Err(e) = std::fs::write(path, &text) {
            eprintln!("error: cannot write {path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    for a in &applied {
        println!("{a}");
    }
    for r in &refused {
        println!("not fixed: {r}");
    }
    println!("{} fix(es) applied, {} left", applied.len(), refused.len());
    // Reports; does not gate. A file with nothing to fix and a file this refuses
    // to touch both exit 0 — `vyrn check` is the gate.
    ExitCode::SUCCESS
}

/// Load `text` as `root_key` and return every diagnostic, printing nothing.
fn fix_diagnostics(root_key: &str, text: &str) -> Vec<vyrn_frontend::diagnostics::Diagnostic> {
    let opts = load_options(root_key);
    let resolver = make_resolver(root_key);
    match vyrn_frontend::load_warned(text, root_key, &opts, &resolver).0 {
        // A program the checker accepts may still be refused by the kernel,
        // and every rule the deletion track moves is refused there and
        // nowhere else (RFC-0125 §3 M3). Without this the tool answered `0
        // fix(es) applied` for a program it used to name — and the kernel
        // prints the same `fix:` menu, so the ways out are readable here too.
        Ok(program) => kernel_diagnostics(&program),
        Err(d) => d,
    }
}

/// The kernel's refusals for `program`, as `movecheck`-stage diagnostics.
///
/// The same reading [`kernel_refuses`] prints, in the shape every other
/// consumer of a diagnostic already takes. `file` is `None` for the root
/// module, which is what tells `vyrn fix` an edit is its to make.
fn kernel_diagnostics(
    program: &vyrn_frontend::ast::Program,
) -> Vec<vyrn_frontend::diagnostics::Diagnostic> {
    if !vyrn_lower::kernel_refuses() {
        return Vec::new();
    }
    let _ = vyrn_frontend::own::analyze(program);
    let mut refusals = vyrn_lower::take_refusals();
    let mut seen = std::collections::HashSet::new();
    refusals.retain(|r| seen.insert((r.file.clone(), r.line, r.message.clone())));
    refusals
        .into_iter()
        .map(|r| {
            let mut d =
                vyrn_frontend::diagnostics::Diagnostic::error(r.line, 0, "movecheck", r.message);
            d.file = r.file;
            d
        })
        .collect()
}

/// The path a `.copy()` fix names, out of a diagnostic's menu.
///
/// A menu line is ``  fix: `PATH.copy()` <why>``. Nothing else in the message
/// has that shape, so this is a read of the text `movecheck::menu` writes rather
/// than a second table that would have to be kept in step with it.
fn copy_path(message: &str) -> Option<String> {
    for line in message.lines() {
        let Some(rest) = line.trim_start().strip_prefix("fix: `") else {
            continue;
        };
        let Some((quoted, _)) = rest.split_once('`') else {
            continue;
        };
        if let Some(p) = quoted.strip_suffix(".copy()") {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// Put `.copy()` after the single occurrence of `path` on 1-based `line`.
///
/// Refuses rather than chooses. The occurrence must be whole — not the tail of a
/// longer name, and not a receiver something else is read out of — and there must
/// be exactly one of it. Two occurrences on one line means the diagnostic's line
/// number cannot say which, and guessing is the failure this tool exists to
/// avoid.
fn insert_copy(text: &str, line: usize, path: &str) -> Result<String, String> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let l = lines
        .get(line.saturating_sub(1))
        .ok_or_else(|| format!("no line {line}"))?;
    let mut hits: Vec<usize> = Vec::new();
    let mut from = 0usize;
    while let Some(i) = l[from..].find(path) {
        let at = from + i;
        let end = at + path.len();
        let before_ok = at == 0
            || !l[..at]
                .chars()
                .next_back()
                .is_some_and(|c| is_word(c) || c == '.');
        // A following `.` or `(` means the occurrence is a receiver or a callee,
        // not the value the diagnostic is about.
        let after_ok = !l[end..]
            .chars()
            .next()
            .is_some_and(|c| is_word(c) || c == '.' || c == '(');
        if before_ok && after_ok {
            hits.push(at);
        }
        from = at + path.len();
    }
    match hits.len() {
        1 => {
            let at = hits[0] + path.len();
            let mut out = String::with_capacity(text.len() + 7);
            for (i, src) in lines.iter().enumerate() {
                if i + 1 == line {
                    out.push_str(&src[..at]);
                    out.push_str(".copy()");
                    out.push_str(&src[at..]);
                } else {
                    out.push_str(src);
                }
            }
            Ok(out)
        }
        0 => Err(format!("`{path}` is not on the line as written")),
        n => Err(format!(
            "`{path}` appears {n} times on the line — which one is not said"
        )),
    }
}

/// A hard refusal by the kernel — a double free, a use after release, a join
/// whose edges disagree, a rule the core states; not a missing release, which
/// the placer repairs, and not a gap, which is a construct the core cannot
/// lower and no opinion about the program — fails the command with the
/// kernel's message as a diagnostic, printed as the checker's are:
/// `file:line:col: message` (RFC-0125 §3 M3).
///
/// It fails by default (RFC-0125 §3 M3, the default slice), which is what
/// lets a rule leave `movecheck.rs`: while the refusal was behind a flag, a
/// deleted rule shipped a program that should be refused. `VYRN_NO_KERNEL=1`
/// turns it off for a bisect.
///
/// The refusals are this program's because [`RefusalScope`] cleared the
/// thread-local when the program was linked — the load runs `gen fn` bodies as
/// whole programs of their own, and each fills the same one. The analysis is
/// asked for here rather than re-run: under the ownership memo it is the one
/// this build already made, and where no memo is armed it is a second run that
/// says the same thing twice, which the dedup below drops.
fn kernel_refuses(program: &vyrn_frontend::ast::Program, path: &str) -> Result<(), ExitCode> {
    if !vyrn_lower::kernel_refuses() {
        return Ok(());
    }
    let _ = vyrn_frontend::own::analyze(program);
    let mut refusals = vyrn_lower::take_refusals();
    let mut seen = std::collections::HashSet::new();
    refusals.retain(|r| seen.insert((r.file.clone(), r.line, r.message.clone())));
    if refusals.is_empty() {
        return Ok(());
    }
    let root = path.trim_start_matches(r"\\?\").replace('\\', "/");
    for r in &refusals {
        let file = r.file.as_deref().unwrap_or(&root);
        eprintln!("{}:{}:0: {}", file, r.line, r.message);
    }
    Err(ExitCode::FAILURE)
}

fn load_program(path: &str, source: &str) -> Result<vyrn_frontend::ast::Program, ExitCode> {
    // Strip Windows' verbatim prefix (`\\?\C:\..`) — it survives neither the
    // slash normalization nor readable diagnostics.
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let (result, warnings) = vyrn_frontend::load_warned(source, &root_key, &opts, &resolver);
    // Pins are kept even when a later stage fails — fetched is pinned.
    save_lock(&resolver)?;
    match result {
        Ok(p) => {
            if print_warnings(&warnings, &root_key) {
                return Err(ExitCode::FAILURE);
            }
            Ok(p)
        }
        Err(diags) => {
            for d in &diags {
                let file = d.file.as_deref().unwrap_or(&root_key);
                eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
                if let Some(note) = &d.note {
                    eprintln!("  note: {note}");
                }
            }
            Err(ExitCode::FAILURE)
        }
    }
}

/// Share every projection expansion for the rest of this command
/// ([`vyrn_frontend::project::Memo`], RFC-0101 M2d).
///
/// `a[i]` and `for x in c` over a user container inline a `place at` / `place
/// nth` AT the access site, so the nodes an engine walks there are nodes the
/// source does not contain. Without this every engine expands for itself: the
/// lowering, the interpreter and each backend land on three sets of addresses,
/// and a side table keyed by address — `own`'s rows, `movecheck`'s, the
/// lowering's own — cannot reach any but its own. With it there is one tree per
/// site, typed by the checker `vyrn_lower::lower` runs, and every engine reads
/// the same answers. Until RFC-0101 M6's second phase this was opened by the
/// corpus gate only, so every residue number the RFC records was measured under
/// a sharing a released compiler did not do.
///
/// **After the load, deliberately.** The loader runs generators (RFC-0021) by
/// loading and checking whole programs of their own and throwing them away, and
/// a memo keyed by node address over a program that dies is the leak the
/// `Memo` doc warns about — with the verification bill still attached. What
/// this covers is the one program the command is about, from the point it is
/// linked to the point it has been lowered and emitted.
fn shared_desugars(
    program: &vyrn_frontend::ast::Program,
) -> (
    vyrn_frontend::project::Memo,
    vyrn_frontend::own::Memo<'_>,
    RefusalScope,
) {
    (
        vyrn_frontend::project::Memo::open(),
        vyrn_frontend::own::Memo::open(program),
        RefusalScope::open(),
    )
}

/// Everything the kernel refuses from here on belongs to THIS program.
///
/// The loader runs generators by loading and judging whole programs of their
/// own (RFC-0021), and each fills the same thread-local. Clearing here, once,
/// at the point the command's program is linked, is what `kernel_refuses` used
/// to do by re-analysing — which under the ownership memo (RFC-0125 §3 M3, the
/// repetition slice) would re-analyse nothing and print nothing.
struct RefusalScope;

impl RefusalScope {
    fn open() -> RefusalScope {
        let _ = vyrn_lower::take_refusals();
        RefusalScope
    }
}

/// Print a load's warnings to stderr, in the same `file:line:col:` shape errors
/// use with a `warning: ` marker. Returns whether the run should FAIL — only
/// under `--deny-warnings`, and never otherwise (RFC-0071 M2b).
fn print_warnings(warnings: &[vyrn_frontend::diagnostics::Diagnostic], root_key: &str) -> bool {
    if warnings.is_empty() {
        return false;
    }
    for d in warnings {
        let file = d.file.as_deref().unwrap_or(root_key);
        eprintln!("{}:{}:{}: warning: {}", file, d.line, d.col, d.message);
        if let Some(note) = &d.note {
            eprintln!("  note: {note}");
        }
    }
    if deny_warnings() {
        eprintln!(
            "error: {} warning(s) — refused by --deny-warnings",
            warnings.len()
        );
        return true;
    }
    false
}

/// `vyrn add <specifier> [--name alias]` — fetch + pin a remote module and
/// record it in vyrn.json's dependencies.
fn add(rest: &[String], _offline: bool) -> ExitCode {
    let Some(spec) = rest.first().filter(|s| !s.starts_with('-')) else {
        eprintln!("usage: vyrn add <github:|gist:|https: specifier> [--name alias]");
        return ExitCode::from(2);
    };
    let spec = if spec.ends_with(".vyrn") || spec.ends_with(".json") {
        spec.clone()
    } else {
        format!("{spec}.vyrn")
    };
    if !vyrn_frontend::loader::is_remote(&spec) {
        eprintln!("error: `add` takes a remote specifier (github:/gist:/https:)");
        return ExitCode::FAILURE;
    }
    let alias = match rest.iter().position(|a| a == "--name") {
        Some(i) => match rest.get(i + 1) {
            Some(a) => a.clone(),
            None => {
                eprintln!("error: --name needs a value");
                return ExitCode::from(2);
            }
        },
        None => Path::new(&spec)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dep".to_string()),
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = nearest_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found — run `vyrn new` or create one first");
        return ExitCode::FAILURE;
    };

    // Fetch + pin now, so `add` fails fast on typos and the build is offline-
    // ready immediately.
    let resolver = make_resolver(&format!("{}/vyrn.json", manifest.dir));
    if let Err(e) = vyrn_frontend::loader::ModuleResolver::read(&resolver, &spec) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    if save_lock(&resolver).is_err() {
        return ExitCode::FAILURE;
    }

    // Record the alias in vyrn.json (a small textual JSON rewrite through the
    // frontend's parser + this serializer keeps key order stable). The document
    // is the one this command already read.
    let manifest_path = Path::new(&manifest.dir).join("vyrn.json");
    use vyrn_frontend::schema::Json;
    let mut fields = match manifest.doc {
        Json::Obj(f) => f,
        _ => Vec::new(),
    };
    let dep_entry = (alias.clone(), Json::Str(spec.clone()));
    match fields.iter_mut().find(|(k, _)| k == "dependencies") {
        Some((_, Json::Obj(deps))) => {
            deps.retain(|(k, _)| k != &alias);
            deps.push(dep_entry);
        }
        Some((_, other)) => *other = Json::Obj(vec![dep_entry]),
        None => fields.push(("dependencies".into(), Json::Obj(vec![dep_entry]))),
    }
    if let Err(e) = std::fs::write(&manifest_path, json_pretty(&Json::Obj(fields), 0)) {
        eprintln!("error: cannot write {}: {e}", manifest_path.display());
        return ExitCode::FAILURE;
    }
    println!("added `{alias}` -> {spec}");
    ExitCode::SUCCESS
}

/// Fetch every published artifact of one pinned tool and record it in the lock
/// as `tool:<name>@<version>/<platform> ⇥ url ⇥ sha256` (RFC-0102 M1).
///
/// The fetch is the one remote modules already take — `curl -sL --fail`, hash,
/// `write_blob` — so a tool blob lands in `~/.vyrn/cache/sha256` beside every
/// other pinned byte and `vyrn vendor` picks it up with no new rule.
///
/// Every platform, not just this one: the hash of an artifact is a fact about
/// the artifact, and a machine that can reach the network can record it for a
/// machine that cannot. A platform whose artifact does not exist upstream is
/// reported and skipped rather than fatal — a tool that never shipped an arm64
/// build must not stop the three platforms it did ship.
fn update_tool(name: &str, version: &str, lock: &mut remote::Lock) -> Result<(), String> {
    use vyrn_frontend::toolpin;
    // The command exists to fetch; offline it can only refuse, before it
    // touches the lock (the retain below would already have dropped pins).
    if env_offline() {
        return Err(format!(
            "{name} {version} must be fetched from the network, and --offline / \
             VYRN_OFFLINE forbids it"
        ));
    }
    // A version bump must not leave the old version's lines behind: they would
    // read as pins nothing points at, and `vyrn vendor` would keep copying them.
    lock.entries
        .retain(|k, _| !k.starts_with(&format!("tool:{name}@")));
    lock.dirty = true;
    let mut pinned = 0;
    for platform in toolpin::tool_platforms(name) {
        let url = toolpin::tool_url(name, version, platform)?;
        println!("fetching {name} {version} for {platform}");
        let bytes = match remote::fetch(&url) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  no artifact for {platform}: {e}");
                continue;
            }
        };
        let sha = remote::sha256_hex(&bytes);
        remote::write_blob(&remote::cache_dir(), &sha, &bytes)?;
        lock.entries.insert(
            toolpin::tool_spec(name, version, platform),
            (url, sha.clone()),
        );
        println!("  pinned {platform} {sha}");
        pinned += 1;
    }
    if pinned == 0 {
        return Err(format!(
            "{name} {version} has no published artifact for any of {} — check the version",
            toolpin::tool_platforms(name).join(", ")
        ));
    }
    Ok(())
}

/// Make this machine hold what the lock ALREADY pins for it, and change nothing
/// (`vyrn update --locked`, RFC-0102 M4).
///
/// This is the other half of a pin, and CI is the caller that needs it: a cache
/// miss leaves `~/.vyrn/tools` empty, and the resolver deliberately never
/// reaches the network. `update_tool` would fetch — and would then write
/// whatever arrived into the lock, which is the one thing a CI run must not do.
/// So the fetch is the LOCKED one: the URL and the hash both come from the lock,
/// a mismatch is [`remote::upstream_changed`], and the lock is never rewritten.
///
/// This machine's platform only, unlike `update_tool`: recording a hash for
/// another platform is a fact a networked machine gathers deliberately; running
/// a build is not the moment to download three artifacts nothing here will run.
fn verify_tool(
    name: &str,
    version: &str,
    lock: &remote::Lock,
    project_dir: Option<&str>,
) -> Result<(), String> {
    use vyrn_frontend::toolpin;
    let platform = if toolpin::tool_platforms(name) == ["any"] {
        "any".to_string()
    } else {
        toolpin::host_platform()
    };
    let spec = toolpin::tool_spec(name, version, &platform);
    // The resolver first, so a machine that already has the bytes — cached,
    // vendored or unpacked — needs no network at all and takes exactly the path
    // the build takes.
    if let Ok(dir) = toolpin::pinned_tool(project_dir, lock, name, version) {
        println!("{spec} -> {}", dir.display());
        return Ok(());
    }
    let Some((url, sha)) = lock.entries.get(&spec).cloned() else {
        // No entry for this platform: the resolver's own refusal names the
        // platforms the lock does cover and the command that adds one.
        return toolpin::pinned_tool(project_dir, lock, name, version).map(|_| ());
    };
    if env_offline() {
        // The same refusal the resolver gives a locked miss: the pin is fine,
        // the bytes are simply not on this machine and the network is off.
        return Err(format!(
            "`{spec}` is locked (sha256 {sha}) but not cached, and this is an \
             offline build — run once online, `vyrn vendor`, or drop any copy \
             of the file with that hash into the cache"
        ));
    }
    println!("fetching {name} {version} for {platform}");
    let bytes = remote::fetch(&url)?;
    let got = remote::sha256_hex(&bytes);
    if got != sha {
        return Err(remote::upstream_changed(
            &spec,
            &url,
            &got,
            &sha,
            &format!("vyrn update {name}"),
        ));
    }
    remote::write_blob(&remote::cache_dir(), &sha, &bytes)?;
    let dir = toolpin::pinned_tool(project_dir, lock, name, version)?;
    println!("  verified {sha}\n  {spec} -> {}", dir.display());
    Ok(())
}

/// `vyrn update [--locked] [alias|tool]` — re-resolve floating refs (all remote
/// deps, or just one alias) and rewrite their pins; and, for a name in the
/// manifest's `toolchain`, fetch every platform's published artifact and pin it
/// too (RFC-0102 M1).
///
/// `--locked` is the same command with the writing taken out: nothing is
/// re-resolved and the lock is never saved, so what it does is make the caches
/// hold what the lock already says — fetching only what is missing, verifying
/// every byte against the recorded hash, and refusing a mismatch.
fn update(alias: Option<&str>, locked: bool) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = nearest_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found");
        return ExitCode::FAILURE;
    };
    let (lock_path, project_dir) = lock_home(&format!("{}/vyrn.json", manifest.dir));
    let mut lock = load_lock(lock_path);
    let tools: Vec<(String, String)> = manifest
        .toolchain
        .iter()
        .filter(|(name, _)| alias.is_none_or(|a| a == name))
        .cloned()
        .collect();
    let targets: Vec<(String, String)> = manifest
        .dependencies
        .iter()
        .filter(|(name, spec)| {
            vyrn_frontend::loader::is_remote(spec) && alias.is_none_or(|a| a == name)
        })
        .map(|(n, s)| {
            let s = if s.ends_with(".vyrn") || s.ends_with(".json") {
                s.clone()
            } else {
                format!("{s}.vyrn")
            };
            (n.clone(), s)
        })
        .collect();
    if targets.is_empty() && tools.is_empty() {
        // A tool the table knows, asked for by name, with nothing in the
        // manifest to resolve it: "nothing to update" would read as "already up
        // to date" for what is really a missing declaration.
        if let Some(a) = alias.filter(|a| vyrn_frontend::toolpin::KNOWN_TOOLS.contains(a)) {
            eprintln!(
                "error: vyrn.json declares no `toolchain.{a}` — add it, then run \
                 `vyrn update {a}` to pin it"
            );
            return ExitCode::FAILURE;
        }
        eprintln!("nothing to update");
        return ExitCode::SUCCESS;
    }
    // The tools first, and every platform of each: the lock a team shares is the
    // lock CI reads, and a lock that only covers the machine that wrote it makes
    // every other machine take the refusal.
    for (name, version) in &tools {
        let r = if locked {
            verify_tool(name, version, &lock, project_dir.as_deref())
        } else {
            update_tool(name, version, &mut lock)
        };
        if let Err(e) = r {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }
    for (name, spec) in &targets {
        // A locked run reads each dependency through its EXISTING pin, which
        // verifies the hash and refuses a changed upstream on the way. Removing
        // the entry is what makes a normal run re-resolve it.
        if !locked {
            lock.entries.remove(spec);
            lock.dirty = true;
            println!("re-resolving `{name}` ({spec})");
        } else if !lock.entries.contains_key(spec) {
            // `--locked` resolves nothing the lock does not already pin. Letting
            // an unpinned spec reach the resolver would fetch it over the
            // network and report success — exactly what the flag exists to
            // refuse (the pinned specs below verify their hashes on the way).
            eprintln!(
                "error: `{name}` ({spec}) is not pinned in vyrn.lock — run \
                 `vyrn update {name}` once online to pin it"
            );
            return ExitCode::FAILURE;
        }
    }
    let resolver = remote::RemoteResolver {
        lock: std::cell::RefCell::new(lock),
        project_dir,
        offline: env_offline(),
    };
    for (_, spec) in &targets {
        if let Err(e) = vyrn_frontend::loader::ModuleResolver::read(&resolver, spec) {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }
    // `--locked` writes nothing: that is the whole difference, and it is one
    // line rather than a promise made in a doc comment.
    if !locked && save_lock(&resolver).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `vyrn vendor [--check]` — copy every locked blob into ./vyrn_vendor (or
/// verify it is already there), making the checkout self-contained forever.
fn vendor(check: bool) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = nearest_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found");
        return ExitCode::FAILURE;
    };
    let (lock_path, _) = lock_home(&format!("{}/vyrn.json", manifest.dir));
    let lock = load_lock(lock_path);
    let vend = remote::vendor_dir(&manifest.dir);
    let cache = remote::cache_dir();
    let mut missing = 0;
    for (spec, (_, sha)) in &lock.entries {
        let vendored = vend.join(sha);
        if vendored.is_file() {
            let ok = std::fs::read(&vendored)
                .map(|b| remote::sha256_hex(&b) == *sha)
                .unwrap_or(false);
            if ok {
                continue;
            }
            eprintln!("corrupt vendor blob for `{spec}` ({sha})");
            missing += 1;
            continue;
        }
        if check {
            eprintln!("missing from vendor: `{spec}` ({sha})");
            missing += 1;
            continue;
        }
        let cached = cache.join(sha);
        match std::fs::read(&cached) {
            Ok(bytes) if remote::sha256_hex(&bytes) == *sha => {
                if let Err(e) =
                    std::fs::create_dir_all(&vend).and_then(|_| std::fs::write(&vendored, &bytes))
                {
                    eprintln!("error: cannot vendor `{spec}`: {e}");
                    return ExitCode::FAILURE;
                }
                println!("vendored `{spec}`");
            }
            _ => {
                eprintln!(
                    "cannot vendor `{spec}`: not in the cache — run the build once                      (online) first"
                );
                missing += 1;
            }
        }
    }
    if missing > 0 {
        eprintln!(
            "{missing} entr{} not vendored",
            if missing == 1 { "y" } else { "ies" }
        );
        return ExitCode::FAILURE;
    }
    println!(
        "vendor is complete ({} entr{})",
        lock.entries.len(),
        if lock.entries.len() == 1 { "y" } else { "ies" }
    );
    ExitCode::SUCCESS
}

/// `s` as a JSON string literal, quotes included.
///
/// Rust's `Debug` escapes (`\u{1}`) are NOT valid JSON — `\u` must be followed
/// by exactly four hex digits — so a manifest rewritten through [`json_pretty`]
/// with Debug escapes would be unreadable to every later command. Short forms
/// where JSON defines one, `\u00xx` for every other control character, and
/// nothing else escaped: any codepoint above `0x1F` may stand as itself.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pretty-print a Json value (4-space indent, stable key order).
fn json_pretty(j: &vyrn_frontend::schema::Json, depth: usize) -> String {
    use vyrn_frontend::schema::Json;
    let pad = "    ".repeat(depth + 1);
    let close = "    ".repeat(depth);
    match j {
        Json::Null => "null".into(),
        Json::Bool(b) => b.to_string(),
        Json::Num(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Json::Str(s) => json_string(s),
        Json::Arr(items) => {
            if items.is_empty() {
                return "[]".into();
            }
            let inner: Vec<String> = items
                .iter()
                .map(|v| format!("{pad}{}", json_pretty(v, depth + 1)))
                .collect();
            format!("[\n{}\n{close}]", inner.join(",\n"))
        }
        Json::Obj(fields) => {
            if fields.is_empty() {
                return "{}".into();
            }
            let inner: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{pad}{}: {}", json_string(k), json_pretty(v, depth + 1)))
                .collect();
            format!("{{\n{}\n{close}}}", inner.join(",\n"))
        }
    }
}

/// `vyrn build <file.vyrn> [-o out] [--target wasm]` — a native executable via
/// textual IR and clang, or a `wasm32-wasi` module emitted directly (RFC-0077 M5:
/// no clang, no wasi sysroot, no builtins archive).
/// `vyrn test [file] [--name <substring>]` (RFC-0015) — load + check the root
/// file, then run its `test` blocks under the interpreter in declaration order.
/// Prints `test "name" ... ok` / `... FAILED: <message>` per test and a
/// `N passed, M failed` summary; exits 1 if any test failed. A file with no
/// tests prints `no tests` and exits 0.
fn test_cmd(path: &str, rest: &[String], engine: Engine) -> ExitCode {
    // Optional `--name <substring>` filter.
    let mut filter: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--name" && i + 1 < rest.len() {
            filter = Some(rest[i + 1].clone());
            i += 2;
        } else {
            eprintln!("test: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let program = match load_program(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
    // A file with no root-module tests: nothing to run.
    let has_tests = program.tests.iter().any(|t| t.module.is_none());
    if !has_tests {
        println!("no tests");
        return ExitCode::SUCCESS;
    }
    if engine == Engine::Wasm {
        let bodies: Vec<Body> = program
            .tests
            .iter()
            .filter(|t| t.module.is_none() && filter.as_deref().is_none_or(|s| t.name.contains(s)))
            .map(|t| Body {
                name: t.name.clone(),
                body: t.body.clone(),
                line: t.line,
            })
            .collect();
        return bodies_wasm(path, &program, "test", &bodies);
    }

    use std::io::Write;
    // The result line prints AFTER the body runs, so any `print` output the test
    // produced has already streamed to stdout (RFC-0015 "print passes through").
    let on_result = |name: &str, result: &Result<(), String>| {
        let mut stdout = std::io::stdout();
        match result {
            Ok(()) => {
                let _ = writeln!(stdout, "test {name:?} ... ok");
            }
            Err(msg) => {
                let _ = writeln!(stdout, "test {name:?} ... FAILED: {msg}");
            }
        }
        let _ = stdout.flush();
    };
    match vyrn_frontend::interp::run_tests(&program, filter.as_deref(), on_result) {
        Ok((passed, failed)) => {
            println!("\n{passed} passed, {failed} failed");
            if failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `vyrn bench [file] [--name <substring>] [--check | --json | --compare <b> [--threshold <f>]]`
/// (RFC-0055 + RFC-0063) — benchmark the root file's `bench` blocks. Modes:
///
/// - **default (native):** a program transform lowers each selected bench body to
///   an ordinary function and synthesizes a `main` harness (warmup / auto-scale /
///   sample / stats / print — plain Vyrn over `std/bench` + `std/time`), then
///   compiles it NATIVE via clang (same discovery/errors as `vyrn build`) and runs
///   it. Timing the interpreter would be a lie; divan-class numbers mean optimized
///   machine code. Report is min/median/mean per iteration with human units.
/// - **`--check`:** run each selected body ONCE under the interpreter and print
///   `bench "name" ... ok` / a trap message — deterministic, byte-pinnable, no
///   timing. Exit 1 if any trapped. This is the CI face.
/// - **`--json`** (RFC-0063): the machine-readable report, built by the Vyrn
///   harness via `std/json` and printed to stdout. Composes with `--name`.
/// - **`--compare <baseline.json>` `[--threshold <factor>]`** (RFC-0063): run,
///   then compare each bench's MIN against the baseline of the same name —
///   `ok` / `REGRESSED xN.NN` / `new` / `missing-from-run`, exit 1 iff any
///   regressed (`min > baselineMin * threshold`, default `1.5`).
///
/// `--check` is mutually exclusive with `--json`/`--compare` (deterministic vs
/// timing). Root-file benches only, declaration order (the RFC-0015 rules
/// verbatim); `--name` filters by substring; manifest-aware like every command.
fn bench_cmd(path: &str, rest: &[String], engine: Engine) -> ExitCode {
    let mut filter: Option<String> = None;
    let mut check = false;
    let mut json = false;
    let mut compare: Option<String> = None;
    let mut threshold: f64 = 1.5;
    let mut ungate: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--name" && i + 1 < rest.len() {
            filter = Some(rest[i + 1].clone());
            i += 2;
        } else if rest[i] == "--check" {
            check = true;
            i += 1;
        } else if rest[i] == "--json" {
            json = true;
            i += 1;
        } else if rest[i] == "--compare" && i + 1 < rest.len() {
            compare = Some(rest[i + 1].clone());
            i += 2;
        } else if rest[i] == "--ungate" && i + 1 < rest.len() {
            ungate = Some(rest[i + 1].clone());
            i += 2;
        } else if rest[i] == "--threshold" && i + 1 < rest.len() {
            match rest[i + 1].parse::<f64>() {
                Ok(t) if t > 0.0 => threshold = t,
                _ => {
                    eprintln!("bench: --threshold needs a positive number");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else {
            eprintln!("bench: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }

    // `--check` is the deterministic face; `--json`/`--compare` capture timings.
    // They are mutually exclusive (RFC-0063 §1).
    if check && (json || compare.is_some()) {
        eprintln!("bench: --check cannot be combined with --json or --compare");
        return ExitCode::from(2);
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let program = match load_program(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);

    // Root-file benches only (RFC-0055), in declaration order, name-filtered.
    let matches = |name: &str| filter.as_deref().is_none_or(|sub| name.contains(sub));
    let has_selected = program
        .benches
        .iter()
        .any(|b| b.module.is_none() && matches(&b.name));
    if !has_selected {
        println!("no benches");
        return ExitCode::SUCCESS;
    }

    if check && engine == Engine::Wasm {
        let bodies: Vec<Body> = program
            .benches
            .iter()
            .filter(|b| b.module.is_none() && matches(&b.name))
            .map(|b| Body {
                name: b.name.clone(),
                body: b.body.clone(),
                line: b.line,
            })
            .collect();
        return bodies_wasm(path, &program, "bench", &bodies);
    }
    if check {
        return bench_check(&program, filter.as_deref());
    }
    if let Some(baseline) = compare {
        return bench_compare(
            path,
            filter.as_deref(),
            &baseline,
            threshold,
            ungate.as_deref(),
        );
    }
    // `--json` streams the machine-readable report; the default streams the human
    // report. Neither captures the child's stdout.
    let (code, _) = bench_native(path, filter.as_deref(), json, false);
    code
}

/// `--check`: run each selected bench body once under the interpreter and pin the
/// output byte-for-byte (declaration order, trap continuation, exit codes).
fn bench_check(program: &vyrn_frontend::ast::Program, filter: Option<&str>) -> ExitCode {
    use std::io::Write;
    let on_result = |name: &str, result: &Result<(), String>| {
        let mut stdout = std::io::stdout();
        match result {
            Ok(()) => {
                let _ = writeln!(stdout, "bench {name:?} ... ok");
            }
            Err(msg) => {
                let _ = writeln!(stdout, "bench {name:?} ... FAILED: {msg}");
            }
        }
        let _ = stdout.flush();
    };
    match vyrn_frontend::interp::run_benches(program, filter, on_result) {
        Ok((ok, failed)) => {
            println!("\n{ok} ok, {failed} failed");
            if failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Default mode: transform the loaded program (lift bench bodies to ordinary
/// functions + synthesize the harness `main`, linking `std/bench`), compile it
/// NATIVE via clang, and run it so it prints real timings.
fn bench_native(
    path: &str,
    filter: Option<&str>,
    json: bool,
    capture: bool,
) -> (ExitCode, Option<String>) {
    use vyrn_frontend::ast::{Block, Expr, Function, Stmt, Type};

    // 1. Pull in the harness runtime (`std/bench` + its transitive `std/time`
    //    and `std/json`) by re-reading the user's source with the import
    //    APPENDED and loading that. Appended, not prepended: every original line
    //    keeps its number, so a trap inside a bench body still names the right
    //    line. An import is legal anywhere at top level.
    //
    //    This is ONE load on purpose. It used to be two — the user's program,
    //    plus a synthetic root importing `std/bench` — with the runtime's
    //    declarations merged in afterwards, "skipping any name the program
    //    already has". That key was the bare name, so a std module's PRIVATE
    //    function was dropped whenever the root program happened to declare the
    //    same name, and the module's own calls then bound to the root's body.
    //    A program defining its own `twoDecimals` printed `min XX µs`: the
    //    harness had called the user's function to format its timings, with no
    //    error. The loader already prevents this (name-privacy auto-rename,
    //    RFC-0046 §3) but only across modules it can see in a single load, which
    //    is exactly what the second load hid from it.
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return (ExitCode::from(2), None);
        }
    };
    let mut program = match load_program(
        path,
        &format!(
            "{source}
import {{ benchOne }} from \"std/bench\"
"
        ),
    ) {
        Ok(p) => p,
        Err(code) => return (code, None),
    };

    // 2. Lift each selected root bench body into an ordinary Unit function
    //    `__vyrn_bench_body_<slot>` (declaration order). `blackBox` inside is fine:
    //    the program is already checked, and codegen — which we go to next without
    //    re-checking — lowers `blackBox` directly.
    let selected: Vec<vyrn_frontend::ast::BenchDecl> = program
        .benches
        .iter()
        .filter(|b| b.module.is_none() && filter.is_none_or(|sub| b.name.contains(sub)))
        .cloned()
        .collect();
    let mut harness_stmts: Vec<Stmt> = Vec::new();
    let mut width = 0i64;
    for b in &selected {
        // label is `bench "<name>"` → 7 for `bench "` + name + 1 for `"`.
        let w = (b.name.len() + 8) as i64;
        if w > width {
            width = w;
        }
    }
    // Lift each body; collect the per-bench `benchMeasure(...)` calls (for `--json`)
    // in parallel with the human `benchOne(...)` statements.
    let mut measure_calls: Vec<Expr> = Vec::new();
    for (slot, b) in selected.iter().enumerate() {
        program.functions.push(Function {
            name: format!("__vyrn_bench_body_{slot}"),
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: b.body.clone(),
            line: b.line,
            col: 0,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        });
        let body_ref = Expr::Var {
            name: format!("__vyrn_bench_body_{slot}"),
            line: 0,
        };
        if json {
            measure_calls.push(Expr::Call {
                name: "benchMeasure".to_string(),
                args: vec![Expr::Str(b.name.clone()), body_ref],
                line: 0,
            });
        } else {
            harness_stmts.push(Stmt::Expr(Expr::Call {
                name: "benchOne".to_string(),
                args: vec![Expr::Str(b.name.clone()), Expr::Int(width), body_ref],
                line: 0,
            }));
        }
    }
    if json {
        // `print(benchJson([benchMeasure(..), ..], "native", "O2"))` — the whole
        // machine-readable report emitted from Vyrn via `std/json` (RFC-0063 §1).
        // The array literal coerces to `Array<BenchResult>` from `benchJson`'s
        // parameter type; declaration order is preserved.
        harness_stmts.push(Stmt::Expr(Expr::Call {
            name: "print".to_string(),
            args: vec![Expr::Call {
                name: "benchJson".to_string(),
                args: vec![
                    Expr::ArrayLit {
                        elems: measure_calls,
                        line: 0,
                    },
                    Expr::Str("native".to_string()),
                    Expr::Str("O2".to_string()),
                ],
                line: 0,
            }],
            line: 0,
        }));
    } else {
        // Footer: a blank line, then the count (mirrors `vyrn test`'s summary shape).
        harness_stmts.push(Stmt::Expr(Expr::Call {
            name: "print".to_string(),
            args: vec![Expr::Str(String::new())],
            line: 0,
        }));
        harness_stmts.push(Stmt::Expr(Expr::Call {
            name: "print".to_string(),
            args: vec![Expr::Str(format!("{} benches", selected.len()))],
            line: 0,
        }));
    }
    harness_stmts.push(Stmt::Return {
        value: Some(Expr::Int(0)),
        line: 0,
    });

    // 3. Replace the user's `main` (bench mode ignores it) with the harness.
    program.functions.retain(|f| f.name != "main");
    program.functions.push(Function {
        name: "main".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: Default::default(),
        params: Vec::new(),
        ret: Type::Int,
        body: Block {
            stmts: harness_stmts,
        },
        line: 0,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    });
    // Benches/tests are now either lifted or irrelevant — drop them so nothing
    // downstream mistakes them for live code.
    program.benches.clear();
    program.tests.clear();

    // 4. Emit IR + shim, compile native via clang into a temp dir, and run it.
    //    The same target `vyrn build` would ship, or the measurement stops
    //    describing the artifact — the bug `-O2` just was.
    let target = match native_target_for(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return (ExitCode::FAILURE, None);
        }
    };
    let ir = match vyrn_codegen::emit(&program) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error: {e}");
            return (ExitCode::FAILURE, None);
        }
    };
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bench");
    let dir = std::env::temp_dir().join(format!(
        "vyrn-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create temp dir {}: {e}", dir.display());
        return (ExitCode::FAILURE, None);
    }
    let exe_name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    let out_path = dir.join(&exe_name);
    let ll_path = out_path.with_extension("ll");
    let shim_path = out_path.with_extension("shim.c");
    if let Err(e) = std::fs::write(&ll_path, ir) {
        eprintln!("error: cannot write {}: {e}", ll_path.display());
        let _ = std::fs::remove_dir_all(&dir);
        return (ExitCode::FAILURE, None);
    }
    let mut shim = runtime_shim();
    shim.push_str(&extern_trap_stubs(&program));
    if let Err(e) = std::fs::write(&shim_path, &shim) {
        eprintln!("error: cannot write {}: {e}", shim_path.display());
        let _ = std::fs::remove_dir_all(&dir);
        return (ExitCode::FAILURE, None);
    }
    let clang = match find_clang() {
        Some(c) => c,
        None => {
            eprintln!(
                "error: could not find `clang`. Install LLVM and put clang on PATH, \
                 or set the CLANG environment variable to its full path."
            );
            let _ = std::fs::remove_dir_all(&dir);
            return (ExitCode::FAILURE, None);
        }
    };
    let mut cmd = Command::new(&clang);
    cmd.arg(&ll_path).arg(&shim_path).arg("-o").arg(&out_path);
    add_native_clang_flags(&mut cmd, target);
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("error: clang exited with {s}");
            let _ = std::fs::remove_dir_all(&dir);
            return (ExitCode::FAILURE, None);
        }
        Err(e) => {
            eprintln!("error: failed to run clang ({}): {e}", clang.display());
            let _ = std::fs::remove_dir_all(&dir);
            return (ExitCode::FAILURE, None);
        }
    }
    // Run the compiled harness. When `capture` is set (`--compare`), grab its
    // stdout as the JSON report to feed the comparator; otherwise let stdout and
    // stderr stream straight through (the `--json` and human paths both print live).
    // VYRN_BENCH_KEEP: leave the temp dir (the .ll, the shim, the binary) for
    // a debugger — a bench binary that heap-faults dies before its report
    // line, and the artifacts are all there is to read (round fifty-eight).
    let keep = std::env::var_os("VYRN_BENCH_KEEP").is_some();
    let cleanup = |dir: &std::path::Path| {
        if keep {
            eprintln!("VYRN_BENCH_KEEP: artifacts left in {}", dir.display());
        } else {
            let _ = std::fs::remove_dir_all(dir);
        }
    };
    let (code, out) = if capture {
        match Command::new(&out_path).output() {
            Ok(o) => {
                // stderr still surfaces (traps, diagnostics); only stdout is captured.
                use std::io::Write;
                let _ = std::io::stderr().write_all(&o.stderr);
                (
                    (o.status.code().unwrap_or(1) & 0xff) as u8,
                    Some(String::from_utf8_lossy(&o.stdout).into_owned()),
                )
            }
            Err(e) => {
                eprintln!(
                    "error: failed to run bench binary ({}): {e}",
                    out_path.display()
                );
                cleanup(&dir);
                return (ExitCode::FAILURE, None);
            }
        }
    } else {
        match Command::new(&out_path).status() {
            Ok(s) => ((s.code().unwrap_or(1) & 0xff) as u8, None),
            Err(e) => {
                eprintln!(
                    "error: failed to run bench binary ({}): {e}",
                    out_path.display()
                );
                cleanup(&dir);
                return (ExitCode::FAILURE, None);
            }
        }
    };
    cleanup(&dir);
    (ExitCode::from(code), out)
}

/// The per-bench minimum times (name → minNs) extracted from a `--json` report or
/// a `bench/baseline.json` baseline. Declaration order is preserved (a `Vec`, not a
/// map) so the comparison prints in the run's order. Returns `None` if `doc` is not
/// the expected `{ benches: [ { name, minNs } ] }` shape.
/// The bench names an ungate file lists: one per line, `#` starts a comment,
/// blank lines ignored. The reasons live in the file beside the names, which is
/// the point of it being a file — a gate turned off without a written reason is
/// a gate nobody can turn back on.
fn bench_ungate_list(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| match l.find('#') {
            Some(i) => &l[..i],
            None => l,
        })
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn bench_min_table(doc: &vyrn_frontend::schema::Json) -> Option<Vec<(String, f64)>> {
    use vyrn_frontend::schema::Json;
    let benches = match doc.get("benches") {
        Some(Json::Arr(items)) => items,
        _ => return None,
    };
    let mut out = Vec::new();
    for b in benches {
        let name = match b.get("name") {
            Some(Json::Str(s)) => s.clone(),
            _ => return None,
        };
        let min = match b.get("minNs") {
            Some(Json::Num(n)) => *n,
            _ => return None,
        };
        out.push((name, min));
    }
    Some(out)
}

/// A baseline is a "placeholder" (seed, not yet refreshed from real CI hardware)
/// when it carries `"placeholder": true` OR has an empty `benches` array. `--compare`
/// then treats every run bench as `new` and never regresses (RFC-0063 §2).
fn baseline_is_placeholder(doc: &vyrn_frontend::schema::Json) -> bool {
    use vyrn_frontend::schema::Json;
    if let Some(Json::Bool(true)) = doc.get("placeholder") {
        return true;
    }
    matches!(doc.get("benches"), Some(Json::Arr(items)) if items.is_empty())
}

/// `vyrn bench --compare <baseline.json> [--threshold <factor>]` (RFC-0063 §2) —
/// run the benches (native, capturing the `--json` report), then compare each
/// bench's MIN against the baseline entry of the same name:
///
/// - `ok` — `min <= baselineMin * threshold`;
/// - `REGRESSED xN.NN` — `min > baselineMin * threshold` (the factor is `min /
///   baselineMin`); the ONLY verdict that fails the command (exit 1);
/// - `new` — in the run, absent from the baseline (informational);
/// - `missing-from-run` — in the baseline, absent from the run (informational).
///
/// A placeholder/empty baseline makes every bench `new` (exit 0): comparing a real
/// run against a not-yet-seeded baseline is meaningless, never a failure.
fn bench_compare(
    path: &str,
    filter: Option<&str>,
    baseline_path: &str,
    threshold: f64,
    ungate_path: Option<&str>,
) -> ExitCode {
    // Read + parse the baseline first — a broken baseline is a usage error, and
    // failing before the (slow) native run gives quick feedback.
    let baseline_text = match std::fs::read_to_string(baseline_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read baseline {baseline_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let baseline_doc = match vyrn_frontend::schema::parse_json(&baseline_text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {baseline_path} is not valid JSON: {e}");
            return ExitCode::from(2);
        }
    };
    let placeholder = baseline_is_placeholder(&baseline_doc);
    let baseline = if placeholder {
        Vec::new()
    } else {
        match bench_min_table(&baseline_doc) {
            Some(t) => t,
            None => {
                eprintln!("error: {baseline_path} is not a bench report (expected `benches: [ {{ name, minNs }} ]`)");
                return ExitCode::from(2);
            }
        }
    };

    // Run the benches native, capturing the machine-readable report.
    let (run_code, captured) = bench_native(path, filter, true, true);
    let run_json = match captured {
        Some(j) => j,
        None => return run_code, // the run failed; its error already printed
    };
    let run_doc = match vyrn_frontend::schema::parse_json(&run_json) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: bench --json output did not parse: {e}");
            return ExitCode::FAILURE;
        }
    };
    let run = match bench_min_table(&run_doc) {
        Some(t) => t,
        None => {
            eprintln!("error: bench --json output was not the expected shape");
            return ExitCode::FAILURE;
        }
    };

    if placeholder {
        eprintln!("note: {baseline_path} is a placeholder baseline — every bench reports `new` (refresh it from a CI --json artifact)");
    }

    // The ungate list is policy, not data: it lives in its own committed file
    // because `bench/baseline.json` is replaced verbatim from a CI artifact
    // every time it is reseeded, and a flag hand-added there would be wiped by
    // the next seeding.
    let ungated = match ungate_path {
        None => Vec::new(),
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => bench_ungate_list(&t),
            Err(e) => {
                eprintln!("error: cannot read ungate list {p}: {e}");
                return ExitCode::from(2);
            }
        },
    };

    let scale = bench_host_scale(&run, &baseline);
    if scale != 1.0 {
        println!(
            "host scale x{scale:.3} (median of the matched benches; every factor below is corrected by it)"
        );
    }
    let (verdicts, regressed) = bench_verdicts(&run, &baseline, threshold, &ungated);
    for (name, v) in &verdicts {
        println!("bench {name:?} ... {}", v.render());
    }
    if regressed > 0 {
        println!("\n{regressed} regressed (threshold x{threshold:.2})");
        ExitCode::FAILURE
    } else {
        println!("\nno regressions (threshold x{threshold:.2})");
        ExitCode::SUCCESS
    }
}

/// One bench's comparison outcome (RFC-0063 §2).
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Within threshold. The factor is the host-normalized `min / baselineMin`.
    Ok,
    /// Slower than `baselineMin * threshold`; the factor is `min / baselineMin`
    /// after host normalization (see [`bench_host_scale`]).
    Regressed(f64),
    /// Over the threshold in a bench the ungate list names: reported with its
    /// factor and NOT counted as a regression. See `bench/ungated.txt`.
    Ungated(f64),
    /// In the run, absent from the baseline (informational).
    New,
    /// In the baseline, absent from the run (informational).
    MissingFromRun,
}

impl Verdict {
    fn render(&self) -> String {
        match self {
            Verdict::Ok => "ok".to_string(),
            Verdict::Regressed(f) => format!("REGRESSED x{f:.2}"),
            Verdict::Ungated(f) => format!("x{f:.2} — not gated on this fleet"),
            Verdict::New => "new".to_string(),
            Verdict::MissingFromRun => "missing-from-run".to_string(),
        }
    }
}

/// How much slower this HOST is than the one the baseline was seeded on, as the
/// median of every matched bench's `min / baselineMin` (RFC-0063, amended).
///
/// A benchmark gate compares nanoseconds measured on one machine against
/// nanoseconds measured on another. GitHub's `ubuntu-latest` is a label over
/// several CPU generations, and the corpus measures whole-run medians drifting
/// 1.2x to 1.35x between runners — a third of a 2.0x threshold spent before a
/// single line of Vyrn changes. The median is the honest correction: a real
/// regression lands in one bench or a few and barely moves the median of
/// dozens, while a slow runner moves every row together and cancels out.
///
/// **It needs a quorum.** With three benches in front of it the median IS the
/// regression, and normalizing would divide the signal away. Below
/// [`BENCH_SCALE_QUORUM`] matched rows the answer is 1.0 and the comparison is
/// raw, which is what a per-example invocation over a whole-corpus baseline
/// mostly gets.
fn bench_host_scale(run: &[(String, f64)], baseline: &[(String, f64)]) -> f64 {
    let mut factors: Vec<f64> = run
        .iter()
        .filter_map(|(name, min)| {
            baseline
                .iter()
                .find(|(n, _)| n == name)
                .filter(|(_, base)| *base > 0.0)
                .map(|(_, base)| min / base)
        })
        .collect();
    if factors.len() < BENCH_SCALE_QUORUM {
        return 1.0;
    }
    factors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = factors.len() / 2;
    if factors.len() % 2 == 0 {
        (factors[mid - 1] + factors[mid]) / 2.0
    } else {
        factors[mid]
    }
}

/// How many matched benches [`bench_host_scale`] needs before it will correct
/// anything. Eight: enough that no single regressed row can carry the median,
/// and low enough that the corpus's larger files (membench's twenty-two rows,
/// benching's nine) still get the correction.
const BENCH_SCALE_QUORUM: usize = 8;

/// The pure comparison core (RFC-0063 §2), factored out so it is unit-testable
/// against synthetic min tables with NO clang and NO real timing. Each run bench
/// is compared by min against the same-named baseline entry; run benches come
/// first in declaration order, then baseline-only benches as `missing-from-run`.
/// Returns the per-bench verdicts and the count of REGRESSED (the exit-1 trigger).
/// A regression is `min > baselineMin * threshold`; a zero/absent baseline min is
/// `new` (can't scale, never a division by zero).
fn bench_verdicts(
    run: &[(String, f64)],
    baseline: &[(String, f64)],
    threshold: f64,
    ungated: &[String],
) -> (Vec<(String, Verdict)>, usize) {
    let lookup = |name: &str| baseline.iter().find(|(n, _)| n == name).map(|(_, m)| *m);
    let scale = bench_host_scale(run, baseline);
    let mut out = Vec::new();
    let mut regressed = 0usize;
    for (name, min) in run {
        let v = match lookup(name) {
            Some(base) if base > 0.0 => {
                // The host correction divides the BASELINE up to this runner's
                // speed rather than dividing the measurement down, so the
                // factor a reader sees is still `this run / that baseline` on
                // comparable hardware.
                let factor = (min / base) / scale;
                if factor > threshold {
                    if ungated.iter().any(|u| u == name) {
                        Verdict::Ungated(factor)
                    } else {
                        regressed += 1;
                        Verdict::Regressed(factor)
                    }
                } else {
                    Verdict::Ok
                }
            }
            _ => Verdict::New,
        };
        out.push((name.clone(), v));
    }
    for (name, _) in baseline {
        if !run.iter().any(|(n, _)| n == name) {
            out.push((name.clone(), Verdict::MissingFromRun));
        }
    }
    (out, regressed)
}

/// `vyrn serve [file] [--port N]` (RFC-0016) — a hand-rolled HTTP/1.1 host on
/// `std::net` (no crates), running the file's `handle` under the interpreter.
/// Sequential accept loop, one request at a time: module state is race-free by
/// construction. Default port 8080.
fn serve_cmd(path: &str, rest: &[String]) -> ExitCode {
    // Optional `--port N` (default 8080).
    let mut port: u16 = 8080;
    let mut workers: Option<usize> = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--port" && i + 1 < rest.len() {
            match rest[i + 1].parse::<u16>() {
                Ok(p) => port = p,
                Err(_) => {
                    eprintln!("serve: --port needs a number in 0..=65535");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else if rest[i] == "--workers" && i + 1 < rest.len() {
            match rest[i + 1].parse::<usize>() {
                Ok(n) if n >= 1 => workers = Some(n),
                _ => {
                    eprintln!("serve: --workers needs a positive number");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else {
            eprintln!("serve: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let program = match load_program(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);

    // `vyrn serve` requires `fn handle(req: Request) -> Response` (exactly this
    // signature — the checker's no-`main` exemption uses the same rule).
    use vyrn_frontend::ast::Type;
    let has_handle = program.functions.iter().any(|f| {
        f.name == "handle"
            && !f.is_extern
            && f.params.len() == 1
            && f.params[0].ty == Type::Named("Request".to_string())
            && f.ret == Type::Named("Response".to_string())
    });
    if !has_handle {
        eprintln!("error: `vyrn serve` needs `fn handle(req: Request) -> Response` in {path}");
        return ExitCode::FAILURE;
    }

    // Bind before running `main`, so a port clash fails fast and cleanly. A
    // `--port 0` lets the OS pick a free port; report the one it chose.
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let actual_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let file_label = path.to_string();

    // `--workers N` (RFC-0025): N worker threads, each owning an independent
    // interpreter, gated on the isolation analysis — refused (with the call
    // path) when `handle` touches module state.
    if let Some(n) = workers {
        if let Some(exit) = refuse_workers_if_stateful(&program) {
            return exit;
        }
        let (tx, rx) = std::sync::mpsc::channel::<std::net::TcpStream>();
        let rx = std::sync::Mutex::new(rx);
        let result = vyrn_frontend::interp::serve_pool(
            &program,
            n,
            |_i, call_handle| loop {
                // spmc over std: each idle worker takes the next connection.
                let stream = rx.lock().unwrap().recv();
                match stream {
                    Ok(mut s) => serve_one(&mut s, call_handle),
                    Err(_) => break, // accept loop gone; drain out
                }
            },
            move || {
                use std::io::Write;
                let _ = std::io::stdout().flush();
                eprintln!(
                    "serving {file_label} on http://localhost:{actual_port} with {n} workers"
                );
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            if tx.send(s).is_err() {
                                break;
                            }
                        }
                        Err(_) => continue,
                    }
                }
                Ok(())
            },
        );
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // The interpreter thread owns one live `Interp` (module state persists); it
    // runs `main` once, then invokes this accept loop with a per-request handler.
    let result = vyrn_frontend::interp::serve(&program, move |call_handle| {
        use std::io::Write;
        // `main` (if any) has already run; flush its stdout so its startup
        // output precedes the serving banner regardless of buffering mode.
        let _ = std::io::stdout().flush();
        eprintln!("serving {file_label} on http://localhost:{actual_port}");
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => serve_one(&mut s, call_handle),
                Err(_) => continue,
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The RFC-0025 worker gate: `--workers` requires a module-state-free `handle`
/// (transitively — the existing isolation analysis answers the question).
/// Prints the refusal naming the offending call path and returns the exit code
/// when parallel serving is unsound; `None` means workers are fine. Other
/// effects (`print`, file I/O) are deliberately allowed — each log/output line
/// stays atomic; only shared mutable state gates parallelism.
fn refuse_workers_if_stateful(program: &vyrn_frontend::ast::Program) -> Option<ExitCode> {
    // RFC-0037: calls through stored function values dispatch over the
    // signature's collected sources — the checker's collection feeds the walk.
    let stored = vyrn_frontend::checker::stored_fn_effects(program);
    let (chain, global) = vyrn_frontend::checker::module_state_use(program, "handle", &stored)?;
    let path = chain
        .iter()
        .map(|f| format!("`{f}`"))
        .collect::<Vec<_>>()
        .join(" -> ");
    eprintln!(
        "error: `--workers` needs a module-state-free `handle`: {path} reads or writes \
         module state `{global}` (shared by definition) — run without `--workers` for \
         the sequential loop"
    );
    Some(ExitCode::FAILURE)
}

/// `vyrn dev [--port N]` (RFC-0019) — the fullstack convenience command.
///
/// Reads `vyrn.json`'s `"server"` / `"client"` (+ optional `"public"`, default
/// `public`), builds the client to wasm (a plain wasm build — no roles), then
/// serves the server root's `handle` over HTTP with static assets in front.
///
/// Routing precedence (LOCKED): a GET whose path names an existing static asset
/// is served from disk; everything else — every POST, and any GET that is not a
/// static file (so all of `/rpc/*`) — goes to the server's `handle`. Static
/// sources, in order: the built `/client.wasm`, the runtimes under
/// `/vyrn-runtime/<name>`, then files under the public dir (`/` → `index.html`).
fn dev_cmd(rest: &[String]) -> ExitCode {
    let mut port: u16 = 8080;
    let mut workers: Option<usize> = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--port" && i + 1 < rest.len() {
            match rest[i + 1].parse::<u16>() {
                Ok(p) => port = p,
                Err(_) => {
                    eprintln!("dev: --port needs a number in 0..=65535");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else if rest[i] == "--workers" && i + 1 < rest.len() {
            match rest[i + 1].parse::<usize>() {
                Ok(n) if n >= 1 => workers = Some(n),
                _ => {
                    eprintln!("dev: --workers needs a positive number");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else {
            eprintln!("dev: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = nearest_manifest(&cwd) else {
        eprintln!("error: `vyrn dev` needs a vyrn.json with `server` and `client` keys");
        return ExitCode::FAILURE;
    };
    // The document this command already read, not a second read of it.
    let doc = &manifest.doc;
    use vyrn_frontend::schema::Json;
    let get_str = |key: &str| -> Option<String> {
        match doc.get(key) {
            Some(Json::Str(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let Some(server_rel) = get_str("server") else {
        eprintln!("error: vyrn.json is missing a `\"server\"` entry (the module with `handle`)");
        return ExitCode::FAILURE;
    };
    let Some(client_rel) = get_str("client") else {
        eprintln!("error: vyrn.json is missing a `\"client\"` entry (the wasm module to build)");
        return ExitCode::FAILURE;
    };
    let public_rel = get_str("public").unwrap_or_else(|| "public".to_string());
    let server_path = format!("{}/{server_rel}", manifest.dir);
    let client_path = format!("{}/{client_rel}", manifest.dir);
    let public_dir = PathBuf::from(format!("{}/{public_rel}", manifest.dir));

    let Some(web_dir) = web_root() else {
        eprintln!("error: could not find the `web/` runtime directory (set VYRN_WEB)");
        return ExitCode::FAILURE;
    };

    // Build the client to wasm into a dev scratch dir served at `/client.wasm`.
    let dev_dir = PathBuf::from(format!("{}/.vyrn-dev", manifest.dir));
    if let Err(e) = std::fs::create_dir_all(&dev_dir) {
        eprintln!("error: cannot create {}: {e}", dev_dir.display());
        return ExitCode::FAILURE;
    }
    let wasm_out = dev_dir.join("client.wasm");
    let _ = std::fs::remove_file(&wasm_out); // a stale wasm must not mask a failed build
    eprintln!("dev: building client {client_rel} -> wasm");
    let build_code = build(
        &client_path,
        &[
            "--target".to_string(),
            "wasm".to_string(),
            "-o".to_string(),
            wasm_out.to_string_lossy().into_owned(),
        ],
    );
    if !wasm_out.is_file() {
        return build_code;
    }

    // Load the server root (must define `handle`, like `vyrn serve`).
    let source = match std::fs::read_to_string(&server_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {server_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let program = match load_program(&server_path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
    use vyrn_frontend::ast::Type;
    let has_handle = program.functions.iter().any(|f| {
        f.name == "handle"
            && !f.is_extern
            && f.params.len() == 1
            && f.params[0].ty == Type::Named("Request".to_string())
            && f.ret == Type::Named("Response".to_string())
    });
    if !has_handle {
        eprintln!(
            "error: the server root `{server_rel}` needs `fn handle(req: Request) -> Response`"
        );
        return ExitCode::FAILURE;
    }

    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let actual_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let assets = DevAssets {
        public_dir,
        web_dir,
        wasm: wasm_out,
    };

    let banner = move |assets: &DevAssets| {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        eprintln!("dev: serving {server_rel} on http://localhost:{actual_port}");
        eprintln!("dev:   /rpc/*         -> server `handle` (rpcHandle + your pages)");
        eprintln!("dev:   /client.wasm   -> built from {client_rel}");
        eprintln!(
            "dev:   /vyrn-runtime/ -> web runtimes (wasi-min.js, vyrn-rpc.js, vyrn-query.js)"
        );
        eprintln!("dev:   /              -> {}/", assets.public_dir.display());
    };

    // `--workers N` passes through to the same RFC-0025 pool as `vyrn serve`,
    // behind the same module-state gate.
    if let Some(n) = workers {
        if let Some(exit) = refuse_workers_if_stateful(&program) {
            return exit;
        }
        let (tx, rx) = std::sync::mpsc::channel::<std::net::TcpStream>();
        let rx = std::sync::Mutex::new(rx);
        let assets = &assets;
        let result = vyrn_frontend::interp::serve_pool(
            &program,
            n,
            |_i, call_handle| loop {
                let stream = rx.lock().unwrap().recv();
                match stream {
                    Ok(mut s) => dev_serve_one(&mut s, assets, call_handle),
                    Err(_) => break,
                }
            },
            move || {
                banner(assets);
                eprintln!("dev:   workers        -> {n}");
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            if tx.send(s).is_err() {
                                break;
                            }
                        }
                        Err(_) => continue,
                    }
                }
                Ok(())
            },
        );
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let result = vyrn_frontend::interp::serve(&program, move |call_handle| {
        banner(&assets);
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => dev_serve_one(&mut s, &assets, call_handle),
                Err(_) => continue,
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Static asset roots for `vyrn dev`.
struct DevAssets {
    public_dir: PathBuf,
    web_dir: String,
    wasm: PathBuf,
}

/// Resolve a GET path to a static file per the locked precedence, or `None` if
/// no static asset matches (so the request falls through to `handle`). No
/// traversal out of a root: `..` segments are refused on the raw path AND on
/// the backslash-normalized form, absolute and drive-letter targets are
/// refused (`Path::join` would replace the base for those), and the join is
/// confirmed by canonicalizing both sides — the last word over symlinks and
/// any separator trick the string checks missed.
fn dev_static_path(path: &str, assets: &DevAssets) -> Option<PathBuf> {
    // Strip a query string; work on the raw path.
    let raw = path.split('?').next().unwrap_or(path);
    if raw.split('/').any(|seg| seg == "..") {
        return None;
    }
    if raw == "/client.wasm" {
        return assets.wasm.is_file().then(|| assets.wasm.clone());
    }
    if let Some(name) = raw.strip_prefix("/vyrn-runtime/") {
        if !name.is_empty() {
            return file_under(Path::new(&assets.web_dir), &safe_rel(name)?);
        }
        return None;
    }
    // Public dir: `/` → index.html, else the path under public/.
    let rel = if raw == "/" {
        "index.html".to_string()
    } else {
        raw.trim_start_matches('/').to_string()
    };
    file_under(&assets.public_dir, &safe_rel(&rel)?)
}

/// Normalize a request path into a relative path that cannot escape by
/// itself: backslashes become `/` (a Windows separator the `..` filter never
/// saw), and absolute or drive-letter forms (`/C:/…`, `C:\…`) are refused,
/// because [`Path::join`] replaces its base for those.
fn safe_rel(name: &str) -> Option<String> {
    let norm = name.replace('\\', "/");
    if norm.starts_with('/') || norm.split('/').any(|seg| seg == "..") {
        return None;
    }
    let b = norm.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None;
    }
    Some(norm)
}

/// Join `root` with a checked relative path, then prove the result stayed
/// inside: canonicalize both and require the prefix. Returns the joined path.
fn file_under(root: &Path, rel: &str) -> Option<PathBuf> {
    let p = root.join(rel);
    let croot = root.canonicalize().ok()?;
    let cp = p.canonicalize().ok()?;
    cp.starts_with(croot).then_some(p)
}

/// The `Content-Type` for a static asset, by extension.
fn dev_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// One `vyrn dev` connection: static-first for a matching GET, otherwise the
/// server's `handle` (all POSTs, `/rpc/*`, and non-file GETs).
fn dev_serve_one(
    stream: &mut std::net::TcpStream,
    assets: &DevAssets,
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeCall,
    ) -> Result<vyrn_frontend::interp::ServeAnswer, String>,
) {
    use vyrn_frontend::interp::{ServeAnswer, ServeCall};
    let req = match parse_request(stream) {
        Ok(r) => r,
        Err(ParseError::Chunked { method, path }) => {
            eprintln!("{method} {path} -> 501");
            write_response(
                stream,
                501,
                "text/plain",
                b"chunked transfer-encoding not supported",
            );
            return;
        }
        Err(ParseError::TooLarge { method, path }) => {
            eprintln!("{method} {path} -> 413");
            write_response(stream, 413, "text/plain", b"request body too large");
            return;
        }
        Err(ParseError::Bad) => {
            eprintln!("- - -> 400");
            write_response(stream, 400, "text/plain", b"bad request");
            return;
        }
    };
    // The browser-origin gate runs before anything else — static assets
    // included: a cross-site page gets nothing from this server at all.
    if let Some(body) = cross_origin_body(&req) {
        write_cross_origin_refusal(stream, &req.method, &req.path, &body);
        return;
    }
    // Static assets: GET (or HEAD) only, so nothing shadows a POST /rpc/*.
    if req.method == "GET" || req.method == "HEAD" {
        if let Some(file) = dev_static_path(&req.path, assets) {
            match std::fs::read(&file) {
                Ok(bytes) => {
                    eprintln!("{} {} -> 200 (static)", req.method, req.path);
                    if req.method == "HEAD" {
                        // RFC 9110 §9.3.2: HEAD sends the headers GET would,
                        // true Content-Length included, and no body.
                        write_head_response(stream, 200, dev_content_type(&file), bytes.len());
                    } else {
                        write_response(stream, 200, dev_content_type(&file), &bytes);
                    }
                }
                Err(_) => {
                    eprintln!("{} {} -> 500", req.method, req.path);
                    write_response(stream, 500, "text/plain", b"cannot read asset");
                }
            }
            return;
        }
    }
    // Otherwise: into Vyrn's `handle` (rpcHandle + the app's own routes).
    let method = req.method.clone();
    let path = req.path.clone();
    match call_handle(ServeCall::Handle(req)) {
        Ok(ServeAnswer::Live(head)) => {
            eprintln!("{method} {path} -> {} (stream)", head.status);
            pump_stream(stream, &head, call_handle);
        }
        Ok(ServeAnswer::Buffered(resp)) => {
            eprintln!("{method} {path} -> {}", resp.status);
            write_response_vary(
                stream,
                resp.status,
                &resp.content_type,
                &resp.vary,
                &resp.headers,
                resp.body.as_bytes(),
            );
        }
        Ok(_) => {
            eprintln!("{method} {path} -> 500");
            write_response(stream, 500, "text/plain", b"internal error");
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!("{method} {path} -> 500");
            write_response(stream, 500, "text/plain", b"internal error");
        }
    }
}

/// The value of a lowercased request-header name.
fn request_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Whether `host` names the loopback interface this server bound. Anything else
/// is a DNS-rebinding page aiming its own domain at 127.0.0.1 — the socket is
/// local either way, so the peer address cannot tell them apart.
fn loopback_host(host: &str) -> bool {
    // Strip the port; an IPv6 literal keeps its brackets (`[::1]:8080`).
    let bare = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        host.split(':').next().unwrap_or(host)
    };
    // A host name is case-insensitive (RFC 9110 §4.2.3), so `LOCALHOST` is the
    // same host and the gate must not refuse it.
    bare.eq_ignore_ascii_case("localhost") || matches!(bare, "127.0.0.1" | "::1")
}

/// Whether `origin`'s authority is `host`. A browser writes the same
/// non-default port in both places (`http://localhost:8080` ↔ `Host:
/// localhost:8080`), so the authorities compare directly.
fn origin_is_host(origin: &str, host: &str) -> bool {
    let rest = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|a| a.eq_ignore_ascii_case(host))
}

/// The browser-origin gate every served request passes before Vyrn's `handle`.
///
/// Binding the loopback interface scopes the server to this machine's PROCESSES,
/// not this machine's programs: any web page can drive the visitor's browser at
/// `http://localhost:8080` — reading responses needs CORS, but writing never did
/// (a cross-site form POST, a no-cors `fetch`, a WebSocket handshake), and a
/// hijacked socket answers with the app's own authority. Two checks close that
/// door, applied ahead of everything else so the 101 upgrade path is covered:
///
/// - **Host** must name the loopback host the server bound, which refuses a
///   rebound domain before anything else runs.
/// - **Origin**, when a client sent one, must name the same authority as Host.
///   Browsers attach it to every cross-site request and every WebSocket
///   handshake, so the mismatch is exactly the cross-site case; a client that
///   sends no Origin — `curl`, scripts, the test harnesses — is not a page and
///   has no site to be cross of, so it passes, upgrade or not.
///
/// The `Some` answer is the refusal body.
fn cross_origin_body(req: &vyrn_frontend::interp::ServeRequest) -> Option<String> {
    let Some(host) = request_header(&req.headers, "host") else {
        return Some("request without a Host header".to_string());
    };
    if !loopback_host(host) {
        return Some(format!(
            "host `{host}` is not this server's loopback address"
        ));
    }
    match request_header(&req.headers, "origin") {
        Some(o) if !origin_is_host(o, host) => {
            Some(format!("cross-origin request from `{o}` refused"))
        }
        _ => None,
    }
}

/// Answer a gate refusal the way every other rejection leaves this host: logged,
/// plain-text 403, connection closed by [`write_response`].
fn write_cross_origin_refusal(
    stream: &mut std::net::TcpStream,
    method: &str,
    path: &str,
    body: &str,
) {
    eprintln!("{method} {path} -> 403 ({body})");
    write_response(stream, 403, "text/plain", b"cross-origin request refused");
}

/// Why a request never reached Vyrn.
enum ParseError {
    /// Malformed request line/headers → 400.
    Bad,
    /// A `Transfer-Encoding: chunked` body (unsupported in v1) → 501. Carries
    /// the parsed method/path so the access line can still be logged.
    Chunked { method: String, path: String },
    /// A body larger than [`MAX_BODY`] → 413. Carries the parsed method/path
    /// for the access line, the same way `Chunked` does.
    TooLarge { method: String, path: String },
}

/// The largest request body this server will hold, 8 MiB.
///
/// The header block has been guarded since RFC-0019, and the BODY was not: the
/// read loop grew its buffer to whatever `Content-Length` announced, so one
/// connection could make the process hold an unbounded amount of memory. The
/// loopback bind and the cross-origin gate do not help here — a script that
/// sends no `Origin` passes that gate by design, and any process on this
/// machine can open the socket.
///
/// 8 MiB is well past a form post or an RPC call and well under anything that
/// hurts; a program that needs to receive more than this wants a streaming
/// upload path, which v1 does not have.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Handle one connection: parse the request, call Vyrn's `handle`, write the
/// response, close. Malformed input answers 400 without reaching Vyrn; a chunked
/// body answers 501; a Vyrn trap is logged and answered 500 (the server keeps
/// running — one bad request must not kill it).
fn serve_one(
    stream: &mut std::net::TcpStream,
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeCall,
    ) -> Result<vyrn_frontend::interp::ServeAnswer, String>,
) {
    use vyrn_frontend::interp::{ServeAnswer, ServeCall};
    match parse_request(stream) {
        Ok(req) => {
            // The same browser-origin gate `dev` answers through, ahead of
            // `handle` — the 101 upgrade path included.
            if let Some(body) = cross_origin_body(&req) {
                write_cross_origin_refusal(stream, &req.method, &req.path, &body);
                return;
            }
            let method = req.method.clone();
            let path = req.path.clone();
            match call_handle(ServeCall::Handle(req)) {
                Ok(ServeAnswer::Live(head)) => {
                    eprintln!("{method} {path} -> {} (stream)", head.status);
                    pump_stream(stream, &head, call_handle);
                }
                Ok(ServeAnswer::Buffered(resp)) => {
                    eprintln!("{method} {path} -> {}", resp.status);
                    write_response_vary(
                        stream,
                        resp.status,
                        &resp.content_type,
                        &resp.vary,
                        &resp.headers,
                        resp.body.as_bytes(),
                    );
                }
                Ok(_) => {
                    eprintln!("{method} {path} -> 500");
                    write_response(stream, 500, "text/plain", b"internal error");
                }
                Err(msg) => {
                    // Canonical trap wording to stderr, then a generic 500.
                    eprintln!("error: {msg}");
                    eprintln!("{method} {path} -> 500");
                    write_response(stream, 500, "text/plain", b"internal error");
                }
            }
        }
        Err(ParseError::Chunked { method, path }) => {
            eprintln!("{method} {path} -> 501");
            write_response(
                stream,
                501,
                "text/plain",
                b"chunked transfer-encoding not supported",
            );
        }
        Err(ParseError::TooLarge { method, path }) => {
            eprintln!("{method} {path} -> 413");
            write_response(stream, 413, "text/plain", b"request body too large");
        }
        Err(ParseError::Bad) => {
            eprintln!("- - -> 400");
            write_response(stream, 400, "text/plain", b"bad request");
        }
    }
}

/// Find the first occurrence of `needle` in `hay`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parse one HTTP/1.1 request off the wire: request line, headers (case-
/// insensitive) up to CRLF CRLF, then exactly `Content-Length` body bytes.
fn parse_request(
    stream: &mut std::net::TcpStream,
) -> Result<vyrn_frontend::interp::ServeRequest, ParseError> {
    use std::io::Read;
    // Read until the header terminator (CRLF CRLF), guarding header size.
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        if let Some(p) = find_subslice(&buf, b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 64 * 1024 {
            return Err(ParseError::Bad);
        }
        match stream.read(&mut tmp) {
            Ok(0) => return Err(ParseError::Bad), // closed before headers done
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return Err(ParseError::Bad),
        }
    };
    // The header block is ASCII by protocol.
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| ParseError::Bad)?;
    let mut lines = head.split("\r\n");

    // Request line: METHOD SP TARGET SP HTTP/x.y
    let request_line = lines.next().ok_or(ParseError::Bad)?;
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::Bad)?
        .to_string();
    let target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::Bad)?
        .to_string();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return Err(ParseError::Bad);
    }

    // Headers: `name: value`, name compared case-insensitively. Every field is
    // kept for the program (RFC-0072 M4), lowercased HERE — the one place a
    // header name crosses from the wire into a Vyrn `Map`, whose lookup is exact.
    // Repeated fields join with `", "`, which RFC 9110 §5.3 makes equivalent to
    // sending them separately.
    let mut content_length: usize = 0;
    let mut chunked = false;
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(ParseError::Bad)?;
        let lname = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if lname == "content-length" {
            content_length = value.parse::<usize>().map_err(|_| ParseError::Bad)?;
        } else if lname == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
        match headers.iter_mut().find(|(n, _)| *n == lname) {
            Some((_, prev)) => {
                prev.push_str(", ");
                prev.push_str(value);
            }
            None => headers.push((lname, value.to_string())),
        }
    }
    if chunked {
        return Err(ParseError::Chunked {
            method,
            path: target,
        });
    }
    // Refused on the ANNOUNCED length, before a byte of the body is read: the
    // point is not to read it and then complain.
    if content_length > MAX_BODY {
        return Err(ParseError::TooLarge {
            method,
            path: target,
        });
    }

    // Body: exactly `content_length` bytes (some already buffered after the
    // header terminator). Absent Content-Length ⇒ no body.
    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let need = content_length - body.len();
        let mut chunk = vec![0u8; need.min(8192)];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(ParseError::Bad),
        }
    }
    body.truncate(content_length);
    // A Vyrn `String` is UTF-8; a body that isn't is a bad request (lossy
    // decoding would silently corrupt it).
    let body = String::from_utf8(body).map_err(|_| ParseError::Bad)?;

    Ok(vyrn_frontend::interp::ServeRequest {
        method,
        path: target,
        headers,
        body,
    })
}

/// A minimal status-code → reason-phrase table. Unknown codes get an empty
/// reason (the space after the code is still required by the grammar).
fn reason_phrase(status: i64) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        413 => "Content Too Large",
        418 => "I'm a teapot",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// Pump one open stream onto the wire (RFC-0074 M3a) — the second response
/// shape, written here rather than by [`write_response_vary`] because nothing it
/// does applies: there is no `Content-Length` (the body ends when the connection
/// does), and a `Vary`/`ETag`/304 is about a representation that exists all at
/// once.
///
/// **The disconnect signal is the write.** The loop is: pull one frame, write it,
/// flush, and the FIRST time any of those fails, `Close` — which runs RFC-0075's
/// release path for the producer. Nothing here asks the host which event means
/// "the client is gone", because that question has a different answer on every
/// deployment; a failing write is the socket rather than the framework, so it is
/// the same everywhere. The consequence is the strong form of RFC-0075's
/// conformance row: the release runs before the next frame would be produced,
/// since the next pull is the statement after the one that failed.
///
/// The first frame is pulled BEFORE the header block goes out, for one reason
/// that is worth the round trip: a producer with nothing to say answers `204 No
/// Content`, which is the one status a plain `EventSource` treats as "stop, do
/// not reconnect" (WHATWG HTML §9.2.5). So a stream that ends normally is not an
/// infinite reconnect loop — the client comes back once, is told 204, and stops.
fn pump_stream(
    stream: &mut std::net::TcpStream,
    head: &vyrn_frontend::interp::ServeResponse,
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeCall,
    ) -> Result<vyrn_frontend::interp::ServeAnswer, String>,
) {
    use std::io::Write;
    use vyrn_frontend::interp::ServeCall;

    // A `101` is `ws` (RFC-0074 M3b). The status is the discriminator because the
    // protocol already made it one: a WebSocket handshake IS a 101, so nothing had
    // to be invented to tell the two adapters apart.
    if head.status == 101 {
        pump_socket(stream, head, call_handle);
        return;
    }

    let first = pull_frame(call_handle);
    let Some(first) = first else {
        let _ = call_handle(ServeCall::Close);
        write_response(stream, 204, "", b"");
        return;
    };

    let mut extra = String::new();
    for (name, value) in &head.headers {
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    let reason = reason_phrase(head.status);
    let header = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nCache-Control: no-store\r\n{extra}Connection: close\r\n\r\n",
        head.status, head.content_type
    );
    // `body` is the stream's PROLOGUE, not a response: `retry:` belongs before
    // the first event and after the header block, and it is the one thing the
    // program writes that is not a frame.
    let opened = stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(head.body.as_bytes()))
        .and_then(|_| stream.write_all(first.as_bytes()))
        .and_then(|_| stream.flush());
    if opened.is_err() {
        let _ = call_handle(ServeCall::Close);
        return;
    }
    loop {
        let Some(frame) = pull_frame(call_handle) else {
            break;
        };
        if stream
            .write_all(frame.as_bytes())
            .and_then(|_| stream.flush())
            .is_err()
        {
            break;
        }
    }
    let _ = call_handle(ServeCall::Close);
}

/// Ask the open stream for one element. A trapping producer ends the connection
/// the way a trapping handler ends a request: logged, and the server keeps
/// running. Shared by both adapters, which is most of what "the signal
/// generalises" means in code.
fn pull_frame(
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeCall,
    ) -> Result<vyrn_frontend::interp::ServeAnswer, String>,
) -> Option<String> {
    use vyrn_frontend::interp::{ServeAnswer, ServeCall};
    match call_handle(ServeCall::Next) {
        Ok(ServeAnswer::Frame(f)) => f,
        Ok(_) => None,
        Err(msg) => {
            eprintln!("error: {msg}");
            None
        }
    }
}

/// Pump one open stream onto a WebSocket (RFC-0074 M3b) — the second adapter,
/// and the test of whether M3a's disconnect signal generalises past one
/// transport. It does: the loop below is `pump_stream`'s, with `write_all` of a
/// raw frame replaced by `write_all` of a framed one. Nothing asks the host which
/// event means the client is gone.
///
/// **The host frames and Vyrn yields the payload**, which is RFC-0074's rule that
/// Vyrn owns what the user chooses and the host owns what the protocol fixes.
/// There is no choice in an opcode, a length or a mask, so none of them is a
/// design surface and none of them is spellable in `std/http`.
///
/// Two numbers the host cannot choose ride in the head's `body`: the close code
/// and the fragment limit. A 101 has no prologue — after the handshake everything
/// is a frame — so that slot carries what the host needs before it can write one.
///
/// **Server-push only.** Inbound frames are parsed rather than ignored, because
/// §5.1 makes a client's frames masked and §5.5.1 makes a close frame something a
/// server must answer; but there is no handler for a client's message, and there
/// is nothing in this RFC that would say what one looks like.
fn pump_socket(
    stream: &mut std::net::TcpStream,
    head: &vyrn_frontend::interp::ServeResponse,
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeCall,
    ) -> Result<vyrn_frontend::interp::ServeAnswer, String>,
) {
    use std::io::Write;
    use vyrn_frontend::interp::ServeCall;

    // `closeCode` and `maxFrame`, in the slot SSE uses for its prologue.
    let mut nums = head.body.split_whitespace();
    let mut close_code: u16 = nums.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
    let max_frame: usize = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let mut extra = String::new();
    for (name, value) in &head.headers {
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    // No `Connection: close` and no `Content-Length`: the connection is the
    // point, and what follows the header block is frames rather than a body.
    let handshake = format!("HTTP/1.1 101 Switching Protocols\r\n{extra}\r\n");
    if stream
        .write_all(handshake.as_bytes())
        .and_then(|_| stream.flush())
        .is_err()
    {
        let _ = call_handle(ServeCall::Close);
        return;
    }

    let mut inbox: Vec<u8> = Vec::new();
    loop {
        let Some(payload) = pull_frame(call_handle) else {
            break;
        };
        if ws_write_message(stream, payload.as_bytes(), max_frame).is_err() {
            // The disconnect signal, unchanged from `sse`: the write failed, so
            // the client is gone, so the producer is released — before the next
            // element would have been produced.
            let _ = call_handle(ServeCall::Close);
            return;
        }
        match ws_drain(stream, &mut inbox) {
            WsIn::Open => {}
            // §5.5.1: a close frame is answered with a close frame.
            WsIn::Closed => break,
            // §5.1: a frame from a client that is not masked.
            WsIn::Protocol => {
                close_code = 1002;
                break;
            }
        }
    }
    let _ = ws_write_frame(stream, 8, true, &close_code.to_be_bytes());
    let _ = stream.flush();
    let _ = call_handle(ServeCall::Close);
}

/// What the inbound half of a socket has to say between two outbound messages.
/// There is deliberately no "the peer is gone" answer here: **that is the write's
/// to give.** A reader could see EOF a fraction earlier than the next write fails,
/// and taking the earlier one would give this adapter a second disconnect signal
/// — which is exactly the per-deployment ambiguity M3a exists to refuse. So the
/// inbound half reports only the two things it alone knows.
enum WsIn {
    /// Nothing that ends the connection — including EOF and a read error.
    Open,
    /// A close frame from the client.
    Closed,
    /// A frame that breaks RFC 6455 — close with 1002.
    Protocol,
}

/// Read whatever inbound bytes are waiting, without waiting for any.
///
/// The socket goes non-blocking for the read alone and back before the next
/// write, which matters more than it looks: a non-blocking WRITE can answer
/// `WouldBlock`, and this adapter reads a failed write as "the client is gone".
/// Making the disconnect signal depend on the socket's mode would be the
/// deployment-specific behaviour M3a exists to avoid.
fn ws_drain(stream: &mut std::net::TcpStream, buf: &mut Vec<u8>) -> WsIn {
    use std::io::Read;
    let mut tmp = [0u8; 2048];
    let _ = stream.set_nonblocking(true);
    let got = stream.read(&mut tmp);
    let _ = stream.set_nonblocking(false);
    match got {
        Ok(n) => buf.extend_from_slice(&tmp[..n]),
        Err(_) => {}
    }
    // Parse every complete frame; leave a partial one for the next call.
    loop {
        if buf.len() < 2 {
            return WsIn::Open;
        }
        let opcode = buf[0] & 0x0f;
        let masked = buf[1] & 0x80 != 0;
        let short = (buf[1] & 0x7f) as usize;
        let (len, head) = match short {
            126 => {
                if buf.len() < 4 {
                    return WsIn::Open;
                }
                (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4)
            }
            127 => {
                if buf.len() < 10 {
                    return WsIn::Open;
                }
                let mut n = [0u8; 8];
                n.copy_from_slice(&buf[2..10]);
                (u64::from_be_bytes(n) as usize, 10)
            }
            n => (n, 2),
        };
        if !masked {
            return WsIn::Protocol;
        }
        // ponytail: a 16 MiB ceiling on one inbound frame. Server-push has no
        // inbound message to be large, and an unbounded length would let a peer
        // name a buffer this loop then waits forever to fill.
        if len > 16 * 1024 * 1024 {
            return WsIn::Protocol;
        }
        if buf.len() < head + 4 + len {
            return WsIn::Open;
        }
        let key = [buf[head], buf[head + 1], buf[head + 2], buf[head + 3]];
        let body: Vec<u8> = buf[head + 4..head + 4 + len]
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % 4])
            .collect();
        buf.drain(..head + 4 + len);
        match opcode {
            8 => return WsIn::Closed,
            // §5.5.2: a ping must be answered with a pong carrying its payload.
            // A failed pong is not reported: the next outbound message will fail
            // the same way, and that write is the disconnect signal.
            9 => {
                let _ = ws_write_frame(stream, 10, true, &body);
            }
            // Data and pongs: read off the wire and dropped, because this
            // milestone is server-push and has nobody to hand them to.
            _ => {}
        }
    }
}

/// One message as one frame, or as a fragment sequence when `max_frame` splits
/// it: the first fragment carries the text opcode with FIN clear, the rest carry
/// the continuation opcode, and the last sets FIN (§5.4). A fragment boundary may
/// fall inside a UTF-8 sequence — the spec validates the reassembled message, not
/// the pieces.
fn ws_write_message(
    stream: &mut std::net::TcpStream,
    payload: &[u8],
    max_frame: usize,
) -> std::io::Result<()> {
    if max_frame == 0 || payload.len() <= max_frame {
        return ws_write_frame(stream, 1, true, payload);
    }
    let mut sent = 0;
    while sent < payload.len() {
        let end = (sent + max_frame).min(payload.len());
        let opcode = if sent == 0 { 1 } else { 0 };
        ws_write_frame(stream, opcode, end == payload.len(), &payload[sent..end])?;
        sent = end;
    }
    Ok(())
}

/// One frame on the wire, never masked: §5.1 forbids a server to mask.
fn ws_write_frame(
    stream: &mut std::net::TcpStream,
    opcode: u8,
    fin: bool,
    payload: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = Vec::with_capacity(payload.len() + 10);
    f.push(if fin { 0x80 | opcode } else { opcode });
    let n = payload.len();
    if n < 126 {
        f.push(n as u8);
    } else if n <= u16::MAX as usize {
        f.push(126);
        f.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        f.push(127);
        f.extend_from_slice(&(n as u64).to_be_bytes());
    }
    f.extend_from_slice(payload);
    stream.write_all(&f)?;
    stream.flush()
}

/// Write one HTTP/1.1 response: status line, `Content-Type`, `Content-Length`,
/// `Connection: close`, blank line, body. Errors are ignored — the peer may
/// have hung up, and one dropped connection must not fault the server.
fn write_response(stream: &mut std::net::TcpStream, status: i64, content_type: &str, body: &[u8]) {
    write_response_vary(stream, status, content_type, "", &[], body)
}

/// The HEAD shape of [`write_response`]: the status line and headers GET
/// would send, the length GET would declare, and no body (RFC 9110 §9.3.2 —
/// strict clients read body bytes after a HEAD as the next response's head).
/// Errors ignored, for the same reason as [`write_response`].
fn write_head_response(
    stream: &mut std::net::TcpStream,
    status: i64,
    content_type: &str,
    len: usize,
) {
    use std::io::Write;
    let header = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        reason_phrase(status)
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.flush();
}

/// [`write_response`] plus a `Vary` field (RFC-0072 M4) and the response's own
/// header map (RFC-0074 M2). `vary` empty writes no header at all, so a response
/// that does not negotiate stays byte-identical to what this host wrote before
/// the field existed; an empty `headers` likewise.
///
/// An empty `content_type` writes NO `Content-Type` line. A 304 carries no body
/// and so has no media type to declare (RFC 9110 §15.4.5 lists the fields a 304
/// SHOULD send, and `Content-Type` is not among them) — writing `Content-Type:`
/// with nothing after it would be a malformed field rather than an absent one.
fn write_response_vary(
    stream: &mut std::net::TcpStream,
    status: i64,
    content_type: &str,
    vary: &str,
    headers: &[(String, String)],
    body: &[u8],
) {
    use std::io::Write;
    let reason = reason_phrase(status);
    let type_line = if content_type.is_empty() {
        String::new()
    } else {
        format!("Content-Type: {content_type}\r\n")
    };
    let vary_line = if vary.is_empty() {
        String::new()
    } else {
        format!("Vary: {vary}\r\n")
    };
    let mut extra = String::new();
    for (name, value) in headers {
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    // RFC 9110 §8.6: a server MUST NOT send Content-Length in a 204 — the
    // status itself means there is no content, so there is no length to
    // declare. (The stream-teardown 204 arrives here through
    // [`write_response`].)
    let length_line = if status == 204 {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n{type_line}{vary_line}{extra}{length_line}Connection: close\r\n\r\n"
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// `vyrn run --engine wasm` (RFC-0125 M5): the program compiled by the direct
/// backend and run in the embedded wasmtime, with the arguments, streams and
/// exit code `vyrn run` gives the interpreter. The kernel's refusals apply as
/// they do to `build`, since this is the same route.
fn run_wasm(path: &str, program: &vyrn_frontend::ast::Program, prog_args: &[String]) -> ExitCode {
    if let Err(code) = kernel_refuses(program, path) {
        return code;
    }
    let bytes = match vyrn_codegen::direct::compile(program) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut argv = vec![path.to_string()];
    argv.extend(prog_args.iter().cloned());
    let run = wasmrun::Run {
        argv,
        stdin_prefix: Vec::new(),
        capture_stderr: false,
    };
    match wasmrun::run(&bytes, run) {
        Ok(out) => ExitCode::from((out.code & 0xff) as u8),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One `test` or `bench` body, as [`bodies_wasm`] runs it.
struct Body {
    name: String,
    body: vyrn_frontend::ast::Block,
    line: usize,
}

/// `vyrn test --engine wasm` and `vyrn bench --check --engine wasm` (RFC-0125
/// M5): the selected bodies, each run once as compiled wasm, with the lines
/// the interpreter prints.
///
/// One module, one instance per body. Each body is lifted into a function
/// `__vyrn_body_<k>`, and the synthesized `main` reads ONE line from standard
/// input and calls the body that line names; the host serves that line before
/// the process's own input. That is how the host knows which body a trap
/// belongs to without reading the program's output: a trap ends the instance
/// with `error: <message>` on fd 2 and exit 1, and the host turns the message
/// into the `FAILED:` line. `assert`, `assertEq` and `blackBox` are lowered by
/// the direct backend like every other builtin; nothing is rewritten here.
/// What differs from the interpreter, on record in RFC-0125 §3 M5: module
/// state is initialized once per body rather than once per run, and input a
/// body read ahead is not seen by the next.
fn bodies_wasm(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    kind: &str,
    bodies: &[Body],
) -> ExitCode {
    use vyrn_frontend::ast::{BinOp, Block, Expr, Function, Pattern, Stmt, Type};
    if bodies.is_empty() {
        // `test` said `no tests` before the filter; `bench` says it after.
        println!("no {kind}es");
        return ExitCode::SUCCESS;
    }
    let mut prog = program.clone();
    // The root's `main` is not run by `test` or `bench`; the harness is `main`.
    prog.functions
        .retain(|f| !(f.name == "main" && f.module.is_none()));
    prog.tests.clear();
    prog.benches.clear();
    let function = |name: String, body: Block, ret: Type, line: usize| Function {
        name,
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: Default::default(),
        params: Vec::new(),
        ret,
        body,
        line,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    };
    let mut dispatch: Vec<Stmt> = Vec::new();
    for (k, b) in bodies.iter().enumerate() {
        let name = format!("__vyrn_body_{k}");
        prog.functions
            .push(function(name.clone(), b.body.clone(), Type::Unit, b.line));
        dispatch.push(Stmt::If {
            cond: Expr::Binary {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Var {
                    name: "__vyrnSlot".to_string(),
                    line: 0,
                }),
                rhs: Box::new(Expr::Str(k.to_string())),
                line: 0,
            },
            then_block: Block {
                stmts: vec![Stmt::Expr(Expr::Call {
                    name,
                    args: Vec::new(),
                    line: 0,
                })],
            },
            else_block: None,
            line: 0,
        });
    }
    let main = Block {
        stmts: vec![
            Stmt::IfLet {
                pattern: Pattern::Some("__vyrnSlot".to_string()),
                scrutinee: Expr::Call {
                    name: "readLine".to_string(),
                    args: Vec::new(),
                    line: 0,
                },
                then_block: Block { stmts: dispatch },
                else_block: None,
                line: 0,
            },
            Stmt::Return {
                value: Some(Expr::Int(0)),
                line: 0,
            },
        ],
    };
    prog.functions
        .push(function("main".to_string(), main, Type::Int, 0));

    let bytes = match vyrn_codegen::direct::compile(&prog) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    use std::io::Write;
    let (mut ok, mut failed) = (0usize, 0usize);
    for (k, b) in bodies.iter().enumerate() {
        let run = wasmrun::Run {
            argv: vec![path.to_string()],
            stdin_prefix: format!("{k}\n").into_bytes(),
            capture_stderr: true,
        };
        let out = match wasmrun::run(&bytes, run) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        // The trap's line is the last `error: ..` on fd 2; everything the body
        // wrote there before it passes through.
        let (rest, message) = if out.code == 0 {
            (out.stderr.as_slice(), None)
        } else {
            let text = String::from_utf8_lossy(&out.stderr);
            match text.rfind("error: ") {
                Some(at) if at == 0 || text.as_bytes()[at - 1] == b'\n' => (
                    &out.stderr[..at],
                    Some(text[at + 7..].trim_end_matches('\n').to_string()),
                ),
                _ => (out.stderr.as_slice(), Some(format!("exit {}", out.code))),
            }
        };
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(rest);
        let _ = stderr.flush();
        let mut stdout = std::io::stdout().lock();
        match message {
            None => {
                ok += 1;
                let _ = writeln!(stdout, "{kind} {:?} ... ok", b.name);
            }
            Some(msg) => {
                failed += 1;
                let _ = writeln!(stdout, "{kind} {:?} ... FAILED: {msg}", b.name);
            }
        }
        let _ = stdout.flush();
    }
    let verdict = if kind == "test" { "passed" } else { "ok" };
    println!("\n{ok} {verdict}, {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn build(path: &str, rest: &[String]) -> ExitCode {
    // parse optional `-o <out>` / `--target wasm` / `--route wasm2c`
    let mut out: Option<String> = None;
    let mut wasm = false;
    let mut wasm2c = false;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "-o" && i + 1 < rest.len() {
            out = Some(rest[i + 1].clone());
            i += 2;
        } else if rest[i] == "--target" && i + 1 < rest.len() {
            match rest[i + 1].as_str() {
                "wasm" | "wasm32-wasi" => wasm = true,
                other => {
                    eprintln!("build: unknown target `{other}` (expected `wasm`)");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else if rest[i] == "--route" && i + 1 < rest.len() {
            // RFC-0125 §2.5's release route, as a flag beside the text-IR route
            // and not in its place: PLAN-0125-runtime §6 step 3 is a decision
            // the numbers in RFC-0125 §3 M4 are for, and this is what produces
            // them. `wasm2c` is the only route name; the default stays the
            // text-IR route.
            match rest[i + 1].as_str() {
                "wasm2c" => wasm2c = true,
                other => {
                    eprintln!("build: unknown route `{other}` (expected `wasm2c`)");
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else {
            eprintln!("build: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }
    if wasm && wasm2c {
        eprintln!(
            "build: `--route wasm2c` produces a native executable; it cannot take `--target wasm`"
        );
        return ExitCode::from(2);
    }

    // Resolved before anything expensive. A misspelled `nativeTarget` is a
    // config error, and reporting it after a full compile — or worse, behind a
    // "could not find clang" — helps nobody. Native only: `nativeTarget` says
    // nothing about a wasm build, so a wasm build must not fail on it.
    let native_target = if wasm {
        None
    } else {
        match native_target_for(path) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        }
    };

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let program = match load_program(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
    if let Err(code) = kernel_refuses(&program, path) {
        return code;
    }
    // default output name: <stem> (+ .exe on Windows, .wasm for wasm)
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a");
    let out_path = out.unwrap_or_else(|| {
        if wasm {
            format!("{stem}.wasm")
        } else if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    });

    // `--target wasm` is the direct backend (RFC-0077 M5), unconditionally. No
    // clang, no wasi sysroot, no builtins archive, no `.ll` and no `.shim.c` — the
    // module is written straight out.
    //
    // There is no switch here on purpose. The LLVM wasm path was kept beside this
    // one behind `VYRN_WASM_BACKEND` for the length of M2, and the flag was given a
    // deletion milestone at the same time it was introduced, because this repo has
    // already watched an ungated second backend rot to unbuildable in twelve days
    // (`vyrn-codegen-llvm`, b1eef04). Native keeps the textual-IR route below, with
    // its own parity column; wasm has this one, with its own.
    if wasm {
        return match vyrn_codegen::direct::compile(&program) {
            Ok(bytes) => match std::fs::write(&out_path, bytes) {
                Ok(()) => {
                    println!("wrote {out_path}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: cannot write {out_path}: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if wasm2c {
        return build_wasm2c(
            path,
            &program,
            &out_path,
            native_target.unwrap_or(DEFAULT_NATIVE_TARGET),
        );
    }

    let ir = match vyrn_codegen::emit(&program) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // write IR + the portable stream shim next to the output so failures are
    // inspectable
    let ll_path = PathBuf::from(&out_path).with_extension("ll");
    if let Err(e) = std::fs::write(&ll_path, ir) {
        eprintln!("error: cannot write {}: {e}", ll_path.display());
        return ExitCode::FAILURE;
    }
    // The portable shim, plus a trap stub per `extern` import (RFC-0012). Native
    // has no host to supply one, so the stub satisfies the symbol by printing the
    // canonical "not available on this target" message and exiting — the same
    // wording the interpreter traps with. On wasm an `extern` resolves to the host
    // page's `vyrn` import namespace, which the direct backend declares itself.
    let shim = runtime_shim() + &extern_trap_stubs(&program);
    let shim_path = PathBuf::from(&out_path).with_extension("shim.c");
    if let Err(e) = std::fs::write(&shim_path, &shim) {
        eprintln!("error: cannot write {}: {e}", shim_path.display());
        return ExitCode::FAILURE;
    }

    let clang = match find_clang() {
        Some(c) => c,
        None => {
            eprintln!(
                "error: could not find `clang`. Install LLVM and put clang on PATH, \
                 or set the CLANG environment variable to its full path."
            );
            return ExitCode::FAILURE;
        }
    };

    let mut cmd = Command::new(&clang);
    cmd.arg(&ll_path).arg(&shim_path).arg("-o").arg(&out_path);
    // Resolved at the top of `build`; the wasm path never reaches this line.
    add_native_clang_flags(&mut cmd, native_target.unwrap_or(DEFAULT_NATIVE_TARGET));
    let status = cmd.status();
    match status {
        Ok(s) if s.success() => {
            println!("wrote {out_path}");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("error: clang exited with {s}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: failed to run clang ({}): {e}", clang.display());
            ExitCode::FAILURE
        }
    }
}

/// `vyrn build --route wasm2c` (RFC-0125 §2.5; PLAN-0125-runtime §6 step 3,
/// first slice): the program's wasm — the bytes `--target wasm` writes — through
/// wasm2c to C, compiled with the WASI host of `wasi_host.c` and wabt's wasm-rt
/// by clang at the native route's own flags, into a native executable.
///
/// The intermediate files stay beside the output the way the text-IR route's
/// `.ll` and `.shim.c` do, so a failure is inspectable: `<out>.wasm`,
/// `<out>.w2c.c`, `<out>.w2c.h`, `<out>.host.c`.
fn build_wasm2c(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    out_path: &str,
    native_target: NativeTarget,
) -> ExitCode {
    let start = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let w2c = match vyrn_codegen::toolchain::wasm2c_from(&start) {
        Ok(Some(t)) => t,
        Ok(None) => {
            eprintln!(
                "error: could not find `wasm2c`. Unpack a wabt release under tools/ \
                 (tools/wabt-<version>/bin/wasm2c) or set VYRN_WASM2C to the executable."
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((simde, _)) = vyrn_codegen::toolchain::simde_from(&start) else {
        eprintln!(
            "error: could not find simde. Unpack a simde release under tools/ \
             (tools/simde/simde/wasm/simd128.h) or set VYRN_SIMDE to the directory that \
             holds `simde/`."
        );
        return ExitCode::FAILURE;
    };
    let clang = match find_clang() {
        Some(c) => c,
        None => {
            eprintln!(
                "error: could not find `clang`. Install LLVM and put clang on PATH, \
                 or set the CLANG environment variable to its full path."
            );
            return ExitCode::FAILURE;
        }
    };

    let bytes = match vyrn_codegen::direct::compile(program) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = PathBuf::from(out_path);
    let wasm_path = out.with_extension("wasm");
    let c_path = out.with_extension("w2c.c");
    let host_path = out.with_extension("host.c");
    let write = |p: &Path, data: &[u8]| -> bool {
        if let Err(e) = std::fs::write(p, data) {
            eprintln!("error: cannot write {}: {e}", p.display());
            return false;
        }
        true
    };
    if !write(&wasm_path, &bytes) {
        return ExitCode::FAILURE;
    }
    // The module name fixes the C names the host calls (`w2c_prog`,
    // `wasm2c_prog_instantiate`, `w2c_prog_0x5Fstart`); wasm2c would otherwise
    // take it from the output file's name.
    let st = Command::new(&w2c.exe)
        .arg("-n")
        .arg("prog")
        .arg(&wasm_path)
        .arg("-o")
        .arg(&c_path)
        .status();
    match st {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("error: wasm2c exited with {s}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: failed to run wasm2c ({}): {e}", w2c.exe.display());
            return ExitCode::FAILURE;
        }
    }
    if !write(&host_path, vyrn_codegen::toolchain::WASI_HOST_C.as_bytes()) {
        return ExitCode::FAILURE;
    }

    // The header is included by its bare name: the host sits beside it, and a
    // full path would put backslashes into a C string literal.
    let h_name = out
        .with_extension("w2c.h")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `show_path`: a tool found under a canonicalized start carries Windows's
    // verbatim `\\?\` prefix, which clang's `#include <simde/..>` search does
    // not take.
    let mut cmd = Command::new(&clang);
    cmd.arg(&c_path)
        .arg(&host_path)
        .arg(show_path(&w2c.runtime.join("wasm-rt-impl.c")))
        .arg(show_path(&w2c.runtime.join("wasm-rt-mem-impl.c")))
        .arg("-o")
        .arg(&out)
        .arg(format!("-I{}", show_path(&w2c.include)))
        .arg(format!("-I{}", show_path(&w2c.runtime)))
        .arg(format!("-I{}", show_path(&simde)))
        .arg(format!("-DVYRN_W2C_HEADER=\"{h_name}\""))
        // wasm-rt counts call depth where it has no guard page (Windows), at
        // 500 frames by default. Vyrn's own counter traps at
        // `CALL_DEPTH_LIMIT` (1,000) user frames, and the runtime's frames
        // (RFC-0125 M4 step 1) are not counted by it, so the host's limit sits
        // above the program's with room for those; `error: call depth exceeds
        // 1000` stays the program's wording, as under the engine.
        .arg(format!(
            "-DWASM_RT_MAX_CALL_STACK_DEPTH={}",
            4 * vyrn_frontend::interp::CALL_DEPTH_LIMIT
        ));
    // The same `-O2 -ffp-contract=off -march=..` the text-IR route ships, so
    // the two routes' numbers differ by the route and nothing else.
    add_native_clang_flags(&mut cmd, native_target);
    if cfg!(windows) {
        // `random_get` is `BCryptGenRandom`, as in `wasmrun.rs`.
        cmd.arg("-lbcrypt");
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            println!("wrote {out_path}");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("error: clang exited with {s}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: failed to run clang ({}): {e}", clang.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the `bench --compare` core (RFC-0063 §2). The comparison is
    //! pure — synthetic min tables in, verdicts out — so these need no clang and
    //! assert NO real timing numbers.
    use super::*;

    fn table(entries: &[(&str, f64)]) -> Vec<(String, f64)> {
        entries.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }

    /// The host correction (RFC-0063, amended): a runner uniformly slower than
    /// the one that seeded the baseline cancels out, because the median moves
    /// with every row. Ten rows all 1.3x slow read `ok` at a 2.0 threshold,
    /// where the raw comparison spends two thirds of the headroom on the
    /// machine before Vyrn changes anything.
    #[test]
    fn a_uniformly_slower_host_is_corrected_away() {
        let base: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 100.0)).collect();
        let run: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 130.0)).collect();
        assert!((bench_host_scale(&run, &base) - 1.3).abs() < 1e-9);
        let (v, regressed) = bench_verdicts(&run, &base, 2.0, &[]);
        assert_eq!(regressed, 0);
        assert!(v.iter().all(|(_, x)| matches!(x, Verdict::Ok)));
    }

    /// And it does not hide a real one: nine rows steady on a host that is
    /// itself 1.3x slow, one row at 3x. The median is 1.3, so the regressed
    /// row still reads 2.3x and fails.
    #[test]
    fn one_regressed_row_survives_the_host_correction() {
        let base: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 100.0)).collect();
        let mut run: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 130.0)).collect();
        run[3].1 = 300.0;
        let (v, regressed) = bench_verdicts(&run, &base, 2.0, &[]);
        assert_eq!(regressed, 1);
        match v.iter().find(|(n, _)| n == "b3").map(|(_, x)| x) {
            Some(Verdict::Regressed(f)) => assert!((f - 300.0 / 130.0).abs() < 1e-9),
            other => panic!("expected b3 regressed, got {other:?}"),
        }
    }

    /// THE QUORUM. With three benches in front of it the median IS the
    /// regression, so the correction stands down and the comparison is raw —
    /// otherwise a file whose every bench tripled would report no change.
    #[test]
    fn a_small_file_compares_raw_because_its_median_is_the_regression() {
        let base = table(&[("a", 100.0), ("b", 100.0), ("c", 100.0)]);
        let run = table(&[("a", 300.0), ("b", 300.0), ("c", 300.0)]);
        assert_eq!(bench_host_scale(&run, &base), 1.0);
        assert_eq!(bench_verdicts(&run, &base, 2.0, &[]).1, 3);
    }

    /// An ungated bench is REPORTED with its factor and not counted — the
    /// point being that it stays visible. Its neighbours still gate.
    #[test]
    fn an_ungated_bench_is_reported_and_not_counted() {
        let base = table(&[("slow", 100.0), ("other", 100.0)]);
        let run = table(&[("slow", 900.0), ("other", 900.0)]);
        let ungated = vec!["slow".to_string()];
        let (v, regressed) = bench_verdicts(&run, &base, 2.0, &ungated);
        assert_eq!(regressed, 1, "only `other` counts");
        let slow = v.iter().find(|(n, _)| n == "slow").map(|(_, x)| x.render());
        assert!(matches!(
            v.iter().find(|(n, _)| n == "slow").map(|(_, x)| x),
            Some(Verdict::Ungated(_))
        ));
        assert!(slow.is_some_and(|r| r.contains("x9.00") && r.contains("not gated")));
    }

    /// The ungate file is names and reasons: `#` comments and blanks are not
    /// bench names.
    #[test]
    fn the_ungate_list_reads_names_and_ignores_reasons() {
        let text = "# why this file exists

copy of a 1000-element Array<Int64>, 1000 times
   # indented
another bench   # trailing reason
";
        assert_eq!(
            bench_ungate_list(text),
            vec![
                "copy of a 1000-element Array<Int64>, 1000 times".to_string(),
                "another bench".to_string(),
            ]
        );
    }

    #[test]
    fn within_threshold_is_ok() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(v, vec![("a".to_string(), Verdict::Ok)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn exactly_at_threshold_is_ok_not_regressed() {
        // min == baseline * threshold is NOT a regression (strict `>`).
        let run = table(&[("a", 150.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(v, vec![("a".to_string(), Verdict::Ok)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn beyond_threshold_regresses_with_the_factor() {
        let run = table(&[("a", 250.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(v, vec![("a".to_string(), Verdict::Regressed(2.5))]);
        assert_eq!(regressed, 1);
    }

    #[test]
    fn threshold_arithmetic_uses_the_supplied_factor() {
        // Same 2x slowdown: a regression at 1.5, ok at 3.0.
        let run = table(&[("a", 200.0)]);
        let base = table(&[("a", 100.0)]);
        assert_eq!(bench_verdicts(&run, &base, 1.5, &[]).1, 1);
        assert_eq!(bench_verdicts(&run, &base, 3.0, &[]).1, 0);
    }

    #[test]
    fn a_run_bench_absent_from_baseline_is_new() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(v, vec![("a".to_string(), Verdict::New)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn a_baseline_bench_absent_from_run_is_missing_from_run() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 100.0), ("ghost", 100.0)]);
        let (v, _) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(
            v,
            vec![
                ("a".to_string(), Verdict::Ok),
                ("ghost".to_string(), Verdict::MissingFromRun),
            ]
        );
    }

    #[test]
    fn run_verdicts_preserve_declaration_order() {
        let run = table(&[("c", 100.0), ("a", 100.0), ("b", 100.0)]);
        let base = table(&[("a", 100.0), ("b", 100.0), ("c", 100.0)]);
        let (v, _) = bench_verdicts(&run, &base, 1.5, &[]);
        let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn zero_baseline_min_is_new_not_a_division_by_zero() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 0.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5, &[]);
        assert_eq!(v, vec![("a".to_string(), Verdict::New)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn placeholder_baseline_is_detected() {
        let flagged =
            vyrn_frontend::schema::parse_json(r#"{"placeholder":true,"benches":[]}"#).unwrap();
        assert!(baseline_is_placeholder(&flagged));
        let empty = vyrn_frontend::schema::parse_json(r#"{"benches":[]}"#).unwrap();
        assert!(baseline_is_placeholder(&empty));
        let real =
            vyrn_frontend::schema::parse_json(r#"{"benches":[{"name":"a","minNs":10}]}"#).unwrap();
        assert!(!baseline_is_placeholder(&real));
    }

    #[test]
    fn min_table_extracts_name_and_min_in_order() {
        let doc = vyrn_frontend::schema::parse_json(
            r#"{"backend":"native","opt":"O2","benches":[
                {"name":"a","minNs":10,"medianNs":11,"meanNs":12,"samples":31,"iters":64},
                {"name":"b","minNs":20,"medianNs":21,"meanNs":22,"samples":31,"iters":64}
            ]}"#,
        )
        .unwrap();
        let t = bench_min_table(&doc).unwrap();
        assert_eq!(t, vec![("a".to_string(), 10.0), ("b".to_string(), 20.0)]);
    }

    #[test]
    fn min_table_rejects_a_non_report() {
        let doc = vyrn_frontend::schema::parse_json(r#"{"nope":1}"#).unwrap();
        assert!(bench_min_table(&doc).is_none());
    }

    /// The default is v2 on x86-64 and *nothing* elsewhere: `-march=x86-64-v2`
    /// is an error on aarch64, and Apple Silicon is a real build host.
    #[test]
    fn default_native_target_is_v2_on_x86_64_and_absent_elsewhere() {
        assert_eq!(DEFAULT_NATIVE_TARGET, NativeTarget::V2);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(DEFAULT_NATIVE_TARGET.march(), Some("x86-64-v2"));
        } else {
            assert_eq!(DEFAULT_NATIVE_TARGET.march(), None);
            // Every value, not just the default — an explicit v3 in a manifest
            // shared with an x86 CI must not break the build on this host.
            for t in [
                NativeTarget::V1,
                NativeTarget::V3,
                NativeTarget::V4,
                NativeTarget::Native,
            ] {
                assert_eq!(t.march(), None);
            }
        }
    }

    #[test]
    fn native_target_parses_only_the_curated_set() {
        assert_eq!(NativeTarget::parse("v3"), Some(NativeTarget::V3));
        assert_eq!(NativeTarget::parse("native"), Some(NativeTarget::Native));
        // A passthrough would hand these to clang; the curated set refuses them.
        for bad in ["", "V2", "x86-64-v2", "haswell", "v5", "-march=native"] {
            assert_eq!(NativeTarget::parse(bad), None, "{bad} must not parse");
        }
    }

    /// The parity flag is unconditional — aarch64's *baseline* has FMA and
    /// there is no `-march` there to hang a condition on.
    #[test]
    fn every_native_build_disables_fp_contraction() {
        for t in [
            NativeTarget::V1,
            NativeTarget::V2,
            NativeTarget::V3,
            NativeTarget::V4,
            NativeTarget::Native,
        ] {
            let mut cmd = Command::new("clang");
            add_native_clang_flags(&mut cmd, t);
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.iter().any(|a| a == "-ffp-contract=off"),
                "{t:?} lost the parity flag"
            );
            assert!(args.iter().any(|a| a == "-O2"), "{t:?} lost -O2");
        }
    }

    /// `δata` starts with a two-byte character, so advancing the search
    /// window one byte per miss used to reslice mid-character and panic with
    /// "byte index is not a char boundary". Non-ASCII identifiers are legal —
    /// the lexer accepts any `is_alphabetic` char — so the fix must be
    /// byte-exact, not ASCII-only.
    #[test]
    fn insert_copy_survives_an_identifier_starting_with_a_multibyte_char() {
        let text = "let δata = read()\nprint(δata)\n";
        let fixed = insert_copy(text, 2, "δata").unwrap();
        assert_eq!(fixed, "let δata = read()\nprint(δata.copy())\n");
    }

    #[test]
    fn insert_copy_still_counts_whole_occurrences_only() {
        // Two candidates on one line: the diagnostic cannot say which, and
        // guessing is the failure this tool exists to avoid.
        let e = insert_copy("let a = f(a)\n", 1, "a").unwrap_err();
        assert!(e.contains("2 times"), "{e}");
    }

    #[test]
    fn json_pretty_emits_json_escapes_not_rust_debug_escapes() {
        use vyrn_frontend::schema::Json;
        let doc = Json::Obj(vec![(
            "ke\u{1}y".to_string(),
            Json::Str("a\u{1}b\"c\\d\ne".to_string()),
        )]);
        let out = json_pretty(&doc, 0);
        assert!(out.contains("\\u0001"), "{out}");
        assert!(out.contains("\\\""), "{out}");
        assert!(out.contains("\\\\"), "{out}");
        assert!(out.contains("\\n"), "{out}");
        assert!(!out.contains("\\u{"), "{out}");
        // What it prints must parse back to the same value — the manifest a
        // command writes is one every later command reads.
        assert_eq!(vyrn_frontend::schema::parse_json(&out).unwrap(), doc);
    }

    #[test]
    fn version_flag_counts_only_before_a_positional_argument() {
        let args = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert!(wants_version(&args(&["vyrn", "--version"])));
        assert!(wants_version(&args(&["vyrn", "-V"])));
        // A leading option does not end the scan.
        assert!(wants_version(&args(&[
            "vyrn",
            "--version",
            "run",
            "x.vyrn"
        ])));
        // But a positional does: the flag belongs to the program being run.
        assert!(!wants_version(&args(&[
            "vyrn",
            "run",
            "app.vyrn",
            "--version"
        ])));
        assert!(!wants_version(&args(&["vyrn", "run", "app.vyrn", "-V"])));
        assert!(!wants_version(&args(&["vyrn", "app.vyrn", "--version"])));
    }

    #[test]
    fn dev_static_paths_cannot_escape_their_root() {
        let dir = std::env::temp_dir().join("vyrn-dev-static-test");
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public/index.html"), b"<html>").unwrap();
        std::fs::write(dir.join("secret.txt"), b"s").unwrap();
        std::fs::write(dir.join("rt.js"), b"").unwrap();
        let assets = DevAssets {
            public_dir: dir.join("public"),
            web_dir: dir.to_string_lossy().into_owned(),
            wasm: dir.join("client.wasm"),
        };
        let go = |p: &str| dev_static_path(p, &assets);
        assert_eq!(go("/"), Some(dir.join("public").join("index.html")));
        assert_eq!(go("/vyrn-runtime/rt.js"), Some(dir.join("rt.js")));
        // Traversal, in either separator, is refused rather than resolved.
        for bad in [
            "/../secret.txt",
            "/..\\secret.txt",
            "/vyrn-runtime/../secret.txt",
            "/vyrn-runtime/..\\secret.txt",
        ] {
            assert_eq!(go(bad), None, "{bad}");
        }
        // A drive-letter target must not let `Path::join` replace the root.
        for bad in ["/C:/Windows/win.ini", "/vyrn-runtime/C:\\Windows\\win.ini"] {
            assert_eq!(go(bad), None, "{bad}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cross_origin_gate_refuses_foreign_pages_and_rebound_hosts() {
        // F2-071: a loopback bind does not stop another site's page from
        // driving the visitor's browser at this server.
        let req = |headers: &[(&str, &str)]| vyrn_frontend::interp::ServeRequest {
            method: "GET".to_string(),
            path: "/rpc/x".to_string(),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: String::new(),
        };
        let ok = req(&[("host", "localhost:8080")]);
        assert_eq!(cross_origin_body(&ok), None);
        let same = req(&[
            ("host", "localhost:8080"),
            ("origin", "http://localhost:8080"),
        ]);
        assert_eq!(cross_origin_body(&same), None);
        // A cross-site POST (browser always attaches Origin) is refused.
        let csrf = req(&[
            ("host", "127.0.0.1:8080"),
            ("origin", "https://evil.example"),
        ]);
        assert!(cross_origin_body(&csrf).is_some());
        // A rebound domain resolves to 127.0.0.1; only Host names it apart.
        let rebound = req(&[("host", "evil.example:8080")]);
        assert!(cross_origin_body(&rebound).is_some());
        // A WebSocket handshake WITHOUT Origin passes: the gate's business is
        // cross-SITE requests, and a client that sends no Origin (curl, the
        // test harnesses, any non-browser) has no site to be cross of. The
        // same-origin upgrade below stays allowed too.
        let ws = req(&[
            ("host", "localhost:8080"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert_eq!(cross_origin_body(&ws), None);
        let ws_same = req(&[
            ("host", "localhost:8080"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ("origin", "http://localhost:8080"),
        ]);
        assert_eq!(cross_origin_body(&ws_same), None);
    }

    /// The body cap answers 413 on the ANNOUNCED length, before the body is
    /// read. Driven over a real socket because that is where it has to work:
    /// the guard is one comparison, and the thing worth checking is that the
    /// refusal reaches the wire from the handler that owns this path.
    #[test]
    fn an_oversized_body_is_refused_before_it_is_read() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            serve_one(&mut stream, &mut |_call| {
                panic!("the request reached `handle` — the cap did not hold");
            });
        });
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        // A timeout, so a regression here FAILS instead of hanging the suite:
        // `parse_request` blocks until the header terminator arrives, and a
        // request that never sends one would otherwise stop the run dead.
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("read timeout");
        let announced = MAX_BODY + 1;
        let request = format!(
            "POST /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {announced}\r\n\r\n"
        );
        client.write_all(request.as_bytes()).expect("write headers");
        // Not one byte of the body is sent: the refusal must not wait for it.
        let mut answer = String::new();
        client.read_to_string(&mut answer).expect("read");
        server.join().expect("server thread");
        assert!(
            answer.starts_with("HTTP/1.1 413 Content Too Large"),
            "got: {answer}"
        );
    }

    #[test]
    fn loopback_host_strips_ports_and_ipv6_brackets() {
        assert!(loopback_host("localhost"));
        // Case-insensitive: a client may spell the host however it likes.
        assert!(loopback_host("LOCALHOST"));
        assert!(loopback_host("LocalHost:8080"));
        assert!(loopback_host("localhost:8080"));
        assert!(loopback_host("127.0.0.1:1"));
        assert!(loopback_host("[::1]:8080"));
        for foreign in ["", ":8080", "evil.example", "evil.example:8080", "[::1"] {
            assert!(!loopback_host(foreign), "{foreign}");
        }
    }
}
