//! The capability floor: a target is a capability set and the
//! manifest cannot relabel it. The tests run `vyrn check` and
//! `vyrn why --capability` over real project trees and assert on the text a
//! user sees.
//!
//! `examples/leak` is a directory, so the parity corpus (`examples/*.vyrn`)
//! never sees it, and `EXPECTED_CHECK_FAILURE` in `tests/common` lists single
//! files only; its assertions live here.

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

/// The chain names the entry once when the CLI is handed a path relative to a directory above
/// the project (#588).
#[test]
fn the_chain_names_the_entry_once_from_a_relative_path() {
    let out = vyrn()
        .current_dir(repo_dir("examples"))
        .arg("check")
        .arg("leak/client/boot.vyrn")
        .output()
        .expect("run check");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("note: client/boot.vyrn → shared/format.vyrn → server/db.vyrn"),
        "{err}"
    );
}

const CLIENT: &str = "import { read } from \"../server/db\"\n\
     fn main() -> Int64 {\n    print(read())\n    return 0\n}\n";
const SERVER: &str = "export fn read() -> String {\n    \
     return match readFile(\"x.txt\") { Ok(s) => s, Err(e) => e, }\n}\n";

/// The same tree without the `artifacts` map compiles clean.
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

/// `wasi` and `browser` are the same bytes under two hosts, and the answers
/// differ: no edit to `vyrn.json` gives a page a filesystem.
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

/// Over `examples/leak`, both artifacts, and both spellings of the argument:
/// the entry's path and the artifact's name.
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

    // The artifact that has a filesystem still gets the chains: "where does it
    // come from" is not "is it refused".
    let (code, text) = why(&["--capability", "fs", "api"], &leak);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("target `native` has `fs`"), "{text}");
    assert!(
        text.contains("server/main.vyrn -> shared/format.vyrn -> server/db.vyrn"),
        "{text}"
    );

    let (code, text) = why(&["--capability", "stdin", "app"], &leak);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("nothing in artifact `app`'s closure needs `stdin`"),
        "{text}"
    );

    let (code, text) = why(&["--capability", "sockets", "app"], &leak);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("unknown capability `sockets`"), "{text}");
    assert!(text.contains("fs, stdin, args, extern"), "{text}");

    let (code, text) = why(&["--capability", "fs", "nope"], &leak);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("declared: api, app"), "{text}");
}

/// The floor's refusal shows the shortest chain; `why` shows every one, because
/// deleting a hop off one path removes nothing while a second path remains.
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

/// The report walks the linked graph. `client(..)` emits the `vyrnRpcCall`
/// extern into a module no resolver can read, so only the linked graph finds
/// it. The check's refusal of the same module is asserted beside it: one graph,
/// two commands.
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
    // The chain names the author's call site.
    assert!(
        text.contains(
            "client/boot.vyrn -> generated by client(\"../server/api\") at client/boot.vyrn"
        ),
        "{text}"
    );

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

/// The time and random host-boundary externs are not host imports: the runtime shim
/// implements all three on every target, so `std/time` is not a capability.
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

/// The effect judgment decides the `stdin`, `args` and `fs` rows.
/// `VYRN_NO_JUDGE=1` puts every row back in the pass, and the two refusals are
/// one text.
#[test]
fn a_moved_row_refuses_in_the_words_the_pass_used() {
    const STDIN: &str = "fn main() -> Int64 {\n    \
         let line = match readLine() { Some(s) => s, None => \"\", }\n    \
         print(line)\n    return 0\n}\n";
    const ARGS: &str =
        "fn main() -> Int64 {\n    let a = args()\n    print(a[0])\n    return 0\n}\n";
    const READ: &str = "fn main() -> Int64 {\n    \
         let t = match readFile(\"a.txt\") { Ok(s) => s, Err(e) => e, }\n    \
         print(t)\n    return 0\n}\n";
    const LIST: &str = "fn main() -> Int64 {\n    \
         let d = match listDir(\".\") { Ok(v) => 1, Err(e) => 0, }\n    \
         print(d.toString())\n    return 0\n}\n";
    for (name, body) in [
        ("stdin", STDIN),
        ("args", ARGS),
        ("fsread", READ),
        ("fslist", LIST),
    ] {
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

/// The `extern` row is carried by the call, not the declaration. A declared
/// import nothing calls emits no `(import "vyrn" ..)`, so the artifact runs
/// without a host and is accepted.
///
/// A called import needs a host and is refused, with the same bytes under
/// `VYRN_NO_JUDGE=1`. A dead ordinary function that calls it is refused by both
/// rules too: `lower` instantiates every non-generic function, so the judgment
/// sees `dead` as the scan does.
#[test]
fn an_unreached_host_import_is_no_capability() {
    const UNUSED: &str = "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\n\
         fn main() -> Int64 {\n    print(\"no host needed\")\n    return 0\n}\n";
    const USED: &str = "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\n\
         fn main() -> Int64 {\n    let x = jsAdd(1, 2)\n    print(x.toString())\n    return 0\n}\n";
    const DEAD: &str = "extern fn jsAdd(a: Int64, b: Int64) -> Int64\n\n\
         fn dead() -> Int64 {\n    return jsAdd(1, 2)\n}\n\n\
         fn main() -> Int64 {\n    print(\"alive\")\n    return 0\n}\n";
    const MANIFEST: &str = "{ \"name\": \"p\", \"artifacts\": { \
         \"app\": { \"entry\": \"host.vyrn\", \"target\": \"native\" } } }\n";

    let dir = scratch("externunused");
    write(&dir, "vyrn.json", MANIFEST);
    write(&dir, "host.vyrn", UNUSED);
    let entry = dir.join("host.vyrn");
    let (ok, err) = check(&entry);
    assert!(ok, "an import nothing calls is no capability:\n{err}");

    for (name, body) in [("used", USED), ("dead", DEAD)] {
        write(&dir, "host.vyrn", body);
        let (ok, judged) = check(&entry);
        assert!(!ok, "{name}: a native artifact has no host:\n{judged}");
        assert!(
            judged.contains("`jsAdd` needs `extern`; target `native` has no host to import from"),
            "{name}: {judged}"
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

/// The sink is a declaration and no effect set holds it, so the pass decides it
/// inside the load, before the check, with every declaration row.
#[test]
fn the_log_sink_is_a_declaration_the_judgment_does_not_clear() {
    let dir = scratch("logsink");
    write(
        &dir,
        "client/boot.vyrn",
        "logging { sink: file(\"app.log\") }\n\nfn main() -> Int64 {\n    \
         info(\"up\")\n    return 0\n}\n",
    );
    write(
        &dir,
        "vyrn.json",
        "{ \"name\": \"p\", \"artifacts\": {           \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n",
    );
    let entry = dir.join("client/boot.vyrn");
    let (ok, judged) = check(&entry);
    assert!(!ok, "a page cannot write a log file:\n{judged}");
    assert!(
        judged.contains("logging { sink: file(\"app.log\") }"),
        "{judged}"
    );
    let out = vyrn()
        .env("VYRN_NO_JUDGE", "1")
        .arg("check")
        .arg(&entry)
        .output()
        .expect("run check");
    let pass =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{pass}");
    assert_eq!(judged, pass, "the judgment touched a declaration");
}

/// The fence reads only the `gen` column of `effects.rs`, so `VYRN_NO_JUDGE=1`
/// changes neither refusal. The knob moves only the floor rows pinned above.
#[test]
fn the_generation_fence_is_the_same_under_the_bisect_knob() {
    const PRINTS: &str = "gen fn g() -> String {\n    print(\"hi\")\n    return \"x\"\n}\n\n\
         fn main() -> Int64 {\n    return 0\n}\n";
    const CLOCK: &str = "extern fn hostNowMillis() -> Int64\n\n\
         gen fn g() -> String {\n    let t = hostNowMillis()\n    return t.toString()\n}\n\n\
         fn main() -> Int64 {\n    return 0\n}\n";
    for (name, body, needle) in [
        ("genprint", PRINTS, "it calls `print`"),
        ("genclock", CLOCK, "it reads the clock"),
    ] {
        let dir = scratch(name);
        write(&dir, "main.vyrn", body);
        let entry = dir.join("main.vyrn");
        let (ok, judged) = check(&entry);
        assert!(!ok, "{name}: the fence must refuse this:\n{judged}");
        assert!(judged.contains(needle), "{name}: {judged}");
        let out = vyrn()
            .env("VYRN_NO_JUDGE", "1")
            .arg("check")
            .arg(&entry)
            .output()
            .expect("run check");
        let pass = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        assert!(
            !out.status.success(),
            "{name}: the knob restored a cell:\n{pass}"
        );
        assert_eq!(judged, pass, "{name}: the knob changed the refusal");
    }
}

/// A lambda in a `test` body is not in the artifact. It joins the closed set of its function type,
/// so an instance that calls through that type must not inherit its `args` effect.
#[test]
fn a_lambda_in_a_test_is_no_capability_of_the_artifact() {
    const MANIFEST: &str = "{ \"name\": \"w\", \"artifacts\": {          \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }
";
    const BOOT: &str = "fn never<T>(x: T) -> Int64 {
    return args().length
}

         fn twice(f: fn(Int64) -> Int64, x: Int64) -> Int64 {
    return f(x)
}

         fn main() -> Int64 {
    print(twice(y -> y + 1, 1).toString())
    return 0
}

         test \"a lambda in a test reads the command line\" {
             let g: fn(Int64) -> Int64 = y -> y + args().length
    assertEq(twice(g, 1), 1)
}
";

    let dir = scratch("testlambda");
    write(&dir, "vyrn.json", MANIFEST);
    write(&dir, "client/boot.vyrn", BOOT);
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(
        ok,
        "the test is not in the artifact:
{err}"
    );
}

/// An instance only a placed row names is in the artifact. `Pair<String>`'s release is named by
/// the row the kernel places in `Outer<String>`'s release, so the first lowering has neither
/// instance, and the one capability in the program is reached through `probe<String>` from that
/// release. The floor must still refuse it.
#[test]
fn a_capability_reached_by_a_release_only_a_placed_row_names_is_refused() {
    const MANIFEST: &str = "{ \"name\": \"w\", \"artifacts\": { \"app\": { \"entry\": \"client/boot.vyrn\", \"target\": \"browser\" } } }\n";
    const BOOT: &str = "fn probe<T>(x: Array<T>) -> Int64 {
    return args().length + x.length
}

type Pair<T> = { a: Array<T>, n: Int64 }

impl<T> Owned for Pair<T> {
    fn release(consume self) {
        print(probe(self.a).toString())
        let a = consume self.a
        drop a
    }
}

type Outer<T> = { p: Pair<T>, k: Int64 }

impl<T> Owned for Outer<T> {
    fn release(consume self) {
        let p = consume self.p
    }
}

fn main() -> Int64 {
    let o = Outer { p: Pair { a: [\"x\"], n: 4 }, k: 2 }
    print((o.k + o.p.n).toString())
    return 0
}
";

    let dir = scratch("lateinstance");
    write(&dir, "vyrn.json", MANIFEST);
    write(&dir, "client/boot.vyrn", BOOT);
    let (ok, err) = check(&dir.join("client/boot.vyrn"));
    assert!(!ok && err.contains("it reads the command line"), "{err}");
}
