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
    let out = vyrn().current_dir(&dir).args(["new", "app"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for f in ["vyrn.json", "src/main.vyrn", ".gitignore"] {
        assert!(dir.join("app").join(f).is_file(), "missing {f}");
    }
    // `vyrn run` with no file argument uses the manifest's main.
    let run = vyrn().current_dir(dir.join("app")).arg("run").output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "hello from app");
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
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
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
    std::fs::write(dir.join("vyrn.json"), r#"{"name": "t", "main": "src/main.vyrn"}"#).unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "import { x } from \"nope\"\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(!run.status.success());
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(err.contains("vyrn.json"), "should point at the manifest: {err}");
}

#[test]
fn no_file_and_no_manifest_is_a_clear_error() {
    let dir = scratch("bare");
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(!run.status.success());
    let err = String::from_utf8_lossy(&run.stderr);
    assert!(err.contains("no input file"), "{err}");
}
