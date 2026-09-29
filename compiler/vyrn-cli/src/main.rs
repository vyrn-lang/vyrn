//! `vyrn`, the Vyrn driver. `USAGE` lists the commands.
//!
//! `--deny-warnings` (or `VYRN_DENY_WARNINGS=1`) turns any load warning into a
//! failure. Without it, warnings go to stderr and change no exit code and no
//! byte of the program's output.
//!
//! The file argument is optional when a `vyrn.json` found by walking up from
//! the current directory declares a `"main"`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use vyrn_frontend::project::Memo;

use vyrn_codegen::toolchain::find_clang;

mod remote;
// In the library target because `vyrn-frontend`'s tests run their programs
// through it too.
use vyrn_cli::wasmrun;

const USAGE: &str = "usage: vyrn <run|check|fix|emit-wat|emit-lowered|emit-gen|build|test|bench|serve|fmt> [file.vyrn] [-o out] [--target wasm] [--native-target v1|v2|v3|v4|native] [--offline] [--deny-warnings]\n       vyrn build [file.vyrn] [-o out]   (the same wasm `--target wasm` writes, through wasm2c and clang to a native executable; needs wabt and simde under tools/, or $VYRN_WASM2C and $VYRN_SIMDE)\n       vyrn run [file.vyrn] [args...]   (trailing args reach the program's args())\n       vyrn run --profile [file.vyrn] [args...]   (where the run spent its time, to stderr; the flag counts only BEFORE the file, so a program can have one of its own. Rows are the phases of the compile and the run, with the operations the guest executed)\n       vyrn check --profile [file.vyrn]   (the same, for generation alone: `check` runs every `gen fn` and stops. Needs a cold generator cache to mean anything)\n       vyrn test [file.vyrn] [--name <substring>]\n       vyrn bench [file.vyrn] [--name <substring>] [--check | --json | --compare <baseline.json> [--threshold <factor>]]   (native timing; --check runs each once, compiled; --json machine-readable; --compare flags regressions)\n       vyrn serve [file.vyrn] [--port N] [--workers N]   (HTTP host; needs `fn handle(req: Request) -> Response`)\n       vyrn dev [--port N] [--workers N]   (fullstack: build client to wasm + serve server root, static, runtimes)\n       vyrn fmt [file.vyrn ...] [--check]   (canonical formatter; no files = project main + local imports)\n       vyrn fmt --from-json <file.json> [--as <Type>] [--from <module>]   (print the JSON file as VON)\n       vyrn doc [file|dir] [-o <dir>] [--std] [--verify]   (Markdown API docs; default docs/api/; --verify is the drift gate)\n       vyrn fix [file.vyrn]   (apply the `.copy()` a move diagnostic names, in the file given; every other fix on the menu is a decision and is refused)
       vyrn why <file>   (a module's audience, the path segment that decided it, and every import chain that reaches it)\n       vyrn why --contract <file>   (which module contract governs a file, and every export's status against it)\n       vyrn why --memory <file>   (per binding: whether it is reclaimed, how, and the reason when it is not)\n       vyrn why --capability <fs|stdin|args|extern> <entry-or-artifact-name>   (every import chain that pulls that capability into the artifact's closure)\n       vyrn routes [file.vyrn] [--json]   (the resolved wire table: every derived, pinned, hand-written and page path the router mounts, with its source; --json attaches each route's declaration from the symbol map)\n       vyrn emit-gen [file.vyrn] [--maps]   (--maps prints each generated module's symbol map as JSON, one per line)\n\
       vyrn new <name> | vyrn add <specifier> [--name alias] | vyrn update [--locked] [alias] | vyrn vendor [--check] | vyrn deps [artifact]   (deps: every declared artifact's module graph, then the toolchain)\n       vyrn --version   (also -V)";

/// `--offline` or `VYRN_OFFLINE=1`: never touch the network; a lock or cache
/// miss is an error.
fn offline(args: &[String]) -> bool {
    args.iter().any(|a| a == "--offline") || std::env::var("VYRN_OFFLINE").is_ok()
}

/// Whether `--version` / `-V` names this program: only among the leading
/// options. After the subcommand or file it belongs to the program being run.
fn wants_version(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.starts_with('-'))
        .any(|a| a == "--version" || a == "-V")
}

/// Whether the environment forbids the network. `real_main` normalizes
/// `--offline` into `VYRN_OFFLINE`, so the variable is the whole answer.
fn env_offline() -> bool {
    std::env::var("VYRN_OFFLINE").is_ok()
}

/// `--deny-warnings` or `VYRN_DENY_WARNINGS=1`: a load that produced warnings
/// fails. `real_main` normalizes the flag into the environment.
fn deny_warnings() -> bool {
    std::env::var("VYRN_DENY_WARNINGS").is_ok()
}

/// The microarchitecture a native build is compiled for.
///
/// A curated set, not a passthrough `-march`: a typo would surface as a clang
/// error, and an arbitrary `-march` can turn on FMA (see
/// `add_native_clang_flags`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NativeTarget {
    /// The bare `x86-64` baseline: SSE2 and nothing later.
    V1,
    /// SSE3/SSSE3/SSE4.1/SSE4.2/POPCNT (Nehalem, 2009).
    V2,
    /// AVX/AVX2/BMI/FMA.
    V3,
    /// AVX-512 (F/BW/CD/DQ/VL), and everything v3 has.
    V4,
    /// `-march=native`: everything this machine has, FMA included on any
    /// recent CPU. The artifact is only guaranteed to run here.
    Native,
}

/// The values `--native-target` and `vyrn.json`'s `nativeTarget` accept, for
/// diagnostics. Keep in step with `NativeTarget::parse`.
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

    /// The `-march=` value, or `None` off x86-64.
    ///
    /// `x86-64-vN` is an error on aarch64, and clang spells AArch64's native
    /// `-mcpu=native` (unverified here: no ARM host). Off x86-64 every value is
    /// inert, not fatal, because a manifest that pins v3 for x86 CI must still
    /// build on a Mac.
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

/// v2, not v1: without SSE4.1, `F32x4.trunc` scalarizes to four `truncf`
/// calls (0.43x of C); SSE4.1's `roundps` makes the loop 2.1x faster.
const DEFAULT_NATIVE_TARGET: NativeTarget = NativeTarget::V2;

/// Resolves the native target for a build rooted at `root`: `--native-target`
/// (via `VYRN_NATIVE_TARGET`), then the `nativeTarget` of the manifest that
/// governs `root`'s directory, then the default.
fn native_target_for(root: &str) -> Result<NativeTarget, String> {
    if let Ok(v) = std::env::var("VYRN_NATIVE_TARGET") {
        // A bad value here was set in the environment directly.
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
    // A misspelled target must not fall back to the default: the binary would
    // be built for something other than what the user wrote.
    NativeTarget::parse(&v).ok_or_else(|| {
        format!(
            "unknown `nativeTarget` `{v}` in {}/vyrn.json (expected one of: {NATIVE_TARGETS})",
            m.dir
        )
    })
}

/// Every flag a native clang invocation needs. `bench_native` and `build` both
/// call it, so the benchmark measures the binary `build` ships.
///
/// - `-O2`: clang's default is `-O0`.
/// - `-march`: see `NativeTarget`.
/// - `-ffp-contract=off`: the parity flag. With FMA, clang's default fuses
///   `a*b+c` (and wasm2c's separate `f64.mul`/`f64.add`) into one rounding,
///   a different number from wasm's. Output must be byte-identical across
///   engines. Passed unconditionally: aarch64's baseline has FMA. At v2 the
///   assembly is identical with and without it.
/// - `-pthread`: worker threads. Win32 threads need no flag.
/// - `-lm`: below SSE4.1 the vector roundings scalarize to `ceilf`/`floorf`/
///   `truncf`/`rintf`, in libm on Unix. Windows links them from the UCRT by
///   default, so a Windows-only check cannot see this missing.
fn add_native_clang_flags(cmd: &mut Command, target: NativeTarget) {
    cmd.arg("-O2").arg("-ffp-contract=off");
    // `VYRN_DEBUG_SYMBOLS=1` keeps debug info for a symbolizer; `-g` changes
    // no codegen under -O2.
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
    if cfg!(windows) {
        cmd.arg(format!(
            "-Wl,/STACK:{}",
            vyrn_frontend::trap::RUN_STACK_BYTES
        ));
    }
}

fn main() -> ExitCode {
    // The compiler's passes recurse over the syntax; the ~1 MB Windows
    // main-thread stack overflows on a realistic program (std/i18n).
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(|| {
            let code = real_main();
            // The phases are thread-local, so the table prints on the thread
            // that did the build.
            eprint!("{}", vyrn_frontend::prof::phase_table());
            code
        })
        .expect("failed to spawn the vyrn worker thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

/// Installs the generator engine and the lowering into this process. Every
/// process that compiles calls it before it loads a module, this binary's own
/// tests included (`tests/hosts.rs`).
fn install() {
    vyrn_genwasm::install();
    // The placer over the named core, into every plan this process makes.
    vyrn_lower::install();
}

fn real_main() -> ExitCode {
    install();
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
    // Validated here so a typo is one clear error, not a clang error.
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
    // Drained so the "no extra arguments" check below holds, but never from
    // `run`: its tail is the program's own `args()`.
    let want_maps = args.iter().any(|a| a == "--maps");
    if args.get(1).map(|a| a.as_str()) != Some("run") {
        args.retain(|a| a != "--maps");
    }
    // `--profile` counts only before the file, as `--version` does:
    // `vyrn run app.vyrn --profile` is a flag for `app.vyrn`.
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
    // Off `run`, `--profile` reports the build phases, and `main` prints the
    // table. `run_wasm` prints its own, with the guest's operation count.
    if want_profile && args.get(1).map(String::as_str) != Some("run") {
        std::env::set_var("VYRN_BUILD_PROFILE", "1");
    }
    // Before the usage screen, which exits 2: a package manager reads that as
    // a broken install. The release workflow checks the tag against this line.
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
        // The first positional anywhere: `vyrn routes --json app.vyrn`.
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
        return test_cmd(&path, rest);
    }
    if cmd == "bench" {
        return bench_cmd(&path, rest);
    }
    if cmd == "serve" {
        return serve_cmd(&path, rest);
    }
    // `run` forwards trailing arguments to the program's `args()`.
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

    match cmd {
        "fix" => fix_cmd(path, &source),
        // `check` must predict the one thing `build` can fail to finish:
        // unbounded monomorphization, visible only while emitting (audit A5.2).
        "check" => match loaded(path, &source) {
            Ok((program, _dsg)) => {
                let _memo = shared_desugars(&program);
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
        },
        "run" => {
            // Generators run in the load; its time is the first row of the
            // table `run_wasm` prints.
            let clock = std::time::Instant::now();
            let (program, dsg) = match loaded(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let load = clock.elapsed();
            let _memo = shared_desugars(&program);
            // What `check` refuses, `run` refuses, with `check`'s sentence: a
            // polymorphic recursion has no finite set of instances.
            if let Err(e) = vyrn_codegen::check_instantiations(&program) {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            let profile = want_profile.then_some(load);
            run_wasm(path, &program, &dsg, &prog_args, profile)
        }
        // The module `build --target wasm` writes and `build` hands wasm2c.
        "emit-wat" => {
            let (program, dsg) = match loaded(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            match vyrn_codegen::direct::wat(&program, &dsg) {
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
        // The form the emitter reads, for the root module only: a linked
        // program's imports are another file's answer.
        "emit-lowered" => {
            let (program, _dsg) = match loaded(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let _memo = shared_desugars(&program);
            print!("{}", vyrn_lower::render(&program, path));
            ExitCode::SUCCESS
        }
        "emit-gen" => emit_gen(path, &source, want_maps),
        other => {
            eprintln!("unknown command `{other}` (expected run, check, fix, emit-wat, emit-lowered, emit-gen, build, test, bench, or serve)");
            ExitCode::from(2)
        }
    }
}

/// `vyrn emit-gen [file] [--maps]`: prints the source of every generated module
/// the file reaches, each under a banner naming its call site.
///
/// `--maps` prints each module's symbol map instead, one JSON document
/// per line with the banners on stderr, so `> api.map.json` writes the file.
fn emit_gen(path: &str, source: &str, maps: bool) -> ExitCode {
    let root_key = normalize_slashes(path);
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let result = vyrn_frontend::loader::generated_modules(source, &root_key, &opts, &resolver);
    // Pins are saved even when the run fails. A pin the disk refused fails the
    // command: a fetched remote must land in vyrn.lock.
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
            print_diagnostics(&diags, &root_key, "");
            ExitCode::FAILURE
        }
    }
}

use vyrn_frontend::loader::DiskResolver;

use vyrn_frontend::manifest::{
    dos_to_slash, find as find_manifest, real_path, std_root, web_root, Manifest,
};

/// [`find_manifest`]; an unreadable manifest prints why and exits 2.
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

/// LoadOptions for a root file: the std root and the nearest manifest's settings.
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

/// `vyrn new <name>`: scaffolds vyrn.json, src/main.vyrn and .gitignore.
fn scaffold(name: &str) -> ExitCode {
    // The name is interpolated raw into vyrn.json and src/main.vyrn; a quote,
    // a backslash or a control character would write a manifest no later
    // command can parse.
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

/// `vyrn why`: dispatches to the audience, `--contract`, `--memory` or
/// `--capability` report. `--contract` prints the contract that governs a
/// module and the status of each of its members; it exits 1 when the file is
/// in no role.
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
        Ok(p) => dos_to_slash(&p.to_string_lossy()),
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
    // directory.
    let manifest = nearest_manifest(&dir);
    let app_dir = manifest
        .as_ref()
        .map(|m| PathBuf::from(&m.dir))
        .unwrap_or_else(|| dir.clone());
    // The manifest already read is passed in, never re-read: two readers of one
    // file are two policies when one of them fails.
    let doc = manifest.as_ref().map(|m| &m.doc);
    let roots = vyrn_frontend::manifest::role_roots(&app_dir, doc);
    let roles = vyrn_frontend::contracts::roles_for_project(doc, &roots, &opts, &DiskResolver);
    let Some(role) = vyrn_frontend::contracts::role_for(&path, &roles) else {
        println!("{path}");
        println!("  no contract: this file is in no role");
        if vyrn_frontend::contracts::is_projection(&path) {
            println!(
                "  (its stem is dotted: a projection written OVER the modules beside it, \
                 which the generator scanning that directory skips)"
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
    let manifest = dos_to_slash(&app_dir.join("vyrn.json").to_string_lossy());
    let Some(view) =
        vyrn_frontend::contracts::load_role_contract(role, &manifest, &opts, &DiskResolver)
    else {
        eprintln!(
            "error: cannot resolve contract `{}:{}`",
            role.module, role.contract
        );
        return ExitCode::FAILURE;
    };

    // A `.vyx`'s module is its `<script>`, plus the `<template>`, which compiles
    // to an export.
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
            // The file's form writes it: a `.vyx` has no other way to declare
            // a view.
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
        // Not a gate: the generator that consumes the module runs the same
        // check at load time.
        println!(
            "  — {objections} objection(s); the generator that consumes this module is the gate"
        );
    }
    ExitCode::SUCCESS
}

/// `vyrn routes [file]`: the resolved wire table, with where each path came
/// from.
///
/// Rows come from three channels, and none of them computes a path, so the
/// table and the mounted router cannot disagree: the `//@route` directives a
/// mounting generator emits (pages included), the symbol maps
/// (`--json` only, for each route's declaration), and the arguments of the
/// program's `mount(..)` call for hand-written lists ([`mounted_routes_wasm`]).
/// The channels are unioned.
fn routes_cmd(file: Option<&str>, json: bool) -> ExitCode {
    let path = match file.map(|s| s.to_string()).or_else(manifest_main) {
        Some(p) => p,
        None => {
            eprintln!("error: no input file, and no vyrn.json with a `main` found");
            eprintln!("usage: vyrn routes [file]");
            return ExitCode::from(2);
        }
    };
    let root_key = normalize_slashes(&path);
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
    // Pins survive a failed run; a pin the disk refuses fails the command.
    if let Err(code) = save_lock(&resolver) {
        return code;
    }
    let mods = match result {
        Ok(m) => m,
        Err(diags) => {
            print_diagnostics(&diags, &root_key, "");
            return ExitCode::FAILURE;
        }
    };
    // `(method, path, procedure, source)`, de-duplicated: a page or api
    // directory reached through two roots generates the same table twice.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for (_, src) in &mods {
        // A `//@route` counts only where the lexer says a comment begins: a
        // generator copies string literals through verbatim.
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
    // The hand-written channel. A failure is reported and survived: the derived
    // rows above are still true.
    match Memo::load(|| vyrn_frontend::load(&source, &root_key, &opts, &resolver))
        .map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default())
        .and_then(|(p, dsg)| {
            let _memo = shared_desugars(&p);
            mounted_routes_wasm(&root_key, &p, &dsg)
        }) {
        Ok(mounted) => {
            for (method, path, procedure) in mounted {
                let row = (method, path, procedure, "explicit".to_string());
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
    // A projection puts two methods on one path.
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

/// `vyrn routes`'s hand-written channel: the arguments of every `mount(..)` the
/// program holds, read by running them.
///
/// A copy of the program swaps its `main` for one that prints `std/http`'s
/// `mountedRows` of each call's three route lists, and runs in the embedded
/// engine with stdout captured. Each row is the method, the path, and for a
/// `Route` the procedure. A `*` row (a `surface(..)`) is dropped: the directive
/// channel lists its members.
///
/// An argument that names a local of its enclosing function cannot be lifted
/// into the new `main`, so the compile fails. A `mount` other than
/// `std/http`'s four-argument one is not found.
fn mounted_routes_wasm(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
) -> Result<Vec<(String, String, String)>, String> {
    use vyrn_frontend::ast::{Block, Expr, Stmt, Type};
    let mut prog = program.clone();
    let mut calls: Vec<Vec<Expr>> = Vec::new();
    for f in &mut prog.functions {
        vyrn_frontend::project::walk_block(&mut f.body, &mut |e| {
            // Top-level names are unique across a linked program, so `mount` is
            // `std/http`'s. Argument 0 is the request.
            if let Expr::Call { name, args, .. } = e {
                if name == "mount" && args.len() == 4 {
                    calls.push(args[1..].to_vec());
                }
            }
        });
    }
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    prog.functions
        .retain(|f| !(f.name == "main" && f.module.is_none()));
    prog.tests.clear();
    prog.benches.clear();
    let mut stmts: Vec<Stmt> = calls
        .into_iter()
        .map(|args| {
            Stmt::Expr(Expr::Call {
                dot: false,
                type_args: Vec::new(),
                name: "print".to_string(),
                args: vec![Expr::Call {
                    dot: false,
                    type_args: Vec::new(),
                    name: "mountedRows".to_string(),
                    args,
                    line: 0,
                }],
                line: 0,
            })
        })
        .collect();
    stmts.push(Stmt::Return {
        value: Some(Expr::Int(0)),
        line: 0,
    });
    prog.functions.push(synth_fn(
        "main".to_string(),
        Block { stmts },
        Type::Int,
        0,
        false,
    ));
    let bytes = vyrn_codegen::direct::compile(&prog, memo)?;
    let out = wasmrun::run(
        &bytes,
        wasmrun::Run {
            argv: vec![path.to_string()],
            stdin_prefix: Vec::new(),
            capture_stdout: true,
            capture_stderr: true,
            meter: false,
        },
    )?;
    if out.code != 0 {
        let text = String::from_utf8_lossy(&out.stderr).into_owned();
        let line = text
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches("error: ");
        return Err(if line.is_empty() {
            format!("the mounted router exited {}", out.code)
        } else {
            line.to_string()
        });
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut words = line.split_whitespace();
        let (Some(method), Some(route)) = (words.next(), words.next()) else {
            continue;
        };
        if method == "*" {
            continue;
        }
        let procedure = match method {
            "SSE" | "WS" => "-",
            _ => words.next().unwrap_or("-"),
        };
        rows.push((method.to_string(), route.to_string(), procedure.to_string()));
    }
    Ok(rows)
}

/// `vyrn routes --json`: the wire table, each route with the declaration its
/// symbol map names. `origin` is `null` for a route no map covers.
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
                // Every generator that maps a procedure names the same
                // declaration, so the first one settles it.
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

/// A JSON string literal, quotes included, escaped by the canonical JSON
/// table. Rust's `Debug` escapes (`\u{1}`) are not valid JSON.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    vyrn_frontend::codec::escape_into(s, &mut out);
    out.push('"');
    out
}

/// `vyrn why --memory <file>`: what the ownership analysis decided about every
/// binding in the file, and why. It prints `own::Ownership::memory` and
/// re-derives nothing. Exit 0 whenever it could answer.
fn why_memory(file: &str) -> ExitCode {
    let path = match Path::new(file).canonicalize() {
        Ok(p) => dos_to_slash(&p.to_string_lossy()),
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
    // A `.vyx`'s module is its `<script>`.
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
        // A return is owned, so the return type is the whole
        // answer.
        match own.proto.release_kind(&f.ret) {
            Some(ref kind) => println!(
                "    transfers: yes — the caller owns the result, and releases it by {}",
                kind.words()
            ),
            None => println!("    transfers: no — the return type {} owns no heap", f.ret),
        }
        let notes = match own.memory.get(&f.name) {
            Some(n) if !n.is_empty() => n,
            _ => {
                println!("    (no bindings)");
                continue;
            }
        };
        for n in notes {
            use vyrn_frontend::own::Bucket;
            bindings += 1;
            match n.bucket {
                Bucket::Reclaimed => reclaimed += 1,
                Bucket::Moved => moved += 1,
                Bucket::Dropped => dropped += 1,
                Bucket::Static => statics += 1,
                Bucket::Discharged => discharged += 1,
                Bucket::Leaked { reason, .. } => {
                    match leaked.iter_mut().find(|(k, _)| *k == reason) {
                        Some((_, c)) => *c += 1,
                        None => leaked.push((reason, 1)),
                    }
                }
            }
            println!("    line {:<5} {:<16} {}", n.line, n.name, n.text);
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

/// `vyrn why <file>`: the audience of a module, the path segment that decided
/// it, and every import chain that reaches it. Exit 0 whenever it could answer,
/// 2 only when the file cannot be read.
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
    let app_slash = dos_to_slash(&app_dir.to_string_lossy());
    let map = manifest.as_ref().and_then(|m| m.audience.clone());

    println!("{path}");
    // The compiler declares the audience of these two modules, by path identity
    // as the loader's fence does; no manifest has a say.
    let fenced = std_root().and_then(|root| {
        use vyrn_frontend::loader::{MEM_SPEC, RUNTIME_SPEC};
        let is = |spec: &str| {
            real_path(&format!("{root}/{}.vyrn", &spec["std/".len()..])).as_deref() == Some(&*path)
        };
        if is(MEM_SPEC) {
            Some(format!("`{RUNTIME_SPEC}`, declared by the compiler"))
        } else if is(RUNTIME_SPEC) {
            Some("the compiler, which links it into every program".to_string())
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

    // Read off the sources, with no load: the file asked about may be the one
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

/// `vyrn why --capability <cap> <entry-or-artifact-name>`: every import chain
/// that pulls a capability into one artifact's closure. The floor's refusal
/// shows only the shortest.
///
/// It walks the linked graph (`loader::capability_graph`), not the files on
/// disk, because a generated module can carry a capability too. The load runs
/// with the fence and the floor disarmed, so it answers for a refused tree.
/// Exit 0 whenever it could answer, 2 for an unknown capability or an argument
/// that names no artifact.
fn why_capability(cap: &str, name: &str) -> ExitCode {
    use vyrn_frontend::floor::{self, Capability};
    let Some(cap) = Capability::parse(cap) else {
        eprintln!(
            "error: unknown capability `{cap}` (expected one of: {})",
            floor::CAPABILITIES
        );
        return ExitCode::from(2);
    };
    // An entry's path or an artifact's name. File identity first, so two
    // spellings of one file name one artifact.
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
    let (graph, root_key) = match vyrn_frontend::loader::capability_graph(
        &source,
        &artifact.entry,
        &opts,
        &DiskResolver,
    ) {
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
            // A module the loader injected has no importer; its chain is the
            // entry and it, the floor's rule.
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

/// Bounds on both import walks: an exhaustive enumeration of a real graph does
/// not return.
const MAX_CHAINS: usize = 24;
const MAX_DEPTH: usize = 12;

/// Every simple path forward from `entry` to `target` in the import graph, the
/// entry first. Empty when `target` is not in the artifact's closure at all.
fn chains_from(entry: &str, target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {
    fn walk(
        node: &str,
        target: &str,
        edges: &[(String, String)],
        seen: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        if out.len() >= MAX_CHAINS || seen.len() >= MAX_DEPTH {
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
/// A generator import contributes edges too. A call naming one file is
/// resolved by `audience::generator_input`, the function that decides that
/// module's audience; a call naming a directory reaches every source under it.
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
    // Key both ends by `real_path`, as audience is decided: on Windows
    // `../Server/store` and `../server/store` name one file.
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
                if name.starts_with('.')
                    || name == "target"
                    || name == "vendor"
                    || name == "node_modules"
                {
                    continue;
                }
                walk(&p, out);
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("vyrn") | Some("vyx")
            ) {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let key = dos_to_slash(&p.to_string_lossy());
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
/// imports (a composition root).
fn import_chains(target: &str, edges: &[(String, String)]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    // `seen` is the path so far, target first.
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
    // A target nothing imports yields the one-element chain of itself.
    out.retain(|c| c.len() > 1);
    out
}

/// The `<script>` body of a `.vyx`, bounded by `vyrn_frontend::vyx`, the rule
/// `std/vyx` compiles with.
fn vyx_script_body(text: &str) -> Option<String> {
    let (start, end) = vyrn_frontend::vyx::script_body(text)?;
    Some(text[start..end].to_string())
}

/// One `toolchain:` row: the tool, the path that would be used, its version,
/// and why that path was chosen.
type ToolRow = (String, String, String, String);

/// A row for a tool resolved by one of `vyrn-codegen`'s `*_from` resolvers.
///
/// The version is the pin for a pinned tool and `unknown` otherwise: running
/// each tool to ask would spend a spawn per row.
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
        // An unresolvable pin is a refusal everywhere else, so it prints, with
        // the refusal's words on one line.
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

/// The same rule as [`normalize_slashes`], from a `Path`.
fn show_path(p: &Path) -> String {
    dos_to_slash(&p.to_string_lossy())
}

/// The `toolchain:` section of `vyrn deps`: one row per tool, with the path
/// that would be used, its version, and why that path was chosen.
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
    // clang is discovered, never pinned, so its version is a probe.
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
    // The native route's tools, pinned under the lock file's names (`wabt`
    // ships `wasm2c`). A found wasm2c prints the version its binary reports.
    rows.push(match vyrn_codegen::toolchain::wasm2c_from(start) {
        Ok(Some(t)) => ("wasm2c".into(), show_path(&t.exe), t.version, t.why.into()),
        other => tool_row(
            "wasm2c",
            other.map(|o| o.map(|t| (t.exe, t.why))),
            pin("wabt").as_deref(),
            "$VYRN_WASM2C, tools/",
        ),
    });
    rows.push(tool_row(
        "simde",
        Ok(vyrn_codegen::toolchain::simde_from(start)),
        pin("simde").as_deref(),
        "$VYRN_SIMDE, tools/",
    ));

    // The version column is not padded: a clang version line can run past a
    // hundred characters.
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

/// `vyrn deps [artifact]`: prints the resolved module graph of every artifact
/// the manifest's artifact map declares (the floor's reader), then the
/// toolchain.
///
/// A lone artifact named `main` prints its graph with no header. A manifest
/// that declares no artifacts is not an error: it prints the toolchain alone.
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
        // By name only; unlike `why --capability`, not by entry path.
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
        match vyrn_frontend::loader::module_graph(&source, root_key, &opts, &DiskResolver) {
            Ok(graph) => {
                for (module, imports) in graph {
                    println!("{module}");
                    for i in imports {
                        println!("  -> {i}");
                    }
                }
            }
            Err(diags) => {
                print_diagnostics(&diags, root_key, "");
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

/// `vyrn fmt [file ...] [--check]`: formats each file in place, or with no
/// files the project `main` and its local imports. `--check` writes nothing,
/// lists the files that would change, and exits 1 if any would.
///
/// The input need only lex. A file that does not is reported and left
/// untouched; the others still format, and the exit is non-zero.
fn fmt_cmd(rest: &[String]) -> ExitCode {
    // A converter, not a formatter run: it prints and writes nothing.
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
        // The formatter would lex a `.vyx` template as Vyrn tokens and put spaces
        // inside every tag and sentence, so it skips the file.
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
        // A file keeps its line endings: the formatter sees LF, and CRLF is
        // re-applied if the source had any. A mixed file becomes all CRLF.
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
                // A lex error or the re-lex invariant: the file stays untouched.
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

/// The converter `vyrn fmt --from-json` runs, written in Vyrn so there is one
/// JSON reader and one VON writer.
///
/// `put` drops the text's final newline because `print` adds one; the output
/// is then exactly what `vyrn fmt` leaves behind.
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

/// `vyrn fmt --from-json <file.json> [--as <Type>] [--from <module>]`: prints a
/// JSON file as VON, headed by an `import type` line. Every nested object
/// arrives as a `Map`. Nothing is written to disk.
fn from_json_cmd(path: &str, type_name: &str, module: &str) -> ExitCode {
    let json = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    // A key beside the input file, so `std/` resolves as it would there. No
    // file is read at it.
    let norm = normalize_slashes(path);
    let key = match norm.rfind('/') {
        Some(i) => format!("{}/from-json.vyrn", &norm[..i]),
        None => "from-json.vyrn".to_string(),
    };
    let (program, dsg) = match loaded(&key, FROM_JSON_SRC) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
    // Stderr is captured because the wording below rewrites it.
    let bytes = match vyrn_codegen::direct::compile(&program, &dsg) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let run = wasmrun::Run {
        argv: vec![key.clone(), json, type_name.to_string(), module.to_string()],
        stdin_prefix: Vec::new(),
        capture_stdout: false,
        capture_stderr: true,
        meter: false,
    };
    let out = match wasmrun::run(&bytes, run) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if out.code == 0 {
        return ExitCode::SUCCESS;
    }
    // The trap names a position in the converter, which the user cannot open;
    // the input file's name replaces it.
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let msg = text
        .trim_end_matches(['\n', '\r'])
        .trim_start_matches("error: ");
    let msg = msg
        .split_once(" (from-json.vyrn:")
        .map(|(m, _)| m)
        .unwrap_or(msg);
    eprintln!("error: {path}: {msg}");
    ExitCode::from((out.code & 0xff) as u8)
}

/// The project's `main` and its local imports: a bare `vyrn fmt`'s targets.
/// Remote imports are pinned, never formatted in place.
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
    let root_key = normalize_slashes(&main);
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    match vyrn_frontend::loader::module_graph(&source, &root_key, &opts, &resolver) {
        Ok(graph) => {
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
            // A graph error falls back to the main file alone.
            print_diagnostics(&diags, &root_key, "");
            Ok(vec![root_key])
        }
    }
}

/// A module to document. `name` (`std/json`, `routes/home`) is the page heading
/// and, with `.md`, the output path.
struct DocModule {
    name: String,
    source: String,
}

/// `vyrn doc [file|dir] [-o <dir>] [--std] [--verify]`: writes Markdown API
/// docs, one `.md` per module plus `index.md`, with `///` blocks verbatim.
/// Output is byte-stable: every list sorted, LF newlines.
///
/// `--verify` writes nothing and exits 1 if the output directory differs from
/// what would be generated.
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

/// The modules to document:
/// - a file: its local-import closure (`--std` adds the std modules it reaches);
/// - a directory: every `.vyrn` under it, named relative to it;
/// - nothing, with a manifest `main`: that file's closure;
/// - nothing, with `--std`: the whole std library.
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

/// Every `.vyrn` file under `dir`, named `<prefix>` plus its path relative to
/// `dir` without the extension. Sorted by name.
fn scan_doc_dir(dir: &str, prefix: &str) -> Result<Vec<DocModule>, ExitCode> {
    let base = normalize_slashes(dir);
    let mut paths: Vec<String> = Vec::new();
    collect_vyrn_files(Path::new(dir), &mut paths);
    let mut out = Vec::new();
    for p in paths {
        let rel = rel_name(&p, &base);
        // A fenced module has no reader outside the compiler.
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

/// Appends every `.vyrn` file under `dir` to `out`, in sorted directory order.
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

/// Every local module `root_file` reaches, named relative to the project.
/// `with_std` adds the std modules reached, as `std/<rel>`. Remote and
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
    // Local module names are relative to the manifest's directory, else the
    // root file's.
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
    // Pins survive a failed run; a pin the disk refuses fails the command.
    save_lock(&resolver)?;
    let graph = match result {
        Ok(g) => g,
        Err(diags) => {
            print_diagnostics(&diags, &root_key, "");
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

/// A path as this toolchain spells it: `dos_to_slash`, the rule `real_path` and
/// module keys use.
fn normalize_slashes(p: &str) -> String {
    dos_to_slash(p)
}

/// The module name: `path` relative to `base`, without `.vyrn`. The file stem
/// when `path` is not under `base`.
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

/// Renders `index.md`: each module links to its page, with the first line of
/// its header doc.
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

/// Renders one module page: the header doc, then per export a `##` heading, the
/// signature and its doc, and a `###` section per documented protocol method.
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

/// Writes `files` under `out_dir`, then deletes every other `.md` there, so a
/// regenerate converges with `--verify`.
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

/// `--verify`: exit 1 if `out_dir`'s `.md` files differ from `files` (missing,
/// extra, or content).
fn verify_doc_dir(out_dir: &str, files: &[(String, String)]) -> ExitCode {
    let wanted: std::collections::HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let existing: std::collections::HashSet<String> =
        existing_md_files(out_dir).into_iter().collect();
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

/// LF newlines, so a CRLF checkout of a generated doc is not drift.
fn normalize_slashes_content(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Every `.md` file under `dir`, as `/`-separated paths relative to `dir`.
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

/// The lock file's path and the project directory for a root file: beside the
/// manifest when there is one, else beside the root file.
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

/// The CLI resolver: files, plus remotes through the lock, the cache and the
/// network. A lock file that will not parse exits here, so nothing re-pins to
/// whatever the network serves.
fn make_resolver(root_key: &str) -> remote::RemoteResolver {
    let (lock_path, project_dir) = lock_home(root_key);
    remote::RemoteResolver {
        lock: std::cell::RefCell::new(load_lock(lock_path)),
        project_dir,
        offline: env_offline(),
    }
}

/// [`remote::Lock::load`]; a damaged lock prints the line and exits 2. A pin
/// the compiler cannot read is not the absence of a pin.
fn load_lock(path: PathBuf) -> remote::Lock {
    match remote::Lock::load(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}

/// Saves the pins the load added. A failed write is an error: an
/// unpinned build is not reproducible.
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

/// `vyrn fix [file]`: applies the `.copy()` a move diagnostic names and
/// refuses every other fix on the menu, because `consume` and
/// `for x in consume xs` are decisions, not edits.
///
/// It edits only the file given; a diagnostic in an import is reported. A round
/// applies at most one edit per line and is kept only if the diagnostic count
/// falls, so the file never compiles worse than it did.
fn fix_cmd(path: &str, source: &str) -> ExitCode {
    let root_key = normalize_slashes(path);
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
        // A round that does not reduce the count is discarded whole.
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
        // Every round reduces the count; the bound stops a file with hundreds
        // of sites, which can run again.
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
    // Not a gate: `vyrn check` is.
    ExitCode::SUCCESS
}

/// Loads `text` as `root_key` and returns every diagnostic, printing nothing.
/// The checker's and the kernel's ownership refusals arrive as one list.
fn fix_diagnostics(root_key: &str, text: &str) -> Vec<vyrn_frontend::diagnostics::Diagnostic> {
    let opts = load_options(root_key);
    let resolver = make_resolver(root_key);
    match vyrn_frontend::load_warned(text, root_key, &opts, &resolver).0 {
        Ok(_) => Vec::new(),
        Err(d) => d,
    }
}

/// The path a `.copy()` fix names, out of a diagnostic's menu.
///
/// A menu line is ``  fix: `PATH.copy()` <why>``, the text `movecheck::menu`
/// writes.
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

/// Puts `.copy()` after the single occurrence of `path` on 1-based `line`.
///
/// The occurrence must be whole (not the tail of a longer name, not a receiver
/// or callee) and unique on the line; otherwise it refuses rather than guesses.
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

/// A function this driver synthesizes: no parameters, no type parameters, no
/// doc, in the root module, at column 0. `door` makes it an export, which the
/// host calls and which makes the body a sweep root.
fn synth_fn(
    name: String,
    body: vyrn_frontend::ast::Block,
    ret: vyrn_frontend::ast::Type,
    line: usize,
    door: bool,
) -> vyrn_frontend::ast::Function {
    vyrn_frontend::ast::Function {
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
        is_export_extern: door,
        is_gen: false,
        is_mut: false,
    }
}

/// Loads and checks a root file. Prints the diagnostics on failure and the
/// warnings on success, to stderr and before the command's own output; every
/// command that builds a program loads here.
fn load_program(path: &str, source: &str) -> Result<vyrn_frontend::ast::Program, ExitCode> {
    let root_key = normalize_slashes(path);
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let (result, warnings) = vyrn_frontend::load_warned(source, &root_key, &opts, &resolver);
    // Pins are saved even when a later stage fails.
    save_lock(&resolver)?;
    match result {
        Ok(p) => {
            if print_warnings(&warnings, &root_key) {
                return Err(ExitCode::FAILURE);
            }
            Ok(p)
        }
        Err(diags) => {
            print_diagnostics(&diags, &root_key, "");
            Err(ExitCode::FAILURE)
        }
    }
}

/// [`load_program`] with the projection memo opened first, so the load and the
/// command share one expansion per site. [`shared_desugars`] adopts the load's
/// ownership judgment, which is sound only if both walk the same nodes.
fn loaded(path: &str, source: &str) -> Result<(vyrn_frontend::ast::Program, Memo), ExitCode> {
    Memo::load(|| load_program(path, source))
}

/// Holds this command's one ownership analysis of `program`, adopted from the
/// load.
///
/// `a[i]` and `for x in c` over a user container inline a projection at the
/// access site, and side tables are keyed by node address. [`loaded`]'s memo
/// gives every engine the same expanded tree, so they read the same rows.
fn shared_desugars(program: &vyrn_frontend::ast::Program) -> vyrn_frontend::own::Memo<'_> {
    vyrn_frontend::own::Memo::open(program)
}

/// Prints `file:line:col: message` per diagnostic, the file defaulting to
/// `root_key`, with its note below. `marker` is `""` for an error and
/// `"warning: "` for a warning.
fn print_diagnostics(
    diags: &[vyrn_frontend::diagnostics::Diagnostic],
    root_key: &str,
    marker: &str,
) {
    for d in diags {
        let file = d.file.as_deref().unwrap_or(root_key);
        eprintln!("{}:{}:{}: {}{}", file, d.line, d.col, marker, d.message);
        if let Some(note) = &d.note {
            eprintln!("  note: {note}");
        }
    }
}

/// Prints a load's warnings to stderr. Returns whether the run fails, which it
/// does only under `--deny-warnings`.
fn print_warnings(warnings: &[vyrn_frontend::diagnostics::Diagnostic], root_key: &str) -> bool {
    if warnings.is_empty() {
        return false;
    }
    print_diagnostics(warnings, root_key, "warning: ");
    if deny_warnings() {
        eprintln!(
            "error: {} warning(s) — refused by --deny-warnings",
            warnings.len()
        );
        return true;
    }
    false
}

/// `vyrn add <specifier> [--name alias]`: fetches and pins a remote module and
/// records it in vyrn.json's dependencies.
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

    // Fetch here, so a typo fails at once and the next build can run offline.
    let resolver = make_resolver(&format!("{}/vyrn.json", manifest.dir));
    if let Err(e) = vyrn_frontend::loader::ModuleResolver::read(&resolver, &spec) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    if save_lock(&resolver).is_err() {
        return ExitCode::FAILURE;
    }

    // Rewrites the document already read; key order stays stable.
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

/// Fetches every platform's published artifact of one pinned tool into the
/// cache and records each in the lock as `tool:<name>@<version>/<platform>`.
///
/// Every platform, so a networked machine records the hashes for one that is
/// not. A platform with no upstream artifact is reported and skipped.
fn update_tool(name: &str, version: &str, lock: &mut remote::Lock) -> Result<(), String> {
    use vyrn_frontend::toolpin;
    // Before the retain below drops the old pins.
    if env_offline() {
        return Err(format!(
            "{name} {version} must be fetched from the network, and --offline / \
             VYRN_OFFLINE forbids it"
        ));
    }
    // Drop every version's lines, or `vyrn vendor` keeps copying stale pins.
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

/// `vyrn update --locked` for one tool: makes this machine hold what the lock
/// pins for its platform, and never rewrites the lock.
///
/// The URL and the hash come from the lock; a mismatch is
/// [`remote::upstream_changed`]. CI calls this, because the resolver never
/// reaches the network.
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
    // The resolver first: bytes already cached, vendored or unpacked need no
    // network, and this is the path the build takes.
    if let Ok(dir) = toolpin::pinned_tool(project_dir, lock, name, version) {
        println!("{spec} -> {}", dir.display());
        return Ok(());
    }
    let Some((url, sha)) = lock.entries.get(&spec).cloned() else {
        // The resolver's refusal names the platforms the lock covers.
        return toolpin::pinned_tool(project_dir, lock, name, version).map(|_| ());
    };
    if env_offline() {
        // The resolver's refusal for a locked miss.
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

/// `vyrn update [--locked] [alias|tool]`: re-resolves the remote dependencies
/// (or one alias) and re-pins them, and pins every platform of each manifest
/// `toolchain` tool.
///
/// `--locked` re-resolves nothing and never saves the lock: it fetches only
/// what the caches miss, verifies every byte against the lock, and refuses a
/// mismatch.
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
        // A known tool the manifest does not declare: "nothing to update"
        // would read as "up to date".
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
        // Removing the entry makes a normal run re-resolve it; a locked run
        // reads through the existing pin, which verifies the hash.
        if !locked {
            lock.entries.remove(spec);
            lock.dirty = true;
            println!("re-resolving `{name}` ({spec})");
        } else if !lock.entries.contains_key(spec) {
            // An unpinned spec would reach the resolver and be fetched.
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
    if !locked && save_lock(&resolver).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `vyrn vendor [--check]`: copies every locked blob into the vendor directory,
/// or with `--check` verifies each is there.
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
                    "cannot vendor `{spec}`: not in the cache — run the build once \
                      (online) first"
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

/// Pretty-prints a Json value with a 4-space indent, keys in their order.
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
        Json::Str(s) => json_str(s),
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
                .map(|(k, v)| format!("{pad}{}: {}", json_str(k), json_pretty(v, depth + 1)))
                .collect();
            format!("{{\n{}\n{close}}}", inner.join(",\n"))
        }
    }
}

/// `vyrn test [file] [--name <substring>]`: runs the root file's `test` blocks
/// in declaration order and exits 1 if any failed. A file with no tests prints
/// `no tests` and exits 0.
fn test_cmd(path: &str, rest: &[String]) -> ExitCode {
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
    let (program, dsg) = match loaded(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
    let has_tests = program.tests.iter().any(|t| t.module.is_none());
    if !has_tests {
        println!("no tests");
        return ExitCode::SUCCESS;
    }
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
    bodies_wasm(path, &program, &dsg, "test", &bodies)
}

/// `vyrn bench`: runs the root file's `bench` blocks in declaration order.
///
/// - default: times each body in a native harness ([`bench_native`]) and
///   prints min, median and mean per iteration.
/// - `--check`: runs each body once, compiled, with no timing; exit 1 if any
///   trapped.
/// - `--json`: the machine-readable report.
/// - `--compare <baseline.json>`: see [`bench_compare`].
///
/// `--check` excludes `--json` and `--compare`.
fn bench_cmd(path: &str, rest: &[String]) -> ExitCode {
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
    let (program, dsg) = match loaded(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);

    let matches = |name: &str| filter.as_deref().is_none_or(|sub| name.contains(sub));
    let has_selected = program
        .benches
        .iter()
        .any(|b| b.module.is_none() && matches(&b.name));
    if !has_selected {
        println!("no benches");
        return ExitCode::SUCCESS;
    }

    if check {
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
        return bodies_wasm(path, &program, &dsg, "bench", &bodies);
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
    let (code, _) = bench_native(path, filter.as_deref(), json, false);
    code
}

/// Lifts the selected bench bodies to functions, replaces `main` with a
/// `std/bench` harness, builds it on the native route and runs it. With
/// `capture`, returns the harness's stdout.
fn bench_native(
    path: &str,
    filter: Option<&str>,
    json: bool,
    capture: bool,
) -> (ExitCode, Option<String>) {
    use vyrn_frontend::ast::{Block, Expr, Stmt, Type};

    // The harness import is appended, so every original line keeps its number.
    // One load, not two: the loader's name-privacy rename works only across
    // modules it sees in one load, and a merged second load bound `std/bench`'s
    // private calls to a user function of the same name.
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return (ExitCode::from(2), None);
        }
    };
    let (mut program, dsg) = match loaded(
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

    let selected: Vec<vyrn_frontend::ast::NamedBlock> = program
        .benches
        .iter()
        .filter(|b| b.module.is_none() && filter.is_none_or(|sub| b.name.contains(sub)))
        .cloned()
        .collect();
    let mut harness_stmts: Vec<Stmt> = Vec::new();
    let mut width = 0i64;
    for b in &selected {
        // The label `bench "<name>"`.
        let w = (b.name.len() + 8) as i64;
        if w > width {
            width = w;
        }
    }
    let mut measure_calls: Vec<Expr> = Vec::new();
    for (slot, b) in selected.iter().enumerate() {
        program.functions.push(synth_fn(
            format!("__vyrn_bench_body_{slot}"),
            b.body.clone(),
            Type::Unit,
            b.line,
            false,
        ));
        let body_ref = Expr::Var {
            name: format!("__vyrn_bench_body_{slot}"),
            line: 0,
        };
        if json {
            measure_calls.push(Expr::Call {
                dot: false,
                type_args: Vec::new(),
                name: "benchMeasure".to_string(),
                args: vec![Expr::Str(b.name.clone()), body_ref],
                line: 0,
            });
        } else {
            harness_stmts.push(Stmt::Expr(Expr::Call {
                dot: false,
                type_args: Vec::new(),
                name: "benchOne".to_string(),
                args: vec![Expr::Str(b.name.clone()), Expr::Int(width), body_ref],
                line: 0,
            }));
        }
    }
    if json {
        // `print(benchJson([benchMeasure(..), ..], "native", "O2"))`.
        harness_stmts.push(Stmt::Expr(Expr::Call {
            dot: false,
            type_args: Vec::new(),
            name: "print".to_string(),
            args: vec![Expr::Call {
                dot: false,
                type_args: Vec::new(),
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
        harness_stmts.push(Stmt::Expr(Expr::Call {
            dot: false,
            type_args: Vec::new(),
            name: "print".to_string(),
            args: vec![Expr::Str(String::new())],
            line: 0,
        }));
        harness_stmts.push(Stmt::Expr(Expr::Call {
            dot: false,
            type_args: Vec::new(),
            name: "print".to_string(),
            args: vec![Expr::Str(format!("{} benches", selected.len()))],
            line: 0,
        }));
    }
    harness_stmts.push(Stmt::Return {
        value: Some(Expr::Int(0)),
        line: 0,
    });

    program.functions.retain(|f| f.name != "main");
    program.functions.push(synth_fn(
        "main".to_string(),
        Block {
            stmts: harness_stmts,
        },
        Type::Int,
        0,
        false,
    ));
    program.benches.clear();
    program.tests.clear();

    // The route and target `vyrn build` ships, so the timing describes the
    // artifact.
    let target = match native_target_for(path) {
        Ok(t) => t,
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
    // As a test host: the lifted bodies are checked again for the lowering's
    // record, and outside a host `blackBox` is refused, so the core cannot
    // lower the bodies and their locals leak.
    vyrn_frontend::checker::set_test_host(true);
    let built = build_wasm2c(path, &program, &dsg, &out_path.to_string_lossy(), target);
    vyrn_frontend::checker::set_test_host(false);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return (ExitCode::FAILURE, None);
    }
    // `VYRN_BENCH_KEEP` leaves the temp dir (the wasm, the C, the binary) for a
    // debugger: a binary that faults dies before its report line.
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

/// The bench names an ungate file lists: one per line, `#` starts a comment.
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

/// `(name, minNs)` per bench of a `--json` report or baseline, in order. `None`
/// if `doc` is not `{ benches: [ { name, minNs } ] }`.
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

/// A baseline not yet seeded from CI: `"placeholder": true` or an empty
/// `benches` array. `--compare` then reports every bench `new`.
fn baseline_is_placeholder(doc: &vyrn_frontend::schema::Json) -> bool {
    use vyrn_frontend::schema::Json;
    if let Some(Json::Bool(true)) = doc.get("placeholder") {
        return true;
    }
    matches!(doc.get("benches"), Some(Json::Arr(items)) if items.is_empty())
}

/// `vyrn bench --compare <baseline.json> [--threshold <factor>]`: runs the
/// benches and compares each min against the baseline's, corrected by
/// [`bench_host_scale`]. Only a `Verdict::Regressed` fails the command.
fn bench_compare(
    path: &str,
    filter: Option<&str>,
    baseline_path: &str,
    threshold: f64,
    ungate_path: Option<&str>,
) -> ExitCode {
    // The baseline first, so a broken one fails before the slow run.
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

    // Its own file: a reseed replaces `bench/baseline.json` verbatim.
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

/// One bench's comparison outcome. A factor is the host-normalized
/// `min / baselineMin`.
#[derive(Debug, PartialEq)]
enum Verdict {
    Ok,
    /// Slower than `baselineMin * threshold`.
    Regressed(f64),
    /// Over the threshold in a bench the ungate list names; not a regression.
    Ungated(f64),
    /// In the run, absent from the baseline.
    New,
    /// In the baseline, absent from the run.
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

/// How much slower this host is than the baseline's, as the median of every
/// matched bench's `min / baselineMin`.
///
/// CI runners drift 1.2x to 1.35x between CPU generations. A regression moves a
/// few rows and barely moves the median; a slow runner moves every row. Below
/// [`BENCH_SCALE_QUORUM`] matched rows the median would be the regression
/// itself, so the scale is 1.0.
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

/// The matched benches [`bench_host_scale`] needs: enough that one regressed
/// row cannot carry the median, few enough that the corpus's larger files get
/// the correction.
const BENCH_SCALE_QUORUM: usize = 8;

/// Compares each run bench's min against the same-named baseline entry. Returns
/// the verdicts (run benches in order, then baseline-only ones) and the count
/// of regressions. A zero or absent baseline min is `New`.
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

/// One HTTP request for a served `handle`, as read off the wire.
pub struct ServeRequest {
    pub method: String,
    pub path: String,
    /// In wire order, names lowercased here so the program's `Map` has one
    /// spelling per header.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The fields a served `handle`'s `Response` returned, for the wire.
pub struct ServeResponse {
    pub status: i64,
    pub content_type: String,
    pub body: String,
    /// The `Vary` header, or `""` for none.
    pub vary: String,
    /// Every other header, in insertion order, written verbatim.
    pub headers: Vec<(String, String)>,
}

/// What the host asks the engine for. A stream is pulled after the call that
/// opened it returned.
pub enum ServeCall {
    Handle(ServeRequest),
    /// The next frame of the stream the last [`ServeAnswer::Live`] opened.
    Next,
    /// Release that stream: sent when it ends and the first time a write to the
    /// client fails.
    Close,
}

/// What the engine answers.
pub enum ServeAnswer {
    /// A complete response, with the `Vary` and conditional-request handling
    /// only a complete response has.
    Buffered(ServeResponse),
    /// A stream's header block: status, content type and headers, plus a `body`
    /// the host writes once as the stream's prologue (SSE's `retry:` line).
    /// Frames follow, one `Next` at a time.
    Live(ServeResponse),
    /// The answer to `Next`: one frame, or `None` when the producer ended.
    Frame(Option<String>),
    /// The answer to `Close`.
    Released,
}

/// The exports `vyrn serve` calls, appended to the served root before the load
/// so every check judges them with the program.
///
/// - The direct backend exports only `_start` and `export extern fn`s, so
///   `handle` gets an exported wrapper.
/// - `Request` and `Response` cross field by field, one call each, under
///   the extern String ABI.
/// - `serveStream` traps in a compiled build, which has no accept loop;
///   [`serve_rewrite`] turns it into `vyrnServePark`, which boxes the producer
///   into module state for the host to pull a frame at a time.
const SERVE_SHIM: &str = r#"
// ---- `vyrn serve`, appended by the CLI -------

let mut vyrnServeMethod: String = ""
let mut vyrnServePath: String = ""
let mut vyrnServeInKeys: Array<String> = []
let mut vyrnServeInVals: Array<String> = []
let mut vyrnServeInBody: String = ""
let mut vyrnServeStatus: Int64 = 0
let mut vyrnServeType: String = ""
let mut vyrnServeBody: String = ""
let mut vyrnServeVary: String = ""
let mut vyrnServeOutKeys: Array<String> = []
let mut vyrnServeOutVals: Array<String> = []
let mut vyrnServeLive: Int64 = 0
let mut vyrnServeFrame: String = ""

/// What `serveStream` becomes on the served route. The producer goes into one
/// box and its address into module state, which is the only place a `Stream<T>`
/// may rest: linearity forbids a field, and `boxStream` exists for exactly
/// this.
fn vyrnServePark(s: consume Stream<String>) {
    if vyrnServeLive != 0 {
        panic("serveStream: this request already opened a stream")
    }
    vyrnServeLive = boxStream(s)
}

/// One request's fields, one call each.
export extern fn vyrnServeBegin() {
    vyrnServeMethod = ""
    vyrnServePath = ""
    vyrnServeInKeys = []
    vyrnServeInVals = []
    vyrnServeInBody = ""
}

export extern fn vyrnServeMethodIs(s: String) {
    vyrnServeMethod = s.copy()
}

export extern fn vyrnServePathIs(s: String) {
    vyrnServePath = s.copy()
}

export extern fn vyrnServeHeaderIs(k: String, v: String) {
    vyrnServeInKeys.push(k.copy())
    vyrnServeInVals.push(v.copy())
}

export extern fn vyrnServeBodyIs(s: String) {
    vyrnServeInBody = s.copy()
}

/// Call `handle` on the fields collected above and keep what it answered. The
/// result says whether a producer is parked behind the response, which is what
/// makes it a live answer rather than a buffered one.
export extern fn vyrnServeHandle() -> Bool {
    let mut hs: Map<String, String> = [:]
    let mut i = 0
    while i < vyrnServeInKeys.length {
        let k = vyrnServeInKeys[i].copy()
        let v = vyrnServeInVals[i].copy()
        hs[k] = v
        i = i + 1
    }
    let req = Request {
        method: vyrnServeMethod.copy(),
        path: vyrnServePath.copy(),
        headers: hs,
        body: vyrnServeInBody.copy(),
    }
    let r = handle(req)
    vyrnServeStatus = r.status
    vyrnServeType = r.contentType.copy()
    vyrnServeBody = r.body.copy()
    vyrnServeVary = r.vary.copy()
    vyrnServeOutKeys = []
    vyrnServeOutVals = []
    for k in r.headers.keys() {
        if let Some(v) = r.headers[k] {
            vyrnServeOutKeys.push(k.copy())
            vyrnServeOutVals.push(v.copy())
        }
    }
    return vyrnServeLive != 0
}

export extern fn vyrnServeStatusOf() -> Int64 {
    return vyrnServeStatus
}

export extern fn vyrnServeTypeOf() -> String {
    return vyrnServeType.copy()
}

export extern fn vyrnServeBodyOf() -> String {
    return vyrnServeBody.copy()
}

export extern fn vyrnServeVaryOf() -> String {
    return vyrnServeVary.copy()
}

export extern fn vyrnServeHeaderCount() -> Int64 {
    return vyrnServeOutKeys.length
}

export extern fn vyrnServeHeaderKey(i: Int64) -> String {
    return vyrnServeOutKeys[i].copy()
}

export extern fn vyrnServeHeaderValue(i: Int64) -> String {
    return vyrnServeOutVals[i].copy()
}

/// One frame off the parked producer. `false` is the end of the stream, and the
/// host answers it by closing. The box comes out of module state for the pull
/// and goes back after it, because the step is ordinary Vyrn and may reach
/// `serveStream` itself — the newest producer wins.
export extern fn vyrnServeNext() -> Bool {
    vyrnServeFrame = ""
    if vyrnServeLive == 0 {
        return false
    }
    let at = vyrnServeLive
    vyrnServeLive = 0
    let got: Option<String> = pullAt(at)
    if vyrnServeLive == 0 {
        vyrnServeLive = at
    } else {
        let old: Stream<String> = unboxStream(at)
        close(old)
    }
    if let Some(f) = got {
        vyrnServeFrame = f.copy()
        return true
    }
    return false
}

export extern fn vyrnServeFrameOf() -> String {
    return vyrnServeFrame.copy()
}

/// Release the producer. The host sends this when the stream ends and the first
/// time a write to the client fails, which is how it learns the client is gone.
export extern fn vyrnServeClose() {
    if vyrnServeLive != 0 {
        let at = vyrnServeLive
        vyrnServeLive = 0
        let s: Stream<String> = unboxStream(at)
        close(s)
    }
}

/// The entry point a served file need not have. The CLI renames this
/// to `main` when the program declares none, because the direct backend has no
/// `_start` without one — and `_start` is what initializes module state.
fn vyrnServeMain() -> Int64 {
    return 0
}
"#;

/// Turns a loaded program into the one the serving host runs: every
/// `serveStream` call becomes `vyrnServePark`, and `vyrnServeMain` becomes
/// `main` when the program has none.
///
/// Both are renames after the check, sound because `vyrnServePark` has
/// `serveStream`'s signature (`consume Stream<String>` to `Unit`).
fn serve_rewrite(program: &mut vyrn_frontend::ast::Program) {
    use vyrn_frontend::ast::Expr;
    // The renames leave `own::ident` unchanged, so the next guard would adopt
    // the load's judgment of the program before them.
    vyrn_frontend::own::forget_loaded();
    let has_main = program
        .functions
        .iter()
        .any(|f| f.name == "main" && f.module.is_none());
    for f in &mut program.functions {
        if !has_main && f.name == "vyrnServeMain" {
            f.name = "main".to_string();
        }
        vyrn_frontend::project::walk_block(&mut f.body, &mut |e| {
            if let Expr::Call { name, args, .. } = e {
                if name == "serveStream" && args.len() == 1 {
                    *name = "vyrnServePark".to_string();
                }
            }
        });
    }
}

/// Answers one [`ServeCall`] on a resident instance through the
/// [`SERVE_SHIM`] exports.
fn serve_wasm_call(res: &mut wasmrun::Resident, call: ServeCall) -> Result<ServeAnswer, String> {
    match call {
        ServeCall::Handle(req) => {
            res.tell("vyrnServeBegin", &[])?;
            res.tell("vyrnServeMethodIs", &[&req.method])?;
            res.tell("vyrnServePathIs", &[&req.path])?;
            for (k, v) in &req.headers {
                res.tell("vyrnServeHeaderIs", &[k, v])?;
            }
            res.tell("vyrnServeBodyIs", &[&req.body])?;
            let live = res.ask_bool("vyrnServeHandle")?;
            let n = res.ask_int("vyrnServeHeaderCount")?;
            let mut headers = Vec::with_capacity(n.max(0) as usize);
            for i in 0..n {
                headers.push((
                    res.ask_text("vyrnServeHeaderKey", Some(i))?,
                    res.ask_text("vyrnServeHeaderValue", Some(i))?,
                ));
            }
            let resp = ServeResponse {
                status: res.ask_int("vyrnServeStatusOf")?,
                content_type: res.ask_text("vyrnServeTypeOf", None)?,
                body: res.ask_text("vyrnServeBodyOf", None)?,
                vary: res.ask_text("vyrnServeVaryOf", None)?,
                headers,
            };
            Ok(if live {
                ServeAnswer::Live(resp)
            } else {
                ServeAnswer::Buffered(resp)
            })
        }
        ServeCall::Next => {
            if res.ask_bool("vyrnServeNext")? {
                Ok(ServeAnswer::Frame(Some(
                    res.ask_text("vyrnServeFrameOf", None)?,
                )))
            } else {
                Ok(ServeAnswer::Frame(None))
            }
        }
        ServeCall::Close => {
            res.tell("vyrnServeClose", &[])?;
            Ok(ServeAnswer::Released)
        }
    }
}

/// `vyrn serve [file] [--port N] [--workers N]`: an HTTP/1.1 host on `std::net`
/// running the file's `handle`, by default on port 8080. See [`serve_loop`].
fn serve_cmd(path: &str, rest: &[String]) -> ExitCode {
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
    // Appended before the load, so it is checked and every program line keeps
    // its number.
    let source = format!("{source}\n{SERVE_SHIM}");
    let (mut program, dsg) = match loaded(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    serve_rewrite(&mut program);
    let program = program;
    let _memo = shared_desugars(&program);

    if !has_served_handle(&program) {
        eprintln!("error: `vyrn serve` needs `fn handle(req: Request) -> Response` in {path}");
        return ExitCode::FAILURE;
    }

    // Bind before running `main`, so a port clash fails first. `--port 0` lets
    // the OS pick.
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let actual_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let file_label = path.to_string();

    serve_loop(
        &program,
        &dsg,
        vec![path.to_string()],
        listener,
        workers,
        None,
        "serve",
        move |n| {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            match n {
                Some(n) => eprintln!(
                    "serving {file_label} on http://localhost:{actual_port} with {n} workers"
                ),
                None => eprintln!("serving {file_label} on http://localhost:{actual_port}"),
            }
        },
    )
}

/// Whether the program has `fn handle(req: Request) -> Response`, exactly. The
/// checker states the rule again over its own signature table.
fn has_served_handle(program: &vyrn_frontend::ast::Program) -> bool {
    use vyrn_frontend::ast::Type;
    program.functions.iter().any(|f| {
        f.name == "handle"
            && !f.is_extern
            && f.params.len() == 1
            && f.params[0].ty == Type::Named("Request".to_string())
            && f.ret == Type::Named("Response".to_string())
    })
}

/// The serving loop of `vyrn serve` and `vyrn dev`.
///
/// Without `--workers`, one resident instance answers every request, one at a
/// time: `_start` runs `main` once and the store stays open, so each request
/// sees what `main` wrote. With it, [`serve_pool_wasm`] answers, behind
/// [`refuse_workers_if_stateful`].
///
/// `banner` prints once `main` has run, given the worker count. `assets` is
/// `vyrn dev`'s static tree.
fn serve_loop(
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
    argv: Vec<String>,
    listener: std::net::TcpListener,
    workers: Option<usize>,
    assets: Option<&DevAssets>,
    what: &str,
    banner: impl Fn(Option<usize>) + Send,
) -> ExitCode {
    if let Some(n) = workers {
        if let Some(exit) = refuse_workers_if_stateful(program) {
            return exit;
        }
        let (tx, rx) = std::sync::mpsc::channel::<std::net::TcpStream>();
        let rx = std::sync::Mutex::new(rx);
        let each =
            |_i: usize, call_handle: &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>| loop {
                // Each idle worker takes the next connection.
                let stream = rx.lock().unwrap().recv();
                match stream {
                    Ok(mut s) => serve_one(&mut s, assets, call_handle),
                    Err(_) => break, // accept loop gone; drain out
                }
            };
        let listen = move || {
            banner(Some(n));
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
        };
        return match serve_pool_wasm(program, memo, argv, n, each, listen) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let bytes = match vyrn_codegen::direct::compile(program, memo) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let run = wasmrun::Run {
        argv,
        stdin_prefix: Vec::new(),
        capture_stdout: false,
        // Read per call, so a trap logs its wording, not a wasm backtrace.
        capture_stderr: true,
        meter: false,
    };
    let mut res = match wasmrun::start(&bytes, &run, None) {
        Ok((res, 0)) => res,
        Ok((mut res, code)) => {
            eprint!("{}", res.drain_err());
            eprintln!("error: main returned {code}, aborting {what}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprint!("{}", res.drain_err());
    banner(None);
    let mut call_handle = |call| serve_wasm_call(&mut res, call);
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => serve_one(&mut s, assets, &mut call_handle),
            Err(_) => continue,
        }
    }
    ExitCode::SUCCESS
}

/// The `--workers` pool: one resident instance per worker thread, over one
/// compile.
///
/// `main` runs once, on a setup instance that is then dropped. The workers run
/// a copy whose `main` returns 0, so each still initializes its module state
/// and none repeats `main`'s effects. Sound only because
/// [`refuse_workers_if_stateful`] proved `handle` touches no module state.
fn serve_pool_wasm<W, A>(
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
    argv: Vec<String>,
    workers: usize,
    worker: W,
    accept: A,
) -> Result<(), String>
where
    W: Fn(usize, &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>) + Send + Sync,
    A: FnOnce() -> Result<(), String> + Send,
{
    use vyrn_frontend::ast::{Block, Expr, Stmt};
    let run = wasmrun::Run {
        argv,
        stdin_prefix: Vec::new(),
        capture_stdout: false,
        capture_stderr: true,
        meter: false,
    };
    let bytes = vyrn_codegen::direct::compile(program, memo)?;
    let (mut setup, code) = wasmrun::start(&bytes, &run, None)?;
    eprint!("{}", setup.drain_err());
    if code != 0 {
        return Err(format!("main returned {code}, aborting serve"));
    }
    drop(setup);

    let mut quiet = program.clone();
    if let Some(main) = quiet
        .functions
        .iter_mut()
        .find(|f| f.name == "main" && f.module.is_none())
    {
        main.body = Block {
            stmts: vec![Stmt::Return {
                value: Some(Expr::Int(0)),
                line: main.line,
            }],
        };
    }
    let module = wasmrun::compile(&vyrn_codegen::direct::compile(&quiet, memo)?, false)?;

    std::thread::scope(|s| {
        let (worker, module, run) = (&worker, &module, &run);
        for i in 0..workers {
            std::thread::Builder::new()
                .stack_size(vyrn_frontend::trap::RUN_STACK_BYTES)
                .spawn_scoped(s, move || {
                    let mut res = match wasmrun::start_on(module, run, None) {
                        Ok((res, 0)) => res,
                        Ok((_, code)) => {
                            eprintln!("error: worker {i}: main returned {code}");
                            return;
                        }
                        Err(e) => {
                            eprintln!("error: worker {i}: {e}");
                            return;
                        }
                    };
                    let mut handler = |call| serve_wasm_call(&mut res, call);
                    worker(i, &mut handler);
                })
                .expect("failed to spawn a worker thread");
        }
        accept()
    })
}

/// The `--workers` gate: `handle` must reach no module state, transitively.
/// Prints the refusal with the call path and returns the exit code, or `None`
/// when workers are sound. Other effects (`print`, file I/O) are allowed: each
/// output line stays atomic.
fn refuse_workers_if_stateful(program: &vyrn_frontend::ast::Program) -> Option<ExitCode> {
    // Calls through stored function values reach every collected source.
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

/// `vyrn dev [--port N] [--workers N]`: builds the manifest's `client` to wasm,
/// then serves the `server` root's `handle` with static assets in front (see
/// [`dev_static_path`]). `public` defaults to `public`.
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

    let source = match std::fs::read_to_string(&server_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {server_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = format!(
        "{source}
{SERVE_SHIM}"
    );
    let (mut program, dsg) = match loaded(&server_path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    serve_rewrite(&mut program);
    let program = program;
    let _memo = shared_desugars(&program);
    if !has_served_handle(&program) {
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

    let public_shown = assets.public_dir.display().to_string();

    serve_loop(
        &program,
        &dsg,
        vec![server_path.clone()],
        listener,
        workers,
        Some(&assets),
        "dev",
        move |n| {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            eprintln!("dev: serving {server_rel} on http://localhost:{actual_port}");
            eprintln!("dev:   /rpc/*         -> server `handle` (rpcHandle + your pages)");
            eprintln!("dev:   /client.wasm   -> built from {client_rel}");
            eprintln!(
                "dev:   /vyrn-runtime/ -> web runtimes (wasi-min.js, vyrn-rpc.js, vyrn-query.js)"
            );
            eprintln!("dev:   /              -> {public_shown}/");
            if let Some(n) = n {
                eprintln!("dev:   workers        -> {n}");
            }
        },
    )
}

/// Static asset roots for `vyrn dev`.
struct DevAssets {
    public_dir: PathBuf,
    web_dir: String,
    wasm: PathBuf,
}

/// Resolves a GET path to a static file, or `None` so the request goes to
/// `handle`. In order: `/client.wasm`, `/vyrn-runtime/<name>`, then the public
/// dir (`/` is `index.html`). [`safe_rel`] and [`file_under`] keep the result
/// inside its root.
fn dev_static_path(path: &str, assets: &DevAssets) -> Option<PathBuf> {
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
    let rel = if raw == "/" {
        "index.html".to_string()
    } else {
        raw.trim_start_matches('/').to_string()
    };
    file_under(&assets.public_dir, &safe_rel(&rel)?)
}

/// A request path as a relative path that cannot escape by itself: backslashes
/// become `/` before the `..` check, and absolute or drive-letter forms are
/// refused because [`Path::join`] replaces its base for those.
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

/// `root` joined with `rel`, if the canonical result is inside the canonical
/// root (symlinks included).
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

/// The value of a lowercased request-header name.
fn request_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Whether `host` names the loopback interface this server bound. Anything else
/// is a DNS-rebinding page; the peer address is local either way.
fn loopback_host(host: &str) -> bool {
    // An IPv6 literal keeps its brackets: `[::1]:8080`.
    let bare = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        host.split(':').next().unwrap_or(host)
    };
    // A host name is case-insensitive (RFC 9110 section 4.2.3).
    bare.eq_ignore_ascii_case("localhost") || matches!(bare, "127.0.0.1" | "::1")
}

/// Whether `origin`'s authority is `host`. A browser writes the same port in
/// both, so they compare directly.
fn origin_is_host(origin: &str, host: &str) -> bool {
    let rest = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    rest.split(['/', '?', '#'])
        .next()
        .is_some_and(|a| a.eq_ignore_ascii_case(host))
}

/// The browser-origin gate every served request passes first. Returns the
/// refusal body, or `None`.
///
/// Any web page can make the visitor's browser write to `localhost` (a form
/// POST, a no-cors `fetch`, a WebSocket handshake). So `Host` must name the
/// loopback host, and `Origin`, when sent, must name the same authority. A
/// client that sends no `Origin` (`curl`, scripts) is not a page and passes.
fn cross_origin_body(req: &ServeRequest) -> Option<String> {
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

/// Logs a gate refusal and answers a plain-text 403.
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
    /// A malformed request line or header: 400.
    Bad,
    /// A `Transfer-Encoding: chunked` body, unsupported: 501. The method and
    /// path are for the access line.
    Chunked { method: String, path: String },
    /// A body larger than [`MAX_BODY`]: 413.
    TooLarge { method: String, path: String },
}

/// The largest request body this server holds, in bytes. Without it one
/// connection could make the process hold unbounded memory; a script with no
/// `Origin` passes the gate.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Handles one connection: parse, gate, then a static asset or `handle`, then
/// close. A trap is logged and answered 500, and the server keeps running.
///
/// `assets` is `vyrn dev`'s: a GET or HEAD that names one is answered off the
/// disk. `None` is `vyrn serve`.
fn serve_one(
    stream: &mut std::net::TcpStream,
    assets: Option<&DevAssets>,
    call_handle: &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>,
) {
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
    // Ahead of static assets and of the 101 upgrade path.
    if let Some(body) = cross_origin_body(&req) {
        write_cross_origin_refusal(stream, &req.method, &req.path, &body);
        return;
    }
    // GET and HEAD only, so nothing shadows a POST.
    if let Some(assets) = assets {
        if req.method == "GET" || req.method == "HEAD" {
            if let Some(file) = dev_static_path(&req.path, assets) {
                match std::fs::read(&file) {
                    Ok(bytes) => {
                        eprintln!("{} {} -> 200 (static)", req.method, req.path);
                        if req.method == "HEAD" {
                            // RFC 9110 section 9.3.2: GET's headers, true
                            // Content-Length included, and no body.
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
            eprintln!("error: {msg}");
            eprintln!("{method} {path} -> 500");
            write_response(stream, 500, "text/plain", b"internal error");
        }
    }
}

/// The first occurrence of `needle` in `hay`; `None` for an empty needle.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parses one HTTP/1.1 request: the request line, headers up to CRLF CRLF
/// (at most 64 KiB), then exactly `Content-Length` body bytes.
fn parse_request(stream: &mut std::net::TcpStream) -> Result<ServeRequest, ParseError> {
    use std::io::Read;
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
    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| ParseError::Bad)?;
    let mut lines = head.split("\r\n");

    // METHOD SP TARGET SP HTTP/x.y
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

    // Names are lowercased here, where they cross into a Vyrn `Map`. Repeated
    // fields join with ", " (RFC 9110 section 5.3).
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
    // Refused on the announced length, before a byte of the body is read.
    if content_length > MAX_BODY {
        return Err(ParseError::TooLarge {
            method,
            path: target,
        });
    }

    // Some body bytes may already be buffered. No Content-Length means no body.
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
    // A Vyrn `String` is UTF-8; lossy decoding would corrupt the body.
    let body = String::from_utf8(body).map_err(|_| ParseError::Bad)?;

    Ok(ServeRequest {
        method,
        path: target,
        headers,
        body,
    })
}

/// The reason phrase for a status code; `""` for an unknown one.
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

/// Writes one open stream to the wire: no `Content-Length` (the body ends with
/// the connection) and no `Vary`/`ETag`/304.
///
/// The disconnect signal is the write: the first failed write or flush sends
/// `Close`, which releases the producer before it yields another frame.
///
/// The first frame is pulled before the header block, so a producer with
/// nothing to say answers 204, which stops an `EventSource` from reconnecting
/// (WHATWG HTML section 9.2.5).
fn pump_stream(
    stream: &mut std::net::TcpStream,
    head: &ServeResponse,
    call_handle: &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>,
) {
    use std::io::Write;
    use ServeCall;

    // A WebSocket handshake is a 101.
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
    // `body` is the stream's prologue (SSE's `retry:`), before the first frame.
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

/// Asks the open stream for one frame; `None` at its end. A trapping producer
/// is logged and ends the connection.
fn pull_frame(
    call_handle: &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>,
) -> Option<String> {
    match call_handle(ServeCall::Next) {
        Ok(ServeAnswer::Frame(f)) => f,
        Ok(_) => None,
        Err(msg) => {
            eprintln!("error: {msg}");
            None
        }
    }
}

/// Writes one open stream to a WebSocket: [`pump_stream`]'s loop, with each
/// payload framed by the host.
///
/// The head's `body` carries the close code and the fragment limit, since a 101
/// has no prologue. Server-push only: inbound frames are parsed because RFC
/// 6455 requires answering a close, but no handler receives a client message.
fn pump_socket(
    stream: &mut std::net::TcpStream,
    head: &ServeResponse,
    call_handle: &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>,
) {
    use std::io::Write;
    use ServeCall;

    // `closeCode` and `maxFrame`, in the slot SSE uses for its prologue.
    let mut nums = head.body.split_whitespace();
    let mut close_code: u16 = nums.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
    let max_frame: usize = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let mut extra = String::new();
    for (name, value) in &head.headers {
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    // No `Connection: close` and no `Content-Length`: frames follow.
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
            // The disconnect signal, as in `pump_stream`.
            let _ = call_handle(ServeCall::Close);
            return;
        }
        match ws_drain(stream, &mut inbox) {
            WsIn::Open => {}
            // RFC 6455 section 5.5.1: a close frame is answered with one.
            WsIn::Closed => break,
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

/// What the inbound half of a socket reports between two outbound messages.
/// It never reports the peer gone: the failed write is the one disconnect
/// signal.
enum WsIn {
    /// Nothing that ends the connection, EOF and read errors included.
    Open,
    /// A close frame from the client.
    Closed,
    /// A frame that breaks RFC 6455: close with 1002.
    Protocol,
}

/// Reads whatever inbound bytes are waiting, without waiting for any.
///
/// The socket is non-blocking for the read alone: a non-blocking write can
/// answer `WouldBlock`, which would read as a disconnect.
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
        // ponytail: a 16 MiB ceiling on one inbound frame; an unbounded length
        // would let a peer name a buffer this loop waits forever to fill.
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
            // Section 5.5.2: a ping is answered with a pong carrying its
            // payload. A failed pong is left to the next write to report.
            9 => {
                let _ = ws_write_frame(stream, 10, true, &body);
            }
            // Data and pongs are dropped: nothing receives them.
            _ => {}
        }
    }
}

/// One text message as one frame, or as fragments of at most `max_frame` bytes
/// (`0` means no limit). A fragment may split a UTF-8 sequence: RFC 6455
/// validates the reassembled message.
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

/// One frame on the wire, never masked: RFC 6455 section 5.1 forbids a server
/// to mask.
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

/// Writes one HTTP/1.1 response with `Connection: close`. Write errors are
/// ignored: a peer that hung up must not fault the server.
fn write_response(stream: &mut std::net::TcpStream, status: i64, content_type: &str, body: &[u8]) {
    write_response_vary(stream, status, content_type, "", &[], body)
}

/// [`write_response`] for HEAD: GET's headers and length, and no body. A
/// strict client reads body bytes after a HEAD as the next response.
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

/// [`write_response`] plus a `Vary` field and the response's own headers. An
/// empty `vary` or `content_type` writes no line: a 304 has no media type.
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
    // RFC 9110 section 8.6: no Content-Length in a 204.
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

/// `vyrn run`: compiles the program and runs it in the embedded wasmtime; the
/// exit code is the guest's. `profile` is the load's time under
/// `vyrn run --profile` (see [`wasm_profile`]).
fn run_wasm(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
    prog_args: &[String],
    profile: Option<std::time::Duration>,
) -> ExitCode {
    let clock = std::time::Instant::now();
    let bytes = match vyrn_codegen::direct::compile(program, memo) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let compile = clock.elapsed();
    let mut argv = vec![path.to_string()];
    argv.extend(prog_args.iter().cloned());
    let run = wasmrun::Run {
        argv,
        stdin_prefix: Vec::new(),
        capture_stdout: false,
        capture_stderr: false,
        meter: profile.is_some(),
    };
    match wasmrun::run(&bytes, run) {
        Ok(out) => {
            if let (Some(load), Some(meter)) = (profile, out.meter.as_ref()) {
                wasm_profile(load, compile, meter);
            }
            ExitCode::from((out.code & 0xff) as u8)
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints a compiled run's profile to stderr: the phase table and the
/// operations the guest executed.
///
/// The count is wasmtime's fuel, read from a budget nothing exhausts. Unlike
/// the times, it is the same number on any machine.
fn wasm_profile(load: std::time::Duration, compile: std::time::Duration, meter: &wasmrun::Meter) {
    vyrn_frontend::prof::charge("load", load);
    vyrn_frontend::prof::charge("compile", compile);
    vyrn_frontend::prof::charge("translate", meter.translate);
    vyrn_frontend::prof::charge("instantiate", meter.instantiate);
    vyrn_frontend::prof::charge("run", meter.run);
    eprint!("{}", vyrn_frontend::prof::phase_table());
    eprintln!(
        "
{} operation(s) executed",
        meter.fuel
    );
}

/// One `test` or `bench` body, as [`bodies_wasm`] runs it.
struct Body {
    name: String,
    body: vyrn_frontend::ast::Block,
    line: usize,
}

/// `vyrn test` and `vyrn bench --check`: runs each body once as compiled wasm.
///
/// One module and one instance, with each body an exported
/// `__vyrn_body_<k>`, so body `k+1` sees the module state body `k` wrote.
/// A trap ends the call, not the store: its message becomes the
/// `FAILED:` line and the next body runs.
fn bodies_wasm(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
    kind: &str,
    bodies: &[Body],
) -> ExitCode {
    use vyrn_frontend::ast::{Block, Expr, Stmt, Type};
    if bodies.is_empty() {
        // `test` said `no tests` before the filter; `bench` says it after.
        println!("no {kind}es");
        return ExitCode::SUCCESS;
    }
    let mut prog = program.clone();
    prog.functions
        .retain(|f| !(f.name == "main" && f.module.is_none()));
    prog.tests.clear();
    prog.benches.clear();
    for (k, b) in bodies.iter().enumerate() {
        prog.functions.push(synth_fn(
            format!("__vyrn_body_{k}"),
            b.body.clone(),
            Type::Unit,
            b.line,
            true,
        ));
    }
    // `_start` initializes module state and needs a `main`; this one does
    // nothing else.
    let main = Block {
        stmts: vec![Stmt::Return {
            value: Some(Expr::Int(0)),
            line: 0,
        }],
    };
    prog.functions
        .push(synth_fn("main".to_string(), main, Type::Int, 0, false));

    // A body that reaches a `gen fn` compiles the module as a generator host;
    // otherwise it pays for no `vyrn_gen` import. As a test host, the checker
    // accepts `assert`, `assertEq` and `blackBox` in the lifted bodies.
    vyrn_frontend::checker::set_test_host(true);
    let reach = vyrn_codegen::direct::gen_reach(&prog);
    let generation = (0..bodies.len()).any(|k| reach.contains(&format!("__vyrn_body_{k}")));
    let compiled = if generation {
        vyrn_genwasm::prepare(&mut prog)
            .ok_or_else(|| "a `test` block calls a generator this route cannot compile".to_string())
            .and_then(|()| vyrn_codegen::direct::compile_gen_host(&prog))
    } else {
        vyrn_codegen::direct::compile(&prog, memo)
    };
    vyrn_frontend::checker::set_test_host(false);
    let bytes = match compiled {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let run = wasmrun::Run {
        argv: vec![path.to_string()],
        stdin_prefix: Vec::new(),
        capture_stdout: false,
        // Read per body: a trap's wording is the `FAILED:` message.
        capture_stderr: true,
        meter: false,
    };
    let gen = generation.then(|| vyrn_genwasm::GenState::new(&prog));
    let mut res = match wasmrun::start(&bytes, &run, gen) {
        Ok((res, _)) => res,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    use std::io::Write;
    let (mut ok, mut failed) = (0usize, 0usize);
    {
        // What the module's initializers wrote.
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(res.drain_err().as_bytes());
        let _ = stderr.flush();
    }
    for (k, b) in bodies.iter().enumerate() {
        let (rest, message) = res.call_body(&format!("__vyrn_body_{k}"));
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(rest.as_bytes());
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
    let mut out: Option<String> = None;
    let mut wasm = false;
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
        } else {
            eprintln!("build: unexpected argument `{}`", rest[i]);
            return ExitCode::from(2);
        }
    }

    // Before the compile, so a misspelled `nativeTarget` fails first. A wasm
    // build ignores it.
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

    let (program, dsg) = match loaded(path, &source) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let _memo = shared_desugars(&program);
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

    // The emitter's module, written as is. The native route starts from the
    // same bytes.
    if wasm {
        return match vyrn_codegen::direct::compile(&program, &dsg) {
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

    match build_wasm2c(
        path,
        &program,
        &dsg,
        &out_path,
        native_target.unwrap_or(DEFAULT_NATIVE_TARGET),
    ) {
        Ok(()) => {
            println!("wrote {out_path}");
            ExitCode::SUCCESS
        }
        Err(()) => ExitCode::FAILURE,
    }
}

/// The native route: the program's wasm through wasm2c to C, compiled by clang
/// with the WASI host and wabt's wasm-rt into an executable.
///
/// The intermediate files stay beside the output for inspection: `<out>.wasm`,
/// `<out>.w2c.c`, `<out>.w2c.h`, `<out>.host.c`.
fn build_wasm2c(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    memo: &Memo,
    out_path: &str,
    native_target: NativeTarget,
) -> Result<(), ()> {
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
            return Err(());
        }
        Err(e) => {
            eprintln!("error: {e}");
            return Err(());
        }
    };
    let Some((simde, _)) = vyrn_codegen::toolchain::simde_from(&start) else {
        eprintln!(
            "error: could not find simde. Unpack a simde release under tools/ \
             (tools/simde/simde/wasm/simd128.h) or set VYRN_SIMDE to the directory that \
             holds `simde/`."
        );
        return Err(());
    };
    let clang = match find_clang() {
        Some(c) => c,
        None => {
            eprintln!(
                "error: could not find `clang`. Install LLVM and put clang on PATH, \
                 or set the CLANG environment variable to its full path."
            );
            return Err(());
        }
    };

    let bytes = match vyrn_codegen::direct::compile(program, memo) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(());
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
        return Err(());
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
            return Err(());
        }
        Err(e) => {
            eprintln!("error: failed to run wasm2c ({}): {e}", w2c.exe.display());
            return Err(());
        }
    }
    // The host is written from wasm2c's header: the `vyrn` import namespace is
    // per program. See `toolchain::wasi_host_c`.
    let h_path = out.with_extension("w2c.h");
    let header = match std::fs::read_to_string(&h_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", h_path.display());
            return Err(());
        }
    };
    if !write(
        &host_path,
        vyrn_codegen::toolchain::wasi_host_c(&header).as_bytes(),
    ) {
        return Err(());
    }

    // The header is included by its bare name: the host sits beside it, and a
    // full path would put backslashes into a C string literal.
    let h_name = h_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `show_path` drops the `\\?\` prefix of a canonicalized start, which
    // clang's include search does not take.
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
        .arg(format!("-DVYRN_W2C_HEADER=\"{h_name}\""));
    // On Windows wasm-rt's guard-page handler maps a stack overflow to a trap,
    // so its per-prologue depth counter goes (2.5x on `benching.vyrn`'s
    // "push 1000"). The POSIX handler needs an alternate stack wasm-rt
    // allocates only when it picks the handler, so the counter stays there.
    // The emitter's own depth limit (`call_depth_enter`) applies either way.
    if cfg!(windows) {
        cmd.arg("-DWASM_RT_NONCONFORMING_UNCHECKED_STACK_EXHAUSTION=1");
    } else {
        cmd.arg(format!(
            "-DWASM_RT_MAX_CALL_STACK_DEPTH={}",
            4 * vyrn_frontend::trap::CALL_DEPTH_LIMIT
        ));
    }
    add_native_clang_flags(&mut cmd, native_target);
    if cfg!(windows) {
        // `random_get` is `BCryptGenRandom`, as in `wasmrun.rs`.
        cmd.arg("-lbcrypt");
    }
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!("error: clang exited with {s}");
            Err(())
        }
        Err(e) => {
            eprintln!("error: failed to run clang ({}): {e}", clang.display());
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, f64)]) -> Vec<(String, f64)> {
        entries.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }

    #[test]
    fn a_uniformly_slower_host_is_corrected_away() {
        let base: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 100.0)).collect();
        let run: Vec<(String, f64)> = (0..10).map(|i| (format!("b{i}"), 130.0)).collect();
        assert!((bench_host_scale(&run, &base) - 1.3).abs() < 1e-9);
        let (v, regressed) = bench_verdicts(&run, &base, 2.0, &[]);
        assert_eq!(regressed, 0);
        assert!(v.iter().all(|(_, x)| matches!(x, Verdict::Ok)));
    }

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

    #[test]
    fn a_small_file_compares_raw_because_its_median_is_the_regression() {
        let base = table(&[("a", 100.0), ("b", 100.0), ("c", 100.0)]);
        let run = table(&[("a", 300.0), ("b", 300.0), ("c", 300.0)]);
        assert_eq!(bench_host_scale(&run, &base), 1.0);
        assert_eq!(bench_verdicts(&run, &base, 2.0, &[]).1, 3);
    }

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

    #[test]
    fn default_native_target_is_v2_on_x86_64_and_absent_elsewhere() {
        assert_eq!(DEFAULT_NATIVE_TARGET, NativeTarget::V2);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(DEFAULT_NATIVE_TARGET.march(), Some("x86-64-v2"));
        } else {
            assert_eq!(DEFAULT_NATIVE_TARGET.march(), None);
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
        for bad in ["", "V2", "x86-64-v2", "haswell", "v5", "-march=native"] {
            assert_eq!(NativeTarget::parse(bad), None, "{bad} must not parse");
        }
    }

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

    /// A search window advanced one byte per miss would reslice inside the
    /// identifier's two-byte first character and panic.
    #[test]
    fn insert_copy_survives_an_identifier_starting_with_a_multibyte_char() {
        let text = "let δata = read()\nprint(δata)\n";
        let fixed = insert_copy(text, 2, "δata").unwrap();
        assert_eq!(fixed, "let δata = read()\nprint(δata.copy())\n");
    }

    #[test]
    fn insert_copy_still_counts_whole_occurrences_only() {
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
        assert_eq!(vyrn_frontend::schema::parse_json(&out).unwrap(), doc);
    }

    #[test]
    fn version_flag_counts_only_before_a_positional_argument() {
        let args = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert!(wants_version(&args(&["vyrn", "--version"])));
        assert!(wants_version(&args(&["vyrn", "-V"])));
        assert!(wants_version(&args(&[
            "vyrn",
            "--version",
            "run",
            "x.vyrn"
        ])));
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
        for bad in [
            "/../secret.txt",
            "/..\\secret.txt",
            "/vyrn-runtime/../secret.txt",
            "/vyrn-runtime/..\\secret.txt",
        ] {
            assert_eq!(go(bad), None, "{bad}");
        }
        for bad in ["/C:/Windows/win.ini", "/vyrn-runtime/C:\\Windows\\win.ini"] {
            assert_eq!(go(bad), None, "{bad}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cross_origin_gate_refuses_foreign_pages_and_rebound_hosts() {
        let req = |headers: &[(&str, &str)]| ServeRequest {
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
        let csrf = req(&[
            ("host", "127.0.0.1:8080"),
            ("origin", "https://evil.example"),
        ]);
        assert!(cross_origin_body(&csrf).is_some());
        // A rebound domain resolves to 127.0.0.1; only Host names it apart.
        let rebound = req(&[("host", "evil.example:8080")]);
        assert!(cross_origin_body(&rebound).is_some());
        // A client that sends no Origin is not a page.
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

    #[test]
    fn an_oversized_body_is_refused_before_it_is_read() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            serve_one(&mut stream, None, &mut |_call| {
                panic!("the request reached `handle` — the cap did not hold");
            });
        });
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        // A regression fails here instead of hanging the suite.
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("read timeout");
        let announced = MAX_BODY + 1;
        let request = format!(
            "POST /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {announced}\r\n\r\n"
        );
        client.write_all(request.as_bytes()).expect("write headers");
        // No body byte is sent: the refusal must not wait for one.
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
        assert!(loopback_host("LOCALHOST"));
        assert!(loopback_host("LocalHost:8080"));
        assert!(loopback_host("localhost:8080"));
        assert!(loopback_host("127.0.0.1:1"));
        assert!(loopback_host("[::1]:8080"));
        for foreign in ["", ":8080", "evil.example", "evil.example:8080", "[::1"] {
            assert!(!loopback_host(foreign), "{foreign}");
        }
    }

    /// The [`SERVE_SHIM`] exports in process, without a socket, so a failure
    /// names the export. With `--nocapture` it prints what one answer costs.
    #[test]
    fn the_serve_doors_answer_on_one_resident_instance() {
        const SRC: &str = r#"
let mut hits: Int64 = 0

fn main() -> Int64 {
    hits = 100
    return 0
}

fn handle(req: Request) -> Response {
    hits = hits + 1
    if req.path == "/live" {
        let xs: Array<String> = ["a", "b"]
        serveStream(fromArray(xs))
        return Response { status: 200, contentType: "text/event-stream", body: "p", vary: "", headers: [:] }
    }
    return Response { status: 200, contentType: "text/plain", body: "\{hits}", vary: "v", headers: ["X-Echo": req.method.copy()] }
}
"#;
        let dir = std::env::temp_dir().join("vyrn-serve-doors");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("doors-{}.vyrn", std::process::id()));
        let source = format!("{SRC}\n{SERVE_SHIM}");
        std::fs::write(&file, &source).unwrap();
        let key = file.to_string_lossy().replace('\\', "/");
        install();
        let (mut program, dsg) = loaded(&key, &source).expect("the doors load and check");
        serve_rewrite(&mut program);
        let bytes = vyrn_codegen::direct::compile(&program, &dsg).expect("the doors compile");
        let run = wasmrun::Run {
            argv: vec![key.clone()],
            stdin_prefix: Vec::new(),
            capture_stdout: true,
            capture_stderr: true,
            meter: false,
        };
        let (mut res, code) = wasmrun::start(&bytes, &run, None).expect("start");
        assert_eq!(code, 0, "main exits 0");

        let ask = |path: &str| ServeRequest {
            method: "GET".to_string(),
            path: path.to_string(),
            headers: vec![("host".to_string(), "localhost".to_string())],
            body: String::new(),
        };

        // `main` wrote 100 and the store outlived its exit.
        for want in ["101", "102"] {
            match serve_wasm_call(&mut res, ServeCall::Handle(ask("/x"))) {
                Ok(ServeAnswer::Buffered(r)) => {
                    assert_eq!(r.status, 200);
                    assert_eq!(r.content_type, "text/plain");
                    assert_eq!(r.body, want);
                    assert_eq!(r.vary, "v");
                    assert_eq!(r.headers, vec![("X-Echo".to_string(), "GET".to_string())]);
                }
                other => panic!("expected a buffered answer, got {:?}", other.map(|_| ())),
            }
        }

        match serve_wasm_call(&mut res, ServeCall::Handle(ask("/live"))) {
            Ok(ServeAnswer::Live(r)) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body, "p", "the prologue crosses with the header block");
            }
            other => panic!("expected a live answer, got {:?}", other.map(|_| ())),
        }
        for want in [Some("a"), Some("b"), None] {
            match serve_wasm_call(&mut res, ServeCall::Next) {
                Ok(ServeAnswer::Frame(got)) => assert_eq!(got.as_deref(), want),
                other => panic!("expected a frame, got {:?}", other.map(|_| ())),
            }
        }
        match serve_wasm_call(&mut res, ServeCall::Close) {
            Ok(ServeAnswer::Released) => {}
            other => panic!("expected a release, got {:?}", other.map(|_| ())),
        }
        match serve_wasm_call(&mut res, ServeCall::Handle(ask("/x"))) {
            Ok(ServeAnswer::Buffered(r)) => assert_eq!(r.body, "104"),
            other => panic!("expected a buffered answer, got {:?}", other.map(|_| ())),
        }

        // Printed, not asserted: the machine carries other gates.
        let n = 200;
        let mut wasm = std::time::Duration::ZERO;
        for _ in 0..3 {
            let clock = std::time::Instant::now();
            for _ in 0..n {
                serve_wasm_call(&mut res, ServeCall::Handle(ask("/x"))).expect("answer");
            }
            wasm += clock.elapsed();
        }
        eprintln!("one answer: {:?} through the doors", wasm / (3 * n));
        let _ = std::fs::remove_file(&file);
    }
}
