# std quality census — thirty axes, thirty-eight modules

Census of every module in `std/` against the thirty quality axes. One report per
module in this directory. Read at commit `82234d6a01922072cd24c289cf49ed7c2d592c09`.

Method: one reader per module. Every performance claim carries a number from a
run of `compiler/target/release/vyrn bench` on a scratch file outside the
repository, or the loop bounds that prove a complexity claim. Claims with no
measurement say `NOT MEASURED`. Severity HIGH means an in-repo caller pays the
cost today; the caller is named in the module file.

Totals: 126 findings — 5 HIGH, 40 MEDIUM, 81 LOW.

## The table

Lines and Exports come from `wc -l` and a count of top-level `export fn` /
`export gen fn` lines at the census commit. Exported types, protocols, and
contracts are not in the Exports column; each module file names them.

| Module | Lines | Exports | HIGH | MEDIUM | LOW |
|---|---|---|---|---|---|
| [vyx](vyx.md) | 5261 | 27 | 0 | 2 | 3 |
| [ui](ui.md) | 3082 | 36 | 0 | 2 | 4 |
| [graphql](graphql.md) | 2556 | 12 | 0 | 2 | 1 |
| [i18n](i18n.md) | 1614 | 1 | 0 | 1 | 4 |
| [http](http.md) | 1822 | 13 | 0 | 2 | 1 |
| [von](von.md) | 1513 | 7 | 0 | 0 | 4 |
| [rpc](rpc.md) | 1475 | 7 | 0 | 2 | 1 |
| [icons](icons.md) | 1233 | 5 | 0 | 1 | 3 |
| [tw](tw.md) | 1127 | 1 | 0 | 1 | 1 |
| [num](num.md) | 884 | 5 | 1 | 3 | 0 |
| [vyx-hints](vyx-hints.md) | 894 | 3 | 0 | 1 | 3 |
| [cli](cli.md) | 848 | 10 | 0 | 0 | 2 |
| [html](html.md) | 848 | 11 | 1 | 2 | 1 |
| [jsonread](jsonread.md) | 704 | 1 | 0 | 1 | 3 |
| [contract](contract.md) | 587 | 3 | 0 | 0 | 2 |
| [jsondec](jsondec.md) | 452 | 24 | 1 | 1 | 2 |
| [strings](strings.md) | 434 | 19 | 0 | 1 | 3 |
| [scan](scan.md) | 394 | 14 | 0 | 2 | 2 |
| [codecs](codecs.md) | 359 | 7 | 0 | 0 | 3 |
| [connect](connect.md) | 380 | 2 | 0 | 0 | 3 |
| [hints](hints.md) | 367 | 5 | 1 | 1 | 0 |
| [text](text.md) | 333 | 6 | 0 | 1 | 2 |
| [strpred](strpred.md) | 313 | 9 | 0 | 1 | 3 |
| [openapi](openapi.md) | 327 | 1 | 1 | 0 | 2 |
| [stream](stream.md) | 279 | 7 | 0 | 4 | 3 |
| [slots](slots.md) | 278 | 8 | 0 | 2 | 3 |
| [bench](bench.md) | 268 | 8 | 0 | 0 | 5 |
| [args](args.md) | 226 | 6 | 0 | 1 | 1 |
| [hash](hash.md) | 173 | 4 | 0 | 1 | 2 |
| [time](time.md) | 148 | 13 | 0 | 0 | 2 |
| [symbolmap](symbolmap.md) | 132 | 4 | 0 | 0 | 3 |
| [random](random.md) | 128 | 4 | 0 | 0 | 2 |
| [math](math.md) | 124 | 8 | 0 | 1 | 1 |
| [diag](diag.md) | 110 | 2 | 0 | 1 | 1 |
| [storage](storage.md) | 94 | 1 | 0 | 1 | 3 |
| [arrays](arrays.md) | 94 | 7 | 0 | 1 | 2 |
| [fallible](fallible.md) | 29 | 0 | 0 | 0 | 1 |

## The twenty findings worth fixing first

Ranked by measured cost to a caller that exists in this repository today.
RECOMMENDATION, NOT A DECISION.

1. [jsondec](jsondec.md) — axis 8, HIGH. Every tree accessor deep-copies the
   subtree it returns, so a value nested d levels is copied about d times per
   decode: 332 µs for one lookup-copy of a 4096-element `JArr` against 2.06 µs
   for a scalar lookup. Every generated `fromJson` decode pays it.
2. [num](num.md) — axis 2, HIGH. `parseFloat64` is O(maxDigits × exponent/10):
   min 202.60 µs for `"1e300"` against 5.02 µs for `"12345.678"`. Caller
   std/jsondec.vyrn:36 — every JSON float parse goes through it.
3. [http](http.md) — axis 2, MEDIUM. The mount shadow audit re-runs on every
   request: 432 µs/request with two groups of 32 routes against 33 µs with one
   group of 64. Caller examples/bin/server.vyrn:81.
4. [hints](hints.md) — axis 2, HIGH. The waiver check walks the whole source
   from byte 0 twice per hint, O(H·N): min 1.07 ms for 64 late-line hints
   against 142.50 µs early-line. Payer std/vyx-hints.vyrn:300-488.
5. [html](html.md) — axis 15, HIGH. Keyed-tree diff of two identical trees
   costs more than building one: 201.27 µs diff (zero operations) against
   32.33 µs build. Every reactive re-render pays more to diff than to rebuild.
6. [http](http.md) — axis 8, MEDIUM. Every 200 response body is `parseJson`ed
   even with no policy (~2.7 µs tax at 4 KiB), three times when
   lastModified/createdAt are set. Caller
   examples/bin/server/api/pastes.http.vyrn:24.
7. [graphql](graphql.md) — axis 2, MEDIUM. Duplicate-key and duplicate-argument
   checks scan siblings linearly, O(k²): 14.59 µs at 64 siblings, 250.75 µs at
   512, per query parse. Callers examples/shelf/server.vyrn:16-18,
   examples/graphql.vyrn:38-39.
8. [vyx-hints](vyx-hints.md) — axis 2, MEDIUM. Report accumulation via
   `out = out + ...` is quadratic in fired reports: 2400 ms/call at 512 fired
   reports against 187 ms parse-only (`vyrn run`, growth ×2.5-×6.0 per
   doubling).
9. [i18n](i18n.md) — axis 2, MEDIUM. Generation is superlinear in key count and
   hard-fails at 200 keys against the generator step budget; keyCollisionErrors
   is O(K²·L) at std/i18n.vyrn:972-1004.
10. [json](json.md) — axis 2, MEDIUM. `emitPretty` is at least cubic in nesting
    depth through per-level space concatenation: 7.42 ms pretty against 39.30 µs
    compact on one 192-deep tree (189×).
11. [strings](strings.md) — axis 8, MEDIUM. `split` copies the needle-length
    string per match: median 3.33 ms against 429 µs without the copy, on a
    16 KB haystack with 4000 matches (~8×).
12. [graphql](graphql.md) — axis 8, MEDIUM. Projection deep-copies path and
    values per depth level, so projection is O(d²): 35.48 µs at 16 levels,
    500.95 µs at 64.
13. [hash](hash.md) — axis 8, MEDIUM. `sha1` allocates a fresh 80-word schedule
    array per block: sha1 633.30 µs against fnv1a 118.31 µs on identical 64 KiB
    input. Callers std/http.vyrn:707 (WebSocket handshake) and the ETag path.
14. [arrays](arrays.md) — axis 2, MEDIUM. `sortBy` is insertion sort plus a key
    function called twice per comparison: 1.48/4.76/19.37 µs at n=100/200/400
    (~4× per doubling).
15. [rpc](rpc.md) — axis 2, MEDIUM. Emitter output accumulates by string
    concatenation: 103.06 µs at 256 appends, 32.92 ms at 4096 (quadratic).
16. [strpred](strpred.md) — axis 8, MEDIUM. `startsWith`/`endsWith`/`contains`
    read lengths through `byteLengthV`, copying the whole string 2-3 times per
    call: exported contains 72.5 ns/call against 4.5 ns/call for a length-field
    rewrite (16×). Paid once per token by std/scan.vyrn:17, std/tw.vyrn:54, and
    the graphql parser.
17. [slots](slots.md) — axis 8, MEDIUM. `get` copies the payload on every read:
    bench of 10000 gets of a 64-byte string min 424.50 µs against 860 ns total
    for index-only access on the same payload. `handles()` adds ~496 µs per call
    at 100k elements.
18. [openapi](openapi.md) — axis 8, HIGH. `openapiJson()` re-parses baked schema
    JSON on every call: median 21.73 µs per call, paid per HTTP request.
    Callers examples/bin/server.vyrn:61, examples/shelf/server.vyrn:64.
19. [stream](stream.md) — axes 2/7, MEDIUM. `merge` drains both inputs into
    arrays before emitting the first element: first output is O(|a|+|b|) and an
    endless side never yields. Structural; no current caller merges live feeds.
20. [tw](tw.md) — axis 21, MEDIUM. `css()` bakes the whole vocabulary × 3 states
    regardless of usage: a 60,616-byte, 1422-rule sheet served per browser
    request at examples/shelf/server.vyrn:61, while site/guide/guide.vyrn:558
    promises only-used-rules emission.

## Patterns that repeat

A finding in three or more modules is a standard library design problem, not a
module problem. Five patterns cleared that bar.

### 1. Quadratic string accumulation (`out = out + ...`)

Appending to a string by concatenation copies the whole prefix each time.
Modules: i18n (std/i18n.vyrn:794, :891-901, :1220-1240), vyx-hints (2400 ms at
512 reports), rpc (103.06 µs → 32.92 ms from 256 to 4096 appends), json
(emitPretty's `spaces()`, ≥cubic in depth), cli (comptime loops at
std/cli.vyrn:595-598, :694-707, :721-729). Five modules carry the same defect.
Two agents also measured the counter-case honestly: repeat/joinWith and short
connect/ui accumulation scale near-linearly, so the cost appears only when the
append count reaches hundreds. A byte-array builder primitive in std would fix
all five sites at once.

### 2. Whole-value copies where a read would do

Vyrn `String`/`Array` values copy on access, and std leans on `.copy()` and
copying accessors everywhere. Modules: jsondec (deep copy per accessor level,
HIGH), slots (`get` copies the payload per read), contract (`matchedMember`
copies the export surface to answer one name query), strings (`split` copies
per match), hash (full padded input copy before hashing), text (`chars` holds a
bytes copy beside the array), scan (whole-source copy in all three
constructors), von (a String copy per token probe), jsonread (per-token byte
array plus double copy), vyx (`bytes()` inside a per-byte loop). Ten modules.
This is the largest measured cost class in the census and it points at one gap:
std has no borrow-shaped or view-shaped read API for strings and arrays.

### 3. Constant work rebuilt per call or per request

Tables, schemas, and audits that never change get rebuilt inside hot paths.
Modules: strpred (Boyer-Moore skip table rebuilt per `contains` call above the
512-byte threshold), strings (`toHex` rebuilds its digit table per call;
caller pays it per HTTP response through the ETag path), json (`hex2` digit
table per control character), icons (`reservedNames()` re-allocated per named
glyph), openapi (`parseJson` of baked schema per request, `oaTypeNames` rebuilt
per procedure), http (mount shadow audit per request), rpc ($schema registry
JSON re-emitted per request). Seven modules. None of these values changes after
startup; each wants a computed-once form.

### 4. Linear-scan lookup where a keyed structure is needed

The most repeated complexity defect. Without a map primitive, every "is this
key present" question becomes an O(n) scan and every dedup loop becomes O(n²).
Modules: jsondec (`hasField` + `fieldAt` double scan per field, O(F²)), graphql
(`gqlHas` sibling scans, O(k²)), i18n (keyCollisionErrors pair loop, O(K²·L)),
contract (`memberNames` linear dedup, `hasMember` rescan), cli (`cliOptAt`
linear scan per token-option probe), args (per-probe argv rescan), icons (linear
glyph scan per name), vyx (pairwise collision loops), ui (pairwise collision
loops), connect (O(T×S) import dedup), von (duplicate field detection). Eleven
modules. A single sorted-map or hash-map type in std would remove the quadratic
term from all eleven.

### 5. Redundant filesystem work

Modules: vyx-hints (`vhIsDir` lists each subdirectory just to test
directory-ness, then the scanner lists it again; 601 `listDir` calls on a
300-entry tree), ui (same double-list shape at std/ui.vyrn:1515-1520 and
:1560-1561), rpc (`rpcScan` lists each subdirectory twice), icons (one readFile
plus full parse of the collection per generated import site). Four modules.
One shared "list entries with kind" host helper would collapse all four.

## Measurement caveats

Three limits shaped what could be measured, recorded here so a `NOT MEASURED`
is read correctly:

- Generated functions have no native lowering, so `vyrn bench` cannot run them
  (clang: undefined symbol `@vyrn_<name>`). i18n, vyx-hints, icons, and
  contract timings therefore came from `vyrn run` wall-clock drivers or from
  generated artifacts, as noted in those files.
- Any file importing std/von fails native codegen today with
  ``error: field `toks` missing during coercion``; only the writer path was
  benched natively (von.md).
- Chained ui combinator benches abort `vyrn bench` with exit 116 and no output
  at three or more chained combinators (ui.md, axis 15). This looks like a
  codegen or harness bug, not a std defect; it is reported, not diagnosed.

---

## Correction, made on verification

The caveat above says "Any file importing std/von fails native codegen today".
That is wrong, and the module file `von.md` is the accurate one: the failure is
in `vyrn bench`, not in native code generation.

Isolated with a 12-line file that imports `std/von`, never calls it, and holds
one trivial bench block:

| command | `std/von` imported | result |
| --- | --- | --- |
| `vyrn build` | yes | **succeeds**, writes the executable |
| `vyrn run` | yes | **succeeds** |
| `vyrn bench` | yes | **fails**: ``error: field `toks` missing during coercion`` |
| `vyrn bench` | no | succeeds, reports the bench |

The repro:

```
import { copyVonArray } from "std/von"

fn work() -> Int64 {
    return 1
}

bench "trivial" {
    blackBox(work())
}

fn main() -> Int64 {
    return 0
}
```

`VonP` at `std/von.vyrn:162` is the only type with a `toks` field. The import is
never used, so the bench path is coercing something the build and run paths do
not.

**This is a compiler defect, not a standard library defect.** `vyrn bench`
cannot measure any program that imports `std/von`, which is why this census
could not measure that module's reader. It is reported, not diagnosed.
