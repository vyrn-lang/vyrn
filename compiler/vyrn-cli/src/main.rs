//! `vyrn` — the Vyrn driver.
//!
//! Usage:
//!   vyrn run     [file.vyrn]            Type-check and interpret; process exits with main's value.
//!   vyrn check   [file.vyrn]            Type-check only; print "ok" or every diagnostic.
//!   vyrn emit-ir [file.vyrn]            Print textual LLVM IR to stdout.
//!   vyrn emit-gen [file.vyrn]           Print every synthesized generator module (RFC-0021).
//!   vyrn build   [file.vyrn] [-o out] [--target wasm]
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
//!   vyrn deps                           Print the resolved module graph.
//!
//! The file argument is optional whenever a `vyrn.json` manifest (found by
//! walking up from the current directory) declares a `"main"`. The manifest's
//! `"dependencies"` map bare import specifiers to real ones.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod remote;

const USAGE: &str = "usage: vyrn <run|check|emit-ir|emit-gen|build|test|bench|serve|fmt> [file.vyrn] [-o out] [--target wasm] [--offline]\n       vyrn run [file.vyrn] [args...]   (trailing args reach the program's args())\n       vyrn test [file.vyrn] [--name <substring>]\n       vyrn bench [file.vyrn] [--name <substring>] [--check | --json | --compare <baseline.json> [--threshold <factor>]]   (native timing; --check runs each once under the interpreter; --json machine-readable; --compare flags regressions)\n       vyrn serve [file.vyrn] [--port N] [--workers N]   (HTTP host; needs `fn handle(req: Request) -> Response`)\n       vyrn dev [--port N] [--workers N]   (fullstack: build client to wasm + serve server root, static, runtimes)\n       vyrn fmt [file.vyrn ...] [--check]   (canonical formatter; no files = project main + local imports)\n       vyrn doc [file|dir] [-o <dir>] [--std] [--verify]   (Markdown API docs; default docs/api/; --verify is the drift gate)\n       vyrn new <name> | vyrn add <specifier> [--name alias] | vyrn update [alias] | vyrn vendor [--check] | vyrn deps";

/// `--offline` flag or `VYRN_OFFLINE=1`: never touch the network; a lock+cache
/// miss is a hard error instead.
fn offline(args: &[String]) -> bool {
    args.iter().any(|a| a == "--offline") || std::env::var("VYRN_OFFLINE").is_ok()
}

fn main() -> ExitCode {
    // The loader runs generators (RFC-0021) by invoking the tree-walking
    // interpreter recursively, nested deep inside the load/parse/check call
    // chain. On Windows the default ~1 MB main-thread stack overflows on a
    // realistic generator (e.g. std/i18n compiling ICU messages). Run the whole
    // CLI on a worker thread with a generous stack so generation has headroom.
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(real_main)
        .expect("failed to spawn the vyrn worker thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

fn real_main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let is_offline = offline(&args);
    if is_offline {
        // Normalized so every later resolver construction sees it.
        std::env::set_var("VYRN_OFFLINE", "1");
    }
    args.retain(|a| a != "--offline");
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
        return deps();
    }
    if cmd == "add" {
        return add(&args[2..], is_offline);
    }
    if cmd == "update" {
        return update(args.get(2).map(|s| s.as_str()));
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

    match cmd {
        "check" => match load_program(path, &source) {
            Ok(_) => {
                println!("ok");
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        "run" => {
            let program = match load_program(path, &source) {
                Ok(p) => p,
                Err(code) => return code,
            };
            match vyrn_frontend::interp::run_with_args(&program, &prog_args) {
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
        "emit-gen" => emit_gen(path, &source),
        other => {
            eprintln!("unknown command `{other}` (expected run, check, emit-ir, emit-gen, build, test, bench, or serve)");
            ExitCode::from(2)
        }
    }
}

/// `vyrn emit-gen [file]` (RFC-0021) — run every generator import the file
/// reaches and print the synthesized module source, each under a banner naming
/// its generator call site. Nothing is printed for a file with no generators.
fn emit_gen(path: &str, source: &str) -> ExitCode {
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let result = vyrn_frontend::loader::generated_modules(source, &root_key, &opts, &resolver);
    let _ = save_lock(&resolver);
    match result {
        Ok(mods) => {
            if mods.is_empty() {
                eprintln!("(no generator imports in {root_key})");
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
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        remote::gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        remote::gen_cache_put(key, value)
    }
}

/// The std-library root: `$VYRN_STD`, or `std/` found by walking up from the
/// executable (dev builds live at `<repo>/compiler/target/<profile>/vyrn`,
/// so the repo's `std/` is a few levels up). `None` if not found — only an
/// error if a program actually imports `std/...`.
fn std_root() -> Option<String> {
    if let Ok(p) = std::env::var("VYRN_STD") {
        if Path::new(&p).exists() {
            return Some(p.replace('\\', "/"));
        }
    }
    let mut dir = std::env::current_exe().ok()?;
    for _ in 0..5 {
        dir = dir.parent()?.to_path_buf();
        let cand = dir.join("std");
        if cand.is_dir() {
            return Some(cand.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// The `web/` root holding the browser runtimes (`wasi-min.js`, `vyrn-rpc.js`,
/// `vyrn-query.js`) — `$VYRN_WEB`, or `web/` found by walking up from the
/// executable (the sibling of `std/`). `vyrn dev` serves these to the page.
fn web_root() -> Option<String> {
    if let Ok(p) = std::env::var("VYRN_WEB") {
        if Path::new(&p).exists() {
            return Some(p.replace('\\', "/"));
        }
    }
    let mut dir = std::env::current_exe().ok()?;
    for _ in 0..5 {
        dir = dir.parent()?.to_path_buf();
        let cand = dir.join("web");
        if cand.is_dir() {
            return Some(cand.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// The project manifest (`vyrn.json`), parsed with the frontend's own JSON
/// parser. All fields optional; unknown keys are ignored (forward compat).
struct Manifest {
    /// Directory the manifest lives in (slash-separated).
    dir: String,
    main: Option<String>,
    dependencies: Vec<(String, String)>,
}

/// Find `vyrn.json` by walking up from `start` (a directory).
fn find_manifest(start: &Path) -> Option<Manifest> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("vyrn.json");
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate).ok()?;
            let doc = match vyrn_frontend::schema::parse_json(&text) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("warning: {} is not valid JSON: {e}", candidate.display());
                    return None;
                }
            };
            use vyrn_frontend::schema::Json;
            let main = match doc.get("main") {
                Some(Json::Str(s)) => Some(s.clone()),
                _ => None,
            };
            let dependencies = match doc.get("dependencies") {
                Some(Json::Obj(entries)) => entries
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Json::Str(s) => Some((k.clone(), s.clone())),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            return Some(Manifest {
                dir: dir.to_string_lossy().replace('\\', "/"),
                main,
                dependencies,
            });
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// The manifest's `main`, resolved relative to the manifest's directory.
fn manifest_main() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let m = find_manifest(&cwd)?;
    let main = m.main?;
    Some(format!("{}/{main}", m.dir))
}

/// LoadOptions for a root file: std root + the nearest manifest's aliases.
fn load_options(root: &str) -> vyrn_frontend::loader::LoadOptions {
    let mut opts =
        vyrn_frontend::loader::LoadOptions { std_root: std_root(), ..Default::default() };
    let start = Path::new(root)
        .parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok());
    if let Some(m) = start.and_then(|d| find_manifest(&d)) {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir;
    }
    opts
}

/// `vyrn new <name>` — scaffold vyrn.json + src/main.vyrn + .gitignore.
fn scaffold(name: &str) -> ExitCode {
    let root = Path::new(name);
    if root.exists() {
        eprintln!("error: `{name}` already exists");
        return ExitCode::FAILURE;
    }
    let manifest = format!(
        "{{\n    \"name\": \"{name}\",\n    \"main\": \"src/main.vyrn\",\n    \"dependencies\": {{}}\n}}\n"
    );
    let main_vyrn = format!(
        "fn main() -> Int64 {{\n    print(\"hello from {name}\")\n    return 0\n}}\n"
    );
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

/// `vyrn deps` — print the resolved module graph of the project's main.
fn deps() -> ExitCode {
    let Some(main) = manifest_main() else {
        eprintln!("error: no vyrn.json with a `main` found upward from here");
        return ExitCode::FAILURE;
    };
    let source = match std::fs::read_to_string(&main) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {main}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let root_key = main.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    match vyrn_frontend::loader::module_graph(&source, &root_key, &opts, &FsResolver) {
        Ok(graph) => {
            for (module, imports) in graph {
                println!("{module}");
                for i in imports {
                    println!("  -> {i}");
                }
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
    let check = rest.iter().any(|a| a == "--check");
    let files: Vec<String> = rest.iter().filter(|a| !a.starts_with('-')).cloned().collect();

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
                let formatted =
                    if uses_crlf { formatted.replace('\n', "\r\n") } else { formatted };
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
        println!("formatted {written} file{}", if written == 1 { "" } else { "s" });
    } else if !had_error {
        println!("already formatted");
    }
    if had_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
        .filter(|(i, a)| {
            !a.starts_with('-')
                && !(*i > 0 && rest[*i - 1] == "-o")
        })
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
fn discover_doc_modules(
    target: Option<&str>,
    with_std: bool,
) -> Result<Vec<DocModule>, ExitCode> {
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
                eprintln!("error: no input file or directory, and no vyrn.json with a `main` found");
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
        let source = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {p}: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
        out.push(DocModule { name: format!("{prefix}{rel}"), source });
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
    let base = find_manifest(Path::new(&root_key).parent().unwrap_or(Path::new(".")))
        .map(|m| m.dir)
        .unwrap_or_else(|| {
            root_key
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default()
        });

    let graph =
        match vyrn_frontend::loader::module_graph_with_sources(&source, &root_key, &opts, &resolver)
        {
            Ok(g) => g,
            Err(diags) => {
                for d in &diags {
                    let file = d.file.as_deref().unwrap_or(&root_key);
                    eprintln!("{}:{}:{}: {}", file, d.line, d.col, d.message);
                }
                return Err(ExitCode::FAILURE);
            }
        };
    let _ = save_lock(&resolver);

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
    stripped.strip_suffix(".vyrn").unwrap_or(stripped).to_string()
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
/// its `///` block verbatim. Blocks are separated by a single blank line; the
/// page ends in exactly one newline.
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
    println!("wrote {} file{} to {out_dir}", files.len(), if files.len() == 1 { "" } else { "s" });
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
    if let Some(m) = start.clone().and_then(|d| find_manifest(&d)) {
        return (Path::new(&m.dir).join("vyrn.lock"), Some(m.dir));
    }
    let dir = start.unwrap_or_else(|| PathBuf::from("."));
    (dir.join("vyrn.lock"), None)
}

/// Build the CLI resolver (fs + lock/cache/network remote handling).
fn make_resolver(root_key: &str) -> remote::RemoteResolver {
    let (lock_path, project_dir) = lock_home(root_key);
    remote::RemoteResolver {
        lock: std::cell::RefCell::new(remote::Lock::load(lock_path)),
        project_dir,
        offline: std::env::var("VYRN_OFFLINE").is_ok(),
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
/// (with their originating file) on failure.
fn load_program(path: &str, source: &str) -> Result<vyrn_frontend::ast::Program, ExitCode> {
    // Strip Windows' verbatim prefix (`\\?\C:\..`) — it survives neither the
    // slash normalization nor readable diagnostics.
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    let opts = load_options(&root_key);
    let resolver = make_resolver(&root_key);
    let result = vyrn_frontend::load(source, &root_key, &opts, &resolver);
    // Pins are kept even when a later stage fails — fetched is pinned.
    save_lock(&resolver)?;
    match result {
        Ok(p) => Ok(p),
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
    let Some(manifest) = find_manifest(&cwd) else {
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
    // frontend's parser + this serializer keeps key order stable).
    let manifest_path = Path::new(&manifest.dir).join("vyrn.json");
    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|_| "{}".into());
    let doc = match vyrn_frontend::schema::parse_json(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: vyrn.json is not valid JSON: {e}");
            return ExitCode::FAILURE;
        }
    };
    use vyrn_frontend::schema::Json;
    let mut fields = match doc {
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

/// `vyrn update [alias]` — re-resolve floating refs (all remote deps, or just
/// one alias) and rewrite their pins.
fn update(alias: Option<&str>) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = find_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found");
        return ExitCode::FAILURE;
    };
    let (lock_path, project_dir) = lock_home(&format!("{}/vyrn.json", manifest.dir));
    let mut lock = remote::Lock::load(lock_path);
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
    if targets.is_empty() {
        eprintln!("nothing to update");
        return ExitCode::SUCCESS;
    }
    for (name, spec) in &targets {
        lock.entries.remove(spec);
        lock.dirty = true;
        println!("re-resolving `{name}` ({spec})");
    }
    let resolver = remote::RemoteResolver {
        lock: std::cell::RefCell::new(lock),
        project_dir,
        offline: false,
    };
    for (_, spec) in &targets {
        if let Err(e) = vyrn_frontend::loader::ModuleResolver::read(&resolver, spec) {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }
    if save_lock(&resolver).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `vyrn vendor [--check]` — copy every locked blob into ./vyrn_vendor (or
/// verify it is already there), making the checkout self-contained forever.
fn vendor(check: bool) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(manifest) = find_manifest(&cwd) else {
        eprintln!("error: no vyrn.json found");
        return ExitCode::FAILURE;
    };
    let (lock_path, _) = lock_home(&format!("{}/vyrn.json", manifest.dir));
    let lock = remote::Lock::load(lock_path);
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
                if let Err(e) = std::fs::create_dir_all(&vend)
                    .and_then(|_| std::fs::write(&vendored, &bytes))
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
        eprintln!("{missing} entr{} not vendored", if missing == 1 { "y" } else { "ies" });
        return ExitCode::FAILURE;
    }
    println!("vendor is complete ({} entr{})", lock.entries.len(),
        if lock.entries.len() == 1 { "y" } else { "ies" });
    ExitCode::SUCCESS
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
        Json::Str(s) => format!("{s:?}"),
        Json::Arr(items) => {
            if items.is_empty() {
                return "[]".into();
            }
            let inner: Vec<String> =
                items.iter().map(|v| format!("{pad}{}", json_pretty(v, depth + 1))).collect();
            format!("[\n{}\n{close}]", inner.join(",\n"))
        }
        Json::Obj(fields) => {
            if fields.is_empty() {
                return "{}".into();
            }
            let inner: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{pad}{k:?}: {}", json_pretty(v, depth + 1)))
                .collect();
            format!("{{\n{}\n{close}}}", inner.join(",\n"))
        }
    }
}

/// The portable half of the runtime: `stderr`/`stdout` are C macros with no
/// linkable symbol, so the emitted IR calls these two functions instead. The
/// shim is compiled by clang next to the IR on every target — MSVC, glibc,
/// and wasi-libc alike.
const RUNTIME_SHIM: &str = r#"
/* MSVC's UCRT deprecates fopen in favor of fopen_s; the portable spelling is
   intentional (glibc and wasi-libc have no fopen_s), so silence the advisory. */
#define _CRT_SECURE_NO_WARNINGS
/* rand_s (a UCRT CSPRNG) needs this defined before <stdlib.h> on MSVC/UCRT; it
   is the native Windows seed source (RFC-0043). Harmless elsewhere. */
#define _CRT_RAND_S
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#if !defined(_WIN32)
/* getentropy: the POSIX/wasi seed CSPRNG (glibc >= 2.25 and wasi-libc). */
#include <unistd.h>
#include <sys/random.h>
#endif
#if defined(_WIN32)
/* _commit / _fileno for fsync (RFC-0044). MoveFileExA gives the atomic overwrite
   the C `rename` refuses on Windows (it fails when the target exists); declared
   here (not via the heavy <windows.h>, which would leak min/max macros into the
   codec below) and satisfied from kernel32. */
#include <io.h>
__declspec(dllimport) int __stdcall MoveFileExA(const char*, const char*, unsigned long);
__declspec(dllimport) unsigned long __stdcall GetLastError(void);
#pragma comment(lib, "kernel32")
#define VYRN_MOVEFILE_REPLACE_EXISTING 0x1u
#define VYRN_ERROR_NOT_SAME_DEVICE 17u
#endif

void* __vyrn_stderr(void) { return stderr; }
void* __vyrn_stdout(void) { return stdout; }

/* size_t-clean wrappers: the IR always passes/returns 64-bit sizes, so these
   adapt on ILP32 targets (wasm32) and are transparent on LP64/LLP64. */
unsigned long long __vyrn_strlen(const char* s) { return (unsigned long long)strlen(s); }

/* charCount (RFC-0058): the number of Unicode scalar values in a validated UTF-8
   string = the count of non-continuation bytes (those where (b & 0xC0) != 0x80).
   Byte-identical to the interpreter's loop. Strings are NUL-terminated (interior
   NUL is rejected at construction), so `strlen`-style iteration is exact. */
unsigned long long __vyrn_charcount(const char* s) {
    unsigned long long n = 0;
    for (const unsigned char* p = (const unsigned char*)s; *p; p++) {
        if ((*p & 0xC0) != 0x80) n++;
    }
    return n;
}

/* Allocation failure is a trap, not a null dereference: the emitted IR never
   null-checks (every alloc site would need a branch), so the single choke
   point checks instead. The size guard matters on ILP32 (wasm32): without it
   a 64-bit request silently truncates in the (size_t) cast, and a huge size
   could wrap to a tiny allocation - a buffer overflow, not an error. */
static void* __vyrn_alloc_check(void* p, unsigned long long n) {
    if (p == NULL && n > 0) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return p;
}
void* __vyrn_malloc(unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return __vyrn_alloc_check(malloc((size_t)n), n);
}
void* __vyrn_realloc(void* p, unsigned long long n) {
    if (n > (unsigned long long)(size_t)-1) {
        fputs("error: out of memory\n", stderr);
        exit(1);
    }
    return __vyrn_alloc_check(realloc(p, (size_t)n), n);
}
int __vyrn_strncmp(const char* a, const char* b, unsigned long long n) {
    return strncmp(a, b, (size_t)n);
}

/* ---- Map<String, V> runtime (RFC-0028) ---------------------------------- */
/* A Map lowers to { char** keys, char* vals, i64 len, i64 cap } — two parallel
   growable buffers sharing one length/capacity, in first-insertion order. The
   value buffer is raw bytes with a per-entry stride `esz` (the value type's
   size, passed by the caller). Keys are stored by pointer (no copy — matching
   the array element-store convention). Lookup is a linear strcmp scan. */
typedef struct { char** keys; char* vals; long long len, cap; } VMap;
/* Index of `key`, or -1. Operates on a raw keys buffer so read paths (`at`,
   `has`) can call it with values extracted from an SSA aggregate. */
long long __vyrn_map_find(char** keys, long long len, const char* key) {
    long long i;
    for (i = 0; i < len; i++) if (strcmp(keys[i], key) == 0) return i;
    return -1;
}
/* Ensure room for one more entry, growing both buffers (cap 0 -> 4, else 2x). */
void __vyrn_map_reserve(VMap* m, long long esz) {
    if (m->len + 1 > m->cap) {
        m->cap = m->cap ? m->cap * 2 : 4;
        m->keys = (char**)__vyrn_realloc(m->keys, (unsigned long long)m->cap * sizeof(char*));
        m->vals = (char*)__vyrn_realloc(m->vals, (unsigned long long)m->cap * (unsigned long long)esz);
    }
}
/* Remove entry `i`, shifting later entries down so first-insertion order is
   preserved for the survivors (remove-then-insert therefore moves a key end). */
void __vyrn_map_remove_at(VMap* m, long long i, long long esz) {
    long long rest = m->len - i - 1;
    if (rest > 0) {
        memmove(m->keys + i, m->keys + i + 1, (size_t)(rest * (long long)sizeof(char*)));
        memmove(m->vals + i * esz, m->vals + (i + 1) * esz, (size_t)(rest * esz));
    }
    m->len--;
}
/* A snapshot copy of the key pointers (for `keys()`), owned by the fresh
   Array<String>; the map may then be mutated without disturbing the snapshot. */
char** __vyrn_map_keys_copy(char** keys, long long len) {
    char** r = (char**)__vyrn_malloc((unsigned long long)(len ? len : 1) * sizeof(char*));
    long long i;
    for (i = 0; i < len; i++) r[i] = keys[i];
    return r;
}
int __vyrn_snprintf(char* buf, unsigned long long n, const char* fmt, ...) {
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(buf, (size_t)n, fmt, ap);
    va_end(ap);
    return r;
}

/* ---- input I/O (RFC-0014) ----------------------------------------------- */
/* argv is stashed by `main` and served to `args()` as argv[1..]. wasi-libc
   populates argv identically on the wasm target (the host provides args_get). */
static int __vyrn_argc = 0;
static char** __vyrn_argv = 0;
long long __vyrn_args_count(void) {
    return (long long)(__vyrn_argc > 1 ? __vyrn_argc - 1 : 0);
}
const char* __vyrn_args_get(long long i) { return __vyrn_argv[i + 1]; }

/* readLine: one line from stdin as a malloc'd, NUL-terminated buffer with its
   trailing \r?\n stripped; *outlen is its byte length. Returns NULL at EOF (no
   bytes) and also for a line containing an embedded NUL byte, which cannot live
   in a NUL-terminated Vyrn String (the parity-safe rule, RFC-0014). The codegen
   validates UTF-8 (via the shared DFA); an invalid line reads as None too. */
char* __vyrn_read_line(unsigned long long* outlen) {
    int c = getchar();
    if (c == EOF) return 0;
    unsigned long long cap = 64, len = 0;
    char* buf = (char*)__vyrn_malloc(cap);
    int had_nul = 0;
    while (c != EOF && c != '\n') {
        if (c == 0) had_nul = 1;
        if (len + 2 >= cap) { cap *= 2; buf = (char*)__vyrn_realloc(buf, cap); }
        buf[len++] = (char)c;
        c = getchar();
    }
    if (len > 0 && buf[len - 1] == '\r') len--;
    buf[len] = '\0';
    if (had_nul) { free(buf); return 0; }
    *outlen = len;
    return buf;
}

/* readFile: whole file into a malloc'd, NUL-terminated buffer (*out, *outlen).
   Status: 0 ok, 1 io-error (missing/permission/directory/read error), 3 the
   file contains an embedded NUL byte. UTF-8 validation (status 2) is done by
   the codegen after this returns, reusing the shared DFA. A read loop (not
   fseek/ftell) keeps it portable across regular files, pipes, and wasi-libc. */
int __vyrn_read_file(const char* path, char** out, unsigned long long* outlen) {
    FILE* f = fopen(path, "rb");
    if (f == 0) return 1;
    unsigned long long cap = 1024, len = 0;
    char* buf = (char*)__vyrn_malloc(cap);
    for (;;) {
        if (len + 1 >= cap) { cap *= 2; buf = (char*)__vyrn_realloc(buf, cap); }
        size_t got = fread(buf + len, 1, (size_t)(cap - len - 1), f);
        len += (unsigned long long)got;
        if (got == 0) break;
    }
    int bad = ferror(f);
    fclose(f);
    if (bad) { free(buf); return 1; }
    buf[len] = '\0';
    for (unsigned long long k = 0; k < len; k++) {
        if (buf[k] == 0) { free(buf); return 3; }
    }
    *out = buf;
    *outlen = len;
    return 0;
}

/* readFileBytes (M2): binary read, no UTF-8/NUL checks. Status 0 ok / 1 io. */
int __vyrn_read_file_bytes(const char* path, char** out, unsigned long long* outlen) {
    FILE* f = fopen(path, "rb");
    if (f == 0) return 1;
    unsigned long long cap = 1024, len = 0;
    char* buf = (char*)__vyrn_malloc(cap);
    for (;;) {
        if (len + 1 >= cap) { cap *= 2; buf = (char*)__vyrn_realloc(buf, cap); }
        size_t got = fread(buf + len, 1, (size_t)(cap - len), f);
        len += (unsigned long long)got;
        if (got == 0) break;
    }
    int bad = ferror(f);
    fclose(f);
    if (bad) { free(buf); return 1; }
    *out = buf;
    *outlen = len;
    return 0;
}

/* writeFile: create/truncate + write all bytes. Status 0 ok / 1 io-error. A
   Vyrn String is NUL-terminated and never contains a NUL, so strlen is its
   full length. */
int __vyrn_write_file(const char* path, const char* contents) {
    FILE* f = fopen(path, "wb");
    if (f == 0) return 1;
    size_t n = strlen(contents);
    size_t wrote = fwrite(contents, 1, n, f);
    int bad = (wrote != n);
    if (fclose(f) != 0) bad = 1;
    return bad ? 1 : 0;
}

/* renameFile: atomically move `from` over `to` (RFC-0044). Status 0 ok / 1 io /
   2 cross-device. POSIX/wasi `rename` replaces atomically and reports EXDEV;
   Windows C `rename` refuses an existing target, so MoveFileExA(REPLACE_EXISTING)
   is used and ERROR_NOT_SAME_DEVICE maps to the cross-device status. */
int __vyrn_rename_file(const char* from, const char* to) {
#if defined(_WIN32)
    if (MoveFileExA(from, to, VYRN_MOVEFILE_REPLACE_EXISTING) != 0) return 0;
    return GetLastError() == VYRN_ERROR_NOT_SAME_DEVICE ? 2 : 1;
#else
    if (rename(from, to) == 0) return 0;
    return errno == EXDEV ? 2 : 1;
#endif
}

/* fsyncFile: flush a file's data to stable storage (RFC-0044, the optional
   power-durability step). Open, sync the descriptor, close. Status 0 ok / 1 io.
   wasi-libc lowers fsync to fd_sync. */
int __vyrn_fsync_file(const char* path) {
    /* read+write (not "rb"): flushing buffers needs write access on Windows
       (_commit → FlushFileBuffers); "rb+" opens an existing file without
       truncating it. */
    FILE* f = fopen(path, "rb+");
    if (f == 0) return 1;
    int rc = 0;
#if defined(_WIN32)
    if (_commit(_fileno(f)) != 0) rc = 1;
#else
    if (fsync(fileno(f)) != 0) rc = 1;
#endif
    fclose(f);
    return rc;
}

/* ---- time & randomness at the host boundary (RFC-0043) ------------------ */
/* now()/monotonic()/randomSeed() are host INPUTS, not part of the deterministic
   core. Each honors an injected value (VYRN_FIXED_TIME / VYRN_FIXED_SEED) so the
   parity harness can fix the clock and seed identically in every backend; the
   interpreter reads the same env. Absent the env vars they read the real host.
   These symbols are compiled on EVERY target (native + wasi), so a clock/random
   program links and runs under wasmtime with no `vyrn` host page: timespec_get /
   clock_gettime / getentropy lower to WASI clock_time_get / random_get. */

/* Wall clock, epoch milliseconds (UTC). timespec_get(TIME_UTC) is the portable
   spelling across UCRT, glibc, and wasi-libc. */
long long __vyrn_now_millis(void) {
    const char* e = getenv("VYRN_FIXED_TIME");
    if (e && e[0]) return strtoll(e, 0, 10);
    struct timespec ts;
    if (timespec_get(&ts, TIME_UTC) == 0) return 0;
    return (long long)ts.tv_sec * 1000 + (long long)(ts.tv_nsec / 1000000);
}

/* Monotonic nanoseconds. Under a fixed clock: a fixed base plus a deterministic
   per-call increment, so successive calls are byte-identical across backends
   (the interpreter mirrors this base/step exactly: 1e9 + n*1e6). */
static long long __vyrn_mono_ctr = 0;
long long __vyrn_monotonic_nanos(void) {
    const char* e = getenv("VYRN_FIXED_TIME");
    if (e && e[0]) {
        long long v = 1000000000LL + __vyrn_mono_ctr * 1000000LL;
        __vyrn_mono_ctr++;
        return v;
    }
#if defined(_WIN32)
    /* UCRT has no clock_gettime(CLOCK_MONOTONIC); the wall clock in ns is an
       adequate elapsed source (never exercised under the fixed-clock harness). */
    struct timespec ts;
    timespec_get(&ts, TIME_UTC);
    return (long long)ts.tv_sec * 1000000000LL + (long long)ts.tv_nsec;
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + (long long)ts.tv_nsec;
#endif
}

/* An unpredictable Int64 seed from the host CSPRNG. */
long long __vyrn_random_seed(void) {
    const char* e = getenv("VYRN_FIXED_SEED");
    if (e && e[0]) return strtoll(e, 0, 10);
#if defined(_WIN32)
    unsigned int a = 0, b = 0;
    rand_s(&a);
    rand_s(&b);
    return (long long)(((unsigned long long)a << 32) ^ (unsigned long long)b);
#else
    unsigned long long v = 0;
    if (getentropy(&v, sizeof v) != 0) v = 0;
    return (long long)v;
#endif
}

/* ---- JSON codec runtime (RFC-0018) -------------------------------------- */
/* A tagged JSON DOM plus a parser, canonical encoder, and a decode-side issue
   accumulator. The per-type encode/decode functions are GENERATED as LLVM IR
   (see vyrn-codegen); this shim owns the parity-critical string work — number
   formatting, escaping, and the parser error wording — so the native output is
   byte-identical to the interpreter's `crate::codec`. */

enum { VJ_NULL = 0, VJ_BOOL = 1, VJ_NUM = 2, VJ_STR = 3, VJ_ARR = 4, VJ_OBJ = 5 };
typedef struct VJ VJ;
typedef struct { char* key; VJ* val; } VJMember;
struct VJ {
    int kind;
    int bval;                                   /* VJ_BOOL */
    char* text;                                 /* VJ_NUM verbatim / VJ_STR bytes */
    int is_int;                                 /* VJ_NUM: integer syntax? */
    VJ** items; unsigned long long nitems, capitems;   /* VJ_ARR */
    VJMember* mem; unsigned long long nmem, capmem;     /* VJ_OBJ */
};

static char* __vyrn_dup(const char* s) {
    unsigned long long n = strlen(s);
    char* r = (char*)__vyrn_malloc(n + 1);
    memcpy(r, s, n + 1);
    return r;
}
static VJ* __vyrn_vj_new(int kind) {
    VJ* v = (VJ*)__vyrn_malloc(sizeof(VJ));
    v->kind = kind; v->bval = 0; v->text = 0; v->is_int = 0;
    v->items = 0; v->nitems = 0; v->capitems = 0;
    v->mem = 0; v->nmem = 0; v->capmem = 0;
    return v;
}
VJ* __vyrn_vj_obj(void) { return __vyrn_vj_new(VJ_OBJ); }
VJ* __vyrn_vj_arr(void) { return __vyrn_vj_new(VJ_ARR); }
VJ* __vyrn_vj_null(void) { return __vyrn_vj_new(VJ_NULL); }
VJ* __vyrn_vj_bool(int b) { VJ* v = __vyrn_vj_new(VJ_BOOL); v->bval = b ? 1 : 0; return v; }
static VJ* __vyrn_vj_num_text(const char* t, int is_int) {
    VJ* v = __vyrn_vj_new(VJ_NUM); v->text = __vyrn_dup(t); v->is_int = is_int; return v;
}
VJ* __vyrn_vj_int(long long x) {
    char buf[32]; __vyrn_snprintf(buf, 32, "%lld", x); return __vyrn_vj_num_text(buf, 1);
}
VJ* __vyrn_vj_uint(unsigned long long x) {
    char buf[32]; __vyrn_snprintf(buf, 32, "%llu", x); return __vyrn_vj_num_text(buf, 1);
}
VJ* __vyrn_vj_float(double x) {
    /* NaN renders as `NaN` (matching the interpreter's Rust formatting). */
    if (x != x) return __vyrn_vj_num_text("NaN", 0);
    char buf[512]; __vyrn_snprintf(buf, 512, "%f", x); return __vyrn_vj_num_text(buf, 0);
}
VJ* __vyrn_vj_str(const char* s) { VJ* v = __vyrn_vj_new(VJ_STR); v->text = __vyrn_dup(s); return v; }
void __vyrn_vj_push(VJ* a, VJ* c) {
    if (a->nitems + 1 > a->capitems) {
        a->capitems = a->capitems ? a->capitems * 2 : 4;
        a->items = (VJ**)__vyrn_realloc(a->items, a->capitems * sizeof(VJ*));
    }
    a->items[a->nitems++] = c;
}
void __vyrn_vj_set(VJ* o, const char* key, VJ* c) {
    if (o->nmem + 1 > o->capmem) {
        o->capmem = o->capmem ? o->capmem * 2 : 4;
        o->mem = (VJMember*)__vyrn_realloc(o->mem, o->capmem * sizeof(VJMember));
    }
    o->mem[o->nmem].key = __vyrn_dup(key);
    o->mem[o->nmem].val = c;
    o->nmem++;
}

/* ---- growable byte buffer (encoder) ------------------------------------- */
typedef struct { char* p; unsigned long long len, cap; } VSB;
static void vsb_init(VSB* s) { s->cap = 64; s->len = 0; s->p = (char*)__vyrn_malloc(s->cap); s->p[0] = 0; }
static void vsb_ensure(VSB* s, unsigned long long extra) {
    if (s->len + extra + 1 > s->cap) {
        while (s->len + extra + 1 > s->cap) s->cap *= 2;
        s->p = (char*)__vyrn_realloc(s->p, s->cap);
    }
}
static void vsb_putc(VSB* s, char c) { vsb_ensure(s, 1); s->p[s->len++] = c; s->p[s->len] = 0; }
static void vsb_puts(VSB* s, const char* t) {
    unsigned long long n = strlen(t); vsb_ensure(s, n); memcpy(s->p + s->len, t, n); s->len += n; s->p[s->len] = 0;
}
static void vsb_escape(VSB* s, const char* t) {
    vsb_putc(s, '"');
    for (const unsigned char* q = (const unsigned char*)t; *q; q++) {
        unsigned char c = *q;
        if (c == '"') vsb_puts(s, "\\\"");
        else if (c == '\\') vsb_puts(s, "\\\\");
        else if (c == '\n') vsb_puts(s, "\\n");
        else if (c == '\t') vsb_puts(s, "\\t");
        else if (c == '\r') vsb_puts(s, "\\r");
        else if (c < 0x20) { char b[8]; __vyrn_snprintf(b, 8, "\\u%04x", (unsigned)c); vsb_puts(s, b); }
        else vsb_putc(s, (char)c);
    }
    vsb_putc(s, '"');
}
static void __vyrn_vj_write(VSB* s, VJ* v) {
    unsigned long long i;
    switch (v->kind) {
        case VJ_NULL: vsb_puts(s, "null"); break;
        case VJ_BOOL: vsb_puts(s, v->bval ? "true" : "false"); break;
        case VJ_NUM: vsb_puts(s, v->text); break;
        case VJ_STR: vsb_escape(s, v->text); break;
        case VJ_ARR:
            vsb_putc(s, '[');
            for (i = 0; i < v->nitems; i++) { if (i) vsb_putc(s, ','); __vyrn_vj_write(s, v->items[i]); }
            vsb_putc(s, ']');
            break;
        default: /* VJ_OBJ */
            vsb_putc(s, '{');
            for (i = 0; i < v->nmem; i++) {
                if (i) vsb_putc(s, ',');
                vsb_escape(s, v->mem[i].key);
                vsb_putc(s, ':');
                __vyrn_vj_write(s, v->mem[i].val);
            }
            vsb_putc(s, '}');
            break;
    }
}
char* __vyrn_vj_encode(VJ* v) { VSB s; vsb_init(&s); __vyrn_vj_write(&s, v); return s.p; }

/* ---- parser (byte positions; wording mirrors crate::codec) -------------- */
typedef struct { const char* b; unsigned long long i, n; char* err; } VJP;
static void vjp_err_pos(VJP* p, const char* what) {
    char buf[64]; __vyrn_snprintf(buf, 64, "%s at position %llu", what, p->i);
    p->err = __vyrn_dup(buf);
}
static void vjp_err_end(VJP* p) { p->err = __vyrn_dup("unexpected end of input"); }
static void vjp_ws(VJP* p) {
    while (p->i < p->n) {
        char c = p->b[p->i];
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') p->i++; else break;
    }
}
static int vjp_hex(unsigned char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}
static VJ* vjp_value(VJP* p);
static char* vjp_string(VJP* p) {
    p->i++;                                     /* opening quote */
    VSB s; vsb_init(&s);
    for (;;) {
        if (p->i >= p->n) { vjp_err_end(p); return 0; }
        unsigned char c = (unsigned char)p->b[p->i];
        if (c == '"') { p->i++; return s.p; }
        if (c == '\\') {
            p->i++;
            if (p->i >= p->n) { vjp_err_end(p); return 0; }
            char e = p->b[p->i];
            if (e == '"') vsb_putc(&s, '"');
            else if (e == '\\') vsb_putc(&s, '\\');
            else if (e == '/') vsb_putc(&s, '/');
            else if (e == 'n') vsb_putc(&s, '\n');
            else if (e == 't') vsb_putc(&s, '\t');
            else if (e == 'r') vsb_putc(&s, '\r');
            else if (e == 'b') vsb_putc(&s, '\b');
            else if (e == 'f') vsb_putc(&s, '\f');
            else if (e == 'u') {
                unsigned int cp = 0; int k;
                for (k = 0; k < 4; k++) {
                    p->i++;
                    if (p->i >= p->n) { vjp_err_end(p); return 0; }
                    int h = vjp_hex((unsigned char)p->b[p->i]);
                    if (h < 0) { vjp_err_pos(p, "unexpected character"); return 0; }
                    cp = cp * 16 + (unsigned)h;
                }
                if (cp >= 0xD800 && cp <= 0xDFFF) { vjp_err_pos(p, "unexpected character"); return 0; }
                if (cp < 0x80) vsb_putc(&s, (char)cp);
                else if (cp < 0x800) {
                    vsb_putc(&s, (char)(0xC0 | (cp >> 6)));
                    vsb_putc(&s, (char)(0x80 | (cp & 0x3F)));
                } else {
                    vsb_putc(&s, (char)(0xE0 | (cp >> 12)));
                    vsb_putc(&s, (char)(0x80 | ((cp >> 6) & 0x3F)));
                    vsb_putc(&s, (char)(0x80 | (cp & 0x3F)));
                }
            } else { vjp_err_pos(p, "unexpected character"); return 0; }
            p->i++;
        } else if (c < 0x20) { vjp_err_pos(p, "unexpected character"); return 0; }
        else { vsb_putc(&s, (char)c); p->i++; }
    }
}
static int vjp_isdigit(VJP* p) { return p->i < p->n && p->b[p->i] >= '0' && p->b[p->i] <= '9'; }
static VJ* vjp_num(VJP* p) {
    unsigned long long start = p->i;
    int is_int = 1;
    if (p->i < p->n && p->b[p->i] == '-') p->i++;
    if (!vjp_isdigit(p)) { if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character"); return 0; }
    while (vjp_isdigit(p)) p->i++;
    if (p->i < p->n && p->b[p->i] == '.') {
        is_int = 0; p->i++;
        if (!vjp_isdigit(p)) { if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character"); return 0; }
        while (vjp_isdigit(p)) p->i++;
    }
    if (p->i < p->n && (p->b[p->i] == 'e' || p->b[p->i] == 'E')) {
        is_int = 0; p->i++;
        if (p->i < p->n && (p->b[p->i] == '+' || p->b[p->i] == '-')) p->i++;
        if (!vjp_isdigit(p)) { if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character"); return 0; }
        while (vjp_isdigit(p)) p->i++;
    }
    unsigned long long len = p->i - start;
    char* t = (char*)__vyrn_malloc(len + 1);
    memcpy(t, p->b + start, len); t[len] = 0;
    return __vyrn_vj_num_text(t, is_int);
}
static VJ* vjp_lit(VJP* p, const char* word, VJ* v) {
    for (const char* w = word; *w; w++) {
        if (p->i >= p->n) { vjp_err_end(p); return 0; }
        if (p->b[p->i] != *w) { vjp_err_pos(p, "unexpected character"); return 0; }
        p->i++;
    }
    return v;
}
static VJ* vjp_obj(VJP* p) {
    p->i++;                                     /* '{' */
    VJ* o = __vyrn_vj_obj();
    vjp_ws(p);
    if (p->i < p->n && p->b[p->i] == '}') { p->i++; return o; }
    for (;;) {
        vjp_ws(p);
        if (!(p->i < p->n && p->b[p->i] == '"')) { if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character"); return 0; }
        char* k = vjp_string(p);
        if (!k) return 0;
        vjp_ws(p);
        if (!(p->i < p->n && p->b[p->i] == ':')) { if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character"); return 0; }
        p->i++;
        vjp_ws(p);
        VJ* v = vjp_value(p);
        if (!v) return 0;
        __vyrn_vj_set(o, k, v);
        vjp_ws(p);
        if (p->i < p->n && p->b[p->i] == ',') { p->i++; continue; }
        if (p->i < p->n && p->b[p->i] == '}') { p->i++; return o; }
        if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character");
        return 0;
    }
}
static VJ* vjp_arr(VJP* p) {
    p->i++;                                     /* '[' */
    VJ* a = __vyrn_vj_arr();
    vjp_ws(p);
    if (p->i < p->n && p->b[p->i] == ']') { p->i++; return a; }
    for (;;) {
        vjp_ws(p);
        VJ* v = vjp_value(p);
        if (!v) return 0;
        __vyrn_vj_push(a, v);
        vjp_ws(p);
        if (p->i < p->n && p->b[p->i] == ',') { p->i++; continue; }
        if (p->i < p->n && p->b[p->i] == ']') { p->i++; return a; }
        if (p->i >= p->n) vjp_err_end(p); else vjp_err_pos(p, "unexpected character");
        return 0;
    }
}
static VJ* vjp_value(VJP* p) {
    if (p->i >= p->n) { vjp_err_end(p); return 0; }
    char c = p->b[p->i];
    if (c == '{') return vjp_obj(p);
    if (c == '[') return vjp_arr(p);
    if (c == '"') { char* s = vjp_string(p); if (!s) return 0; return __vyrn_vj_str(s); }
    if (c == 't') return vjp_lit(p, "true", __vyrn_vj_bool(1));
    if (c == 'f') return vjp_lit(p, "false", __vyrn_vj_bool(0));
    if (c == 'n') return vjp_lit(p, "null", __vyrn_vj_null());
    if (c == '-' || (c >= '0' && c <= '9')) return vjp_num(p);
    vjp_err_pos(p, "unexpected character");
    return 0;
}
VJ* __vyrn_json_parse(const char* src, char** errout) {
    VJP p; p.b = src; p.i = 0; p.n = strlen(src); p.err = 0;
    vjp_ws(&p);
    VJ* v = vjp_value(&p);
    if (!v) { *errout = p.err; return 0; }
    vjp_ws(&p);
    if (p.i != p.n) { vjp_err_pos(&p, "trailing characters"); *errout = p.err; return 0; }
    return v;
}

/* ---- decode-side accessors + issue accumulator -------------------------- */
int __vyrn_vj_kind(VJ* v) { return v->kind; }
VJ* __vyrn_vj_get(VJ* o, const char* key) {
    unsigned long long i;
    for (i = 0; i < o->nmem; i++) if (strcmp(o->mem[i].key, key) == 0) return o->mem[i].val;
    return 0;
}
int __vyrn_vj_bool_get(VJ* v) { return v->bval; }
long long __vyrn_vj_len(VJ* a) { return (long long)a->nitems; }
VJ* __vyrn_vj_at(VJ* a, long long i) { return a->items[i]; }
/* An array element, or a fresh `null` node when `i` is out of range — the
   RFC-0024 tuple-payload decode treats a short array's missing slots as null. */
VJ* __vyrn_vj_at_or_null(VJ* a, long long i) {
    if (i < 0 || (unsigned long long)i >= a->nitems) return __vyrn_vj_null();
    return a->items[i];
}
/* Object member count / i-th key / i-th value — the RFC-0024 payload-enum
   decode enforces "exactly one key" and reads it back. */
long long __vyrn_vj_obj_len(VJ* o) { return (long long)o->nmem; }
const char* __vyrn_vj_obj_key(VJ* o, long long i) { return o->mem[i].key; }
VJ* __vyrn_vj_obj_at(VJ* o, long long i) { return o->mem[i].val; }
const char* __vyrn_vj_str_get(VJ* v) { return v->text; }
/* Parse a number node into an integer target: 0 ok (*out set), 1 rejected
   (non-integer syntax, or out of range for the width/signedness). */
int __vyrn_vj_asint(VJ* v, int bits, int is_signed, long long* out) {
    if (v->kind != VJ_NUM || !v->is_int) return 1;
    char* end;
    if (is_signed) {
        errno = 0;
        long long x = strtoll(v->text, &end, 10);
        if (errno != 0 || *end != 0) return 1;
        if (bits < 64) {
            long long mx = (1LL << (bits - 1)) - 1;
            long long mn = -(1LL << (bits - 1));
            if (x < mn || x > mx) return 1;
        }
        *out = x;
        return 0;
    }
    if (v->text[0] == '-') return 1;
    errno = 0;
    unsigned long long x = strtoull(v->text, &end, 10);
    if (errno != 0 || *end != 0) return 1;
    if (bits < 64) {
        unsigned long long mx = (1ULL << bits) - 1ULL;
        if (x > mx) return 1;
    }
    *out = (long long)x;
    return 0;
}
double __vyrn_vj_asfloat(VJ* v) { return strtod(v->text, 0); }
const char* __vyrn_vj_kindname(int kind) {
    switch (kind) {
        case VJ_NULL: return "null";
        case VJ_BOOL: return "boolean";
        case VJ_NUM: return "number";
        case VJ_STR: return "string";
        case VJ_ARR: return "array";
        default: return "object";
    }
}
/* `expected <what>, found <kind>` — the runtime half of a `json.type` Issue. */
char* __vyrn_json_type_msg(const char* expected, int kind) {
    const char* found = __vyrn_vj_kindname(kind);
    unsigned long long n = strlen("expected , found ") + strlen(expected) + strlen(found) + 1;
    char* r = (char*)__vyrn_malloc(n);
    __vyrn_snprintf(r, n, "expected %s, found %s", expected, found);
    return r;
}
char* __vyrn_json_field_path(const char* parent, const char* field) {
    if (parent[0] == 0) return __vyrn_dup(field);
    unsigned long long n = strlen(parent) + 1 + strlen(field) + 1;
    char* r = (char*)__vyrn_malloc(n);
    __vyrn_snprintf(r, n, "%s.%s", parent, field);
    return r;
}
char* __vyrn_json_index_path(const char* parent, long long i) {
    unsigned long long n = strlen(parent) + 2 + 24 + 1;
    char* r = (char*)__vyrn_malloc(n);
    __vyrn_snprintf(r, n, "%s[%lld]", parent, i);
    return r;
}
typedef struct { char* key; char* path; char* message; } VIssue;
typedef struct { VIssue* items; unsigned long long n, cap; } VIssues;
VIssues* __vyrn_issues_new(void) {
    VIssues* s = (VIssues*)__vyrn_malloc(sizeof(VIssues));
    s->items = 0; s->n = 0; s->cap = 0; return s;
}
void __vyrn_issues_push(VIssues* s, const char* key, const char* path, const char* message) {
    if (s->n + 1 > s->cap) {
        s->cap = s->cap ? s->cap * 2 : 4;
        s->items = (VIssue*)__vyrn_realloc(s->items, s->cap * sizeof(VIssue));
    }
    s->items[s->n].key = __vyrn_dup(key);
    s->items[s->n].path = __vyrn_dup(path);
    s->items[s->n].message = __vyrn_dup(message);
    s->n++;
}
long long __vyrn_issues_len(VIssues* s) { return (long long)s->n; }
const char* __vyrn_issue_key(VIssues* s, long long i) { return s->items[i].key; }
const char* __vyrn_issue_path(VIssues* s, long long i) { return s->items[i].path; }
const char* __vyrn_issue_msg(VIssues* s, long long i) { return s->items[i].message; }

/* ---- worker threads (RFC-0025) ------------------------------------------ */
/* `spawn f(args)` lowers to __vyrn_spawn(thunk, frame): the IR packs the
   already-evaluated arguments (behind a leading result slot) into a heap frame
   and passes a per-callee thunk that loads them, calls the isolated task
   function, and stores the result back into the frame. The task is isolated
   (checker-enforced, transitively: no I/O, no module state, no shared cells,
   no `drop`), so ANY schedule produces byte-identical program output — the
   threads below are pure wall-clock optimization. `t.join()` lowers to
   __vyrn_join: block until completion, return the frame (the IR loads the
   result from its leading slot).

   One shared IR, three behaviors, all byte-identical:
     - native: a real OS thread per task (Win32 / pthreads);
     - VYRN_SEQUENTIAL_SPAWN=1 (native): the thunk runs inline at the spawn
       point — the old eager path, a debugging escape hatch;
     - wasm (__wasi__): no threads exist; the thunk always runs inline.

   Locked trap protocol: a trapping task performs the standard trap protocol
   itself (one fputs of the canonical `error: ...` line to stderr, then
   exit(1)) from whichever thread it runs on — same wording, same exit code,
   printed once; exit() flushes stdout so no output is lost. Tasks that were
   never joined are joined at process exit (below, in spawn order): the eager
   semantics ran every task, so a trap in a leaked task must not be lost.

   Ownership: task records and frames are never freed — a task may be joined
   more than once (join is idempotent), and the count is bounded by the number
   of spawns (the "unproven ownership leaks, which is always safe" rule). */
#if defined(__wasi__)
typedef struct VTask { void* frame; } VTask;
void* __vyrn_spawn(void (*thunk)(void*), void* frame) {
    VTask* t = (VTask*)__vyrn_malloc(sizeof(VTask));
    t->frame = frame;
    thunk(frame); /* eager: single-threaded target */
    return t;
}
void* __vyrn_join(void* task) { return ((VTask*)task)->frame; }
static void __vyrn_join_all(void) {}
#else
#ifdef _WIN32
#include <windows.h>
typedef struct VTask {
    void (*thunk)(void*);
    void* frame;
    HANDLE done; /* manual-reset event, signaled when the task completed */
    struct VTask* next;
} VTask;
static DWORD WINAPI __vyrn_task_main(LPVOID p) {
    VTask* t = (VTask*)p;
    t->thunk(t->frame);
    SetEvent(t->done);
    return 0;
}
static SRWLOCK __vyrn_task_lock = SRWLOCK_INIT;
static void __vyrn_tasks_acquire(void) { AcquireSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_tasks_release(void) { ReleaseSRWLockExclusive(&__vyrn_task_lock); }
static void __vyrn_task_wait(VTask* t) { WaitForSingleObject(t->done, INFINITE); }
#else
#include <pthread.h>
typedef struct VTask {
    void (*thunk)(void*);
    void* frame;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int done;
    struct VTask* next;
} VTask;
static void* __vyrn_task_main(void* p) {
    VTask* t = (VTask*)p;
    t->thunk(t->frame);
    pthread_mutex_lock(&t->mu);
    t->done = 1;
    pthread_cond_broadcast(&t->cv);
    pthread_mutex_unlock(&t->mu);
    return 0;
}
static pthread_mutex_t __vyrn_task_lock = PTHREAD_MUTEX_INITIALIZER;
static void __vyrn_tasks_acquire(void) { pthread_mutex_lock(&__vyrn_task_lock); }
static void __vyrn_tasks_release(void) { pthread_mutex_unlock(&__vyrn_task_lock); }
static void __vyrn_task_wait(VTask* t) {
    pthread_mutex_lock(&t->mu);
    while (!t->done) pthread_cond_wait(&t->cv, &t->mu);
    pthread_mutex_unlock(&t->mu);
}
#endif
/* Registry of every spawned task, appended in spawn order (a task may itself
   spawn — the list is append-only under the lock, so the exit-time walk below
   observes children its waits allowed to be registered). */
static VTask* __vyrn_task_head = 0;
static VTask* __vyrn_task_tail = 0;

void* __vyrn_spawn(void (*thunk)(void*), void* frame) {
    int started = 0;
    VTask* t = (VTask*)__vyrn_malloc(sizeof(VTask));
    t->thunk = thunk;
    t->frame = frame;
    t->next = 0;
#ifdef _WIN32
    t->done = CreateEvent(0, TRUE, FALSE, 0);
#else
    pthread_mutex_init(&t->mu, 0);
    pthread_cond_init(&t->cv, 0);
    t->done = 0;
#endif
    {
        const char* seq = getenv("VYRN_SEQUENTIAL_SPAWN");
        if (!(seq && seq[0] == '1' && seq[1] == 0)) {
#ifdef _WIN32
            HANDLE th = CreateThread(0, 0, __vyrn_task_main, t, 0, 0);
            if (th != 0) { CloseHandle(th); started = 1; } /* completion is t->done */
#else
            pthread_t th;
            pthread_attr_t at;
            pthread_attr_init(&at);
            pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
            started = (pthread_create(&th, &at, __vyrn_task_main, t) == 0);
            pthread_attr_destroy(&at);
#endif
        }
    }
    if (!started) {
        /* sequential mode, or thread creation failed: the eager path (run at
           the spawn point, on this thread) — the same bytes, by isolation. */
        __vyrn_task_main(t);
    }
    __vyrn_tasks_acquire();
    if (__vyrn_task_tail) __vyrn_task_tail->next = t; else __vyrn_task_head = t;
    __vyrn_task_tail = t;
    __vyrn_tasks_release();
    return t;
}

void* __vyrn_join(void* task) {
    VTask* t = (VTask*)task;
    __vyrn_task_wait(t); /* idempotent; safe from any number of joiners */
    return t->frame;
}

/* Join every task that is still outstanding when the program returns from
   `main` — under eager semantics every spawned task ran, so a leaked task's
   work (and, if it traps, its canonical trap + exit(1)) must still happen. */
static void __vyrn_join_all(void) {
    __vyrn_tasks_acquire();
    VTask* t = __vyrn_task_head;
    __vyrn_tasks_release();
    while (t) {
        __vyrn_task_wait(t);
        __vyrn_tasks_acquire();
        t = t->next;
        __vyrn_tasks_release();
    }
}
#endif

/* The real C entry point: every target's crt (MSVC, glibc, wasi-libc) knows
   how to call a plain C main; the IR only exports vyrn_entry. argv is stashed
   for `args()` (RFC-0014). Outstanding tasks are joined before the exit code
   is returned (RFC-0025). */
extern int vyrn_entry(void);
int main(int argc, char** argv) {
    __vyrn_argc = argc;
    __vyrn_argv = argv;
    int code = vyrn_entry();
    __vyrn_join_all();
    return code;
}
"#;

/// C trap stubs for the program's `extern` imports (RFC-0012), one per `extern
/// fn`, appended to the shim on the **native** target only. Each defines the
/// import symbol (`__vyrn_extern_<name>`, matching codegen) as a function that
/// prints the canonical trap and exits — so a native binary that reaches an
/// `extern` call behaves exactly like the interpreter (`error: extern \`name\`
/// is not available on this target`), rather than failing to link. The declared
/// `(void)` signature is intentional: the stub never returns (it `exit`s), so
/// the caller's argument/return registers are never observed.
fn extern_trap_stubs(program: &vyrn_frontend::ast::Program) -> String {
    let mut s = String::new();
    for f in program
        .functions
        .iter()
        // RFC-0043 host-boundary externs (time/random) have REAL implementations
        // in RUNTIME_SHIM on every target, so they get no trap stub.
        .filter(|f| f.is_extern && vyrn_codegen::host_boundary_extern(&f.name).is_none())
    {
        // `f.name` is a Vyrn identifier (alphanumeric + `_`), safe to inline
        // into both a C symbol and a C string literal.
        s.push_str(&format!(
            "void __vyrn_extern_{name}(void) {{ \
             fputs(\"error: extern `{name}` is not available on this target\\n\", stderr); \
             exit(1); }}\n",
            name = f.name
        ));
    }
    s
}

/// `vyrn build <file.vyrn> [-o out] [--target wasm]` — emit IR, then invoke
/// clang to link a native executable (or a `wasm32-wasi` module).
/// `vyrn test [file] [--name <substring>]` (RFC-0015) — load + check the root
/// file, then run its `test` blocks under the interpreter in declaration order.
/// Prints `test "name" ... ok` / `... FAILED: <message>` per test and a
/// `N passed, M failed` summary; exits 1 if any test failed. A file with no
/// tests prints `no tests` and exits 0.
fn test_cmd(path: &str, rest: &[String]) -> ExitCode {
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
    // A file with no root-module tests: nothing to run.
    let has_tests = program.tests.iter().any(|t| t.module.is_none());
    if !has_tests {
        println!("no tests");
        return ExitCode::SUCCESS;
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
fn bench_cmd(path: &str, rest: &[String]) -> ExitCode {
    let mut filter: Option<String> = None;
    let mut check = false;
    let mut json = false;
    let mut compare: Option<String> = None;
    let mut threshold: f64 = 1.5;
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

    if check {
        return bench_check(&program, filter.as_deref());
    }
    if let Some(baseline) = compare {
        return bench_compare(path, program, filter.as_deref(), &baseline, threshold);
    }
    // `--json` streams the machine-readable report; the default streams the human
    // report. Neither captures the child's stdout.
    let (code, _) = bench_native(path, program, filter.as_deref(), json, false);
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
    mut program: vyrn_frontend::ast::Program,
    filter: Option<&str>,
    json: bool,
    capture: bool,
) -> (ExitCode, Option<String>) {
    use vyrn_frontend::ast::{Block, Expr, Function, Stmt, Type};

    // 1. Pull in the harness runtime (`std/bench` + its transitive `std/time`) by
    //    loading a synthetic root that imports it, then merge every module decl it
    //    brought (module-tagged, so the synthetic root's own dummy `main` is left
    //    out) into the user's program — skipping any name the program already has.
    // Importing `benchOne` loads the whole `std/bench` module, so its `benchMeasure`
    // / `benchJson` / `BenchResult` (and their transitive `std/time` + `std/json`)
    // are merged too — the `--json` harness needs them.
    let runtime_src = "import { benchOne } from \"std/bench\"\nfn main() -> Int64 { return 0 }\n";
    let rt = match load_program(path, runtime_src) {
        Ok(p) => p,
        Err(code) => return (code, None),
    };
    let have_fn: std::collections::HashSet<String> =
        program.functions.iter().map(|f| f.name.clone()).collect();
    for f in rt.functions {
        if f.module.is_some() && !have_fn.contains(&f.name) {
            program.functions.push(f);
        }
    }
    let have_ty: std::collections::HashSet<String> =
        program.type_decls.iter().map(|t| t.name.clone()).collect();
    for t in rt.type_decls {
        if t.module.is_some() && !have_ty.contains(&t.name) {
            program.type_decls.push(t);
        }
    }
    let have_pr: std::collections::HashSet<String> =
        program.protocols.iter().map(|p| p.name.clone()).collect();
    for pr in rt.protocols {
        if pr.module.is_some() && !have_pr.contains(&pr.name) {
            program.protocols.push(pr);
        }
    }
    // Impls carry no module tag; dedup by (protocol, implementing type). In
    // practice the harness runtime defines none, so this loop is empty.
    let have_im: std::collections::HashSet<(String, String)> = program
        .impls
        .iter()
        .map(|im| (im.protocol.clone(), im.ty.to_string()))
        .collect();
    for im in rt.impls {
        if !have_im.contains(&(im.protocol.clone(), im.ty.to_string())) {
            program.impls.push(im);
        }
    }
    let have_g: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    for g in rt.globals {
        if g.module.is_some() && !have_g.contains(&g.name) {
            program.globals.push(g);
        }
    }

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
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        });
        let body_ref = Expr::Var { name: format!("__vyrn_bench_body_{slot}"), line: 0 };
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
                    Expr::ArrayLit { elems: measure_calls, line: 0 },
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
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
    });
    // Benches/tests are now either lifted or irrelevant — drop them so nothing
    // downstream mistakes them for live code.
    program.benches.clear();
    program.tests.clear();

    // 4. Emit IR + shim, compile native via clang into a temp dir, and run it.
    let ir = match vyrn_codegen::emit(&program) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error: {e}");
            return (ExitCode::FAILURE, None);
        }
    };
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("bench");
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
        return (ExitCode::FAILURE, None);
    }
    let mut shim = RUNTIME_SHIM.to_string();
    shim.push_str(&extern_trap_stubs(&program));
    if let Err(e) = std::fs::write(&shim_path, &shim) {
        eprintln!("error: cannot write {}: {e}", shim_path.display());
        return (ExitCode::FAILURE, None);
    }
    let clang = match find_clang() {
        Some(c) => c,
        None => {
            eprintln!(
                "error: could not find `clang`. Install LLVM and put clang on PATH, \
                 or set the CLANG environment variable to its full path."
            );
            return (ExitCode::FAILURE, None);
        }
    };
    let mut cmd = Command::new(&clang);
    cmd.arg(&ll_path)
        .arg(&shim_path)
        .arg("-O2")
        .arg("-o")
        .arg(&out_path)
        .arg("-Wno-override-module");
    if !cfg!(windows) {
        cmd.arg("-pthread");
    }
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("error: clang exited with {s}");
            return (ExitCode::FAILURE, None);
        }
        Err(e) => {
            eprintln!("error: failed to run clang ({}): {e}", clang.display());
            return (ExitCode::FAILURE, None);
        }
    }
    // Run the compiled harness. When `capture` is set (`--compare`), grab its
    // stdout as the JSON report to feed the comparator; otherwise let stdout and
    // stderr stream straight through (the `--json` and human paths both print live).
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
                eprintln!("error: failed to run bench binary ({}): {e}", out_path.display());
                let _ = std::fs::remove_dir_all(&dir);
                return (ExitCode::FAILURE, None);
            }
        }
    } else {
        match Command::new(&out_path).status() {
            Ok(s) => ((s.code().unwrap_or(1) & 0xff) as u8, None),
            Err(e) => {
                eprintln!("error: failed to run bench binary ({}): {e}", out_path.display());
                let _ = std::fs::remove_dir_all(&dir);
                return (ExitCode::FAILURE, None);
            }
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    (ExitCode::from(code), out)
}

/// The per-bench minimum times (name → minNs) extracted from a `--json` report or
/// a `bench/baseline.json` baseline. Declaration order is preserved (a `Vec`, not a
/// map) so the comparison prints in the run's order. Returns `None` if `doc` is not
/// the expected `{ benches: [ { name, minNs } ] }` shape.
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
    program: vyrn_frontend::ast::Program,
    filter: Option<&str>,
    baseline_path: &str,
    threshold: f64,
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
    let (run_code, captured) = bench_native(path, program, filter, true, true);
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

    let (verdicts, regressed) = bench_verdicts(&run, &baseline, threshold);
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
    /// Within threshold.
    Ok,
    /// Slower than `baselineMin * threshold`; the factor is `min / baselineMin`.
    Regressed(f64),
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
            Verdict::New => "new".to_string(),
            Verdict::MissingFromRun => "missing-from-run".to_string(),
        }
    }
}

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
) -> (Vec<(String, Verdict)>, usize) {
    let lookup = |name: &str| baseline.iter().find(|(n, _)| n == name).map(|(_, m)| *m);
    let mut out = Vec::new();
    let mut regressed = 0usize;
    for (name, min) in run {
        let v = match lookup(name) {
            Some(base) if base > 0.0 => {
                let factor = min / base;
                if *min > base * threshold {
                    regressed += 1;
                    Verdict::Regressed(factor)
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
        eprintln!(
            "error: `vyrn serve` needs `fn handle(req: Request) -> Response` in {path}"
        );
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
                eprintln!("serving {file_label} on http://localhost:{actual_port} with {n} workers");
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
    let (chain, global) =
        vyrn_frontend::checker::module_state_use(program, "handle", &stored)?;
    let path = chain.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(" -> ");
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
    let Some(manifest) = find_manifest(&cwd) else {
        eprintln!("error: `vyrn dev` needs a vyrn.json with `server` and `client` keys");
        return ExitCode::FAILURE;
    };
    let manifest_path = Path::new(&manifest.dir).join("vyrn.json");
    let text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let doc = match vyrn_frontend::schema::parse_json(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: vyrn.json is not valid JSON: {e}");
            return ExitCode::FAILURE;
        }
    };
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
        &["--target".to_string(), "wasm".to_string(), "-o".to_string(), wasm_out.to_string_lossy().into_owned()],
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
    use vyrn_frontend::ast::Type;
    let has_handle = program.functions.iter().any(|f| {
        f.name == "handle"
            && !f.is_extern
            && f.params.len() == 1
            && f.params[0].ty == Type::Named("Request".to_string())
            && f.ret == Type::Named("Response".to_string())
    });
    if !has_handle {
        eprintln!("error: the server root `{server_rel}` needs `fn handle(req: Request) -> Response`");
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
    let assets = DevAssets { public_dir, web_dir, wasm: wasm_out };

    let banner = move |assets: &DevAssets| {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        eprintln!("dev: serving {server_rel} on http://localhost:{actual_port}");
        eprintln!("dev:   /rpc/*         -> server `handle` (rpcHandle + your pages)");
        eprintln!("dev:   /client.wasm   -> built from {client_rel}");
        eprintln!("dev:   /vyrn-runtime/ -> web runtimes (wasi-min.js, vyrn-rpc.js, vyrn-query.js)");
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
/// no static asset matches (so the request falls through to `handle`). Rejects
/// any path containing a `..` segment (no traversal out of a root).
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
            let p = Path::new(&assets.web_dir).join(name);
            return p.is_file().then_some(p);
        }
        return None;
    }
    // Public dir: `/` → index.html, else the path under public/.
    let rel = if raw == "/" { "index.html" } else { raw.trim_start_matches('/') };
    let p = assets.public_dir.join(rel);
    p.is_file().then_some(p)
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
        vyrn_frontend::interp::ServeRequest,
    ) -> Result<vyrn_frontend::interp::ServeResponse, String>,
) {
    let req = match parse_request(stream) {
        Ok(r) => r,
        Err(ParseError::Chunked { method, path }) => {
            eprintln!("{method} {path} -> 501");
            write_response(stream, 501, "text/plain", b"chunked transfer-encoding not supported");
            return;
        }
        Err(ParseError::Bad) => {
            eprintln!("- - -> 400");
            write_response(stream, 400, "text/plain", b"bad request");
            return;
        }
    };
    // Static assets: GET (or HEAD) only, so nothing shadows a POST /rpc/*.
    if req.method == "GET" || req.method == "HEAD" {
        if let Some(file) = dev_static_path(&req.path, assets) {
            match std::fs::read(&file) {
                Ok(bytes) => {
                    eprintln!("{} {} -> 200 (static)", req.method, req.path);
                    write_response(stream, 200, dev_content_type(&file), &bytes);
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
    match call_handle(req) {
        Ok(resp) => {
            eprintln!("{method} {path} -> {}", resp.status);
            write_response(stream, resp.status, &resp.content_type, resp.body.as_bytes());
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!("{method} {path} -> 500");
            write_response(stream, 500, "text/plain", b"internal error");
        }
    }
}

/// Why a request never reached Vyrn.
enum ParseError {
    /// Malformed request line/headers → 400.
    Bad,
    /// A `Transfer-Encoding: chunked` body (unsupported in v1) → 501. Carries
    /// the parsed method/path so the access line can still be logged.
    Chunked { method: String, path: String },
}

/// Handle one connection: parse the request, call Vyrn's `handle`, write the
/// response, close. Malformed input answers 400 without reaching Vyrn; a chunked
/// body answers 501; a Vyrn trap is logged and answered 500 (the server keeps
/// running — one bad request must not kill it).
fn serve_one(
    stream: &mut std::net::TcpStream,
    call_handle: &mut dyn FnMut(
        vyrn_frontend::interp::ServeRequest,
    ) -> Result<vyrn_frontend::interp::ServeResponse, String>,
) {
    match parse_request(stream) {
        Ok(req) => {
            let method = req.method.clone();
            let path = req.path.clone();
            match call_handle(req) {
                Ok(resp) => {
                    eprintln!("{method} {path} -> {}", resp.status);
                    write_response(stream, resp.status, &resp.content_type, resp.body.as_bytes());
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
            write_response(stream, 501, "text/plain", b"chunked transfer-encoding not supported");
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
    let method = parts.next().filter(|s| !s.is_empty()).ok_or(ParseError::Bad)?.to_string();
    let target = parts.next().filter(|s| !s.is_empty()).ok_or(ParseError::Bad)?.to_string();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return Err(ParseError::Bad);
    }

    // Headers: `name: value`, name compared case-insensitively.
    let mut content_length: usize = 0;
    let mut chunked = false;
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
    }
    if chunked {
        return Err(ParseError::Chunked { method, path: target });
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

    Ok(vyrn_frontend::interp::ServeRequest { method, path: target, body })
}

/// A minimal status-code → reason-phrase table. Unknown codes get an empty
/// reason (the space after the code is still required by the grammar).
fn reason_phrase(status: i64) -> &'static str {
    match status {
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

/// Write one HTTP/1.1 response: status line, `Content-Type`, `Content-Length`,
/// `Connection: close`, blank line, body. Errors are ignored — the peer may
/// have hung up, and one dropped connection must not fault the server.
fn write_response(stream: &mut std::net::TcpStream, status: i64, content_type: &str, body: &[u8]) {
    use std::io::Write;
    let reason = reason_phrase(status);
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// The dev-tree wasi sysroot, if one exists: the first `tools/wasi-sysroot-*`
/// directory found walking up from `start` (sorted, so the pick is
/// deterministic when several versions are unpacked side by side).
fn tools_wasi_sysroot_from(start: &Path) -> Option<std::path::PathBuf> {
    for dir in start.ancestors() {
        let tools = dir.join("tools");
        if !tools.is_dir() {
            continue;
        }
        let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(&tools)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("wasi-sysroot"))
            })
            .collect();
        hits.sort();
        if let Some(hit) = hits.into_iter().next() {
            return Some(hit);
        }
    }
    None
}

/// Auto-discovered wasi sysroot for the running exe (see
/// [`tools_wasi_sysroot_from`]); `None` when no `tools/` convention applies.
fn discovered_wasi_sysroot() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    tools_wasi_sysroot_from(exe.parent()?)
}

/// `libclang_rt.builtins-wasm32.a` from a `libclang_rt.builtins-wasm32-wasi-*`
/// directory next to the sysroot (the wasi-sdk release-artifact layout),
/// version-agnostic and deterministic (sorted).
fn builtins_near_sysroot(sysroot: &Path) -> Option<std::path::PathBuf> {
    let parent = sysroot.parent()?;
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path().join("libclang_rt.builtins-wasm32.a"))
        .filter(|p| {
            p.exists()
                && p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("libclang_rt.builtins-wasm32"))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

fn build(path: &str, rest: &[String]) -> ExitCode {
    // parse optional `-o <out>` / `--target wasm`
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
    let ir = match vyrn_codegen::emit(&program) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // default output name: <stem> (+ .exe on Windows, .wasm for wasm)
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("a");
    let out_path = out.unwrap_or_else(|| {
        if wasm {
            format!("{stem}.wasm")
        } else if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    });

    // write IR + the portable stream shim next to the output so failures are
    // inspectable
    let ll_path = PathBuf::from(&out_path).with_extension("ll");
    if let Err(e) = std::fs::write(&ll_path, ir) {
        eprintln!("error: cannot write {}: {e}", ll_path.display());
        return ExitCode::FAILURE;
    }
    // The portable shim, plus (native only) a trap stub per `extern` import
    // (RFC-0012). On wasm the stubs are OMITTED so each `extern` resolves to the
    // host page's `vyrn` import namespace; on native there is no host, so the
    // stub satisfies the symbol by printing the canonical "not available on this
    // target" message and exiting — the same wording the interpreter traps with.
    let mut shim = RUNTIME_SHIM.to_string();
    if !wasm {
        shim.push_str(&extern_trap_stubs(&program));
    }
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
    cmd.arg(&ll_path)
        .arg(&shim_path)
        .arg("-o")
        .arg(&out_path)
        // our IR carries no target triple; clang supplies the target's — don't warn.
        .arg("-Wno-override-module");
    if !wasm && !cfg!(windows) {
        // Worker threads (RFC-0025): pthreads. Win32 threads need no flag and
        // wasm builds get the shim's inline (sequential) path instead.
        cmd.arg("-pthread");
    }
    if wasm {
        // wasm32-wasi: the same IR, compiled against wasi-libc. The sysroot
        // comes from $WASI_SYSROOT (a wasi-sdk checkout's `share/wasi-sysroot`),
        // else is auto-discovered from the dev-tree convention: a `tools/`
        // directory holding `wasi-sysroot-*` in an ancestor of the running exe
        // (`<repo>/tools/…` with vyrn.exe at `<repo>/compiler/target/<p>/`).
        let sysroot = match std::env::var("WASI_SYSROOT") {
            Ok(s) if Path::new(&s).exists() => s,
            _ => match discovered_wasi_sysroot() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => {
                    eprintln!(
                        "error: `--target wasm` needs the wasi-libc sysroot. Download wasi-sdk \
                         (github.com/WebAssembly/wasi-sdk, or just its wasi-sysroot artifact) \
                         and set WASI_SYSROOT to its wasi-sysroot directory, or unpack it \
                         under <repo>/tools/."
                    );
                    return ExitCode::FAILURE;
                }
            },
        };
        cmd.arg("--target=wasm32-wasip1").arg(format!("--sysroot={sysroot}"));
        // clang's own wasm32 compiler-rt builtins are not bundled with the
        // Windows LLVM installer; wasi-sdk ships them as a separate archive.
        // Accept it via $WASI_BUILTINS (path to libclang_rt.builtins-wasm32.a)
        // or find a `libclang_rt.builtins-wasm32-wasi-*` dir next to the sysroot.
        let builtins = std::env::var("WASI_BUILTINS")
            .ok()
            .filter(|b| Path::new(b).exists())
            .or_else(|| {
                let near = builtins_near_sysroot(Path::new(&sysroot));
                near.map(|p| p.to_string_lossy().into_owned())
            });
        match builtins {
            Some(b) => {
                cmd.arg("-nodefaultlibs").arg(&b).arg("-lc");
            }
            None => {
                eprintln!(
                    "error: wasm builtins not found — set WASI_BUILTINS to \
                     libclang_rt.builtins-wasm32.a (from the wasi-sdk release artifact \
                     libclang_rt.builtins-wasm32-wasi-*.tar.gz), or unpack that archive \
                     next to the sysroot under <repo>/tools/."
                );
                return ExitCode::FAILURE;
            }
        }
        // `export extern fn` (RFC-0012 M2) — the functions themselves export via
        // their `wasm-export-name` attribute (a GC root, no flag needed). But if
        // any takes a `String` parameter, the JS shim must allocate the argument
        // buffer inside the module before calling in, so the module's own
        // allocator has to be reachable. `__vyrn_malloc` lives in the C shim (no
        // IR attribute to hang off), so force-export it with a linker flag.
        let needs_malloc_export = program.functions.iter().any(|f| {
            f.is_export_extern
                && f.params.iter().any(|p| matches!(p.ty, vyrn_frontend::ast::Type::Str))
        });
        if needs_malloc_export {
            cmd.arg("-Wl,--export=__vyrn_malloc");
        }
    }
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

/// Locate a clang executable: `$CLANG`, then PATH, then the default Windows
/// install location.
fn find_clang() -> Option<PathBuf> {
    if let Ok(c) = std::env::var("CLANG") {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    // Trust PATH: if `clang --version` runs, use the bare name.
    if Command::new("clang")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("clang"));
    }
    if cfg!(windows) {
        let default = PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe");
        if default.exists() {
            return Some(default);
        }
    }
    None
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

    #[test]
    fn within_threshold_is_ok() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5);
        assert_eq!(v, vec![("a".to_string(), Verdict::Ok)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn exactly_at_threshold_is_ok_not_regressed() {
        // min == baseline * threshold is NOT a regression (strict `>`).
        let run = table(&[("a", 150.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5);
        assert_eq!(v, vec![("a".to_string(), Verdict::Ok)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn beyond_threshold_regresses_with_the_factor() {
        let run = table(&[("a", 250.0)]);
        let base = table(&[("a", 100.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5);
        assert_eq!(v, vec![("a".to_string(), Verdict::Regressed(2.5))]);
        assert_eq!(regressed, 1);
    }

    #[test]
    fn threshold_arithmetic_uses_the_supplied_factor() {
        // Same 2x slowdown: a regression at 1.5, ok at 3.0.
        let run = table(&[("a", 200.0)]);
        let base = table(&[("a", 100.0)]);
        assert_eq!(bench_verdicts(&run, &base, 1.5).1, 1);
        assert_eq!(bench_verdicts(&run, &base, 3.0).1, 0);
    }

    #[test]
    fn a_run_bench_absent_from_baseline_is_new() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5);
        assert_eq!(v, vec![("a".to_string(), Verdict::New)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn a_baseline_bench_absent_from_run_is_missing_from_run() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 100.0), ("ghost", 100.0)]);
        let (v, _) = bench_verdicts(&run, &base, 1.5);
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
        let (v, _) = bench_verdicts(&run, &base, 1.5);
        let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn zero_baseline_min_is_new_not_a_division_by_zero() {
        let run = table(&[("a", 100.0)]);
        let base = table(&[("a", 0.0)]);
        let (v, regressed) = bench_verdicts(&run, &base, 1.5);
        assert_eq!(v, vec![("a".to_string(), Verdict::New)]);
        assert_eq!(regressed, 0);
    }

    #[test]
    fn placeholder_baseline_is_detected() {
        let flagged = vyrn_frontend::schema::parse_json(
            r#"{"placeholder":true,"benches":[]}"#,
        )
        .unwrap();
        assert!(baseline_is_placeholder(&flagged));
        let empty = vyrn_frontend::schema::parse_json(r#"{"benches":[]}"#).unwrap();
        assert!(baseline_is_placeholder(&empty));
        let real = vyrn_frontend::schema::parse_json(
            r#"{"benches":[{"name":"a","minNs":10}]}"#,
        )
        .unwrap();
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

    /// The dev-tree toolchain discovery: `tools/wasi-sysroot-*` found from any
    /// ancestor of the starting dir, builtins found version-agnostically next
    /// to the sysroot, and both absent on a layout without the convention.
    #[test]
    fn wasi_toolchain_discovery_walks_the_tools_convention() {
        let root = std::env::temp_dir()
            .join(format!("vyrn_tools_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sysroot = root.join("tools/wasi-sysroot-25.0");
        let builtins_dir = root.join("tools/libclang_rt.builtins-wasm32-wasi-25.0");
        let deep = root.join("compiler/target/release");
        std::fs::create_dir_all(&sysroot).unwrap();
        std::fs::create_dir_all(&builtins_dir).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(builtins_dir.join("libclang_rt.builtins-wasm32.a"), b"x").unwrap();

        let found = tools_wasi_sysroot_from(&deep).expect("sysroot discovered from exe dir");
        assert_eq!(found, sysroot);
        let b = builtins_near_sysroot(&found).expect("builtins discovered next to sysroot");
        assert!(b.ends_with("libclang_rt.builtins-wasm32.a"));

        // No convention → no discovery (never invent a path).
        let bare = root.join("elsewhere/deeper");
        std::fs::create_dir_all(&bare).unwrap();
        let _ = std::fs::remove_dir_all(root.join("tools"));
        assert!(tools_wasi_sysroot_from(&bare).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
