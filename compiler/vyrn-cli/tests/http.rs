//! Integration tests for RFC-0074 — `std/http`, the REST projection, driven
//! through the real `vyrn` binary over real project trees.
//!
//! M2's claims are at the bottom: the policy reaches the response, a conditional
//! request is answered 304 with no body and no media type, an absence the route
//! named is a 404 carrying the codec's own error, and the validator is the same
//! in a second process (which is the difference between a working `If-None-Match`
//! and a feature that silently never fires).
//!
//! M1's four claims, one test each:
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
// RFC-0074 M1 wire types, plus the two M2 needs.
/// A note id.
export type Id = Int64 where value >= 1
/// A stored note.
export type Note = { id: Id, body: String }
/// A lookup by id.
export type IdReq = { id: Id }
/// A page of notes.
export type NoteList = { items: Array<Note> }
/// A note with the wall-clock stamp `lastModified` reads.
export type Stamped = { id: Id, at: Int64 }
/// A fallible lookup, so `notFoundWhen` has an `Err` to read and `createdAt` has
/// an `Ok` payload to unwrap.
export type StampedResult = Result<Stamped, String>
";

const API: &str = "\
import { Id, Note, IdReq, NoteList, Stamped, StampedResult } from \"./wire\"

/// Every note.
export fn recent() -> NoteList {
    return NoteList { items: [Note { id: 1, body: \"first\" }] }
}

/// One note by id.
export fn byId(req: IdReq) -> Note {
    return Note { id: req.id, body: \"note\\{req.id}\" }
}

/// One stamped note; `Err` when there is no such note. The stamp is a constant
/// so the test can assert the exact `Last-Modified` byte for byte.
export fn stamped(req: IdReq) -> StampedResult {
    if req.id > 99 {
        return Err(\"no such note\")
    }
    return Ok(Stamped { id: req.id, at: 1785332610782 })
}
";

/// A project with `notes.vyrn` procedures and a `notes.http.vyrn` projection
/// whose route list is `routes`.
fn project(tag: &str, routes: &str) -> PathBuf {
    project_using(tag, "http, Route, GET, POST", routes)
}

/// [`project`] with the `std/http` import list spelled out, for the M2
/// combinators (imports are explicit in Vyrn, and an unused one is noise).
fn project_using(tag: &str, imports: &str, routes: &str) -> PathBuf {
    let dir = scratch(tag);
    write(&dir, "wire.vyrn", WIRE);
    write(&dir, "notes.vyrn", API);
    write(
        &dir,
        "notes.http.vyrn",
        &format!(
            "import {{ {imports} }} from \"std/http\"\n\
             import {{ recent, byId, stamped }} from http(\"./notes\")\n\
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
    return match mount(Request { method: method, path: path, headers: [:], body: \"\" }, groups, [], []) {
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

// ---- M2: cache, validators, conditionals -----------------------------------

/// A driver that mounts the projection and prints one labelled answer per line:
/// `<status> | <contentType> | <ETag> | <Cache-Control> | <Last-Modified> |
/// <Location> | <body>`. One shape for every case keeps the assertions below
/// about policy rather than about parsing.
const DRIVER: &str = "\
import { mount, Route } from \"std/http\"
import * as api from \"./notes.http\"

fn hit(method: String, path: String, body: String, headers: Map<String, String>) -> Response {
    return match mount(Request { method: method, path: path, headers: headers, body: body }, [api.routes()], [], []) {
        Some(r) => r,
        None => Response { status: 0, contentType: \"\", body: \"\", vary: \"\", headers: [:] },
    }
}

fn hdr(r: Response, name: String) -> String {
    return match r.headers[name] {
        Some(v) => v,
        None => \"\",
    }
}

fn show(label: String, r: Response) {
    print(\"\\{label} | \\{r.status} | \\{r.contentType} | \\{hdr(r, \"ETag\")} | \\{hdr(r, \"Cache-Control\")} | \\{hdr(r, \"Last-Modified\")} | \\{hdr(r, \"Location\")} | \\{r.body}\")
}

fn main() -> Int64 {
    let one = hit(\"GET\", \"/notes/7\", \"\", [:])
    show(\"plain\", one)
    show(\"conditional\", hit(\"GET\", \"/notes/7\", \"\", [\"if-none-match\": hdr(one, \"ETag\")]))
    show(\"weak\", hit(\"GET\", \"/notes/7\", \"\", [\"if-none-match\": \"W/\" + hdr(one, \"ETag\")]))
    show(\"star\", hit(\"GET\", \"/notes/7\", \"\", [\"if-none-match\": \"*\"]))
    show(\"stale\", hit(\"GET\", \"/notes/7\", \"\", [\"if-none-match\": \"\\\"0\\\"\"]))
    show(\"since\", hit(\"GET\", \"/notes/7\", \"\", [\"if-modified-since\": hdr(one, \"Last-Modified\")]))
    // RFC 9110 13.1.3: a present If-None-Match makes If-Modified-Since irrelevant.
    show(\"both\", hit(\"GET\", \"/notes/7\", \"\", [\"if-none-match\": \"\\\"0\\\"\", \"if-modified-since\": hdr(one, \"Last-Modified\")]))
    show(\"absent\", hit(\"GET\", \"/notes/500\", \"\", [:]))
    show(\"created\", hit(\"POST\", \"/notes\", \"{\\\"id\\\":5}\", [:]))
    return 0
}
";

/// The full M2 chain over one fallible, timestamped procedure.
fn m2_project(tag: &str) -> PathBuf {
    let dir = project_using(
        tag,
        "http, Policy, Route, GET, POST",
        "[\n        GET(stamped(\"/{id}\")).cacheFor(60).etag().lastModified(\"at\").notFoundWhen(|why| why == \"no such note\"),\n        \
         POST(stamped(\"/\")).createdAt(\"/notes/{id}\"),\n    ]",
    );
    write(&dir, "driver.vyrn", DRIVER);
    dir
}

/// One driver line as its fields.
fn line<'a>(out: &'a str, label: &str) -> Vec<&'a str> {
    let row = out
        .lines()
        .find(|l| l.starts_with(&format!("{label} |")))
        .unwrap_or_else(|| panic!("no `{label}` line in:\n{out}"));
    row.split(" | ").collect()
}

#[test]
fn the_policy_reaches_the_response_and_a_conditional_gets_a_bodyless_304() {
    let dir = m2_project("m2");
    let (ok, out) = run(&dir, "driver.vyrn");
    assert!(ok, "the M2 driver must run:\n{out}");

    // The declared policy, on the answer.
    let plain = line(&out, "plain");
    assert_eq!(plain[1], "200", "{out}");
    assert_eq!(plain[4], "max-age=60", "bare max-age, neither public nor private:\n{out}");
    assert!(plain[3].starts_with('"') && plain[3].ends_with('"'), "a quoted strong ETag:\n{out}");
    // `at` lives inside the `Ok` payload; reading it is what makes the validator
    // work on a fallible procedure at all.
    assert_eq!(plain[5], "Wed, 29 Jul 2026 13:43:30 GMT", "IMF-fixdate:\n{out}");

    // The acceptance criterion: 304, no body, and no media type for the body it
    // does not have — with the validators the client will send back next time.
    for label in ["conditional", "weak", "star"] {
        let f = line(&out, label);
        assert_eq!(f[1], "304", "{label}:\n{out}");
        assert_eq!(f[2], "", "a 304 declares no Content-Type ({label}):\n{out}");
        assert_eq!(f[7], "", "a 304 carries no body ({label}):\n{out}");
        assert_eq!(f[3], plain[3], "the 304 keeps its validator ({label}):\n{out}");
        assert_eq!(f[4], "max-age=60", "and its freshness ({label}):\n{out}");
    }

    // A validator we never issued is not a match.
    let stale = line(&out, "stale");
    assert_eq!(stale[1], "200", "{out}");
    assert_eq!(stale[7], plain[7], "{out}");

    // `If-Modified-Since` alone is honored; alongside a NON-matching
    // `If-None-Match` it is ignored, or a mismatched validator would still 304.
    assert_eq!(line(&out, "since")[1], "304", "{out}");
    assert_eq!(line(&out, "both")[1], "200", "RFC 9110 13.1.3:\n{out}");
}

#[test]
fn not_found_when_reads_the_codecs_own_error_and_created_at_implies_201() {
    let dir = m2_project("m2b");
    let (ok, out) = run(&dir, "driver.vyrn");
    assert!(ok, "{out}");

    // The absence the route named is a 404 — carrying the same `{"Err":…}` the
    // derived RPC surface answers with, because there is one codec.
    let absent = line(&out, "absent");
    assert_eq!(absent[1], "404", "{out}");
    assert_eq!(absent[7], "{\"Err\":\"no such note\"}", "{out}");
    // No cache directives on an error: a 4xx is not a representation.
    assert_eq!(absent[3], "", "{out}");
    assert_eq!(absent[4], "", "{out}");

    // `createdAt` fills its template from the created object's own fields, which
    // for a `Result`-returning procedure sit inside the `Ok` payload.
    let made = line(&out, "created");
    assert_eq!(made[1], "201", "{out}");
    assert_eq!(made[6], "/notes/5", "{out}");
}

#[test]
fn an_etag_is_the_same_in_a_second_process() {
    // The whole feature rests on this: a validator that changed per process (a
    // seeded hash, a start time, a counter) would make every `If-None-Match` miss
    // and nobody would ever see it fail — the response would just always be a 200.
    let dir = m2_project("m2c");
    let (ok, first) = run(&dir, "driver.vyrn");
    assert!(ok, "{first}");
    let (ok2, second) = run(&dir, "driver.vyrn");
    assert!(ok2, "{second}");
    let a = line(&first, "plain")[3].to_string();
    assert!(!a.is_empty(), "{first}");
    assert_eq!(a, line(&second, "plain")[3], "same content, second process, same tag");
}

#[test]
fn the_derived_line_carries_the_policy() {
    // M1 left `derived` holding the route line and promised M2's combinators would
    // append to it. Its one reader is the shadow diagnostic, so that is where the
    // policy shows up today — `vyrn routes` still cannot see an explicit route at
    // all (see the RFC's M1 note; M2 did not change that).
    let dir = project_using(
        "m2derived",
        "mount, surface, http, Policy, Route, GET, POST",
        "[GET(byId(\"/{id}\")).cacheFor(60).etag()]",
    );
    write(
        &dir,
        "root.vyrn",
        "import { mount, surface, Route } from \"std/http\"\n\
         import * as api from \"./notes.http\"\n\
         \n\
         fn all(req: Request) -> Option<Response> {\n    return None\n}\n\
         \n\
         fn main() -> Int64 {\n    \
         let r = mount(Request { method: \"GET\", path: \"/x\", headers: [:], body: \"\" }, [[surface(\"/\", all)], api.routes()], [], [])\n    \
         return 0\n}\n",
    );
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(!ok, "the shadowed route must trap:\n{out}");
    assert!(out.contains("max-age=60 etag"), "the policy is in the route line:\n{out}");
}

// The wire end of M2 — the header map on the socket, and a 304 with neither body
// nor `Content-Type` — is pinned in `tests/serve.rs`
// (`a_304_carries_its_validators_and_neither_body_nor_content_type`), over the
// serve harness that file already owns. A second `vyrn serve` harness here was
// written and removed: it could not read a response from the child it spawned on
// this host, while the identical pattern in `serve.rs` and `rpc.rs` can. Rather
// than ship a test that fails for a reason that is not about `std/http`, the
// wire claims live in one place and the policy claims live here.

// ---- M3a: `sse` is not a `Route`, and `mount` takes both --------------------
//
// The wire end of this — the header block, the frames, the 204, and the
// disconnect — is pinned in `tests/serve.rs`, over the serve harness that file
// owns (see the note above). What is pinned here is what `std/http` VALUES do:
// that a stream resolves before the buffered groups, that `retryAfter` and
// `resumable` reach the answer, that `Last-Event-ID` becomes the producer's
// seed, and that `event` writes the frame SSE actually specifies.

/// A root that mounts one buffered group and one stream, and prints what a
/// request resolves to. `serveStream` is a serving-host call, so a `vyrn run`
/// cannot pull the stream — but it CAN show which shape answered and with what
/// prologue, which is the routing claim.
const LIVE_ROOT: &str = "\
import { mount, surface, event, sse, ws, Frames, Live, Route, Socket, Wire } from \"std/http\"
import * as api from \"./notes.http\"

fn under(req: Request) -> Option<Response> {
    return None
}

fn feedStep(c: Ref<Int64>) -> Option<String> {
    let n = get(c)
    if n >= 3 {
        return None
    }
    set(c, n + 1)
    return Some(event(\"\\{n}\", \"note\", \"line\\{n}\"))
}

fn feed(req: Request, ps: Map<String, String>, since: Int64) -> Stream<String> {
    return fromStep(since, feedStep)
}

fn feeds() -> Array<Live> {
    return [sse(\"/notes/live\", feed).retryAfter(2500).resumable()]
}

fn sockets() -> Array<Socket> {
    return [ws(\"/notes/socket\", feed).closeCode(1001).subprotocol(\"notes.v1\").maxFrame(4096)]
}

fn hit(path: String, lastId: String) -> String {
    let req = Request { method: \"GET\", path: path, headers: [\"last-event-id\": lastId], body: \"\" }
    return match mount(req, [[surface(\"/_\", under)], api.routes()], feeds(), sockets()) {
        Some(r) => \"\\{r.status} \\{r.contentType} [\\{r.body}]\",
        None => \"none\",
    }
}

fn main() -> Int64 {
    print(feeds()[0].derived)
    print(sockets()[0].derived)
    print(event(\"7\", \"note\", \"a\\nb\"))
    print(hit(\"/notes\", \"\"))
    return 0
}
";

#[test]
fn a_stream_carries_its_own_vocabulary_and_never_policys() {
    let dir = project_using("live", "http, Route, GET, POST", "[GET(recent(\"/\"))]");
    write(&dir, "root.vyrn", LIVE_ROOT);
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(ok, "the live root must run:\n{out}");
    // `derived` is the stream's own line: no `max-age`, no `etag`, because a
    // `Live` has no `Policy` to write one with.
    assert!(out.contains("SSE /notes/live retry=2500 resumable"), "{out}");
    // M3b's line beside it, and the point is which words are NOT in each: a
    // `Live` has no close code and a `Socket` has no retry hint, because an
    // option meaningless to a transport is absent from it rather than ignored.
    assert!(out.contains("WS /notes/socket close=1001 subprotocol=notes.v1 max-frame=4096"), "{out}");
    // The frame SSE specifies: a field per line, a `data:` per payload line
    // (a raw newline inside one would end the event), and a blank line to close.
    assert!(out.contains("id: 7\nevent: note\ndata: a\ndata: b\n"), "{out}");
    // The buffered group still answers everything it did before.
    assert!(out.contains("200 application/json"), "{out}");
}

#[test]
fn a_stream_route_that_shadows_a_buffered_one_is_a_startup_error() {
    // The stream resolves first, so `/notes/{id}` behind it would be dead for
    // every id — which is exactly the shape `mount` refuses between groups.
    let dir = project_using("liveshadow", "http, Route, GET, POST", "[GET(byId(\"/{id}\"))]");
    write(&dir, "root.vyrn", &LIVE_ROOT.replace("\"/notes/live\"", "\"/notes/{id}\""));
    let (ok, out) = run(&dir, "root.vyrn");
    assert!(!ok, "the shadowed route must trap:\n{out}");
    assert!(out.contains("is unreachable"), "{out}");
    assert!(out.contains("the stream route /notes/{id}"), "{out}");
}
