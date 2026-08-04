//! Integration tests for RFC-0072 M3 — derived RPC paths, driven through the
//! real `vyrn` binary over a real project tree.
//!
//! The milestone's claim is that nothing in the tree declares a route: the path
//! is a total function of the module's api-relative path and the export's name.
//! So the tests build a directory, mount it with `rpc(dir)`, and assert on what
//! a caller sees — the router's own answers over the wire shapes, the table
//! `vyrn routes` prints, and the build failures a colliding path produces.
//!
//! `client(dir)`'s server-blindness gets a mechanical test rather than a
//! rhetorical one: the emitted module is inspected for any import of an api
//! module and for the text of a procedure body.

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
    let dir = std::env::temp_dir().join(format!("vyrn_derived_{tag}_{}_{n}", std::process::id()));
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
  "name": "derived",
  "server": "server.vyrn",
  "client": "client/boot.vyrn",
  "audience": { "server": ["server"], "client": ["client"], "universal": ["app", "shared"] },
  "roles": { "server/api": "std/rpc:Api" }
}
"#;

const WIRE: &str = "\
/// A paste id.
export type Id = Int64 where value >= 1
/// A stored paste.
export type Paste = { id: Id, body: String }
/// A page of pastes.
export type PasteList = { items: Array<Paste> }
/// A one-field request.
export type IdReq = { id: Id }
/// The outcome of a lookup.
export type PasteResult = Result<Paste, String>
/// A refund request.
export type RefundReq = { order: Id }
/// A refund outcome.
export type RefundResult = Result<Id, String>
";

const PASTES: &str = "\
import { Paste, PasteList, IdReq, PasteResult } from \"../../shared/wire\"

/// The recent pastes.
export fn recent() -> PasteList {
    return PasteList { items: [Paste { id: 1, body: \"hello\" }] }
}

/// One paste by id.
export fn byId(req: IdReq) -> PasteResult {
    if req.id == 1 {
        return Ok(Paste { id: 1, body: \"hello\" })
    }
    return Err(\"no such paste\")
}
";

const REFUND: &str = "\
import { RefundReq, RefundResult } from \"../../../shared/wire\"

/// Refund one order.
export fn run(req: RefundReq) -> RefundResult {
    return Ok(req.order)
}
";

/// A second module exporting a name `pastes` also exports — the case a flat
/// namespace makes hard and a derived path makes ordinary.
const NOTES: &str = "\
import { Paste, PasteList } from \"../../shared/wire\"

/// The recent notes.
export fn recent() -> PasteList {
    return PasteList { items: [Paste { id: 2, body: \"note\" }] }
}
";

/// The RFC's tree: an api directory with a nested module, a shared wire module,
/// and a server composition root.
fn project(dir: &Path) {
    write(dir, "vyrn.json", MANIFEST);
    write(dir, "shared/wire.vyrn", WIRE);
    write(dir, "server/api/pastes.vyrn", PASTES);
    write(dir, "server/api/notes.vyrn", NOTES);
    write(dir, "server/api/orders/refund.vyrn", REFUND);
}

/// A server root that drives the mounted router directly, printing
/// `status body` per request — the parity story RFC-0016 established.
const DRIVER: &str = "\
import { rpc } from \"std/rpc\"
import { rpcHandle } from rpc(\"./server/api\")

fn show(r: Option<Response>) {
    match r {
        Some(res) => print(\"\\{res.status} \\{res.body}\"),
        None => print(\"not ours\"),
    }
}

fn hit(method: String, path: String, body: String) {
    show(rpcHandle(Request { method: method, path: path, headers: [:], body: body }))
}

fn main() -> Int64 {
    hit(\"POST\", \"/_/pastes/recent\", \"\")
    hit(\"POST\", \"/_/notes/recent\", \"\")
    hit(\"POST\", \"/_/pastes/byId\", \"{\\\"id\\\":1}\")
    hit(\"POST\", \"/_/orders/refund/run\", \"{\\\"order\\\":9}\")
    hit(\"POST\", \"/_/pastes/byId\", \"{\\\"id\\\":0}\")
    hit(\"GET\", \"/_/pastes/recent\", \"\")
    hit(\"POST\", \"/_/nope\", \"\")
    hit(\"GET\", \"/elsewhere\", \"\")
    return 0
}
";

#[test]
fn every_export_is_mounted_at_its_derived_path() {
    let dir = scratch("mount");
    project(&dir);
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("run").arg(dir.join("server.vyrn")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "200 {\"items\":[{\"id\":1,\"body\":\"hello\"}]}");
    // Two modules exporting `recent` coexist, distinguished by `{module}`.
    assert_eq!(lines[1], "200 {\"items\":[{\"id\":2,\"body\":\"note\"}]}");
    assert_eq!(lines[2], "200 {\"Ok\":{\"id\":1,\"body\":\"hello\"}}");
    // A nested directory becomes a nested path segment.
    assert_eq!(lines[3], "200 {\"Ok\":9}");
    // A request the declared type rejects is the same 422 the single-module
    // form produces (RFC-0068), with the server's own issues.
    assert!(lines[4].starts_with("422 {\"issues\""), "{}", lines[4]);
    assert_eq!(lines[5], "405 method not allowed");
    assert_eq!(lines[6], "404 no such procedure");
    assert_eq!(lines[7], "not ours");
}

#[test]
fn routes_prints_the_resolved_table_with_its_source() {
    let dir = scratch("table");
    project(&dir);
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("routes").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("method"), "{text}");
    for (path, proc) in [
        ("/_/pastes/recent", "pastes/recent"),
        ("/_/notes/recent", "notes/recent"),
        ("/_/pastes/byId", "pastes/byId"),
        ("/_/orders/refund/run", "orders/refund/run"),
    ] {
        let row = text.lines().find(|l| l.contains(path)).unwrap_or_else(|| panic!("{path}:\n{text}"));
        assert!(row.contains(proc), "{row}");
        assert!(row.ends_with("convention"), "{row}");
    }
}

/// The table says "every" and has to mean it. `examples/bin` is the measure
/// because it publishes all three producers at once: a derived RPC surface, a
/// hand-written REST projection over the same three procedures, and a stream and
/// a socket beside them. Before this it printed the three `/_/*` rows and
/// nothing else — the missing five were exactly the ones somebody wrote by hand.
///
/// The row COUNT is asserted, not just the presence of each row: a table that
/// grows a phantom is as wrong as one that drops a route, and only the count
/// catches a group counted twice.
#[test]
fn routes_shows_the_hand_written_projection_beside_the_derived_surface() {
    let bin = repo_dir("examples/bin");
    let out = vyrn().arg("routes").arg("server.vyrn").current_dir(&bin).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    // Column widths follow the widest row, so compare on words rather than on
    // the padding.
    let rows: Vec<String> =
        text.lines().skip(1).map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    for want in [
        // The derived surface, unchanged — its rows still come from `//@route`.
        "POST /_/pastes/byId pastes/byId convention",
        "POST /_/pastes/create pastes/create convention",
        "POST /_/pastes/recent pastes/recent convention",
        // The REST projection: written in `server/api/pastes.http.vyrn`, so no
        // generator ever saw these and `source` is neither convention nor
        // override but `explicit`.
        "GET /pastes recent explicit",
        "POST /pastes create explicit",
        "GET /pastes/{id} byId explicit",
        // RFC-0074 M3a/M3b. `SSE`/`WS` and not `GET`, because that is the word
        // the value's own `derived` line uses for itself; a `Live` carries no
        // handler name, hence `-`.
        "SSE /pastes/live - explicit",
        "WS /pastes/socket - explicit",
        // Both page trees, mounted as ordinary groups. `convention` and not
        // `explicit`: the file tree derived these paths, nobody wrote them down.
        // The procedure column names the page's place in the tree, which is the
        // only name a page has. `/raw/{id}` is the whole path and not the
        // tree-relative `/{id}` — the tree contains `raw/[id].vyrn`.
        "GET / index convention",
        "GET /about about convention",
        "GET /p/{id} p/[id] convention",
        "GET /raw/{id} raw/[id] convention",
    ] {
        assert!(rows.iter().any(|r| r == want), "missing `{want}`:\n{text}");
    }
    assert_eq!(rows.len(), 12, "{text}");
    // The `surface(\"/_\", rpcHandle)` the same `mount` call carries is NOT a
    // thirteenth row: a prefix stands for a subsystem the directives already list
    // one row at a time, and printing both would double-count it.
    assert!(!text.contains("/_ "), "{text}");
}

/// RFC-0073 M4: `--json` is the same table plus each route's DECLARATION, read
/// from the symbol map the mounting generator baked in — the same reader the
/// LSP's hover and route lenses use, which is what makes "`vyrn routes --json`
/// and the LSP agree" a fact about the source rather than a promise.
#[test]
fn routes_json_carries_the_declaration_each_path_came_from() {
    let dir = scratch("json");
    project(&dir);
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("routes").arg(dir.join("server.vyrn")).arg("--json").output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let doc = vyrn_frontend::schema::parse_json(&text).unwrap_or_else(|e| panic!("{e}:
{text}"));
    let vyrn_frontend::schema::Json::Arr(rows) = doc else { panic!("an array:
{text}") };
    let row = rows
        .iter()
        .find(|r| r.get("path").and_then(|v| v.as_str()) == Some("/_/pastes/byId"))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(row.get("method").and_then(|v| v.as_str()), Some("POST"));
    assert_eq!(row.get("source").and_then(|v| v.as_str()), Some("convention"));
    let origin = row.get("origin").unwrap_or_else(|| panic!("{text}"));
    assert_eq!(origin.get("name").and_then(|v| v.as_str()), Some("byId"));
    assert!(
        origin.get("file").and_then(|v| v.as_str()).is_some_and(|f| f.ends_with("pastes.vyrn")),
        "{text}"
    );
    // A line and a column the text table has nowhere to put — the whole reason
    // the JSON reads the map rather than re-formatting the directives.
    assert!(matches!(origin.get("line"), Some(vyrn_frontend::schema::Json::Num(n)) if *n > 0.0), "{text}");
    assert!(matches!(origin.get("col"), Some(vyrn_frontend::schema::Json::Num(n)) if *n > 0.0), "{text}");
    // Both channels list the same paths: the JSON is a union, and today the
    // directives and the maps come from one generator over one route list.
    let table = vyrn().arg("routes").arg(dir.join("server.vyrn")).output().unwrap();
    let table = String::from_utf8_lossy(&table.stdout);
    for r in &rows {
        let p = r.get("path").and_then(|v| v.as_str()).unwrap();
        assert!(table.contains(p), "`{p}` is in the JSON but not the table:
{table}");
    }
}

#[test]
fn a_pinned_path_wins_and_routes_says_it_is_an_override() {
    let dir = scratch("pin");
    project(&dir);
    write(&dir, "server/api/rpc.json", "{ \"pin\": { \"pastes/recent\": \"/pastes/latest\" } }\n");
    write(&dir, "server.vyrn", DRIVER.replace("/_/pastes/recent", "/pastes/latest").as_str());
    let out = vyrn().arg("run").arg(dir.join("server.vyrn")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(text.lines().next().unwrap(), "200 {\"items\":[{\"id\":1,\"body\":\"hello\"}]}");

    let out = vyrn().arg("routes").arg(dir.join("server.vyrn")).output().unwrap();
    let table = String::from_utf8_lossy(&out.stdout);
    let row = table.lines().find(|l| l.contains("/pastes/latest")).expect(&table);
    assert!(row.ends_with("override"), "{row}");
    // Everything the pin did not touch keeps the convention.
    let row = table.lines().find(|l| l.contains("/_/notes/recent")).expect(&table);
    assert!(row.ends_with("convention"), "{row}");
}

#[test]
fn a_directory_scope_template_reaches_every_module_under_it() {
    let dir = scratch("template");
    project(&dir);
    // Drop the colliding module so `{name}` alone is well-defined here.
    std::fs::remove_file(dir.join("server/api/notes.vyrn")).unwrap();
    write(&dir, "server/api/rpc.json", "{ \"rpc\": { \"prefix\": \"/internal\", \"path\": \"{name}\" } }\n");
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("routes").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let table = String::from_utf8_lossy(&out.stdout);
    assert!(table.contains("/internal/recent"), "{table}");
    assert!(table.contains("/internal/run"), "{table}");
}

#[test]
fn two_procedures_deriving_one_path_fail_the_build_naming_both() {
    let dir = scratch("clash");
    project(&dir);
    // `{name}` alone collapses `pastes/recent` and `notes/recent`.
    write(&dir, "server/api/rpc.json", "{ \"path\": \"{name}\" }\n");
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("check").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(!out.status.success(), "last-wins is never silent");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("two_procedures_derive_the_same_path"), "{err}");
    assert!(err.contains("notes_recent"), "the first declaration: {err}");
    assert!(err.contains("pastes_recent"), "the second declaration: {err}");
}

#[test]
fn a_pin_onto_an_occupied_path_is_the_same_error() {
    let dir = scratch("pinclash");
    project(&dir);
    write(&dir, "server/api/rpc.json", "{ \"pin\": { \"pastes/byId\": \"/_/notes/recent\" } }\n");
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("check").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("two_procedures_derive_the_same_path"), "{err}");
    assert!(err.contains("pastes_byId"), "{err}");
    assert!(err.contains("notes_recent"), "{err}");
}

/// A client root, calling the generated stubs by their qualified names.
const BOOT: &str = "\
import { client } from \"std/rpc\"
import * as api from client(\"../server/api\")

fn onRecent(res: api.RpcReply<api.PasteList>) {
    match res {
        Done(v) => print(\"done\"),
        Rejected(iss) => print(\"rejected\"),
        Failed(m) => print(\"failed\"),
    }
}

fn main() -> Int64 {
    api.pastesRecent(onRecent)
    api.notesRecent(onRecent)
    return 0
}
";

#[test]
fn the_generated_client_is_ordinary_typechecked_code() {
    let dir = scratch("client");
    project(&dir);
    write(&dir, "client/boot.vyrn", BOOT);
    let out = vyrn().arg("check").arg(dir.join("client/boot.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn client_is_server_blind() {
    let dir = scratch("blind");
    project(&dir);
    write(&dir, "client/boot.vyrn", BOOT);
    let out = vyrn().arg("emit-gen").arg(dir.join("client/boot.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    // Not one import of anything under the api directory — the generated client
    // reads INTERFACES and re-emits types, so there is no edge to follow.
    for line in text.lines().filter(|l| l.trim_start().starts_with("import ")) {
        assert!(!line.contains("server/api"), "the client imported an api module: {line}");
    }
    // And no trace of a procedure BODY: the string literals only the server has.
    assert!(!text.contains("no such paste"), "a procedure body reached the client bundle");
    // What it does carry is the wire types and one stub per procedure.
    assert!(text.contains("export type PasteList"), "{text}");
    assert!(text.contains("export fn pastesRecent"), "{text}");
    assert!(text.contains("export fn notesRecent"), "{text}");
}

/// The audience rule and the generator enforce the same boundary from two
/// directions. This is the other direction: mounting the SERVER surface from a
/// client module is rejected by audience, not by `std/rpc`.
#[test]
fn mounting_the_server_surface_from_a_client_module_is_an_audience_error() {
    let dir = scratch("leak");
    project(&dir);
    write(
        &dir,
        "client/boot.vyrn",
        "import { rpc } from \"std/rpc\"\n\
         import { rpcHandle } from rpc(\"../server/api\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("client/boot.vyrn")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("client/boot.vyrn` is client-only"), "{err}");
    assert!(err.contains("which is server-only"), "{err}");
}

/// The in-process flavor: same stub names, no wire.
#[test]
fn client_in_process_dispatches_directly() {
    let dir = scratch("inproc");
    project(&dir);
    write(
        &dir,
        "server.vyrn",
        "import { PasteList } from \"./shared/wire\"\n\
         import { clientInProcess } from \"std/rpc\"\n\
         import * as api from clientInProcess(\"./server/api\")\n\
         \n\
         fn onRecent(res: api.RpcReply<PasteList>) {\n\
         \x20   match res {\n\
         \x20       Done(v) => print(\"in-process: \\{v.items.length}\"),\n\
         \x20       Rejected(iss) => print(\"rejected\"),\n\
         \x20       Failed(m) => print(\"failed\"),\n\
         \x20   }\n\
         }\n\
         \n\
         fn main() -> Int64 {\n\
         \x20   api.pastesRecent(onRecent)\n\
         \x20   return 0\n\
         }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "in-process: 1");
}

#[test]
fn a_module_vyrn_is_configuration_not_wire_surface() {
    let dir = scratch("modvyrn");
    project(&dir);
    write(&dir, "server/api/module.vyrn", "export fn notAProcedure() -> Int64 {\n    return 1\n}\n");
    write(&dir, "server.vyrn", DRIVER);
    let out = vyrn().arg("routes").arg(dir.join("server.vyrn")).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let table = String::from_utf8_lossy(&out.stdout);
    assert!(!table.contains("notAProcedure"), "a config module must not be mounted:\n{table}");
}
