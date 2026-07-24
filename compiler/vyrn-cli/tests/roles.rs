//! Integration tests for RFC-0072 M2 — roles and the `Api` contract, driven
//! through the real `vyrn` binary.
//!
//! M2 carries RFC-0071's deferred M3. Three claims are under test:
//!
//!   1. `std/rpc` declares `Api`, and a project attaches it by ROLE — so a
//!      module nobody has generated from yet can still be asked what governs it.
//!   2. A role scope may span the audience segment, so `server/api` and
//!      `client/api` are different roles rather than one that happened to win.
//!   3. Serializability is checked on BOTH ends of every procedure, and the
//!      failure names the rule rather than the symptom.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c.env("VYRN_STD", repo_dir("std"));
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_roles_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const MANIFEST: &str = r#"{
  "name": "roled",
  "main": "main.vyrn",
  "audience": { "server": ["server"], "client": ["client"], "universal": ["app", "shared"] },
  "roles": { "server/api": "std/rpc:Api", "app/routes": "std/ui:Page" }
}
"#;

#[test]
fn api_attaches_by_role_and_why_reports_it() {
    let dir = scratch("attach");
    write(&dir, "vyrn.json", MANIFEST);
    write(
        &dir,
        "server/api/pastes.vyrn",
        "export type Req = { id: Int64 }\nexport fn list(req: Req) -> Req {\n    return req\n}\n",
    );
    write(&dir, "main.vyrn", "fn main() -> Int64 {\n    return 0\n}\n");

    let out = vyrn().arg("why").arg("--contract").arg(dir.join("server/api/pastes.vyrn")).output().unwrap();
    assert!(out.status.success(), "`why` reports; it does not gate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("path segments `server/api`"), "{text}");
    assert!(text.contains("contract: Api (std/rpc)"), "{text}");
    // The open rule admits the application's own vocabulary.
    assert!(text.contains("list: matches the open rule"), "{text}");
}

#[test]
fn the_same_inner_segment_under_a_different_audience_is_a_different_role() {
    let dir = scratch("compose");
    write(&dir, "vyrn.json", MANIFEST);
    write(&dir, "client/api/thing.vyrn", "export fn go() -> Int64 {\n    return 1\n}\n");
    write(&dir, "main.vyrn", "fn main() -> Int64 {\n    return 0\n}\n");

    let out = vyrn().arg("why").arg("--contract").arg(dir.join("client/api/thing.vyrn")).output().unwrap();
    // `client/api` is in no declared role: the run `server/api` does not match.
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no contract: this file is in no role"), "{text}");
}

/// A procedure whose PARAMETER cannot cross the wire, named against the rule.
#[test]
fn a_non_serializable_parameter_is_a_named_error() {
    let dir = scratch("param");
    write(
        &dir,
        "api.vyrn",
        "export type Req = { id: Int64 }\nexport fn go(xs: Array<Int64>) -> Req {\n    return Req { id: xs[0] }\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { rpcServer } from \"std/rpc\"\n\
         import { rpcHandle } from rpcServer(\"./api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("go__parameter_Array_Int64__is_not_serializable"), "{err}");
    assert!(err.contains("a_procedure_parameter_must_be_an_exported_named_type"), "{err}");
}

/// And the RETURN, which nothing checked before RFC-0072 M2 — the server would
/// emit a `null` schema for it and the client would fail to decode at run time.
#[test]
fn a_non_serializable_return_is_a_named_error() {
    let dir = scratch("ret");
    write(
        &dir,
        "api.vyrn",
        "export type Req = { id: Int64 }\nexport fn go(req: Req) -> Array<Int64> {\n    return [req.id]\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { rpcServer } from \"std/rpc\"\n\
         import { rpcHandle } from rpcServer(\"./api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("go__return_Array_Int64__is_not_serializable"), "{err}");
    assert!(err.contains("a_procedure_must_return_an_exported_named_type_or_nothing"), "{err}");
}

/// A `Unit` return stays legal: nothing crosses the wire, which is the 204.
#[test]
fn a_unit_return_is_serializable() {
    let dir = scratch("unit");
    write(
        &dir,
        "api.vyrn",
        "export type Req = { id: Int64 }\nexport fn go(req: Req) {\n    print(\"\\{req.id}\")\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { rpcServer } from \"std/rpc\"\n\
         import { rpcHandle } from rpcServer(\"./api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

/// RFC-0072 M2 closes the item RFC-0071 M2b, M2c and M4 each recorded and each
/// left open: a `.vyrn` page's own surface carries `page`/`respond`, so the
/// CLOSED rule could not be applied there. `Page` names them now, so a typo in a
/// `.vyrn` page's exports is as loud as one in a `.vyx` page's.
#[test]
fn a_vyrn_page_gets_the_closed_rule_too() {
    let dir = scratch("closed");
    write(
        &dir,
        "routes/index.vyrn",
        "import { el, text, Html } from \"std/html\"\n\
         export fn page() -> Html {\n    return el(\"h1\", [], [text(\"hi\")])\n}\n\
         export fn hedd() -> Html {\n    return el(\"h1\", [], [text(\"x\")])\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./routes\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success(), "an export the contract does not name must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("contract_unknown_didYouMean__hedd"), "{err}");
}

/// …and an honest `.vyrn` page still compiles, because `page` and `respond` are
/// now members rather than surface the contract could not account for.
#[test]
fn an_ordinary_vyrn_page_still_satisfies_the_contract() {
    let dir = scratch("ok");
    write(
        &dir,
        "routes/index.vyrn",
        "import { el, text, Html } from \"std/html\"\n\
         export fn page() -> Html {\n    return el(\"h1\", [], [text(\"hi\")])\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./routes\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
