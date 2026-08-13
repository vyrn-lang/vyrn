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
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn run(root: &Path) -> String {
    let out = vyrn().arg("run").arg(root).output().expect("run");
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
export mut fn createUser(req: CreateReq) -> UserResult {
    return Err("nope")
}
/// The whole tally.
export fn listTally() -> Tally {
    let m: Map<String, Int64> = [:]
    return m
}
/// Echo the canonical shape. Deliberately without a `get`/`list` prefix: the
/// naming convention RFC-0074 M4a deleted would have made this a Mutation.
export fn shape() -> Shape {
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
    assert!(
        src.contains("import { getUser, createUser, listTally, shape"),
        "procedures imported:\n{src}"
    );
    assert!(
        src.contains("} from \"./contract\""),
        "contract import:\n{src}"
    );
    assert!(
        src.contains("from \"./wire\""),
        "wire types imported:\n{src}"
    );
    // The Connect error envelope + the two error builders.
    assert!(
        src.contains(
            "type ConnectError = { code: String, message: String, details: Array<Issue> }"
        ),
        "{src}"
    );
    assert!(
        src.contains("code: \"invalid_argument\""),
        "invalid_argument builder:\n{src}"
    );
    assert!(
        src.contains("\\\"unimplemented\\\"") || src.contains("\"unimplemented\""),
        "unimplemented:\n{src}"
    );
    // A validated request decodes and a Result return is a 200 (RFC-0024).
    assert!(
        src.contains("fn connectDispatchGetUser(body: String) -> Response"),
        "{src}"
    );
    assert!(src.contains("Valid(input) => Response { status: 200, contentType: \"application/json\", body: toJson(getUser(input)), vary: \"\", headers: [:] }"), "{src}");
    assert!(
        src.contains("Invalid(issues) => connectFail400(issues)"),
        "{src}"
    );
    // The router uses the Connect path shape `/contract.<Proc>` and mounts as an
    // Option-returning handler (beside rpcHandle).
    assert!(
        src.contains("export fn connectHandle(req: Request) -> Option<Response>"),
        "{src}"
    );
    assert!(
        src.contains("req.method == \"POST\" && req.path == \"/contract.getUser\""),
        "{src}"
    );
    assert!(
        src.contains("if req.path.startsWith(\"/contract.\")"),
        "unknown-proc prefix:\n{src}"
    );
    // A zero-parameter procedure ignores the body.
    assert!(
        src.contains("fn connectDispatchListTally(body: String) -> Response"),
        "{src}"
    );
}

#[test]
fn emit_gen_connect_client_shows_stubs_dispatchers_and_unify() {
    let dir = fixture();
    let src = emit_gen(&dir.join("connect_client.vyrn"));
    // The contract's types re-emitted verbatim (the client links no server body).
    assert!(
        src.contains("export type UserResult = Result<User, String>"),
        "{src}"
    );
    // One shared transport extern.
    assert!(
        src.contains("extern fn vyrnConnectCall(path: String, body: String) -> Int64"),
        "{src}"
    );
    // A same-named stub POSTing to the Connect path, and a completion dispatcher.
    assert!(src.contains("export fn getUser(req: IdReq) {"), "{src}");
    assert!(
        src.contains("vyrnConnectCall(\"/contract.getUser\", toJson(req))"),
        "{src}"
    );
    assert!(
        src.contains("export extern fn connectDoneGetUser(id: Int64, status: Int64, body: String)"),
        "{src}"
    );
    // The unifier: 200 decode, 400 -> the Connect error's details, transport Issue.
    assert!(
        src.contains("if status == 200 { return fromJson(UserResult, body) }"),
        "{src}"
    );
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
    assert_eq!(
        doc,
        again.trim_end(),
        "OpenAPI generation must be byte-stable"
    );

    // Parse with the compiler's OWN minimal JSON parser (no new dependency).
    let v = parse_json(doc).expect("OpenAPI must parse as JSON");
    assert_eq!(v.get("openapi").and_then(|j| j.as_str()), Some("3.1.0"));
    assert!(
        v.get("info")
            .and_then(|i| i.get("title"))
            .and_then(|t| t.as_str())
            .is_some(),
        "info.title"
    );
    assert!(
        v.get("info")
            .and_then(|i| i.get("version"))
            .and_then(|t| t.as_str())
            .is_some(),
        "info.version"
    );
    // One path per procedure, in declaration order.
    let paths = v.get("paths").expect("paths");
    assert_eq!(
        obj_keys(paths),
        vec![
            "/rpc/getUser",
            "/rpc/createUser",
            "/rpc/listTally",
            "/rpc/shape"
        ]
    );
    // getUser's request refs a component; its 200 refs the Result component; the
    // 422 carries the Issues shape.
    let op = paths
        .get("/rpc/getUser")
        .and_then(|p| p.get("post"))
        .expect("getUser.post");
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
    let schemas = v
        .get("components")
        .and_then(|c| c.get("schemas"))
        .expect("schemas");
    let names = obj_keys(schemas);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "components/schemas keys must be sorted");
    for want in [
        "UserId",
        "UserName",
        "User",
        "UserResult",
        "Tally",
        "Shape",
        "Colour",
        "IdReq",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "missing schema {want}: {names:?}"
        );
    }
    // A validated scalar's bound, a Result oneOf, and a Map additionalProperties
    // all survive into components.
    assert_eq!(
        schemas.get("UserId").and_then(|s| s.get("minimum")),
        Some(&Json::Num(1.0))
    );
    assert!(
        matches!(
            schemas.get("UserResult").and_then(|s| s.get("oneOf")),
            Some(Json::Arr(_))
        ),
        "Result -> oneOf"
    );
    assert!(
        schemas
            .get("Tally")
            .and_then(|s| s.get("additionalProperties"))
            .is_some(),
        "Map -> additionalProperties"
    );
    // Each component is $id-scoped so its self-contained $defs refs resolve.
    assert_eq!(
        schemas
            .get("User")
            .and_then(|s| s.get("$id"))
            .and_then(|i| i.as_str()),
        Some("User")
    );
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
    // Stronger still: the two rules braces cannot see — no empty body, no name
    // defined twice.
    sdl_definitions_are_valid(&sdl);

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
    assert!(
        sdl.contains("type Shape {"),
        "payload enum -> tagged type:\n{sdl}"
    );
    assert!(
        sdl.contains("Circle: Int"),
        "single-payload variant:\n{sdl}"
    );
    assert!(
        sdl.contains("Rect: JSON"),
        "multi-payload variant -> JSON:\n{sdl}"
    );
    assert!(
        sdl.contains("Dot: Boolean"),
        "nullary variant marker:\n{sdl}"
    );
    assert!(
        sdl.contains("type UserResult {")
            && sdl.contains("Ok: User")
            && sdl.contains("Err: String"),
        "Result -> tagged:\n{sdl}"
    );
    // Map => the documented JSON scalar (named alias => its own scalar).
    assert!(sdl.contains("scalar JSON"), "JSON scalar:\n{sdl}");
    assert!(
        sdl.contains("scalar Tally"),
        "named map alias -> scalar:\n{sdl}"
    );
    // Query/Mutation split: a `mut fn` is a Mutation, everything else a Query
    // (RFC-0074 M4a). `shape` is the pin — no `get`/`list` prefix, so the naming
    // convention this replaced would have filed it under Mutation.
    let q = sdl
        .split("type Query {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(
        q.contains("getUser(input: IdReqInput!): UserResult"),
        "getUser in Query:\n{q}"
    );
    assert!(q.contains("listTally: Tally"), "listTally in Query:\n{q}");
    assert!(
        q.contains("shape: Shape"),
        "unprefixed accessor in Query:\n{q}"
    );
    let m = sdl
        .split("type Mutation {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(
        m.contains("createUser(input: CreateReqInput!): UserResult"),
        "createUser in Mutation:\n{m}"
    );
    assert_eq!(
        m.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "only the `mut fn` is a Mutation:\n{m}"
    );
    // A type's /// doc becomes a description block string.
    assert!(
        sdl.contains("\"\"\"A stored user.\"\"\""),
        "type doc -> description:\n{sdl}"
    );
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
    let rec = sdl
        .split("type Rec {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(rec.contains("url: Url!"), "url field:\n{rec}");
    assert!(rec.contains("weird: Weird!"), "weird field:\n{rec}");
    assert_eq!(rec.matches(':').count(), 2, "exactly two fields:\n{rec}");
    // The trailing-quote description is emitted on its own line (the padded form).
    assert!(sdl.contains("\"\"\"\nA URL: must look like http(s)://… — String where value =~ \"https?://.+\"\n\"\"\""),
        "padded Url description:\n{sdl}");
}

/// A `///` doc is arbitrary prose, and GraphQL block strings define exactly ONE
/// escape — `\"""`. So a doc whose last byte is a backslash turns the CLOSING
/// delimiter into that escape: `"""Ends with a backslash \"""` never terminates,
/// and every definition after it is swallowed until the next `"""` in the
/// document. `graphql-js` on the result: "Syntax Error: Unexpected description,
/// only GraphQL definitions support descriptions."
///
/// A trailing backslash is not exotic — a Windows path, a line-continuation habit,
/// a TeX fragment. The cases here are the ones that live at the delimiter: a
/// trailing `\`, a doc that is ONLY `\`, an embedded `"""`, a literal `\"""`, and
/// a type with no doc at all.
#[test]
fn graphql_sdl_descriptions_close_around_a_trailing_backslash() {
    // The minimal document first: ONE type, and nothing after its description to
    // close the runaway block string. Unfixed, the scan reaches the end of the
    // document without finding a terminator, which is exactly what `graphql-js`
    // reports as "Unexpected description".
    let dir = gql_fixture(
        "/// Ends with a backslash \\\n\
         export type Thing = { name: String }\n\
         export fn getThing() -> Thing { return Thing { name: \"x\" } }\n",
    );
    let sdl = run(&dir.join("gql.vyrn"));
    sdl_definitions_are_valid(&sdl);
    sdl_block_strings_wellformed(&sdl);

    // And the awkward set together, where a LATER description's opener would
    // otherwise close the runaway one and hide it — the definitions in between are
    // swallowed all the same, so they are looked for in the body a lexer sees
    // rather than in the raw bytes.
    let dir = gql_fixture(
        "/// Ends with a backslash \\\n\
         export type Slash = { name: String }\n\
         /// \\\n\
         export type Lone = { a: Int64 }\n\
         /// Holds a \"\"\" triple quote inside\n\
         export type Triple = { b: Int64 }\n\
         /// Holds a literal \\\"\"\" escape inside\n\
         export type Escaped = { c: Int64 }\n\
         export type Plain = { d: Int64 }\n\
         export fn getSlash() -> Slash { return Slash { name: \"x\" } }\n\
         export fn getLone() -> Lone { return Lone { a: 1 } }\n\
         export fn getTriple() -> Triple { return Triple { b: 1 } }\n\
         export fn getEscaped() -> Escaped { return Escaped { c: 1 } }\n\
         export fn getPlain() -> Plain { return Plain { d: 1 } }\n",
    );
    let sdl = run(&dir.join("gql.vyrn"));
    // Both checks: every block string terminates, AND the definitions after each
    // description are still definitions rather than string content.
    sdl_block_strings_wellformed(&sdl);
    sdl_definitions_are_valid(&sdl);
    let body = sdl_without_descriptions(&sdl);
    for name in [
        "type Slash {",
        "type Lone {",
        "type Triple {",
        "type Escaped {",
        "type Plain {",
    ] {
        assert!(
            body.contains(name),
            "{name} was swallowed into a description:\n{sdl}"
        );
    }
    // A backslash-terminated body takes the own-line form the trailing-quote body
    // already took — the newline is what keeps it away from the delimiter.
    assert!(
        sdl.contains("\"\"\"\nEnds with a backslash \\\n\"\"\""),
        "trailing backslash not padded:\n{sdl}"
    );
    assert!(sdl.contains("\"\"\"\n\\\n\"\"\""), "lone backslash:\n{sdl}");
    // An interior `"""` is escaped, and a body that ALREADY holds `\"""` emits
    // `\\"""` — a literal backslash, then the escape — so it round-trips.
    assert!(
        sdl.contains("\"\"\"Holds a \\\"\"\" triple quote inside\"\"\""),
        "embedded triple quote:\n{sdl}"
    );
    assert!(
        sdl.contains("\"\"\"Holds a literal \\\\\"\"\" escape inside\"\"\""),
        "literal escape sequence:\n{sdl}"
    );
    // No doc, no description: the definition sits directly under the one above it.
    assert!(
        sdl.contains("type Plain {") && !sdl.contains("\"\"\"\"\"\""),
        "an absent doc emitted an empty description:\n{sdl}"
    );
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
                if b[i] == b'\\'
                    && i + 3 < n
                    && b[i + 1] == b'"'
                    && b[i + 2] == b'"'
                    && b[i + 3] == b'"'
                {
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
            assert!(
                i < n,
                "stray quote / quadruple-quote boundary in SDL:\n{sdl}"
            );
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

// ---- the executor's own suites, which nothing was running -------------------

/// The path of a repo-relative file, in the loader-parseable spelling.
fn repo_file(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

/// `vyrn test <file>` must be green, and must have run at least `least` blocks —
/// a suite that silently stops being discovered would otherwise pass.
fn assert_suite_green(rel: &str, least: usize) {
    let out = vyrn()
        .arg("test")
        .arg(repo_file(rel))
        .output()
        .expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(combined.contains("0 failed"), "{rel}:\n{combined}");
    let ran: usize = combined
        .split(" passed")
        .next()
        .and_then(|s| s.rsplit(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(
        ran >= least,
        "{rel}: expected at least {least} tests, ran {ran}:\n{combined}"
    );
}

/// The executor's projection/parser suite (RFC-0085 M1–M3): the null-bubbling
/// rule, path attribution and the two type-graph refusals live here, and the SDL
/// goldens above cannot see any of them.
#[test]
fn graphql_executor_unit_tests_run_green() {
    assert_suite_green("std/graphql.vyrn", 20);
}

/// The end-to-end half over `examples/shelf`'s real procedures — partial `data`,
/// a path carrying a list index, and where a `null` stops climbing. The parity
/// harness runs this example's `main` on three engines but never its `test`
/// blocks, so without this the assertions are not checked anywhere.
#[test]
fn graphql_example_unit_tests_run_green() {
    assert_suite_green("examples/graphql.vyrn", 10);
}

// ---- std/graphql: the awkward contract ---------------------------------------

/// A fixture directory holding just `contract.vyrn` and the SDL root.
fn gql_fixture(contract: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_gql_awkward_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir.join("contract.vyrn"), contract);
    write(&dir.join("gql.vyrn"), GQL_ROOT);
    dir
}

/// Generate the SDL for `contract`, expecting the generator to REFUSE it, and
/// return what it said.
fn gql_refusal(contract: &str) -> String {
    let dir = gql_fixture(contract);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("gql.vyrn"))
        .output()
        .expect("run");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the generator emitted a document it cannot define:\n{text}"
    );
    text
}

/// A contract whose types GraphQL cannot express, and a contract whose names
/// GraphQL cannot define — the inputs this generator had never been given.
///
/// Before: a zero-field record emitted `type Empty {}` and `input EmptyInput {}`
/// (the spec requires at least one field, so graphql-js reports "Expected Name,
/// found }") and the invalid types poisoned every field referencing them; a
/// contract type named `Query` emitted a SECOND `type Query`, which no document
/// may do.
///
/// After: an empty body gets the `_placeholder: Boolean` field the query root
/// has always used for the same reason, and a name the document cannot define
/// twice — or one carrying the introspection-reserved `__` — is an RFC-0099
/// `Error` rather than a rename, because these names are the wire surface.
#[test]
fn graphql_sdl_answers_the_awkward_contract_instead_of_emitting_an_invalid_document() {
    // 1. A zero-field record, referenced by another type: repaired, not refused.
    let dir = gql_fixture(
        "/// A record with nothing in it.\n\
         export type Empty = {}\n\
         export type Wrap = { inner: Empty, label: String }\n\
         export fn getWrap() -> Wrap {\n\
         return Wrap { inner: Empty {}, label: \"hi\" }\n\
         }\n",
    );
    let sdl = run(&dir.join("gql.vyrn"));
    sdl_grammar_sane(&sdl);
    sdl_block_strings_wellformed(&sdl);
    sdl_definitions_are_valid(&sdl);
    assert!(
        sdl.contains("type Empty {\n  _placeholder: Boolean\n}"),
        "the empty object is not repaired:\n{sdl}"
    );
    assert!(
        sdl.contains("input EmptyInput {\n  _placeholder: Boolean\n}"),
        "the empty input twin is not repaired:\n{sdl}"
    );
    // The referencing field still names it, so the repair kept the graph whole.
    assert!(sdl.contains("inner: Empty!"), "reference lost:\n{sdl}");

    // 2. A contract type named `Query` — the generated root's name.
    let text = gql_refusal(
        "export type Query = { hits: Int64 }\n\
         export fn getQuery() -> Query { return Query { hits: 1 } }\n",
    );
    assert!(
        text.contains(
            "the GraphQL document would define `Query` twice — the contract's type `Query`, \
             and the generated query root"
        ),
        "the collision is not reported:\n{text}"
    );

    // 3. A record `Foo` beside a type `FooInput`: the twin's name, taken.
    let text = gql_refusal(
        "export type Foo = { a: Int64 }\n\
         export type FooInput = { b: Int64 }\n\
         export fn getFoo() -> Foo { return Foo { a: 1 } }\n",
    );
    assert!(
        text.contains("would define `FooInput` twice"),
        "the input-twin collision is not reported:\n{text}"
    );

    // 4. `JSON` — the scalar the document always defines.
    let text = gql_refusal(
        "export type JSON = { a: Int64 }\n\
         export fn getJson() -> JSON { return JSON { a: 1 } }\n",
    );
    assert!(
        text.contains("would define `JSON` twice"),
        "the built-in scalar collision is not reported:\n{text}"
    );

    // 5. A name reserved by the target grammar: `__` is the introspection prefix.
    let text = gql_refusal(
        "export type __Secret = { a: Int64 }\n\
         export fn getSecret() -> __Secret { return __Secret { a: 1 } }\n",
    );
    assert!(
        text.contains("reserved for introspection"),
        "the reserved prefix is not reported:\n{text}"
    );
    assert_eq!(
        text.matches("reserved for introspection").count(),
        1,
        "one mistake, one diagnostic:\n{text}"
    );

    // 6. Two names differing only by case are DISTINCT in GraphQL, and must not
    //    be refused — the check is a collision check, not a similarity check.
    let dir = gql_fixture(
        "export type Item = { a: Int64 }\n\
         export type ITEM = { b: Int64 }\n\
         export fn getItem() -> Item { return Item { a: 1 } }\n",
    );
    let sdl = run(&dir.join("gql.vyrn"));
    sdl_definitions_are_valid(&sdl);
    for name in [
        "type Item {",
        "input ItemInput {",
        "type ITEM {",
        "input ITEMInput {",
    ] {
        assert!(sdl.contains(name), "{name} missing:\n{sdl}");
    }
}

/// The document with every `"""…"""` description removed, so the checks below
/// read definitions and never a description's contents. `\"""` is the sole
/// block-string escape, as in the lexer — which is also why the scan can run off
/// the end: a body whose last byte is a backslash turns the CLOSING delimiter into
/// that escape, and the block string never terminates. That is a document no
/// GraphQL parser accepts, so the scan reports it rather than returning a
/// silently-truncated document for the definition checks to pass over.
fn sdl_without_descriptions(sdl: &str) -> String {
    let b = sdl.as_bytes();
    let n = b.len();
    let is_tq = |j: usize| j + 2 < n && b[j] == b'"' && b[j + 1] == b'"' && b[j + 2] == b'"';
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if is_tq(i) {
            let opened = i;
            i += 3;
            while i < n && !is_tq(i) {
                i += if b[i] == b'\\' && i + 3 < n && b[i + 1] == b'"' {
                    4
                } else {
                    1
                };
            }
            assert!(
                i < n,
                "the block-string description at byte {opened} never closes — its \
                 last bytes read as the `\\\"\"\"` escape, so a parser swallows every \
                 definition after it:\n{sdl}"
            );
            i += 3;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// Two rules a GraphQL parser enforces that balanced braces cannot see, checked
/// without a new dependency: (1) no definition has an EMPTY body — the spec
/// requires an object or input type to define at least one field, and `{}` is
/// where a parser reports "Expected Name, found }"; (2) no NAME is defined
/// twice — a document may define a name once.
///
/// What it does not verify: that every type REFERENCE resolves, argument and
/// directive syntax, or the rest of the grammar. It is the smallest check that
/// catches the two defects this generator had, and a `graphql-js` parse would
/// subsume it.
fn sdl_definitions_are_valid(sdl: &str) {
    let body = sdl_without_descriptions(sdl).replace("\r\n", "\n");
    assert!(
        !body.contains("{\n}"),
        "a definition with no fields (a parser reports `Expected Name, found }}`):\n{sdl}"
    );
    let mut seen: Vec<&str> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let mut tokens = t.split_whitespace();
        let kind = tokens.next().unwrap_or("");
        if !["type", "input", "enum", "scalar", "interface", "union"].contains(&kind) {
            continue;
        }
        let name = tokens.next().unwrap_or("").trim_end_matches('{');
        assert!(
            !seen.contains(&name),
            "`{name}` is defined twice in the document:\n{sdl}"
        );
        seen.push(name);
    }
    assert!(seen.len() >= 2, "no definitions found in:\n{sdl}");
}
