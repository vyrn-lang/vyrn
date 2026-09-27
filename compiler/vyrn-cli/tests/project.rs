//! Project mode: `vyrn new`, manifest-driven `run`/`check`, bare-specifier
//! dependencies, and `vyrn deps`. No clang needed.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

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

    let deps = vyrn().current_dir(&dir).arg("deps").output().unwrap();
    let text = String::from_utf8_lossy(&deps.stdout);
    assert!(text.contains("dep/money.vyrn"), "{text}");
    assert!(text.contains("-> "), "{text}");
}

/// The `toolchain:` section: a row per tool, with the path used, its version,
/// and why that path was chosen.
///
/// The environment override is set on the child process, never on this one:
/// `set_var` beside another test thread's `getenv` is a race. The shape of the
/// report is asserted, not which tools this runner has.
#[test]
fn deps_reports_the_toolchain_and_why() {
    let dir = scratch("toolchain");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "src/main.vyrn"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.vyrn"),
        "fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();

    // No `toolchain` key: every row is a discovery. `VYRN_WASMTIME` is removed
    // because CI exports it, and the override would otherwise answer every case.
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("toolchain:"), "{text}");
    for tool in ["clang", "wasmtime", "wasm2c", "simde"] {
        assert!(
            text.lines().any(|l| l.trim_start().starts_with(tool)),
            "no row for {tool}: {text}"
        );
    }
    assert!(!text.contains("(pinned)"), "{text}");

    // A pin whose bytes are not cached: the row prints the refusal, and `deps`
    // still answers.
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "src/main.vyrn", "toolchain": {"wasmtime": "46.0.1"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("vyrn.lock"),
        format!(
            "tool:wasmtime@46.0.1/x86_64-linux\thttps://example.invalid/w\t{}\n",
            "d".repeat(64)
        ),
    )
    .unwrap();
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let row = row_for(&text, "wasmtime");
    assert!(row.contains("unresolved"), "{row}");
    assert!(row.contains("46.0.1"), "{row}");

    // The environment override beats the pin, and prints as an override.
    let hatch = PathBuf::from(env!("CARGO_BIN_EXE_vyrn"));
    let out = vyrn()
        .current_dir(&dir)
        .env("VYRN_WASMTIME", &hatch)
        .arg("deps")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let row = row_for(&text, "wasmtime");
    assert!(row.contains("(override: environment)"), "{row}");
    assert!(!row.contains("(pinned)"), "{row}");
}

/// The `toolchain:` row for one tool.
fn row_for(text: &str, tool: &str) -> String {
    text.lines()
        .find(|l| l.trim_start().starts_with(tool))
        .unwrap_or_else(|| panic!("no {tool} row in:\n{text}"))
        .to_string()
}

/// `VYRN_WASMTIME` is removed because CI exports it, and a report about a
/// discovered tool must not be answered by this runner's environment.
#[test]
fn deps_reports_every_declared_artifact() {
    // Not `artifacts`: another test owns that scratch name, and a shared
    // directory is a race.
    let dir = scratch("artifact-graphs");
    std::fs::create_dir_all(dir.join("client")).unwrap();
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "artifacts": {
             "api": {"entry": "server.vyrn", "target": "native"},
             "app": {"entry": "client/boot.vyrn", "target": "browser"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("shared/wire.vyrn"),
        "export fn tag() -> Int64 { return 7 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("server.vyrn"),
        "import { tag } from \"./shared/wire\"\nfn main() -> Int64 { return tag() }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("client/boot.vyrn"),
        "import { tag } from \"../shared/wire\"\nfn main() -> Int64 { return tag() }\n",
    )
    .unwrap();

    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // The entry as the manifest writes it, relative to the project.
    assert!(
        text.contains("artifact `api` (native) — server.vyrn"),
        "{text}"
    );
    assert!(
        text.contains("artifact `app` (browser) — client/boot.vyrn"),
        "{text}"
    );
    assert!(text.contains("shared/wire.vyrn"), "{text}");
    // One toolchain section: the tools are the project's, not the artifact's.
    assert_eq!(text.matches("toolchain:").count(), 1, "{text}");

    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .args(["deps", "app"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("artifact `app` (browser)"), "{text}");
    assert!(!text.contains("artifact `api`"), "{text}");

    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .args(["deps", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no artifact `nope`"), "{err}");
    assert!(err.contains("api, app"), "{err}");
}

/// A project declaring only `main` gets no artifact header at all.
#[test]
fn the_entry_point_keys_are_artifacts_to_deps_too() {
    let dir = scratch("artifact-sugar");
    std::fs::create_dir_all(dir.join("client")).unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "server": "server.vyrn", "client": "client/boot.vyrn"}"#,
    )
    .unwrap();
    let src = "fn main() -> Int64 { return 0 }\n";
    std::fs::write(dir.join("server.vyrn"), src).unwrap();
    std::fs::write(dir.join("client/boot.vyrn"), src).unwrap();
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("artifact `server` (native)"), "{text}");
    assert!(text.contains("artifact `client` (browser)"), "{text}");

    let dir = scratch("artifact-main-only");
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"name": "t", "main": "main.vyrn"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.vyrn"), src).unwrap();
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("artifact `"), "{text}");
    // The first line is the entry, not a header. The path is not compared whole:
    // a temp directory reached through a symlink is canonicalized.
    assert!(
        text.lines()
            .next()
            .is_some_and(|l| l.ends_with("main.vyrn")),
        "{text}"
    );
    assert!(text.contains("toolchain:"), "{text}");
}

/// The repository's own root is such a manifest.
#[test]
fn deps_answers_a_toolchain_only_manifest() {
    let dir = scratch("toolchain-only");
    std::fs::write(dir.join("vyrn.json"), r#"{"toolchain": {}}"#).unwrap();
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a toolchain-only manifest is not an error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("declares no artifacts"), "{text}");
    assert!(text.contains("toolchain:"), "{text}");
    assert!(row_for(&text, "clang").contains("clang"), "{text}");

    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .args(["deps", "app"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("declares no artifacts"), "{err}");
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

/// The refusal arrives on the unreadable-manifest channel, before anything
/// compiles. A silent fallback would build for a target nobody declared.
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

    // The long form of the `main` key runs as the short form does.
    manifest(r#"{"main":{"entry":"src/main.vyrn","target":"native"}}"#);
    let run = vyrn().current_dir(&dir).arg("run").output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "1");
}

/// The refusal comes before clang is looked for. A silent fallback to the
/// default would ship a binary built for a target the project did not write.
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
    // `--native-target` wins. Asserted as "does not complain about the key",
    // not "succeeds", because this suite runs without clang.
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

    // A wasm build ignores the key, and needs no clang.
    let w = vyrn()
        .current_dir(&dir)
        .args(["build", "src/main.vyrn", "--target", "wasm"])
        .output()
        .unwrap();
    assert!(w.status.success(), "{}", String::from_utf8_lossy(&w.stderr));
}

/// CI's cache-miss path needs this: plain `vyrn update` would write whatever
/// arrived into the lock. The fetch is a `file://` URL, so no network is needed.
#[test]
fn update_locked_verifies_against_the_lock_and_never_rewrites_it() {
    use vyrn_frontend::toolpin::{host_platform, tool_spec};
    let dir = scratch("update-locked");
    let archive = dir.join("not-really-wasmtime.tar.gz");
    std::fs::write(&archive, b"these bytes are not the pinned bytes").unwrap();
    let url = format!(
        "file:///{}",
        archive
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    std::fs::write(
        dir.join("vyrn.json"),
        r#"{"toolchain": {"wasmtime": "9.9.9"}}"#,
    )
    .unwrap();
    let lock = format!(
        "{}\t{url}\t{}\n",
        tool_spec("wasmtime", "9.9.9", &host_platform()),
        "e".repeat(64)
    );
    std::fs::write(dir.join("vyrn.lock"), &lock).unwrap();

    let out = vyrn()
        .current_dir(&dir)
        .args(["update", "--locked"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a hash mismatch must not pass");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("the upstream changed under an immutable URL"),
        "{err}"
    );
    assert!(err.contains(&"e".repeat(64)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("vyrn.lock")).unwrap(),
        lock
    );
}
