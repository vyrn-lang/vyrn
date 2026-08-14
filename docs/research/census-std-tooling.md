# Census: mechanisms kept after their reason left — std and tooling

A census of Vyrn's standard library and tooling at `3ac6d54`. The test applied
is not "is this wrong". It is: **the mechanism is correct, and it should not
exist** — something was built to cope with a condition the author had the
authority to remove.

Scope: `std/` (35 modules), `site/`, `compiler/vyrn-lsp/`,
`compiler/vyrn-genwasm/`, `compiler/vyrn-play/`, `web/`,
`.github/workflows/`, `examples/`, and the RFC corpus. (`vyrn-frontend`,
`vyrn-codegen` and `vyrn-cli` belong to a sibling census.)

Every finding cites `file:line` and states three things: what the mechanism
is, what base change would delete it, what that change costs. Findings are
ranked **CONFIRMED** (a program ran, output recorded) above **PLAUSIBLE**
(argued from reading). Where a decision is recorded with its argument, the
entry says "design critique", not "defect".

Known findings this census does not re-report: the string-concat emitter
family and its escaping holes (#168, #169, #171), the `std/diag` two-severity
closed set matched back as literals in `origin.rs`, the `gqlTrim`/
`gqlIndexOf`/`gqlSlice` helper duplication, the `.vyx` wrong-alphabet scanner
(#168), the frontend's prose-grepping tests, and RFC-0077's superseded memory
map (bannered).

---

## Top 10 by severity

| # | Severity | Finding | Ref |
|---|---|---|---|
| 1 | **High** | **Contract errors still ride bare identifiers** (`RPC_CONTRACT_ERROR__…` read out of a parser complaint) in six modules, one RFC after RFC-0099 shipped real generator diagnostics — and the mechanism's own comments state the dead premise ("a generator has no message-carrying trap"). | 1.1 |
| 2 | **High** | **The router's URL boundary converts alphabets in neither direction**: `hrefTags("a b")` emits a raw-space URL, the served route hands user code `a%20b`, and the no-op decoder's justifying comment cites a wall (`Int64→UInt8` narrowing) that RFC-0078 M4c removed — `urlDecode`/`urlEncode` are builtins. | 1.2 |
| 3 | **High** | **The derived RPC client dedupes re-emitted types by name alone**: two api modules declaring `CreateReq` differently compile to a client whose `usersCreate` is silently typed by posts' record — every call 422s, and the fix (a name registry that errors, `gqlDefine`) already exists in `std/graphql` since #169. | 1.4 |
| 4 | **High** | **Nine of twelve `vyrn-codegen` integration tests skip silently in every CI job** (early `return`, not `require_tools`; no job supplies wasmtime where they run) — including the layout-vs-clang ground truth that `ci.yml` cites as evidence in an argument about ARM coverage. | 3.1 |
| 5 | **High** | **48 std test blocks run in no gate** (`std/i18n` 16, `std/args` 8, `std/jsondec` 7, …): std coverage is an opt-in list of hand-written wrappers, not a sweep, and `vyrn test` exits 0 on "no tests". | 3.4 |
| 6 | **High** | **`web/wasi-min.js` parses the wasm binary and guesses ABI shapes** (`i32`+`i64` "is" a String, a documented-caveat collision) to recover signatures the compiler already half-writes into its `vyrn:exports` custom section — for results only. | 2.2 |
| 7 | **High** | **The LSP re-implements the CLI's project-context reader** ("the CLI is a binary crate, not linkable") and the copy has drifted on path canonicalization — the exact divergence its own comment claims to avoid. | 2.3 |
| 8 | **High** | **RFC-0046 documents a `slice` builtin, trap semantics, and always-available string predicates that no longer exist**, with no banner; plus the RFC-0011/0028/0013 "safe leak" triangle contradicting RFC-0089 rule 4 as shipped. | 4.1, 4.2 |
| 9 | Medium | **Both CI wasm-toolchain fetches provision a dead code path**: the only readers of `WASI_SYSROOT`/`WASI_BUILTINS` are `shim_wasm()` (no production caller) and a test that never runs; the gen-engine job needs nothing from the cache it shares. | 3.2 |
| 10 | Medium | **Five modules parse record fields back out of `TypeInfo.source`** with four independent parsers (lex-walk ×2, byte splitter, `gqlSplitDecl`) because reflection hands types over as text; `site/` re-scans source twice more because the reflected `Token` has no extent. | 1.5, 2.1 |

---

## Section 1 — std: mechanisms whose base decision has already changed

### 1.1 CONFIRMED — High. Contract errors still ride bare identifiers, one RFC after the language shipped real generator diagnostics

The mechanism. A generator that finds a contract violation cannot trap with a
message, so it emits the message AS AN IDENTIFIER and lets the parser's
complaint carry it. `std/rpc.vyrn:119`:

```vyrn
fn contractError(detail: String) -> String {
    return "RPC_CONTRACT_ERROR__" + detail + "\n"
}
```

fed by `rpcIdentOf` (`std/rpc.vyrn:222`), a whole alphabet-folding transform
that exists to make an error message survive being an identifier. The comment
at `std/rpc.vyrn:126-128` states the premise: "a generator has no
message-carrying trap and an objection rides a bare identifier". The same
premise is restated at `std/rpc.vyrn:218-221`.

The premise is dead. RFC-0099 shipped `std/diag` (`std/diag.vyrn:41-57`):
`report(Error, file, line, col, message)` "fails the load, at the anchor,
with the generator's own wording" (`std/diag.vyrn:21-22`). Four std modules
already use it (`std/graphql.vyrn:88`, `std/ui.vyrn:68`, `std/hints.vyrn:66`,
`std/vyx-hints.vyrn:51`).

What the kept mechanism costs the user, reproduced (2-parameter procedure,
`rpcInProcess`):

```
generated by rpcInProcess("./contract") at main.vyrn:1:1: expected `fn`,
`type`, `protocol`, `contract`, `impl`, `let`, or `logging` at top level,
found Ident("RPC_CONTRACT_ERROR__add__takes_2_parameters__a_procedure_takes_at_most_one")
```

The user decodes their error out of a parser's objection to a synthetic
identifier, underscores for spaces, at a position (`main.vyrn:1:1`) that names
the import line rather than the offending procedure.

The identifier-as-message mechanism survives in six modules:

| module | site |
|---|---|
| `std/rpc` | `std/rpc.vyrn:119` (`RPC_CONTRACT_ERROR__`) |
| `std/connect` | `std/connect.vyrn:134` (`CONNECT_CONTRACT_ERROR__`) |
| `std/i18n` | `std/i18n.vyrn:954,961` (`I18N_PARSE_ERROR__`, `I18N_ERROR__`) |
| `std/tw` | `std/tw.vyrn:853,861` (`TW_PARSE_ERROR__`, `TW_ERROR__`) |
| `std/ui` | `std/ui.vyrn:2402,2423,2700,2719` (`PAGES_ERROR__`) |
| `std/vyx` | `std/vyx.vyrn:2670` (`VYX_ERROR__`) |

`std/ui` and `std/vyx` are the strangest case: each already imports
`reportHere` from `std/diag` and uses it elsewhere, and keeps the identifier
trick beside it.

The base change: replace each `*_ERROR__` return with
`reportHere(Error, …)` — or `report(Error, file, line, col, …)` where the
generator knows the anchor (i18n and tw both computed one; `identKey` and
`twIdentOf` are the same alphabet-folding transform again,
`std/i18n.vyrn:954`, `std/tw.vyrn:853`). Then delete `contractError`,
`rpcIdentOf`, `identKey`, `twIdentOf` and their ui/vyx twins.

The cost: message wording changes (tests that pin the identifier spellings
must re-pin); nothing else. The mechanism's own documentation is the strongest
evidence — it records a premise RFC-0099 removed, and a design record that
contradicts the code is worse than none.

### 1.2 CONFIRMED — High. The router's URL boundary converts alphabets in neither direction, while both converters ship as builtins

`std/ui`'s generated router binds a dynamic `String` segment through
`uiRouteDecode` (`std/ui.vyrn:1740`), which is the identity
(`std/ui.vyrn:1633-1635`). The comment at `std/ui.vyrn:1570-1572` gives the
reason: "full percent-decoding (`%XX` → byte) needs an Int64→UInt8 narrowing
builtin Vyrn lacks … so it is deferred."

That reason is stale. RFC-0078 M4c made `urlDecode`/`urlEncode` builtins
routed through `std/codecs` "on every engine" (`std/codecs.vyrn:18-25`).
The narrowing wall was climbed; the stub stayed.

On the write side, the typed-URL helper splices the raw parameter into the
path (`uiDynHelperBody`, `std/ui.vyrn:804-814`) with no `urlEncode`, and the
generated `RoutePath` refinement admits any non-slash byte
(`uiSegRegex`, `std/ui.vyrn:917-922`: `([^/]+)`).

Reproduced (a `pages/tags/[name].vyrn` app, interp):

```
hrefTags("a b")            -> /tags/a b        (raw space; RoutePath accepts)
GET /tags/a%20b            -> 200, body renders  tag=[a%20b]
GET /tags/a b              -> 200, body renders  tag=[a b]
urlDecode("a%20b")         -> Some("a b")      (the builtin exists)
hrefTags("a/b")            -> trap: error: validation failed for `RoutePath`
```

Every line is a hole in the same boundary:

- The helper writes `/tags/a b`, a request-target no HTTP client sends
  verbatim; a browser encodes it to `/tags/a%20b`.
- The server then hands user code `name = "a%20b"` — the wire alphabet leaked
  into the value, so `href → route → Params` never round-trips for any name
  that needs encoding. This is the #168 wrong-alphabet finding again, at the
  URL boundary.
- A `/` in the parameter is the one case the boundary notices, and its answer
  is a runtime abort of the whole program, not a diagnostic or a Validation.

The base change: `urlEncode` each dynamic part in the emitted href helper,
`urlDecode` each `String` segment in the emitted try-fn (its `None` row — a
`%00` — becomes the 404 the router already has). Both functions exist on
every engine; the stub, its stale comment, and the raw-space RoutePath
acceptance all delete.

The cost: routes over already-encoded names change meaning (a stored
`"a%20b"` now arrives as `"a b"`), which is the fix, and byte-stability of
existing rendered pages that embedded unencoded hrefs.

### 1.3 CONFIRMED — Medium. `std/openapi` emits a `$ref` no resolver can follow, six lines from the guard that exists for the response side

`oaPathValue` (`std/openapi.vyrn:194-204`) splices a procedure's parameter
type into a reference with no membership check:

```vyrn
body = body + "\"requestBody\":{…\"schema\":{\"$ref\":\"#/components/schemas/" + f.params[0].spelling + "\"}…"
```

while the response side, in the same file, guards: `oaResponseSchema`
(`std/openapi.vyrn:181-189`) checks `oaIsNamedType(f.ret, iface)` and falls
back to `{}`.

Reproduced with a contract whose procedure takes a bare scalar
(`export fn double(n: Int64) -> Int64`):

```json
"requestBody": { … "schema": { "$ref": "#/components/schemas/Int64" } … }
…
"components": { "schemas": {} }
```

`#/components/schemas/Int64` resolves to nothing in the emitted document —
a reference a resolver refuses, i.e. the #171 class ("a document a parser
refuses is not output") in the one emitter that already migrated to the
`std/json` tree.

The deeper version of the finding: `std/rpc.vyrn:191` (`validateContract`)
and `std/graphql.vyrn:83` both gate generation on the shared contract rules;
`std/openapi` imports no gate at all (`std/openapi.vyrn:45-47`) and so
documents whatever it is handed. A scalar parameter is legal under
`validateContract` (it serializes), so the honest base change is inside
`oaPathValue`: treat the request side exactly as the response side —
`$ref` for a named type, an inline schema (or `{}`) otherwise. Cost: a
conditional; the document for every well-formed contract is byte-identical.

### 1.4 CONFIRMED — High. The derived client dedupes re-emitted types by NAME alone, and a collision silently retypes another module's procedure

`rpcClientTypes` (`std/rpc.vyrn:1238-1253`) re-emits every reachable type
declaration into the client module — a recorded decision with its argument
("re-emitted rather than imported: importing would put the api modules — and
therefore their bodies — into the client build", `std/rpc.vyrn:1230-1232`).
That part is design, not defect.

The dedupe is the defect: `std/rpc.vyrn:1245` keeps the FIRST declaration per
name (`rpcListContains(seen, t.name)`), comparing nothing else. Two api
modules declaring the same type name with different shapes are legal — each
module's own contract validates in isolation — and the collision is resolved
by silently dropping one shape.

Reproduced. `api/users.vyrn` declares `CreateReq = { name: String }`;
`api/posts.vyrn` declares `CreateReq = { title: String, body: String }`.
`vyrn emit-gen` on `import { … } from client("./api")`:

```vyrn
export type CreateReq = { title: String, body: String }   // posts' shape won
…
export fn usersCreate(req: CreateReq, cb: fn(RpcReply<UserRes>)) {
    let id = vyrnRpcCall("usersCreate", "/_/users/create", toJson(req))
```

`usersCreate` is now typed by POSTS' record. The client compiles clean, the
symbol map records `CreateReq`'s origin as `api/posts.vyrn` only, and every
call to `usersCreate` serializes `{title, body}` at a server validating
`{name}` — a 422 on every request, discovered at runtime, from a generator
that had both declarations in hand.

The base change already exists in a sibling: PR #169 gave `std/graphql` a
name registry (`gqlDefine`) where "a repeat is an RFC-0099 `Error` naming both
routes rather than a silent rename". The same registry in `rpcClientTypes` —
same name, different `t.source`, `report(Error, …)` naming both files —
deletes the silent pick. Cost: contracts that today collide benignly
(identical source text in two modules) can stay deduped by comparing the
source, so nothing working breaks.

### 1.5 CONFIRMED — Medium. Five modules parse a record's fields back out of `TypeInfo.source`, each with its own parser

`moduleInterface` reflection hands a type over as its SOURCE TEXT, so every
generator that needs field structure re-derives it by parsing Vyrn:

| module | parser | site |
|---|---|---|
| `std/http` | `lex()` walk at brace depth 1 | `std/http.vyrn:1415` |
| `std/cli` | the same walk, restated | `std/cli.vyrn:246` (its own doc: "the `std/http:httpFields` walk", `std/cli.vyrn:11`) |
| `std/ui` | a hand-rolled byte splitter (`uiRecordBody`/`uiSplitFields`/`uiParseFields`) | `std/ui.vyrn:825-908` |
| `std/graphql` | `gqlSplitDecl` | `std/graphql.vyrn:259,659,688,834,1932` |
| `std/connect`, `std/rpc` | no parsing, but verbatim `t.source` splicing | `std/connect.vyrn:343`, `std/rpc.vyrn:534,1247` |

Four independent parsers for one grammar the compiler parsed once already —
it BUILT the interface from the AST, rendered the declaration back to text,
and each consumer un-renders it. `std/ui`'s copy is brace-aware but not
comment-aware or string-aware; `uiSplitFields` splits on top-level commas by
byte scanning (`std/ui.vyrn:847-868`), so a field type spelling containing a
comma inside a string-refinement (`String where value =~ "a,b"`) would
mis-split — the same class as #168's wrong-alphabet scan, latent because
`Params` records are simple today.

The base change: `TypeInfo` carries structured fields (name, type spelling,
per-field doc) the way `FnInfo` already carries `params`. RFC-0098 M3 records
the per-field-docs half of this as a planned compiler change
(`std/cli.vyrn:31-34`); the field-structure half is what deletes the four
parsers. Cost: a reflection-surface addition in the frontend and a gen-cache
format bump; the four parsers and their tests (`std/ui.vyrn:2761-2812` pin
the splitter's brace behaviour) all delete.

### 1.6 PLAUSIBLE — Low. Three hand-rolled insertion sorts, because `std/arrays` has no sort

`oaSorted` (`std/openapi.vyrn:106-124`), `sortedCopy`
(`std/bench.vyrn:39-58`), and the ICU selector sort inside
`joinSortedSelectors` (`std/i18n.vyrn:505-510`, "insertion sort (selector
lists are tiny)") are the same algorithm three times. `std/arrays` (RFC-0023)
ships generic `map`/`filter`/`fold`/`includes` and stops short of `sort`.
String `<` ordering exists (RFC-0022), so `sort(Array<String>)` — or
`sortBy` under a protocol bound — is ordinary Vyrn today. Cost: one function
and three deletions; the only reason it does not exist is that nobody moved
the third copy into the library.

### 1.7 PLAUSIBLE — Medium. The generated router carries its own runtime as quoted text in every output, instead of importing it

`uiFixedRuntime` (`std/ui.vyrn:1565-1637`), `uiHeadRuntime`
(`std/ui.vyrn:1641-1659`) and `uiErrorRuntime` (`std/ui.vyrn:1664-1681`) are
fixed, parameterless blocks of Vyrn — a segment splitter, an Option probe,
the 404/error pages, head-merging — spliced verbatim into every generated
router. The emitted functions live in the router's flat namespace, which is
why they wear the `uiRoute…` prefix ("prefixed so it never clashes with page
or app names", `std/ui.vyrn:1562-1564`) — a collision-avoidance convention
that exists only because the code is spliced rather than imported.

The same file already does it the other way: the generated module IMPORTS its
runtime helpers from `std/ui` when they are `export fn`s
(`std/ui.vyrn:2241-2244` emits `import { uiWantsData, uiPayload, … } from
"std/ui"`), and `std/graphql` states the division as a principle — runtime
code "lives in this file so the generated module is nothing but an import
block and a resolver table — the same division `std/ui` draws"
(`std/graphql.vyrn:883-889`).

The one genuine blocker is that `uiRouteNotFound`/`uiRouteError` reference
`Request`/`Response`/`Issue`/`document`/`el`/`text`, which resolve in the
generated module's context — but `std/ui` itself imports `std/html`
(`std/ui.vyrn:59`) and could own these outright. The base change: promote the
three quoted blocks to `export fn`s of `std/ui` and emit three import lines.
What deletes: the quotes, the prefix convention's reason, and ~150 lines of
every generated router. Cost: the emitted module's byte shape changes (golden
tests re-pin); `RoutePath`-adjacent code that assumed no `std/ui` runtime
import gains one.

---

## Section 2 — tooling: the LSP, web/, site/, the play crate, genwasm

### 2.1 CONFIRMED — High. The reflected `Token` drops the extent the lexer had, so two site modules re-scan the source to recover it

`compiler/vyrn-frontend/src/interp.rs:623-659` builds the RFC-0054 `Token`
as `{kind, text, line, col}` — decoded text, no end position, no trivia. So
`site/app/hl.vyrn:201-287` carries `lineStarts`/`scanString`/`scanToEol`/
`scanNumber`/`extentOf` plus a second `//`-trivia scanner
(`site/app/hl.vyrn:316-333`) — roughly 110 lines re-deriving where each
token ENDS, with an admitted ceiling (an interpolation ends the string scan
early, `hl.vyrn:219-221`). `site/app/apidoc.vyrn:39-104` does it again at
declaration granularity (`offsetOf`/`sigEnd`/`trimTail`; `sigEnd`'s own doc
records that getting the rule order wrong "shipped ten unbalanced
signatures").

The counter-example is in-tree: `compiler/vyrn-play/src/lib.rs:162-201`
colours the same language in ~25 lines with no scanner, because it sits on
`lexer::lex_with_trivia`, which already yields verbatim text and trivia.

Base change: widen the reflected record (`interp.rs:626-634,646-656`) with
`endLine`/`endCol` (or the raw span) and route `lex()` through
`lex_with_trivia`. Cost: two field lists, a prelude record, a gen-cache
format bump; both site scanners shrink to lookups.

### 2.2 CONFIRMED — High. `web/wasi-min.js` parses the wasm binary to re-learn signatures the compiler knows and already half-writes down

`web/wasi-min.js:57-150` is a hand-rolled reader of the type/import/
function/export sections ("the JS WebAssembly API exposes names but not
types"), feeding a String-detection heuristic at `wasi-min.js:328-340`
(an `i32` followed by an `i64` IS a String) whose collision case is a
documented caveat (`web/README.md:68-70`), plus an argument encoder deciding
slots by JS runtime type (`wasi-min.js:421-448`).

Meanwhile `compiler/vyrn-codegen/src/direct.rs:642-677` already emits a
`vyrn:exports` custom section — but only `name → "string"|"bool"` for
RESULTS. `web/README.md:122-125` states the principle this census would
apply: "The compiler is the thing that knows, and it writes the section
now." It just stops at results.

Base change: the custom section carries the full declared signature of every
`export extern fn` and `vyrn.*` import. `readModule` collapses to a ~20-line
custom-section read; the ABI heuristic and its caveat delete. Cost: a
versioned signature table in `direct.rs` (sibling census's crate — flagged,
not claimed) and a shim rewrite.

### 2.3 CONFIRMED — High. The LSP re-implements the CLI's project-context reader because `vyrn-cli` is a binary crate — and the copy has drifted on the exact rule it cites

`compiler/vyrn-lsp/src/main.rs:162-177` (`std_root`, "Mirrors `vyrn`'s
discovery"), `:181-188` (`real_path`), `:198-235` (`find_manifest`, "A
compact duplicate of `vyrn`'s reader (the CLI is a binary crate, not
linkable)"), `:286-304` (an inline `vyrn.lock` TSV parser), `:350-358`
(`gen_cache_dir`, "kept byte-identical to the CLI's") duplicate
`compiler/vyrn-cli/src/main.rs:536-551,603-610,620-676` and
`vyrn-cli/src/remote.rs:195-225`.

The drift: the CLI canonicalizes the audience base
(`vyrn-cli/src/main.rs:652-657`); the LSP passes the raw walked-up directory
(`vyrn-lsp/src/main.rs:218-223`) — precisely the "second spelling of a path
(case, a junction)" divergence the LSP's own comment at `:219-221` says it
avoids. The parenthetical IS the base decision: nothing prevents a `[lib]`
target on `vyrn-cli` (or a `project` module in the frontend) owning manifest
discovery, canonicalization, lockfile reading and cache paths. Cost: one
`[lib]` stanza and ~150 moved lines; both binaries become consumers.

### 2.4 CONFIRMED — Medium. One LSP file answers "does this root call a generator?" twice — once structurally, once by substring

`vyrn-lsp/src/main.rs:858-875` (`is_dev_entry`) lexes and parses imports and
matches `ImportSource::Generator`. `main.rs:3154-3200` answers the same
question with `src.contains("rpc(")` against two string tables
(`RPC_GENERATORS`, `MAP_GENERATORS`) that also match a comment, a local
`fn http(`, or `rpcClient(` under the `client(` entry — each false positive
costing a full `analyze_doc` of an unrelated root. Base change: give
`mounting_roots` the parse `is_dev_entry` already performs; the tables
become name lists. Cost: ~10 lines.

### 2.5 CONFIRMED — Medium. Hover text is rendered, then parsed back to recover the value that produced it

`vyrn-lsp/src/main.rs:3762-3769` recovers a class name via
`hover.ends_with("— safelisted (app-styled)")` + `strip_prefix("**\`")`,
re-reading the string `vyrn-frontend/src/symbols.rs:1374` formatted from a
token it held structurally; `main.rs:887-914` (`fence_signature`) re-decides
"is this a signature" by testing rendered prose against nine keyword
prefixes. This is the diagnostics-through-prose finding (#177's class) alive
in hover. Base change: `class_token_hover`/`resolve` return a small struct
beside the prose; the adapter renders. Cost: one struct, two call sites,
re-pinned hover tests.

### 2.6 CONFIRMED — Medium. The RPC dispatcher name is derived in three languages, in a file whose own comment forbids exactly that

`web/vyrn-rpc.js:62-66` explains the derived PATH is passed as an argument
because "inverting the path template in the host would be a second
implementation of the derivation rule" — then `web/vyrn-rpc.js:34-36`
implements `"vyrnRpcDone" + capFirst(proc)` anyway, byte-identically
duplicated at `web/vyrn-query.js:35-38`, and predicted a third time in Rust
(`vyrn-lsp/src/rename.rs:169-202`, where a failed prediction refuses the
whole rename). Base change: pass the dispatcher name as one more argument on
the `vyrnRpcCall` extern, exactly as the path already is, and record the
generated prefix in `MappedSymbol.derived`
(`vyrn-frontend/src/symbolmap.rs:47-58` — an open string-keyed slot). Cost:
one extern argument in `std/rpc`, one map key; both JS copies and the Rust
prediction delete.

### 2.7 CONFIRMED — Medium. `site/export.vyrn` maintains an asset copy-table, an SVG-only favicon, and two CI copy steps because `writeFileBytes` does not exist

`site/export.vyrn:192-206` is an 11-row source→name table whose doc states
the reason ("`writeFile` takes a `String`, and a wasm module is not text"),
`export.vyrn:140-143` records the favicon constraint, and
`.github/workflows/site.yml:90-98,107-115` repeat it as two `cp` steps.
`readFileBytes` exists (`vyrn-frontend/src/checker.rs:6157`); its write twin
never shipped. Base change: `writeFileBytes(path, Array<UInt8>)`; `assets()`
becomes a `listDir` walk and both workflow steps delete. Cost: one builtin
across the engines (RFC-0014's pattern).

### 2.8 CONFIRMED — Medium. Two web tests build a fake browser because the served JS lives in two directories

`web/test/explore-search.test.mjs:18-45` and
`web/test/inline-copy.test.mjs:18-45` share a byte-identical block that
regex-strips `import` lines out of `site/public/widgets.js`, stubs
`document`/`matchMedia`, and runs the result in a `node:vm` realm (forcing a
cross-realm workaround at `explore-search.test.mjs:52-54`) — while
`web/test/dom-svg.test.mjs:20-27` tests `web/vyrn-dom.js` with a plain
`await import`. The difference: `web/*.js` imports relatively;
`site/public/*.js` imports by served-root path because the export table
flattens two directories into one root. Base change: one on-disk home for
browser JS so specifiers are relative both places, plus a `document` guard
on `widgets.js`'s boot. Cost: a file move; four tests lose the vm harness.

### 2.9 Assorted smaller mechanisms (all traced)

- **genwasm's `UNSERVED` scan** (`vyrn-genwasm/src/lib.rs:61,125-135`): a
  per-generation AST walk declining modules that CONTAIN a write builtin,
  which its own comment concedes the purity check already forbids calling
  (`checker.rs:10369-10375`). PLAUSIBLE deletion — needs one check that the
  lowering path fails cleanly for unreachable write calls.
- **Closed sets restated across the boundary**: the contextual-keyword list
  in `vyrn-play/src/lib.rs:121-123` = `site/app/hl.vyrn:159-161`; the HTML
  void-element set in `web/vyrn-dom.js:39-42` = `std/html.vyrn:317-325`
  (where the JS copy defends an invariant `el()` itself does not enforce,
  `std/html.vyrn:164-166`); `site/app/code.vyrn:36-82` is a third HTML
  escaper, disagreeing with `std/html`'s private pair on the apostrophe,
  written only because `escapeText`/`escapeAttr` are not exported.
- **`vyx_script` split four times**: `vyrn-lsp/src/contracts.rs:127-133`,
  `vyrn-lsp/src/rename.rs:274-280` (with its own test), plus copies in
  `vyrn-cli/src/main.rs:1596` and `vyrn-frontend/src/contracts.rs:699` —
  the LSP-internal pair is a one-import fix today.
- **`spells_type`** (`vyrn-lsp/src/main.rs:2278-2310`) re-derives "did the
  author write the type" by scanning the declaration line for `:`, because
  `LocalBinding` merged the checker's inferred type with the annotation and
  erased which it was (`symbols.rs:2301-2306`). One `annotated: bool` at the
  merge site deletes the scan.
- **`site/app/apidoc.vyrn:39-205`** rebuilds `{name, kind, signature, doc}`
  from raw `lex()` tokens although the frontend returns exactly that shape
  as `ModuleDoc` (`symbols.rs:2808+`) to `vyrn doc` — the comptime surface
  never got a `moduleDoc(path)` builtin. Lands fully only with 2.1.
- **EXPECTED_CHECK_FAILURE prose twins**: eight examples carry a comment
  saying they are listed in `vyrn-cli/tests/common/mod.rs:35-146`; the list
  itself is the sibling census's territory, but the corpus-side mechanism —
  an example's expectation living in another crate — would delete under a
  `//! check-fails:` header directive.

---

## Section 3 — the gates: .github/workflows, release, and what no gate proves

The workflow findings divide into gates that cannot fail, claims no gate
proves, and provisioning for dead code. All were traced to the exact shell
or Rust logic; the four most severe:

### 3.1 CONFIRMED — High. Nine of the twelve `vyrn-codegen` integration tests skip silently in every CI job

`vyrn-codegen/tests/layout_vs_clang.rs:168-186`, `wasm_runs.rs:95-290` (five
tests), `shim_link.rs:225-231`: each is a plain `#[test]` whose body is
`let Some(..) = <tool lookup> else { eprintln!("NOTE: …"); return; }` — an
early return, not a panic, and none routes through `common::require_tools`,
so `VYRN_REQUIRE_TOOLS=1` (`ci.yml:32`) does not bite. No job that runs them
supplies `wasmtime`/`WASI_SYSROOT` (`ci.yml:115-117` runs the workspace
suite with neither; the parity and gen-engine jobs filter to `vyrn-cli`
tests only, `ci.yml:213,267-268`). The layout engine's ground-truth-vs-clang
check has never executed in CI — and `ci.yml:151-156` cites that very test
as the reason an ARM parity leg would add nothing.

### 3.2 CONFIRMED — High. Both wasm-toolchain fetches download two tarballs for a dead code path

`ci.yml:196-203` and its byte-identical copy at `ci.yml:246-253` fetch
wasi-sysroot + builtins + wasmtime. The only readers of
`WASI_SYSROOT`/`WASI_BUILTINS` are `vyrn-codegen/src/toolchain.rs:839,843`
inside `shim_wasm()` — which has NO production caller (its one call site is
the never-running test of 3.1) — and the exports at `ci.yml:265-266` are
themselves built from constructs that cannot fail (`echo <glob>` and
`find | head -1` both exit 0 on no match). The gen-engine job needs nothing
from the cache at all (its wasmtime is an embedded crate, `ci.yml:242`;
RFC-0076 M7 moved the shim to the direct backend,
`vyrn-genwasm/src/lib.rs:1245`). Base change: delete `shim_wasm()` + its
test, then the gen-engine job's whole fetch/cache/export block deletes and
parity's collapses to one wasmtime curl.

### 3.3 CONFIRMED — High. Nothing in `site/` is gated before merge, and the site test loop omits the highlighter it names

`site.yml:19-29` triggers on release/workflow_run/dispatch/cron — no
`pull_request`; `ci.yml` never touches `site/`. A PR breaking any site
module merges green and turns the Pages deploy red afterwards. Inside the
job, `site.yml:64` hardcodes 13 files while `site/app/` holds 14 modules:
`hl.vyrn` (6 test blocks) and `facts.vyrn` (3) never run — and the step's
own comment claims "the other modules check the highlighter", which is
exactly what is not run. The fmt step two steps down already uses the glob.
Compounding: `vyrn test` on a file with zero tests prints "no tests" and
exits 0 (`vyrn-cli/src/main.rs:2933-2937`), so a module silently losing its
tests keeps the loop green.

### 3.4 CONFIRMED — High. 48 std test blocks run nowhere

There is no sweep over `std/` anywhere in the Rust suite (the only
`read_dir` sweeps cover `examples/`). Cross-referencing the 23 std modules
carrying `test "` against every `vyrn test <path>` invocation: `std/i18n`
(16 blocks), `std/args` (8), `std/jsondec` (7), `std/bench` (5), `std/diag`
(4), `std/math` (3), `std/openapi` (3), `std/connect` (2) never run in any
gate. Base change: one `read_dir("std")` test asserting
`success && !contains("no tests")` — the `parity.rs` pattern — which also
deletes the need for the 15 hand-written per-module wrappers.

### 3.5 The rest, compressed

- **Release requires nothing**: `release.yml:7-10` fires on any `v*` tag
  with no needs/status check — a tag at a red commit ships (CONFIRMED).
- **The wasm smoke never runs its module**: `release.yml:167-168` is
  `test -s hello.wasm`; the release notes promise a working wasm target.
  Node is on every runner; `WebAssembly.compile` is a one-line upgrade
  (CONFIRMED).
- **The install scripts are executed by nothing** — including the checksum
  refusal README:196 advertises; `install.sh:13` documents a `VYRN_REPO`
  hook "used by the test harness" and no harness exists (CONFIRMED).
- **The bench gate is doubly vacuous**: the placeholder baseline makes every
  bench `New` (self-documented), and a bench DELETED from the corpus is
  `MissingFromRun`, which no path fails on (`vyrn-cli/src/main.rs:3557-3586`)
  — plus the loop compares per-example against a whole-corpus baseline, so
  the missing-signal is pre-flooded (CONFIRMED).
- **Shell-contract asymmetry**: the test job sets `bash` defaults (pipefail
  on, `ci.yml:78-85`); the bench job doesn't, so its corpus-discovery grep
  (`ci.yml:303`) silently degrades to an empty loop where the test job's
  identical pattern would fail (CONFIRMED).
- **`paths-ignore: '**.md'`** makes the docs-drift gate one-directional: a
  commit editing only `docs/api/*.md` runs no workflow at all (CONFIRMED).
- **README vs workflows**: README:265 still says the test job runs "on
  Linux" (it is a four-platform matrix since #174); README:271 calls
  benchmarks "informational" while `ci.yml:278-285` argues the opposite;
  README's hand counts (32 std modules / 141 examples / 95 RFCs) are 37 /
  160 / 99 — the site generates and gates the same numbers, README states
  them (CONFIRMED).
- **"every one of them on all three backends"** (`site/app/repo.vyrn:13-14`,
  README:44) overstates by the 17 skip-listed examples
  (`EXPECTED_CHECK_FAILURE` 16 + `WASM_ONLY` 1) (CONFIRMED).

### 3.6 CONFIRMED — Medium. `docs/api/` is 38 committed generated files with no reader, kept alive by a four-legged drift gate and a `.gitattributes` clause

The chain: commit generated output → it can drift → add `--verify`
(`ci.yml:125-130`, run once per matrix leg) → the gate compares bytes →
Windows checkouts could change bytes → `.gitattributes:5-7`. Nothing
consumes the directory: the website explicitly refuses to
(`site/app/apidoc.vyrn:1-12` — rendering the Markdown back "loses on its own
terms", so it reads `std/*.vyrn` directly). Deleting the committed copy
deletes every link; what is worth keeping — proof the generator runs — is
`vyrn doc --std -o $(mktemp -d)` on one leg. (`cleanup-census.md:480`
reviewed the directory and answered a different question: "cannot drift" is
true and beside the point when nothing reads it.)

---

## Section 4 — the RFC corpus: records superseded in substance that still read as current

Most of the corpus is well-kept: RFC-0004, 0012, 0025, 0034, 0088, 0090,
0091 carry inline supersession notes, and RFC-0019 correctly marks the
withdrawn `rpc fn` keyword. The stale ones cluster in two places: pre-memory-
arc RFCs whose "safe leak" paragraphs RFC-0089/0092 silently invalidated, and
RFC-0046, whose central mechanism was deleted without a note. Every claim
below was verified against current code.

### 4.1 MAJOR — RFC-0046's title mechanism does not exist

`rfcs/RFC-0046-strings.md` is titled "+ a `slice` builtin" and argues at
length (`:20-38`) why `slice` must be a builtin (pure Vyrn "has no way" to
avoid revalidation) and that it TRAPS on bad input with recorded wording.
Current code: `slice` is ordinary Vyrn in `std/strpred.vyrn:188` returning
`Result<String, SliceError>` — no builtin (`checker.rs:281-293` lists it
under `MOVED_TO_STD`), no trap, and it pays exactly the walk the RFC says is
impossible. The as-landed note (`:138-141`) says `contains`/`startsWith`/
`endsWith` "stayed compiler builtins … available without importing" —
RFC-0094 M2 made all three imports. The changes are recorded in RFC-0078,
0079 and 0094; RFC-0046 itself has no banner and still reads "Implemented".
It is the file a reader opens for the string surface.

### 4.2 MAJOR — three "safe leak" records contradict the shipped memory model

- `RFC-0011-array-mutation.md:52-58,110-112`: "an overwritten heap element
  is not freed by the store (a safe leak)". Rule 4 as implemented releases
  it: `vyrn-codegen/src/direct.rs:3426-3454`, `movecheck.rs:3686-3691`,
  pinned by `vyrn-cli/tests/memory.rs:492`.
- `RFC-0028-map.md:147-152`: elements are "a safe leak … `keys()` copies the
  key pointers". Both false: `direct.rs:2378-2389` (a map releases its
  keys), `direct.rs:11411-11419` (`keys()` copies the strings BECAUSE a
  pointer snapshot would double-free), `direct.rs:3400-3414` (`m[k] = v`
  releases the displaced value).
- `RFC-0013-module-state-event-loop.md:86-90`: "overwriting a heap-valued
  `mut` global leaks the old value". `direct.rs:2988-3014,2624-2630`;
  `memory.rs:480-487` pins the accumulator steady.

Each cites the others ("consistent with array element stores") — a web of
mutually-supporting stale claims. One banner paragraph per file, pointing at
RFC-0089 rule 4, deletes the contradiction.

### 4.3 MODERATE

- **RFC-0079** `:337-346` records "a discarded error payload is a safe leak
  … when arm binders become droppable, both spellings gain it together" as a
  live limitation. PR #166 landed it by another route
  (`movecheck.rs:2201-2223`; measured 141.7 MB → 3.6 MB in that commit).
- **RFC-0099**'s status still reads "M1 landed" and its §M2 ("not in this
  RFC") shipped as RFC-0100 with no cross-note; its anchor-containment table
  (`:139`) says "v1 does not check" what `origin.rs:455-486` now refuses
  three ways (#176/#179).
- **RFC-0021** `:194-202` describes the gen cache as `v2`-tagged and
  unauthenticated; it is `v3`, HMAC-authenticated under a per-user key
  (`loader.rs:1776,1901-1917`, #175), and `:205-209`'s "generators run only
  in the interpreter" was superseded by RFC-0076 (compiled to wasm).

### 4.4 MINOR

RFC-0007 (the `list([..])` desugar it documents was removed,
`checker.rs:5604-5607`; the shipped `Template` record appears in no RFC but
an aside in RFC-0054); RFC-0059 (reader now lives in `std/jsonread`, not
`std/json`; the "`contains` is reserved" justification no longer holds;
`emit` now validates `JNum` per #169); RFC-0008 (§"Retiring `print`" reads
normative — "`print(x)` is removed" — while its own status banner says the
opposite and `print` is everywhere); RFC-0012 (`encodeInto` rejected
"because the path never frees" — it frees now, by the same RFC's as-landed
section); RFC-0037 (justifies the lambda-capture leak by analogy to a boxed-
enum leak that RFC-0092/0096 fixed).

And the index: `rfcs/README.md:45` says "97 RFCs, numbered 0001 to 0098" —
there are 99, through 0100, and RFC-0099/RFC-0100 are absent from the index
table entirely.

---

## Section 5 — the three specific assessments

### 5.1 How much of std/ui (116 KB) and std/vyx (205 KB) is a consequence of building code as text?

Measured shape (`wc -l`, test blocks located by `grep -n '^test '`):

| | total lines | code (before tests) | tests |
|---|---|---|---|
| `std/ui.vyrn` | 2,822 | ~2,735 | ~87 |
| `std/vyx.vyrn` | 4,815 | ~4,390 | ~420 |

Attribution, by reading every function (the fn inventory is in section 1's
findings):

**vyx** — the text-output tax is concentrated and identifiable:
- position relocation: `vyxRegion`/`vyxShiftAttrs`/`vyxShiftNodes`/
  `vyxShiftNode`/`vyxShiftIf`/`vyxCountNewlines`/`vyxNormHelper`/
  `vyxRelocateComp` (`std/vyx.vyrn:1739-1935`, ~195 lines) exist ONLY
  because the output is text: origins must be re-derived by counting
  newlines in emitted strings so `//@origin` lines up.
- textual import merging: `vyxParseImport`/`vyxMergeImports`/
  `vyxImportLine` (`std/vyx.vyrn:2271-2460`, ~190 lines) parse Vyrn import
  LINES with `vyxFind(" from ")` and re-emit them — structure → text →
  parsed structure → text.
- escaping into Vyrn literals: `vyxEscSecond`/`vyxStrLit`
  (`std/vyx.vyrn:186-227`, ~40 lines).
- emission plumbing: the `vyxEmit*` family + module assembly
  (`std/vyx.vyrn:1936-2270,2461-2700`, ~570 lines), roughly half of which
  is string bookkeeping (acc threading, paren/comma joining, newline
  placement) rather than the semantic template→hyperscript mapping.
- interface dummies: `vyxNoSchema`/`vyxNoOrigin`/`vyxEmittedInterface`
  (`std/vyx.vyrn:2621-2667`, ~45 lines) — a synthesized module must fake
  reflection metadata.

Total: **~650-750 lines, 15-17% of the module's code**. The remaining ~83%
is honest input work — the markup-alphabet scanner and parser (~1,450
lines, the #168 lesson made structural), section/props handling, and
`moduleInterface` interrogation — which a tree-emitting rewrite would keep
unchanged.

**ui** — same method: the three quoted runtime blocks (~150 lines,
finding 1.7), the route/typed-URL emission plumbing's string half (share of
~700 lines), the flat-namespace collision machinery (`uiCollisionKey`/
`uiHelperCollisions`, ~55 lines), and the stringly-reflection field parsers
(~100 lines, finding 1.5) put the tax at **~400-450 lines, ~15%**.

So the honest answer is: **the text decision costs each module about a
sixth of its bulk — but that sixth is where every shipped defect in this
family lived** (#168's alphabet, #169's invalid documents, this census's
1.2/1.4), and it costs a second thing the line count hides: **the test
suites are pinned to the text**. 48 vyx tests assert with
`out.contains("Raw(html)")`-style substring probes over emitted source
(63 `contains(` in the file) — the std-side twin of the frontend's
prose-grepping tests, and the reason any representation change now carries
a re-pinning bill. The tree alternative is not hypothetical in this repo:
`std/html` (the tree emitter) is the one that never shipped invalid output,
and the fixed-runtime quote blocks (RFC-0054) already moved the most
error-prone emissions from concatenation to compiler-checked quotes.

### 5.2 RFCs superseded in substance but reading as current

Section 4. The two worth a banner TODAY: **RFC-0046** (its title mechanism
— the `slice` builtin, its trap semantics, and the builtin status of
`contains`/`startsWith`/`endsWith` — is gone) and the **RFC-0011/0028/0013
"safe leak" triangle** (three mutually-citing records of a stance RFC-0089
rule 4 reversed, each now describing frees that DO happen as leaks). Plus
the index file itself, which has not heard of RFC-0099/0100.

### 5.3 The workflows: gated so it cannot fail, or claimed with no gate

Section 3. The three structural answers: (a) yes — nine codegen integration
tests early-return in every job (3.1), the bench gate cannot fire (3.5),
site's highlighter tests are not in the loop (3.3), and 48 std test blocks
run nowhere (3.4); (b) yes — the release gate proves the binary starts but
not that CI passed, the wasm it ships is only stat'ed, and the install
scripts (with their checksum promise) are executed by nothing (3.5); (c)
the Taelin instances are the dead-toolchain fetches (3.2) and the
committed-generated-docs chain (3.6).

---

## Counting

| | CONFIRMED | PLAUSIBLE |
|---|---|---|
| Section 1 (std) | 4 (1.1-1.4) | 3 (1.5*, 1.6, 1.7) |
| Section 2 (tooling) | 8 | 3 |
| Section 3 (gates) | 13 | 1 |
| Section 4 (RFCs) | 12 stale records verified against code | — |

*1.5's mechanism count is verified by citation; its latent mis-split is the
plausible half.

