//! `velac` — the Vela driver.
//!
//! Usage:
//!   velac run     <file.vela>            Type-check and interpret; process exits with main's value.
//!   velac check   <file.vela>            Type-check only; print "ok" or every diagnostic.
//!   velac emit-ir <file.vela>            Print textual LLVM IR to stdout.
//!   velac build   <file.vela> [-o out]   Compile to a native executable via clang.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: velac <run|check|emit-ir|build> <file.vela> [-o out]");
        return ExitCode::from(2);
    }
    let (cmd, path) = (&args[1], &args[2]);

    if cmd == "build" {
        return build(path, &args[3..]);
    }

    if args.len() != 3 {
        eprintln!("usage: velac <run|check|emit-ir> <file.vela>");
        return ExitCode::from(2);
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    match cmd.as_str() {
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
            match vela_frontend::interp::run(&program) {
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
            match vela_codegen::emit(&program) {
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
        other => {
            eprintln!("unknown command `{other}` (expected run, check, emit-ir, or build)");
            ExitCode::from(2)
        }
    }
}

/// Filesystem module resolver for multi-file programs (RFC-0010): resolved
/// specifiers are normalized slash-paths relative to the root file.
struct FsResolver;

impl vela_frontend::loader::ModuleResolver for FsResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

/// The std-library root: `$VELA_STD`, or `std/` found by walking up from the
/// executable (dev builds live at `<repo>/compiler/target/<profile>/velac`,
/// so the repo's `std/` is a few levels up). `None` if not found — only an
/// error if a program actually imports `std/...`.
fn std_root() -> Option<String> {
    if let Ok(p) = std::env::var("VELA_STD") {
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

/// Load + check a root file through the module loader, printing diagnostics
/// (with their originating file) on failure.
fn load_program(path: &str, source: &str) -> Result<vela_frontend::ast::Program, ExitCode> {
    // Strip Windows' verbatim prefix (`\\?\C:\..`) — it survives neither the
    // slash normalization nor readable diagnostics.
    let root_key = path.trim_start_matches(r"\\?\").replace('\\', "/");
    match vela_frontend::load(source, &root_key, std_root().as_deref(), &FsResolver) {
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

/// The portable half of the runtime: `stderr`/`stdout` are C macros with no
/// linkable symbol, so the emitted IR calls these two functions instead. The
/// shim is compiled by clang next to the IR on every target — MSVC, glibc,
/// and wasi-libc alike.
const RUNTIME_SHIM: &str = r#"
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void* __vela_stderr(void) { return stderr; }
void* __vela_stdout(void) { return stdout; }

/* size_t-clean wrappers: the IR always passes/returns 64-bit sizes, so these
   adapt on ILP32 targets (wasm32) and are transparent on LP64/LLP64. */
unsigned long long __vela_strlen(const char* s) { return (unsigned long long)strlen(s); }
void* __vela_malloc(unsigned long long n) { return malloc((size_t)n); }
void* __vela_realloc(void* p, unsigned long long n) { return realloc(p, (size_t)n); }
int __vela_strncmp(const char* a, const char* b, unsigned long long n) {
    return strncmp(a, b, (size_t)n);
}
int __vela_snprintf(char* buf, unsigned long long n, const char* fmt, ...) {
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(buf, (size_t)n, fmt, ap);
    va_end(ap);
    return r;
}

/* The real C entry point: every target's crt (MSVC, glibc, wasi-libc) knows
   how to call a plain C main; the IR only exports vela_entry. */
extern int vela_entry(void);
int main(void) { return vela_entry(); }
"#;

/// `velac build <file.vela> [-o out] [--target wasm]` — emit IR, then invoke
/// clang to link a native executable (or a `wasm32-wasi` module).
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
    let ir = match vela_codegen::emit(&program) {
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
    let shim_path = PathBuf::from(&out_path).with_extension("shim.c");
    if let Err(e) = std::fs::write(&shim_path, RUNTIME_SHIM) {
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
