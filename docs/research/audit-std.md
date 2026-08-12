# Audit: the standard library through five lenses

An external review of the Vyrn STANDARD LIBRARY (`std/*.vyrn`) at `52e462f`. Five
reviewers — Linus Torvalds, a C systems reviewer, a Rust reviewer, an Agda
implementer, a PL/product theorist — each hunting what only that reviewer would
find. This is the library dogfooding itself: 34 modules, the two largest 202 KB
each, almost all of it comptime-pure Vyrn.

Every finding carries evidence. A code finding cites `file:line`. A soundness
finding carries a program that was RUN, with its output recorded. Findings are
ranked **CONFIRMED** (reproduced or measured) above **PLAUSIBLE** (argued from
reading). Where an RFC or a checked-in example records a decision with its
argument, the entry says "design critique", not "defect".

The prior audit (`docs/research/audit-linus-to-agda.md`) and the July census
were read first. Their std-library findings were re-checked: the `std/html`
name-injection (`attr("src\" onload=..")`), the `std/tw` breakpoint soundness
hole, and the `std/i18n` ICU apostrophe/duplicate-key bugs are all **FIXED** and
are recorded as such below, not re-reported.

Timings and outputs are from `compiler/target/release/vyrn.exe`, run interpreted
unless a native build is named.

---

## Top 7 by severity

| # | Severity | Lens | Finding | Ref |
|---|---|---|---|---|
| 1 | **High** | Agda | **`std/graphql` `sdl` emits INVALID SDL for ordinary contracts.** A zero-field record → `type X {}` / `input XInput {}` (an object must define ≥1 field); a contract type named `Query` → two `type Query {}` blocks. Both run, both break every GraphQL parser. | S4.1, S4.2 |
| 2 | **High** | Agda | **`std/vyx`'s `</template>` scanner reads the HTML body as CODE.** An odd `"` in template text (`a 6" nail`), or a single-quoted attribute holding `"` (`value='2"'`, a feature the file claims to support), makes the scanner walk past the real close → false `VYX_NO_TEMPLATE`. `</template>` inside an HTML comment truncates the section. | S4.3 |
| 3 | High | Agda | **`std/ui` `pages()` emits a malformed Vyrn router for an ordinary hyphenated filename.** `pages/about-us.vyrn` bakes an export named `hrefAbout-us`; the generated module fails to parse (`expected LParen, found Minus`). `sign-in`, `terms-of-service` — every slug with a `-` bricks the router. | S4.4 |
| 4 | Medium | Rust / PL | **`std/json` `JNum` and `std/von` `VInt`/`VFloat` are public, unvalidated `String` constructors,** and `emit`/`emitVon` copy them out verbatim — invalid JSON/VON from ordinary calls. `std/von` also emits record/field/variant NAMES verbatim, with no `requireName` analog the way `std/html` has one. | S3.1, S5.1 |
| 5 | Medium | PL | **`std/codecs` has a byte encoder with no byte decoder.** `base64EncodeBytes(Array<UInt8>)` takes any bytes, but the only inverse `base64Decode` (and `hexDecode`, `urlDecode`) returns `Option<String>` and silently `None`s any output that is not valid UTF-8 — 129 of 256 single-byte payloads. Text-only intent is documented, so this is coherence, not a defect: the round-trip law fails for the inputs the byte-encoder accepts. | S5.2 |
| 6 | Low/Med | Rust | **`std/vyx`'s `@event` multi-arg guard counts `<`/`>` as bracket depth.** `@click="pick(a > b, c)"` drives depth negative, the `VYX_NON_SCALAR_EVENT_ARG` diagnostic is skipped, and the generator emits invalid Vyrn (`expected RParen, found Comma`). | S3.2 |
| 7 | Low | Linus | **Every generator hand-rolls the byte helpers `std/strings` and `std/scan` already export** — `gqlTrim`/`gqlIndexOf`/`gqlSlice` duplicate `trim`/`indexOf`/`substring`, once per file. | S1.1 |

Below the line and worth naming: the recursive value trees (`Json`, `Von`,
`GqlSel`) all carry explicit `Owned`/`release` impls, so the structural-release
walk the C lens went looking for a leak in is closed by construction (clean);
and the `out = out + x` accumulator that pervades every emitter is LINEAR in both
engines, not quadratic (measured) — Vyrn's `+` appends in place when the left
operand's refcount is 1.

---

## Lens 1 — Linus Torvalds: taste and performance

### S1.1 PLAUSIBLE — Low. Every generator reimplements the string library

`std/strings` exports `trim` (`strings.vyrn:253`), `indexOf` (`:111`), `split`
(`:162`), `substring` (`:87`); `std/scan` is the shared, comment-aware cursor
(RFC-0054). Yet `std/graphql` carries its own `gqlTrim` (`graphql.vyrn:95`),
`gqlIndexOf` (`:197`), `gqlSlice` (`:79`), `gqlStripBang` (`:110`),
`gqlBetweenBraces` (`:532`), `gqlBetweenParens` (`:581`). `std/tw`, `std/von` and
`std/i18n` each carry the same family. Some of it is load-bearing —
`gqlSplitTop` (`:148`) is brace-depth-and-quote aware, which `split` is not — but
`gqlTrim`/`gqlIndexOf`/`gqlSlice` are the plain versions with a prefix. This is
the "four copies of one table" shape the compiler audit found in the builtin
surface (L1.8), one layer up.

**Fix.** Lift the byte-level `indexOf`/`slice`/`trim` to `std/strings` (or
`std/scan`) as the `Array<UInt8>` primitives the generators actually want, and
delete the per-file copies. Keep only the genuinely different scanners
(`gqlSplitTop`).

### What this lens found clean

- **No quadratic string building.** The `out = out + x` loop in every emitter
  (`json.vyrn:192`, `von.vyrn:868`, `graphql.vyrn:512`, `tw.vyrn:713`) is linear:
  `emit` of an N-element `JArr` measured 45 / 98 / 191 / 381 ms interpreted and
  0.45 / 0.87 / 1.73 / 3.42 ms native for N = 2k / 4k / 8k / 16k — 2x per
  doubling both ways. Vyrn's `+` appends in place when the accumulator's refcount
  is 1, so the reference audit's "concatenation always allocates" (C2.3) does not
  make this quadratic. The pattern is fine; leave it.

---

## Lens 2 — C systems: ownership, consume and region discipline

The recursive value trees are where a code-generating library allocates most,
and they are where a structural-release walk would silently leak. They do not.

### What this lens found clean

- **`Json`, `Von` and `GqlSel` each declare `impl Owned`/`release`** (`json.vyrn:88`,
  `von.vyrn:97`, `graphql.vyrn:795`), the declared bottom the structural walk
  needs for a self-referential type. `JsonField`/`VonField`/`VonEntry` get their
  row back through them. The one honest gap is recorded in the code itself:
  `graphql.vyrn:794` notes `JsonField` reaches `Json`, "which declares nothing,
  so its elements are the leak `std/json` still carries" — a known, written
  deferral, so it is a `ponytail:`-style note, not a finding.
- **The escapers append into one `Array<UInt8>` and convert once.**
  `html.escapeText` (`html.vyrn:235`), `json.emitString` (`json.vyrn:145`) and
  `von.emitString` (`von.vyrn:777`) build a byte buffer and call
  `stringFromBytes` a single time. `appendBytes` (`html.vyrn:226`) takes
  `consume`, so the growing buffer is moved, not copied. No per-byte allocation.

The Rust and PL lenses carry the two real defects this library has in the
"value that should be a type" and "round-trip" categories (S3.1, S5.1, S5.2);
they are correctness bugs before they are memory bugs, and are filed there.

---

## Lens 3 — Rust reviewer: correctness and API taste

### S3.1 CONFIRMED — Medium. `JNum` / `VInt` / `VFloat` are unvalidated `String`, and `emit` copies them out verbatim

`std/json`'s value tree is `JNum(String)` (`json.vyrn:35`); `emit`'s number arm
is `JNum(raw) => raw.copy()` (`json.vyrn:224`). The doc claims the string is
"RAW, validated number text" (`json.vyrn:29`), but the constructor is a public
exported enum variant with no smart constructor, so the invariant is unenforced.
Same in `std/von`: `VInt(s) => s.copy()`, `VFloat(s) => s.copy()`
(`von.vyrn:955-956`), and the record/variant/field NAMES are emitted verbatim too
(`emitRecordBlock` `von.vyrn:858` writes `name` and `f.name` with no check),
where `std/html` learned to CHECK a name rather than escape it (`requireName`,
`html.vyrn:303`).

Repro (`z_json.vyrn`):

```vyrn
import { Json, JsonField, emit } from "std/json"
fn main() -> Int64 {
    let bad = JObj([
        JsonField { key: "x", value: JNum("NaN") },
        JsonField { key: "y", value: JNum("") },
        JsonField { key: "z", value: JNum("01+bad") },
    ])
    print(emit(bad))
    return 0
}
```

```
$ vyrn run z_json.vyrn
{"x":NaN,"y":,"z":01+bad}
```

Not JSON on any of three counts. The `std/von` twin (`z_von.vyrn`):

```vyrn
let v = VRecord("Bad Name", [VonField { name: "amount", value: VInt("}injected: true, x: {"), line: 0 }])
print(emitVon(v))
```

```
$ vyrn run z_von.vyrn
Bad Name {
    amount: }injected: true, x: {,
}
```

The record name has a space, and the `VInt` payload injects `}` and `{`. `VStr`
and map keys ARE escaped (`von.vyrn:896`), so a reader who trusts the type sees
strings handled and assumes the tree is safe; the number variant and the
structural names are the hole.

**Blast radius.** Inside `std` these variants are only ever fed integer
`toString()` (`http.vyrn:1297`, `symbolmap.vyrn:53`), which is safe today. The
defect is that the type lets any caller — a generator built on top of these
trees, or `emit`/`emitVon` called on a tree assembled from untrusted data —
produce a malformed document with no error.

**Fix.** A smart constructor `jnum(text) -> Option<Json>` (or a `Number` newtype
validated at the boundary) and a `requireName` on the VON structural names, the
way `std/html` already draws the value/name line.

### S3.2 CONFIRMED — Low/Medium. `std/vyx`'s `@event` arg guard treats `<`/`>` as brackets

`vyxHasTopComma` (`vyx.vyrn:2112`), used by `vyxEmitEvent` (`vyx.vyrn:2084`) to
refuse a multi-argument event handler, tracks nesting by counting `<` and `>` as
open/close. A comparison operator `>` drives the depth negative, so a top-level
comma after it is not seen, the `VYX_NON_SCALAR_EVENT_ARG` diagnostic is skipped,
and the generator emits invalid Vyrn.

```
Input:  <button @click="pick(a > b, c)">go</button>
emit-gen: expected RParen, found Comma   (in the GENERATED code)
```

The failure is closed (the generated module does not compile), not a silent
miscompile, which is why it ranks below S4.3. Root cause: `<`/`>` are comparison
operators in an expression, not brackets. **Fix.** Track only `(` `)` `[` `]`
`{` depth in an expression context.

### What this lens found clean

- **`std/i18n` ICU apostrophe handling is now single-sourced and correct.**
  `icuApostrophe` (`i18n.vyrn:316`) is the one rule every quote-aware scanner
  reads: `''` is always one literal apostrophe, a lone `'` opens quoting only
  before a syntax char, a `'` while quoting closes it, any other `'` is a literal.
  The prior "apostrophe" bug is fixed. Duplicate keys are rejected by the strict
  reader (`i18n.vyrn:193`), closing the prior key-collision bug.

---

## Lens 4 — Agda implementer: soundness of the generators

The brief: feed a generator hostile-but-valid input and show the emitted
artifact is invalid in its target grammar. This is the highest-value lens for a
code-generating stdlib, and it found the three worst defects.

### S4.1 CONFIRMED — High. `std/graphql` `sdl` emits an empty object type

A GraphQL object type "must define one or more fields" (spec §3.6); an input
object the same (§3.10). `gqlObjectSdl` (`graphql.vyrn:512`) and `gqlRecord`
(`:523`) loop over the members and emit `type Name {` … `}` with no guard for the
empty case — while the Query root two functions down IS guarded, with a
`_placeholder: Boolean` when it has no fields (`graphql.vyrn:734`). A zero-field
record slips through the unguarded path.

Contract (`zc_contract.vyrn`) — ordinary, compiles and runs:

```vyrn
export type Empty = {}
export type Wrap = { inner: Empty, label: String }
export fn getWrap() -> Wrap { return Wrap { inner: Empty {}, label: "hi" } }
```

Driver (`zc_driver.vyrn`): `import { sdlText } from sdl("./zc_contract")`, print it.

```
$ vyrn run zc_driver.vyrn
...
"""A tiny GraphQL contract with a zero-field record type."""
type Empty {
}
input EmptyInput {
}
type Wrap {
  inner: Empty!
  label: String!
}
...
```

`type Empty {}` is a syntax error in every GraphQL parser (`graphql-js`: "Syntax
Error: Expected Name, found }"). One empty record poisons the whole document,
because `Wrap.inner: Empty!` references it.

**Fix.** Guard `gqlObjectSdl`/`gqlRecord` the same way the Query root is guarded,
or refuse a zero-field record at the reflection boundary.

### S4.2 CONFIRMED — High. `std/graphql` `sdl` emits a duplicate `type Query`

The generator always writes a root `type Query` (`graphql.vyrn:733`) and also
emits one `type Name` per contract type (`:719`). A contract type named `Query`
therefore appears twice.

Contract (`zc_contract2.vyrn`):

```vyrn
export type Query = { hits: Int64 }
export fn stats() -> Query { return Query { hits: 1 } }
```

```
$ vyrn run zc_driver2.vyrn
...
type Query {
  hits: Int!
}
input QueryInput {
  hits: Int!
}
type Query {
  stats: Query
}
```

Two `type Query` definitions — "There can be only one type named Query" — and
`stats: Query` now names two different types. Same class of bug as an author type
called `Mutation`. **Fix.** Reserve `Query`/`Mutation`/`JSON`/the built-in scalar
names at the reflection boundary and refuse (or rename) a colliding author type.

### S4.3 CONFIRMED — High. `std/vyx`'s `</template>` scanner reads the HTML body as code

The end of a `<template>` section is located by `vyxScanFindCode(.., codeMode=false)`
(`vyx.vyrn:306`, via `vyxSectionAvoid` `:992`, called at `:1461`). That walk skips
`"…"` as string literals and does not skip `<!-- … -->` HTML comments — correct
for Vyrn code, wrong for an HTML body. Two confirmed consequences:

- An odd number of `"` in template text opens a never-closed "string" and the
  scanner walks past the real `</template>`:
  `<template>\n  <p>a 6" nail</p>\n</template>` →
  `found Ident("VYX_NO_TEMPLATE__…")`. The same fires on a single-quoted
  attribute that holds a `"` — `<input value='2"'/>` — which this file
  deliberately supports elsewhere (`vyx.vyrn:474-476`).
- A literal `</template>` inside an HTML comment truncates the section early →
  false `VYX_UNCLOSED_ELEMENT__…__div`.

A carpentry blog post (`a 6" nail`) or a single-quoted attribute is ordinary,
valid input, silently rejected. **Fix.** The section-close scan must be
HTML-comment aware and must not treat `"` as a string delimiter in template text.

### S4.4 CONFIRMED — High. `std/ui` `pages()` emits a malformed router for a hyphenated filename

`uiStaticHelperName` (`ui.vyrn:714`) and `uiDynHelperName` (`:733`) build an
exported function name by concatenating the raw route segment through
`uiUpperFirst` (`:505`), which preserves every byte including `-`. The name is
emitted unconditionally per route (`ui.vyrn:2034`) as
`export fn <name>() -> RoutePath`. A hyphen in the filename yields `hrefAbout-us`,
which is not a Vyrn identifier, so the whole generated router fails to parse.

Repro: copy `examples/pages/index.vyrn` to `zpages/about-us.vyrn`, then
`import { route } from pages("./zpages")`:

```
$ vyrn run z_ui_driver.vyrn
generated by pages("./zpages") at z_ui_driver.vyrn:8:16: expected LParen, found Minus
```

Every ordinary web slug (`about-us`, `sign-in`, `terms-of-service`) bricks the
router. Fails closed, not silent — hence High-but-not-critical. **Fix.** A shared
"segment → safe Vyrn identifier" step (camel-fold or refuse with a naming
diagnostic) in `uiStaticHelperName`/`uiDynHelperName`; also route
`uiDynHelperBody` (`ui.vyrn:745`, which builds a string literal by raw concat, not
through the `uiStrLit` choke point) through `uiStrLit` so the same segment bytes
cannot break the literal there.

### What this lens found clean

- **`std/html` closed the name-injection route.** A name is CHECKED (`nameOk`
  `html.vyrn:287`, `requireName` `:303`) and a value is ESCAPED
  (`escapeText`/`escapeAttr`, minimal correct sets: `& < > "` for text, `& "` for
  a double-quoted attribute). The prior `attr("src\" onload=..")` breakout is
  fixed. `std/vyx` and `std/ui` delegate all markup to it, so the classic
  `</script>` / attribute-breakout vectors are closed for those two giants.
- **`std/json`'s string escaper is total.** `emitString` (`json.vyrn:145`)
  escapes `"` `\`, the C0 short forms, and every remaining byte `< 32` as
  `\u00XX`. Raw UTF-8 passes through, which JSON permits. Correct.
- **`std/tw`'s value choke point holds.** Every CSS leaf value flows through
  `twCssValueSafe` (`tw.vyrn:212`) — a narrow hex/length/ident grammar AND a
  single-token, comment-free check via `std/scan`. Breakpoint values are checked
  by `twUnsafeBreakpointErrors` (`tw.vyrn:614`) at generation time. The prior
  breakpoint soundness hole (a value forging the token grammar) is fixed.

---

## Lens 5 — PL / product theory: coherence and the round-trip laws

### S5.1 CONFIRMED — Medium. VON: strings round-trip, names and numbers do not

`std/von` is the project's own serialization format with a stated round-trip law
(`emit`∘`parse` = id, the canonical text a fixed point of `vyrn fmt`). The law
holds for `VStr` (escaped, `von.vyrn:777`) and map keys, but `emitVon` writes
`VInt`/`VFloat` payloads and every structural NAME (record type, field, variant)
verbatim (S3.1's second half). A `Von` value carrying a name with a space or a
number payload with a brace emits text that `parseVon` cannot read back — the law
fails on the values the public `emitVon` accepts. Filed here as coherence and in
S3.1 as the type defect it comes from.

### S5.2 CONFIRMED — Medium (design critique). `std/codecs`: a byte encoder with no byte decoder

`base64EncodeBytes(b: Array<UInt8>) -> String` (`codecs.vyrn:164`) accepts
arbitrary bytes. Its only inverse is `base64Decode(s) -> Option<String>`
(`codecs.vyrn:198`), which computes the right bytes into an `Array<UInt8>` and
then funnels them through `decoded` (`codecs.vyrn:74` → `stringFromBytes`), so any
decoded output that is not valid UTF-8 becomes a silent `None`. Measured
(`z_codecs.vyrn`): of the 256 single-byte payloads,

```
$ vyrn run z_codecs.vyrn
base64 all-bytes bad=129
```

129 fail — byte `0x00` (the NUL rule) plus all 128 high bytes. `hexDecode` and
`urlDecode` share the shape. Every base64 payload the wider world cares about — a
key, a hash, a gzip blob, an image — decodes to `None`.

This is ranked a **design critique**, not a defect, because the text-only intent
is recorded: `examples/codecbytes.vyrn:67` documents `hexDecode("80")` → `None`
"(a lone continuation byte)". The coherence objection stands regardless: one half
of the API (`base64EncodeBytes`) speaks `Array<UInt8>` and the other half refuses
to, so `EncodeBytes` has no inverse and the round-trip law fails for exactly the
inputs `EncodeBytes` is for. **Fix.** Add `base64DecodeBytes(s) -> Option<Array<UInt8>>`
(and `hexDecodeBytes`) as the primitive, with the `String` forms layered on top.

### What this lens found clean

- **The injection escapers actually escape.** The hostile payloads landed as
  data: `escapeText`/`escapeAttr` (S4 clean note), `emitString` in both
  `std/json` and `std/von`, and the `std/tw` value grammar. The breakouts the
  prior audit found are closed.
- **`std/i18n` and the ICU quoting rule** are single-sourced and correct (S3
  clean note).

---

## What this audit did not cover

The two 202 KB giants were audited by focused read plus targeted execution, not
exhaustively; `std/vyx`'s runtime template-diff path and `std/ui`'s client TEA
runtime were read only for their emission side. `std/rpc` (61 KB), `std/http`
(64 KB), `std/connect`, `std/openapi`, `std/cli`, `std/num`, `std/scan`,
`std/jsonread`/`std/jsondec` (the readers, as opposed to the writers audited
here) were sampled, not swept — the OpenAPI/Connect emitters build their JSON
through `std/json`'s `emit`, so they are structurally valid by construction, but
their SEMANTIC validity (a spec a validator accepts) was not tested. No fuzzer
was run; every input here was hand-written. Severity is this audit's judgement.
