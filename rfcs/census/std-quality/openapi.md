# std/openapi.vyrn

Lines: 327. Exports: 1 (`export gen fn openapi`, std/openapi.vyrn:236). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller imports the generator `openapi(contract)` at compile time and gets back a synthesized module exporting `openapiJson() -> String`: an OpenAPI 3.1 document describing the contract's `POST /rpc/<proc>` surface, with one `paths` entry per procedure in declaration order and a sorted `components/schemas` entry per type in the RFC-0031 closure. The compiler knows nothing about OpenAPI; the whole library is comptime-pure Vyrn string building over `moduleInterface`. In-repo callers serve the document at `/openapi.json`: `examples/bin/server.vyrn:61` and `examples/shelf/server.vyrn:64`.

## Findings

### 8. Allocation frequency — HIGH

What: the emitted `openapiJson()` re-parses every baked compact-JSON constant through `parseJson` and re-renders the whole tree with `emitPretty` on every single call, though the inputs are compile-time constants that never change.
Where: `std/openapi.vyrn:285` (one `oaNode(...)` parse per procedure) and `std/openapi.vyrn:291` (one `oaSchema(...)` parse per closure type), ending in `emitPretty(doc, 2)` at `std/openapi.vyrn:300`.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/openapi/b.vyrn` reports `bench "parseJson one path value" min 16.55 µs median 21.73 µs mean 22.73 µs` for one ~430-byte `oaPathValue` constant; a contract with P procedures and T types performs P+T such parses plus the full render per call.
Cost if unfixed: both in-repo servers re-pay this on every `GET /openapi.json` request (`examples/bin/server.vyrn:61`, `examples/shelf/server.vyrn:64`); a ten-procedure contract spends roughly 0.2 ms of pure recomputation per request producing identical bytes.
Smallest fix: bake the final pretty-printed document as a string constant in the generated source (or compute it once into a lazily initialized binding) instead of re-parsing and re-rendering per call. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: `oaSorted` is an insertion sort implemented as a full array rebuild per element, giving O(n²) comparisons and O(n²) string copies (each surviving element goes through `.copy()` on every pass).
Where: `std/openapi.vyrn:109-127` (inner copy loop at 114-120, `.copy()` at 116 and 119); called once per compile at `std/openapi.vyrn:289`.
Evidence: same command as above: `oaSorted 16 names` median 10.25 µs, `oaSorted 64 names` median 111.40 µs, `oaSorted 256 names` median 1.58 ms — 4× the input costs about 15× the time, twice in a row, which pins the quadratic term.
Cost if unfixed: only compile time of `openapi()` itself, and the header comment already scopes the sort to "small" closures (`std/openapi.vyrn:106`), so no in-repo caller pays a visible cost today.
Smallest fix: sort in place with index-based insertion into one array instead of rebuilding. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: the per-procedure path-value build rescans all closure types twice per procedure, making envelope generation O(procedures × types): `oaPathValue` rebuilds the whole type-name array inside its single-parameter branch, and `oaResponseSchema` linear-scans `iface.types` per procedure.
Where: `std/openapi.vyrn:214` (`oaRequestSchema(f.params[0].spelling, oaTypeNames(iface))` inside the per-procedure loop started at 284), `std/openapi.vyrn:199-204`, `std/openapi.vyrn:130-137` (scan called from 188).
Evidence: the nested-loop bounds prove the bound: `for f in iface.functions` (line 284) encloses `oaTypeNames`'s `for t in iface.types` (lines 225-229) and `oaIsNamedType`'s scan (131-136); timing NOT MEASURED separately.
Cost if unfixed: compile-time only, proportional to procedures times types; negligible at current repo contract sizes.
Smallest fix: hoist one `oaTypeNames(iface)` call above the procedure loop. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
