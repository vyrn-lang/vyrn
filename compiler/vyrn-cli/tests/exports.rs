//! Integration tests for the RFC-0038 contract-export generators — `std/connect`
//! (Connect wire compat), `std/openapi` (OpenAPI 3.1), and `std/graphql` (SDL) —
//! driven through the real `vyrn` binary over a self-contained fixture contract.
//!
//! The fixture exercises every axis the RFC asks a golden to cover: imported wire
//! types reached through the RFC-0031 closure, validated scalars, a `Result`
//! return, a `Map` field, a payload enum AND a nullary enum, and `///` docs.
//!
//! Coverage:
//!   * `emit-gen` the connect server/client and assert the synthesized surface;
//!   * `run` the OpenAPI document and assert it is well-formed 3.1 JSON, then
//!     generate it twice and assert byte-equality (determinism);
//!   * `run` the GraphQL SDL, assert a grammar sanity check (balanced braces +
//!     known keyword shapes — no new dependency), then assert determinism.
//!
//! Generation runs with the cache disabled so a stale entry never masks a
//! regression.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use vyrn_frontend::schema::{parse_json, Json};

/// Ordered keys of a JSON object (the parser preserves insertion order).
fn obj_keys(j: &Json) -> Vec<String> {
    match j {
        Json::Obj(fields) => fields.iter().map(|(k, _)| k.clone()).collect(),
        other => panic!("expected a JSON object, got {other:?}"),
    }
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh fixture directory holding `wire.vyrn` + `contract.vyrn` and the four
/// generator roots. Returned so a test can point `vyrn` at a specific root.
fn fixture() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_exports_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir.join("wire.vyrn"), WIRE);
    write(&dir.join("contract.vyrn"), CONTRACT);
    write(&dir.join("connect_server.vyrn"), CONNECT_SERVER_ROOT);
    write(&dir.join("connect_client.vyrn"), CONNECT_CLIENT_ROOT);
    write(&dir.join("oa.vyrn"), OA_ROOT);
    write(&dir.join("gql.vyrn"), GQL_ROOT);
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

fn emit_gen(root: &Path) -> String {
    let out = vyrn().arg("emit-gen").arg(root).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn run(root: &Path) -> String {
    let out = vyrn().arg("run").arg(root).output().expect("run");
    assert!(out.status.success(), "run failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

const WIRE: &str = r#"/// A user id (positive).
export type UserId = Int64 where value >= 1
/// A bounded display name.
export type UserName = String where value.byteLength >= 1 && value.byteLength <= 40
/// A shape with payload and nullary variants.
export type Shape = | Circle(Int64) | Rect(Int64, Int64) | Dot
/// A nullary-only colour.
export type Colour = | Red | Green | Blue
/// A stored user.
export type User = { id: UserId, name: UserName, colour: Colour }
/// A create request.
export type CreateReq = { name: UserName }
/// An application outcome.
export type UserResult = Result<User, String>
/// Name -> count.
export type Tally = Map<String, Int64>
"#;

const CONTRACT: &str = r#"import { UserId, UserName, User, CreateReq, UserResult, Tally, Shape } from "./wire"
/// A fetch-by-id request.
export type IdReq = { id: UserId }
/// Fetch a user by id.
export fn getUser(req: IdReq) -> UserResult {
    return Err("nope")
}
/// Create a user.
export fn createUser(req: CreateReq) -> UserResult {
    return Err("nope")
}
/// The whole tally.
export fn listTally() -> Tally {
    let m: Map<String, Int64> = [:]
    return m
}
/// Echo the canonical shape.
export fn getShape() -> Shape {
    return Dot
}
"#;

const CONNECT_SERVER_ROOT: &str = r#"import { connectServer } from "std/connect"
import { connectHandle } from connectServer("./contract")
fn main() -> Int64 {
    return 0
}
"#;

const CONNECT_CLIENT_ROOT: &str = r#"import { connectClient } from "std/connect"
import { getUser } from connectClient("./contract")
fn main() -> Int64 {
    return 0
}
"#;

const OA_ROOT: &str = r#"import { openapi } from "std/openapi"
import { openapiJson } from openapi("./contract")
fn main() -> Int64 {
    print(openapiJson())
    return 0
}
"#;

const GQL_ROOT: &str = r#"import { sdl } from "std/graphql"
import { sdlText } from sdl("./contract")
fn main() -> Int64 {
    print(sdlText())
    return 0
}
"#;

// ---- std/connect: the synthesized server surface ---------------------------

#[test]
fn emit_gen_connect_server_shows_the_router_and_dispatchers() {
    let dir = fixture();
    let src = emit_gen(&dir.join("connect_server.vyrn"));
    // Imports the procedures (and the contract's own `IdReq`) from the contract,
    // and the closure types from wire.
    assert!(src.contains("import { getUser, createUser, listTally, getShape"), "procedures imported:\n{src}");
    assert!(src.contains("} from \"./contract\""), "contract import:\n{src}");
    assert!(src.contains("from \"./wire\""), "wire types imported:\n{src}");
    // The Connect error envelope + the two error builders.
    assert!(src.contains("type ConnectError = { code: String, message: String, details: Array<Issue> }"), "{src}");
    assert!(src.contains("code: \"invalid_argument\""), "invalid_argument builder:\n{src}");
    assert!(src.contains("\\\"unimplemented\\\"") || src.contains("\"unimplemented\""), "unimplemented:\n{src}");
    // A validated request decodes and a Result return is a 200 (RFC-0024).
    assert!(src.contains("fn connectDispatchGetUser(body: String) -> Response"), "{src}");
    assert!(src.contains("Valid(input) => Response { status: 200, contentType: \"application/json\", body: toJson(getUser(input)) }"), "{src}");
    assert!(src.contains("Invalid(issues) => connectFail400(issues)"), "{src}");
    // The router uses the Connect path shape `/contract.<Proc>` and mounts as an
    // Option-returning handler (beside rpcHandle).
    assert!(src.contains("export fn connectHandle(req: Request) -> Option<Response>"), "{src}");
    assert!(src.contains("req.method == \"POST\" && req.path == \"/contract.getUser\""), "{src}");
    assert!(src.contains("if req.path.startsWith(\"/contract.\")"), "unknown-proc prefix:\n{src}");
    // A zero-parameter procedure ignores the body.
    assert!(src.contains("fn connectDispatchListTally(body: String) -> Response"), "{src}");
}

#[test]
fn emit_gen_connect_client_shows_stubs_dispatchers_and_unify() {
    let dir = fixture();
    let src = emit_gen(&dir.join("connect_client.vyrn"));
    // The contract's types re-emitted verbatim (the client links no server body).
    assert!(src.contains("export type UserResult = Result<User, String>"), "{src}");
    // One shared transport extern.
    assert!(src.contains("extern fn vyrnConnectCall(path: String, body: String) -> Int64"), "{src}");
    // A same-named stub POSTing to the Connect path, and a completion dispatcher.
    assert!(src.contains("export fn getUser(req: IdReq) {"), "{src}");
    assert!(src.contains("vyrnConnectCall(\"/contract.getUser\", toJson(req))"), "{src}");
    assert!(src.contains("export extern fn connectDoneGetUser(id: Int64, status: Int64, body: String)"), "{src}");
    // The unifier: 200 decode, 400 -> the Connect error's details, transport Issue.
    assert!(src.contains("if status == 200 { return fromJson(UserResult, body) }"), "{src}");
    assert!(src.contains("Valid(err) => Invalid(err.details)"), "{src}");
    assert!(src.contains("procedure `getUser` is unreachable"), "{src}");
}

// ---- std/openapi: a well-formed, deterministic OpenAPI 3.1 document ---------

#[test]
fn openapi_document_is_wellformed_and_deterministic() {
    let dir = fixture();
    let doc = run(&dir.join("oa.vyrn"));
    let doc = doc.trim_end();
    // Deterministic: generate again, byte-equal.
    let again = run(&dir.join("oa.vyrn"));
    assert_eq!(doc, again.trim_end(), "OpenAPI generation must be byte-stable");

    // Parse with the compiler's OWN minimal JSON parser (no new dependency).
    let v = parse_json(doc).expect("OpenAPI must parse as JSON");
    assert_eq!(v.get("openapi").and_then(|j| j.as_str()), Some("3.1.0"));
    assert!(v.get("info").and_then(|i| i.get("title")).and_then(|t| t.as_str()).is_some(), "info.title");
    assert!(v.get("info").and_then(|i| i.get("version")).and_then(|t| t.as_str()).is_some(), "info.version");
    // One path per procedure, in declaration order.
    let paths = v.get("paths").expect("paths");
    assert_eq!(
        obj_keys(paths),
        vec!["/rpc/getUser", "/rpc/createUser", "/rpc/listTally", "/rpc/getShape"]
    );
    // getUser's request refs a component; its 200 refs the Result component; the
    // 422 carries the Issues shape.
    let op = paths.get("/rpc/getUser").and_then(|p| p.get("post")).expect("getUser.post");
    let ref_of = |op: &Json, code: &str| -> String {
        op.get("responses")
            .and_then(|r| r.get(code))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("application/json"))
            .and_then(|c| c.get("schema"))
            .and_then(|s| s.get("$ref"))
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string()
    };
    let req_ref = op
        .get("requestBody")
        .and_then(|b| b.get("content"))
        .and_then(|c| c.get("application/json"))
        .and_then(|c| c.get("schema"))
        .and_then(|s| s.get("$ref"))
        .and_then(|r| r.as_str());
    assert_eq!(req_ref, Some("#/components/schemas/IdReq"));
    assert_eq!(ref_of(op, "200"), "#/components/schemas/UserResult");
    assert!(
        op.get("responses")
            .and_then(|r| r.get("422"))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("application/json"))
            .and_then(|c| c.get("schema"))
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get("issues"))
            .is_some(),
        "422 Issues shape"
    );
    // components/schemas is sorted and carries imported wire types.
    let schemas = v.get("components").and_then(|c| c.get("schemas")).expect("schemas");
    let names = obj_keys(schemas);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "components/schemas keys must be sorted");
    for want in ["UserId", "UserName", "User", "UserResult", "Tally", "Shape", "Colour", "IdReq"] {
        assert!(names.iter().any(|n| n == want), "missing schema {want}: {names:?}");
    }
    // A validated scalar's bound, a Result oneOf, and a Map additionalProperties
    // all survive into components.
    assert_eq!(schemas.get("UserId").and_then(|s| s.get("minimum")), Some(&Json::Num(1.0)));
    assert!(
        matches!(schemas.get("UserResult").and_then(|s| s.get("oneOf")), Some(Json::Arr(_))),
        "Result -> oneOf"
    );
    assert!(
        schemas.get("Tally").and_then(|s| s.get("additionalProperties")).is_some(),
        "Map -> additionalProperties"
    );
    // Each component is $id-scoped so its self-contained $defs refs resolve.
    assert_eq!(schemas.get("User").and_then(|s| s.get("$id")).and_then(|i| i.as_str()), Some("User"));
}

// ---- std/graphql: a grammar-sane, deterministic SDL document ----------------

#[test]
fn graphql_sdl_is_wellformed_and_deterministic() {
    let dir = fixture();
    let sdl = run(&dir.join("gql.vyrn"));
    let again = run(&dir.join("gql.vyrn"));
    assert_eq!(sdl, again, "SDL generation must be byte-stable");

    // Grammar sanity check (no new dependency): balanced braces/parens/brackets,
    // and every block opener is a `type|input|enum Name {` header.
    sdl_grammar_sane(&sdl);
    // Stronger: block-string-aware well-formedness (descriptions are opaque).
    sdl_block_strings_wellformed(&sdl);

    // The honest mappings.
    // A record => type/input pair.
    assert!(sdl.contains("type User {"), "object type:\n{sdl}");
    assert!(sdl.contains("input UserInput {"), "input twin:\n{sdl}");
    // A validated scalar => a custom scalar with its constraint documented.
    assert!(sdl.contains("scalar UserId"), "validated scalar:\n{sdl}");
    assert!(sdl.contains("value >= 1"), "constraint documented:\n{sdl}");
    // A non-Option field is non-null.
    assert!(sdl.contains("id: UserId!"), "non-null field:\n{sdl}");
    // A nullary enum => a real enum; a payload enum + Result => tagged objects.
    assert!(sdl.contains("enum Colour {"), "nullary enum:\n{sdl}");
    assert!(sdl.contains("type Shape {"), "payload enum -> tagged type:\n{sdl}");
    assert!(sdl.contains("Circle: Int"), "single-payload variant:\n{sdl}");
    assert!(sdl.contains("Rect: JSON"), "multi-payload variant -> JSON:\n{sdl}");
    assert!(sdl.contains("Dot: Boolean"), "nullary variant marker:\n{sdl}");
    assert!(sdl.contains("type UserResult {") && sdl.contains("Ok: User") && sdl.contains("Err: String"), "Result -> tagged:\n{sdl}");
    // Map => the documented JSON scalar (named alias => its own scalar).
    assert!(sdl.contains("scalar JSON"), "JSON scalar:\n{sdl}");
    assert!(sdl.contains("scalar Tally"), "named map alias -> scalar:\n{sdl}");
    // Query/Mutation split: get*/list* -> Query, else Mutation.
    let q = sdl.split("type Query {").nth(1).unwrap().split('}').next().unwrap();
    assert!(q.contains("getUser(input: IdReqInput!): UserResult"), "getUser in Query:\n{q}");
    assert!(q.contains("listTally: Tally"), "listTally in Query:\n{q}");
    assert!(q.contains("getShape: Shape"), "getShape in Query:\n{q}");
    let m = sdl.split("type Mutation {").nth(1).unwrap().split('}').next().unwrap();
    assert!(m.contains("createUser(input: CreateReqInput!): UserResult"), "createUser in Mutation:\n{m}");
    // A type's /// doc becomes a description block string.
    assert!(sdl.contains("\"\"\"A stored user.\"\"\""), "type doc -> description:\n{sdl}");
}

/// A validated scalar whose regex predicate carries a `"` (and a `,` and `}`),
/// plus a type whose `///` doc embeds a literal `"""`, previously folded UNESCAPED
/// into a `"""…"""` description and produced INVALID SDL (four consecutive quotes
/// at the Url boundary; a phantom field from the comma). Now sanitized.
#[test]
fn graphql_sdl_escapes_descriptions_and_splits_string_aware() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_gql_torture_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A URL-like scalar (trailing quote in the regex — the shelf `Url` shape) and a
    // scalar whose predicate holds a comma, a brace, and an escaped quote.
    write(
        &dir.join("wire.vyrn"),
        r#"/// A URL: must look like http(s)://…
export type Url = String where value =~ "https?://.+"
/// A weird scalar with a comma, a brace } and a quote in the predicate.
export type Weird = String where value =~ "a,b}c\"d"
/// A record referencing both validated scalars.
export type Rec = { url: Url, weird: Weird }
export type IdReq = { id: Int64 }
"#,
    );
    write(
        &dir.join("contract.vyrn"),
        r#"import { Rec, Url, Weird, IdReq } from "./wire"
/// Fetch a record.
export fn getRec(req: IdReq) -> Rec {
    return Rec { url: "http://x", weird: "y" }
}
"#,
    );
    write(&dir.join("gql.vyrn"), GQL_ROOT);
    let sdl = run(&dir.join("gql.vyrn"));

    // The document must now be valid SDL (was invalid before the fix).
    sdl_block_strings_wellformed(&sdl);
    // The record split into EXACTLY two fields — the predicate's comma did not
    // fabricate a phantom field.
    let rec = sdl.split("type Rec {").nth(1).unwrap().split('}').next().unwrap();
    assert!(rec.contains("url: Url!"), "url field:\n{rec}");
    assert!(rec.contains("weird: Weird!"), "weird field:\n{rec}");
    assert_eq!(rec.matches(':').count(), 2, "exactly two fields:\n{rec}");
    // The trailing-quote description is emitted on its own line (the padded form).
    assert!(sdl.contains("\"\"\"\nA URL: must look like http(s)://… — String where value =~ \"https?://.+\"\n\"\"\""),
        "padded Url description:\n{sdl}");
}

/// A block-string-aware SDL well-formedness check (no new dependency): scans the
/// document as a GraphQL lexer would, treating `"""…"""` descriptions as OPAQUE
/// (their interior braces/quotes are content, not code) and honoring `\"""` as the
/// sole block-string escape. Asserts every block string terminates, no stray quote
/// survives (the old `""""` boundary bug lexes as an unterminated string here), and
/// braces/parens/brackets balance OUTSIDE strings and `#` comments. Stronger than
/// `sdl_grammar_sane` — a description that contains `,` `}` or `"` cannot corrupt it.
fn sdl_block_strings_wellformed(sdl: &str) {
    let b = sdl.as_bytes();
    let n = b.len();
    let (mut i, mut depth, mut paren, mut brack) = (0usize, 0i32, 0i32, 0i32);
    let is_tq = |j: usize| j + 2 < n && b[j] == b'"' && b[j + 1] == b'"' && b[j + 2] == b'"';
    while i < n {
        if b[i] == b'#' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if is_tq(i) {
            i += 3;
            loop {
                assert!(i < n, "unterminated block string in SDL:\n{sdl}");
                if b[i] == b'\\' && i + 3 < n && b[i + 1] == b'"' && b[i + 2] == b'"' && b[i + 3] == b'"' {
                    i += 4; // an escaped `\"""`
                } else if is_tq(i) {
                    i += 3;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'"' {
            // A bare quote outside a block string: the old trailing-quote/`""""`
            // boundary bug lands here and fails to terminate.
            i += 1;
            while i < n && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            assert!(i < n, "stray quote / quadruple-quote boundary in SDL:\n{sdl}");
            i += 1;
        } else {
            match b[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    assert!(depth >= 0, "unbalanced }} in SDL:\n{sdl}");
                }
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => brack += 1,
                b']' => brack -= 1,
                _ => {}
            }
            i += 1;
        }
    }
    assert_eq!(depth, 0, "unbalanced braces in SDL:\n{sdl}");
    assert_eq!(paren, 0, "unbalanced parens in SDL:\n{sdl}");
    assert_eq!(brack, 0, "unbalanced brackets in SDL:\n{sdl}");
}

/// A dependency-free SDL grammar sanity check: brackets balance, and each `{`
/// opens on a `type|input|enum <Name> {` header line while `scalar` lines carry
/// exactly two tokens.
fn sdl_grammar_sane(sdl: &str) {
    let mut depth: i32 = 0;
    let (mut paren, mut brack) = (0i32, 0i32);
    for c in sdl.chars() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                assert!(depth >= 0, "unbalanced }} in SDL");
            }
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => brack += 1,
            ']' => brack -= 1,
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unbalanced braces in SDL");
    assert_eq!(paren, 0, "unbalanced parens in SDL");
    assert_eq!(brack, 0, "unbalanced brackets in SDL");

    for line in sdl.lines() {
        let t = line.trim();
        if t.ends_with('{') {
            let head: Vec<&str> = t.trim_end_matches('{').trim().split_whitespace().collect();
            assert!(
                matches!(head.as_slice(), [kw, _name] if ["type", "input", "enum"].contains(kw)),
                "block opener is not a `type|input|enum Name {{` header: {t:?}"
            );
        }
        if t.starts_with("scalar ") {
            assert_eq!(t.split_whitespace().count(), 2, "scalar line shape: {t:?}");
        }
    }
}
