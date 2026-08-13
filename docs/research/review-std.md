# Review: the standard library, second pass, five lenses

An external review of the Vyrn STANDARD LIBRARY (`std/*.vyrn`) at `aa36ce4`.
Five reviewers — Linus Torvalds, a C systems reviewer, a Rust reviewer, an Agda
implementer, a PL/product theorist — each hunting what only that reviewer would
find. This pass covers what `docs/research/audit-std.md` (the first std audit,
at `52e462f`) did not reach: `std/http`, `std/rpc`, the readers
(`std/jsonread`, `std/jsondec`), `std/num`, `std/openapi`, `std/connect`, and
the small modules, plus `std/i18n` and `std/tw` beyond their settled bugs.

Every finding carries evidence. A code finding cites `file:line`. A soundness
finding carries a program that was RUN, with its output recorded. Findings are
ranked **CONFIRMED** (reproduced or measured) above **PLAUSIBLE** (argued from
reading). Where an RFC or a leading `///` doc records a decision with its
argument, the entry says "design critique", not "defect".

Generated SDL was validated with `graphql-js` 16 (`parse` +
`buildASTSchema`/`validateSchema`), not by eye. Probes ran against this
worktree's `std/` via `VYRN_STD`, with a `compiler/target/release/vyrn.exe`
freshly built at `dc4c929`.

---

## Status of the earlier-reported issues

Each earlier finding was re-verified by running a probe against current `std/`,
not carried forward on trust.

| Earlier finding | Status | Evidence |
|---|---|---|
| `std/graphql`: interior `"""` in a doc folded into the block string unescaped; trailing `"` abutting the close (2026-07-18 census) | **FIXED** | Probe: a type documented `/// A doc with an interior """ triple quote and a trailing quote "` → `graphql-js`: `PARSE OK … SCHEMA VALID`. `gqlEscTripleQuote` and the own-line rule in `gqlDescBlock` hold. |
| `std/graphql`: `gqlSplitTop` string-blind (phantom/dropped fields on `,`/`}` inside predicate strings) | **FIXED** | Probe: `type Url = String where startsWith(value, "http://a,b}c")` plus a field `note: String where startsWith(value, "n,{}")` → `PARSE OK … SCHEMA VALID`. |
| `std/graphql`: zero-field record emits `type X {}` / `input XInput {}` (first std audit S4.1) | **FIXED in #169** (`aa36ce4`) | Re-probed: the empty body carries `_placeholder: Boolean` through the new `gqlBlock` funnel; `type Empty {}` gone; `graphql-js`: `PARSE OK … SCHEMA VALID`. |
| `std/graphql`: contract type named `Query` emits two `type Query` blocks (first std audit S4.2) | **FIXED in #169** | Re-probed on a fresh binary: `vyrn run` fails the load with `the GraphQL document would define \`Query\` twice — the contract's type \`Query\`, and the generated query root …` (RFC-0099 `Error` via `gqlDefine`). Also covers `Mutation`, `JSON`, `Foo`/`FooInput`, and `__`-prefixed names. |
| `std/json` `JNum` / `std/von` `VInt`/`VFloat` unvalidated; VON emits names verbatim (first std audit S3.1/S5.1) | **FIXED in #169** | `emit(JNum("NaN"))` now traps: `json: \`NaN\` is not a usable number …` (`std/json.vyrn:248`). `std/von` gained `requireName` on record/field/variant/imported-type names (`von.vyrn:835`) and `emitNumber` gating `VInt`/`VFloat` (`von.vyrn:886`), matching `std/html`'s check-the-name model. Hostile-key JSON round-trips: `emit`→`parseJson` over a key holding `"` and a value holding `\` returns `reparse ok`. |
| `std/tw`: breakpoint key bypasses validation (regex forgery); leaf values baked unescaped (CSS injection) | **FIXED** | Single gate `twSheetSafetyErrors` (`tw.vyrn:775`) runs before any CSS/token grammar is emitted (`twBuildModule` `:792`); `twCssValueSafe` (`:212`) = single-token check via `std/scan` AND narrow hex/length/ident grammar; breakpoint keys checked by `twUnsafeBreakpointErrors` (`:614`). Pinned by tests `:1035`, `:1045`. |
| `std/i18n`: lone apostrophe unconditionally toggled ICU quote mode; key collisions undetected | **FIXED** | `icuApostrophe` (`i18n.vyrn:316`) is the single lenient-ICU rule (`''` always literal; a lone `'` quotes only before `{`/`}`/`#`); every scanner (`readLiteral` `:335`, `matchBrace` `:366`, `splitTopCommas` `:396`, `branchBlob` `:594`) reads it. `I18N_DUP_KEY__`/`I18N_KEY_COLLISION__` diagnostics exist. |
| `std/vyx`: template scanner reads HTML as code; `@event` guard counts `<`/`>` as brackets. `std/ui`: hyphenated filename bricks the router | **FIXED in PR #168** (merged): scanner reads markup by markup rules; slugs fold to camelCase with an RFC-0099 diagnostic on collision. Out of this pass's scope; not re-probed beyond confirming the merge. |
| D1 nested-array-literal native codegen bug (2026-07-18 census) | **FIXED** earlier (28dfcc9..2ce1aa3, ragged-literal parity pins); compiler-side, out of std scope. |

---

## Top findings by severity

| # | Severity | Reviewer | Finding | Ref |
|---|---|---|---|---|
| 1 | High | Agda | **`std/graphql`: a `///` doc ending in a backslash makes the emitted block string swallow the definitions after it.** `"""…\"""` — the trailing `\` plus the closing quotes IS GraphQL's one block-string escape, so the string never closes there; `graphql-js` refuses the document. Independent of the two holes #169 fixed. | S4.1 |
| 2 | Medium | Agda | **`std/http` `event()` lets `id`/`name` inject arbitrary SSE fields and drops a `\r` from `data`.** A newline in `id` or `name` writes extra `event:`/`data:` lines; a carriage return in `data` (an SSE line terminator) survives raw and truncates the payload at the client. The escaping the doc reasons about covers `\n`-in-`data` only. | S4.2 |
| 3 | Medium | Rust | **`std/jsonread` `parseJson` TRAPS on a ~500-deep document instead of returning `Err`.** Its type is `Result<Json, String>`, but a nested array trips the engine's 1000 call-depth cap; the trap is uncatchable, so a server decoding an untrusted body with it (every `std/http`/`std/rpc` handler) has a ~9 KB DoS. Both engines agree, so parity hides it. | S3.1 |
| 4 | Low | Linus | **Every generator still hand-rolls the byte helpers `std/strings` exports** — `gqlTrim`/`gqlIndexOf`/`gqlSlice` (34 uses in `graphql.vyrn` alone) duplicate `trim`/`indexOf`/`substring`. Carried from the first audit (S1.1); still present at `aa36ce4`. | S1.1 |

---

## Lens 1 — Linus Torvalds: taste and performance

### S1.1 PLAUSIBLE — Low. The generators still reimplement the string library (carry-forward)

`std/strings` exports `trim` (`strings.vyrn:253`), `indexOf` (`:111`),
`substring` (`:87`). `std/graphql` carries its own `gqlTrim`, `gqlIndexOf`,
`gqlSlice`, used 34 times across the file; `std/tw`, `std/von`, `std/i18n`,
`std/openapi` each carry the same family. Some copies are genuinely different
(the brace/quote-aware `gqlSplitTop`); the plain ones are not. This is the
first audit's S1.1, re-confirmed unchanged at `aa36ce4`. The measured
non-issue also still holds: the `out = out + x` emitter accumulator is LINEAR
in both engines (Vyrn's `+` appends in place at refcount 1), so it is not a
performance defect — only the duplication is.

**Fix.** Lift byte-level `indexOf`/`slice`/`trim` over `Array<UInt8>` into
`std/strings` (or `std/scan`) and delete the per-file copies; keep the
genuinely different scanners.

---

## Lens 2 — C systems: ownership and resource discipline

### What this lens found clean

- **The recursive value trees still carry their `Owned` bottom.** `Json`
  (`json.vyrn:87`), `Von` (`von.vyrn:97`) and `GqlSel` each declare
  `impl Owned`/`release`, the declared floor the structural-release walk needs
  for a self-referential type; #169 touched the writers, not these, and did not
  regress them.
- **`std/http`'s framing helpers move, not copy.** `event()` (`http.vyrn:520`)
  and the SSE/WS builders accumulate into one `String`; no per-byte
  allocation was found on the emission path. The `event()` defect (S4.2) is a
  correctness bug, not a resource one, and is filed under the Agda lens.

---

## Lens 3 — Rust reviewer: correctness and API taste

### S3.1 CONFIRMED — Medium. `parseJson`'s type promises `Err`; a deep document delivers an uncatchable trap

`std/jsonread` is recursive descent: `parseValue` (`jsonread.vyrn:292`) →
`parseArray` (`:328`) → `parseValue`, with no depth budget of its own. Its
signature is `parseJson(src) -> Result<Json, String>` (`:421`) — the whole
point of which is that a malformed or hostile document comes back as `Err`.
It does not for a deeply nested one: the recursion trips the engine's 1000
call-depth cap (added in #167) and traps the process.

**Repro.** `deepjson2.vyrn` reads `n.json` and `match`es on `parseJson`:

```vyrn
import { parseJson } from "std/jsonread"
fn main() -> Int64 {
    let s = match readFile("n.json") { Ok(v) => v, Err(e) => "" }
    match parseJson(s) {
        Ok(j) => print("parsed ok"),
        Err(e) => print("err: " + e),
    }
    return 0
}
```

**Recorded output**, `n.json` = `[` × depth + `]` × depth:

```
depth=400 -> parsed ok
depth=500 -> error: call depth exceeds 1000
depth=2000 -> error: call depth exceeds 1000
```

The threshold sits between 400 and 500 nesting levels (~2 interpreter frames
per level plus overhead). The `Err(e)` arm never runs — the trap bypasses the
`match`. Interpreter and native agree (both `error: call depth exceeds 1000`,
exit 1), which is why the parity harness cannot see it: it is not a
disagreement, it is both engines breaking the same API contract.

**Blast radius.** `std/http` and `std/rpc` decode untrusted request bodies with
`parseJson` (`http.vyrn:1032` `httpSingle`, `http.vyrn` payload readers; the
RPC handler's `fromJson`). A ~9 KB document nested ~460 deep takes the handler
down with an uncatchable trap rather than a 400. This is a trivial
denial-of-service reachable from ordinary input, on a function whose type says
it cannot happen. The same recursive-descent shape without a self-imposed depth
budget appears in `std/jsondec`, `std/von`'s reader, and `std/graphql`'s
`gqlParseQuery`, all of which run in the served process over client input.

**Fix.** Give the readers an explicit depth counter that returns
`Err("maximum nesting depth exceeded")` well below the engine cap, so the
`Result` the type advertises is the one the caller gets.

### What this lens found clean

- **`parseInt64`/`parseUInt64` are overflow-honest.** `num.vyrn:415`/`:460`
  accumulate in `UInt64` and compare against the magnitude limit *before*
  multiplying (`acc > 1844674407370955161`, then `acc > limit - dig`), and
  handle `Int64.min`'s asymmetric magnitude through the bit pattern
  (`num.vyrn:452`). No wrap, no off-by-one at the boundary.
- **#169's writer checks are total.** `emit(JNum("NaN"))`, `JNum("")`,
  `JNum("01+bad")` all trap with a message naming the number grammar; the
  hostile-key round-trip (`"` in a key, `\` in a value) reparses. The first
  audit's S3.1 is closed.

---

## Lens 4 — Agda implementer: soundness of the generators

### S4.1 CONFIRMED — High. A doc comment ending in `\` corrupts the whole SDL document

**Not the #169 cases.** PR #169 fixed the zero-field record and the duplicate
name; both re-verified fixed above. This is a THIRD, independent hole in the
same emitter — in `gqlDescBlock`'s escaping, not the member loops or the name
registry — and it reproduces at `aa36ce4` with a one-field record and no
reserved name anywhere.

**The rule it breaks.** GraphQL block strings have exactly one escape sequence:
`\"""` (spec, BlockStringCharacter). `gqlDescBlock` (`graphql.vyrn:352`) guards
a body that ENDS in `"` (own-line form) and `gqlEscTripleQuote` (`:362`)
escapes interior `"""` runs — but a body ending in a BACKSLASH is emitted as
`"""<body>\"""`, and the parser reads that trailing `\"""` as the escape, not
the close. The block string runs on, swallowing the SDL that follows until the
next `"""` in the document.

**Repro.** Contract (`api1.vyrn`) whose doc's last character is a backslash:

```vyrn
/// Ends with a backslash \
export type Thing = { name: String }

export fn getThing() -> Thing {
    return Thing { name: "x" }
}
```

Driver: `import { sdlText } from sdl("./api1")` / `print(sdlText())`.

**Recorded output** (`vyrn run`, std at `aa36ce4`), the poisoned line:

```
"""Ends with a backslash \"""
type Thing {
  name: String!
}
```

`graphql-js` on the full document:

```
PARSE ERROR: Syntax Error: Unexpected description, only GraphQL definitions support descriptions.
```

`type Thing` and `input ThingInput` are swallowed into the unterminated block
string; the next `"""` in the document (a later description opener) closes it,
and parsing fails there. A contract whose doc comment ends in `\` — a Windows
path, a line-continuation habit, a TeX fragment — emits a document no GraphQL
parser accepts.

**Fix.** `gqlDescBlock` guards a trailing `"` but not a trailing `\`. A body
ending in `\` needs the same own-line treatment plus assurance that the last
byte before `"""` is never a lone `\` (e.g. append a newline after a
backslash-terminated body, the `printBlockString` shape).

### S4.2 CONFIRMED — Medium. `std/http` `event()` injects SSE fields from `id`/`name` and drops `\r` from `data`

`event(id, name, data)` (`http.vyrn:520`) builds an SSE frame. Its doc reasons
carefully about ONE hazard — a `\n` inside `data` would end the field — and
splits `data` on `\n` into multiple `data:` lines. It does nothing for (a) a
`\n` in `id` or `name`, and (b) a `\r` anywhere. Per the WHATWG EventSource
grammar a line is terminated by CR, LF, or CRLF, so all three are hazards.

**Repro.** `sse1.vyrn`:

```vyrn
import { event } from "std/http"
fn main() -> Int64 {
    print(event("1\nevent: hijack\ndata: pwned", "msg", "hello"))
    print("---")
    print(event("2", "msg", "line-a\rinjected"))
    print("---")
    print(event("3", "msg\ndata: x", "hi"))
    return 0
}
```

**Recorded output** (`cat -A`, `$` = LF, `^M` = CR):

```
id: 1$
event: hijack$
data: pwned$
event: msg$
data: hello$
$
---
id: 2$
event: msg$
data: line-a^Minjected$
$
---
id: 3$
event: msg$
data: x$
data: hi$
$
```

Frame 1: a newline in `id` injected a whole second event (`event: hijack` +
`data: pwned`) ahead of the real payload. Frame 3: a newline in `name` injected
a `data: x` line. Frame 2: the raw `^M` (CR) survives — the client splits
`data: line-a` from `injected`, so the payload is truncated to `line-a` and the
rest is read as a stray (colon-less, ignored) line. A doc comment that reasons
explicitly about newline safety in `data` invites callers to trust the function
is injection-safe; it is not for `id`, `name`, or CR.

The dogfood (`examples/bin/server/api/pastes.http.vyrn:49`) passes
`toJson(...)` as `data`, and the JSON writer escapes control bytes, so it is
safe there — but `event` is a public export and the hazard is one untrusted
`id`/event-name or one non-JSON payload away.

**Fix.** Reject (or strip) CR and LF in `id`/`name`, and split `data` on CR and
CRLF as well as LF (or refuse CR outright, the way `std/html` refuses an unsafe
name rather than trying to escape it).

### What this lens found clean

- **The `toJson`-tree emitters are valid by construction.** `std/connect`
  (`connect.vyrn:207`) and `std/openapi` (`openapi.vyrn`) build their bodies
  through `toJson`/a `std/json` `Json` tree and identifier-only keys
  (`operationId`, `$ref`, `/rpc/<name>`), so a malformed wire document has no
  path there; no hostile input reproduced an invalid artifact.
- **`std/tw`'s CSS value choke point holds** (re-confirmed): every leaf value
  flows through `twCssValueSafe`, and the settled breakpoint/injection holes
  stay closed.

---

## Lens 5 — PL / product theory: coherence and round-trip laws

### S5.1 CONFIRMED — Medium (coherence). The readers are total in name only

The reader modules present a `Result`-returning, total-parser interface —
`parseJson(src) -> Result<Json, String>`, `std/von`'s reader, `std/jsondec` —
whose contract is "every input maps to `Ok` or `Err`, never a crash". That
contract holds for shallow input and breaks for deep input, where the parse
traps instead (S3.1). The coherence objection is that the type is the promise a
consumer reads: `std/http` wraps `parseJson` in a `match … Err(e) =>` precisely
because it believes the parser total, and that belief is false for ~500-deep
input. Filed here as the contract violation it is, and in S3.1 as the reachable
trap it comes from.

### What this lens found clean

- **The JSON writer/reader round-trip law holds for the hostile inputs #169
  added.** `emit`→`parseJson` reparses a key holding `"` and a value holding
  `\`; the number and name gates make an un-round-trippable tree a trap at
  `emit` time rather than a silent malformed document. VON gained the same
  gates. The first audit's S3.1/S5.1 pair is closed.

---

## What this pass did not cover

`std/rpc` (61 KB) and `std/http` (64 KB) were read for their emission and wire
surfaces and probed at `event()` and the JSON-decode path; their full mount/ETag/
WebSocket-handshake logic was read, not exhaustively fuzzed. `std/openapi` and
`std/connect` were judged valid-by-construction from reading, not validated
against a real OpenAPI/Connect validator. `std/num`'s float formatting
(`f64Str`) was read but not differentially tested against `%f` over a large
sample. `std/cli`, `std/slots`, `std/stream`, `std/text`, `std/scan`,
`std/strpred`, `std/hash`, `std/bench`, `std/args`, `std/arrays`, `std/time`,
`std/random`, `std/storage`, `std/math`, `std/fallible`, `std/diag`,
`std/symbolmap`, `std/codecs` were sampled, not swept — the first audit's
`std/codecs` byte-encoder/decoder asymmetry (S5.2) was not re-examined and is
presumed unchanged. `std/vyx` and `std/ui` were out of scope (fixed in #168).
No fuzzer was run; every input here was hand-written. Severity is this review's
judgement.
