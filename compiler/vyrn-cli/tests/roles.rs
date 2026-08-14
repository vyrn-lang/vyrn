//! Integration tests for RFC-0072 M2 — roles and the `Api` contract, driven
//! through the real `vyrn` binary.
//!
//! M2 carries RFC-0071's deferred M3, whose remainder — what `Serializable`
//! actually admits — landed here too. Four claims are under test:
//!
//!   1. `std/rpc` declares `Api`, and a project attaches it by ROLE — so a
//!      module nobody has generated from yet can still be asked what governs it.
//!   2. A role scope may span the audience segment, so `server/api` and
//!      `client/api` are different roles rather than one that happened to win.
//!   3. Serializability is checked on BOTH ends of every procedure, and the
//!      failure names the rule rather than the symptom.
//!   4. That check is TOTAL: it is the compiler's own codec rule, reflected, so
//!      a nameable record whose field cannot be encoded is refused at the
//!      declaration — and a `Stream` is refused by name, pointing at `sse`/`ws`.

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

    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("server/api/pastes.vyrn"))
        .output()
        .unwrap();
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
    write(
        &dir,
        "client/api/thing.vyrn",
        "export fn go() -> Int64 {\n    return 1\n}\n",
    );
    write(&dir, "main.vyrn", "fn main() -> Int64 {\n    return 0\n}\n");

    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("client/api/thing.vyrn"))
        .output()
        .unwrap();
    // `client/api` is in no declared role: the run `server/api` does not match.
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("no contract: this file is in no role"),
        "{text}"
    );
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("procedure `go` takes `Array<Int64>`, which cannot cross the wire"),
        "{err}"
    );
    assert!(
        err.contains("it must be a type the contract exports by name"),
        "{err}"
    );
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("procedure `go` returns `Array<Int64>`, which cannot cross the wire"),
        "{err}"
    );
    assert!(
        err.contains("it must be a type the contract exports by name"),
        "{err}"
    );
}

/// RFC-0071 M3. A record is nameable by construction — it is a `type`
/// declaration — so the name test alone said yes to one whose field is a
/// function, and the generated module then failed on `toJson`, at a line in
/// source nobody wrote. Serializability is the compiler's OWN codec rule now,
/// reflected through `ParamInfo.uncodable` / `FnInfo.retUncodable`, so the
/// objection arrives at the declaration with the offender named.
#[test]
fn a_nameable_record_that_cannot_be_encoded_is_refused_at_the_contract() {
    let dir = scratch("codable");
    write(
        &dir,
        "api.vyrn",
        "export type Req = { id: Int64 }\n\
         export type Cb = { f: fn(Int64) -> Int64 }\n\
         fn dbl(x: Int64) -> Int64 {\n    return x * 2\n}\n\
         export fn go(req: Req) -> Cb {\n    return Cb { f: dbl }\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { rpcServer } from \"std/rpc\"\n\
         import { rpcHandle } from rpcServer(\"./api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("procedure `go` returns `Cb`, which cannot cross the wire: `Cb` cannot cross a JSON wire"),
        "{err}"
    );
    // …and it is the contract that objects, not the generated module.
    assert!(
        !err.contains("toJson"),
        "the objection must arrive before generation: {err}"
    );
}

/// A stream is refused on BOTH ends, and by name rather than by falling through
/// the general rule — because it is the one unserializable thing with somewhere
/// else to go (RFC-0074's `sse`/`ws`, from a dotted projection module).
#[test]
fn a_stream_is_refused_by_name_and_pointed_at_sse() {
    for (tag, decl) in [
        (
            "streamret",
            "export fn go(req: Req) -> Stream<Req> {\n    return unfold(0, step)\n}\n",
        ),
        (
            "streamparam",
            "export fn go(s: Stream<Req>) -> Req {\n    drop s\n    return Req { id: 1 }\n}\n",
        ),
    ] {
        let dir = scratch(tag);
        write(
            &dir,
            "api.vyrn",
            &format!(
                "import {{ unfold }} from \"std/stream\"\n\
                 export type Req = {{ id: Int64 }}\n\
                 fn step(c: Ref<Int64>) -> Option<Req> {{\n    return None\n}}\n{decl}"
            ),
        );
        write(
            &dir,
            "main.vyrn",
            "import { rpcServer } from \"std/rpc\"\n\
             import { rpcHandle } from rpcServer(\"./api\")\n\
             fn main() -> Int64 {\n    return 0\n}\n",
        );
        let out = vyrn()
            .arg("check")
            .arg(dir.join("main.vyrn"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{tag}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("`Stream<Req>`"), "{tag}: {err}");
        assert!(
            err.contains("publish a feed with `sse` or `ws`"),
            "{tag}: {err}"
        );
    }
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "an export the contract does not name must fail"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown export `hedd` — did you mean `head`?"),
        "{err}"
    );
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
