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

### Narrowing the `vyrn bench` defect

Further isolation, all with `compiler/target/release/vyrn`:

**Confirmed.** A file that imports `std/von`, never calls it, and holds one
trivial bench block fails. The same file under `vyrn build` and `vyrn run`
succeeds. Removing the import makes the bench pass.

**Not a `gen fn` problem.** The first guess was that `vyrn bench` natively lowers
comptime-only code that `build` prunes. It does not explain this: `std/vyx` has
`gen fn` exports and the same trivial bench file importing `vyxParseTemplate`
passes. `std/hash` passes. Only `std/von` fails.

| import | `vyrn bench` |
| --- | --- |
| `std/vyx` (`vyxParseTemplate`) | passes |
| `std/hash` (`fnv1aStr`) | passes |
| `std/von` (any export) | ``error: field `toks` missing during coercion`` |

**Where it comes from.** `compiler/vyrn-codegen/src/lib.rs:2817`, inside
`coerce`, rebuilding one record type into another. For each field of the target
it searches the source record and errors when the name is absent. So a record
lacking `toks` is being coerced into one that has it. `VonP` at
`std/von.vyrn:162` is the only type in the module with that field, and the only
place it is built is `walkVon` at `std/von.vyrn:732`, which constructs it from a
`consume Array<VonTok>` parameter and then passes it by value to
`firstErrorToken` at `std/von.vyrn:757`.

**A guess that was tested and failed.** That shape alone does not reproduce. A
standalone program with the same structure — a record built from a `consume`
array parameter, passed by value to a function that reads two of its fields,
beside a trivial bench block — compiles and benches cleanly under both `bench`
and `build`. Whatever triggers this needs more of `std/von` than its shape.

Recorded so the next attempt does not repeat the same guess. Still not
diagnosed.

**Verified gone (2026-08-28).** The twelve-line repro passes — and so does a
bench that actually calls `emitVon` at runtime — with the compiler at the
RFC-0118 head. The defect was fixed incidentally by one of the arcs between
the census commit and now (the record-coercion and generic-solving paths both
moved under RFC-0114..0118); nobody diagnosed it by name, so if it returns,
the narrowing above is still the map. `vyrn bench` can measure `std/von`
importers again.

### Correction: the map primitive already exists

Pattern 4 says "Without a map primitive, every 'is this key present' question
becomes an O(n) scan", and its conclusion is that "a single sorted-map or
hash-map type in std would remove the quadratic term from all eleven".

**Vyrn has had `Map<String, V>` since RFC-0028.** It is a language type, not a
library one: `let m: Map<String, Int64> = ["a": 1, "b": 2]` is in
`examples/branchtypes.vyrn:40`, and `std/contract` names the type throughout.

Its lookup is keyed, not scanned. `MapVal` at
`compiler/vyrn-frontend/src/interp.rs:417` holds ordered `pairs` beside an
`idx: HashMap<String, usize>`, and `get` is `self.idx.get(k).map(..)`. The `Vec`
exists to keep iteration order deterministic for the parity gate; the `HashMap`
answers the question.

So the eleven modules do not scan because the language gives them nothing
better. They scan for their own reasons — age, or a key that is not a String, or
a loop that grew a second purpose. **Each of the eleven has to be read on its own
terms**, and the fix is eleven local changes rather than one new type.

This matters because the two conclusions cost differently. "Add a map to std"
is language work and would have been started on a false premise. "Use the map
that is already there" is a refactor per module, each one measurable on its own,
and none of it blocks on a design decision.

### Note on pattern 5: the double list cannot be removed in Vyrn today

The obvious repair is to drop the `vhIsDir` guard and let `vhScan` handle a
non-directory. That would be wrong, and the code says why.

`listDir` answers `Ok(names)` or `Err(text)` and nothing else. A regular file
lists as an error and an unreadable directory lists as an error, and the two are
the same value. `vhScan` at `std/vyx-hints.vyrn` treats a failed list as an
ERROR on purpose — its own comment records the reason: a checker that silently
skipped an unreadable directory would tell a project its tree is checked while
it is not. So removing the guard would turn every regular file in the tree into
a spurious error.

Distinguishing them from the error TEXT is not available either. The project
single-sources canonical I/O error strings and refuses to depend on operating
system wording, which is what telling `ENOTDIR` from `EACCES` would require.

So the census's own recommendation is the right shape after all: an entry
listing that carries a kind. **That is a host capability, not a library change**,
and it is the repository owner's decision, not something to be slipped in while
fixing a performance finding. All four modules named in the pattern — vyx-hints,
ui, rpc, icons — are blocked on the same thing, which is a decent argument for
it.

Recorded so nobody removes the guard and calls it a fix.

---

## What is left, after the local fixes were taken

Every finding in this census was read. The ones that could be repaired without a
decision have been, on branch `fix/byte-column`: four constant tables that were
rebuilt per call, one predicate family that copied its haystack to read a length,
one waiver check that walked its source twice, and one symbol builder that could
collide the modules it exists to keep apart.

**What remains does not divide into thirty-eight module problems. It divides into
two questions, and both are the repository owner's.**

### Question one: can a read borrow?

Pattern 2 of this rollup, ten modules, the largest measured cost class. Also the
cause of three of the five benchmark gaps in `rfcs/census/benchmark-gaps.md`, and
of the five failed shapes recorded in RFC-0108 section 5c. Three findings from
three directions, none of which saw the others.

The remaining entries under it cannot be fixed locally, because each one is a
function that must hand a value to something else without copying it:
`slots.get` copying a payload per read, `contract.matchedMember` copying the whole
export surface to answer one name query, `jsondec` copying a subtree per accessor
level, `std/html`'s `diff` copying every unchanged subtree per event.

Written up as RFC-0109, with four candidate designs and the one question it
leaves open: whether a view can be STORED. The hot loop does not need that. These
ten modules do.

**2026-08-29: RFC-0120 answered the getter half.** Projections lost their
three-name gate and their `place`/`yield` spelling — any impl member
`fn f(read self, ..) -> read T` now reads in place at every engine, priced at
3 ns against the 24 ns copy in its witness bench. The conversion pass then
sharpened this question rather than closing it: `slots.get` waits on an
Option of a place, `jsondec` on a place surviving a `match`, `contract` on a
receiver for a free function's result. Each payer now names its missing
extension instead of citing RFC-0109 wholesale.

**Same day: RFC-0121 closed the `jsondec` row.** The refutable `let`
(`let JArr(items) = j` — bind the payload in the enclosing scope, or trap)
put an enum's payload inside a projection's flat prologue, and `std/json`
now reads `j[i]` and `j.field(key)` in place: 3 ns against 43 ns for the
copying reader on the census's own 4096-element shape (the copy scales with
the subtree; the place is flat). The tolerant `elemAt`/`fieldAt` stay for
the JNull-on-miss paths. Still waiting: `slots.get` (an Option of a place)
and `contract` (a receiver for a free function's result).

**Same day again: RFC-0122 closed the `slots.get` row.** An optional
projection (`-> read Option<T>`) misses to the `else` arm and hits as a
place, read only where `if let` tests it — no `Option` built, nothing
copied. `slots.tryAt` prices the same live handle at 3 ns against `get`'s
50 ns, and `json.tryField` gives absence the same treatment. `get` stays for
the value that must outlive the test. The last pattern-2 payer standing is
`contract`, on a receiver for a free function's result.

**And the `contract` row dissolved under measurement, no language needed.**
The census filed it as "must hand a value to something else without copying
it" — but the something else was `matchedMember`'s own helpers.
`moduleExports` materialized the whole export surface (every name, parameter
and return spelling copied) so `exportIndex` could find one row, and
`alternatives` copied the member rows so `matchIndex` could read them. The
query now compares the member rows and the reflected function where they
live (`fnSatisfies`, no `Export` built): 6.55 µs → 1.37 µs per query in
`examples/contractquery.vyrn`, answers pinned unchanged, and what remains is
`typeMatches` comparison work rather than copies. `checkContract` keeps
`moduleExports` — a full check reads every row it builds. **Pattern 2 is
closed**: every payer either reads in place (RFC-0120/0121/0122) or never
needed to escape at all.

### Question two: is there anywhere to put work that happens once?

Pattern 3, seven modules, plus `std/openapi` and `std/http` and `std/rpc`
recomputing per HTTP request. Every one of them wants the same thing and cannot
have it:

- `std/openapi` cannot bake its document at generation time, because
  `jsonSchema(Name)` resolves in the generated module and not in the generator
  that emits the text.
- `std/http`'s mount audit has, in its own words, "no init hook to hang it on",
  and memoizing needs a key meaning "the same routes I checked last time".
- Module state exists under RFC-0029 and answers some of this, but it is refused
  inside a `gen fn`, which is exactly where several of these live.

So the shape of the answer is a place for once-only work that a generator may
also use. That is a language question, not eleven library patches.

### The two that are neither

`vyrn bench` cannot measure any program importing `std/von`, which is a compiler
defect with a twelve-line repro, narrowed but not diagnosed. And `std/num`'s
float parser is 40 times slower on a large decimal exponent, which is real and is
its own piece of work with its own correctness corpus, not a performance patch.

---

## The twenty, re-judged (2026-08-29)

Every row re-read against the code at the RFC-0119 head and re-measured where
a measurement was cheap. The arcs between the census commit and now
(RFC-0114..0119, and the interpreter's amortized string append) closed some
rows without anyone acting on the census — which is why this table exists:
a list that stops being re-read starts lying.

| # | Module | Verdict | Today's evidence |
|---|---|---|---|
| 1 | jsondec | LIVE, by design | Copies at every accessor level; the scalar baseline got ~11x faster since the census, so the relative gap WIDENED (201.6 µs vs 0.18 µs). Blocked on RFC-0109 (can a read borrow). |
| 2 | num | CLOSED | `parseFloat64` takes Clinger's fast path: 4.92 µs → 155 ns on `"12345.678"`; `"1e300"` keeps the exact path. |
| 3 | http | CLOSED | The mount audit runs until it passes once, then stands for the process (module state). Was 280 µs/request re-measured (432 µs in the census). |
| 4 | hints | PARTIAL | The double walk is gone (one walk carrying the previous line's bounds); the per-hint restart from byte 0 remains, late-line ~200x early-line. |
| 5 | html | LIVE | Diff of two identical keyed trees still costs 6x a build (213 µs vs 35 µs); copies per level, partly an RFC-0109 case. |
| 6 | http | LIVE | Every policy-free 200 body still `parseJson`ed, ~2.9 µs at 4 KiB, up to three parses with stamps. |
| 7 | graphql | CLOSED (2026-08-29) | The duplicate-argument and duplicate-response-key accumulators are `Map<String, Bool>` now: 325.55 µs → 96.59 µs on a 512-sibling parse, messages unchanged. `gqlHas` survives only on the generator's cold import-dedup path. |
| 8 | vyx-hints | STALE | The `out = out + ...` pattern remains but the mechanism died: string append-assign is amortized-linear now; a fixed-total-bytes probe ran flat. |
| 9 | i18n | CLOSED (2026-08-29) | Two quadratics, closed in order of conviction. `keyCollisionErrors` re-derived both names per PAIR (84,620 `keyFnName` calls at 400 keys, 3.263 s of a 3.28 s generation) — a linear map screen now decides WHETHER any pair collides, and only then runs the old pair loop verbatim, so diagnostic bytes never move. Past that wall, `entryIndex` was a linear scan asked per key per locale per pass — `LocaleData` now carries a key→index map built once at flatten. 400 keys: hard fail → 0.62 s; scaling linear to 2000 (3.2 s, ~1.6 ms/key flat per doubling). |
| 10 | json | CLOSED (2026-08-29) | Two mechanisms, not one: the pads (now built as bytes) and — the dominant term — the recursion returning strings, every child's output copied into its parent per level. The writer accumulates into module state (a `modify` parameter is call-by-value-result, so threading one pays the same copies): depth 192 = 596.65 µs, from 10.35 ms, output byte-identical. |
| 11 | strings | STALE | `split` hoists the skip table ("ONE TABLE FOR EVERY MATCH"); measured at or under the census's own no-copy baseline. |
| 12 | graphql | CLOSED (2026-08-29) | The dominant term was the VALUES, twice: `gqlMemberOf` deep-copied the member's whole remaining subtree down per level, and `gqlPick` copied the result back up where `gqlProjectEach` already took it. The member now flows down as the place it is (RFC-0120) and the result is consumed: 459.80 µs → 150.42 µs at d64, output byte-identical, all 31 tests green. The path strings are still copied per position (O(d²) in count, ~a third of the residual beside parse) — killing those needs prepend-on-unwind through an exported signature, recorded here as not worth it at these sizes. |
| 13 | hash | CLOSED (2026-08-29) | The schedule is allocated once and overwritten per block — every word is written before it is read, so one buffer serves: 336.60 µs → 169.08 µs on 64 KiB, digest unchanged. |
| 14 | arrays | LIVE | `sortBy` is still insertion sort with the key called twice per comparison; `sortWith` landed BESIDE it, not instead of it. Census numbers reproduce. |
| 15 | rpc | STALE | The concat emitter is linear now: 16x the appends costs 11.4x the time. The census's 320x was the old quadratic append. |
| 16 | strpred | STALE | The predicates read the `byteLength` field; the module doc records the fix and keeps the census's numbers as history. |
| 17 | slots | LIVE | `get` still copies per read (468 µs / 10k gets); the `s[h]` place projection is a 9 ns/call escape hatch. `handles()` measured worse than the census (1.41 ms at 100k). |
| 18 | openapi | CLOSED | The generated `openapiJson()` builds once into module state: 45.31 µs → 34 ns per call. |
| 19 | stream | LIVE, deliberate | `merge` still drains eagerly and its post-census doc comment says so out loud, advising `take` on an endless side. |
| 20 | tw | LIVE | `css()` still bakes the whole vocabulary (60,616 bytes, byte-identical to the census). The guide sentence that promised only-used-rules emission is corrected to stop claiming it. |

Pattern 5 (the double list) closed wholesale: RFC-0119's `listDirKinds` marks
directories in the listing itself, and the three `isDir` helpers are deleted.
Pattern 3 (constant work per call) closed for its three census-named
per-request payers — openapi, rpc's registry, http's audit — via module
state, whose "refused inside a `gen fn`" turned out to be about the
generator's body, never its output. Pattern 1 (quadratic string
accumulation) dissolved underneath the census when append-assign went
amortized-linear. What remains open above is pattern 2 — the copies, which
are RFC-0109's question — plus the local LIVE rows, each fixable on its own
terms.

**Both re-judged 2026-08-28.** The `std/von` defect is GONE — the repro and a
bench that calls `emitVon` both pass; fixed incidentally by an RFC-0114..0118
arc, verified above. And finding 2 got the half that pays: `parseFloat64` now
takes Clinger's fast path — at most 15 significant digits and an exponent
within ±22 is one exact multiply or divide, correctly rounded by construction
— measured 4.92 µs → 155 ns on `"12345.678"`, the shape every JSON float in
this repository actually has. The pins at the path borders and the randomized
bit-identical differential against Rust's parser both hold. `"1e300"` still
walks the exact decimal machinery at ~200 µs: the remaining cost is confined
to exponents beyond ±22, which no caller in this repository parses outside a
test.
