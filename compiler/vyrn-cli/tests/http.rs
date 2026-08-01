//! Integration tests for RFC-0074 M1 — `std/http`, the REST projection, driven
//! through the real `vyrn` binary over real project trees.
//!
//! The milestone's four claims, one test each:
//!   * a placeholder that is not a field of the procedure's input type is a
//!     CHECKER error at the call site, with the legal placeholders in the message
//!     (no symbol map, no compiler rule — a generated `String where value =~ …`);
//!   * `mount` resolves groups in order, first match wins, and reports an overlap
//!     BETWEEN groups as a startup error instead of shadowing it silently;
//!   * the base path comes from the stem, and a projection colocated with its
//!     procedures does not join them (`rpc(dir)` skips a dotted stem);
//!   * REST and RPC answer the same procedure with the same bytes, because the
//!     projection decodes and encodes through the same codec rather than a
//!     second wire format.

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
    let dir = std::env::temp_dir().join(format!("vyrn_http_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const WIRE: &str = "\
/// A note id.
export type Id = Int64 where value >= 1
/// A stored note.
export type Note = { id: Id, body: String }
/// A lookup by id.
export type IdReq = { id: Id }
/// A page of notes.
export type NoteList = { items: Array<Note> }
";

const API: &str = "\
import { Id, Note, IdReq, NoteList } from \"./wire\"

/// Every note.
export fn recent() -> NoteList {
    return NoteList { items: [Note { id: 1, body: \"first\" }] }
}

/// One note by id.
export fn byId(req: IdReq) -> Note {
    return Note { id: req.id, body: \"note\\{req.id}\" }
}
";

/// A project with `notes.vyrn` procedures and a `notes.http.vyrn` projection
/// whose route list is `routes`.
fn project(tag: &str, routes: &str) -> PathBuf {
    let dir = scratch(tag);
    write(&dir, "wire.vyrn", WIRE);
    write(&dir, "notes.vyrn", API);
    write(
        &dir,
        "notes.http.vyrn",
        &format!(
            "import {{ http, Route, GET, POST }} from \"std/http\"\n\
             import {{ recent, byId }} from http(\"./notes\")\n\
             \n\
             export fn routes() -> Array<Route> {{\n    return {routes}\n}}\n"
        ),
    );
    dir
}

fn run(dir: &Path, file: &str) -> (bool, String) {
    let out = vyrn().arg("run").arg(dir.join(file)).output().expect("vyrn run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), combined)
}

// ---- placeholder checking --------------------------------------------------

#[test]
fn a_placeholder_that_is_not_a_field_is_a_checker_error() {
    let dir = project("typo", "[GET(byId(\"/{ID}\"))]");
    let out = vyrn().arg("check").arg(dir.join("notes.http.vyrn")).output().expect("vyrn check");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a bad placeholder must fail the check:\n{combined}");
    // The message quotes the pattern that was written and the predicate, whose
    // alternatives ARE the input type's fields — `{id}`, and nothing else.
    assert!(combined.contains("\"/{ID}\" does not satisfy `PathById`"), "{combined}");
    assert!(combined.contains("\\\\{id\\\\}"), "the legal placeholder is in the message:\n{combined}");
}

#[test]
fn the_placeholder_a_field_does_name_is_accepted() {
    let dir = project("ok", "[GET(recent(\"/\")), GET(byId(\"/{id}\"))]");
    let out = vyrn().arg("check").arg(dir.join("notes.http.vyrn")).output().expect("vyrn check");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the good projection must check:\n{combined}");
}

// ---- mounting --------------------------------------------------------------

const ROOT: &str = "\
import { mount, surface, Route } from \"std/http\"
import * as api from \"./notes.http\"

fn under(req: Request) -> Option<Response> {
    return if req.path.startsWith(\"/_/\") { Some(Response { status: 200, contentType: \"text/plain\", body: \"surface\", vary: \"\", headers: [:] }) } else { None }
}

fn hit(method: String, path: String) -> String {
    let groups: Array<Array<Route>> = [[surface(\"/_\", under)], api.routes()]
    return match mount(Request { method: method, path: path, headers: [:], body: \"\" }, groups) {
        Some(r) => \"\\{r.status} \\{r.body}\",
        None => \"none\",
    }
}

fn main() -> Int64 {
    print(hit(\"GET\", \"/_/anything\"))
    print(hit(\"GET\", \"/notes/7\"))
    print(hit(\"GET\", \"/notes\"))
    print(hit(\"GET\", \"/elsewhere\"))
    return 0
}
";

#[test]
fn mount_resolves_groups_in_order_and_the_first_match_wins() {
    let dir = project("order", "[GET(recent(\"/\")), GET(byId(\"/{id}\"))]");
    write(&dir, "root.vyrn", ROOT);
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(ok, "root must run:\n{out}");
    let lines: Vec<&str> = out.lines().collect();
    // The surface owns its prefix; the REST group answers everything under the
    // stem-derived base; nothing claims the rest.
    assert_eq!(lines[0], "200 surface", "{out}");
    assert_eq!(lines[1], "200 {\"id\":7,\"body\":\"note7\"}", "{out}");
    assert_eq!(lines[2], "200 {\"items\":[{\"id\":1,\"body\":\"first\"}]}", "{out}");
    assert_eq!(lines[3], "none", "{out}");
}

#[test]
fn a_route_shadowed_by_an_earlier_group_is_a_startup_error() {
    // The surface is mounted over the very prefix the REST group derives, so
    // every route in the second group is dead. Silence here is the failure mode
    // this check exists for.
    let dir = project("shadow", "[GET(recent(\"/\"))]");
    write(&dir, "root.vyrn", &ROOT.replace("surface(\"/_\", under)", "surface(\"/notes\", under)"));
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(!ok, "a shadowed route must trap:\n{out}");
    assert!(out.contains("is unreachable"), "{out}");
    assert!(out.contains("GET /notes (group 1"), "the shadowed route is named:\n{out}");
    assert!(out.contains("* /notes (group 0)"), "the route that shadows it is named:\n{out}");
}

#[test]
fn a_route_with_no_method_is_a_startup_error() {
    let dir = project("nomethod", "[recent(\"/\")]");
    write(&dir, "root.vyrn", ROOT);
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(!ok, "a methodless route must trap:\n{out}");
    assert!(out.contains("has no method"), "{out}");
}

// ---- what is generated -----------------------------------------------------

#[test]
fn the_base_path_comes_from_the_stem_and_the_codec_is_the_rpc_one() {
    let dir = project("gen", "[GET(byId(\"/{id}\"))]");
    let out =
        vyrn().arg("emit-gen").arg(dir.join("notes.http.vyrn")).output().expect("vyrn emit-gen");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);
    // The stem is the base: nothing in the tree wrote `/notes`.
    assert!(src.contains("httpRoute(\"/notes\" + path"), "{src}");
    // The pattern type carries the placeholders the input type has fields for.
    assert!(
        src.contains("export type PathById = String where value =~ \"([^{}]|\\\\{id\\\\})*\""),
        "{src}"
    );
    // `Id` is `Int64`-based, so the captured segment is passed as a NUMBER.
    assert!(src.contains("httpInput(ps, req.body, [\"id\"])"), "{src}");
    // Decode and encode are the RPC surface's, verbatim — one codec, one 422.
    assert!(src.contains("fromJson(IdReq, httpInput"), "{src}");
    assert!(
        src.contains("Invalid(issues) => Some(Response { status: 422, contentType: \"application/json\", body: toJson(HttpIssues { issues: issues })"),
        "{src}"
    );
}

// ---- the two surfaces agree ------------------------------------------------

#[test]
fn rest_and_rpc_answer_the_same_procedure_with_the_same_bytes() {
    let example = repo_dir("examples/rest.vyrn");
    let out = vyrn()
        .arg("run")
        .arg(&example)
        .current_dir(repo_dir("examples"))
        .output()
        .expect("vyrn run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "examples/rest.vyrn must run:\n{combined}");
    assert!(
        combined.contains("GET /users/7 -> 200 application/json {\"id\":7,\"name\":\"user7\",\"age\":30}"),
        "an Int64 path id binds as a number:\n{combined}"
    );
    assert!(combined.contains("REST body == RPC body: true"), "{combined}");
    assert!(combined.contains("REST type == RPC type: true"), "{combined}");
    assert!(combined.contains("REST status == RPC status: true"), "{combined}");
}

#[test]
fn a_projection_beside_the_procedures_is_not_mounted_as_one() {
    // `rpc("./fullstack/server/api")` scans the directory `users.http.vyrn` now
    // lives in. If the dotted stem were treated as a procedure module, its
    // `routes()` would be on the wire (and `Array<Route>` is not serializable, so
    // generation would fail outright).
    let example = repo_dir("examples/rest.vyrn");
    let out = vyrn().arg("emit-gen").arg(&example).output().expect("vyrn emit-gen");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(src.contains("//@route POST /_/users/byId"), "the derived surface is intact:\n{src}");
    // No procedure namespace for it, and no route to its `routes()` export.
    assert!(!src.contains("from \"./users.http\""), "not scanned as a procedure module:\n{src}");
    assert!(!src.contains("users.http/routes"), "{src}");
}
