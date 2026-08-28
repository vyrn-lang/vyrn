# A8 — Standard library calls that read wrong

Census of call shapes across all 38 modules of `std/`, plus the string
builtins. Read-only job. Nothing was renamed, no method was added, and `std/`
was not touched. Reproduction programs live in
`C:\Users\demko\AppData\Local\Temp\claude\ox-a8\` and are not committed.

## 1. Counts

| count | value |
|---|---|
| modules read | 38 (every module under `std/`) |
| exports read | 322 (`export fn` + `export gen fn` + protocol/impl methods) |
| classified rows | 344 — `std/http` also classifies 12 protocol methods beyond its 13 exports, `std/von` 2 impl methods, and the `std/ui` table includes 4 `Page` contract members |
| `SUBJECT FIRST` | 33 |
| **`SUBJECT AS ARGUMENT`** | **219** |
| `NO SUBJECT` | 71 |
| `AMBIGUOUS` | 21 |
| silent byte traps | 9 operations (section 2) |
| reproductions run | 7 programs, all with quoted output |
| live defects in this repository | 12 sites (section 3) |

Counting note: `std/num`'s table includes 8 private helpers beside its 5
exports; all 5 exports are `SUBJECT AS ARGUMENT`. Every other table covers
exports and protocol methods only.

The owner wanted this number first: **219 exports put their subject in
argument one.** The largest single block is the generator layer — `std/vyx`
(26), `std/jsondec` (16), `std/ui` (19), `std/time` (10) — followed by the
string and array layers a caller touches daily: `std/strings` (18),
`std/strpred` (8), `std/arrays` (7), `std/codecs` (7).

## 2. Silent byte traps

An operation lands here when it works in bytes, has no character-safe
sibling, and its name does not say `byte`. Listed first, as asked. The
character-safe surface that exists today is exactly: `charCount()` (method,
Unicode scalars), `chars(s)` (`std/text.vyrn:144`, full decode),
`charCountV(s)` (`std/text.vyrn:163`), and `decodeUtf8`
(`std/text.vyrn:66`). There is no character-indexed access, slice, search,
padding, distance, or column helper anywhere.

| operation | home | unit | what a caller assumes | what goes wrong on non-ASCII | char-safe sibling? | name says byte? |
|---|---|---|---|---|---|---|
| `s[i]` / `at(s,i)` | builtin | one UTF-8 byte, as `UInt8` | the i-th character | wrong character or a bare number like `195`; no error | no | no |
| `substring(s,start,end)` | std/strings.vyrn:87 | byte offsets | character positions | silent mis-cut, or process end on a mid-character offset | no | no |
| `slice(s,start,end)` | std/strpred.vyrn:289 | byte offsets | character positions | `Err(SplitsCharacter)` at best; a boundary-landing cut is silently short | no | no |
| `indexOf(s,needle)` | std/strings.vyrn:111 | returns byte offset | character index | every downstream position is off by the extra bytes before the match | no | no |
| `lastIndexOf(s,needle)` | std/strings.vyrn:129 | returns byte offset | character index | same as `indexOf` | no | no |
| `findPlain` / `findSkipping` | std/strpred.vyrn:150,183 | byte offset in, byte offset out | character index | same as `indexOf`, exported to every generator author | no | no |
| `padStart` / `padEnd` | std/strings.vyrn:320,334 | width in bytes | visual width in characters | columns misalign against ASCII padded the same way | no | no |
| `editDistance(a,b)` | std/strings.vyrn:376 | bytes | characters | one different accented character costs up to its byte length; a did-you-mean threshold mis-fires | no | no |
| `colAt(b,off)` / `colAtV` | builtin; std/text.vyrn:211 | bytes since last LF | columns as an editor shows them | every diagnostic column after a multi-byte character over-counts | no | no |

Named-byte and therefore not silent: `s.byteLength` (a field whose name says
byte), `bytes(s)`, `byteLengthV`, `stringFromBytes`, `readFileBytes`.
Safe by construction on valid UTF-8 (byte-wise scans that can only match on
character boundaries, per the self-synchronization argument at
`std/strpred.vyrn:44-50`): `contains`, `startsWith`, `endsWith`, `split`,
`replace`, `lines`, `splitWhitespace`, `trimStart`, `trimEnd`, `trim`.
ASCII-limited by design and documented per function: `toLower`, `toUpper`
(non-ASCII passes through unchanged, so text is never corrupted).

### Reproductions

Each program was run with `compiler/target/release/vyrn run` from
`C:\Users\demko\AppData\Local\Temp\claude\ox-a8\`.

**Trap 1 — `s[i]` yields a byte** (`trap-index.vyrn`)

```
fn main() -> Int64 {
    let s = "héllo"
    print(s[1])
    print(s.byteLength)
    print(s.charCount())
    return 0
}
```

Output:

```
195
6
5
```

`s[1]` is `195`, the lead byte of `é`. The character answer needs `chars(s)`.

**Trap 2 — `substring` with character-count intent cuts short**
(`trap-substring-width.vyrn`)

```
import { substring } from "std/strings"
fn main() -> Int64 {
    let s = "héllo wörld"
    print(substring(s, 0, 5))
    return 0
}
```

Output:

```
héll
```

The caller asked for the first five characters and got four, with no error.
Exit code 0.

**Trap 3 — `substring` with a mid-character offset ends the process**
(`trap-substring-crash.vyrn`) — same program with `substring(s, 0, 9)`:

```
error: substring: byte offset 9 is inside a multi-byte UTF-8 character (std/strings.vyrn:94)
exit=1
```

So the same wrong-unit call either fails loudly or lies quietly, depending on
where the offset happens to land. The quiet case is the dangerous one.

**Trap 4 — `lastIndexOf` answers in bytes** (`trap-indexof-unit.vyrn`)

```
import { lastIndexOf, substring } from "std/strings"
fn main() -> Int64 {
    let s = "héllo héllo"
    let at = lastIndexOf(s, "héllo") ?? panic("absent")
    print(at)
    print(substring(s, 0, at).charCount())
    return 0
}
```

Output:

```
7
6
```

The match sits at byte 7, but only 6 characters precede it. A caller that
treats the answer as a character position is wrong by the width of one `é`,
silently.

**Trap 5 — padding measures bytes, columns drift**
(`trap-padend.vyrn`)

```
import { padEnd } from "std/strings"
fn main() -> Int64 {
    print("[" + padEnd("cafe", 10, ".") + "]")
    print("[" + padEnd("café", 10, ".") + "]")
    return 0
}
```

Output:

```
[cafe......]
[café.....]
```

Both rows were padded to width 10; the second renders one column short.

**Trap 6 — edit distance counts bytes** (`trap-editdistance.vyrn`)

```
import { editDistance } from "std/strings"
fn main() -> Int64 {
    print(editDistance("e", "é"))
    return 0
}
```

Output:

```
2
```

One different character costs 2 — exactly RFC-0071's did-you-mean threshold,
so `"é"` qualifies as a close match for `"e"`.

**Trap 7 — columns are bytes** (`trap-colat.vyrn`)

```
fn main() -> Int64 {
    let b = bytes("é x")
    print(colAt(b, 3))
    return 0
}
```

Output:

```
4
```

In an editor, `x` on the line `é x` sits in column 3. Every Vyrn diagnostic
on a line holding any multi-byte character reports a larger column than the
one the user sees.

## 3. Live defects in this repository

Callers that already hold the bug, with `path:LINE`. Each was read and
checked; two subagent claims were tested and rejected (end of section).

1. `std/symbolmap.vyrn:93-103` — `mapSlug` walks `module.byteLength` and keeps only ASCII alphanumeric bytes. A non-ASCII character in a generator call disappears from the emitted function name, so two distinct calls can collide on one slug (`http(./påster)` and `http(./paster)`).
2. `std/icons.vyrn:903-923` — `camel` builds an icon identifier from `bytes(name)` with an ASCII-only letter test; every continuation byte fails both tests, so a non-ASCII glyph name loses characters and mis-capitalizes the next segment.
3. `std/bench.vyrn:102-110` — `padRight` pads to a byte width (the doc comment admits it). A bench label with any multi-byte character leaves the report column one short per character. Same defect as trap 5, shipped.
4. `std/scan.vyrn:144-156` — `advance` adds 1 to `col` per byte, so every line/column pair `std/scan` hands to generators is wrong on any line holding non-ASCII. Same defect as trap 7, shipped.
5. `std/jsonread.vyrn:77-92` — `step` advances `col` once per byte; the `col M:` prefix of every parse diagnostic under-counts what an editor shows.
6. `std/vyx.vyrn:2450-2454` — `argCol = a.col + argRel` adds a byte offset inside an attribute value to a 1-based column; the code comment says "byte offset" in so many words. Diagnostic-only.
7. `std/vyx.vyrn:229` — `vyxColAt` is documented as "(chars since the last LF)" while it returns `colAt`'s byte columns; the doc defect `std/text.vyrn:207-208` predicts, present at its own address.
8. `examples/lib/gen_table.vyrn:63-67` — `nameCol = nameStart - start + 1` derives a displayed column from byte offsets; a table row preceded by multi-byte text shows a shifted column.
9. `std/strings.vyrn:320-344` — `padStart`/`padEnd` themselves: the live API surface of trap 5.
10. `std/strings.vyrn:376-434` — `editDistance`: the live API surface of trap 6, feeding RFC-0071 thresholds.
11. `std/strings.vyrn:111,129` — `indexOf`/`lastIndexOf`: byte offsets handed to callers who have no character-position API to reconcile them against.
12. `std/ui.vyrn:714-728` — `uiSegIdent` drops every non-ASCII byte of a route slug while building a helper name (`café` becomes `caf`). The doc comment declares the lossy many-to-one conversion deliberate, so this is a recorded decision rather than an accident; it is listed because the failure is silent at the call site that matters — a slug the developer never sees truncated.

Checked and rejected:

- `std/cli.vyrn:115-117` — claimed as a mixed-unit split of `--name=value`. Both `indexOf(tok, "=")` and `substring` work in bytes, so the arithmetic is consistent and correct for any token. Not a defect.
- `std/icons.vyrn:928` — claimed as a bad first-byte test on a possibly non-ASCII name. `core` holds only bytes that survived the ASCII filter, so the first byte is always ASCII. Not a defect.
- `site/app/markdown.vyrn:115-120` — `boundaryCut` walks back over continuation bytes by hand. This is correct, and it is evidence of a different kind: site code already had to re-implement character-boundary logic because no character-safe slice exists.


## 4. Full export classification table, by module

One subsection per module. Each carries its classification table, its counts, its own smells, and its byte-op notes, exactly as collected.
### std/args
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| args | cli | fn cli() -> Args | live argv (not an argument) | cli() | cli() | SUBJECT FIRST |
| args | cliOf | fn cliOf(list: Array<String>) -> Args | NONE (builds a new Args; list is raw material, not a receiver) | cliOf(tokens) | constructor over an explicit token list; no distinguished receiver | NO SUBJECT |
| args | flag | fn flag(a: Args, name: String) -> Bool | a | flag(a, "--verbose") | a.flag("--verbose") | SUBJECT AS ARGUMENT |
| args | opt | fn opt(a: Args, name: String) -> Option<String> | a | opt(a, "--port") | a.opt("--port") | SUBJECT AS ARGUMENT |
| args | positionals | fn positionals(a: Args) -> Array<String> | a | positionals(a) | a.positionals() | SUBJECT AS ARGUMENT |
| args | rest | fn rest(a: Args, terminator: String) -> Array<String> | a | rest(a, "--") | a.rest("--") | SUBJECT AS ARGUMENT |

#### Counts
exports=6 SUBJECT FIRST=1 SUBJECT AS ARGUMENT=4 NO SUBJECT=1 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/args.vyrn:37 `terminatorAt` is private yet only three of the four probes honor it — `rest` re-scans argv with its own loop and its own terminator parameter, so `rest(a, "++")` diverges from the fixed `"--"` the other probes use.
- std/args.vyrn:77-80 `opt` calls `indexOf(tok, "=")` on every token before checking the name, so a positional like `a=b` earlier than the option takes the equals branch and never matches the space form — harmless today but the two forms are distinguished by token shape rather than by a leading-dash test.
- std/args.vyrn:111-118 `positionals` greedily treats any `-`-token plus following non-`-` token as option-plus-value and skips both; a free positional after a flag is silently eaten (documented at 99-104, still a caller trap).
- std/args.vyrn:135 `rest(a, terminator)` accepts an arbitrary terminator string, but nothing rejects an empty terminator, which would make everything after token 0 the rest.

#### Byte-shaped string ops seen in this module
- `indexOf(tok, "=")` + `substring(tok, 0, eq)` / `substring(tok, eq+1, tok.byteLength)` — std/args.vyrn:77-80, unit = bytes. Both cuts land on the single-byte ASCII `=` returned by `indexOf`, so the byte offsets stay on UTF-8 character boundaries even when the option name or value holds multi-byte characters. Currently safe; would break if the delimiter ever became multi-byte.

### std/arrays
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| arrays | map | `map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U>` | xs (arg 1) | `map(xs, f)` | `xs.map(f)` | SUBJECT AS ARGUMENT |
| arrays | filter | `filter<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Array<T>` | xs (arg 1) | `filter(xs, pred)` | `xs.filter(pred)` | SUBJECT AS ARGUMENT |
| arrays | fold | `fold<T, A>(xs: Array<T>, init: A, f: fn(A, T) -> A) -> A` | xs (arg 1) | `fold(xs, init, f)` | `xs.fold(init, f)` | SUBJECT AS ARGUMENT |
| arrays | any | `any<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool` | xs (arg 1) | `any(xs, pred)` | `xs.any(pred)` | SUBJECT AS ARGUMENT |
| arrays | all | `all<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool` | xs (arg 1) | `all(xs, pred)` | `xs.all(pred)` | SUBJECT AS ARGUMENT |
| arrays | includes | `includes(xs: Array<String>, x: String) -> Bool` | xs (arg 1) | `includes(xs, x)` | `xs.includes(x)` | SUBJECT AS ARGUMENT |
| arrays | sortBy | `sortBy<T>(xs: Array<T>, key: fn(T) -> Int64) -> Array<T>` | xs (arg 1) | `sortBy(xs, key)` | `xs.sortBy(key)` | SUBJECT AS ARGUMENT |

#### Counts
exports=7 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=7 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/arrays.vyrn:73 — sortBy is an O(n^2) insertion sort; fine for small arrays, but a growth path over large inputs will be quadratic.
- std/arrays.vyrn:61 — includes is concretely `Array<String>` by design (see doc comment); no generic membership sibling exists in this file.

#### Byte-shaped string ops seen in this module
- (none — the module never takes or returns byte positions, lengths, or values on String)

### std/bench
#### Classification
| module | export | signature | subject | call form today | should be | class |
| --- | --- | --- | --- | --- | --- | --- |
| std/bench | minOf | fn minOf(xs: Array<Int64>) -> Int64 | xs | minOf(samples) | xs.minOf() | SUBJECT AS ARGUMENT |
| std/bench | mean | fn mean(xs: Array<Int64>) -> Int64 | xs | mean(samples) | xs.mean() | SUBJECT AS ARGUMENT |
| std/bench | median | fn median(xs: Array<Int64>) -> Int64 | xs | median(samples) | xs.median() | SUBJECT AS ARGUMENT |
| std/bench | formatDuration | fn formatDuration(ns: Int64) -> String | ns | formatDuration(r.minNs) | pure computation on a scalar duration; no distinguished receiver | NO SUBJECT |
| std/bench | padRight | fn padRight(s: String, width: Int64) -> String | s | padRight("bench \"...\"", width) | s.padRight(width) | SUBJECT AS ARGUMENT |
| std/bench | BenchResult | type BenchResult = { name, minNs, medianNs, meanNs, samples, iters } | NONE | BenchResult { ... } | constructor; record literal, no receiver | NO SUBJECT |
| std/bench | benchMeasure | fn benchMeasure(name: String, body: fn() -> Unit) -> BenchResult | name vs body | benchMeasure(name, body) | operation times `body` (argument two) while `name` (argument one) is only a label; neither ordering is clearly subject-first | AMBIGUOUS |
| std/bench | benchOne | fn benchOne(name: String, width: Int64, body: fn() -> Unit) | name vs body | benchOne(name, width, body) | same shape as benchMeasure: `body` is what runs, `name` is label text | AMBIGUOUS |
| std/bench | benchJson | fn benchJson(results: Array<BenchResult>, backend: String, opt: String) -> String | results | benchJson(results, "native", "O2") | results.benchJson(backend, opt) | SUBJECT AS ARGUMENT |

#### Counts
exports=9 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=5 NO SUBJECT=2 AMBIGUOUS=2

#### Other smells (path:LINE relative to repo root)
- std/bench.vyrn:39 — private `sortedCopy` uses insertion sort, O(n^2); acceptable because sample count is capped near 31, but worth a comment bound or `sq.sort` if one exists.
- std/bench.vyrn:139-178 — nanosecond thresholds are bare literals (50000000, 1000000, 500000000, 2000000000); named constants would keep ms/s units from being misread.
- std/bench.vyrn:70-83 — private `pad2`/`twoDecimals` re-implement fixed-point decimal rendering inline; a shared int-format helper in std would remove duplication.
- std/bench.vyrn:199 — `benchOne` builds the padded label itself (`padRight("bench \"\{name}\"", width)`), so callers must pre-compute `width` against the decorated label, not the raw bench name; easy to get off-by-N columns.

#### Byte-shaped string ops seen in this module
- padRight, std/bench.vyrn:102-110 — unit: bytes (`s.byteLength`, line 104). Padding loop counts bytes, so a bench name with multi-byte characters produces short visual width and misaligned report columns. Doc comment (line 100) admits the byte measure. Fix needs a char/scalar-count sibling such as the existing `charCount()`; none is used here.

### std/cli

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| cli | readArgv | `(opts: Array<CliOpt>, argv: Array<String>) -> CliRead` | argv (walked); opts is only the spec | `readArgv(opts, argv)` | `argv.readArgv(opts)` | SUBJECT AS ARGUMENT |
| cli | cliFlag | `(r: CliRead, field: String) -> Bool` | r | `cliFlag(r, field)` | `r.cliFlag(field)` | SUBJECT AS ARGUMENT |
| cli | cliValue | `(r: CliRead, field: String) -> Option<String>` | r | `cliValue(r, field)` | `r.cliValue(field)` | SUBJECT AS ARGUMENT |
| cli | cliIssues | `(r: CliRead) -> Array<Issue>` | r | `cliIssues(r)` | `r.cliIssues()` | SUBJECT AS ARGUMENT |
| cli | wantsHelp | `(argv: Array<String>) -> Bool` | argv | `wantsHelp(argv)` | `argv.wantsHelp()` | SUBJECT AS ARGUMENT |
| cli | cliMissing | `(field: String, long: String) -> Issue` | NONE | `cliMissing(field, long)` | Issue constructor, no distinguished receiver | NO SUBJECT |
| cli | cliNotNumber | `(field: String, long: String) -> Issue` | NONE | `cliNotNumber(field, long)` | Issue constructor, no distinguished receiver | NO SUBJECT |
| cli | cliRefused | `(field: String, long: String, want: String) -> Issue` | NONE | `cliRefused(field, long, want)` | Issue constructor, no distinguished receiver | NO SUBJECT |
| cli | cliUnexpected | `(value: String) -> Issue` | NONE | `cliUnexpected(value)` | Issue constructor, no distinguished receiver | NO SUBJECT |
| cli | cli (gen) | `(module: String) -> String` | module (the reflected module) | `cli("./serve")` | `module.cli()` | SUBJECT AS ARGUMENT |

Exported types `CliOpt`, `CliHit`, `CliRead` are records, not functions; not classified. No protocol or impl declarations in this module. Non-exported helpers (`cliFieldsOf`, `cliTypeOf`, `cliBaseOf`, `cliPlansOf`, `cliCommand`, and the `fn` helpers) are out of scope per rules.

#### Counts
exports=10 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=6 NO SUBJECT=4 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/cli.vyrn:88 — `return 0 - 1` instead of `-1`; suggests the surface lacks negative integer literals, worth confirming rather than working around silently.
- std/cli.vyrn:172 — `cliIssues` rebuilds and deep-copies every `Issue` on each call (`cliIssueOf(e.key, e.path, e.message)`); the generated parser calls it once per parse, so every parse pays a full copy of the issues array.
- std/cli.vyrn:373 — `cliBytes` names a count of BYTES in user-facing help text ("at most 32 bytes long"); honest but byte-flavored wording leaking into CLI help.
- std/cli.vyrn:551 — help column alignment measures `l.byteLength`, then pads with `padEnd(lefts[i], width + 2, " ")`; if `padEnd` counts scalars, mixed-unit padding misaligns rows containing any wide text.
- std/cli.vyrn:23 — header doc states "the short name is the first byte of the field name"; byte semantics chosen deliberately for identifiers, but never checked against non-ASCII input.

#### Byte-shaped string ops seen in this module
- `tok.byteLength` + `substring(tok, ...)` — std/cli.vyrn:116-117 — bytes — splits the `--name=value` token at offsets from `indexOf(tok, "=")`; if `indexOf` returns a non-byte offset, both halves are wrong on non-ASCII tokens.
- `bytes(name)` loop — std/cli.vyrn:298-315 (`cliKebab`) — bytes — per-byte ASCII case fold; safe for identifiers, corrupts nothing because non-ASCII bytes pass through, but is byte-shaped by construction.
- `ba[0]` — std/cli.vyrn:334-346 (`cliInitial`) — bytes — short flag is the FIRST BYTE lowercased; a non-ASCII field name yields one broken UTF-8 byte as `-x`.
- `l.byteLength > width` — std/cli.vyrn:549-553 — bytes — help gutter width measured in bytes, padded by `padEnd` (unit unverified).
- `ty.byteLength - 1` in `cliInner` — std/cli.vyrn:289-294 — bytes — strips `Option<`/`>` at byte offsets; inputs are type spellings (ASCII identifiers), so not reachable with non-ASCII today.

Live-defect candidates are the argv-token slices only: argv text is user-controlled and routinely non-ASCII; field names and type spellings are compiler identifiers and effectively ASCII.

### std/codecs

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| codecs | hexEncode | fn hexEncode(s: String) -> String | s (the text whose UTF-8 bytes are hex-encoded) | hexEncode(s) | s.hexEncode() | SUBJECT AS ARGUMENT |
| codecs | hexDecode | fn hexDecode(s: String) -> Option<String> | s (the hex text being decoded) | hexDecode(s) | s.hexDecode() | SUBJECT AS ARGUMENT |
| codecs | base64Encode | fn base64Encode(s: String) -> String | s (the text whose UTF-8 bytes are base64-encoded) | base64Encode(s) | s.base64Encode() | SUBJECT AS ARGUMENT |
| codecs | base64EncodeBytes | fn base64EncodeBytes(b: Array<UInt8>) -> String | b (the byte array being encoded; may hold NUL or non-UTF-8) | base64EncodeBytes(b) | b.base64Encode() | SUBJECT AS ARGUMENT |
| codecs | base64Decode | fn base64Decode(s: String) -> Option<String> | s (the base64 text being decoded) | base64Decode(s) | s.base64Decode() | SUBJECT AS ARGUMENT |
| codecs | urlEncode | fn urlEncode(s: String) -> String | s (the text being percent-encoded) | urlEncode(s) | s.urlEncode() | SUBJECT AS ARGUMENT |
| codecs | urlDecode | fn urlDecode(s: String) -> Option<String> | s (the percent-encoded text being decoded) | urlDecode(s) | s.urlDecode() | SUBJECT AS ARGUMENT |

No `export gen fn` and no protocol/impl methods exist in this module; the eight private helpers (`hexDigit`, `hexDigitUpper`, `hexVal`, `decoded`, `ascii`, `b64Alphabet`, `b64Val`, `urlUnreserved`) are out of scope per the rules.

#### Counts
exports=7 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=7 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/codecs.vyrn:77, std/codecs.vyrn:88 — both `match stringFromBytes(b)` arms bind an error `e` that is never used; a wildcard arm would say the same thing.
- std/codecs.vyrn:130 — `b64Alphabet()` rebuilds the 64-entry `Array<UInt8>` from a literal on every call of `base64EncodeBytes`/`base64Decode`; `b64Val`'s comment rejects a reverse table as too costly per call, yet the forward table pays the same per call.
- std/codecs.vyrn:85 — `ascii` duplicates the `std/strings:fromBytesOr` fallback inline on purpose (module imports nothing); noted only because it is a second spelling of an existing helper.
- std/codecs.vyrn:57 — `hexVal`/`b64Val` return `-1` instead of an `Option`; intentional (one-comparison failure test, matches emitted IR), documented at :54.

#### Byte-shaped string ops seen in this module
- `bytes(s)` conversion then `b[i]` / `b.length` — std/codecs.vyrn:94-97, 109-110, 158, 165-168, 199-200, 254-257, 274-277 — unit: UTF-8 byte — every loop indexes the byte ARRAY, never the String itself, so non-ASCII input cannot be mis-sliced; the whole module is byte-correct by construction.
- Encoders treat each input BYTE independently (`hexEncode`, `urlEncode`: std/codecs.vyrn:97-101, 257-265) — unit: UTF-8 byte per output pair/triplet — correct for these codecs; a multi-byte char correctly becomes two hex digits per byte (test at :301 pins `"é"` → `"c3a9"`).
- Decoders validate the OUTPUT as UTF-8 via `stringFromBytes` (`decoded`, std/codecs.vyrn:74-79) before returning a String — invalid byte sequences answer `None` rather than producing mojibake (pinned at :308, :343).
- Length checks are in BYTES of the ASCII-coded input (`b.length % 2` at :110, `b.length % 4` at :200) — safe only because hex/base64 text is ASCII by definition; a non-ASCII digit would fail `hexVal`/`b64Val` anyway.
- Deliberate NUL refusal: decoders return `None` when output contains `0x00` because Vyrn Strings cannot hold NUL (:346-359); documented divergence from the deleted builtins, not a defect.

### std/connect

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| std/connect | `connectServer` | `(contract: String) -> String` | NONE | `connectServer("./contract")` | generator constructor over a contract specifier; emits a module source string, no distinguished receiver | NO SUBJECT |
| std/connect | `connectClient` | `(contract: String) -> String` | NONE | `connectClient("./contract")` | generator constructor over a contract specifier; emits a module source string, no distinguished receiver | NO SUBJECT |

Notes: std/connect has no protocol or impl blocks. All other top-level items are private `fn` helpers (outside the classification rule); they appear under Other smells where relevant.

#### Counts
exports=2 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=2 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/connect.vyrn:71 `connectJoin(parts, sep)` — private reinvention of a string join; subject `parts` is argument one of a free function. Private, so outside the export table, but it matches defect class 1 if ever exported.
- std/connect.vyrn:85 `connectListContains(items, needle)` — hand-rolled linear membership test over `Array<String>`; subject `items` is argument one.
- std/connect.vyrn:95 `connectIsNamedType(spelling, iface)` — two plausible subjects (`spelling`, `iface`); would be AMBIGUOUS if exported.
- std/connect.vyrn:170,173,181,186 `.copy()` calls sprinkled to satisfy value semantics while building import specs/names; noisy but mechanical.
- std/connect.vyrn:55-68,107-136 both helpers rebuild strings through `Array<UInt8>` round-trips instead of any char-safe slice helper; see next section.
- std/connect.vyrn:282-285 generated modules each embed private `connectFail400`/`connectCodeErr` copies — deliberate per the std-generator convention (header comment at lines 42-45), noted for completeness.

#### Byte-shaped string ops seen in this module
- `connectCapFirst` — std/connect.vyrn:55-68 — unit: bytes (`bytes(s)`, `b >= 'a' && b <= 'z'`, `b - 32`). Breaks nothing today: a non-ASCII lead byte fails the a-z test and passes through, and `fromBytesOr(out, s)` (line 67) restores the original on invalid UTF-8. Still byte-shaped where `std/text` `chars(s)` would express it directly.
- `connectServiceName` — std/connect.vyrn:107-136 — unit: bytes (`ba[i] == '/'` scan at 112-117, byte copy at 118-123, `nb.length - 5` suffix strip at 129). Scanning for `/` and stripping the 5-byte ASCII suffix `.vyrn` are safe because UTF-8 continuation bytes never collide with ASCII, and `fromBytesOr` (124, 133) falls back to the input. Correct by luck of ASCII delimiters, not by design.

### Live byte defects
None found. Every byte-shaped op above either targets an ASCII-only delimiter/suffix or routes through the lossless `fromBytesOr` fallback, so non-ASCII input survives. They remain fragile byte-shaped code worth migrating to char-safe siblings, but no in-module bug fires on non-ASCII today.

### std/contract

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| contract | checkContract | fn checkContract(iface: ModuleInterface, c: ContractInfo) -> Array<Issue> | AMBIGUOUS: `iface` (module surface being checked) vs `c` (contract it is checked against) | checkContract(iface, c) | either `iface.checkContract(c)` (doc reads "check a module against a contract") or `c.check(iface)`; both readings are defensible | AMBIGUOUS |
| contract | suppliesMember | fn suppliesMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Bool | AMBIGUOUS: `iface` vs `c` (`name` is a key into `c`, not a receiver candidate) | suppliesMember(iface, c, name) | `c.supplies(iface, name)` or `iface.supplies(c, name)`; no single reading wins | AMBIGUOUS |
| contract | matchedMember | fn matchedMember(iface: ModuleInterface, c: ContractInfo, name: String) -> Int64 | AMBIGUOUS: `iface` vs `c` (same shape as `suppliesMember`) | matchedMember(iface, c, name) | `c.matched(iface, name)` or `iface.matchedMember(c, name)`; both defensible | AMBIGUOUS |

No `export gen fn`, no protocol/impl declarations in this file.

#### Counts
exports=3 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=0 AMBIGUOUS=3

#### Other smells (path:LINE relative to repo root)
- std/contract.vyrn:63 `fn nearThreshold() -> Int64` wraps a constant 2 in a function called per comparison loop; a named constant would be plainer.
- std/contract.vyrn:80,114,149,152 `slice(...) ?? panic(...)` repeated five times with near-identical messages; the invariant (offset came from scanning the same string) is restated inline each time.
- std/contract.vyrn:70-81 `trimSpaces` hand-rolls an ASCII-space trim over `bytes()` although trimming is a common string concern; worth checking whether std/strings grows a char-safe trim before this stays local.
- std/contract.vyrn:380,386,547,552 `0 - 1` written out instead of `-1`; consistent but noisy.
- std/contract.vyrn:269-271 `issue(key, path, message)` constructor takes three unrelated values; genuinely free, listed only for completeness (not an export).

#### Byte-shaped string ops seen in this module
- `bytes(s)` + `b.length` + `b[i]` byte scan + `slice(s, start, end)` — std/contract.vyrn:70-81 (`trimSpaces`), unit = UTF-8 bytes. Breaks nothing today: it trims only ASCII spaces, and the offsets come from scanning the very same byte buffer, so they stay consistent on non-ASCII input. Still byte-shaped; a char-safe `trim` sibling would remove the pattern entirely.
- `b[i] == '<'` byte scan + `slice(spelling, 0, i)` — std/contract.vyrn:109-119 (`headOf`), unit = bytes. Safe on non-ASCII because `<` is ASCII and cannot occur inside a multi-byte UTF-8 sequence; the cut lands on a character boundary.
- depth-0 `<`/`>`/`,` byte scan + slices — std/contract.vyrn:123-158 (`argsOf`), unit = bytes. Same reasoning as `headOf`: all delimiters are ASCII, so cuts are boundary-safe.
- `bytes(name)` + `b[0]`/`b[i]` range checks — std/contract.vyrn:90-106 (`isTypeParam`), unit = bytes. Input is by grammar an ASCII identifier; non-ASCII simply fails the checks correctly.

#### Live byte defects
None. Every byte-position `slice` in this module derives its offsets from a scan of the same string's own byte buffer, and every scanned delimiter/space is ASCII, so cuts stay on UTF-8 boundaries. No defect where a byte count is treated as a char count.

### std/diag
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| diag | Severity | `export type Severity = \| Warning \| Error` | NONE | `Severity` (type) | enum type declaration; no receiver | NO SUBJECT |
| diag | report | `(severity: Severity, file: String, line: Int64, col: Int64, message: String) -> String` | NONE | `report(Warning, path, 9, 3, msg)` | constructor of a directive line from five independent parts; no distinguished receiver | NO SUBJECT |
| diag | reportHere | `(severity: Severity, message: String) -> String` | NONE | `reportHere(Warning, msg)` | constructor of an unanchored directive line; severity is not a subject | NO SUBJECT |

#### Counts
exports=3 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=3 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/diag.vyrn:41 `report` takes five positional parameters (`file`, `line`, `col`, `message` adjacent same-typed Int64 pair); swapping `line`/`col` type-checks and silently misanchors.
- std/diag.vyrn:70 `oneLine` replaces `\n` then `\r` independently; a `\r\n` pair collapses into two spaces instead of one. Harmless but visible in editor output.
- std/diag.vyrn:48 anchor built by string concatenation with manual `.toString()` calls; a small local helper would keep the `file:line:col` shape in one place (also used at no other site, so low priority).
- std/diag.vyrn:76 `atLeastOne` clamps silently by design (documented at :73-75); fine, but callers cannot distinguish a real position 0 from an off-by-one.

#### Byte-shaped string ops seen in this module
- none. The module only does whole-string interpolation, equality, and pattern `replace` on `\n`/`\r` (std/diag.vyrn:70); it never takes byte positions, byte lengths, or indexes a String.

### std/fallible
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| fallible | protocol Fallible (type Output + 2 fns) | `type Output` | NONE | `T: Fallible` bound | constructor-like associated type; no receiver | NO SUBJECT (associated type decl, pure type-level member of protocol) |
| fallible | Fallible.isSuccess | `fn isSuccess(self) -> Bool` | self | `x.isSuccess()` via protocol resolution (`x?`) | already a method | SUBJECT FIRST |
| fallible | Fallible.success | `fn success(self) -> Output` | self | `x.success()` via protocol resolution | already a method | SUBJECT FIRST |

Note: the single `export` declaration is the protocol; its two methods are counted as rows per the shared rule ("every protocol/impl method"). `exports=3` below counts classified rows so the class counts sum to it.
#### Counts
exports=3 SUBJECT FIRST=2 SUBJECT AS ARGUMENT=0 NO SUBJECT=1 AMBIGUOUS=0
#### Other smells (path:LINE relative to repo root)
- std/fallible.vyrn:18 — `success` is total-but-panicking on failing arms by contract; fine per doc comment (RFC-0079 M1), no change needed.
#### Byte-shaped string ops seen in this module
- (none — module declares a protocol over arbitrary Output types; no String/byte handling)

### std/graphql

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| graphql | sdl | gen fn sdl(contract: String) -> String | contract | sdl(contract) | contract.sdl() | SUBJECT AS ARGUMENT |
| graphql | graphqlServer | gen fn graphqlServer(contract: String) -> String | contract | graphqlServer(contract) | contract.graphqlServer() | SUBJECT AS ARGUMENT |
| graphql | gqlParseQuery | fn gqlParseQuery(src: String) -> GqlQuery | src | gqlParseQuery(src) | src.parseGqlQuery() | SUBJECT AS ARGUMENT |
| graphql | gqlQueryText | fn gqlQueryText(body: String) -> String | body | gqlQueryText(body) | body.gqlQueryText() | SUBJECT AS ARGUMENT |
| graphql | gqlAnswer | fn gqlAnswer(body: String, resolve: fn(String, String, Array<JsonField>) -> Result<Json, String>, schema: fn(String, String) -> String) -> Response | body | gqlAnswer(body, resolve, schema) | body.gqlAnswer(resolve, schema) | SUBJECT AS ARGUMENT |
| graphql | gqlProject | fn gqlProject(v: Json, sels: Array<GqlSel>, tref: String, path: Array<Json>, schema: fn(String, String) -> String) -> GqlOut | v (the value tree projected; `sels` is the projection applied to it) | gqlProject(v, sels, tref, path, schema) | v.project(sels, tref, path, schema) | SUBJECT AS ARGUMENT |
| graphql | gqlRootType | fn gqlRootType(root: String) -> String | root | gqlRootType(root) | root.toGqlRootType() | SUBJECT AS ARGUMENT |
| graphql | gqlArgOf | fn gqlArgOf(field: String, args: Array<JsonField>, want: String) -> GqlArg | AMBIGUOUS: args (the list searched) vs want (the key sought); field names the error context only | gqlArgOf(field, args, want) | args.argOf(want, field) or args.find(want) | AMBIGUOUS |
| graphql | gqlNoArgs | fn gqlNoArgs(field: String, args: Array<JsonField>) -> String | AMBIGUOUS: args (the list under test) vs field (the declaration the args are checked against) | gqlNoArgs(field, args) | args.noStrayFor(field) or field.refuseStrayArgs(args) | AMBIGUOUS |
| graphql | gqlErrorBody | fn gqlErrorBody(message: String) -> String | NONE | gqlErrorBody(message) | constructor of the request-fault reply envelope; message is payload, no distinguished receiver | NO SUBJECT |
| graphql | gqlArgError | fn gqlArgError(field: String, issues: Array<Issue>) -> String | NONE | gqlArgError(field, issues) | error-message formatter; both arguments are ingredients | NO SUBJECT |
| graphql | GqlSel.release | impl Owned for GqlSel: fn release(consume self) | self | sel.release() | already a method | SUBJECT FIRST |

#### Counts
exports=11 SUBJECT FIRST=1 SUBJECT AS ARGUMENT=7 NO SUBJECT=2 AMBIGUOUS=2
(Class counts cover all 12 table rows: 11 exports plus the one protocol method, `impl Owned for GqlSel::release`.)

#### Other smells (path:LINE relative to repo root)
- std/graphql.vyrn:93 `gqlSlice` rebuilds every substring through a per-byte push loop plus `fromBytesOr`, though `std/strpred.slice` (imported at :86) already does bounds and character-boundary checks; each call copies its whole range.
- std/graphql.vyrn:597-609 `gqlStripWhere` calls `gqlSlice(ba, i, i + 7)` and compares at every offset — quadratic on long field spellings; `contains`/byte scan would find `" where "` once.
- std/graphql.vyrn:1925-1936 `gqlJoin(parts, sep)` reimplements array join by hand; the owner's canonical shape is `parts.joinWith(sep)`.
- std/graphql.vyrn:1915-1922 `gqlHas(items, needle)` reimplements array membership; a `contains`-style method on `Array<String>` would delete it (used at :1162, :1230-ish keys checks, :1890).
- std/graphql.vyrn:955 `gqlScanner` passes bare numeric literals `34`, `-1`, `92` (quote byte, no-block-comment marker, escape byte) into `scanner`; named constants would keep the lexing config legible.
- std/graphql.vyrn:802-811 `gqlRootMembers` re-walks `iface.functions` on every call; `gqlResolverTable` (:1983) and `gqlSchemaTable` (:2066, :2071) recompute roots the SDL pass already computed — comptime-only, so cost not correctness.
- std/graphql.vyrn:1263 vs :1443/:1806: parse entry (`gqlParseQuery`) takes the source first, but `gqlCheckSel` (:1329) takes `owner, field` with the schema callback last — parameter order across the executor family is consistent enough, yet `schema` as trailing callback appears in six signatures; a named callback record would shorten them all.

#### Byte-shaped string ops seen in this module
The module is built entirely on byte cursors over `String`: `bytes(s)` plus index loops, `ba.length` as the bound, and `gqlSlice` ranges.
- std/graphql.vyrn:93 `gqlSlice(ba, s, e)` — byte range to String; falls back to `""` via `fromBytesOr` if the range splits a character. Not reachable from current callers: every boundary comes from a match on an ASCII byte.
- std/graphql.vyrn:109 `gqlTrim` — ASCII whitespace trim over byte indices; safe on UTF-8 (ASCII ws bytes never occur inside multibyte sequences), but trims only ASCII whitespace by design.
- std/graphql.vyrn:124 `gqlStripBang` — drops the last byte after `s.endsWith("!")`; correct because the matched suffix is exactly one ASCII byte.
- std/graphql.vyrn:134/:575/:624 `gqlBetweenAngles` / `gqlBetweenBraces` / `gqlBetweenParens` — byte scans for ASCII delimiters over canonical type sources (ASCII identifiers); boundaries cannot split a character.
- std/graphql.vyrn:162 `gqlSplitTop` — splits on a separator BYTE with bracket nesting and string-literal awareness; operates on canonical declaration text, ASCII throughout. Emptiness tested via `part.byteLength > 0` (:194, :204) — an emptiness check in bytes, harmless.
- std/graphql.vyrn:384 `gqlEscTripleQuote` and :405 `gqlFoldWs` — byte-wise transforms over human-authored `///` doc text, which CAN hold non-ASCII. Both pass bytes >= 0x80 through untouched, so they stay correct only because of the UTF-8 self-synchronizing property; the file nowhere states that invariant.
- std/graphql.vyrn:1008/:1072 `slice(sc.src, start, sc.pos) ?? ""` — `std/strpred.slice` over scanner byte positions; positions come from ASCII-delimiter scanning, and the callee refuses character-splitting ranges anyway.
Nothing in the module takes or returns a CHARACTER position, and no byte-shaped op here is silently wrong on non-ASCII input today; the exposure is the unstated reliance on UTF-8 self-synchronization in `gqlSlice`'s silent `""` fallback.

### std/hash

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| hash | fnv1a | fn fnv1a(data: Array<UInt8>) -> UInt64 | data | fnv1a(data) | pure computation (digest of an input; no distinguished receiver) | NO SUBJECT |
| hash | fnv1aStr | fn fnv1aStr(s: String) -> UInt64 | s | fnv1aStr(s) | pure computation (UTF-8 bytes of `s` digested; see byte-op note) | NO SUBJECT |
| hash | sha1 | fn sha1(data: Array<UInt8>) -> Array<UInt8> | data | sha1(data) | pure computation (digest of an input) | NO SUBJECT |
| hash | sha1Hex | fn sha1Hex(s: String) -> String | s | sha1Hex(s) | pure computation (hex spelling of `sha1(bytes(s))`) | NO SUBJECT |

Notes on the class calls:

- All four exports are deterministic transforms of their argument into a digest. Nothing is mutated or queried *on* the argument, so none has a subject in the `sq.push(x)` sense; the module doc frames them as pure functions over byte sequences.
- If Vyrn ever grows method-call sugar for value-to-value transforms, `fnv1aStr` and `sha1Hex` are the natural candidates (`s.fnv1a()`, `s.sha1Hex()`), since they exist only to adapt the String overload. Recorded here as an observation, not a defect; class stays NO SUBJECT under the given rules.
- Non-exported helpers `sha1Rotl1`, `sha1Rotl5`, `sha1Rotl30` (std/hash.vyrn:163-173) are private bit rotations, outside the census scope (not exports, not protocol methods).

#### Counts
exports=4 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=4 AMBIGUOUS=0

#### Other smells
- std/hash.vyrn:154-157 — `sha1Hex` swallows the `stringFromBytes` failure and returns `""`, indistinguishable from a real digest of empty-prefixed output; a caller cannot tell error from success.
- std/hash.vyrn:65-72 — `sha1` copies the whole message into a fresh array byte-by-byte before padding; a copy-free append would avoid doubling peak memory for large inputs.
- std/hash.vyrn:148 — hex digit table built by calling `bytes("0123456789abcdef")` on every call instead of a constant; wasted work per invocation.

#### Byte-shaped string ops seen in this module
- `fnv1aStr` — std/hash.vyrn:30-31 — unit: UTF-8 bytes via `bytes(s)` — nothing breaks on non-ASCII: the operation is defined over the exact UTF-8 byte sequence, which is the documented intent (std/hash.vyrn:28-29). Deterministic regardless of content.
- `sha1Hex` — std/hash.vyrn:147,150 — unit: UTF-8 bytes via `bytes(s)` fed to `sha1` — same as above: hashing is byte-oriented by definition, so non-ASCII input changes the digest but is never silently mis-sliced. The hex encoding afterwards operates on the digest bytes, not on string positions.
- The module never indexes a String with `s[i]`, never takes byte positions or `byteLength` of a String, so there is no position-arithmetic defect surface here.

### std/hints

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| hints | noPolicy | `noPolicy() -> Policy` | NONE | `noPolicy()` | constructor | NO SUBJECT |
| hints | policyOf | `policyOf(configText: String, key: String) -> Result<Policy, String>` | configText | `policyOf(configText, key)` | `configText.policyOf(key)` | SUBJECT AS ARGUMENT |
| hints | levelOf | `levelOf(p: Policy, code: String, dflt: Severity) -> String` | p | `levelOf(p, code, dflt)` | `p.levelOf(code, dflt)` | SUBJECT AS ARGUMENT |
| hints | hint | `hint(p: Policy, code: String, dflt: Severity, src: String, file: String, line: Int64, col: Int64, message: String) -> String` | p | `hint(p, code, dflt, src, file, line, col, message)` | `p.hint(code, dflt, src, file, line, col, message)` | SUBJECT AS ARGUMENT |
| hints | waived | `waived(src: String, line: Int64, code: String) -> Bool` | src, code | `waived(src, line, code)` | `src.waivedAt(line, code)` or `code.isWaivedAt(src, line)` — either reading works | AMBIGUOUS |

Notes: `Policy` is an exported type, not an export fn, so it gets no row. The module has eight private helpers (`waivesLine`, `continuesCode`, `lineText`, `fieldsOf`, `isObject`, `stringOf`, plus test helpers `policyOr`, `refusal`); they are not exports and get no rows.

#### Counts
exports=5 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=3 NO SUBJECT=1 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/hints.vyrn:124-136 — `levelOf` returns a level word (`String`) and `hint` at :162 maps it back to a `Severity`; a stringly-typed round trip through the same module. A `Severity`-returning lookup would delete the mapping.
- std/hints.vyrn:168-176 — `waived` calls `lineText` twice per query; each call re-walks `src` from byte 0, so a waiver check is two full newline walks plus a scan. Fine for lines, but the walk could yield both lines in one pass.
- std/hints.vyrn:184-204 — `waivesLine` is an O(n·m) naive byte search per line; acceptable at line scale, worth a note only.
- std/hints.vyrn:269-278 — `stringOf` returns `""` for every non-string JSON value, silently conflating "not a string" with the empty level word; the caller's comparison happens to reject it, as the doc admits. A `Result<String, String>` would be honest.
- std/hints.vyrn:69 — `contains` from `std/strpred` is imported at module top but used only inside tests; import could move with the tests or the tests could qualify it.

#### Byte-shaped string ops seen in this module
- `line.byteLength` / `marker.byteLength`, std/hints.vyrn:186-187 — unit: bytes. Used only as a length bound for an ASCII-marker match; no breakage on non-ASCII (a UTF-8 continuation byte never equals an ASCII marker byte).
- `bytes(marker)` + `line[i + j] == mb[j]`, std/hints.vyrn:191-198 — unit: bytes. Matching a pure-ASCII marker byte-wise over UTF-8 text cannot false-positive on multibyte sequences; safe.
- `line[at]` in `continuesCode`, std/hints.vyrn:208-213 — unit: bytes (`UInt8`). A non-ASCII byte falls outside every ASCII range and so correctly terminates a code; safe by construction.
- `src.byteLength` + `src[i] == '\n'` walk + `substring(src, start, i)`, std/hints.vyrn:223-238 — unit: bytes for length and slice bounds. Safe here: `\n` (0x0A) never occurs inside a UTF-8 multibyte sequence, so every slice boundary is a newline, which is a character boundary. The doc comment at :216-218 states this invariant.
- Net: this module is byte-shaped throughout but has no live defect — every byte position consumed is either produced by the newline walk or compared against an ASCII-only pattern.

### std/html

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| html | yes | `fn copy(self) -> Html` (impl Copy for Html) | self | `h.copy()` | already a method | SUBJECT FIRST |
| html | yes | `fn release(consume self)` (impl Owned for Html) | self | `drop h` / runtime call | already a method | SUBJECT FIRST |
| html | yes | `fn copyHtmlArray(ks: Array<Html>) -> Array<Html>` | ks (arg 1) | `copyHtmlArray(ks)` | `ks.copy()` (doc itself admits this: "Exported because `ks.copy()` cannot be written") | SUBJECT AS ARGUMENT |
| html | yes | `fn el(tag: consume String, attrs: consume Array<Attr>, kids: consume Array<Html>) -> Html` | NONE | `el(tag, attrs, kids)` | constructor | NO SUBJECT |
| html | yes | `fn text(s: consume String) -> Html` | NONE | `text(s)` | constructor | NO SUBJECT |
| html | yes | `fn empty() -> Html` | NONE | `empty()` | constructor | NO SUBJECT |
| html | yes | `fn cls(s: consume String) -> Attr` | NONE | `cls(s)` | constructor | NO SUBJECT |
| html | yes | `fn attr(n: consume String, v: consume String) -> Attr` | NONE | `attr(n, v)` | constructor | NO SUBJECT |
| html | yes | `fn on(event: consume String, handler: consume String, payload: consume String) -> Attr` | NONE | `on(event, handler, payload)` | constructor | NO SUBJECT |
| html | yes | `fn keyed(k: consume String, node: consume Html) -> Html` | node (arg 2) vs k (arg 1) | `keyed(k, node)` | `node.keyed(k)` if node wins; `k.keyed(node)` if key wins — see note below | AMBIGUOUS |
| html | yes | `fn toHtmlString(h: Html) -> String` | h (arg 1) | `toHtmlString(h)` | `h.toHtmlString()` | SUBJECT AS ARGUMENT |
| html | yes | `fn diff(old: Html, new: Html) -> Array<PatchOp>` | old (arg 1) vs new (arg 2) — symmetric operands | `diff(old, new)` | `old.diff(new)` if first operand wins; binary operator otherwise | AMBIGUOUS |
| html | yes | `fn document(title: String, head: Array<Html>, body: Html) -> String` | NONE | `document(title, head, body)` | page assembly over three parts; no single distinguished receiver | NO SUBJECT |

Note on `keyed`: the node is transformed and the key is metadata attached to it, so `node.keyed(k)` reads best; but the subject sits at argument TWO, which fails the strict "argument one" test for SUBJECT AS ARGUMENT, hence AMBIGUOUS with both candidates named.

Note on `diff`: `old` and `new` are equal-rank operands of a binary relation; either could claim the slot. Named both per the rules.

#### Counts
exports=13 SUBJECT FIRST=2 SUBJECT AS ARGUMENT=2 NO SUBJECT=7 AMBIGUOUS=2

#### Other smells (path:LINE relative to repo root)
- std/html.vyrn:139 — `copyHtmlArray` exists only because the builtin `.copy()` cannot answer about a part of a value; a partial-copy primitive would delete it (the doc comment says exactly this).
- std/html.vyrn:579-581 — `attrsEqual` decides equality by RENDERING both attribute lists to strings: O(size) allocation per compared element during every diff; a structural `==` on `Attr` would remove it (comment admits false negatives cost redundant ops).
- std/html.vyrn:201-208 — `keyed` on a non-element node silently discards the key (documented as total, but a caller keying a text node gets no signal).
- std/html.vyrn:606-620 — `diffText`/`diffRaw` extract via `htmlText`/`htmlRaw`, which COPY the payload string before the equality test; the copy is wasted when the strings match (the common case).
- std/html.vyrn:761-788 — keyed matching is O(old × new) with a nested `if found < 0` guard standing in for an early exit; fine at view scale, worth knowing.
- std/html.vyrn:317-325 — `isVoid` does a linear scan of the void-element list on every element render; a comptime map would do, minor.
- std/html.vyrn:114-117 — `htmlGive<T>` takes an unconsumed generic just so a `match` arm can be an expression; idiomatic here (twin of `jsonGive`), noted for completeness.

#### Byte-shaped string ops seen in this module
- `bytes(s)` byte-loop in `escapeText` — std/html.vyrn:236-252 — unit: UInt8 bytes of the String. Breaks nothing: it only substitutes ASCII entity bytes for ASCII special bytes, and an ASCII code unit can never equal a UTF-8 continuation byte, so multi-byte characters pass through untouched (comment at :220-223 states this deliberately).
- `bytes(s)` byte-loop in `escapeAttr` — std/html.vyrn:257-268 — unit: UInt8. Same reasoning; safe on non-ASCII.
- `bytes(n)` byte-loop in `nameOk` — std/html.vyrn:288-298 — unit: UInt8. Safe: non-ASCII names are REFUSED (any byte outside the ASCII set fails the check), which is the intended behavior for markup names.
- No function in this module indexes a String with `s[i]`, uses `byteLength`, or takes/returns byte POSITIONS into a String. All byte arrays here round-trip through `fromBytesOr`, and every byte scanned is compared against an ASCII constant, so there is no position arithmetic to get wrong.

### std/http

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| http | httpRoute | `fn httpRoute(pattern: String, run: Handler, derived: String) -> Route` | NONE | `httpRoute(pattern, run, derived)` | constructor of `Route` | NO SUBJECT |
| http | GET | `fn GET(r: Route) -> Route` | r | `GET(r)` | `r.GET()` | SUBJECT AS ARGUMENT |
| http | POST | `fn POST(r: Route) -> Route` | r | `POST(r)` | `r.POST()` | SUBJECT AS ARGUMENT |
| http | PUT | `fn PUT(r: Route) -> Route` | r | `PUT(r)` | `r.PUT()` | SUBJECT AS ARGUMENT |
| http | PATCH | `fn PATCH(r: Route) -> Route` | r | `PATCH(r)` | `r.PATCH()` | SUBJECT AS ARGUMENT |
| http | DELETE | `fn DELETE(r: Route) -> Route` | r | `DELETE(r)` | `r.DELETE()` | SUBJECT AS ARGUMENT |
| http | surface | `fn surface(prefix: String, run: consume Surface) -> Route` | NONE | `surface(prefix, run)` | constructor of a prefix `Route` | NO SUBJECT |
| http | sse | `fn sse(pattern: String, feed: consume Feed) -> Live` | NONE | `sse(pattern, feed)` | constructor of `Live` | NO SUBJECT |
| http | ws | `fn ws(pattern: String, feed: consume Feed) -> Socket` | NONE | `ws(pattern, feed)` | constructor of `Socket` | NO SUBJECT |
| http | event | `fn event(id: String, name: String, data: String) -> String` | NONE | `event(id, name, data)` | pure formatter over three peer arguments; no distinguished receiver | NO SUBJECT |
| http | mount | `fn mount(req: Request, groups: Array<Array<Route>>, live: Array<Live>, sockets: Array<Socket>) -> Option<Response>` | req | `mount(req, groups, live, sockets)` | `req.mount(groups, live, sockets)` | SUBJECT AS ARGUMENT |
| http | httpInput | `fn httpInput(ps: Map<String, String>, body: String, numeric: Array<String>) -> String` | ps, body (either could be the receiver of the merge) | `httpInput(ps, body, numeric)` | unresolved: `ps.mergedWith(body)` vs `body.withBindings(ps)` | AMBIGUOUS |
| http | http | `gen fn http(module: String) -> String` | module | `http(module)` | `module.http()` | SUBJECT AS ARGUMENT |
| http | cacheFor (Policy) | `fn cacheFor(self, seconds: Int64) -> Route` | self | `r.cacheFor(seconds)` | already a method | SUBJECT FIRST |
| http | etag (Policy) | `fn etag(self) -> Route` | self | `r.etag()` | already a method | SUBJECT FIRST |
| http | lastModified (Policy) | `fn lastModified(self, field: String) -> Route` | self | `r.lastModified(field)` | already a method | SUBJECT FIRST |
| http | vary (Policy) | `fn vary(self, headers: String) -> Route` | self | `r.vary(headers)` | already a method | SUBJECT FIRST |
| http | status (Policy) | `fn status(self, code: Int64) -> Route` | self | `r.status(code)` | already a method | SUBJECT FIRST |
| http | createdAt (Policy) | `fn createdAt(self, template: String) -> Route` | self | `r.createdAt(template)` | already a method | SUBJECT FIRST |
| http | notFoundWhen (Policy) | `fn notFoundWhen(self, isMissing: IsMissing) -> Route` | self | `r.notFoundWhen(isMissing)` | already a method | SUBJECT FIRST |
| http | retryAfter (Wire) | `fn retryAfter(self, ms: Int64) -> Live` | self | `l.retryAfter(ms)` | already a method | SUBJECT FIRST |
| http | resumable (Wire) | `fn resumable(self) -> Live` | self | `l.resumable()` | already a method | SUBJECT FIRST |
| http | closeCode (Frames) | `fn closeCode(self, code: Int64) -> Socket` | self | `s.closeCode(code)` | already a method | SUBJECT FIRST |
| http | subprotocol (Frames) | `fn subprotocol(self, name: String) -> Socket` | self | `s.subprotocol(name)` | already a method | SUBJECT FIRST |
| http | maxFrame (Frames) | `fn maxFrame(self, size: Int64) -> Socket` | self | `s.maxFrame(size)` | already a method | SUBJECT FIRST |

#### Counts
exports=13 (export fn x12, export gen fn x1) SUBJECT FIRST=12 SUBJECT AS ARGUMENT=7 NO SUBJECT=5 AMBIGUOUS=1
(class counts cover all 25 rows: 13 exports + 12 protocol impl methods)

#### Other smells (path:LINE relative to repo root)
- std/http.vyrn:222-240 — GET/POST/PUT/PATCH/DELETE are free prefix functions over `Route` while the seven sibling combinators are `Policy` protocol methods on the same record; the module's own comment (line 290) rejects two spellings of one combinator, yet the verb vocabulary kept the old spelling.
- std/http.vyrn:852 — `mount` takes four positional parameters, two of them parallel arrays (`live`, `sockets`) that exist only because there is no sum of `Live` and `Socket`; the module says so itself (lines 846-851).
- std/http.vyrn:181-197 — `httpCopy` restates all thirteen `Route` fields by hand; every new policy field must be added here plus in `httpRoute` (160) and `surface` (254), three sites for one record shape.
- std/http.vyrn:1498-1505 — `httpListHas` and std/http.vyrn:1748-1755 `httpContains` are the same array-contains walk written twice.
- std/http.vyrn:1032-1037 — `httpHeader` and std/http.vyrn:1507-1512 `httpAt` duplicate the Map get-or-default-to-"" pattern.
- std/http.vyrn:554 — manual CRLF/CR normalization via nested `replace(replace(..))`; three-step fold inline rather than one named step.
- std/http.vyrn:687-732 — `httpOpenSocket` calls `serveStream` (line 724) as a side effect before returning the handshake response; the response's success depends on an effect fired mid-function.

#### Byte-shaped string ops seen in this module
- `substring(pattern, 0, pattern.byteLength - 1)` — std/http.vyrn:207, bytes. Strips a trailing slash; guarded by `endsWith("/")` (an ASCII test), so the sliced byte is always `/` — safe on non-ASCII patterns, but byte-unit by construction.
- `substring(t, 2, t.byteLength)` — std/http.vyrn:1056 (`httpUnweak`), bytes. Strips the `W/` weak-validator prefix after `startsWith("W/")`; both stripped bytes are ASCII, so safe on any tag content.
- `substring(seg, 1, seg.byteLength - 1)` — std/http.vyrn:1290 (`httpPlaceholderName`), bytes. Strips `{`/`}` confirmed by `startsWith("{") && endsWith("}")`; ASCII guards, safe.
- `indexOf(path, "?")` + `substring(path, 0, i)` — std/http.vyrn:1278-1283 (`httpPathOnly`), byte-index consistent with the same `indexOf`; `?` is ASCII, safe.
- `lowerOf` byte loop — std/http.vyrn:758-768, bytes. Deliberate ASCII-only lowering of header values (documented at 756-757); non-ASCII passes through unchanged, no truncation.
- `sseOneLine` byte loop — std/http.vyrn:560-568, bytes. Filters `\r`/`\n` bytes; both are single ASCII bytes in UTF-8, so multi-byte characters survive intact.
- `httpCapFirst` first-byte uppercase — std/http.vyrn:1539-1552, bytes. Documented ASCII-only, applied to internal identifier names.

### std/i18n

Compile-time generator module. One export (`i18n(dir)`); every helper is a
module-private `gen fn`/`fn`. No protocols or impls exist in this file. The
ICU scanners deliberately run over `Array<UInt8>` (header comment at
std/i18n.vyrn:49-53), so their indices are bytes by design, not the String
defect class.

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| i18n | i18n | `gen fn i18n(dir: String) -> String` | dir (the locale directory read and compiled) | `i18n("./locales")` | `dir.i18n()` — or keep free as a generator entry point; the directory is unambiguously the subject | SUBJECT AS ARGUMENT |

#### Counts
exports=1 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=1 NO SUBJECT=0 AMBIGUOUS=0

Note: the remaining ~40 top-level `gen fn`/`fn` helpers are not exported and
are not protocol/impl methods, so per the shared rules they get no rows.
Nearly all take their primary data (`ba: Array<UInt8>`, `msg: String`,
`br: Branches`, `d: LocaleData`) as argument one already, so if the parent
later wants the private helpers censused, most would classify SUBJECT FIRST
or SUBJECT AS ARGUMENT mechanically.

#### Other smells (std/i18n.vyrn:LINE)
- 711 `pluralCategories` returns the sentinel string `"__unsupported__"` instead of an Option/Result; callers re-detect it (724-732).
- 1028-1031 `listDir` errors are swallowed into `[]`, and 1037-1040 `readFile` errors into `""` — an unreadable directory reports the misleading "no locale files under …".
- 209/227/240 error-as-empty-string: `dataError` and the `flattenObj`/`flattenValue` chain signal failure by returning `""`, which conflates an empty error message with success.
- 300 `entryIndex` spells `-1` as `0 - 1`; an `Option<Int64>` would fit the codebase's record style better.
- 976 `out = out + localeErr(…)` inside a branch with no `else` — dead-looking self-concatenation pattern repeated through the collision checker.
- Imported free functions show the SUBJECT AS ARGUMENT class in live use inside this module: `startsWith(name, "__")` (514), `contains(b.selector, ",")` (587), `joinWith(names, ",")` (535), `trim(sliceStr(…))` (422, 429), `substring(…)` (853, 1036).

#### Byte-shaped string ops seen in this module
- `substring(sel, 1, sel.byteLength)` — 852-853, byte positions. Guarded by `bytes(sel)[0] == '='`, so the cut always lands between UTF-8 sequences; nothing breaks on non-ASCII selectors, but the op is byte-shaped API on a locale-file String.
- `substring(name, 0, name.byteLength - 5)` — 1036, byte length minus ASCII suffix `.json`. Boundary stays valid for any tag content; breaks only if the extension check ever stops being ASCII.
- `lit.text.byteLength > 0` — 793, byte length used purely as an emptiness test; unit-safe.
- `upperFirst` (86-99) / `lowerFirst` (142-155) — first-BYTE case folding. Non-leading and non-ASCII bytes pass through verbatim, so no corruption, but a key starting with a non-ASCII letter is never case-mapped.
- `docComment` — 1401-1412, byte-level `\n`/`\r` replacement; ASCII-only, safe.
- Module design note: all brace/comma/apostrophe scanners thread byte indices over `Array<UInt8>` on purpose (49-53); every delimiter scanned (`{`, `}`, `,`, `#`, `'`, `.`) is ASCII and therefore cannot occur inside a multi-byte UTF-8 sequence, so the parsing itself is Unicode-safe.

### std/icons
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| icons | gen fn icons | `(collection: String, names: String) -> String` | collection (arg 1) | `icons("icons", "github rss")` | `collection.icons(names)` | SUBJECT AS ARGUMENT |
| icons | gen fn iconsAt | `(collection: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String` | collection (arg 1) | `iconsAt("icons", "github", f, l, c)` | `collection.iconsAt(names, f, l, c)` | SUBJECT AS ARGUMENT |
| icons | gen fn iconsModule | `(collectionText: String, collectionPath: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String` | collectionText (arg 1) | `iconsModule(text, path, names, "", 0, 0)` | `collectionText.iconsModule(path, names, anchorFile, line, col)` | SUBJECT AS ARGUMENT |
| icons | gen fn iconProvider | `(attrs: String, file: String, line: Int64, col: Int64, collections: String) -> String` | AMBIGUOUS: `attrs` (the tag being interpreted) vs `collections` (the prefix vocabulary) | `iconProvider(attrs, f, l, c, "ui codex")` | either `attrs.iconProvider(f, l, c, collections)` or `collections.providerFor(attrs, f, l, c)` — name the chosen reading | AMBIGUOUS |
| icons | gen fn lit | `(s: String) -> String` | s (arg 1), but the op is a pure text transform | `lit(s)` | pure computation: string to literal-source text; no distinguished receiver | NO SUBJECT |
| icons | fn iconAttrs | `(g: consume Html, size: String, label: String, class: String) -> Html` | g (arg 1), the drawn glyph | `iconAttrs(drawn, size, label, cls)` | `drawn.withAttrs(size, label, cls)` | SUBJECT AS ARGUMENT |

No protocol/impl blocks in this module; nothing else to classify there.

#### Counts
exports=6 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=4 NO SUBJECT=1 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/icons.vyrn:393,753 — `contains(s, ":")` + hand-rolled `afterColon`/`beforeColon`: three passes over the same bytes where one split-once helper would do.
- std/icons.vyrn:903-933 — `camel` derives an identifier byte-by-byte; ASCII-only letter/digit test means a non-ASCII letter acts as a separator (`café-latte` becomes `cafLatte`).
- std/icons.vyrn:928 — `bytes(core)[0]` checks the FIRST BYTE for ASCII-letterness; a name starting with a multibyte letter gets an unwanted `icon` prefix.
- std/icons.vyrn:430-439,792-801 — repeated `out = out + ...` loop concatenation builds generated source; quadratic in the number of glyphs (small n, minor).
- std/icons.vyrn:320-328 — `pick` reinvents a first-non-empty ternary chain; harmless but a candidate for a shared strings helper.
- Module consumes subject-as-argument APIs from siblings: `splitWhitespace`, `joinWith`, `fromBytesOr` (std/strings, :97), `contains` (std/strpred, :98), `includes` (std/arrays, :99) — consistent with defect class 1 owned elsewhere; noted here because call sites will change when those modules gain method forms.

#### Byte-shaped string ops seen in this module
- `bytes(name)` + `b - 32` case shift, std/icons.vyrn:904-924 — unit: byte. On non-ASCII: each UTF-8 continuation byte fails the ASCII letter/digit test, so the byte is DROPPED from the identifier and sets `upper`; a glyph named with any non-ASCII letter loses that letter and capitalizes the next segment. Silently wrong identifier.
- `bytes(core)[0]`, std/icons.vyrn:928 — unit: byte. A name whose first character is a multibyte letter reads as a non-letter first byte, so the function prepends `icon` although the name already starts with a letter.
- `beforeColon`/`afterColon`, std/icons.vyrn:936-963 — unit: byte, but the scanned delimiter `:` is ASCII, so positions cannot split a code point; no non-ASCII breakage observed.
- No byte-position slicing of user-facing String content otherwise: `viewBox` numbers ride through `jsonNumber` raw text, and markup bodies pass through unmodified.

### std/json
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| json | impl Copy for Json.`copy` | `fn copy(self) -> Json` | self (`Json` tree) | `j.copy()` | already a method | SUBJECT FIRST |
| json | impl Owned for Json.`release` | `fn release(consume self)` | self (`Json` tree) | `drop j` / release row | already a method | SUBJECT FIRST |
| json | `copyJson` | `fn copyJson(j: Json) -> Json` | `j` | `copyJson(j)` | `j.copy()` (already exists; wrapper is redundant) | SUBJECT AS ARGUMENT |
| json | `copyJsonArray` | `fn copyJsonArray(xs: Array<Json>) -> Array<Json>` | `xs` | `copyJsonArray(xs)` | `xs.copy()` (exported because `xs.copy()` cannot be written for this element type — see std/json.vyrn:106-107) | SUBJECT AS ARGUMENT |
| json | `copyJsonFields` | `fn copyJsonFields(fs: Array<JsonField>) -> Array<JsonField>` | `fs` | `copyJsonFields(fs)` | `fs.copy()` (same element-type reason) | SUBJECT AS ARGUMENT |
| json | `emit` | `fn emit(j: Json) -> String` | `j` | `emit(j)` | `j.emit()` | SUBJECT AS ARGUMENT |
| json | `emitPretty` | `fn emitPretty(j: Json, indent: Int64) -> String` | `j` | `emitPretty(j, indent)` | `j.emitPretty(indent)` | SUBJECT AS ARGUMENT |
| json | `jsonEq` | `fn jsonEq(a: Json, b: Json) -> Bool` | both `a` and `b` (two candidate receivers) | `jsonEq(a, b)` | either `a.jsonEq(b)` or a free comparator; two symmetric tree arguments, no distinguished receiver | AMBIGUOUS |

Types `Json` and `JsonField` are plain data declarations (std/json.vyrn:27, 32-38); no callable exports, so no rows.

#### Counts
exports=8 SUBJECT FIRST=2 SUBJECT AS ARGUMENT=5 NO SUBJECT=0 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/json.vyrn:102-104 — `copyJson` is a pure alias of the existing `j.copy()`; every caller could write the method form.
- std/json.vyrn:380-382 — `jsonEq` serializes both trees with `emit` and compares strings; correct per the injectivity argument, but it is an O(tree) allocation where a structural walk would allocate nothing.
- std/json.vyrn:131-140 — `hex2` rebuilds the 16-byte digit table on every call; hoistable, though comptime-pure code may fold it.
- std/json.vyrn:254-265, 268-279 — compact emitter builds output by repeated `out = out + ","` string concatenation; quadratic in worst case for large arrays/objects.
- std/json.vyrn:283-293 — documented depth ceiling: `emit`/`emitPretty` recurse about two frames per level and trap past roughly 450 nesting levels for program-built trees; reader-side `maxDepth` covers parsed input only.
- std/json.vyrn:136-139, 185-188 — `stringFromBytes` failure collapses silently to `""`; unreachable for well-formed output, but the error channel is swallowed rather than asserted.

#### Byte-shaped string ops seen in this module
- `emitString`, std/json.vyrn:145-189 — unit: bytes (`bytes(s)`, per-byte loop). Escapes only single-byte ASCII (`"` `\` C0 controls); multi-byte UTF-8 passes through verbatim via `out.push(b)`. This matches RFC-8259 (raw UTF-8 allowed) and is documented at std/json.vyrn:144 — safe by design, not a defect, PROVIDED the input `String` is valid UTF-8 (invalid bytes would fail `stringFromBytes` and collapse to `""` per the smell above).
- `numberOk`, std/json.vyrn:195-237 — unit: bytes (`ba[i]` yields `UInt8`). Operates on `JNum` raw text, which the grammar restricts to ASCII digits/punctuation — no non-ASCII path can reach it.
- `hex2`, std/json.vyrn:131-140 — unit: bytes (indexing `bytes("0123456789abcdef")`). Purely ASCII constant.

### std/jsondec

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| jsondec | kindName | fn kindName(v: Json) -> String | v | kindName(v) | v.kindName() | SUBJECT AS ARGUMENT |
| jsondec | isNull | fn isNull(v: Json) -> Bool | v | isNull(v) | v.isNull() | SUBJECT AS ARGUMENT |
| jsondec | fieldsOf | fn fieldsOf(v: Json) -> Array<JsonField> | v | fieldsOf(v) | v.fieldsOf() | SUBJECT AS ARGUMENT |
| jsondec | itemsOf | fn itemsOf(v: Json) -> Array<Json> | v | itemsOf(v) | v.itemsOf() | SUBJECT AS ARGUMENT |
| jsondec | numText | fn numText(v: Json) -> String | v | numText(v) | v.numText() | SUBJECT AS ARGUMENT |
| jsondec | hasField | fn hasField(fs: Array<JsonField>, key: String) -> Bool | fs | hasField(fs, key) | fs.hasField(key) | SUBJECT AS ARGUMENT |
| jsondec | fieldAt | fn fieldAt(fs: Array<JsonField>, key: String) -> Json | fs | fieldAt(fs, key) | fs.fieldAt(key) | SUBJECT AS ARGUMENT |
| jsondec | elemAt | fn elemAt(items: Array<Json>, i: Int64) -> Json | items | elemAt(items, i) | items.elemAt(i) (or items[i] with an Option-free sentinel) | SUBJECT AS ARGUMENT |
| jsondec | tagOf | fn tagOf(v: Json) -> String | v | tagOf(v) | v.tagOf() | SUBJECT AS ARGUMENT |
| jsondec | keyOf | fn keyOf(v: Json) -> String | v | keyOf(v) | v.keyOf() | SUBJECT AS ARGUMENT |
| jsondec | valOf | fn valOf(v: Json) -> Json | v | valOf(v) | v.valOf() | SUBJECT AS ARGUMENT |
| jsondec | pushType | fn pushType(iss: modify Array<Issue>, path: String, expected: String, found: String) -> Unit | iss | pushType(iss, path, expected, found) | iss.pushType(path, expected, found) | SUBJECT AS ARGUMENT |
| jsondec | pushMissing | fn pushMissing(iss: modify Array<Issue>, path: String, field: String) -> Unit | iss | pushMissing(iss, path, field) | iss.pushMissing(path, field) | SUBJECT AS ARGUMENT |
| jsondec | pushValidate | fn pushValidate(iss: modify Array<Issue>, path: String, message: String) -> Unit | iss | pushValidate(iss, path, message) | iss.pushValidate(path, message) | SUBJECT AS ARGUMENT |
| jsondec | fieldPath | fn fieldPath(parent: String, field: String) -> String | parent | fieldPath(parent, field) | parent.fieldPath(field) | SUBJECT AS ARGUMENT |
| jsondec | indexPath | fn indexPath(parent: String, i: Int64) -> String | parent | indexPath(parent, i) | parent.indexPath(i) | SUBJECT AS ARGUMENT |
| jsondec | readDoc | fn readDoc(src: String, iss: modify Array<Issue>) -> Array<Json> | src; iss | readDoc(src, iss) | either src.readDoc(iss) (document parsed) or iss.readDoc(src) (issue sink mutated); both defensible | AMBIGUOUS |

#### Counts
exports=17 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=16 NO SUBJECT=0 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/jsondec.vyrn:43-174 — eight tree accessors each enumerate all six `Json` kinds with an identical constant fallback (`""`/`JNull`/`false`); a shared expect-kind guard would collapse the repetition.
- std/jsondec.vyrn:103-121 — `hasField` and `fieldAt` duplicate the same linear scan; `fieldAt` could be written through `hasField`.
- std/jsondec.vyrn:285-382 — `dIntRange`, `dUIntMax`, `dFloat64`, `dFloat32` repeat `dInt64`'s kind-check/parse/push skeleton four times.
- Non-exported helpers `pushParse` (std/jsondec.vyrn:237) and `boolOf` (std/jsondec.vyrn:271) exist; they follow the same subject-first shapes but are out of census scope.

#### Byte-shaped string ops seen in this module
- None. The module only copies (`s.copy()`), concatenates (`+`, std/jsondec.vyrn:185,210,215), and compares strings whole (`==`, std/jsondec.vyrn:105,116,207). No `s[i]` indexing, no `byteLength`, no byte-position `slice`/`substring`; raw number text leaves via `numText` and is parsed inside `std/num`.

### std/jsonread
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| std/jsonread | `export fn parseJson(src: String) -> Result<Json, String>` (std/jsonread.vyrn:513) | parseJson(src) | `src`, the JSON document text | `parseJson(src)` | `src.parseJson()` | SUBJECT AS ARGUMENT |

Notes on scope: the module declares exactly one exported function. Every other `fn` in the file (`maxDepth`, `newParser`, `cur`, `ahead`, `step`, `errAt`, `skipWs`, `isHex`, `hexVal`, `pushUtf8`, `readHex4`, `parseString`, `parseNumber`, `parseKeyword`, `parseValue`, `parseArray`, `newKeySet`, `ksFind`, `ksPlace`, `ksAdd`, `parseObject`, `parseOr`, `parseErr`, `nest`) is module-private, so per the shared rules it gets no row. There are no protocol or impl declarations in the file. The private parser helpers already follow subject-first shape internally (`p.cur()`, `p.step(p)` style with explicit cursor passing), so no hidden SUBJECT AS ARGUMENT candidates exist among them.

#### Counts
exports=1 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=1 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/jsonread.vyrn:50 `maxDepth()` is a function returning the literal 128; a named constant would say the same with less ceremony (the doc comment stays either way).
- std/jsonread.vyrn:77-89 `step` advances `col` once per BYTE, so `errAt` (line 92) reports byte-offset columns; on any line holding non-ASCII before the error, the `col M:` in every diagnostic undercounts what users see in an editor.
- std/jsonread.vyrn:449-454 `ksAdd` re-inlines the probe-and-place loop instead of calling its own `ksPlace` (line 418) on the non-rehash path; two copies of one invariant.
- std/jsonread.vyrn:306 `val.copy()` on literals like `JBool(true)` suggests defensive copies the tree may not need; harmless, just noise.
- std/jsonread.vyrn:530-544 `parseOr`/`parseErr` are test helpers living in production module body; fine if Vyrn lacks a test-support idiom, else they belong under the tests.

#### Byte-shaped string ops seen in this module
- `bytes(s)` in `newParser` (std/jsonread.vyrn:55): converts the whole document String to `Array<UInt8>`. Deliberate and correct here — the parser is specified over UTF-8 bytes (header, line 17), and raw non-ASCII bytes pass through untouched into strings validated by `stringFromBytes`.
- Byte-position cursor `pos` over `Parser.src` throughout (`cur`, `ahead`, `step`): unit = bytes. Correct for parsing, but it makes the `col M` in error messages a byte count (see smell above); breaks nothing about the parse itself.
- `errAt` (std/jsonread.vyrn:92): emits `col` derived from byte stepping; on non-ASCII input the reported column is wrong for humans, though `line` stays right.

### std/math
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| math | min | fn min(a: Int64, b: Int64) -> Int64 | AMBIGUOUS: a and b are symmetric peers | min(a, b) | no distinguished receiver; pure pairwise computation | AMBIGUOUS |
| math | max | fn max(a: Int64, b: Int64) -> Int64 | AMBIGUOUS: a and b are symmetric peers | max(a, b) | no distinguished receiver; pure pairwise computation | AMBIGUOUS |
| math | abs | fn abs(x: Int64) -> Int64 | x | abs(x) | x.abs() | SUBJECT AS ARGUMENT |
| math | clamp | fn clamp(x: Int64, lo: Int64, hi: Int64) -> Int64 | x (lo/hi are bounds, not subjects) | clamp(x, lo, hi) | x.clamp(lo, hi) | SUBJECT AS ARGUMENT |
| math | pi | fn pi() -> Float64 | NONE | pi() | pure constant; no receiver | NO SUBJECT |
| math | floorF | fn floorF(x: Float64) -> Float64 | x | floorF(x) | x.floorF() | SUBJECT AS ARGUMENT |
| math | sin | fn sin(x: Float64) -> Float64 | x | sin(x) | x.sin() | SUBJECT AS ARGUMENT |
| math | cos | fn cos(x: Float64) -> Float64 | x | cos(x) | x.cos() | SUBJECT AS ARGUMENT |

#### Counts
exports=8 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=6 NO SUBJECT=1 AMBIGUOUS=2

#### Other smells (path:LINE relative to repo root)
- std/math.vyrn:73 and std/math.vyrn:92 — the halfPi literal 1.5707963267948966 appears twice; cos re-declares it locally instead of sharing a named constant with sin (pi exists as an export, its quarter does not).
- std/math.vyrn:72-73 — twoPi and halfPi are rebuilt as locals on every sin call; hoisting them to module constants would match how pi is already exported.
- std/math.vyrn:35-37 — clamp silently assumes lo <= hi; no doc or assert states the precondition (max(lo, min(x, hi)) returns hi when inverted).
- No Float64 siblings for the Int64 trio: min/max/clamp/abs exist only for Int64; floorF/sin/cos only for Float64. Noted as coverage asymmetry, not a defect.

#### Byte-shaped string ops seen in this module
- none — the module touches only Int64/Float64; no String parameters, byte lengths, or byte indexing anywhere.

### std/num
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| num | no | fn maxDigits() -> Int64 | NONE | maxDigits() | pure computation: named constant, no receiver | NO SUBJECT |
| num | no | fn tidy(d: Array<Int64>, dp: Int64, trunc: Bool) -> Dec | NONE | tidy(d, dp, trunc) | constructor: assembles a Dec record from parts, no distinguished receiver | NO SUBJECT |
| num | no | fn halveBy(x: Dec, m: Int64) -> Dec | x | halveBy(x, m) | x.halveBy(m) | SUBJECT AS ARGUMENT |
| num | no | fn twiceBy(x: Dec, m: Int64) -> Dec | x | twiceBy(x, m) | x.twiceBy(m) | SUBJECT AS ARGUMENT |
| num | no | fn scan(s: String) -> Scanned | s | scan(s) | s.scanNumber() | SUBJECT AS ARGUMENT |
| num | no | fn ldexp(neg: Bool, mIn: Int64, exIn: Int64) -> Float64 | NONE | ldexp(neg, m, ex) | pure computation: three scalar parameters, none is a receiver | NO SUBJECT |
| num | no | fn toFloat(sc: Scanned, mantBits: Int64, minExp: Int64) -> Float64 | sc | toFloat(sc, mantBits, minExp) | sc.toFloat(mantBits, minExp) | SUBJECT AS ARGUMENT |
| num | yes | fn parseFloat64(s: String) -> Option<Float64> | s | parseFloat64(s) | s.parseFloat64() | SUBJECT AS ARGUMENT |
| num | yes | fn parseFloat32(s: String) -> Option<Float32> | s | parseFloat32(s) | s.parseFloat32() | SUBJECT AS ARGUMENT |
| num | yes | fn parseInt64(s: String) -> Option<Int64> | s | parseInt64(s) | s.parseInt64() | SUBJECT AS ARGUMENT |
| num | yes | fn parseUInt64(s: String) -> Option<UInt64> | s | parseUInt64(s) | s.parseUInt64() | SUBJECT AS ARGUMENT |
| num | yes | fn f64Str(x: Float64) -> String | x | f64Str(x) | x.toFixed6() (or x.f64Str()) | SUBJECT AS ARGUMENT |
| num | no | fn asciiStr(out: Array<UInt8>) -> String | out | asciiStr(out) | out.asciiString() (same shape as std/codecs `ascii`) | SUBJECT AS ARGUMENT |

No `export gen fn`, protocol, or impl appears in this module.

#### Counts
exports=5 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=10 NO SUBJECT=3 AMBIGUOUS=0

Counts cover all 13 classified functions (5 exported, 8 private helpers); all 5 exports are SUBJECT AS ARGUMENT. Every text-parsing entry point takes its String subject as argument one — the exact defect class A8 names.

#### Other smells (path:LINE relative to repo root)
- std/num.vyrn:71-76 — tidy hand-copies the kept digits with a push loop; toFloat at std/num.vyrn:305 already uses `sc.d.copy()`, so the loop can be `.copy()` plus a length bound.
- std/num.vyrn:116-121 — halveBy repeats the same manual strip-and-copy loop as tidy.
- std/num.vyrn:145-150 — twiceBy repeats the manual reversal copy loop; a reverse/copy primitive would remove it.
- std/num.vyrn:701-706 — asciiStr re-implements std/codecs `ascii` inline; comment at std/num.vyrn:700 says this is deliberate (module imports nothing). Kept, but it is a second copy of three lines.
- std/num.vyrn:190,223,438,472,682 — repeated `Int64(b[i]) - 48` digit decode; harmless, but a `digitValue` helper would state intent once.

#### Byte-shaped string ops seen in this module
- `bytes(s)` + `b[i]` indexing — std/num.vyrn:157-231 (`scan`), unit: byte. Grammar accepts only ASCII sign/digit/dot/e bytes, so every non-ASCII input is refused, not misread; no silent wrong answer.
- `bytes(s)` + `b[i]` indexing — std/num.vyrn:415-455 (`parseInt64`), unit: byte. Same: only `'0'..'9'` pass, so non-ASCII yields None correctly.
- `bytes(s)` + `b[i]` indexing — std/num.vyrn:460-484 (`parseUInt64`), unit: byte. Same as parseInt64; correct refusals.
- No function here returns byte positions or byte lengths of a String, and none slices a String by byte offsets.

Live byte defects inside the module: none. All three byte-walkers reject any byte outside the ASCII digit/sign/exponent set before using it, so multi-byte UTF-8 sequences cannot be consumed as if they were digits.

### std/openapi
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| openapi | openapi | `gen fn openapi(contract: String) -> String` | `contract` (module specifier) | `openapi("./contract")` | `contract.openapi()` — subject-first form exists, though as a comptime generator entry point a free form is also defensible | SUBJECT AS ARGUMENT |

No `protocol`/`impl` methods exist in this module. All other `fn`s (oaBaseName, oaEscBody, oaSorted, oaIsNamedType, oaImportBlock, oaIssuesSchema, oaResponseSchema, oaRequestSchema, oaPathValue, oaTypeNames) are private helpers, not exports; per the shared rules they take no table row. They are listed under Other smells where relevant.

#### Counts
exports=1 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=1 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/openapi.vyrn:56-84 `oaBaseName` reimplements last-segment split and suffix strip as manual byte loops; char-safe `std/text` helpers (if any split/suffix helpers land there) or `std/strpred` predicates would replace ~28 lines.
- std/openapi.vyrn:89-104 `oaEscBody` hand-rolls Vyrn-literal escaping byte by byte; a second escaper coexisting with `std/json`'s writer invites drift between the two escaping rules.
- std/openapi.vyrn:109-127 `oaSorted` hand-writes insertion sort; if `std/arrays` gains a sorted copy, this dies.
- std/openapi.vyrn:130-137 `oaIsNamedType` is a linear membership scan over `iface.types`; same shape as the `includes` import at line 48 but over a different field.
- std/openapi.vyrn:210-221 `oaPathValue` recomputes `oaTypeNames(iface)` per procedure via the `oaRequestSchema(f.params[0].spelling, oaTypeNames(iface))` call at line 214 — the sorted-name array is rebuilt once per function in the contract.
- std/openapi.vyrn:236-303 the generator body builds the emitted source with repeated `out = out + …` concatenation (quadratic in output size); the RFC-0059 change removed concat only for the JSON document, not for the generated source text.
- std/openapi.vyrn:266-275 `oaWithId` match arms rebuild each variant identically (`JBool(b) => JBool(b)` etc.) — an ownership workaround (RFC-0089 rule 3) that a borrow-friendly `read` parameter would collapse to a single arm.
- std/openapi.vyrn:251 `Err(e) => JObj([])` silently swallows parse failure of a baked constant with an empty object; a baked-valid constant failing is a bug that would surface as a mysterious empty schema.

#### Byte-shaped string ops seen in this module
- `bytes(spec)` + byte indexing `ba[i] == '/'` + byte-range copy, std/openapi.vyrn:57-72 — unit: bytes — splits a module specifier at `/`. Safe on non-ASCII: 0x2F cannot occur inside a UTF-8 multi-byte sequence, and `fromBytesOr` falls back to the original string.
- `.endsWith(".vyrn")` then `nb.length - 5` byte trim, std/openapi.vyrn:73-81 — unit: bytes — strips an ASCII suffix from the end; suffix-boundary arithmetic is correct regardless of non-ASCII content before it.
- `oaEscBody` byte-wise copy/escape, std/openapi.vyrn:90-103 — unit: bytes — copies all bytes verbatim except ASCII `\` and `"`; pass-through preserves UTF-8 sequences intact.

No byte op in this module corrupts non-ASCII content: every byte scan keys on ASCII delimiters (which cannot appear inside multi-byte sequences) or copies bytes verbatim, and every rebuild routes through `fromBytesOr` with the original string as fallback. The byte loops are verbose, not wrong.

#### Live byte defects
None found. See analysis above: the module's byte-shaped helpers are ASCII-delimited scans and verbatim copies with infallible fallbacks, so they survive non-ASCII input.

### std/random

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| random | randomSeed | `randomSeed() -> Int64` | NONE | `randomSeed()` | host-effect query, no distinguished receiver | NO SUBJECT |
| random | seededRng | `seededRng(seed: Int64) -> Rng` | NONE | `seededRng(seed)` | constructor for the `Rng` value type | NO SUBJECT |
| random | nextInt | `nextInt(rng: Rng) -> Draw` | rng (argument one) | `nextInt(rng)` | `rng.nextInt()` | SUBJECT AS ARGUMENT |
| random | nextInRange | `nextInRange(rng: Rng, lo: Int64, hi: Int64) -> Draw` | rng (argument one) | `nextInRange(rng, lo, hi)` | `rng.nextInRange(lo, hi)` | SUBJECT AS ARGUMENT |

Notes:
- `hostRandomSeed` (`std/random.vyrn:35`) is an `extern fn` but NOT exported (module-private); excluded from counts per rules. If it were counted, it would be NO SUBJECT (raw extern).
- `int64Min` (`std/random.vyrn:97`) is a private helper, not an export; excluded from counts. It would be NO SUBJECT (pure computation of a constant).
- For `nextInRange`, `lo`/`hi` bound the output but do not own the operation: the function's contract is "advance this generator," proven by the returned `.rng`. Subject is unambiguously `rng`.

#### Counts
exports=4 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=2 NO SUBJECT=2 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/random.vyrn:73-74 modulo reduction has slight low-bias for very wide ranges; documented and accepted for v1 — fine.
- std/random.vyrn:56,81 both draw functions take the generator as argument one and return it threaded inside `Draw`; consistent candidate for method form on `Rng` (the only module whose whole API hangs off one value type).
- No protocol/impl methods exist in this module.

#### Byte-shaped string ops seen in this module
- None. The module touches no `String` values; all arithmetic is on `Int64`/`UInt64`.

### std/rpc
#### Classification
| module | export | signature | subject | call form today | should be | class |
| --- | --- | --- | --- | --- | --- | --- |
| rpc | contract Api | `contract Api { fn *(..) -> R }` | NONE | `Api` (contract declaration) | contract declaration; a shape rule over exports, no receiver | NO SUBJECT |
| rpc | validateContract | `export gen fn validateContract(iface: ModuleInterface) -> String` | iface | `validateContract(iface)` | `iface.validateContract()` | SUBJECT AS ARGUMENT |
| rpc | rpcServer | `export gen fn rpcServer(contract: String) -> String` | contract (the module the specifier names) | `rpcServer(contract)` | `contract.rpcServer()` | SUBJECT AS ARGUMENT |
| rpc | rpcClient | `export gen fn rpcClient(contract: String) -> String` | contract (the module the specifier names) | `rpcClient(contract)` | `contract.rpcClient()` | SUBJECT AS ARGUMENT |
| rpc | rpcInProcess | `export gen fn rpcInProcess(contract: String) -> String` | contract (the module the specifier names) | `rpcInProcess(contract)` | `contract.rpcInProcess()` | SUBJECT AS ARGUMENT |
| rpc | rpc | `export gen fn rpc(dir: String) -> String` | dir (the api directory mounted) | `rpc(dir)` | `dir.rpc()` | SUBJECT AS ARGUMENT |
| rpc | client | `export gen fn client(dir: String) -> String` | dir (the api directory mirrored) | `client(dir)` | `dir.client()` | SUBJECT AS ARGUMENT |
| rpc | clientInProcess | `export gen fn clientInProcess(dir: String) -> String` | dir (the api directory mirrored) | `clientInProcess(dir)` | `dir.clientInProcess()` | SUBJECT AS ARGUMENT |

No protocols or impls exist in this module; every export above is a top-level declaration. All seven generator/validator exports take exactly one argument and that argument is unambiguously the thing the operation is about, so none is AMBIGUOUS. The emitted generated code (string-built inside these generators) calls its own procedures subject-first (`sq.push` style, e.g. `out.push(...)` at :86, `.remove(key)` at :486), which is already the target form.

#### Counts
exports=8 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=7 NO SUBJECT=1 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/rpc.vyrn:643-652 — `rpcStem` strips the `.vyrn` suffix by hand at byte level with a magic `5`; `endsWith` is already imported from std/strpred and a char-safe suffix-strip helper would remove both the magic number and the byte slicing.
- std/rpc.vyrn:647 — `while i < b.length - 5` has no guard for names shorter than 5 bytes; the bound goes negative and correctness depends on signed comparison semantics rather than on an explicit check.
- std/rpc.vyrn:95-106 — `joinList(parts, sep)` is a subject-as-argument-one free function hand-rolling a join; a private copy of a strings-layer concern (not exported, so outside the export table).
- std/rpc.vyrn:257-264 — `rpcListContains` re-implements a membership scan over `Array<String>`; a library-level contains/membership predicate would delete it (used again indirectly via `rpcHasPair` at :1088-1097).
- std/rpc.vyrn:79-92 — `capFirst` uppercases through `bytes(s)` and `fromBytesOr` instead of a char/string helper; safe today only because procedure names are ASCII identifiers, but it is byte-shaped code on a `String`.
- std/rpc.vyrn:83-90 — `capFirst` copies the whole byte array although only index 0 can ever change; the loop could stop after the first byte.

#### Byte-shaped string ops seen in this module
- `capFirst` — std/rpc.vyrn:79-92 — unit: bytes — `bytes(s)` gives a byte array; the first byte is uppercased when in `a..z` and the array is rebuilt with `fromBytesOr`. On a String starting with a multi-byte scalar the lead byte falls outside `a..z` and passes through unchanged, so output stays valid UTF-8; the risk is silent misbehavior only if callers expect case folding of non-ASCII names. Inputs here are procedure/module identifier names, ASCII in practice.
- `rpcStem` — std/rpc.vyrn:643-652 — unit: bytes — copies `b.length - 5` bytes of a directory-entry name to drop the `.vyrn` suffix. Positions and length are byte-based; correct on non-ASCII names only because the stripped suffix is fixed-width ASCII, and wrong for any future variable-length suffix. A char-safe `stripSuffix`-shaped sibling removes the hazard.

Neither site currently corrupts non-ASCII data (both transforms touch only fixed ASCII prefix/suffix regions), so no live byte defect is claimed.

### std/scan
RFC-0054 cursor over foreign text (CSS, ICU messages, HTML templates, SDL). All
offsets are byte offsets by design (header, std/scan.vyrn:7); the module walks
and slices `src` purely by byte index.

#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| scan | newScanner | newScanner(src: String) -> Scanner | NONE | newScanner(src) | constructor: builds a fresh record, no distinguished receiver | NO SUBJECT |
| scan | cssScanner | cssScanner(src: String) -> Scanner | NONE | cssScanner(src) | constructor: builds a fresh record, no distinguished receiver | NO SUBJECT |
| scan | scanner | scanner(src: String, lineComment: String, blockOpen: String, blockClose: String, quote1: Int64, quote2: Int64, escape: Int64) -> Scanner | NONE | scanner(src, lc, bo, bc, q1, q2, esc) | constructor: fully parameterized builder, no distinguished receiver | NO SUBJECT |
| scan | atEnd | atEnd(sc: Scanner) -> Bool | sc | atEnd(sc) | sc.atEnd() | SUBJECT AS ARGUMENT |
| scan | peek | peek(sc: Scanner) -> Int64 | sc | peek(sc) | sc.peek() | SUBJECT AS ARGUMENT |
| scan | peekAt | peekAt(sc: Scanner, n: Int64) -> Int64 | sc | peekAt(sc, n) | sc.peekAt(n) | SUBJECT AS ARGUMENT |
| scan | looksAt | looksAt(sc: Scanner, s: String) -> Bool | sc | looksAt(sc, s) | sc.looksAt(s) | SUBJECT AS ARGUMENT |
| scan | advance | advance(sc: modify Scanner) -> Unit | sc | advance(sc) | sc.advance() | SUBJECT AS ARGUMENT |
| scan | skipWs | skipWs(sc: modify Scanner) -> Unit | sc | skipWs(sc) | sc.skipWs() | SUBJECT AS ARGUMENT |
| scan | ident | ident(sc: modify Scanner) -> String | sc | ident(sc) | sc.ident() | SUBJECT AS ARGUMENT |
| scan | quotedString | quotedString(sc: modify Scanner) -> Option<String> | sc | quotedString(sc) | sc.quotedString() | SUBJECT AS ARGUMENT |
| scan | until | until(sc: modify Scanner, stop: Int64) -> String | sc | until(sc, stop) | sc.until(stop) | SUBJECT AS ARGUMENT |
| scan | untilStr | untilStr(sc: modify Scanner, stop: String) -> String | sc | untilStr(sc, stop) | sc.untilStr(stop) | SUBJECT AS ARGUMENT |
| scan | balanced | balanced(sc: modify Scanner, open: Int64, close: Int64) -> String | sc | balanced(sc, open, close) | sc.balanced(open, close) | SUBJECT AS ARGUMENT |

Non-exported helpers (isSpaceByte :135, isWordByte :139, walked :214,
skipBlockComment :162, isQuoteByte :227, skipUnit :263) are outside the census
scope: not `export fn`, not protocol/impl methods.

#### Counts
exports=14 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=11 NO SUBJECT=3 AMBIGUOUS=0

#### Other smells (std/scan.vyrn:LINE)
- std/scan.vyrn:40-53 and std/scan.vyrn:57-70 and std/scan.vyrn:83-94: three near-identical record literals; `newScanner` and `cssScanner` could build on `scanner`, and all three copy `src` separately.
- std/scan.vyrn:193-198, std/scan.vyrn:271-275, std/scan.vyrn:325-328: the "consume through end-of-line comment" loop is written out three times (`skipWs`, `skipUnit`, `balanced` inline branch); one shared helper would do.
- std/scan.vyrn:263-278: `skipUnit` re-tests quote/block-comment/line-comment in the same order as `balanced`'s inline chain (std/scan.vyrn:321-328); `balanced` could call `skipUnit` plus its own bracket check instead of duplicating the policy.
- std/scan.vyrn:135-141: `isSpaceByte`/`isWordByte` take `Int64` but are only ever fed `peek(sc)` results; fine, but they are byte predicates named without a `Byte` marker while the module's own docs stress the byte/char split.
- std/scan.vyrn:354, std/scan.vyrn:381, std/scan.vyrn:393: tests compare raw magic byte constants (44, 59, 120, 98, 123, 125) next to character comments; a small `'x' as byte` spelling would keep them readable.
- Every `slice` site (8 of them) relies on the `walked()` invariant argument at std/scan.vyrn:205-216; sound today, but any future edit that lets a loop stop inside a multi-byte character silently breaks all eight at once.

#### Byte-shaped string ops seen in this module
This module is byte-shaped end to end by design (std/scan.vyrn:7). Ops:
- `sc.src.byteLength` as end-of-input bound — std/scan.vyrn:99, std/scan.vyrn:104, std/scan.vyrn:113, std/scan.vyrn:122, std/scan.vyrn:145 — unit: bytes — breaks nothing internally, but every position handed OUT to callers is a byte offset, so a caller that treats it as a character offset corrupts non-ASCII text downstream.
- `sc.src[sc.pos]` / `sc.src[i]` single-byte indexing yielding `UInt8` (cast to `Int64`) — std/scan.vyrn:107, std/scan.vyrn:116, std/scan.vyrn:127, std/scan.vyrn:148 — unit: bytes — a multi-byte character surfaces as 2-4 unrelated integer values; callers matching against character codes misread non-ASCII.
- `advance` moves one byte per step — std/scan.vyrn:144-156 — unit: bytes — `col` counts each UTF-8 continuation byte as a column, so `line`/`col` are wrong for any line containing non-ASCII.
- `looksAt` compares byte-by-byte — std/scan.vyrn:120-133 — unit: bytes — correct for byte-aligned markers, but only because the walk never stops inside a character (see `walked` invariant).
- `slice(sc.src, start, pos)` byte-range slicing — std/scan.vyrn:224, std/scan.vyrn:249, std/scan.vyrn:257, std/scan.vyrn:288, std/scan.vyrn:292, std/scan.vyrn:301, std/scan.vyrn:305, std/scan.vyrn:335, std/scan.vyrn:344 — unit: bytes — safe here ONLY under the invariant that both endpoints came from byte-walking this same string; violated if any endpoint is ever derived from a character count.
- `quotedString` escape handling advances exactly one byte past the escape byte — std/scan.vyrn:243-247 — unit: bytes — an escaped non-ASCII character (backslash followed by a multi-byte character) is split mid-sequence; the byte-slice keeps the text intact today, but the "escaped byte" model is wrong for non-ASCII escapes.

### std/slots

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| slots | newSlots | `newSlots<T>() -> Slots<T>` | NONE | `newSlots()` | constructor; no distinguished receiver | NO SUBJECT |
| slots | insert | `insert<T>(s: modify Slots<T>, v: consume T) -> Handle<T>` | s (arg 1) | `insert(s, v)` | `s.insert(v)` | SUBJECT AS ARGUMENT |
| slots | alive | `alive<T>(s: Slots<T>, h: Handle<T>) -> Bool` | s (arg 1) | `alive(s, h)` | `s.alive(h)` | SUBJECT AS ARGUMENT |
| slots | get | `get<T>(s: Slots<T>, h: Handle<T>) -> Option<T>` | s (arg 1) | `get(s, h)` | `s.get(h)` | SUBJECT AS ARGUMENT |
| slots | remove | `remove<T>(s: modify Slots<T>, h: Handle<T>) -> Bool` | s (arg 1) | `remove(s, h)` | `s.remove(h)` | SUBJECT AS ARGUMENT |
| slots | count | `count<T>(s: Slots<T>) -> Int64` | s (arg 1) | `count(s)` | `s.count()` | SUBJECT AS ARGUMENT |
| slots | capacity | `capacity<T>(s: Slots<T>) -> Int64` | s (arg 1) | `capacity(s)` | `s.capacity()` | SUBJECT AS ARGUMENT |
| slots | handles | `handles<T>(s: Slots<T>) -> Array<Handle<T>>` | s (arg 1) | `handles(s)` | `s.handles()` | SUBJECT AS ARGUMENT |
| slots | Index.at (place) | `place at(read self, h: Handle<T>) -> T` | self (receiver) | `s[h]` | already a method form | SUBJECT FIRST |
| slots | Index.atSet (place) | `place atSet(modify self, h: Handle<T>) -> T` | self (receiver) | `s[h] = v` | already a method form | SUBJECT FIRST |
| slots | Iterate.size | `fn size(self) -> Int64` | self (receiver) | protocol method of `for x in s` | already a method form | SUBJECT FIRST |
| slots | Iterate.nth (place) | `place nth(read self, i: Int64) -> T` | self (receiver) | protocol method of `for x in s` | already a method form | SUBJECT FIRST |
| slots | Owned.release | `fn release(consume self)` | self (receiver) | implicit on drop | already a method form | SUBJECT FIRST |
| slots | Copy.copy | `fn copy(self) -> Slots<T>` | self (receiver) | `s.copy()` | already a method form | SUBJECT FIRST |

Non-exported `takeIdentity()` (std/slots.vyrn:77) is module-private plumbing; not classified per rules. Exported types `Handle<T>` (:56) and `Slots<T>` (:64) are data, not calls.

#### Counts
classified=14 (8 export fns + 6 protocol/impl methods)
exports=14 SUBJECT FIRST=6 SUBJECT AS ARGUMENT=7 NO SUBJECT=1 AMBIGUOUS=0

Note: `newSlots` takes no arguments at all, so its subject is NONE rather than argument one; it stays free because it is a constructor.

#### Other smells (path:LINE relative to repo root)
- std/slots.vyrn:75 — module-level mutable identity counter (`issued`); a single program-wide sequence is the stated design, but it makes container identity non-reproducible under snapshot/replay and assumes single-threaded issue.
- std/slots.vyrn:118, 186, 194 — the liveness predicate (`h.owner != s.owner || h.slot < 0 || h.slot >= s.gens.length || gens[slot] != gen`) appears three times verbatim beside `alive`; `at`/`atSet` could call `alive` instead of restating it.
- std/slots.vyrn:153 — dead-slot sentinel written as `0 - 1` instead of a `-1` literal; harmless but noisy, and the sentinel is never documented against `denseAt`.
- std/slots.vyrn:130 vs :184 — two spellings of the same read (`get` copies, `s[h]` yields a place) are both correct per the module doc; listed only to confirm the pair is intentional, not drift.

#### Byte-shaped string ops seen in this module
- None. The module handles `T` generically and touches only `Int64` slots/generations and array lengths; it takes or returns no byte positions, byte lengths, or byte values on `String`.

### std/storage
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| storage | writeAtomic | `fn writeAtomic(path: String, content: String) -> Result<Bool, String>` | AMBIGUOUS: `path` (the file being published) vs `content` (the bytes being stored) | `writeAtomic(path, content)` | either `path.writeAtomic(content)` or `content.writeAtomicTo(path)` — name both candidates | AMBIGUOUS |

Non-export helpers (not counted, listed for completeness): `writeAtomicTemp(tmp, path, content)` at std/storage.vyrn:58 and `tempName(path, seed)` at std/storage.vyrn:66 are module-private free functions; `path` is arguably argument-subject in both, but they are not exports and there are no protocol/impl methods in this module.

#### Counts
exports=1 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=0 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/storage.vyrn:53 vs std/storage.vyrn:66 — the `<path>.tmp.<seed>` spelling exists twice: `writeAtomic` formats it inline while `tempName` exists as the tested helper and is never called by production code. `writeAtomic` should call `tempName`.
- std/storage.vyrn:52 and std/storage.vyrn:67 — `UInt64(randomSeed())` / `UInt64(seed)` bit-pattern conversion duplicated; `tempName` taking `Int64` then widening forces the odd `0 - 7` spelling in the test (std/storage.vyrn:77).
- std/storage.vyrn:59-62 — `match writeFile(...) { Ok(done) => ..., Err(why) => Err(why) }` re-wraps the error identically; an `andThen`-style combinator (or ignoring the unused `done` binding) would drop the boilerplate.
- std/storage.vyrn:51 — `Result<Bool, String>` carries a `Bool` payload no caller inspects (both tests only check Ok/Err); the success type could be unit-like unless the Bool means something documented elsewhere.
- std/storage.vyrn:92-93 — the rename-failure test deliberately leaks `wa-rename-failure-leak.tmp` into the working directory; acknowledged in comments and gitignored, but still a side effect of running the suite.

#### Byte-shaped string ops seen in this module
(none — the module handles whole `String` values for paths and contents; no byte positions, lengths, or indexing on strings)

### std/stream

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| stream | cursorGet | fn cursorGet(c: Cursor) -> Int64 | c (the cursor to read) | cursorGet(c) | c.get() | SUBJECT AS ARGUMENT |
| stream | cursorSet | fn cursorSet(c: Cursor, v: Int64) | c (the cursor to write) | cursorSet(c, v) | c.set(v) | SUBJECT AS ARGUMENT |
| stream | unfold | fn unfold<T>(seed: Int64, step: fn(Cursor) -> Option<T>) -> Stream<T> | NONE | unfold(seed, step) | constructor: builds a Stream that does not exist yet; no distinguished receiver | NO SUBJECT |
| stream | map | fn map<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U> | s (consumed source stream) | map(unfold(..), f) | s.map(f) | SUBJECT AS ARGUMENT |
| stream | filter | fn filter<T>(s: Stream<T>, pred: fn(T) -> Bool) -> Stream<T> | s (consumed source stream) | filter(unfold(..), p) | s.filter(p) | SUBJECT AS ARGUMENT |
| stream | take | fn take<T>(s: Stream<T>, n: Int64) -> Stream<T> | s (source stream being bounded) | take(unfold(..), n) | s.take(n) | SUBJECT AS ARGUMENT |
| stream | merge | fn merge<T>(a: Stream<T>, b: Stream<T>) -> Stream<T> | a or b (both consumed symmetrically) | merge(a, b) | no principled receiver: turn-taking is symmetric between the two inputs; if a primary must be named, a.merge(b) | AMBIGUOUS |

Exported type `Cursor` (std/stream.vyrn:37) is a data declaration, not a callable; it carries no row. Internal helpers `handleOf`, `newCursor`, `takeCursor`, `srcOf` are non-exported and carry no row either.

#### Counts
exports=7 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=5 NO SUBJECT=1 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/stream.vyrn:45 — module-level `let mut cells: Slots<CursorCell>` is global mutable slab state; every cursor operation reads/writes this shared container with no scoping per stream.
- std/stream.vyrn:66-73 — `cursorGet`/`cursorSet` expose raw cursor read/write publicly; they exist mainly for step functions, but nothing stops a caller from mutating a foreign stream's resume token.
- std/stream.vyrn:258-278 — `merge` drains both inputs eagerly into three `Array`s, contradicting the module's lazy-wrapper design; documented hang risk on an endless side (std/stream.vyrn:251-257) rather than fixed.
- std/stream.vyrn:102-117 — `unfold` registers its step through a closure adapter (`run`) capturing `step`; correct, but the seed/cursor protocol is entirely by-convention across `unfold`, `map`, `filter`, `take` (three near-identical copies, deliberately kept — see std/stream.vyrn:132-159).
- std/stream.vyrn:108,162,188,223 — the four combinators repeat the same closing-release boilerplate (`takeCursor` + `close`) four times; a shared form was measured and dropped for speed, so the duplication is intentional but load-bearing.

#### Byte-shaped string ops seen in this module
None. This module handles `Stream<T>`, `Int64`, `Bool`, and `Array<T>`; no String byte positions, lengths, or values appear.

### std/strpred

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| strpred | byteLengthV | `(s: String) -> Int64` | s | `byteLengthV(s)` | `s.byteLength` (field read already exists; the fn is a wrapper over `bytes(s).length`) | SUBJECT AS ARGUMENT |
| strpred | startsWith | `(s: String, needle: String) -> Bool` | s | `startsWith(s, needle)` | `s.startsWith(needle)` | SUBJECT AS ARGUMENT |
| strpred | endsWith | `(s: String, needle: String) -> Bool` | s | `endsWith(s, needle)` | `s.endsWith(needle)` | SUBJECT AS ARGUMENT |
| strpred | skipTable | `(needle: String, haystackBytes: Int64) -> Array<Int64>` | needle (haystackBytes is a size hint, not a receiver) | `skipTable(needle, n)` | `needle.skipTable()` — see smells; the parameter is unused | SUBJECT AS ARGUMENT |
| strpred | findPlain | `(s: String, needle: String, from: Int64) -> Int64` | s | `findPlain(s, needle, from)` | `s.findPlain(needle, from)` | SUBJECT AS ARGUMENT |
| strpred | worthPreparing | `(needle: String, haystackBytes: Int64) -> Bool` | needle vs haystackBytes — the predicate decides between them jointly | `worthPreparing(needle, n)` | AMBIGUOUS between `needle.worthPreparing(haystackBytes)` and a free decision helper over the pair | AMBIGUOUS |
| strpred | findSkipping | `(s: String, needle: String, from: Int64, skip: Array<Int64>) -> Int64` | s | `findSkipping(s, needle, from, skip)` | `s.findSkipping(needle, from, skip)` | SUBJECT AS ARGUMENT |
| strpred | contains | `(s: String, needle: String) -> Bool` | s | `contains(s, needle)` | `s.contains(needle)` | SUBJECT AS ARGUMENT |
| strpred | slice | `(s: String, start: Int64, end: Int64) -> Result<String, SliceError>` | s | `slice(s, start, end)` | `s.slice(start, end)` — start/end stay byte offsets with boundary check | SUBJECT AS ARGUMENT |

Note: `export type SliceError` (std/strpred.vyrn:239) is a type declaration, not an `fn`/gen/protocol member, so it has no table row and is not in the counts.

#### Counts
exports=9 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=8 NO SUBJECT=0 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/strpred.vyrn:123 — `skipTable` declares `haystackBytes: Int64` but the body never reads it; dead parameter (doc at :119-122 says emptiness is decided before the call, so the parameter carries nothing).
- std/strpred.vyrn:64 vs :124,:151,:290 — two spellings for byte length in one module: `byteLengthV(s)` in the early functions, the `s.byteLength` field everywhere later (`skipTable`, `findPlain`, `findSkipping`, `slice`). The header (:52-58) records why `slice` must keep the field form; the predicates were left on the allocating wrapper after measurement, but the inconsistency invites copy-paste drift.
- std/strpred.vyrn:211-226 — `contains` re-inlines the naive scan that `findPlain` (:150) already implements; deliberate and measured (2.7% generator-phase cost), but it is a second copy of the loop to keep correct.
- std/strpred.vyrn:297,:300 — boundary test written with decimal literals `192`/`128`; the comment at :255-257 names the shape as `(b & 0xC0) == 0x80`. Decimal form hides the bit pattern.
- std/strpred.vyrn:167,:201 — `return 0 - 1` for -1; if Vyrn lacks a unary-minus literal this is forced, otherwise noise.
- std/strpred.vyrn:178 — `worthPreparing` hardcodes thresholds 2 and 512 with no named constants; tuning means editing a predicate body.

#### Byte-shaped string ops seen in this module
Every operation in this module is byte-positioned by design (header :44-50): a `String` is UTF-8 bytes and all offsets/lengths are byte units.
- `byteLengthV` — std/strpred.vyrn:64-66 — unit: bytes (via `bytes(s).length`) — returns byte count; any caller reading it as a character count breaks on non-ASCII (char-safe sibling: `charCount()` / `chars(s)` in std/text, per assignment note).
- `startsWith` — std/strpred.vyrn:70-83 — unit: bytes (`s[j]`/`needle[j]` are `UInt8`, bound by `byteLengthV`) — byte-wise compare is SAFE on non-ASCII: UTF-8 self-synchronization means a valid needle cannot match at a non-boundary offset (proof at :45-50). Result is position-free (Bool), so nothing leaks.
- `endsWith` — std/strpred.vyrn:86-100 — unit: bytes (`off` derived from byte lengths, indexed with `s[off+j]`) — same self-synchronization safety; Bool result.
- `skipTable` — std/strpred.vyrn:123-137 — unit: bytes (indexes `needle[j]` raw bytes into a 256-entry table) — internal only; table keys are bytes, which is exactly right for the byte view.
- `findPlain` — std/strpred.vyrn:150-168 — unit: bytes (`from` is a byte offset, return value is a BYTE index) — BREAKS callers who treat the returned index as a character index on non-ASCII strings; there is no char-index sibling in this file.
- `worthPreparing` — std/strpred.vyrn:178-180 — unit: bytes (`haystackBytes` compared against 512-byte threshold) — threshold semantics only; mis-tunes, does not corrupt.
- `findSkipping` — std/strpred.vyrn:183-201 — unit: bytes — same byte-index return hazard as `findPlain`.
- `contains` — std/strpred.vyrn:203-228 — unit: bytes — Bool result, safe per the self-synchronization argument.
- `slice` — std/strpred.vyrn:289-313 — unit: bytes (`start`/`end` are BYTE offsets that must land on UTF-8 boundaries) — the one op where byte positioning is user-visible input: wrong-unit offsets (character positions) on non-ASCII either split a character (`SplitsCharacter`, checked at :297,:300) or cut mid-string silently if they happen to land on boundaries. Boundary-checked and reported, not silent.

No live defects found: the module's own byte handling is deliberate, documented (:44-58, :241-288), and guarded — `slice` rejects bad cut points with `SplitsCharacter` instead of producing mojibake, and the predicates' byte scans cannot match at non-boundary offsets.

### std/strings

Module doc (std/strings.vyrn:1-24) states the house contract plainly: a `String` is UTF-8 bytes, indices and lengths are BYTE offsets, `slice` refuses cuts inside a multi-byte character. Every offset-returning or offset-taking function below is therefore byte-shaped BY DESIGN, not by accident. Case/whitespace helpers are ASCII-only and pass non-ASCII bytes through unchanged.

Note: `contains` / `startsWith` / `endsWith` are compiler builtins (methods, also callable free), NOT exports here — they are only imported from std/strpred for internal use (std/strings.vyrn:31).

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| strings | fromBytesOr | `(b: Array<UInt8>, fallback: String) -> String` | `b` | `fromBytesOr(b, fallback)` | `b.fromBytesOr(fallback)` | SUBJECT AS ARGUMENT |
| strings | repeat | `(s: String, n: Int64) -> String` | `s` | `repeat(s, n)` | `s.repeat(n)` | SUBJECT AS ARGUMENT |
| strings | joinWith | `(parts: Array<String>, sep: String) -> String` | `parts` | `joinWith(parts, sep)` | `parts.joinWith(sep)` | SUBJECT AS ARGUMENT |
| strings | substring | `(s: String, start: Int64, end: Int64) -> String` | `s` | `substring(s, start, end)` | `s.substring(start, end)` | SUBJECT AS ARGUMENT |
| strings | indexOf | `(s: String, needle: String) -> Option<Int64>` | `s` | `indexOf(s, needle)` | `s.indexOf(needle)` | SUBJECT AS ARGUMENT |
| strings | lastIndexOf | `(s: String, needle: String) -> Option<Int64>` | `s` | `lastIndexOf(s, needle)` | `s.lastIndexOf(needle)` | SUBJECT AS ARGUMENT |
| strings | split | `(s: String, sep: String) -> Array<String>` | `s` | `split(s, sep)` | `s.split(sep)` | SUBJECT AS ARGUMENT |
| strings | lines | `(s: String) -> Array<String>` | `s` | `lines(s)` | `s.lines()` | SUBJECT AS ARGUMENT |
| strings | splitWhitespace | `(s: String) -> Array<String>` | `s` | `splitWhitespace(s)` | `s.splitWhitespace()` | SUBJECT AS ARGUMENT |
| strings | trimStart | `(s: String) -> String` | `s` | `trimStart(s)` | `s.trimStart()` | SUBJECT AS ARGUMENT |
| strings | trimEnd | `(s: String) -> String` | `s` | `trimEnd(s)` | `s.trimEnd()` | SUBJECT AS ARGUMENT |
| strings | trim | `(s: String) -> String` | `s` | `trim(s)` | `s.trim()` | SUBJECT AS ARGUMENT |
| strings | toLower | `(s: String) -> String` | `s` | `toLower(s)` | `s.toLower()` | SUBJECT AS ARGUMENT |
| strings | toUpper | `(s: String) -> String` | `s` | `toUpper(s)` | `s.toUpper()` | SUBJECT AS ARGUMENT |
| strings | replace | `(s: String, from: String, to: String) -> String` | `s` | `replace(s, from, to)` | `s.replace(from, to)` | SUBJECT AS ARGUMENT |
| strings | padStart | `(s: String, len: Int64, fill: String) -> String` | `s` | `padStart(s, len, fill)` | `s.padStart(len, fill)` | SUBJECT AS ARGUMENT |
| strings | padEnd | `(s: String, len: Int64, fill: String) -> String` | `s` | `padEnd(s, len, fill)` | `s.padEnd(len, fill)` | SUBJECT AS ARGUMENT |
| strings | toHex | `(n: UInt64) -> String` | `n` | `toHex(n)` | `n.toHex()` | SUBJECT AS ARGUMENT |
| strings | editDistance | `(a: String, b: String) -> Int64` | `a`, `b` | `editDistance(a, b)` | symmetric metric; either `a.editDistance(b)` or keep free | AMBIGUOUS |

Non-exported helper `fn isAsciiSpace(b: UInt8) -> Bool` (std/strings.vyrn:103) is private plumbing, not an export; not classified per rules.

#### Counts
exports=19 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=18 NO SUBJECT=0 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/strings.vyrn:50-58 — `repeat` rebuilds `out = out + s` in a loop: O(n*m) reallocation; no capacity preallocation.
- std/strings.vyrn:61-72 — `joinWith` same O(total^2) concat pattern; a method on `Array<String>` with a builder would fix both at once.
- std/strings.vyrn:129-148 — `lastIndexOf` does a naive byte-by-byte backward scan while every forward search (`indexOf`, `split`, `replace`) gets the `worthPreparing`/skip-table path; performance asymmetry, not a correctness bug.
- std/strings.vyrn:315-319, 320-330, 332-344 — `padStart`/`padEnd` measure the target width `len` in BYTES; a caller thinking in characters pads non-ASCII text to the wrong visual width. Docs admit it ("to at least `len` BYTES").
- std/strings.vyrn:366-375 — `editDistance` counts UTF-8 bytes, so any multi-byte character costs up to its byte length; docs scope this to ASCII identifiers but nothing enforces it.
- std/strings.vyrn:103-105 — `isAsciiSpace` is an ASCII-whitespace classifier local to this file [INFERENCE] — std/strpred may already carry equivalent byte predicates; worth deduplicating when methods land.

#### Byte-shaped string ops seen in this module
Unit for all of the following is BYTE offsets/lengths (per module header, std/strings.vyrn:1-7):
- `substring(s, start, end)` — std/strings.vyrn:87-99 — byte positions; safe against corruption (`slice` panics on mid-character cut) but a caller passing character indices crashes or mis-cuts.
- `indexOf` / `lastIndexOf` — std/strings.vyrn:111-149 — return BYTE offsets; UTF-8-safe matches (self-synchronizing property), but callers must treat the result as a byte index, never a char index.
- `split`, `lines`, `splitWhitespace`, `trimStart`, `trimEnd`, `trim` — std/strings.vyrn:156-258 — scan/cut at BYTE level; all cuts land on ASCII boundaries, so results are always valid UTF-8.
- `padStart` / `padEnd` — std/strings.vyrn:320-344 — width `len` is BYTES; breaks visual alignment on non-ASCII content even though output stays valid UTF-8.
- `editDistance` — std/strings.vyrn:376-434 — distance over BYTE sequences; overcounts on multi-byte text (each accented/CJK char costs >1).
- Char-safe siblings present in THIS file: none defined here; the header points at `charCount()` (Unicode scalars) and `chars(s)` living elsewhere (std/text per owner brief); `split` explicitly refuses per-byte splitting (std/strings.vyrn:151-155).

### std/symbolmap
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| symbolmap | symbol | `fn symbol(name: String, origin: Origin, derived: consume Array<JsonField>) -> Symbol` | NONE | `symbol(name, origin, derived)` | constructor of a `Symbol` record; no distinguished receiver | NO SUBJECT |
| symbolmap | strField | `fn strField(key: String, value: String) -> JsonField` | NONE | `strField(key, value)` | constructor of a `JsonField`; key and value are peers, no receiver | NO SUBJECT |
| symbolmap | mapJson | `fn mapJson(module: String, symbols: Array<Symbol>) -> String` | NONE | `mapJson(module, symbols)` | pure serialization of two inputs into one JSON document; neither argument is operated on as a receiver | NO SUBJECT |
| symbolmap | symbolMapFn | `gen fn symbolMapFn(module: String, symbols: Array<Symbol>) -> String` | NONE | `symbolMapFn(module, symbols)` | generation-context renderer over the same two inputs as `mapJson`; no receiver | NO SUBJECT |

#### Counts
exports=4 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=4 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/symbolmap.vyrn:107-109 `match stringFromBytes(bytes) { ... Err(why) => "" }` — unreachable empty fallback written instead of trapped; comment documents the choice, but the binding `why` is unused.
- std/symbolmap.vyrn:38,43 `.copy()` on every field by hand in each constructor; if the type system already consumes/moves these arguments, the copies may be redundant boilerplate repeated in both builders.
- Private helpers `originJson` (std/symbolmap.vyrn:50) and `mapSlug` (std/symbolmap.vyrn:89) are correctly unexported; no protocol/impl methods exist in this module.

#### Byte-shaped string ops seen in this module
- `module[i]` byte index + `module.byteLength` loop, std/symbolmap.vyrn:93-97 (`mapSlug`) — unit: UTF-8 bytes. The slug keeps only ASCII alphanumeric bytes; any non-ASCII character in a generator call (e.g. a non-ASCII path segment in `http(./påster)`), including any multi-byte letter, is silently dropped from the emitted function name, so two distinct calls can collide on the same slug.

### std/text
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| text | decodeUtf8 | fn decodeUtf8(b: Array<UInt8>) -> Option<Array<Int64>> | b | decodeUtf8(b) | b.decodeUtf8() | SUBJECT AS ARGUMENT |
| text | chars | fn chars(s: String) -> Array<Int64> | s | chars(s) | s.chars() | SUBJECT AS ARGUMENT |
| text | charCountV | fn charCountV(s: String) -> Int64 | s | charCountV(s) | s.charCountV() | SUBJECT AS ARGUMENT |
| text | lineAtV | fn lineAtV(b: Array<UInt8>, off: Int64) -> Int64 | b | lineAtV(b, off) | b.lineAt(off) | SUBJECT AS ARGUMENT |
| text | colAtV | fn colAtV(b: Array<UInt8>, off: Int64) -> Int64 | b | colAtV(b, off) | b.colAt(off) | SUBJECT AS ARGUMENT |
| text | showCps | fn showCps(a: Array<Int64>) -> String | a | showCps(a) | a.showCps() | SUBJECT AS ARGUMENT |

#### Counts
exports=6 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=6 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/text.vyrn:6 — naming split inside one module: `chars` is a plain export (free spelling, imported), `charCountV` carries the V suffix because method-only lookup cannot be shadowed by an import; two conventions for the same retirement story.
- std/text.vyrn:144 — `chars` returns `Array<Int64>` of raw scalar values rather than a char/String view type; every caller that wants text back must re-encode.
- std/text.vyrn:145-147 — `None` arm in `chars` is unreachable by language invariant (a `String` is always valid UTF-8); dead branch kept only so `decodeUtf8` stays total.
- std/text.vyrn:227 — `showCps` is a test-pinning helper (`out = out + c.toString()`) exported from the public std surface; O(n^2) concatenation and test-only purpose.

#### Byte-shaped string ops seen in this module
- `s.byteLength` (std/text.vyrn:164) — unit: bytes — used as the scan bound for `charCountV`; correct there because the loop counts leading bytes, but any reuse of the bound as a character count breaks on non-ASCII.
- `s[i]` yields `UInt8` (std/text.vyrn:168) — unit: single UTF-8 byte — indexing a `String` gives bytes, not characters; safe here (continuation-bit test), silently wrong if reused as character access.
- `bytes(s)` (std/text.vyrn:145) — unit: UTF-8 byte array — converts `String` to bytes; positions in the result are byte offsets, not character offsets.
- `lineAtV` / `colAtV` (std/text.vyrn:186, 211) — unit: byte offset (`off`) — columns count BYTES by design and match the builtins (pinned at std/text.vyrn:328-332); the byte after `é` is column 3. Documented mismatch lives in std/vyx, whose wrapper calls them "chars since the last LF" (referenced std/text.vyrn:207).

### Character-safe siblings present in this file (census-wide answer)
- `chars(s)` (std/text.vyrn:144) — full Unicode decode of a `String` to scalar values; the ONLY character-wise iteration primitive here.
- `charCountV(s)` (std/text.vyrn:163) — Unicode scalar count without allocation.
- Adjacent but on `Array<UInt8>`, not `String`: `decodeUtf8` (validated decode to scalars). Nothing else char-safe exists here: there is no char-indexed `charAt`, `slice`, `find`, or case op in this module.

### std/time
#### Classification
| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| time | now | `now() -> Instant` | NONE | `now()` | constructor of `Instant` from the host clock; no distinguished receiver | NO SUBJECT |
| time | monotonic | `monotonic() -> Int64` | NONE | `monotonic()` | host clock read; no receiver | NO SUBJECT |
| time | toMillis | `toMillis(i: Instant) -> Int64` | `i: Instant` | `toMillis(i)` | `i.toMillis()` | SUBJECT AS ARGUMENT |
| time | fromMillis | `fromMillis(n: Int64) -> Instant` | NONE | `fromMillis(n)` | constructor of `Instant` from a raw count; no distinguished receiver | NO SUBJECT |
| time | civil | `civil(i: Instant) -> Civil` | `i: Instant` | `civil(i)` | `i.civil()` | SUBJECT AS ARGUMENT |
| time | year | `year(i: Instant) -> Int64` | `i: Instant` | `year(i)` | `i.year()` | SUBJECT AS ARGUMENT |
| time | month | `month(i: Instant) -> Int64` | `i: Instant` | `month(i)` | `i.month()` | SUBJECT AS ARGUMENT |
| time | day | `day(i: Instant) -> Int64` | `i: Instant` | `day(i)` | `i.day()` | SUBJECT AS ARGUMENT |
| time | hour | `hour(i: Instant) -> Int64` | `i: Instant` | `hour(i)` | `i.hour()` | SUBJECT AS ARGUMENT |
| time | minute | `minute(i: Instant) -> Int64` | `i: Instant` | `minute(i)` | `i.minute()` | SUBJECT AS ARGUMENT |
| time | second | `second(i: Instant) -> Int64` | `i: Instant` | `second(i)` | `i.second()` | SUBJECT AS ARGUMENT |
| time | format | `format(i: Instant) -> String` | `i: Instant` | `format(i)` | `i.format()` | SUBJECT AS ARGUMENT |
| time | formatIso | `formatIso(i: Instant) -> String` | `i: Instant` | `formatIso(i)` | `i.formatIso()` | SUBJECT AS ARGUMENT |

#### Counts
exports=13 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=10 NO SUBJECT=3 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/time.vyrn:76 `civil(i)` recomputed for each field: `year`, `month`, `day` (std/time.vyrn:81,86,91) and again inside `format`/`formatIso` via the field accessors (std/time.vyrn:135-147); callers formatting many fields pay the division chain repeatedly.
- std/time.vyrn:18 `Instant` is `Int64 where value >= 0`, but `fromMillis` (std/time.vyrn:53) does no visible validation beyond that boundary predicate; doc says "validated non-negative at the boundary" while the body is a plain return.

#### Byte-shaped string ops seen in this module
- none. The module takes and returns only `Int64` fields; string building uses `\{}` interpolation (`pad2`/`pad4`, std/time.vyrn:113-132), which never indexes a `String`.

### std/tw

Module note: `std/tw` is a compile-time generator. One exported `gen fn` reads
`theme.json`, derives a closed utility vocabulary, and returns a synthesized
module as Vyrn source. Every other function in the file is module-private
(plumbing for the generator); the file declares no protocols or impls.

#### Classification

| module | export | signature | subject | call form today | should be | class |
|--------|--------|-----------|---------|-----------------|-----------|-------|
| tw | tw | `gen fn tw(theme: String) -> String` | NONE | `tw("./theme.json")` | generator entry point — constructs a synthesized module from a theme path; no distinguished receiver (constructor-like) | NO SUBJECT |

#### Counts
exports=1 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=0 NO SUBJECT=1 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)

- std/tw.vyrn:60 imports `joinWith`, `substring`, `fromBytesOr` from std/strings — three subject-as-argument helpers pulled in for generator plumbing.
- std/tw.vyrn:411 `joinWith(twKnownTopLevel(), ", ")` — array is argument one; method form is `twKnownTopLevel().joinWith(", ")`.
- std/tw.vyrn:908 `joinWith(twKnownFamilies(), ", ")` — same shape as :411.
- std/tw.vyrn:770 `joinWith(bps, "|")` — breakpoint-prefix array is argument one; should be `bps.joinWith("|")`.
- std/tw.vyrn:780 `joinWith(bases, "|")` — base-alternation array is argument one; should be `bases.joinWith("|")`.
- std/tw.vyrn:407 `includes(known, head)` and `includes(seen, head)` — array is argument one; should be `known.includes(head)`.
- std/tw.vyrn:652 `includes(families, twHeadOf(e.key))` — same `includes` shape as :407.
- std/tw.vyrn:247 `contains(prefix, ".")` — string is argument one; should be `prefix.contains(".")`.
- std/tw.vyrn:349 `substring(key, i + 1, key.byteLength)` — free-function substring over byte positions; method form `key.substring(i + 1, key.byteLength)` keeps the byte-unit cost visible at the call site.
- std/tw.vyrn:425 `substring(key, 0, i)` — same `substring` shape as :349.
- std/tw.vyrn:368,685 `fromBytesOr(out, key)` — bytes-to-string rebuild with the original as fallback; byte round-trip is inherent here (byte-level `'.'`→`'-'` and `':'` escaping), but the fallback argument hides which string the bytes came from.
- std/tw.vyrn:76-94,107-159 `bytes(s)` + index loops in `twClassSafe`/`twValueSafe` — hand-rolled byte grammars over `String`; six near-identical character-class loops that a shared predicate module (`std/strpred`) could absorb.
- std/tw.vyrn:224,236 `.copy()` calls scattered through the flatten path (`msg.copy()`, `key.copy()`, `value.copy()`) — value-semantics noise, no functional effect noted.
- std/tw.vyrn:586 `consume p.value` inside `twSafelist` — only consume-site in the module; inconsistent with the `.copy()` style everywhere else.

#### Byte-shaped string ops seen in this module

- `twClassSafe` (std/tw.vyrn:76-94): iterates `bytes(name)`; unit = byte. Deliberate gate — any non-ASCII byte fails the `[a-z][a-z0-9-]*` grammar, so non-ASCII names are rejected loudly, not corrupted. Sound.
- `twValueSafe` (std/tw.vyrn:107-159): iterates `bytes(v)`; unit = byte. Same deliberate rejection of non-ASCII values. Sound.
- `twDropHead` (std/tw.vyrn:344-354): finds the first `'.'` by byte scan, then `substring(key, i + 1, key.byteLength)`; unit = byte. Currently sound only because `'.'` is ASCII (it cannot occur inside a multi-byte sequence), so the split lands on a code-point boundary and the byte offsets are self-consistent. Breaks silently if anyone swaps the byte scan for a char index without changing `substring`'s unit.
- `twHeadOf` (std/tw.vyrn:420-430): same pattern as `twDropHead`; same reasoning, same fragility.
- `twDashDots` (std/tw.vyrn:358-369) and `twCssEscape` (std/tw.vyrn:674-686): full byte-copy rebuilds via `fromBytesOr`; unit = byte. Copying and single-byte substitutions are UTF-8-transparent; sound.
- `twCssSingleToken` (std/tw.vyrn:169-189): whitespace checks against `sc.pos` / `peek(sc)`; unit = byte through the `std/scan` cursor. Whitespace bytes are ASCII, so tokenization is unaffected by non-ASCII content. Sound.

### std/ui

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| ui | pageError | (status: Int64, message: String) -> PageError | NONE | pageError(404, msg) | constructor of PageError | NO SUBJECT |
| ui | notFound | (message: String) -> PageError | NONE | notFound(msg) | constructor of PageError | NO SUBJECT |
| ui | badRequest | (message: String) -> PageError | NONE | badRequest(msg) | constructor of PageError | NO SUBJECT |
| ui | noHead | () -> Head | NONE | noHead() | constructor of the empty Head | NO SUBJECT |
| ui | withTitle | (h: Head, title: String) -> Head | h | withTitle(h, t) | h.withTitle(t) | SUBJECT AS ARGUMENT |
| ui | withStylesheet | (h: Head, href: String) -> Head | h | withStylesheet(h, href) | h.withStylesheet(href) | SUBJECT AS ARGUMENT |
| ui | withModule | (h: Head, src: String) -> Head | h | withModule(h, src) | h.withModule(src) | SUBJECT AS ARGUMENT |
| ui | withScript | (h: Head, src: String) -> Head | h | withScript(h, src) | h.withScript(src) | SUBJECT AS ARGUMENT |
| ui | withMeta | (h: Head, name: String, content: String) -> Head | h | withMeta(h, n, c) | h.withMeta(n, c) | SUBJECT AS ARGUMENT |
| ui | headHtml | (h: Head) -> Array<Html> | h | headHtml(h) | h.headHtml() | SUBJECT AS ARGUMENT |
| ui | headTitleOf | (h: Head) -> String | h | headTitleOf(h) | h.headTitleOf() | SUBJECT AS ARGUMENT |
| ui | query | <T>(run: consume fn() -> T) -> Query<T> | NONE | query(f) | constructor of Query from a closure | NO SUBJECT |
| ui | lazy | <T>(q: consume Query<T>) -> Lazy<T> | q | lazy(q) | q.lazy() | SUBJECT AS ARGUMENT |
| ui | paramQuery | <P, T>(run: consume fn(P) -> T) -> ParamQuery<P, T> | NONE | paramQuery(f) | constructor of ParamQuery from a closure | NO SUBJECT |
| ui | paramLazy | <P, T>(q: consume ParamQuery<P, T>) -> ParamLazy<P, T> | q | paramLazy(q) | q.paramLazy() | SUBJECT AS ARGUMENT |
| ui | runQuery | <T>(q: Query<T>) -> T | q | runQuery(q) | q.runQuery() | SUBJECT AS ARGUMENT |
| ui | runLazy | <T>(q: Lazy<T>) -> T | q | runLazy(q) | q.runLazy() | SUBJECT AS ARGUMENT |
| ui | runParamQuery | <P, T>(q: ParamQuery<P, T>, p: P) -> T | q (p is the payload, not the receiver) | runParamQuery(q, p) | q.runParamQuery(p) | SUBJECT AS ARGUMENT |
| ui | runParamLazy | <P, T>(q: ParamLazy<P, T>, p: P) -> T | q (p is the payload, not the receiver) | runParamLazy(q, p) | q.runParamLazy(p) | SUBJECT AS ARGUMENT |
| ui | noQuery | () -> Query<Unit> | NONE | noQuery() | constructor of the absent query | NO SUBJECT |
| ui | Page.head (contract member) | () -> Head = noHead(); also (d: T), (p: P), (p: P, d: T) | page module | p.head(...) | already member form | SUBJECT FIRST |
| ui | Page.data (contract member) | () -> Query<T> = noQuery(); also Lazy/ParamQuery/ParamLazy shapes | page module | p.data() | already member form | SUBJECT FIRST |
| ui | Page.page (contract member) | () -> Html = uiNoView(); also (d: T), (p: P), (p: P, d: T) | page module | p.page(...) | already member form | SUBJECT FIRST |
| ui | Page.respond (contract member) | () -> Response = uiNoRespond(); also (d: T), (p: P), (p: P, d: T) | page module | p.respond(...) | already member form | SUBJECT FIRST |
| ui | uiNoView | () -> Html | NONE | uiNoView() | constructor of the absent view | NO SUBJECT |
| ui | uiNoRespond | () -> Response | NONE | uiNoRespond() | constructor of the absent response | NO SUBJECT |
| ui | uiDataQuery | () -> Int64 | NONE | uiDataQuery() | pure constant (contract index 0) | NO SUBJECT |
| ui | uiDataLazy | () -> Int64 | NONE | uiDataLazy() | pure constant (contract index 1) | NO SUBJECT |
| ui | uiDataParamQuery | () -> Int64 | NONE | uiDataParamQuery() | pure constant (contract index 2) | NO SUBJECT |
| ui | uiDataParamLazy | () -> Int64 | NONE | uiDataParamLazy() | pure constant (contract index 3) | NO SUBJECT |
| ui | uiWantsData | (req: Request) -> Bool | req | uiWantsData(req) | req.wantsData() | SUBJECT AS ARGUMENT |
| ui | uiPayload | (page: String, title: String, props: String, params: String) -> String | NONE | uiPayload(p, t, pr, pa) | pure JSON assembly; four equal parts, no receiver | NO SUBJECT |
| ui | uiErrorPayload | (status: Int64, props: String) -> String | NONE | uiErrorPayload(s, props) | pure JSON assembly | NO SUBJECT |
| ui | uiDataResponse | (body: String) -> Response | NONE | uiDataResponse(body) | constructor of a 200 JSON Response | NO SUBJECT |
| ui | uiDataMiss | () -> Response | NONE | uiDataMiss() | constant response value | NO SUBJECT |
| ui | uiErrorResponseOf | (e: PageError) -> Response | e | uiErrorResponseOf(e) | e.errorResponse() | SUBJECT AS ARGUMENT |
| ui | pages | gen (dir: String) -> String | dir | pages("./pages") | dir.pages() | SUBJECT AS ARGUMENT |
| ui | pagesThemed | gen (dir: String, theme: String) -> String | dir | pagesThemed(dir, theme) | dir.pagesThemed(theme) | SUBJECT AS ARGUMENT |
| ui | pagesClient | gen (dir: String) -> String | dir | pagesClient(dir) | dir.pagesClient() | SUBJECT AS ARGUMENT |
| ui | pagesClientThemed | gen (dir: String, theme: String) -> String | dir | pagesClientThemed(dir, theme) | dir.pagesClientThemed(theme) | SUBJECT AS ARGUMENT |

#### Counts
exports=40 (32 `export fn` + 4 `export gen fn` + 4 `contract Page` members) SUBJECT FIRST=4 SUBJECT AS ARGUMENT=19 NO SUBJECT=17 AMBIGUOUS=0

#### Other smells (std/ui.vyrn relative to repo root)
- std/ui.vyrn:153-212 — the whole `Head` family (`withTitle`/`withStylesheet`/`withModule`/`withScript`/`withMeta`/`headHtml`/`headTitleOf`) is free functions over argument-one `Head`; the clearest subject-first candidates in the module.
- std/ui.vyrn:276-298 — the four runners (`runQuery`/`runLazy`/`runParamQuery`/`runParamLazy`) are free functions whose first argument is always the query value; natural methods on `Query`/`Lazy`/`ParamQuery`/`ParamLazy`.
- std/ui.vyrn:2597-2618, 2624-2645, 2909-2930, 2934-2955 — the four generator entry points repeat the identical scan + empty-check + error-preamble block four times; one shared helper would remove ~60 lines.
- std/ui.vyrn:1956-1969 — `uiDynSegIndex` returns `0` when the k-th dynamic segment is not found; a caller/count mismatch would silently bind the wrong segment instead of failing loudly.
- std/ui.vyrn:1601-1602 — garbled comment: "Longest ancestor-prefix length wins ties are impossible (one layout per dir)".
- std/ui.vyrn:424-429 — `uiAcceptNames` re-implements membership with `indexOf` while the module already imports `includes` from std/arrays (used at 1166).
- std/ui.vyrn:826-827, 1626 — sentinels spelled `0 - 1` instead of `-1` throughout.
- std/ui.vyrn:1058-1060 — `uiPageErrAt` anchors every page diagnostic at line 1 column 1; inherent to reflection, but worth recording.

#### Byte-shaped string ops seen in this module
- `bytes(s)` + byte-index loops over `String` throughout: `uiSliceStr` (497), `uiUpperFirst` (508), `uiRegexEscapeLit` (558), `uiStem` (580), `uiDynName` (591), `uiSegIdent` (714), `uiIdentLead` (737), `uiRecordBody`/`uiSplitFields`/`uiParseFields` (824-892), `uiSegNeedsDecode` (1886), `uiScanAll` extension strip (1548-1549). Unit: bytes (`ba[i]`, `.byteLength`). Most scan for ASCII delimiters/affixes (`{`, `,`, `:`, `.vyrn`, `[`) — safe on UTF-8 because ASCII bytes never appear inside multibyte sequences.
- `trimmed.byteLength > 0` emptiness tests (875, 1178, 1774, 2017, 2035): byteLength used only as empty/non-empty — unit-independent, safe.
- `uiUpperFirst` (508): byte 0 only; a non-ASCII leading character is passed through untouched (no case mapping, no corruption).
- `uiSegIdent` (714): every byte outside ASCII alnum is dropped as a word break; a non-ASCII route slug (`café.vyrn`) silently loses its non-ASCII characters in the generated helper name (`cafPath`). The doc comment declares the lossy many-to-one conversion deliberate.
- Emitted router runtime `uiRouteSegments`/`uiRouteSlice` (1707-1747) and client twins `uiClientSlice`/`uiClientSegments` (2714-2754): split request paths on `/` and `?` by byte offset, reassembling segments from raw bytes. Delimiters are ASCII so segment boundaries are correct; segment content passes through byte-for-byte (and is percent-decoded separately via std/codecs `urlDecode`).

### std/von

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| von | yes | `copyVonArray(xs: Array<Von>) -> Array<Von>` | xs | `copyVonArray(xs)` | `xs.copy()` (method form unwritable today — see std/von.vyrn:112-114) | SUBJECT AS ARGUMENT |
| von | yes | `copyVonFields(fs: Array<VonField>) -> Array<VonField>` | fs | `copyVonFields(fs)` | `fs.copy()` (same unwritable-method reason) | SUBJECT AS ARGUMENT |
| von | yes | `copyVonEntries(es: Array<VonEntry>) -> Array<VonEntry>` | es | `copyVonEntries(es)` | `es.copy()` (same unwritable-method reason) | SUBJECT AS ARGUMENT |
| von | yes (`gen`) | `parseVon(src: String) -> Result<VonDoc, String>` | src | `parseVon(src)` | `src.parseVon()` | SUBJECT AS ARGUMENT |
| von | yes | `emitVon(v: Von) -> String` | v | `emitVon(v)` | `v.emitVon()` | SUBJECT AS ARGUMENT |
| von | yes | `toVon(doc: VonDoc) -> String` | doc | `toVon(doc)` | `doc.toVon()` | SUBJECT AS ARGUMENT |
| von | yes | `jsonToVon(json: Json, typeName: String, module: String) -> Result<String, String>` | json | `jsonToVon(json, typeName, module)` | `json.jsonToVon(typeName, module)` | SUBJECT AS ARGUMENT |
| von | impl `Copy for Von` | `fn copy(self) -> Von` | self (the receiver) | `v.copy()` | already a method | SUBJECT FIRST |
| von | impl `Owned for Von` | `fn release(consume self)` | self (the receiver) | `v.release()` | already a method | SUBJECT FIRST |

#### Counts
exports=7 SUBJECT FIRST=2 SUBJECT AS ARGUMENT=5 NO SUBJECT=0 AMBIGUOUS=0

(SUBJECT FIRST rows are the two protocol methods; both are non-export impl members, so exports=7 while classified rows=9.)

#### Other smells (path:LINE relative to repo root)
- std/von.vyrn:354-355 — hand-builds `"line N, col M:"` instead of reusing `errAt`; duplicated position formatting.
- std/von.vyrn:439,460,480,514,641 — unreachable `return Err(...)` after `while true` loops; dead code in five readers.
- std/von.vyrn:320-323,797-800,833-836 — `stringFromBytes` failure silently becomes `""` in `rawNumber`, `hex2`, `emitString`; an invalid-byte failure would masquerade as valid output.
- std/von.vyrn:781-789 — `indent(n)` rebuilds the padding by `n` string concatenations; quadratic in depth (depth capped at 128 by the reader, so bounded, but the writer accepts deeper trees built by hand — see std/von.vyrn:1079-1083).
- std/von.vyrn:1128 — `isKeyword` hardcodes the compiler's keyword list inline; drift risk when Vyrn gains keywords.
- std/von.vyrn:1138-1156 — `isIdent` accepts ASCII letters only; correct per its own doc comment, but it silently rejects any future non-ASCII identifier policy.
- std/von.vyrn:89-92 — `vonGive` is a generic whose `T` is never constrained or used beyond `drop v`; naming-only indirection so a match arm stays an expression.

#### Byte-shaped string ops seen in this module
- `bytes(src)` — std/von.vyrn:727,732 — whole document String to `Array<UInt8>`; used for BOM/tab layout checks and verbatim number extraction. Safe: layout scan and digit scanning are encoding-agnostic.
- `raw.byteLength` — std/von.vyrn:354 — compares token column (CHARACTERS, per std/von.vyrn:287-289) against `ncol + raw.byteLength`. Safe here only because `rawNumber` (std/von.vyrn:307-324) collects solely `'0'..'9'` and `.`, all single-byte ASCII, so byte length equals character count. Would break if the collected character set ever widened past ASCII.
- `colOffset` skipping UTF-8 continuation bytes — std/von.vyrn:290-301 — deliberately converts character columns to byte offsets by skipping `(b/64)==2` bytes; the one place the module reconciles char columns with byte indexing, done correctly.
- `bytes(s)` in `emitString` — std/von.vyrn:806-837 — byte-wise escaping; passes bytes >= 32 through untouched, so multi-byte UTF-8 survives intact. Safe.
- `bytes(raw)` in `hasLeadingZero`, `numberOk`, `hasExponent`, `hasPoint`, `isIdent` — std/von.vyrn:328-334,869-899,1161-1178,1138-1156 — byte scans over number text (ASCII by construction) and identifier text (ASCII-only by rule). Safe under current contracts.

No live byte defects found: every byte-indexed access sits on data proven ASCII (digit text, identifiers) or is paired with the continuation-byte skip in `colOffset`.

### std/vyx

The `.vyx` single-file component compiler. Almost every export takes the `.vyx`
source text (or its file path) as argument one and is therefore in the
SUBJECT AS ARGUMENT class; the private helpers below it work on
`Array<UInt8>` by design and are not exports.

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| vyx | vyxParseTemplate | gen (source: String) -> VyxTemplate | source | vyxParseTemplate(source) | source.parseTemplate() | SUBJECT AS ARGUMENT |
| vyx | vyxCompileComponent | gen (compName: String, source: String, dir: String) -> VyxComp | compName / source | vyxCompileComponent(compName, source, dir) | source.compileComponentAs(compName, dir) — the compiled artifact is the source; the stem only names it | AMBIGUOUS |
| vyx | vyxBuildModule | gen (comps: consume Array<VyxComp>, themed: Bool, theme: String) -> String | comps | vyxBuildModule(comps, themed, theme) | comps.buildModule(themed, theme) | SUBJECT AS ARGUMENT |
| vyx | components | gen (dir: String) -> String | dir | components(dir) | dir.components() | SUBJECT AS ARGUMENT |
| vyx | componentsThemed | gen (dir: String, theme: String) -> String | dir | componentsThemed(dir, theme) | dir.componentsThemed(theme) | SUBJECT AS ARGUMENT |
| vyx | vyxDataForm | fn (ret: String) -> Int64 | ret | vyxDataForm(ret) | ret.dataForm() | SUBJECT AS ARGUMENT |
| vyx | vyxDataFormIsLazy | fn (form: Int64) -> Bool | form | vyxDataFormIsLazy(form) | form.isLazyDataForm() | SUBJECT AS ARGUMENT |
| vyx | vyxDataFormHasParams | fn (form: Int64) -> Bool | form | vyxDataFormHasParams(form) | form.hasParamsDataForm() | SUBJECT AS ARGUMENT |
| vyx | vyxDataRunner | fn (form: Int64) -> String | form | vyxDataRunner(form) | form.dataRunner() | SUBJECT AS ARGUMENT |
| vyx | vyxQueryDataType | fn (ret: String) -> String | ret | vyxQueryDataType(ret) | ret.queryDataType() | SUBJECT AS ARGUMENT |
| vyx | vyxPageInterface | gen (source: String) -> ModuleInterface | source | vyxPageInterface(source) | source.pageInterface() | SUBJECT AS ARGUMENT |
| vyx | vyxPageShape | gen (source: String) -> VyxPageShape | source | vyxPageShape(source) | source.pageShape() | SUBJECT AS ARGUMENT |
| vyx | vyxBuildPageModule | gen (source: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildPageModule(source, dir, themed, theme) | source.buildPageModule(dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildPageClientModuleAt | gen (source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildPageClientModuleAt(source, srcPath, dir, themed, theme) | source.buildPageClientModuleAt(srcPath, dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildPageModuleAt | gen (source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildPageModuleAt(source, srcPath, dir, themed, theme) | source.buildPageModuleAt(srcPath, dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxPage | gen (vyxPath: String) -> String | vyxPath (the `.vyx` file read at generation) | vyxPage(vyxPath) | vyxPath.page() | SUBJECT AS ARGUMENT |
| vyx | vyxPageThemed | gen (vyxPath: String, theme: String) -> String | vyxPath | vyxPageThemed(vyxPath, theme) | vyxPath.pageThemed(theme) | SUBJECT AS ARGUMENT |
| vyx | vyxPageClient | gen (vyxPath: String) -> String | vyxPath | vyxPageClient(vyxPath) | vyxPath.pageClient() | SUBJECT AS ARGUMENT |
| vyx | vyxPageClientThemed | gen (vyxPath: String, theme: String) -> String | vyxPath | vyxPageClientThemed(vyxPath, theme) | vyxPath.pageClientThemed(theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildLayoutModule | gen (source: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildLayoutModule(source, dir, themed, theme) | source.buildLayoutModule(dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildLayoutModuleAt | gen (source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildLayoutModuleAt(source, srcPath, dir, themed, theme) | source.buildLayoutModuleAt(srcPath, dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxLayout | gen (vyxPath: String) -> String | vyxPath | vyxLayout(vyxPath) | vyxPath.layout() | SUBJECT AS ARGUMENT |
| vyx | vyxLayoutThemed | gen (vyxPath: String, theme: String) -> String | vyxPath | vyxLayoutThemed(vyxPath, theme) | vyxPath.layoutThemed(theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildErrorModule | gen (source: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildErrorModule(source, dir, themed, theme) | source.buildErrorModule(dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxBuildErrorModuleAt | gen (source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String | source | vyxBuildErrorModuleAt(source, srcPath, dir, themed, theme) | source.buildErrorModuleAt(srcPath, dir, themed, theme) | SUBJECT AS ARGUMENT |
| vyx | vyxError | gen (vyxPath: String) -> String | vyxPath | vyxError(vyxPath) | vyxPath.error() | SUBJECT AS ARGUMENT |
| vyx | vyxErrorThemed | gen (vyxPath: String, theme: String) -> String | vyxPath | vyxErrorThemed(vyxPath, theme) | vyxPath.errorThemed(theme) | SUBJECT AS ARGUMENT |
| vyx | Component.* (contract method) | fn *(..) -> Html | NONE | contract shape promise, checked against the synthesized module surface | open contract constraining shape only; no distinguished receiver | NO SUBJECT |
| vyx | Owned for VyxNode.release (impl method) | fn release(consume self) | self | node.release() | already a method | SUBJECT FIRST |

#### Counts
exports=29 SUBJECT FIRST=1 SUBJECT AS ARGUMENT=26 NO SUBJECT=1 AMBIGUOUS=1

#### Other smells (path:LINE relative to repo root)
- std/vyx.vyrn:218 — `vyxAppend` is self-documented as unused by the compiler today ("kept for historical API"); delete it.
- std/vyx.vyrn:196 and std/vyx.vyrn:3805 — `vyxIsIdent` and `vyxIsIdentByte` are byte-identical predicates defined twice; keep one.
- std/vyx.vyrn:315 — `vyxFindLast` re-runs `vyxFind` from `from` to end of file for every hit; O(n^2) on needle-rich input.
- std/vyx.vyrn:1517 — `vyxFindPropsBlockFrom` copies the whole tail of `ba` byte-by-byte into a fresh array instead of threading a start offset.
- std/vyx.vyrn:3956 — `vyxStripDeadHelpers` caps the fixed point at 16 rounds and silently returns a partially pruned script; more than 16 removable helpers stay in the client bundle with no diagnostic.
- std/vyx.vyrn:3972 — `vyxStripDeadHelpersOnce` removes at most one helper per pass, making the fixed point quadratic in the dead-helper count.
- std/vyx.vyrn:2169 — `hasKids` computes `trim(vyxChildText(kids)).byteLength > 0 || kids.length > 0`; the second clause makes the first dead weight (any child list wins).
- std/vyx.vyrn:3019 and std/vyx.vyrn:3056 — `components` and `componentsThemed` duplicate roughly 30 lines of body differing only in the `themed` flag threaded to `vyxFinish`.
- std/vyx.vyrn:4546-4580 — `vyxPage` / `vyxPageThemed` / `vyxPageClient` / `vyxPageClientThemed` repeat the same `readFile` + match + report wrapper four times.
- std/vyx.vyrn:3810, std/vyx.vyrn:3873, std/vyx.vyrn:3918 — `vyxWordUsed`, `vyxCalledInCorpus` and `vyxMentionsIdent` are three near-identical hand-rolled whole-token scanners over byte arrays; one boundary-aware find helper would replace them.
- std/vyx.vyrn:2482 — `vyxHasTopComma` tracks `(` and `[` but not `{`, so a record-literal argument `{a: 1, b: 2}` reads as two arguments and is refused.
- std/vyx.vyrn:1210 — `vyxSplitFields` splits at generic-depth-0 commas tracking only `<>`; parentheses, brackets and string literals in an entry split wrongly.
- std/vyx.vyrn:1956, std/vyx.vyrn:3120, std/vyx.vyrn:3460, std/vyx.vyrn:3568 — magic prefix offsets (`vyxSlice(bytes(t), 7, …)` for `"export "`); also 6 (:3325, :3344, :3652), 11/12 (:2526, :3654-3658), `fromAt + 6` (:2523), `idx + 6` (:3910), `asIdx + 4` (:4049). Brittle literals scattered through the scanner.
- std/vyx.vyrn:1271 — `vyxLineStartOffset` is a full-file linear scan per call and runs once per leading-keyword token.
- std/vyx.vyrn:2424 — `vyxEmitEvent` finds the first `(` in raw bytes with no quote awareness, so a quoted `(` earlier in the handler spec mis-splits it.

#### Byte-shaped string ops seen in this module
- `bytes(s)` + `Array<UInt8>` slicing with byte offsets everywhere — the module's core representation and a documented design choice (std/vyx.vyrn:128-132): a compiler walks file bytes, and `substring` on String was deliberately not a drop-in for `vyxSlice`.
- `s.byteLength` as emptiness/length test — std/vyx.vyrn:659, 775, 791-792, 916, 1447, 2443, 2538, 3268, 3516, 4168, 4405 — safe where used as `> 0`; the length-arithmetic sites stay in the byte domain consistently.
- `vyxLowerFirst` std/vyx.vyrn:201 and `vyxUpperFirst` std/vyx.vyrn:3550 — case-fold the FIRST BYTE of a String; safe only because inputs are ASCII identifiers (file stems, `"head"`/`"data"`). A non-ASCII stem passes through unfolded, not corrupted.
- Diagnostic-column math std/vyx.vyrn:2452-2454 — `argCol = a.col + argRel` adds a BYTE offset within the attribute value to a 1-based column; see live defects.
- `vyxStrLit` std/vyx.vyrn:271 — byte-wise escaping of a String into a literal; UTF-8 passes through untouched (documented at :269), so template text survives verbatim.
- `vyxCollapseWs` std/vyx.vyrn:338 — byte-wise ASCII-whitespace collapse; UTF-8 continuation bytes never match an ASCII whitespace byte, so multi-byte text is safe.
- `vyxIdentBytes` std/vyx.vyrn:2231 — maps every non-identifier BYTE to `_`; a non-ASCII path yields one `_` per byte (cosmetic mangling of a minted namespace alias only).

### std/vyx-hints

#### Classification

| module | export | signature | subject | call form today | should be | class |
|---|---|---|---|---|---|---|
| vyx-hints | vyxHints | `gen fn vyxHints(dir: String) -> String` | dir (the `.vyx` tree checked) | `vyxHints("./app/widgets")` | `dir.vyxHints()` | SUBJECT AS ARGUMENT |
| vyx-hints | vyxHintsConfigured | `gen fn vyxHintsConfigured(dir: String, config: String) -> String` | dir (config only parameterizes severity) | `vyxHintsConfigured("./app/widgets", "./vyrn.json")` | `dir.vyxHintsConfigured("./vyrn.json")` | SUBJECT AS ARGUMENT |
| vyx-hints | vhCheck | `gen fn vhCheck(p: Policy, src: String, file: String) -> String` | src (the template text linted) | `vhCheck(noPolicy(), src, "T.vyx")` | `src.vhCheck(noPolicy(), "T.vyx")` | SUBJECT AS ARGUMENT (subject is arg 2, behind the policy; policy would lead as `src.vhCheck(p, file)`) |

No protocol or impl declarations exist in this module; all three exports are free generator functions.

#### Counts
exports=3 SUBJECT FIRST=0 SUBJECT AS ARGUMENT=3 NO SUBJECT=0 AMBIGUOUS=0

#### Other smells (path:LINE relative to repo root)
- std/vyx-hints.vyrn:81 — `readFile` `Err` is collapsed to `""`, then `""` means "cannot read": a genuinely empty-but-valid config file reports "cannot read the hints config", a wrong diagnosis.
- std/vyx-hints.vyrn:137 — error payload moved (`err: e`) while siblings copy (`e.copy()` at :122 and :170); inconsistent ownership idiom across the two record shapes.
- std/vyx-hints.vyrn:157 — `vhIsDir` decides directory-ness by performing a full recursive-capable `listDir` (reads the whole listing to answer a boolean); documented as mirroring `std/ui`'s `uiIsDir`, still a wasted allocation per directory visited.
- std/vyx-hints.vyrn:676 — `vhHasAny(attrs, a1, a2, a3)` hardcodes arity 3; every call site passes exactly the aria-label/title trio, so the helper adds a shape without generality.
- std/vyx-hints.vyrn:226 — `VNFor`/`VNIf` match arms are documented unreachable ("cannot appear") yet fully destructured and walked; deliberate defensive weight, but dead by the module's own comment.
- std/vyx-hints.vyrn:736 — `vhAnchorLine`/`vhAnchorCol` (:743) use a for-loop-return-on-first-iteration idiom to read the first element; an explicit emptiness check would say so directly.
- std/vyx-hints.vyrn:86 — after a failed config read the function still emits `vhModule(0)`, synthesizing a `vyxHintsChecked() -> 0` declaration alongside an Error report; a tree that was never checked gets a clean-looking count artifact.

#### Byte-shaped string ops seen in this module
- `s.byteLength` — std/vyx-hints.vyrn:565 (`vhStripForScheme`) — unit: bytes — bound for end-trimming loop; nothing breaks: only bytes `<= ' '` (ASCII C0+space) are trimmed, which cannot occur inside a UTF-8 multibyte sequence, so a non-ASCII head just stops the loop at once.
- `s[start]`, `s[end - 1]` — std/vyx-hints.vyrn:568,:572 — unit: `UInt8` — compared against `' '`; ASCII-only comparisons, sound for non-ASCII input.
- `let b = s[i]` — std/vyx-hints.vyrn:582 — unit: `UInt8` — interior scan dropping `\t`/`\n`/`\r`; those bytes never appear inside multibyte sequences, so segment cuts never split a character; non-ASCII content passes through untouched.
- `slice(s, from, to)` via `vhSlice` — std/vyx-hints.vyrn:599-604 — unit: byte positions — safe by construction here: every bound is a position this module itself walked on the same string (comment at :597 states this); would silently split characters if ever handed a char index.
- `vhSlice(name, 2, name.byteLength)` in `vhEventOf` — std/vyx-hints.vyrn:654 — unit: bytes — strips the `on` prefix; inputs come exclusively from the fixed ASCII handler lists (:618-650), so byte offset 2 is always the boundary.

**Live byte defects: none.** All four byte-shaped sites are ASCII-delimiter handling whose inputs and bounds are self-consistent; each is correct on non-ASCII input. The risk is latent, not live: `vhSlice` is a byte-position wrapper one careless caller away from splitting characters, and it carries that invariant only as a comment.


## 5. Other smells

By class. Line references come from the per-module tables in section 4.

### Two exports doing the same thing under different names

- `std/json.vyrn:102-104` — `copyJson` is an alias of the existing method `j.copy()`; every caller could write the method form today.
- `std/num.vyrn:701-706` — `asciiStr` re-implements `std/codecs`'s `ascii`; the comment says the duplication is deliberate (leaf module imports nothing).
- `std/strpred.vyrn:64` vs `:124,:151,:290` — two spellings of byte length in one module: `byteLengthV(s)` early, the `s.byteLength` field later.
- Private join copies of `joinWith`: `std/connect.vyrn:71`, `std/rpc.vyrn:95-106`, `std/graphql.vyrn:1925-1936`.
- Private membership copies of `includes`: `std/rpc.vyrn:257-264`, `std/graphql.vyrn:1915-1922`, `std/openapi.vyrn:130-137`; `std/ui.vyrn:424-429` uses `indexOf` where it already imports `includes`.
- `std/http.vyrn:1498-1505` and `:1748-1755` — `httpListHas` and `httpContains` are the same walk written twice inside one module.
- `std/strpred.vyrn:150` vs `std/strings.vyrn:107` — `findPlain` and `indexOf` are the same scan returning `-1` versus `Option<Int64>`.

### A name that does not say what it returns

- `std/hints.vyrn:269-278` — `stringOf` answers `""` for every non-string JSON value; "not a string" and "empty string" are indistinguishable.
- `std/i18n.vyrn:711` — `pluralCategories` returns the sentinel `"__unsupported__"`; callers re-detect it at `:724-732`.
- `std/hash.vyrn:154-157` — `sha1Hex` collapses a decode failure to `""`, which reads as the digest of empty input.
- `std/json.vyrn:185-188` and `std/von.vyrn:320-323,797-800,833-836` — `stringFromBytes` failures become `""` in emitters; an invalid-byte error would masquerade as output.
- `std/vyx-hints.vyrn:81,86` — a failed config read still emits `vhModule(0)`, publishing a clean-looking check count beside an error.
- `std/storage.vyrn:51` — `writeAtomic` returns `Result<Bool, String>` but no caller reads the `Bool`.

### Safe and trapping pair, trapping one with the shorter name

- `slice` (returns `Result<String, SliceError>`, 5 letters) versus `substring` (panics, 9 letters): here the trapping form is the longer name, but it is also the one ten callers reach for, because it spares them the match. The doc at `std/strings.vyrn:74-86` states the trade. Recorded, since the direction the owner asked about is real elsewhere:
- `findSkipping` (safe, returns `-1`) versus `findPlain` (same shape): neither says that both answer in bytes, and the honest-Option twin lives in another module (`indexOf`).

### Argument order that differs between similar functions

- `std/strpred.vyrn:123` and `:178` — `skipTable(needle, haystackBytes)` and `worthPreparing(needle, haystackBytes)` put the needle first, while `startsWith(s, needle)`, `contains(s, needle)` and `findPlain(s, needle, from)` put the haystack first. Two orders in one family.
- `std/contract.vyrn` — `checkContract(iface, c)`, `suppliesMember(iface, c, name)`, `matchedMember(iface, c, name)` lead with either of two defensible subjects (all three classified AMBIGUOUS).

### Three or more arguments of one type, swappable and still compiling

Counted separately, as asked: 5 exports.

- `std/diag.vyrn:41-47` — `report(severity, file, line, col, message)`: adjacent `Int64` `line`/`col` swap silently and misanchor every diagnostic.
- `std/http.vyrn:544` — `event(id: String, name: String, data: String)`: three peer strings, any permutation compiles.
- `std/ui.vyrn:454` — `uiPayload(page, title, props, params)`: four peer strings.
- `std/cli.vyrn:207` — `cliRefused(field, long, want)`: three peer strings.
- `std/cli.vyrn:75` — `cliIssueOf(key, path, message)`: three peer strings.

## 6. What a fix would break

`RECOMMENDATION, NOT A DECISION`

Occurrences of `name(` in `.vyrn` files under `std/`, `site/`, `examples/`,
`docs/`, `web/`, `rfcs/` (worktrees and `target` excluded). The count includes
definitions, doc comments and tests; it is an upper bound on call sites, not
an exact one. Measured with
`grep -rEo "\b<name>\(" --include='*.vyrn' --exclude-dir=worktrees --exclude-dir=target <dirs> | wc -l`.

| export | home module | occurrences |
|---|---|---|
| contains | strpred | 295 |
| substring | strings | 168 |
| startsWith | strpred | 158 |
| trim | strings | 119 |
| toJson | json | 108 |
| joinWith | strings | 103 |
| split | strings | 59 |
| emit | json | 55 |
| indexOf | strings | 53 |
| map | arrays | 33 |
| endsWith | strpred | 33 |
| slice | strpred | 33 |
| replace | strings | 29 |
| includes | arrays | 24 |
| chars | text | 16 |
| filter | arrays | 14 |
| sortBy | arrays | 3 |
| fold | arrays | 2 |
| any | arrays | 1 |
| all | arrays | 0 |
| **total** | | **1306** |

Reading: a subject-first rename of the twenty most-used `SUBJECT AS
ARGUMENT` exports touches roughly thirteen hundred occurrences, most of them
mechanical `f(x, ..)` to `x.f(..)` rewrites. The long tail is cheap: eleven
of the twenty have 60 or fewer occurrences. The expensive core is
`contains`/`startsWith`/`endsWith`/`substring`/`trim` alone, at about 800.
A mechanical codemod over `\bname\(` would carry most of it; the residue is
doc comments and prose, which a compiler-checked rename would leave alone.

The byte-unit question is separate from the call-shape question and cheaper
to answer first: adding character-safe siblings (`charAt`, a character
slice, a character-width pad) breaks nothing, and every trap in section 2
stays open until those exist.


---

## Correction, made on verification

Four of the twelve live defects in section 3 are not defects. Items 4, 5, 6 and
7 report that `std/scan`, `std/jsonread` and `std/vyx` advance a column once per
byte, so a column is wrong on any line holding non-ASCII.

The behaviour is real. The verdict is wrong. **Byte columns are the documented
convention, chosen deliberately and measured against the compiler.**
`std/text.vyrn:204-210` states it:

> **The column counts BYTES, not codepoints**, and that was measured off the
> builtin rather than assumed: both the interpreter (`off - lineStart + 1`) and
> the shim (a backward walk to the LF) count bytes [...] RFC-0033 origin
> directives feed a C-style `#line`, where byte columns are the convention
> anyway.

`compiler/vyrn-codegen/src/direct.rs:14585` carries the same statement on the
other side. So `std/scan.vyrn:150` and `std/jsonread.vyrn:86` counting bytes are
CONSISTENT with the language they serve, not broken. Changing them to count
characters would put the standard library at odds with the interpreter and with
the `#line` directives the origin machinery emits.

**Item 7 is the one real defect in that group, and it is a documentation defect.**
`vyxColAt` was documented as "chars since the last LF" while returning
`colAt`'s byte column. The census found it at the exact address `std/text.vyrn`
predicts. Fixed in this commit: the doc comment now says bytes and points at the
measurement.

That leaves 8 live defects, not 12. The remaining eight are unaffected: they
drop or mis-fold non-ASCII bytes while building an identifier or padding a
column, which no convention makes correct.

### Why this matters beyond the count

A census cannot tell a deliberate convention from an accident by reading the
code alone. Four sites did the same thing consistently, which reads as a
repeated defect and is in fact a followed rule. The distinguishing evidence was
a doc comment in a fourth file and a comment in the compiler. Any future census
of this kind should be told to search for a stated convention before reporting
consistency as a fault.

## Re-adjudication of all twelve

Every item in section 3 was re-read against the rest of the repository. Eight
are withdrawn. Each was withdrawn for one of two reasons, and both reasons are
the same mistake: the census read one function and did not look for the file
that handles its consequence.

| # | site | verdict | why |
| --- | --- | --- | --- |
| 1 | `std/symbolmap.vyrn:93` | **REAL — fixed** | Builds a program-wide symbol, drops bytes above 0x7F, and nothing checks the result. Its own doc says it exists to prevent exactly this collision. |
| 2 | `std/icons.vyrn:903` | withdrawn | `camel` is lossy, and `std/icons.vyrn:413` catches it: "`{want}` and an earlier glyph in this import both become `{fnName}()`". Detected, named, and reported. |
| 3 | `std/bench.vyrn:102` | cosmetic | `padRight` pads to a byte width, so a bench label with a multi-byte character misaligns one column. No correctness effect. The doc comment already says it. |
| 4 | `std/scan.vyrn:144` | withdrawn | Byte columns are the documented convention. See the correction above. |
| 5 | `std/jsonread.vyrn:77` | withdrawn | Same convention. |
| 6 | `std/vyx.vyrn:2450` | withdrawn | Same convention; the code comment says "byte offset" in so many words. |
| 7 | `std/vyx.vyrn:229` | **REAL — fixed** | Documentation defect: promised characters, returned bytes. |
| 8 | `examples/lib/gen_table.vyrn:63` | withdrawn | Derives a column from byte offsets, which is the convention. |
| 9 | `std/strings.vyrn:320` | cosmetic | `padStart`/`padEnd`, the public surface of item 3. |
| 10 | `std/strings.vyrn:376` | cosmetic | `editDistance` over bytes. Affects the ranking of a "did you mean" suggestion, not whether one is offered. |
| 11 | `std/strings.vyrn:111` | withdrawn | `indexOf` returns a byte offset because a Vyrn String is UTF-8 bytes by definition. That is the string model, not a defect in a function. |
| 12 | `std/ui.vyrn:714` | withdrawn | `uiSegIdent` is documented MANY-TO-ONE, and `uiHelperCollisions` at `std/ui.vyrn:1669` compares every pair of routes and raises an Error naming both. |

**Two real defects, both fixed. Three cosmetic. Seven withdrawn.**

### What this says about the census method

The prompt asked for callers that already hold the bug, and the subagents found
functions that lose information. Those are not the same question. A function may
lose information safely when a second function refuses the result — which is
what `std/icons` and `std/ui` both do, each with a message written for the
developer who hits it.

A future census of this kind must be required to answer, for every lossy
function it reports: what refuses the bad result, and where? An item with no
answer is a finding. An item whose answer is a file and a line is not.

The one that survived that test, `mapSlug`, survived it exactly: nothing refuses
its result, and its own documentation says something must.
