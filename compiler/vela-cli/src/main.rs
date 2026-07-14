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
        "check" => {
            let diags = vela_frontend::diagnostics(&source);
            if diags.is_empty() {
                println!("ok");
                ExitCode::SUCCESS
            } else {
                for d in &diags {
                    // `file:line:col: message` — the conventional compiler
                    // diagnostic shape editors can parse. col is 0 when a stage
                    // knows only the line (checker/movecheck); emit `0`.
                    eprintln!("{}:{}:{}: {}", path, d.line, d.col, d.message);
                }
                ExitCode::FAILURE
            }
        }
        "run" => match vela_frontend::run(&source) {
            Ok(code) => {
                // main's return value becomes the process exit code (0..=255).
                ExitCode::from((code & 0xff) as u8)
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        "emit-ir" => {
            let program = match vela_frontend::check(&source) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
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

/// `velac build <file.vela> [-o out]` — emit IR, then invoke clang to link a
/// native executable.
fn build(path: &str, rest: &[String]) -> ExitCode {
    // parse optional `-o <out>`
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "-o" && i + 1 < rest.len() {
            out = Some(rest[i + 1].clone());
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

    let program = match vela_frontend::check(&source) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ir = match vela_codegen::emit(&program) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // default output name: <stem> (+ .exe on Windows)
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("a");
    let out_path = out.unwrap_or_else(|| {
        if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    });

    // write IR next to the output so failures are inspectable
    let ll_path = PathBuf::from(&out_path).with_extension("ll");
    if let Err(e) = std::fs::write(&ll_path, ir) {
        eprintln!("error: cannot write {}: {e}", ll_path.display());
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

    let status = Command::new(&clang)
        .arg(&ll_path)
        .arg("-o")
        .arg(&out_path)
        // our IR carries no target triple; clang supplies the host's — don't warn.
        .arg("-Wno-override-module")
        .status();
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
