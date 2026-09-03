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

mod common;

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

/// Every project entry the registry says must be refused, is — with the text
/// it names. `examples/listing` is RFC-0125 M6's prediction program for
/// finding 6: a browser artifact that lists a directory.
#[test]
fn every_registered_project_entry_is_refused() {
    for (entry, why, needle) in common::EXPECTED_PROJECT_CHECK_FAILURE {
        let (ok, err) = check(&repo_dir("examples").join(entry));
        assert!(!ok, "{entry} should be refused ({why}):\n{err}");
        assert!(
            err.contains(needle),
            "{entry}: expected `{needle}` in:\n{err}"
        );
    }
    let (_, err) = check(&repo_dir("examples/listing/client/boot.vyrn"));
    assert!(
        err.contains(
            "artifact `app` (browser) cannot include `client/boot.vyrn`: it reaches the filesystem"
        ),
        "{err}"
    );
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
    // The repository root carries a `vyrn.json` since RFC-0102 M4, and it
    // declares only a `toolchain`: no artifacts, so no floor, so this file is
    // still nobody's artifact and still untouched. That is the inertness the
    // root manifest has to have, asserted where it would first be lost.
    assert!(
        ok,
        "an extern demo under a manifest that declares no artifacts is untouched:\n{err}"
    );
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

/// RFC-0103 M4's finding 2, fixed: the report walks the LINKED graph, so a
/// GENERATED module's capability is in it.
///
/// `client(..)` emits the `vyrnRpcCall` extern into a module the loader
/// produces and no resolver can read — the one carrier no author writes and no
/// reading of the project's own files can find. The report used to say
/// `nothing … needs 'extern'` about the very artifact the check refuses when it
/// is retargeted, which is asserted here beside it: one graph, two commands.
#[test]
fn why_capability_sees_what_a_generator_wrote() {
    let dir = scratch("generated");
    write(
        &dir,
        "server/api/notes.vyrn",
        "export type CreateReq = { body: String }\n\
         export type Created = { id: Int64 }\n\
         export fn create(req: CreateReq) -> Created {\n    return Created { id: 1 }\n}\n",
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { client } from \"std/rpc\"\n\
         import { notesCreate } from client(\"../server/api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let manifest = |target: &str| {
        format!(
            "{{ \"name\": \"p\", \"artifacts\": {{ \
              \"app\": {{ \"entry\": \"client/boot.vyrn\", \"target\": \"{target}\" }} }} }}\n"
        )
    };
    write(&dir, "vyrn.json", &manifest("browser"));

    let out = vyrn()
        .arg("why")
        .args(["--capability", "extern", "app"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("`vyrnRpcCall` needs `extern`"),
        "the stub's own import is the carrier:\n{text}"
    );
    // The chain names the author's call site, which is all a banner has to give.
    assert!(
        text.contains(
            "client/boot.vyrn -> generated by client(\"../server/api\") at client/boot.vyrn"
        ),
        "{text}"
    );

    // The check refuses the same module off the browser. Same graph, and the
    // report can no longer be silent about what the check names.
    write(&dir, "vyrn.json", &manifest("native"));
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(!ok, "a native artifact has no host to import from:\n{err}");
    assert!(
        err.contains("`vyrnRpcCall` needs `extern`; target `native` has no host to import from"),
        "{err}"
    );
    assert!(
        err.contains(
            "client/boot.vyrn → generated by client(\"../server/api\") at client/boot.vyrn"
        ),
        "{err}"
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

/// RFC-0125 M6, fourth slice: the `stdin` and `args` rows are decided by the
/// effect judgment, and the refusal is the pass's own words. `VYRN_NO_JUDGE=1`
/// puts both rows back in the pass, and the two texts must be one text —
/// which is what "the rule moved, the refusal did not" means.
#[test]
fn a_moved_row_refuses_in_the_words_the_pass_used() {
    const STDIN: &str = "fn main() -> Int64 {\n    \
         let line = match readLine() { Some(s) => s, None => \"\", }\n    \
         print(line)\n    return 0\n}\n";
    const ARGS: &str =
        "fn main() -> Int64 {\n    let a = args()\n    print(a[0])\n    return 0\n}\n";
    for (name, body) in [("stdin", STDIN), ("args", ARGS)] {
        let dir = scratch(name);
        write(&dir, "client/boot.vyrn", body);
        write(
            &dir,
            "vyrn.json",
            "{ \"name\": \"p\", \"artifacts\": { \
              \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n",
        );
        let entry = dir.join("client/boot.vyrn");
        let (ok, judged) = check(&entry);
        assert!(
            !ok,
            "{name}: the browser artifact must be refused:\n{judged}"
        );
        let out = vyrn()
            .env("VYRN_NO_JUDGE", "1")
            .arg("check")
            .arg(&entry)
            .output()
            .expect("run check");
        let pass = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success(), "{name}: {pass}");
        assert_eq!(judged, pass, "{name}: the judgment changed the refusal");
    }
}
