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
//!   vyrn serve   [file.vyrn] [--port N] Run `fn handle(req: Request) -> Response` as an HTTP host.
//!   vyrn new     <name>                 Scaffold a project (vyrn.json + src/main.vyrn).
//!   vyrn deps                           Print the resolved module graph.
//!
//! The file argument is optional whenever a `vyrn.json` manifest (found by
//! walking up from the current directory) declares a `"main"`. The manifest's
//! `"dependencies"` map bare import specifiers to real ones.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod remote;

const USAGE: &str = "usage: vyrn <run|check|emit-ir|emit-gen|build|test|serve|fmt> [file.vyrn] [-o out] [--target wasm] [--offline]\n       vyrn run [file.vyrn] [args...]   (trailing args reach the program's args())\n       vyrn test [file.vyrn] [--name <substring>]\n       vyrn serve [file.vyrn] [--port N]   (HTTP host; needs `fn handle(req: Request) -> Response`)\n       vyrn fmt [file.vyrn ...] [--check]   (canonical formatter; no files = project main + local imports)\n       vyrn new <name> | vyrn add <specifier> [--name alias] | vyrn update [alias] | vyrn vendor [--check] | vyrn deps";

/// `--offline` flag or `VYRN_OFFLINE=1`: never touch the network; a lock+cache
/// miss is a hard error instead.
fn offline(args: &[String]) -> bool {
    args.iter().any(|a| a == "--offline") || std::env::var("VYRN_OFFLINE").is_ok()
}

fn main() -> ExitCode {
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
            eprintln!("unknown command `{other}` (expected run, check, emit-ir, emit-gen, build, test, or serve)");
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
        // Normalize to LF for a stable comparison; the formatter emits LF and one
        // trailing newline. (Repo files are LF; a CRLF checkout still formats to LF.)
        let normalized = source.replace("\r\n", "\n");
        match vyrn_frontend::fmt(&normalized) {
            Ok(formatted) => {
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
            }
            Ok(vec![root_key])
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
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void* __vyrn_stderr(void) { return stderr; }
void* __vyrn_stdout(void) { return stdout; }

/* size_t-clean wrappers: the IR always passes/returns 64-bit sizes, so these
   adapt on ILP32 targets (wasm32) and are transparent on LP64/LLP64. */
unsigned long long __vyrn_strlen(const char* s) { return (unsigned long long)strlen(s); }

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

/* The real C entry point: every target's crt (MSVC, glibc, wasi-libc) knows
   how to call a plain C main; the IR only exports vyrn_entry. argv is stashed
   for `args()` (RFC-0014). */
extern int vyrn_entry(void);
int main(int argc, char** argv) {
    __vyrn_argc = argc;
    __vyrn_argv = argv;
    return vyrn_entry();
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
    for f in program.functions.iter().filter(|f| f.is_extern) {
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

/// `vyrn serve [file] [--port N]` (RFC-0016) — a hand-rolled HTTP/1.1 host on
/// `std::net` (no crates), running the file's `handle` under the interpreter.
/// Sequential accept loop, one request at a time: module state is race-free by
/// construction. Default port 8080.
fn serve_cmd(path: &str, rest: &[String]) -> ExitCode {
    // Optional `--port N` (default 8080).
    let mut port: u16 = 8080;
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
    if wasm {
        // wasm32-wasi: the same IR, compiled against wasi-libc. The sysroot
        // comes from $WASI_SYSROOT (a wasi-sdk checkout's `share/wasi-sysroot`).
        let sysroot = match std::env::var("WASI_SYSROOT") {
            Ok(s) if Path::new(&s).exists() => s,
            _ => {
                eprintln!(
                    "error: `--target wasm` needs the wasi-libc sysroot. Download                      wasi-sdk (github.com/WebAssembly/wasi-sdk, or just its                      wasi-sysroot artifact) and set WASI_SYSROOT to its                      wasi-sysroot directory."
                );
                return ExitCode::FAILURE;
            }
        };
        cmd.arg("--target=wasm32-wasip1").arg(format!("--sysroot={sysroot}"));
        // clang's own wasm32 compiler-rt builtins are not bundled with the
        // Windows LLVM installer; wasi-sdk ships them as a separate archive.
        // Accept it via $WASI_BUILTINS (path to libclang_rt.builtins-wasm32.a)
        // or find it next to the sysroot.
        let builtins = std::env::var("WASI_BUILTINS").ok().or_else(|| {
            let near = Path::new(&sysroot)
                .parent()
                .map(|p| p.join("libclang_rt.builtins-wasm32-wasi-25.0/libclang_rt.builtins-wasm32.a"));
            near.filter(|p| p.exists()).map(|p| p.to_string_lossy().into_owned())
        });
        match builtins {
            Some(b) => {
                cmd.arg("-nodefaultlibs").arg(&b).arg("-lc");
            }
            None => {
                eprintln!(
                    "error: wasm builtins not found — set WASI_BUILTINS to                      libclang_rt.builtins-wasm32.a (from the wasi-sdk release                      artifact libclang_rt.builtins-wasm32-wasi-*.tar.gz)."
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
