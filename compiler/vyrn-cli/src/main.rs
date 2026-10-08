//! `vyrn`, the Vyrn driver. `COMMANDS` lists the commands.
//!
//! `--deny-warnings` (or `VYRN_DENY_WARNINGS=1`) turns any load warning into a
//! failure. Without it, warnings go to stderr and change no exit code and no
//! byte of the program's output.
//!
//! The file argument is optional when a `vyrn.json` found by walking up from
//! the current directory declares a `"main"`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use vyrn_frontend::project::Expansions;
use vyrn_genwasm::engine;

use vyrn_codegen::toolchain::find_clang;

mod remote;
// In the library target because `vyrn-frontend`'s tests run their programs
// through it too.
use vyrn_cli::wasmrun;

/// Whether `--version` / `-V` names this program: only among the leading
/// options. After the subcommand or file it belongs to the program being run.
fn wants_version(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.starts_with('-'))
        .any(|a| a == "--version" || a == "-V")
}

/// The flags every command shares, each also set by an environment variable.
/// `real_main` takes them off the command line once; nothing else reads them
/// from the command line or the environment.
#[derive(Clone, Copy, Default)]
struct GlobalFlags {
    /// `--offline` or `VYRN_OFFLINE`: never touch the network; a lock or cache
    /// miss is an error.
    offline: bool,
    /// `--deny-warnings` or `VYRN_DENY_WARNINGS`: a load that produced warnings
    /// fails.
    deny_warnings: bool,
    /// `--native-target` or `VYRN_NATIVE_TARGET`; `None` defers to the
    /// manifest's `nativeTarget`.
    native_target: Option<NativeTarget>,
    /// `--profile` before the file.
    profile: bool,
}

impl GlobalFlags {
    /// Takes the global flags out of `args`, `args[1]` being the command.
    /// `--profile` counts only before the file, as `--version` does:
    /// `vyrn run app.vyrn --profile` is a flag for `app.vyrn`.
    ///
    /// # Errors
    ///
    /// A missing or unknown native target: printed, exit 2. It is validated
    /// here so a typo is one clear error, not a clang error.
    fn take(args: &mut Vec<String>) -> Result<GlobalFlags, ExitCode> {
        let mut take = |flag: &str| {
            let had = args.iter().any(|a| a == flag);
            args.retain(|a| a != flag);
            had
        };
        let mut flags = GlobalFlags {
            offline: take("--offline") || std::env::var("VYRN_OFFLINE").is_ok(),
            deny_warnings: take("--deny-warnings") || std::env::var("VYRN_DENY_WARNINGS").is_ok(),
            ..GlobalFlags::default()
        };
        let named = match args.iter().position(|a| a == "--native-target") {
            Some(i) => {
                let Some(v) = args.get(i + 1).cloned() else {
                    eprintln!(
                        "error: --native-target needs a value (one of: {})",
                        NativeTarget::names()
                    );
                    return Err(ExitCode::from(2));
                };
                args.drain(i..=i + 1);
                Some(("--native-target", v))
            }
            None => std::env::var("VYRN_NATIVE_TARGET")
                .ok()
                .map(|v| ("VYRN_NATIVE_TARGET", v)),
        };
        if let Some((from, v)) = named {
            let Some(t) = NativeTarget::parse(&v) else {
                eprintln!(
                    "error: unknown {from} `{v}` (expected one of: {})",
                    NativeTarget::names()
                );
                return Err(ExitCode::from(2));
            };
            flags.native_target = Some(t);
        }
        let head = args
            .iter()
            .skip(2)
            .position(|a| !a.starts_with('-'))
            .map_or(args.len(), |i| i + 2)
            .max(2.min(args.len()));
        let at = args
            .get(2.min(args.len())..head)
            .and_then(|h| h.iter().position(|a| a == "--profile"));
        // Removed once, so a program's own `--profile` further along survives.
        if let Some(i) = at {
            args.remove(i + 2);
            flags.profile = true;
        }
        Ok(flags)
    }
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

impl NativeTarget {
    /// Each target and its spelling in `--native-target` and `vyrn.json`'s
    /// `nativeTarget`.
    const ALL: [(NativeTarget, &'static str); 5] = [
        (NativeTarget::V1, "v1"),
        (NativeTarget::V2, "v2"),
        (NativeTarget::V3, "v3"),
        (NativeTarget::V4, "v4"),
        (NativeTarget::Native, "native"),
    ];

    fn parse(s: &str) -> Option<NativeTarget> {
        Self::ALL.iter().find(|t| t.1 == s).map(|t| t.0)
    }

    /// Every spelling, for a diagnostic.
    fn names() -> String {
        Self::ALL.map(|t| t.1).join(", ")
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

/// The compiler is allocation-bound; see `mimalloc` in `Cargo.toml`.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// A command's result. `Err` is a failure the command has already printed, so
/// `?` passes it up unchanged.
type Outcome = Result<ExitCode, ExitCode>;

fn real_main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    match dispatch(&mut args) {
        Ok(code) | Err(code) => code,
    }
}

fn dispatch(args: &mut Vec<String>) -> Outcome {
    let flags = GlobalFlags::take(args)?;
    // Off `run`, `--profile` reports the build phases, and `main` prints the
    // table. On `run` it reports the guest's operation count (`wasm_profile`).
    if flags.profile && args.get(1).map(String::as_str) != Some("run") {
        vyrn_frontend::prof::arm();
    }
    // Before the usage screen, which exits 2: a package manager reads that as
    // a broken install. The release workflow checks the tag against this line.
    if wants_version(args) {
        println!("vyrn {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    let Some(name) = args.get(1) else {
        eprintln!("{}", usage());
        return Err(ExitCode::from(2));
    };
    let Some(cmd) = COMMANDS.iter().find(|c| c.name == name) else {
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        eprintln!(
            "unknown command `{name}` (expected one of: {})",
            names.join(", ")
        );
        return Err(ExitCode::from(2));
    };
    (cmd.run)(&Call::parse(cmd, flags, &args[2..])?)
}

/// One `vyrn` command. The usage screen, the unknown-command sentence, the
/// argument parser and `docs/tooling.md`'s drift test read this row, so a
/// command or a flag is stated once.
struct Cmd {
    name: &'static str,
    pos: Pos,
    flags: &'static [Flag],
    /// The usage screen's gloss of the command.
    about: &'static str,
    run: fn(&Call) -> Outcome,
}

/// The positional arguments a command takes.
enum Pos {
    None,
    /// `[file.vyrn]`, else the manifest's `main`: [`Call::root`].
    File,
    /// `[file.vyrn]`, then the program's own arguments, verbatim. The file is
    /// the first argument or none, so a flag after it is the program's.
    Program,
    /// Exactly one, named for the usage screen.
    One(&'static str),
    /// At most one.
    Maybe(&'static str),
    /// Any number.
    Many(&'static str),
}

/// A flag: its spelling, the placeholder of its value (`None` for a switch)
/// and the usage screen's gloss (`""` when the command's gloss covers it).
struct Flag(&'static str, Option<&'static str>, &'static str);

/// The flags [`GlobalFlags::take`] reads, before or after the command.
const GLOBAL_FLAGS: &[Flag] = &[
    Flag("--offline", None, "never touch the network; a lock or cache miss is an error (also VYRN_OFFLINE=1)"),
    Flag("--deny-warnings", None, "a load warning fails the command (also VYRN_DENY_WARNINGS=1)"),
    Flag("--native-target", Some("target"), "the native build's microarchitecture, over vyrn.json's `nativeTarget` (also VYRN_NATIVE_TARGET)"),
    Flag("--profile", None, "where the time went, to stderr; it counts only before the file, so a program can take its own"),
];

const PORT: Flag = Flag("--port", Some("N"), "default 8080; 0 lets the OS pick");
const WORKERS: Flag = Flag(
    "--workers",
    Some("N"),
    "N instances; refused if `handle` reaches module state",
);

/// A [`Cmd`] row by position, so the table reads one command to a line.
const fn cmd(
    name: &'static str,
    pos: Pos,
    run: fn(&Call) -> Outcome,
    about: &'static str,
    flags: &'static [Flag],
) -> Cmd {
    Cmd {
        name,
        pos,
        flags,
        about,
        run,
    }
}

/// Every command, in the order of the usage screen and `docs/tooling.md`.
#[rustfmt::skip]
const COMMANDS: &[Cmd] = &[
    cmd("run", Pos::Program, run_cmd, "compiles and runs; trailing args reach the program's args()", &[]),
    cmd("check", Pos::File, check_cmd, "loads, checks and judges the program, runs every generator, and prints ok", &[]),
    cmd("fix", Pos::File, fix_cmd, "applies the `.copy()` a move diagnostic names, in the file given; every other fix on the menu is a decision and is refused", &[]),
    cmd("build", Pos::File, build, "a native executable: the module through wasm2c and clang (needs wabt and simde under tools/, or $VYRN_WASM2C and $VYRN_SIMDE)", &[
        Flag("-o", Some("out"), ""),
        Flag("--target", Some("wasm"), "writes the module itself, with no LLVM, clang or sysroot"),
    ]),
    cmd("test", Pos::File, test_cmd, "runs the root file's `test` blocks", &[Flag("--name", Some("substring"), "")]),
    cmd("bench", Pos::File, bench_cmd, "times the root file's `bench` blocks natively", &[
        Flag("--name", Some("substring"), ""),
        Flag("--check", None, "runs each once, compiled, with no timing; excludes --json and --compare"),
        Flag("--json", None, "the machine-readable report"),
        Flag("--compare", Some("baseline.json"), "fails on a bench slower than the baseline's by the threshold"),
        Flag("--threshold", Some("factor"), "the regression factor for --compare (default 1.5)"),
        Flag("--ungate", Some("file"), "bench names, one per line, whose regressions --compare reports and does not fail"),
    ]),
    cmd("serve", Pos::File, serve_cmd, "an HTTP host for `fn handle(req: Request) -> Response`", &[PORT, WORKERS]),
    cmd("dev", Pos::None, dev_cmd, "fullstack: builds vyrn.json's `client` to wasm, then serves its `server`, static files and the runtimes", &[PORT, WORKERS]),
    cmd("fmt", Pos::Many("file.vyrn"), fmt_cmd, "the canonical formatter; no files = the project's main and its local imports", &[
        Flag("--check", None, "writes nothing and lists the files that would change"),
        Flag("--from-json", Some("file.json"), "prints the JSON file as VON instead, headed by `import type`"),
        Flag("--as", Some("Type"), "the type --from-json names (default Config)"),
        Flag("--from", Some("module"), "the module --from-json imports it from (default ./config.vyrn)"),
    ]),
    cmd("doc", Pos::Maybe("file|dir"), doc_cmd, "Markdown API docs", &[
        Flag("-o", Some("dir"), "default docs/api/"),
        Flag("--std", None, "documents the std modules too, or the whole std library alone"),
        Flag("--verify", None, "writes nothing and fails on drift"),
    ]),
    cmd("why", Pos::One("file"), why_cmd, "a module's audience, the path segment that decided it, and every import chain that reaches it", &[
        Flag("--contract", None, "which module contract governs the file, and every export's status against it"),
        Flag("--cost", None, "per line: what it allocates, copies and grows, and the checks it keeps"),
        Flag("--capability", Some("capability"), "every import chain that pulls the capability into the artifact the file argument names"),
    ]),
    cmd("routes", Pos::File, routes_cmd, "the resolved wire table: every derived, pinned, hand-written and page path the router mounts, with its source", &[
        Flag("--json", None, "attaches each route's declaration from the symbol map"),
    ]),
    cmd("emit-wat", Pos::File, emit_wat, "the module `build --target wasm` writes, as WAT", &[]),
    cmd("emit-lowered", Pos::File, emit_lowered, "the named core the emitter reads, root module only", &[]),
    cmd("emit-gen", Pos::File, emit_gen, "the source of every generated module, each under a banner naming its call site", &[
        Flag("--maps", None, "each generated module's symbol map instead, one JSON document per line"),
    ]),
    cmd("new", Pos::One("name"), scaffold, "scaffolds vyrn.json, src/main.vyrn and .gitignore", &[]),
    cmd("add", Pos::One("github:|gist:|https: specifier"), add, "fetches and pins a remote module and adds it to `dependencies`", &[
        Flag("--name", Some("alias"), ""),
    ]),
    cmd("update", Pos::Maybe("alias|tool"), update, "re-resolves and re-pins the remote dependencies and toolchain tools", &[
        Flag("--locked", None, "reads through the existing pins and never writes the lock"),
    ]),
    cmd("vendor", Pos::None, vendor, "copies every locked blob into vyrn_vendor/", &[
        Flag("--check", None, "verifies each one is there and intact"),
    ]),
    cmd("deps", Pos::Maybe("artifact"), deps, "every declared artifact's module graph, then the toolchain", &[]),
];

impl Cmd {
    /// `vyrn <name> <positionals> <flags>`, for the usage screen.
    fn synopsis(&self) -> String {
        let mut line = format!("vyrn {}", self.name);
        match self.pos {
            Pos::None => {}
            Pos::File => line.push_str(" [file.vyrn]"),
            Pos::Program => line.push_str(" [file.vyrn] [args...]"),
            Pos::One(n) => line.push_str(&format!(" <{n}>")),
            Pos::Maybe(n) => line.push_str(&format!(" [{n}]")),
            Pos::Many(n) => line.push_str(&format!(" [{n} ...]")),
        }
        for f in self.flags {
            line.push_str(&f.synopsis());
        }
        line
    }
}

impl Flag {
    /// ` [--name <value>]`.
    fn synopsis(&self) -> String {
        match self.1 {
            Some(v) => format!(" [{} <{v}>]", self.0),
            None => format!(" [{}]", self.0),
        }
    }
}

/// The usage screen: every command with its gloss and its flags' glosses, then
/// the global flags.
fn usage() -> String {
    let mut out = String::from("usage: vyrn <command> [arguments] [global flags]\n");
    for c in COMMANDS {
        out.push_str(&format!("  {}\n      {}\n", c.synopsis(), c.about));
        for f in c.flags.iter().filter(|f| !f.2.is_empty()) {
            out.push_str(&format!("      {}: {}\n", f.0, f.2));
        }
    }
    out.push_str("global flags:\n");
    for f in GLOBAL_FLAGS {
        out.push_str(&format!("  {}\n      {}\n", f.synopsis().trim(), f.2));
    }
    out.push_str("  vyrn --version (also -V)");
    out
}

/// One command line, parsed against its [`Cmd`] row.
struct Call {
    cmd: &'static Cmd,
    flags: GlobalFlags,
    /// The file of a [`Pos::File`] or [`Pos::Program`] command, if named.
    file: Option<String>,
    /// The other positionals; a [`Pos::Program`]'s are the program's.
    pos: Vec<String>,
    /// Each flag given, with its value, in command-line order.
    given: Vec<(&'static str, Option<String>)>,
}

impl Call {
    /// Splits `rest` into the row's flags and positionals.
    ///
    /// # Errors
    ///
    /// An unknown flag, a flag with no value, or the wrong count of
    /// positionals: printed with the command's usage line, exit 2.
    fn parse(cmd: &'static Cmd, flags: GlobalFlags, rest: &[String]) -> Result<Call, ExitCode> {
        let mut call = Call {
            cmd,
            flags,
            file: None,
            pos: Vec::new(),
            given: Vec::new(),
        };
        let most = match cmd.pos {
            Pos::Program => {
                let named = rest.first().filter(|a| !a.starts_with('-'));
                call.file = named.cloned();
                call.pos = rest[named.map_or(0, |_| 1)..].to_vec();
                return Ok(call);
            }
            Pos::None => 0,
            Pos::File | Pos::One(_) | Pos::Maybe(_) => 1,
            Pos::Many(_) => usize::MAX,
        };
        let mut args = rest.iter();
        while let Some(a) = args.next() {
            match cmd.flags.iter().find(|f| f.0 == a) {
                Some(Flag(name, None, _)) => call.given.push((name, None)),
                Some(Flag(name, Some(what), _)) => match args.next() {
                    Some(v) => call.given.push((name, Some(v.clone()))),
                    None => return Err(call.refuse(&format!("{name} needs a value: <{what}>"))),
                },
                None if a.len() > 1 && a.starts_with('-') => {
                    return Err(call.refuse(&format!("unexpected argument `{a}`")))
                }
                None => call.pos.push(a.clone()),
            }
        }
        if let Some(extra) = call.pos.get(most) {
            return Err(call.refuse(&format!("unexpected argument `{extra}`")));
        }
        match cmd.pos {
            Pos::One(n) if call.pos.is_empty() => Err(call.refuse(&format!("missing <{n}>"))),
            Pos::File => {
                call.file = call.pos.pop();
                Ok(call)
            }
            _ => Ok(call),
        }
    }

    /// Prints `error: <why>` and the command's usage line; returns exit 2.
    fn refuse(&self, why: &str) -> ExitCode {
        eprintln!("error: {why}");
        eprintln!("usage: {}", self.cmd.synopsis());
        ExitCode::from(2)
    }

    /// Whether the switch `flag` was given.
    fn has(&self, flag: &str) -> bool {
        self.value_of(flag).is_some()
    }

    /// The value of `flag`; the last one when it was given twice.
    fn value(&self, flag: &str) -> Option<&str> {
        self.value_of(flag)?.as_deref()
    }

    /// The value of `flag` parsed as `T`, or `None` when it was not given.
    ///
    /// # Errors
    ///
    /// A value that does not parse: `<flag> needs <what>`, exit 2.
    fn parsed<T: std::str::FromStr>(&self, flag: &str, what: &str) -> Result<Option<T>, ExitCode> {
        self.value(flag)
            .map(|v| {
                v.parse()
                    .map_err(|_| self.refuse(&format!("{flag} needs {what}")))
            })
            .transpose()
    }

    /// `Some(value)` for a flag given; panics on a flag the row does not
    /// declare, so a misspelling fails the first test that reaches it.
    fn value_of(&self, flag: &str) -> Option<&Option<String>> {
        assert!(
            self.cmd.flags.iter().any(|f| f.0 == flag),
            "`vyrn {}` declares no flag {flag}",
            self.cmd.name
        );
        self.given.iter().rev().find(|g| g.0 == flag).map(|g| &g.1)
    }

    /// The only positional of a [`Pos::One`] or [`Pos::Maybe`] command.
    fn arg(&self) -> Option<&str> {
        self.pos.first().map(String::as_str)
    }

    /// The root file and its project: the file the command names, else the
    /// manifest's `main`.
    ///
    /// # Errors
    ///
    /// Neither exists: printed with the usage line, exit 2. The project's own
    /// errors as [`Project::of`].
    fn root(&self) -> Result<(Project, String), ExitCode> {
        let p = Project::of(self.file.as_deref(), self.flags)?;
        match self.file.clone().or_else(|| p.main()) {
            Some(path) => Ok((p, path)),
            None => Err(self.refuse("no input file, and no vyrn.json with a `main` found")),
        }
    }
}

/// `vyrn check [file]`. It must predict the one thing `build` can fail to
/// finish: unbounded monomorphization, visible only while emitting (audit
/// A5.2).
fn check_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    vyrn_frontend::movecheck::emit_nothing();
    let (program, world) = p.checked(&path, &read_source(&path)?)?;
    instantiable(&program, &world)?;
    println!("ok");
    Ok(ExitCode::SUCCESS)
}

/// What `check` refuses, `run` refuses, with `check`'s sentence: a polymorphic
/// recursion has no finite set of instances.
fn instantiable(
    program: &vyrn_frontend::ast::Program,
    world: &vyrn_lower::World,
) -> Result<(), ExitCode> {
    failed(vyrn_codegen::check_instantiations(program, world))
}

/// A pass's `Err` printed as `error: ..`, exit 1.
fn failed<T, E: std::fmt::Display>(r: Result<T, E>) -> Result<T, ExitCode> {
    r.map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    })
}

/// `vyrn emit-gen [file] [--maps]`: prints the source of every generated module
/// the file reaches, each under a banner naming its call site.
///
/// `--maps` prints each module's symbol map instead, one JSON document
/// per line with the banners on stderr, so `> api.map.json` writes the file.
/// `vyrn emit-wat [file]`: the module `build --target wasm` writes and
/// `build` hands wasm2c.
fn emit_wat(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let (program, world) = p.checked(&path, &read_source(&path)?)?;
    print!("{}", failed(vyrn_codegen::direct::wat(&program, world))?);
    Ok(ExitCode::SUCCESS)
}

/// `vyrn emit-lowered [file]`: the form the emitter reads, for the root module
/// only. A linked program's imports are another file's answer.
fn emit_lowered(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let (program, world) = p.checked(&path, &read_source(&path)?)?;
    print!("{}", vyrn_lower::render(&program, &world, &path));
    Ok(ExitCode::SUCCESS)
}

fn emit_gen(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let root_key = dos_to_slash(&path);
    let graph = p.graph(&path, &read_source(&path)?)?;
    let mods = generated(&graph);
    if mods.is_empty() {
        eprintln!("(no generator imports in {root_key})");
    }
    if call.has("--maps") {
        let mut any = false;
        for (banner, src) in mods {
            if let Some(json) = vyrn_frontend::symbolmap::json_of(src) {
                eprintln!("// ==== {banner} ====");
                println!("{json}");
                any = true;
            }
        }
        if !any {
            eprintln!("(no generated module in {root_key} carries a symbol map)");
        }
        return Ok(ExitCode::SUCCESS);
    }
    for (banner, src) in mods {
        println!("// ==== {banner} ====");
        print!("{src}");
        if !src.ends_with('\n') {
            println!();
        }
        println!();
    }
    Ok(ExitCode::SUCCESS)
}

/// The generated modules of a graph, as `(banner, source)` in load order.
fn generated(graph: &loader::ModuleGraph) -> Vec<(&str, &str)> {
    graph
        .iter()
        .filter_map(|(key, _, gen)| Some((key.as_str(), gen.as_deref()?)))
        .collect()
}

use vyrn_frontend::diagnostics::{Diagnostic, Fix};
use vyrn_frontend::loader;

use vyrn_frontend::manifest::{
    dos_to_slash, find as find_manifest, real_path, std_root, web_root, Manifest,
};

/// One command's project, read once: the nearest `vyrn.json` above the start
/// directory, the load options and the lock-aware resolver it implies, and the
/// global flags. Every load reports through [`Project::report`], so each one
/// saves the lock and prints its diagnostics and warnings the same way.
struct Project {
    manifest: Option<Manifest>,
    opts: loader::LoadOptions,
    /// Files, plus remotes through the lock, the cache and the network.
    resolver: remote::RemoteResolver,
    flags: GlobalFlags,
}

impl Project {
    /// The project that governs `file`'s directory, or the working directory's
    /// for `None` or a bare file name.
    ///
    /// # Errors
    ///
    /// A manifest or a lock that will not parse: printed, exit 2. A pin the
    /// compiler cannot read is not the absence of a pin, so nothing re-pins to
    /// whatever the network serves.
    fn of(file: Option<&str>, flags: GlobalFlags) -> Result<Project, ExitCode> {
        let dir = file
            .and_then(|f| Path::new(f).parent())
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Project::at(&dir, flags)
    }

    /// The project that governs `dir`. The lock sits beside the manifest, else
    /// in `dir`. Errors as [`Project::of`].
    fn at(dir: &Path, flags: GlobalFlags) -> Result<Project, ExitCode> {
        let fail = |e: String| {
            eprintln!("error: {e}");
            ExitCode::from(2)
        };
        let manifest = find_manifest(dir).map_err(fail)?;
        let mut opts = loader::LoadOptions {
            std_root: std_root(),
            ..Default::default()
        };
        let lock = match &manifest {
            Some(m) => {
                opts.aliases = m.dependencies.iter().cloned().collect();
                opts.alias_base = m.dir.clone();
                opts.audience = m.audience.clone();
                opts.artifacts = m.artifacts.clone();
                Path::new(&m.dir).join("vyrn.lock")
            }
            None => dir.join("vyrn.lock"),
        };
        let resolver = remote::RemoteResolver {
            lock: std::cell::RefCell::new(remote::Lock::load(lock).map_err(fail)?),
            project_dir: manifest.as_ref().map(|m| m.dir.clone()),
            offline: flags.offline,
        };
        Ok(Project {
            manifest,
            opts,
            resolver,
            flags,
        })
    }

    /// The manifest's `main`, resolved relative to the manifest's directory.
    fn main(&self) -> Option<String> {
        let m = self.manifest.as_ref()?;
        Some(format!("{}/{}", m.dir, m.main.as_ref()?))
    }

    /// Saves the pins added since the last save. A failed write is an error:
    /// an unpinned build is not reproducible.
    fn save_lock(&self) -> Result<(), ExitCode> {
        let mut lock = self.resolver.lock.borrow_mut();
        if lock.dirty {
            if let Err(e) = lock.save() {
                eprintln!("error: cannot write {}: {e}", lock.path.display());
                return Err(ExitCode::FAILURE);
            }
            lock.dirty = false;
            eprintln!("pinned new remote imports in {}", lock.path.display());
        }
        Ok(())
    }

    /// Saves the lock, then prints a load's diagnostics, or its warnings when
    /// it succeeded, to stderr before the command's own output. A diagnostic's
    /// file defaults to `root`.
    ///
    /// # Errors
    ///
    /// The load failed, or it warned under `--deny-warnings`: printed, exit 1.
    fn report<T>(
        &self,
        root: &str,
        (result, warnings): (Result<T, Vec<Diagnostic>>, loader::Warnings),
    ) -> Result<T, ExitCode> {
        // Pins are saved even when the load failed.
        self.save_lock()?;
        let root_key = dos_to_slash(root);
        let t = result.map_err(|diags| {
            print_diagnostics(&diags, &root_key, "");
            ExitCode::FAILURE
        })?;
        if !warnings.is_empty() {
            print_diagnostics(&warnings, &root_key, "warning: ");
            if self.flags.deny_warnings {
                eprintln!(
                    "error: {} warning(s) — refused by --deny-warnings",
                    warnings.len()
                );
                return Err(ExitCode::FAILURE);
            }
        }
        Ok(t)
    }

    /// Loads and checks `root`, with shared expansions so the command reads the
    /// load's World. Sound only because both walk the same nodes: `a[i]` and
    /// `for x in c` over a user container inline a projection at the access
    /// site, and side tables are keyed by node address.
    fn checked(&self, root: &str, source: &str) -> Result<Loaded, ExitCode> {
        let opts = loader::LoadOptions {
            expansions: Expansions::shared(),
            ..self.opts.clone()
        };
        let key = dos_to_slash(root);
        let loaded = vyrn_lower::load_warned(source, &key, &opts, &self.resolver, Some(&*engine()));
        self.report(root, loaded)
    }

    /// Every module `root` reaches, loaded and not checked.
    fn graph(&self, root: &str, source: &str) -> Result<loader::ModuleGraph, ExitCode> {
        let key = dos_to_slash(root);
        let graph =
            loader::module_graph(source, &key, &self.opts, &self.resolver, Some(&*engine()));
        self.report(root, graph)
    }

    /// The native target: the global flag, then the manifest's
    /// `nativeTarget`, then the default.
    ///
    /// # Errors
    ///
    /// A misspelled `nativeTarget`: printed, exit 2. It must not fall back to
    /// the default, or the binary is built for something the user did not
    /// write.
    fn native_target(&self) -> Result<NativeTarget, ExitCode> {
        let written =
            (self.manifest.as_ref()).and_then(|m| Some((&m.dir, m.native_target.as_ref()?)));
        match (self.flags.native_target, written) {
            (Some(t), _) => Ok(t),
            (None, None) => Ok(DEFAULT_NATIVE_TARGET),
            (None, Some((dir, v))) => NativeTarget::parse(v).ok_or_else(|| {
                eprintln!(
                    "error: unknown `nativeTarget` `{v}` in {dir}/vyrn.json (expected one of: {})",
                    NativeTarget::names()
                );
                ExitCode::from(2)
            }),
        }
    }
}

/// Reads a file a command names. Unreadable: printed, exit 2.
fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("error: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

/// `vyrn new <name>`: scaffolds vyrn.json, src/main.vyrn and .gitignore.
fn scaffold(call: &Call) -> Outcome {
    let name = call.arg().unwrap_or_default();
    // The name is interpolated raw into vyrn.json and src/main.vyrn; a quote,
    // a backslash or a control character would write a manifest no later
    // command can parse.
    if name.contains('"') || name.contains('\\') || name.chars().any(char::is_control) {
        eprintln!("error: project name cannot contain `\"`, `\\`, or control characters");
        return Err(ExitCode::FAILURE);
    }
    let root = Path::new(name);
    if root.exists() {
        eprintln!("error: `{name}` already exists");
        return Err(ExitCode::FAILURE);
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
                return Err(ExitCode::FAILURE);
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return Err(ExitCode::FAILURE);
        }
    }
    println!("created {name}/ (vyrn.json, src/main.vyrn) — try: cd {name} && vyrn run");
    Ok(ExitCode::SUCCESS)
}

/// `vyrn why`: dispatches to the audience, `--contract`, `--cost` or
/// `--capability` report. `--contract` prints the contract that governs a
/// module and the status of each of its members; it exits 1 when the file is
/// in no role.
fn why_cmd(call: &Call) -> Outcome {
    let (flags, file) = (call.flags, call.arg().unwrap_or_default());
    if let Some(cap) = call.value("--capability") {
        return why_capability(flags, cap, file);
    }
    if call.has("--cost") {
        return why_cost(flags, file);
    }
    if !call.has("--contract") {
        return why_audience(flags, file);
    }
    let path = match Path::new(&file).canonicalize() {
        Ok(p) => dos_to_slash(&p.to_string_lossy()),
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return Err(ExitCode::from(2));
        }
    };
    let p = Project::of(Some(&path), flags)?;

    // The editor's app root, so both name the same roles.
    let app_dir =
        vyrn_frontend::manifest::app_root(Path::new(path.rsplit_once('/').map_or(".", |(d, _)| d)));
    // The manifest already read is passed in, never re-read: two readers of one
    // file are two policies when one of them fails.
    let doc = p.manifest.as_ref().map(|m| &m.doc);
    let roots = vyrn_frontend::manifest::role_roots(&app_dir, doc);
    let roles = vyrn_frontend::contracts::roles_for_project(doc, &roots, &p.opts, &p.resolver);
    let Some(role) = vyrn_frontend::contracts::role_for(&path, &roles) else {
        p.save_lock()?;
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
        return Ok(ExitCode::FAILURE);
    };
    let manifest = dos_to_slash(&app_dir.join("vyrn.json").to_string_lossy());
    let view = vyrn_frontend::contracts::load_role_contract(role, &manifest, &p.opts, &p.resolver);
    p.save_lock()?;
    let Some(view) = view else {
        eprintln!(
            "error: cannot resolve contract `{}:{}`",
            role.module, role.contract
        );
        return Err(ExitCode::FAILURE);
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
    let parsed = |text: &str| {
        vyrn_frontend::lexer::lex(text)
            .ok()
            .map(|t| vyrn_frontend::parser::parse_accum(t).0)
    };
    let decl = vyrn_frontend::loader::ModuleResolver::read(&p.resolver, &view.file)
        .ok()
        .and_then(|s| parsed(&s))
        .and_then(|p| p.contracts.into_iter().find(|c| c.name == view.name));
    let (Some(mut decl), Some(module)) = (decl, parsed(&source)) else {
        eprintln!("error: cannot lex {path} or {}", view.file);
        return Err(ExitCode::FAILURE);
    };
    // The generator's `contractOf` names the module as its importer wrote it.
    decl.module = Some(view.module.clone());
    // A `.vyrn` module is reflected linked, as `moduleInterface` reflects it: a
    // page that does not compile has no interface to judge. A `.vyx` page is
    // reflected from its source, as `vyxPageInterface` reflects it.
    let interface = if path.ends_with(".vyx") {
        use vyrn_frontend::schema_reflect::{module_interface_lit, Origins};
        module_interface_lit(&module, &Default::default(), &Origins::new([]))
    } else {
        let importer_dir = path.rsplit_once('/').map_or(".", |(d, _)| d);
        let linked = vyrn_frontend::gen::linked_module_interface(
            Some(&*engine()),
            &p.resolver,
            &p.opts,
            importer_dir,
            &path,
            &path,
            &source,
            &mut Vec::new(),
        );
        match linked {
            Ok(lit) => lit,
            Err(diags) => {
                print_diagnostics(&diags, &path, "");
                return Err(ExitCode::FAILURE);
            }
        }
    };
    let verdict = match contract_verdict(&p, &decl, interface) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: std/contract cannot judge {path}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };

    println!("{path}");
    println!("  role: {}", role.scope);
    println!("  contract: {} ({})", view.name, view.module);
    println!("  declared in: {}", view.file);
    let mut objections = 0;
    // `checkContract` reports at most one issue per name: a member's, or that
    // of an export the contract does not name.
    let mut objection = |name: &str| {
        let (key, _, message) = verdict.issues.iter().find(|(_, at, _)| at == name)?;
        objections += 1;
        let label = match key.as_str() {
            "contract.missing" => "MISSING ",
            "contract.type" | "contract.open" => "MISMATCH",
            "contract.unknown" | "contract.unknown.didYouMean" => "UNKNOWN ",
            other => other,
        };
        Some(format!("{label}  {name}: {message}"))
    };
    // The exports `moduleInterface` reflects, in source order.
    let exports: Vec<&str> = module
        .functions
        .iter()
        .filter(|f| f.exported && !f.is_extern)
        .map(|f| f.name.as_str())
        .collect();
    for m in &view.members {
        let want = m
            .shapes
            .iter()
            .map(|s| s.spelling.as_str())
            .collect::<Vec<_>>()
            .join(" or ");
        let shape = verdict
            .matched
            .iter()
            .find(|(n, _)| *n == m.name)
            .map_or(-1, |(_, i)| *i);
        let line = if shape >= 0 {
            format!(
                "ok        {}: shape {} of {} — {want}",
                m.name,
                shape + 1,
                m.shapes.len()
            )
        } else if synthesized.contains(&m.name) && !exports.contains(&m.name.as_str()) {
            // The file's form writes it: a `.vyx` has no other way to declare
            // a view.
            format!(
                "ok        {}: the `<template>` compiles to it — {want}",
                m.name
            )
        } else if let Some(line) = objection(&m.name) {
            line
        } else {
            format!("default   {}: absent, optional — {want}", m.name)
        };
        println!("  {line}");
    }
    for name in exports {
        if view.member(name).is_some() {
            continue;
        }
        let line = match (objection(name), &view.open_rule) {
            (Some(line), _) => line,
            (None, Some(rule)) => format!(
                "ok        {name}: matches the open rule — {}",
                rule.spelling
            ),
            (None, None) => format!("ok        {name}: std/contract raises no issue"),
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
    Ok(ExitCode::SUCCESS)
}

/// What `std/contract` says of one module: each member's matched shape
/// (`matchedMember`, -1 for none) in declaration order, and every
/// `checkContract` issue as `(key, path, message)`.
struct ContractVerdict {
    matched: Vec<(String, i64)>,
    issues: Vec<(String, String, String)>,
}

/// The program [`contract_verdict`] compiles. Each stub's body is replaced by a
/// reflection literal before the compile.
const WHY_CONTRACT_SRC: &str = r#"import { checkContract, matchedMember } from "std/contract"

fn whyContract() -> ContractInfo {
    return whyContract()
}

fn whyModule() -> ModuleInterface {
    return whyModule()
}

fn main() -> Int64 {
    let c = whyContract()
    let m = whyModule()
    for x in c.members {
        print("\{x.name}\t\{matchedMember(m, c, x.name)}")
    }
    for i in checkContract(m, c) {
        print("\{i.key}\t\{i.path}\t\{i.message}")
    }
    return 0
}
"#;

/// Asks `std/contract` what a generator asks of `module`, so `why --contract`
/// states no matching rule of its own. `decl` is reflected as `contractOf`
/// reflects it, and `interface` is the `moduleInterface` literal of the module.
fn contract_verdict(
    p: &Project,
    decl: &vyrn_frontend::ast::ContractDecl,
    interface: vyrn_frontend::ast::Expr,
) -> Result<ContractVerdict, String> {
    use vyrn_frontend::ast::{Block, Id, Stmt};
    use vyrn_frontend::schema_reflect::contract_info_lit;
    let mut interface = Some(interface);
    let opts = loader::LoadOptions {
        expansions: Expansions::shared(),
        ..p.opts.clone()
    };
    let mut prog = vyrn_lower::load(
        WHY_CONTRACT_SRC,
        "why-contract.vyrn",
        &opts,
        &p.resolver,
        None,
    )
    .map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default())?;
    for f in &mut prog.functions {
        let lit = match f.name.as_str() {
            "whyContract" => contract_info_lit(decl),
            "whyModule" => interface.take().expect("one whyModule"),
            _ => continue,
        };
        f.body = Block {
            id: Id::NEW,
            stmts: vec![Stmt::ret(lit, 0)],
        };
    }
    prog.number();
    let bytes = vyrn_codegen::direct::compile(&prog, vyrn_lower::analyze(&prog))?;
    let out = wasmrun::run(
        &bytes,
        wasmrun::Run {
            capture_stdout: true,
            capture_stderr: true,
            ..Default::default()
        },
    )?;
    if out.code != 0 {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let mut verdict = ContractVerdict {
        matched: Vec::new(),
        issues: Vec::new(),
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.split('\t').collect::<Vec<_>>()[..] {
            [name, shape] => verdict.matched.push((
                name.to_string(),
                shape.parse().map_err(|_| line.to_string())?,
            )),
            [key, at, message] => {
                verdict
                    .issues
                    .push((key.to_string(), at.to_string(), message.to_string()))
            }
            _ => return Err(format!("unexpected row `{line}`")),
        }
    }
    Ok(verdict)
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
fn routes_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let root_key = dos_to_slash(&path);
    let source = read_source(&root_key)?;
    let graph = p.graph(&root_key, &source)?;
    let mods = generated(&graph);
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
    let opts = loader::LoadOptions {
        expansions: Expansions::shared(),
        ..p.opts.clone()
    };
    match vyrn_lower::load(&source, &root_key, &opts, &p.resolver, Some(&*engine()))
        .map_err(|d| d.first().map(|d| d.message.clone()).unwrap_or_default())
        .and_then(|p| mounted_routes_wasm(&root_key, &p))
    {
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
    if call.has("--json") {
        return Ok(routes_json(&mods, rows));
    }
    if rows.is_empty() {
        println!("(no derived routes in {root_key})");
        return Ok(ExitCode::SUCCESS);
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
    Ok(ExitCode::SUCCESS)
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
) -> Result<Vec<(String, String, String)>, String> {
    use vyrn_frontend::ast::{Block, Expr, Id, Stmt, Type};
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
            Stmt::expr(Expr::call(
                "print",
                vec![Expr::call("mountedRows", args, 0)],
                0,
            ))
        })
        .collect();
    stmts.push(Stmt::ret(Expr::int(0), 0));
    prog.functions.push(synth_fn(
        "main".to_string(),
        Block { id: Id::NEW, stmts },
        Type::Int,
        0,
        false,
    ));
    prog.number();
    let bytes = vyrn_codegen::direct::compile(&prog, vyrn_lower::analyze(&prog))?;
    let out = wasmrun::run(
        &bytes,
        wasmrun::Run {
            argv: vec![path.to_string()],
            capture_stdout: true,
            capture_stderr: true,
            ..Default::default()
        },
    )?;
    if out.code != 0 {
        let text = String::from_utf8_lossy(&out.stderr);
        return Err(match vyrn_frontend::trap::split(&text).1 {
            Some(msg) => msg.to_string(),
            None => format!("the mounted router exited {}", out.code),
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
    mods: &[(&str, &str)],
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

/// `vyrn why --cost <file>`: per function of the file, the lines that allocate,
/// copy, grow a container, enter an allocating function of another file or
/// keep a check, and how many loops enclose each. It prints
/// `insight::root` and decides nothing. Exit 0 whenever it could answer.
fn why_cost(flags: GlobalFlags, file: &str) -> Outcome {
    use std::collections::BTreeMap;
    use vyrn_frontend::core::Copied;
    use vyrn_lower::insight::{self, Kind};
    let path = match Path::new(file).canonicalize() {
        Ok(p) => dos_to_slash(&p.to_string_lossy()),
        Err(e) => {
            eprintln!("error: cannot read {file}: {e}");
            return Err(ExitCode::from(2));
        }
    };
    let raw = read_source(&path)?;
    // A `.vyx`'s module is its `<script>`.
    let source = if path.ends_with(".vyx") {
        vyx_script_body(&raw).unwrap_or_default()
    } else {
        raw
    };
    let p = Project::of(Some(&path), flags)?;
    let (program, world) = p.checked(&path, &source)?;

    println!("{}", dos_to_slash(file));
    let (std, here) = (std_root(), path.rsplit_once('/').map_or("", |(d, _)| d));
    // The file a call enters, as an import spells it: `std/strings`.
    let named = |file: &str| match std.as_deref().and_then(|s| file.strip_prefix(s)) {
        Some(m) => format!("std{}", m.trim_end_matches(".vyrn")),
        None => rel_to(file, here),
    };
    // `(rows, rows in loops)` of allocating, growing, copying, kept and proved.
    let mut tally = [(0usize, 0usize); 5];
    for (id, facts) in insight::root(&program, &world) {
        // Keyed by line, then verb in the order a reader asks: what is copied,
        // what is allocated, what grows, what is checked.
        let mut shown: BTreeMap<(u32, u8), (&str, u32, Vec<(String, usize)>)> = BTreeMap::new();
        for f in &facts {
            let (slot, order, verb, what) = match &f.kind {
                Kind::Copy { what, implicit } => {
                    let what = match what {
                        Copied::Value => "a value",
                        Copied::Render => "a String render",
                    };
                    let how = if *implicit { " (implicit)" } else { "" };
                    (2, 0, "copies", format!("{what}{how}"))
                }
                Kind::Alloc(w) => (0, 1, "allocates", w.clone()),
                Kind::Enters(c, from) => (0, 2, "enters", format!("{c}(..) in {}", named(from))),
                Kind::Grows(b) => (1, 3, "grows", format!("{b}(..)")),
                Kind::Check { raises, kept } => (
                    3 + usize::from(!kept),
                    4,
                    "check kept",
                    raises.census().to_string(),
                ),
            };
            tally[slot].0 += 1;
            tally[slot].1 += usize::from(f.depth > 0);
            if slot == 4 {
                continue;
            }
            let (_, depth, whats) = shown
                .entry((f.line, order))
                .or_insert((verb, 0, Vec::new()));
            *depth = (*depth).max(f.depth);
            match whats.iter_mut().find(|w| w.0 == what) {
                Some(w) => w.1 += 1,
                None => whats.push((what, 1)),
            }
        }
        if shown.is_empty() {
            continue;
        }
        let f = &program.functions[id.index()];
        let mut tys: Vec<&vyrn_frontend::ast::Type> = f.params.iter().map(|p| &p.ty).collect();
        tys.push(&f.ret);
        let sp = program.spellings.speech(&None).sentence(&tys, &[&f.name]);
        let params: Vec<String> = (f.params.iter())
            .map(|p| format!("{}: {}", p.name, sp.ty(&p.ty)))
            .collect();
        println!(
            "fn {}({}) -> {}",
            sp.name(&f.name),
            params.join(", "),
            sp.ty(&f.ret)
        );
        let mut last = 0;
        for ((line, _), (verb, depth, whats)) in shown {
            let num = if line == last {
                String::new()
            } else {
                line.to_string()
            };
            let lp = if depth > 0 {
                format!("loop {depth}")
            } else {
                String::new()
            };
            last = line;
            let whats: Vec<String> = (whats.iter())
                .map(|(w, n)| {
                    if *n > 1 {
                        format!("{w} x{n}")
                    } else {
                        w.clone()
                    }
                })
                .collect();
            println!("{num:>5}  {lp:<7}  {verb:<10} {}", whats.join(", "));
        }
    }
    let [alloc, grow, copy, kept, proved] = tally;
    let count = |(all, lp): (usize, usize), what: &str| format!("{what} {all} ({lp} in loops)");
    println!(
        "summary: {}, {}, {}, {}, {}",
        count(alloc, "allocating rows"),
        count(grow, "growing rows"),
        count(copy, "copies"),
        count(kept, "checks kept"),
        count(proved, "checks proved"),
    );
    Ok(ExitCode::SUCCESS)
}

/// `vyrn why <file>`: the audience of a module, the path segment that decided
/// it, and every import chain that reaches it. Exit 0 whenever it could answer,
/// 2 only when the file cannot be read.
fn why_audience(flags: GlobalFlags, file: &str) -> Outcome {
    let Some(path) = real_path(file) else {
        eprintln!("error: cannot read {file}");
        return Err(ExitCode::from(2));
    };
    let p = Project::of(Some(&path), flags)?;
    let app_slash = match &p.manifest {
        Some(m) => m.dir.clone(),
        None => path.rsplit_once('/').map_or(".", |(d, _)| d).to_string(),
    };
    let map = p.manifest.as_ref().and_then(|m| m.audience.as_ref());

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
            let v = vyrn_frontend::audience::audience_of(&path, map, None);
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
    let edges = project_imports(Path::new(&app_slash), &p.opts);
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
    Ok(ExitCode::SUCCESS)
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
fn why_capability(flags: GlobalFlags, cap: &str, name: &str) -> Outcome {
    use vyrn_frontend::floor::{self, Capability};
    let Some(cap) = Capability::parse(cap) else {
        eprintln!(
            "error: unknown capability `{cap}` (expected one of: {})",
            floor::CAPABILITIES
        );
        return Err(ExitCode::from(2));
    };
    // An entry's path or an artifact's name. File identity first, so two
    // spellings of one file name one artifact.
    let path = real_path(name);
    let p = Project::of(path.as_deref(), flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: no vyrn.json found upward from `{name}`");
        return Err(ExitCode::from(2));
    };
    let Some(map) = manifest.artifacts.as_ref() else {
        eprintln!(
            "error: {}/vyrn.json declares no artifacts, so nothing in this project has a target",
            manifest.dir
        );
        return Err(ExitCode::from(2));
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
        return Err(ExitCode::from(2));
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

    let opts = loader::LoadOptions {
        audience: None,
        artifacts: None,
        ..p.opts.clone()
    };
    let source = read_source(&artifact.entry)?;
    let graph = loader::capability_graph(
        &source,
        &artifact.entry,
        &opts,
        &p.resolver,
        Some(&*engine()),
    );
    p.save_lock()?;
    let (graph, root_key) = match graph {
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
            return Err(ExitCode::from(2));
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
    Ok(ExitCode::SUCCESS)
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

/// Every file under `dir` with one of the extensions `exts`, as sorted slash
/// paths. Hidden directories, build output and vendored trees are skipped:
/// they are not the project.
fn files_under(dir: &Path, exts: &[&str]) -> Vec<String> {
    fn walk(dir: &Path, exts: &[&str], out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let (p, name) = (e.path(), e.file_name());
            let name = name.to_string_lossy();
            if p.is_dir() {
                if !(name.starts_with('.')
                    || ["target", "vendor", "node_modules"].contains(&&*name))
                {
                    walk(&p, exts, out);
                }
            } else if p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)) {
                out.push(dos_to_slash(&p.to_string_lossy()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, exts, &mut out);
    out.sort();
    out
}

/// Every `importer -> imported` edge in a project, resolved with the loader's
/// own `resolve_spec`.
///
/// A generator import contributes edges too. A call naming one file is
/// resolved by `audience::generator_input`, the function that decides that
/// module's audience; a call naming a directory reaches every source under it.
fn project_imports(app_dir: &Path, opts: &loader::LoadOptions) -> Vec<(String, String)> {
    let files: Vec<(String, String)> = files_under(app_dir, &["vyrn", "vyx"])
        .into_iter()
        .filter_map(|path| Some((path.clone(), std::fs::read_to_string(&path).ok()?)))
        .collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for (path, source) in &files {
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
                    Some(Expr::Str(s, _)) => {
                        if let Some(input) = vyrn_frontend::audience::generator_input(path, s) {
                            out.push((path.clone(), input));
                            continue;
                        }
                        s.clone()
                    }
                    _ => continue,
                },
            };
            let Ok(resolved) = vyrn_frontend::loader::resolve_spec(&spec, path, opts) else {
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

/// [`dos_to_slash`], from a `Path`.
fn show_path(p: &Path) -> String {
    dos_to_slash(&p.to_string_lossy())
}

/// The `toolchain:` section of `vyrn deps`: one row per tool, with the path
/// that would be used, its version, and why that path was chosen.
///
/// Nothing here touches the network: a pin resolves through vendor and the
/// content-addressed cache, and an unresolved one prints as unresolved.
fn print_toolchain(start: &Path, pins: &[(String, String)]) {
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
fn deps(call: &Call) -> Outcome {
    let name = call.arg();
    let p = Project::of(None, call.flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: no vyrn.json found upward from here");
        return Err(ExitCode::FAILURE);
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
                return Err(ExitCode::from(2));
            }
        },
        (Some(map), None) => map.list.iter().collect(),
        (None, Some(want)) => {
            eprintln!(
                "error: {}/vyrn.json declares no artifacts, so it declares no `{want}`",
                manifest.dir
            );
            return Err(ExitCode::from(2));
        }
        (None, None) => Vec::new(),
    };
    if list.is_empty() {
        println!(
            "{}/vyrn.json declares no artifacts, so there is no module graph to report",
            manifest.dir
        );
        print_toolchain(&dir, &manifest.toolchain);
        return Ok(ExitCode::SUCCESS);
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
        let graph = read_source(&artifact.entry).and_then(|s| p.graph(&artifact.entry, &s));
        let Ok(graph) = graph else {
            failed = true;
            continue;
        };
        for (module, imports, _) in graph {
            println!("{module}");
            for i in imports {
                println!("  -> {i}");
            }
        }
    }
    print_toolchain(&dir, &manifest.toolchain);
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `vyrn fmt [file ...] [--check]`: formats each file in place, or with no
/// files the project `main` and its local imports. `--check` writes nothing,
/// lists the files that would change, and exits 1 if any would.
///
/// The input need only lex. A file that does not is reported and left
/// untouched; the others still format, and the exit is non-zero.
fn fmt_cmd(call: &Call) -> Outcome {
    let flags = call.flags;
    // A converter, not a formatter run: it prints and writes nothing.
    if let Some(path) = call.value("--from-json") {
        return from_json_cmd(
            flags,
            path,
            call.value("--as").unwrap_or("Config"),
            call.value("--from").unwrap_or("./config.vyrn"),
        );
    }
    let check = call.has("--check");
    let files = call.pos.clone();

    let mut had_error = false;
    // No files: the project's `main` and its local imports. Remote modules are
    // pinned and generated ones have no file, so neither is formatted. A load
    // that fails formats `main` alone and fails the command.
    let mut targets = files;
    if targets.is_empty() {
        let p = Project::of(None, flags)?;
        if let Some(main) = p.main() {
            targets = match p.graph(&main, &read_source(&main)?) {
                Ok(graph) => graph
                    .into_iter()
                    .filter(|(key, _, gen)| gen.is_none() && !loader::is_remote(key))
                    .map(|(key, _, _)| key)
                    .collect(),
                Err(_) => {
                    had_error = true;
                    vec![dos_to_slash(&main)]
                }
            };
        }
    }
    if targets.is_empty() {
        return Err(call.refuse("no input files, and no vyrn.json with a `main` found"));
    }

    let mut would_change: Vec<String> = Vec::new();
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
            return Err(ExitCode::FAILURE);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if written > 0 {
        println!(
            "formatted {written} file{}",
            if written == 1 { "" } else { "s" }
        );
    } else if !had_error {
        println!("already formatted");
    }
    Ok(if had_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
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
fn from_json_cmd(flags: GlobalFlags, path: &str, type_name: &str, module: &str) -> Outcome {
    let json = read_source(path)?;
    // A key beside the input file, so `std/` resolves as it would there. No
    // file is read at it.
    let norm = dos_to_slash(path);
    let key = match norm.rfind('/') {
        Some(i) => format!("{}/from-json.vyrn", &norm[..i]),
        None => "from-json.vyrn".to_string(),
    };
    let (program, world) = Project::of(Some(&key), flags)?.checked(&key, FROM_JSON_SRC)?;
    // Stderr is captured because the wording below rewrites it.
    let bytes = failed(vyrn_codegen::direct::compile(&program, world))?;
    let run = wasmrun::Run {
        argv: vec![key.clone(), json, type_name.to_string(), module.to_string()],
        capture_stderr: true,
        ..Default::default()
    };
    let out = failed(wasmrun::run(&bytes, run))?;
    if out.code == 0 {
        return Ok(ExitCode::SUCCESS);
    }
    // The trap names a position in the converter, which the user cannot open;
    // the input file's name replaces it.
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let msg = vyrn_frontend::trap::split(&text)
        .1
        .unwrap_or(text.trim_end());
    let msg = msg
        .split_once(" (from-json.vyrn:")
        .map(|(m, _)| m)
        .unwrap_or(msg);
    eprintln!("error: {path}: {msg}");
    Ok(ExitCode::from((out.code & 0xff) as u8))
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
fn doc_cmd(call: &Call) -> Outcome {
    let out_dir = call.value("-o").unwrap_or("docs/api");
    let modules = discover_doc_modules(call, call.arg(), call.has("--std"))?;
    if modules.is_empty() {
        eprintln!("error: no modules to document");
        return Err(ExitCode::from(2));
    }

    let mut files: Vec<(String, String)> = Vec::new();
    files.push(("index.md".to_string(), render_doc_index(&modules)));
    for m in &modules {
        let doc = vyrn_frontend::module_doc(&m.source);
        files.push((format!("{}.md", m.name), render_doc_page(&m.name, &doc)));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(if call.has("--verify") {
        verify_doc_dir(out_dir, &files)
    } else {
        write_doc_dir(out_dir, &files)
    })
}

/// The modules to document:
/// - a file: its local-import closure (`--std` adds the std modules it reaches);
/// - a directory: every `.vyrn` under it, named relative to it;
/// - nothing, with a manifest `main`: that file's closure;
/// - nothing, with `--std`: the whole std library.
fn discover_doc_modules(
    call: &Call,
    target: Option<&str>,
    with_std: bool,
) -> Result<Vec<DocModule>, ExitCode> {
    if let Some(dir) = target.filter(|t| Path::new(t).is_dir()) {
        return scan_doc_dir(dir, "");
    }
    let p = Project::of(target, call.flags)?;
    if let Some(root) = target.map(str::to_string).or_else(|| p.main()) {
        return closure_doc_modules(&p, &root, with_std);
    }
    if !with_std {
        return Err(call.refuse("no input file or directory, and no vyrn.json with a `main` found"));
    }
    match std_root() {
        Some(root) => scan_doc_dir(&root, "std/"),
        None => {
            eprintln!("error: --std given but no std library found (set VYRN_STD)");
            Err(ExitCode::FAILURE)
        }
    }
}

/// Every `.vyrn` file under `dir`, named `<prefix>` plus its path relative to
/// `dir` without the extension. Sorted by name.
fn scan_doc_dir(dir: &str, prefix: &str) -> Result<Vec<DocModule>, ExitCode> {
    let base = dos_to_slash(dir);
    let mut out = Vec::new();
    for p in files_under(Path::new(dir), &["vyrn"]) {
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

/// Every local module `root_file` reaches, named relative to the project.
/// `with_std` adds the std modules reached, as `std/<rel>`. Remote and
/// generated modules are never documented.
fn closure_doc_modules(
    p: &Project,
    root_file: &str,
    with_std: bool,
) -> Result<Vec<DocModule>, ExitCode> {
    let source = read_source(root_file)?;
    let root_key = dos_to_slash(root_file);
    let std_root = p.opts.std_root.as_deref().map(dos_to_slash);
    // Local module names are relative to the manifest's directory, else the
    // root file's.
    let base = match &p.manifest {
        Some(m) => m.dir.clone(),
        None => root_key
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default(),
    };
    let graph = p.graph(&root_key, &source)?;

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

/// The module name: `path` relative to `base`, without `.vyrn`. The file stem
/// when `path` is not under `base`.
fn rel_name(path: &str, base: &str) -> String {
    let path = dos_to_slash(path);
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
            // LF, so a CRLF checkout of a generated doc is not drift.
            Ok(on_disk) if on_disk.replace("\r\n", "\n") == *content => {}
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

/// Every `.md` file under `dir`, as `/`-separated paths relative to `dir`.
fn existing_md_files(dir: &str) -> Vec<String> {
    let base = format!("{}/", dos_to_slash(dir).trim_end_matches('/'));
    files_under(Path::new(dir), &["md"])
        .into_iter()
        .map(|f| f.strip_prefix(&base).map_or(f.clone(), str::to_string))
        .collect()
}

/// `vyrn fix [file]`: applies the `.copy()` a diagnostic carries as a
/// [`Fix`] and reports every other diagnostic in the file, because `consume`
/// and `for x in consume xs` are decisions, not edits.
///
/// It edits only the file given; a diagnostic in an import is reported. A round
/// is kept only if the diagnostic count falls, so the file never compiles worse
/// than it did.
fn fix_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let source = read_source(&path)?;
    let root_key = dos_to_slash(&path);
    let mut text = source.to_string();
    let mut rounds = 0usize;
    let mut applied: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let note = |refused: &mut Vec<String>, n: String| {
        if !refused.contains(&n) {
            refused.push(n);
        }
    };

    loop {
        let diags = fix_diagnostics(&p, &root_key, &text);
        let mut edits: Vec<Fix> = Vec::new();
        let mut elsewhere: Vec<String> = Vec::new();
        for d in &diags {
            let first = d.message.lines().next().unwrap_or_default();
            match (&d.file, d.fixes.as_slice()) {
                (Some(f), _) => elsewhere.push(format!("{f}:{}: {first} (another file)", d.line)),
                (None, []) => note(&mut refused, format!("{root_key}:{}: {first}", d.line)),
                (None, fixes) => edits.extend_from_slice(fixes),
            }
        }
        if edits.is_empty() {
            for n in elsewhere {
                note(&mut refused, n);
            }
            break;
        }
        edits.sort_unstable_by_key(|f| f.edit());
        edits.dedup();
        let next = match apply_edits(&text, &edits) {
            Ok(t) => t,
            Err(why) => {
                note(&mut refused, format!("{root_key}: {why}"));
                break;
            }
        };
        // A round that does not reduce the count is discarded whole.
        if fix_diagnostics(&p, &root_key, &next).len() >= diags.len() {
            refused.push(format!(
                "{root_key}: {} edit(s) rolled back — they did not reduce the diagnostics",
                edits.len()
            ));
            break;
        }
        text = next;
        applied.extend(edits.iter().map(|f| {
            let (line, col, del, _) = f.edit();
            let what = if del == 0 {
                "`.copy()` inserted"
            } else {
                "`consume` removed"
            };
            format!("{root_key}:{line}:{col}: {what}")
        }));
        rounds += 1;
        // Every round reduces the count; the bound stops a file with hundreds
        // of sites, which can run again.
        if rounds >= 100 {
            break;
        }
    }

    p.save_lock()?;
    if text != source {
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("error: cannot write {path}: {e}");
            return Err(ExitCode::FAILURE);
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
    Ok(ExitCode::SUCCESS)
}

/// Loads `text` as `root_key` and returns every diagnostic, printing nothing.
/// The checker's and the kernel's ownership refusals arrive as one list.
fn fix_diagnostics(p: &Project, root_key: &str, text: &str) -> Vec<Diagnostic> {
    match vyrn_lower::load_warned(text, root_key, &p.opts, &p.resolver, Some(&*engine())).0 {
        Ok(_) => Vec::new(),
        Err(d) => d,
    }
}

/// Applies each of `at`, sorted ascending by position and distinct, a column
/// counting characters. The edits run from the end, so none moves another's
/// position.
fn apply_edits(text: &str, at: &[Fix]) -> Result<String, String> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut out = text.to_string();
    for f in at.iter().rev() {
        let (line, col, del, with) = f.edit();
        let start = *starts
            .get(line.wrapping_sub(1))
            .ok_or_else(|| format!("no line {line}"))?;
        let end = starts.get(line).map_or(text.len(), |e| e - 1);
        let l = &text[start..end];
        // The byte offset of the character `n` places in, `l.len()` for one past the end.
        let offset = |n: usize| {
            let mut at = l.char_indices().map(|(i, _)| i).chain([l.len()]);
            at.nth(n)
                .ok_or_else(|| format!("line {line} has no column {}", n + 1))
        };
        let from = offset(col.wrapping_sub(1))?;
        let to = offset(col.wrapping_sub(1) + del)?;
        out.replace_range(start + from..start + to, with);
    }
    Ok(out)
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
        body,
        line,
        is_export_extern: door,
        ..vyrn_frontend::ast::Function::synth(name, Vec::new(), ret, Vec::new())
    }
}

/// A checked program and the World its check judged.
type Loaded = (
    vyrn_frontend::ast::Program,
    std::sync::Arc<vyrn_lower::World>,
);

/// Prints `file:line:col: message` per diagnostic, the file defaulting to
/// `root_key`, with its note below. `marker` is `""` for an error and
/// `"warning: "` for a warning.
fn print_diagnostics(diags: &[Diagnostic], root_key: &str, marker: &str) {
    for d in diags {
        let file = d.file.as_deref().unwrap_or(root_key);
        eprintln!("{}:{}:{}: {}{}", file, d.line, d.col, marker, d.message);
        if let Some(note) = &d.note {
            eprintln!("  note: {note}");
        }
    }
}

/// `vyrn add <specifier> [--name alias]`: fetches and pins a remote module and
/// records it in vyrn.json's dependencies.
fn add(call: &Call) -> Outcome {
    let spec = call.arg().unwrap_or_default();
    let spec = if spec.ends_with(".vyrn") || spec.ends_with(".json") {
        spec.to_string()
    } else {
        format!("{spec}.vyrn")
    };
    if !vyrn_frontend::loader::is_remote(&spec) {
        eprintln!("error: `add` takes a remote specifier (github:/gist:/https:)");
        return Err(ExitCode::FAILURE);
    }
    let alias = match call.value("--name") {
        Some(a) => a.to_string(),
        None => Path::new(&spec)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dep".to_string()),
    };

    let p = Project::of(None, call.flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: no vyrn.json found — run `vyrn new` or create one first");
        return Err(ExitCode::FAILURE);
    };

    // Fetch here, so a typo fails at once and the next build can run offline.
    failed(vyrn_frontend::loader::ModuleResolver::read(
        &p.resolver,
        &spec,
    ))?;
    p.save_lock()?;

    // Rewrites the document already read; key order stays stable.
    let manifest_path = Path::new(&manifest.dir).join("vyrn.json");
    use vyrn_frontend::schema::Json;
    let mut fields = match &manifest.doc {
        Json::Obj(f) => f.clone(),
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
        return Err(ExitCode::FAILURE);
    }
    println!("added `{alias}` -> {spec}");
    Ok(ExitCode::SUCCESS)
}

/// Fetches every platform's published artifact of one pinned tool into the
/// cache and records each in the lock as `tool:<name>@<version>/<platform>`.
///
/// Every platform, so a networked machine records the hashes for one that is
/// not. A platform with no upstream artifact is reported and skipped.
fn update_tool(
    name: &str,
    version: &str,
    lock: &mut remote::Lock,
    offline: bool,
) -> Result<(), String> {
    use vyrn_frontend::toolpin;
    // Before the retain below drops the old pins.
    if offline {
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
    offline: bool,
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
    if offline {
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
fn update(call: &Call) -> Outcome {
    let (flags, alias, locked) = (call.flags, call.arg(), call.has("--locked"));
    let p = Project::of(None, flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: no vyrn.json found");
        return Err(ExitCode::FAILURE);
    };
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
            return Err(ExitCode::FAILURE);
        }
        eprintln!("nothing to update");
        return Ok(ExitCode::SUCCESS);
    }
    let dir = Some(manifest.dir.as_str());
    for (name, version) in &tools {
        let mut lock = p.resolver.lock.borrow_mut();
        failed(if locked {
            verify_tool(name, version, &lock, dir, flags.offline)
        } else {
            update_tool(name, version, &mut lock, flags.offline)
        })?;
    }
    for (name, spec) in &targets {
        let mut lock = p.resolver.lock.borrow_mut();
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
            return Err(ExitCode::FAILURE);
        }
    }
    for (_, spec) in &targets {
        failed(vyrn_frontend::loader::ModuleResolver::read(
            &p.resolver,
            spec,
        ))?;
    }
    if !locked {
        p.save_lock()?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `vyrn vendor [--check]`: copies every locked blob into the vendor directory,
/// or with `--check` verifies each is there.
fn vendor(call: &Call) -> Outcome {
    let check = call.has("--check");
    let p = Project::of(None, call.flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: no vyrn.json found");
        return Err(ExitCode::FAILURE);
    };
    let lock = p.resolver.lock.borrow();
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
                    return Err(ExitCode::FAILURE);
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
        return Err(ExitCode::FAILURE);
    }
    println!(
        "vendor is complete ({} entr{})",
        lock.entries.len(),
        if lock.entries.len() == 1 { "y" } else { "ies" }
    );
    Ok(ExitCode::SUCCESS)
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
fn test_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let filter = call.value("--name");
    let (program, _) = p.checked(&path, &read_source(&path)?)?;
    let has_tests = program.tests.iter().any(|t| t.module.is_none());
    if !has_tests {
        println!("no tests");
        return Ok(ExitCode::SUCCESS);
    }
    let bodies: Vec<Body> = program
        .tests
        .iter()
        .filter(|t| t.module.is_none() && filter.is_none_or(|s| t.name.contains(s)))
        .map(|t| Body {
            name: t.name.clone(),
            body: t.body.clone(),
            line: t.line,
        })
        .collect();
    Ok(bodies_wasm(&path, &program, "test", &bodies))
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
fn bench_cmd(call: &Call) -> Outcome {
    let filter = call.value("--name");
    let (check, json, compare) = (
        call.has("--check"),
        call.has("--json"),
        call.value("--compare"),
    );
    let threshold = match call.parsed::<f64>("--threshold", "a positive number")? {
        None => 1.5,
        Some(t) if t > 0.0 => t,
        Some(_) => return Err(call.refuse("--threshold needs a positive number")),
    };
    if check && (json || compare.is_some()) {
        return Err(call.refuse("--check cannot be combined with --json or --compare"));
    }

    let (p, path) = call.root()?;
    let (program, _) = p.checked(&path, &read_source(&path)?)?;

    let matches = |name: &str| filter.is_none_or(|sub| name.contains(sub));
    let has_selected = program
        .benches
        .iter()
        .any(|b| b.module.is_none() && matches(&b.name));
    if !has_selected {
        println!("no benches");
        return Ok(ExitCode::SUCCESS);
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
        return Ok(bodies_wasm(&path, &program, "bench", &bodies));
    }
    if let Some(baseline) = compare {
        let ungate = call.value("--ungate");
        return Ok(bench_compare(
            &p, &path, filter, baseline, threshold, ungate,
        ));
    }
    let (code, _) = bench_native(&p, &path, filter, json, false)?;
    Ok(code)
}

/// Lifts the selected bench bodies to functions, replaces `main` with a
/// `std/bench` harness, builds it on the native route and runs it. With
/// `capture`, returns the harness's stdout.
fn bench_native(
    p: &Project,
    path: &str,
    filter: Option<&str>,
    json: bool,
    capture: bool,
) -> Result<(ExitCode, Option<String>), ExitCode> {
    use vyrn_frontend::ast::{Block, Expr, Id, Stmt, Type};

    // The harness import is appended, so every original line keeps its number.
    // One load, not two: the loader's name-privacy rename works only across
    // modules it sees in one load, and a merged second load bound `std/bench`'s
    // private calls to a user function of the same name.
    let source = read_source(path)?;
    let source = format!(
        "{source}
import {{ benchOne }} from \"std/bench\"
"
    );
    let (mut program, _) = p.checked(path, &source)?;

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
        let body_ref = Expr::var(format!("__vyrn_bench_body_{slot}"), 0);
        if json {
            measure_calls.push(Expr::call(
                "benchMeasure",
                vec![Expr::str(b.name.clone()), body_ref],
                0,
            ));
        } else {
            harness_stmts.push(Stmt::expr(Expr::call(
                "benchOne",
                vec![Expr::str(b.name.clone()), Expr::int(width), body_ref],
                0,
            )));
        }
    }
    if json {
        // `print(benchJson([benchMeasure(..), ..], "native", "O2"))`.
        harness_stmts.push(Stmt::expr(Expr::call(
            "print",
            vec![Expr::call(
                "benchJson",
                vec![
                    Expr::ArrayLit {
                        id: Id::NEW,
                        elems: measure_calls,
                        line: 0,
                    },
                    Expr::str("native"),
                    Expr::str("O2"),
                ],
                0,
            )],
            0,
        )));
    } else {
        harness_stmts.push(Stmt::expr(Expr::call("print", vec![Expr::str("")], 0)));
        harness_stmts.push(Stmt::expr(Expr::call(
            "print",
            vec![Expr::str(format!("{} benches", selected.len()))],
            0,
        )));
    }
    harness_stmts.push(Stmt::ret(Expr::int(0), 0));

    program.functions.retain(|f| f.name != "main");
    program.functions.push(synth_fn(
        "main".to_string(),
        Block {
            id: Id::NEW,
            stmts: harness_stmts,
        },
        Type::Int,
        0,
        false,
    ));
    program.benches.clear();
    program.tests.clear();
    program.number();
    // As a test host: the lifted bodies are checked again for the lowering's
    // record, and outside a host `blackBox` is refused, so the core cannot
    // lower the bodies and their locals leak.
    program.host.test = true;

    // The route and target `vyrn build` ships, so the timing describes the
    // artifact.
    let target = p.native_target()?;
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
        return Err(ExitCode::FAILURE);
    }
    let exe_name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    let out_path = dir.join(&exe_name);
    let world = vyrn_lower::analyze(&program);
    let built = build_wasm2c(path, &program, world, &out_path.to_string_lossy(), target);
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(ExitCode::FAILURE);
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
                return Err(ExitCode::FAILURE);
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
                return Err(ExitCode::FAILURE);
            }
        }
    };
    cleanup(&dir);
    Ok((ExitCode::from(code), out))
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
    p: &Project,
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

    let (run_code, captured) = match bench_native(p, path, filter, true, true) {
        Ok(run) => run,
        Err(code) => return code,
    };
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
struct ServeResponse {
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
enum ServeCall {
    Handle(ServeRequest),
    /// The next frame of the stream the last [`ServeAnswer::Live`] opened.
    Next,
    /// Release that stream: sent when it ends and the first time a write to the
    /// client fails.
    Close,
}

/// What the engine answers.
enum ServeAnswer {
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
fn serve_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    serve_loop(call, &p, &path, None, |port, n| match n {
        Some(n) => eprintln!("serving {path} on http://localhost:{port} with {n} workers"),
        None => eprintln!("serving {path} on http://localhost:{port}"),
    })
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

/// The one serving path of `vyrn serve` and `vyrn dev`: loads `root` with the
/// [`SERVE_SHIM`], binds `--port`, then answers on it until the process ends.
///
/// Without `--workers`, one resident instance answers every request, one at a
/// time: `_start` runs `main` once and the store stays open, so each request
/// sees what `main` wrote. With it, [`serve_pool_wasm`] answers, behind
/// [`refuse_workers_if_stateful`].
///
/// `banner` prints once `main` has run, given the bound port and the worker
/// count. `assets` is `vyrn dev`'s static tree.
fn serve_loop(
    call: &Call,
    p: &Project,
    root: &str,
    assets: Option<&DevAssets>,
    banner: impl Fn(u16, Option<usize>) + Send,
) -> Outcome {
    let port = call
        .parsed("--port", "a number in 0..=65535")?
        .unwrap_or(8080);
    let workers = call.parsed::<std::num::NonZeroUsize>("--workers", "a positive number")?;
    let what = call.cmd.name;
    // Appended before the load, so it is checked and every program line keeps
    // its number.
    let source = format!("{}\n{SERVE_SHIM}", read_source(root)?);
    let (mut program, _) = p.checked(root, &source)?;
    serve_rewrite(&mut program);
    let (program, world) = (&program, &vyrn_lower::analyze(&program));
    if !has_served_handle(program) {
        eprintln!("error: `vyrn {what}` needs `fn handle(req: Request) -> Response` in {root}");
        return Err(ExitCode::FAILURE);
    }
    // Bound before `main` runs, so a port clash fails first.
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        eprintln!("error: cannot bind port {port}: {e}");
        ExitCode::FAILURE
    })?;
    let port = listener.local_addr().map_or(port, |a| a.port());
    let banner = move |n| {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        banner(port, n);
    };
    let argv = vec![root.to_string()];
    if let Some(n) = workers.map(usize::from) {
        if let Some(exit) = refuse_workers_if_stateful(program, world) {
            return Err(exit);
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
        failed(serve_pool_wasm(program, world, argv, n, each, listen))?;
        return Ok(ExitCode::SUCCESS);
    }
    let bytes = failed(vyrn_codegen::direct::compile(program, world.clone()))?;
    let run = wasmrun::Run {
        argv,
        // Read per call, so a trap logs its wording, not a wasm backtrace.
        capture_stderr: true,
        ..Default::default()
    };
    let mut res = match failed(wasmrun::start(&bytes, &run, None))? {
        (res, 0) => res,
        (mut res, code) => {
            eprint!("{}", res.drain_err());
            eprintln!("error: main returned {code}, aborting {what}");
            return Err(ExitCode::FAILURE);
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
    Ok(ExitCode::SUCCESS)
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
    world: &std::sync::Arc<vyrn_lower::World>,
    argv: Vec<String>,
    workers: usize,
    worker: W,
    accept: A,
) -> Result<(), String>
where
    W: Fn(usize, &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>) + Send + Sync,
    A: FnOnce() -> Result<(), String> + Send,
{
    use vyrn_frontend::ast::{Block, Expr, Id, Stmt};
    let run = wasmrun::Run {
        argv,
        capture_stderr: true,
        ..Default::default()
    };
    let bytes = vyrn_codegen::direct::compile(program, world.clone())?;
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
            id: Id::NEW,
            stmts: vec![Stmt::ret(Expr::int(0), main.line)],
        };
    }
    quiet.number();
    let quiet_world = vyrn_lower::analyze(&quiet);
    let module = wasmrun::compile(&vyrn_codegen::direct::compile(&quiet, quiet_world)?, false)?;

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
fn refuse_workers_if_stateful(
    program: &vyrn_frontend::ast::Program,
    world: &vyrn_lower::World,
) -> Option<ExitCode> {
    // Calls through stored function values reach every collected source.
    let stored = &world.ownership.record.stored;
    let (chain, global) = vyrn_frontend::checker::module_state_use(program, "handle", stored)?;
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
fn dev_cmd(call: &Call) -> Outcome {
    let p = Project::of(None, call.flags)?;
    let Some(manifest) = &p.manifest else {
        eprintln!("error: `vyrn dev` needs a vyrn.json with `server` and `client` keys");
        return Err(ExitCode::FAILURE);
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
        return Err(ExitCode::FAILURE);
    };
    let Some(client_rel) = get_str("client") else {
        eprintln!("error: vyrn.json is missing a `\"client\"` entry (the wasm module to build)");
        return Err(ExitCode::FAILURE);
    };
    let public_rel = get_str("public").unwrap_or_else(|| "public".to_string());
    let server_path = format!("{}/{server_rel}", manifest.dir);
    let client_path = format!("{}/{client_rel}", manifest.dir);
    let public_dir = PathBuf::from(format!("{}/{public_rel}", manifest.dir));

    let Some(web_dir) = web_root() else {
        eprintln!("error: could not find the `web/` runtime directory (set VYRN_WEB)");
        return Err(ExitCode::FAILURE);
    };

    let dev_dir = PathBuf::from(format!("{}/.vyrn-dev", manifest.dir));
    if let Err(e) = std::fs::create_dir_all(&dev_dir) {
        eprintln!("error: cannot create {}: {e}", dev_dir.display());
        return Err(ExitCode::FAILURE);
    }
    let wasm_out = dev_dir.join("client.wasm");
    let _ = std::fs::remove_file(&wasm_out); // a stale wasm must not mask a failed build
    eprintln!("dev: building client {client_rel} -> wasm");
    let built = build_to(&p, &client_path, Some(&wasm_out.to_string_lossy()), true);
    if !wasm_out.is_file() {
        return built;
    }

    let assets = DevAssets {
        public_dir,
        web_dir,
        wasm: wasm_out,
    };
    let public_shown = assets.public_dir.display().to_string();
    serve_loop(call, &p, &server_path, Some(&assets), |port, n| {
        eprintln!("dev: serving {server_rel} on http://localhost:{port}");
        eprintln!("dev:   /rpc/*         -> server `handle` (rpcHandle + your pages)");
        eprintln!("dev:   /client.wasm   -> built from {client_rel}");
        eprintln!(
            "dev:   /vyrn-runtime/ -> web runtimes (wasi-min.js, vyrn-rpc.js, vyrn-query.js)"
        );
        eprintln!("dev:   /              -> {public_shown}/");
        if let Some(n) = n {
            eprintln!("dev:   workers        -> {n}");
        }
    })
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

/// `vyrn run [file] [args...]`. Generators run in the load, so its time is the
/// first row of the table `run_wasm` prints.
fn run_cmd(call: &Call) -> Outcome {
    let (p, path) = call.root()?;
    let clock = std::time::Instant::now();
    let (program, world) = p.checked(&path, &read_source(&path)?)?;
    let load = clock.elapsed();
    instantiable(&program, &world)?;
    let profile = call.flags.profile.then_some(load);
    Ok(run_wasm(&path, &program, world, &call.pos, profile))
}

/// Compiles the program and runs it in the embedded wasmtime; the exit code is
/// the guest's. `profile` is the load's time under `vyrn run --profile` (see
/// [`wasm_profile`]).
fn run_wasm(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    world: std::sync::Arc<vyrn_lower::World>,
    prog_args: &[String],
    profile: Option<std::time::Duration>,
) -> ExitCode {
    let clock = std::time::Instant::now();
    let bytes = match vyrn_codegen::direct::compile(program, world) {
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
        meter: profile.is_some(),
        ..Default::default()
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

/// Prints the operations the guest executed to stderr. Under
/// `VYRN_BUILD_PROFILE`, the phase table follows when `main` exits.
///
/// The count is wasmtime's fuel, read from a budget nothing exhausts. Unlike
/// the times, it is the same number on any machine.
fn wasm_profile(load: std::time::Duration, compile: std::time::Duration, meter: &wasmrun::Meter) {
    if vyrn_frontend::prof::phases_on() {
        vyrn_frontend::prof::charge("load", load);
        vyrn_frontend::prof::charge("compile", compile);
        vyrn_frontend::prof::charge("translate", meter.translate);
        vyrn_frontend::prof::charge("instantiate", meter.instantiate);
        vyrn_frontend::prof::charge("run", meter.run);
    }
    eprintln!("{} operation(s) executed", meter.fuel);
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
    kind: &str,
    bodies: &[Body],
) -> ExitCode {
    use vyrn_frontend::ast::{Block, Expr, Id, Stmt, Type};
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
        id: Id::NEW,
        stmts: vec![Stmt::ret(Expr::int(0), 0)],
    };
    prog.functions
        .push(synth_fn("main".to_string(), main, Type::Int, 0, false));
    prog.number();

    // A body that reaches a `gen fn` compiles the module as a generator host;
    // otherwise it pays for no `vyrn_gen` import. As a test host, the checker
    // accepts `assert`, `assertEq` and `blackBox` in the lifted bodies.
    prog.host.test = true;
    let world = vyrn_lower::analyze(&prog);
    let reach = vyrn_codegen::direct::gen_reach(&prog, &world);
    let generation = (0..bodies.len()).any(|k| reach.contains(&format!("__vyrn_body_{k}")));
    let compiled = if generation {
        vyrn_genwasm::prepare(&mut prog)
            .ok_or_else(|| "a `test` block calls a generator this route cannot compile".to_string())
            .and_then(|()| {
                vyrn_codegen::direct::compile_gen_host(&prog).map_err(|e| match e {
                    vyrn_frontend::gen::GenError::Failed(e) => e,
                    vyrn_frontend::gen::GenError::Refused(ds) => {
                        ds.iter().map(|d| d.render()).collect()
                    }
                })
            })
    } else {
        vyrn_codegen::direct::compile(&prog, world)
    };
    let bytes = match compiled {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let run = wasmrun::Run {
        argv: vec![path.to_string()],
        // Read per body: a trap's wording is the `FAILED:` message.
        capture_stderr: true,
        ..Default::default()
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

fn build(call: &Call) -> Outcome {
    let wasm = match call.value("--target") {
        None => false,
        Some("wasm" | "wasm32-wasi") => true,
        Some(other) => {
            return Err(call.refuse(&format!("unknown target `{other}` (expected `wasm`)")))
        }
    };
    let (p, path) = call.root()?;
    build_to(&p, &path, call.value("-o"), wasm)
}

/// Builds `path` to `out`, by default its stem in the working directory: the
/// module itself under `wasm`, else a native executable.
fn build_to(p: &Project, path: &str, out: Option<&str>, wasm: bool) -> Outcome {
    // Before the compile, so a misspelled `nativeTarget` fails first. A wasm
    // build ignores it.
    let native_target = if wasm { None } else { Some(p.native_target()?) };
    let (program, world) = p.checked(path, &read_source(path)?)?;
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a");
    let out_path = out.map(str::to_string).unwrap_or_else(|| {
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
        let bytes = failed(vyrn_codegen::direct::compile(&program, world))?;
        if let Err(e) = std::fs::write(&out_path, bytes) {
            eprintln!("error: cannot write {out_path}: {e}");
            return Err(ExitCode::FAILURE);
        }
    } else {
        let target = native_target.unwrap_or(DEFAULT_NATIVE_TARGET);
        build_wasm2c(path, &program, world, &out_path, target).map_err(|()| ExitCode::FAILURE)?;
    }
    println!("wrote {out_path}");
    Ok(ExitCode::SUCCESS)
}

/// The native route: the program's wasm through wasm2c to C, compiled by clang
/// with the WASI host and wabt's wasm-rt into an executable.
///
/// The intermediate files stay beside the output for inspection: `<out>.wasm`,
/// `<out>.w2c.c`, `<out>.w2c.h`, `<out>.host.c`.
fn build_wasm2c(
    path: &str,
    program: &vyrn_frontend::ast::Program,
    world: std::sync::Arc<vyrn_lower::World>,
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

    let bytes = match vyrn_codegen::direct::compile(program, world) {
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
        // `random_get` is `BCryptGenRandom`, as in `vyrn-genwasm/src/wasi.rs`.
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

    /// `docs/tooling.md` names each command of the table as `` `vyrn <name>``,
    /// names no other, and spells every flag the table declares.
    #[test]
    fn the_tooling_doc_indexes_every_command_and_flag() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/tooling.md"
        ))
        .expect("docs/tooling.md");
        let word = |c: char| c.is_ascii_lowercase() || c == '-';
        let mut named: Vec<&str> = doc
            .match_indices("`vyrn ")
            .map(|(i, m)| &doc[i + m.len()..])
            .map(|rest| &rest[..rest.find(|c| !word(c)).unwrap_or(rest.len())])
            .filter(|w| !w.is_empty() && !w.starts_with('-'))
            .collect();
        named.sort();
        named.dedup();
        let mut commands: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        commands.sort();
        assert_eq!(named, commands, "docs/tooling.md and COMMANDS disagree");
        let flags = GLOBAL_FLAGS
            .iter()
            .chain(COMMANDS.iter().flat_map(|c| c.flags));
        for Flag(name, _, _) in flags {
            let spelled = doc.match_indices(name).any(|(i, _)| {
                !doc[i + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '-')
            });
            assert!(spelled, "docs/tooling.md never spells {name}");
        }
    }

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

    #[test]
    fn apply_edits_counts_columns_in_characters_and_runs_from_the_end() {
        let text = "let δata = read()\nprint(δata) print(δata)\n";
        let copy = |line, col| Fix::Copy { line, col };
        let fixed = apply_edits(text, &[copy(2, 11), copy(2, 23)]).unwrap();
        assert_eq!(
            fixed,
            "let δata = read()\nprint(δata.copy()) print(δata.copy())\n"
        );
    }

    #[test]
    fn apply_edits_refuses_a_position_past_the_line() {
        let copy = |line, col| [Fix::Copy { line, col }];
        assert!(apply_edits(
            "a
",
            &copy(1, 3)
        )
        .is_err());
        assert!(apply_edits(
            "a
",
            &copy(3, 1)
        )
        .is_err());
    }

    #[test]
    fn apply_edits_deletes_the_keyword_and_inserts_the_call() {
        let fixes = [
            Fix::Unconsume {
                line: 1,
                col: 9,
                len: 9,
            },
            Fix::Copy { line: 1, col: 21 },
        ];
        assert_eq!(
            apply_edits(
                "let a = consume  p.x
",
                &fixes
            )
            .unwrap(),
            "let a = p.x.copy()
"
        );
        assert!(apply_edits(
            "a
",
            &fixes[..1]
        )
        .is_err());
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
        let (mut program, _) = Project::of(Some(&key), GlobalFlags::default())
            .and_then(|p| p.checked(&key, &source))
            .expect("the doors load and check");
        serve_rewrite(&mut program);
        let world = vyrn_lower::analyze(&program);
        let bytes = vyrn_codegen::direct::compile(&program, world).expect("the doors compile");
        let run = wasmrun::Run {
            argv: vec![key.clone()],
            capture_stdout: true,
            capture_stderr: true,
            ..Default::default()
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
