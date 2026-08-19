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

/// RFC-0102 M3: the `toolchain:` section — a row per tool, with the path that
/// would be used, its version, and WHY that path was chosen.
///
/// Every case runs `vyrn` as a CHILD process, so the environment override is set
/// on the child rather than on this one: `set_var` beside another test thread's
/// `getenv` is the race M2's codegen checks avoid by living in one `#[test]`.
///
/// No host-only branch: a machine with clang and one without both print a clang
/// row, and what is asserted is the shape of the report, not which tools this
/// runner happens to have.
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

    // (a) No `toolchain` key: every row is a discovery, and the report says so.
    // `VYRN_WASMTIME` is removed from the CHILD because CI exports it for the
    // jobs that need a runtime, and step 1 would otherwise answer every case.
    let out = vyrn()
        .current_dir(&dir)
        .env_remove("VYRN_WASMTIME")
        .arg("deps")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("toolchain:"), "{text}");
    for tool in ["clang", "wasmtime", "wasi-sysroot", "wasi-builtins"] {
        assert!(
            text.lines().any(|l| l.trim_start().starts_with(tool)),
            "no row for {tool}: {text}"
        );
    }
    // Nothing is pinned here, so no row may claim to be.
    assert!(!text.contains("(pinned)"), "{text}");

    // (b) A pin whose bytes are not cached: the row prints the refusal rather
    // than being omitted, and `deps` still answers.
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

    // (c) The environment override beats the pin, and prints AS an override —
    // the line a machine that disagrees with CI points at.
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

/// The `toolchain:` row for one tool, for a check that wants the whole line.
fn row_for(text: &str, tool: &str) -> String {
    text.lines()
        .find(|l| l.trim_start().starts_with(tool))
        .unwrap_or_else(|| panic!("no {tool} row in:\n{text}"))
        .to_string()
}

/// RFC-0103 M1 made `artifacts` — and the `main`/`server`/`client` keys that
/// are sugar for it — the declaration of what a project builds, and `vyrn deps`
/// asked for a `main`. So the command that answers "what does this build depend
/// on" could not answer for `examples/shelf`, which is a project this repository
/// builds. It reports every declared artifact now.
///
/// Child processes, `VYRN_WASMTIME` removed, for the reason the M3 checks give:
/// CI exports it, and a report about a discovered tool must not be answered by
/// this runner's environment.
#[test]
fn deps_reports_every_declared_artifact() {
    // Not `artifacts`: another test in this file already owns that scratch name,
    // and two tests sharing one directory is a race that reads the other's
    // manifest.
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
    // Each artifact is named with its entry and its target, and its graph
    // follows — the entry as the manifest writes it, relative to the project.
    assert!(
        text.contains("artifact `api` (native) — server.vyrn"),
        "{text}"
    );
    assert!(
        text.contains("artifact `app` (browser) — client/boot.vyrn"),
        "{text}"
    );
    assert!(text.contains("shared/wire.vyrn"), "{text}");
    // One toolchain section, under both graphs: the tools are the project's, not
    // the artifact's.
    assert_eq!(text.matches("toolchain:").count(), 1, "{text}");

    // Naming one artifact scopes the report to it.
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

    // A name nobody declared says which names are declared.
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

/// The sugar keys are artifacts, so a project that never wrote an `artifacts`
/// map is reported the same way — and a project declaring only `main` is
/// reported exactly as it was before this change, with no header at all.
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

    // The `main`-only project: the graph, then the toolchain, and nothing else.
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
    // The first line is the graph's root — the entry, not a header. The path is
    // not compared whole: a temp directory reached through a symlink is
    // canonicalized, and this check is about the SHAPE of the report.
    assert!(
        text.lines()
            .next()
            .is_some_and(|l| l.ends_with("main.vyrn")),
        "{text}"
    );
    assert!(text.contains("toolchain:"), "{text}");
}

/// A manifest that pins tools and declares nothing to build — the repository's
/// own root — is answered, not refused. The question was "what does this build
/// depend on", and "no artifact is declared here" is the true answer to it.
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

    // Asking it for an artifact by name is still an error: that name is not
    // there to report.
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

/// `vyrn update --locked` fetches what the lock pins and REFUSES what does not
/// hash to what the lock says — the behaviour CI's cache-miss path needs
/// (RFC-0102 M4). `vyrn update` would fetch too, and would then write whatever
/// arrived into the lock, which is the one thing a CI run must not do.
///
/// The fetch is a `file://` URL, so this needs no network: what is under test is
/// the comparison and the refusal, not curl's transport.
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
    // And the lock is the file it was: a locked run reads it, never writes it.
    assert_eq!(
        std::fs::read_to_string(dir.join("vyrn.lock")).unwrap(),
        lock
    );
}
