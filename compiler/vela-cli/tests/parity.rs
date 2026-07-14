//! Corpus parity harness: every example must behave byte-identically under the
//! interpreter (`velac run`, the reference semantics) and the native binary
//! (`velac build` + execute). Compares stdout, stderr, and exit code.
//!
//! Ignored by default (needs `clang` and builds every example — ~a minute):
//!
//!     cargo test -p vela-cli --test parity -- --ignored --nocapture
//!
//! Line endings are normalized (CRLF → LF): the interpreter writes LF while
//! the native binary inherits the platform's text-mode CRLF — a documented,
//! benign difference.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Examples currently expected to diverge, with the reason. Shrink this list —
/// never grow it silently.
const KNOWN_DIVERGENT: &[(&str, &str)] = &[
    // Trap wording/stream differs until the trap-unification phase lands:
    // interp: stderr "error: ..."; native: stdout "Vela: validation failed".
    ("validate_fail.vela", "trap text/stream not yet unified"),
];

fn examples_dir() -> PathBuf {
    // vela-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").canonicalize().unwrap()
}

fn velac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_velac"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vela-cli --test parity -- --ignored"]
fn examples_interp_native_parity() {
    let dir = examples_dir();
    let out_dir = std::env::temp_dir().join("vela-parity");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vela"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut failures: Vec<String> = Vec::new();
    let mut skipped = 0usize;

    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some((_, why)) = KNOWN_DIVERGENT.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  ({why})");
            skipped += 1;
            continue;
        }

        let interp = velac().arg("run").arg(path).output().expect("run interp");

        let exe = out_dir.join(format!("{name}.exe"));
        let build = velac().arg("build").arg(path).arg("-o").arg(&exe).output().expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name}: native build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let native = Command::new(&exe).output().expect("run native");

        let (i_out, n_out) = (norm(&interp.stdout), norm(&native.stdout));
        let (i_err, n_err) = (norm(&interp.stderr), norm(&native.stderr));
        let (i_code, n_code) = (interp.status.code(), native.status.code());

        if i_out != n_out || i_err != n_err || i_code != n_code {
            failures.push(format!(
                "{name}: DIVERGED\n  exit: interp {i_code:?} vs native {n_code:?}\n  \
                 stdout interp: {i_out:?}\n  stdout native: {n_out:?}\n  \
                 stderr interp: {i_err:?}\n  stderr native: {n_err:?}"
            ));
        } else {
            eprintln!("ok    {name}");
        }
    }

    eprintln!(
        "\nparity: {} checked, {} skipped (known divergent), {} failed",
        names.len() - skipped,
        skipped,
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
