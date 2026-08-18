//! Integration tests for RFC-0103's floor — M2's check and M3's
//! `vyrn why --capability`, driven through the real `vyrn` binary over real
//! project trees.
//!
//! The floor's claim is that a target is a capability set and nobody can relabel
//! it, so every test here runs `vyrn check` and asserts on the text a user would
//! see. The headline case is `examples/leak`, a committed project whose browser
//! artifact reaches a file reader three hops away: the milestone's gate asks for
//! that example and for the full chain in its diagnostic.
//!
//! `examples/leak` is a DIRECTORY, so the parity corpus never sees it — that loop
//! reads `examples/*.vyrn` and nothing below. The `EXPECTED_CHECK_FAILURE` list
//! in `tests/common` is the precedent for an example that must fail `check`, and
//! it is a list of single files; a project does not fit it, so the assertion is
//! here instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c.env("VYRN_STD", repo_dir("std"));
    c
}

fn check(path: &Path) -> (bool, String) {
    let out = vyrn().arg("check").arg(path).output().expect("run check");
    let text = String::from_utf8_lossy(&out.stderr).to_string()
        + &String::from_utf8_lossy(&out.stdout).to_string();
    (out.status.success(), text)
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_floor_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The milestone's gate: the committed leak example is refused, with the whole
/// chain, and the module it names is fine for the artifact that CAN reach a
/// filesystem. One module, two artifacts, two answers.
#[test]
fn the_leak_example_is_refused_with_the_full_chain() {
    let leak = repo_dir("examples/leak");
    let (ok, err) = check(&leak.join("client/boot.vyrn"));
    assert!(!ok, "expected the browser artifact to be refused:\n{err}");
    assert!(
        err.contains(
            "artifact `app` (browser) cannot include `server/db.vyrn`: it reaches the filesystem"
        ),
        "{err}"
    );
    // The chain is the usability story — the author never saw hop three.
    assert!(
        err.contains("client/boot.vyrn → shared/format.vyrn → server/db.vyrn"),
        "{err}"
    );
    assert!(
        err.contains("`readFile` needs `fs`; target `browser` has no filesystem"),
        "{err}"
    );
    // The remedy names the module actually reached, as its importer would spell
    // it — not RFC-0072's one fixed path.
    assert!(err.contains("connect(\"../server/db\")"), "{err}");

    let (ok, err) = check(&leak.join("server/main.vyrn"));
    assert!(ok, "the native artifact reaches the same module:\n{err}");
}

/// The floor fires for a declared ENTRY POINT and for nothing else. A module in
/// the middle of the same project, checked on its own, is nobody's artifact —
/// which is what leaves `examples/externdemo.vyrn` (built natively by the parity
/// suite, and no project's entry) exactly as it was.
#[test]
fn a_file_that_is_no_artifacts_entry_has_no_target() {
    let leak = repo_dir("examples/leak");
    let (ok, err) = check(&leak.join("shared/format.vyrn"));
    assert!(ok, "a middle module declares no target:\n{err}");
    let (ok, err) = check(&repo_dir("examples/externdemo.vyrn"));
    assert!(ok, "an extern demo under no manifest is untouched:\n{err}");
}

const CLIENT: &str = "import { read } from \"../server/db\"\n\
     fn main() -> Int64 {\n    print(read())\n    return 0\n}\n";
const SERVER: &str = "export fn read() -> String {\n    \
     return match readFile(\"x.txt\") { Ok(s) => s, Err(e) => e, }\n}\n";

/// Opt-in is absolute: the same tree, with the `artifacts` map removed, compiles
/// clean. Nothing that builds today can start failing because this landed.
#[test]
fn a_project_that_declares_no_artifacts_gets_no_floor() {
    let dir = scratch("optin");
    write(&dir, "server/db.vyrn", SERVER);
    write(&dir, "client/boot.vyrn", CLIENT);

    write(&dir, "vyrn.json", "{ \"name\": \"p\" }\n");
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(ok, "no artifacts, no floor:\n{err}");

    write(
        &dir,
        "vyrn.json",
        "{ \"name\": \"p\", \"artifacts\": { \
          \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n",
    );
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(!ok, "declaring the artifact is what turns it on:\n{err}");
    assert!(err.contains("target `browser` has no filesystem"), "{err}");
}

/// The manifest cannot relabel the floor. `wasi` and `browser` are the identical
/// bytes under two hosts, and the two answers differ — which is the point: no
/// edit to `vyrn.json` gives a page a filesystem, and calling the browser
/// artifact `wasi` is not a fix, it is a different artifact.
#[test]
fn the_target_decides_and_the_manifest_cannot_argue() {
    let dir = scratch("targets");
    write(&dir, "server/db.vyrn", SERVER);
    write(&dir, "client/boot.vyrn", CLIENT);
    write(
        &dir,
        "host.vyrn",
        "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\
         fn main() -> Int64 {\n    return jsAdd(1, 2)\n}\n",
    );
    let manifest = |boot: &str, host: &str| {
        format!(
            "{{ \"name\": \"p\", \"artifacts\": {{ \
              \"app\": {{ \"entry\": \"client/boot.vyrn\", \"target\": \"{boot}\" }}, \
              \"h\": {{ \"entry\": \"host.vyrn\", \"target\": \"{host}\" }} }} }}\n"
        )
    };

    // `fs` is native's and wasi's; `extern` is the browser's, and only there.
    write(&dir, "vyrn.json", &manifest("wasi", "browser"));
    assert!(
        check(&dir.join("client/boot.vyrn")).0,
        "wasi has a filesystem"
    );
    assert!(check(&dir.join("host.vyrn")).0, "a page IS the namespace");

    write(&dir, "vyrn.json", &manifest("browser", "wasi"));
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(!ok, "{err}");
    assert!(err.contains("has no filesystem"), "{err}");
    let (ok, err) = check(&dir.join("host.vyrn"));
    assert!(!ok, "{err}");
    assert!(
        err.contains("`jsAdd` needs `extern`; target `wasi` has no host to import from"),
        "{err}"
    );
    assert!(err.contains("it imports a host function"), "{err}");
}

/// RFC-0103 M3 — the capability axis of `vyrn why`. The floor's refusal shows
/// the SHORTEST chain; this shows every one, because deleting a hop off the
/// shortest path removes nothing while a second path still reaches the module.
///
/// Driven over the committed leak example, both artifacts, both spellings of the
/// argument: the entry's path and the artifact's name.
#[test]
fn why_capability_names_the_artifact_and_every_chain() {
    let leak = repo_dir("examples/leak");
    let why = |args: &[&str], cwd: &Path| -> (i32, String) {
        let out = vyrn()
            .arg("why")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run why");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };

    // By path, and by name from inside the project — the same artifact.
    for arg in ["client/boot.vyrn", "app"] {
        let (code, text) = why(&["--capability", "fs", arg], &leak);
        assert_eq!(code, 0, "{text}");
        assert!(
            text.contains("artifact: `app` (browser) — target `browser` has no filesystem"),
            "{text}"
        );
        assert!(text.contains("`readFile` needs `fs`"), "{text}");
        assert!(
            text.contains("client/boot.vyrn -> shared/format.vyrn -> server/db.vyrn"),
            "{text}"
        );
    }

    // The same module, reached by the artifact that HAS a filesystem: the report
    // still answers, because "where does it come from" is not "is it refused".
    let (code, text) = why(&["--capability", "fs", "api"], &leak);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("target `native` has `fs`"), "{text}");
    assert!(
        text.contains("server/main.vyrn -> shared/format.vyrn -> server/db.vyrn"),
        "{text}"
    );

    // A closure that never touches the capability answers in one line.
    let (code, text) = why(&["--capability", "stdin", "app"], &leak);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("nothing in artifact `app`'s closure needs `stdin`"),
        "{text}"
    );

    // The vocabulary is closed, and a refusal names all four.
    let (code, text) = why(&["--capability", "sockets", "app"], &leak);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("unknown capability `sockets`"), "{text}");
    assert!(text.contains("fs, stdin, args, extern"), "{text}");

    // An argument that names no artifact says which ones exist.
    let (code, text) = why(&["--capability", "fs", "nope"], &leak);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("declared: api, app"), "{text}");
}

/// EVERY chain, not the shortest one. Two routes reach the same reader, and a
/// report that showed one would have the author delete a hop and find the
/// capability still there.
#[test]
fn why_capability_shows_more_than_one_route() {
    let dir = scratch("routes");
    write(&dir, "server/db.vyrn", SERVER);
    write(
        &dir,
        "shared/format.vyrn",
        "import { read } from \"../server/db\"\n\
         export fn titled() -> String {\n    return read()\n}\n",
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { titled } from \"../shared/format\"\n\
         import { read } from \"../server/db\"\n\
         fn main() -> Int64 {\n    print(titled())\n    print(read())\n    return 0\n}\n",
    );
    write(
        &dir,
        "vyrn.json",
        "{ \"name\": \"p\", \"artifacts\": { \
          \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n",
    );
    let out = vyrn()
        .arg("why")
        .args(["--capability", "fs", "app"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("client/boot.vyrn -> shared/format.vyrn -> server/db.vyrn"),
        "{text}"
    );
    assert!(
        text.contains("client/boot.vyrn -> server/db.vyrn"),
        "{text}"
    );
}

/// RFC-0043's host-boundary externs are not host imports — the C runtime shim
/// implements all three on every target — so `std/time` is not a capability, and
/// every native server in the tree still compiles. M0's census read `extern fn`
/// as one thing; it is two.
#[test]
fn the_shim_implemented_externs_are_not_a_capability() {
    let dir = scratch("clock");
    write(
        &dir,
        "main.vyrn",
        "import { now, toMillis } from \"std/time\"\n\
         fn main() -> Int64 {\n    return toMillis(now())\n}\n",
    );
    write(
        &dir,
        "vyrn.json",
        "{ \"name\": \"p\", \"main\": \"main.vyrn\" }\n",
    );
    let (ok, err) = check(&dir.join("main.vyrn"));
    assert!(ok, "a clock is not a host import:\n{err}");
}
