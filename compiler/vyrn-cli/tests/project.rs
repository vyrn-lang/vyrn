//! Project-mode integration tests (RFC-0010 M3): `vyrn new`, manifest-driven
//! `run`/`check`, bare-specifier dependencies, and `vyrn deps`. No clang
//! needed (interpreter only), so these run in the default suite.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-project-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn new_scaffolds_a_runnable_project() {
    let dir = scratch("scaffold");
    let out = vyrn()
        .current_dir(&dir)
        .args(["new", "app"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for f in ["vyrn.json", "src/main.vyrn", ".gitignore"] {
        assert!(dir.join("app").join(f).is_file(), "missing {f}");
    }
    // `vyrn run` with no file argument uses the manifest's main.
    let run = vyrn()
        .current_dir(dir.join("app"))
        .arg("run")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "hello from app"
    );
}

#[test]
fn bare_specifiers_resolve_through_the_manifest() {
    let dir = scratch("aliases");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("dep")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "src/main.vyrn", "dependencies": {"money": "./dep/money"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("dep/money.vyrn"),
        "export fn addTax(n: Int64) -> Int64 { return n * 120 / 100 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "import { addTax } from \"money\"\nfn main() -> Int64 { print(addTax(1000)) return 0 }\n",
    )
    .unwrap();
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1200");

    // `vyrn deps` prints the graph including the aliased module.
    let deps = vyrn().current_dir(&dir).arg("deps").output().unwrap();
    let text = String::from_utf8_lossy(&deps.stdout);
    assert!(text.contains("dep/money.vyrn"), "{text}");
    assert!(text.contains("-> "), "{text}");
}

#[test]
fn unknown_bare_specifier_names_the_manifest_fix() {
    let dir = scratch("unknown");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "src/main.vyrn"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "import { x } from \"nope\"\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(!run.status.success());
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(
        err.contains("vyrn.json"),
        "should point at the manifest: {err}"
    );
}

#[test]
fn no_file_and_no_manifest_is_a_clear_error() {
    let dir = scratch("bare");
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(!run.status.success());
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(err.contains("no input file"), "{err}");
}

/// RFC-0103 M1: an artifact whose `target` is not one of the three capability
/// sets fails naming the artifact, the file and the three values — on the same
/// channel an unreadable manifest already uses, so it arrives before anything
/// is compiled. A silent fallback would build for a target nobody declared.
#[test]
fn an_unknown_artifact_target_names_the_artifact_and_the_valid_ones() {
    let dir = scratch("artifacts");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "fn main() -> Int64 { print(1) return 0 }\n",
    )
    .unwrap();
    let manifest = |artifacts: &str| {
        std::fs::write(
            dir.join("vyrn.json"),
            format!(r#"{{"name":"t","main":"src/main.vyrn","artifacts":{artifacts}}}"#),
        )
        .unwrap()
    };

    manifest(r#"{"app":{"entry":"src/main.vyrn","target":"wasm"}}"#);
    let out = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    for want in [
        "artifact `app`",
        "wasm",
        "vyrn.json",
        "native, wasi, browser",
    ] {
        assert!(err.contains(want), "missing {want:?} in: {err}");
    }

    // …and a manifest that writes out what its `main` key already says runs
    // exactly as it did before the key was written out.
    manifest(r#"{"main":{"entry":"src/main.vyrn","target":"native"}}"#);
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1");
}

/// A misspelled `nativeTarget` must fail naming the key and the file, before
/// the compile and before clang is even looked for — so this runs in the
/// default suite. A silent fall back to the default would ship a binary built
/// for something other than what the project wrote down.
#[test]
fn an_unknown_native_target_names_the_manifest_key() {
    let dir = scratch("nativetarget");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "src/main.vyrn", "nativeTarget": "haswell"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn()
        .current_dir(&dir)
        .args(["build", "src/main.vyrn"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    for want in [
        "nativeTarget",
        "haswell",
        "vyrn.json",
        "v1, v2, v3, v4, native",
    ] {
        assert!(err.contains(want), "missing {want:?} in: {err}");
    }
    // `--native-target` wins, so the same project gets past the config error.
    // Asserted as "no longer complains about the key" rather than "succeeds",
    // because this file's suite is the one that runs without clang.
    let ov = vyrn()
        .current_dir(&dir)
        .args(["--native-target", "v2", "build", "src/main.vyrn"])
        .output()
        .unwrap();
    let ov_err = String::from_utf8_lossy(&ov.stderr);
    assert!(
        !ov_err.contains("nativeTarget"),
        "the override did not win: {ov_err}"
    );

    // A wasm build ignores the key entirely rather than failing on it — and
    // since RFC-0077 M5 it needs no clang, so this half can assert success.
    let w = vyrn()
        .current_dir(&dir)
        .args(["build", "src/main.vyrn", "--target", "wasm"])
        .output()
        .unwrap();
    assert!(w.status.success(), "{}", String::from_utf8_lossy(&w.stderr));
}
