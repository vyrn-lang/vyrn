# Review: the standard library, second pass, five lenses

An external review of the Vyrn STANDARD LIBRARY (`std/*.vyrn`) at `aa36ce4`.
Five reviewers — Linus Torvalds, a C systems reviewer, a Rust reviewer, an Agda
implementer, a PL/product theorist — each hunting what only that reviewer would
find. This pass covers what `docs/research/audit-std.md` (the first std audit,
at `52e462f`) did not reach: `std/http`, `std/rpc`, the readers
(`std/jsonread`, `std/jsondec`), `std/num`, `std/cli`, `std/openapi`,
`std/connect`, and the small modules, plus `std/i18n` and `std/tw` beyond their
settled bugs.

Every finding carries evidence. A code finding cites `file:line`. A soundness
finding carries a program that was RUN, with its output recorded. Findings are
ranked **CONFIRMED** (reproduced or measured) above **PLAUSIBLE** (argued from
reading). Where an RFC or a leading `///` doc records a decision with its
argument, the entry says "design critique", not "defect".

Generated SDL was validated with `graphql-js` 16 (`parse` +
`buildASTSchema`/`validateSchema`), not by eye. Probes ran against this
worktree's `std/` via `VYRN_STD`, with `compiler/target/release/vyrn.exe`.

---

## Status of the earlier-reported issues

Each earlier finding was re-verified by running a probe, not carried forward.

| Earlier finding | Status | Evidence |
|---|---|---|
| `std/graphql`: interior `"""` in a doc folded into the block string unescaped; trailing `"` abutting the close (2026-07-18 census) | **FIXED** | Probe: a contract type documented `/// A doc with an interior """ triple quote and a trailing quote "` → SDL parses, schema valid (`graphql-js`: `PARSE OK … SCHEMA VALID`). `gqlEscTripleQuote` and the own-line rule in `gqlDescBlock` hold. |
| `std/graphql`: `gqlSplitTop` string-blind (phantom/dropped fields on `,` or `}` inside predicate strings) | **FIXED** | Probe: `type Url = String where startsWith(value, "http://a,b}c")` and a field `note: String where startsWith(value, "n,{}")` → SDL parses, schema valid. |
| `std/graphql`: zero-field record emits `type X {}` / `input XInput {}` (first std audit S4.1) | **FIXED in #169** (`aa36ce4`) | Re-probed at `aa36ce4`: the empty body now carries `_placeholder: Boolean` through `gqlBlock`; `graphql-js`: `PARSE OK … SCHEMA VALID`. |
| `std/graphql`: contract type named `Query` emits two `type Query` blocks (first std audit S4.2) | **FIXED in #169** — the name registry (`gqlDefine`) reports an RFC-0099 `Error` naming both routes. Caveat recorded below: a stale toolchain binary that predates RFC-0099 ignores the `//@diag` line and still emits the doubled document silently; with a current binary the load fails as designed. |
| `std/json` `JNum` / `std/von` `VInt`/`VFloat` unvalidated, VON names emitted verbatim (first std audit S3.1/S5.1) | **FIXED in #169** — the writers now check where they escape; round-trip tests feed quotes, backslashes, control bytes, above-BMP characters. Not re-probed beyond reading the diff; the tests in `compiler/vyrn-cli/tests/json.rs`/`von.rs` pin it. |
| `std/tw`: breakpoint key bypasses validation (regex forgery), leaf values baked unescaped (CSS injection) | **FIXED** | Single gate `twSheetSafetyErrors` (`std/tw.vyrn:775`) runs before any CSS or token grammar is emitted (`twBuildModule` `:792`); `twCssValueSafe` (`:212`) = single-token check via `std/scan` AND narrow hex/length/ident grammar; breakpoint keys checked by `twUnsafeBreakpointErrors` (`:614`). Pinned by tests at `:1035`, `:1045`. |
| `std/i18n`: lone apostrophe unconditionally toggled ICU quote mode; key collisions undetected | **FIXED** | `icuApostrophe` (`std/i18n.vyrn:316`) is the single quote rule (ICU lenient subset: `''` always literal; a lone `'` quotes only before `{`/`}`/`#`); every scanner (`readLiteral` `:335`, `matchBrace` `:366`, `splitTopCommas` `:396`, `branchBlob` `:594`) reads it. `I18N_DUP_KEY__`/`I18N_KEY_COLLISION__` diagnostics exist. |
| `std/vyx`: template scanner reads HTML as code; `@event` guard counts `<`/`>` as brackets. `std/ui`: hyphenated filename bricks the router | **FIXED in PR #168** (merged): the scanner reads markup by markup rules; slugs fold to camelCase with an RFC-0099 diagnostic on collision. Out of this pass's scope; not re-probed beyond confirming the merge. |
| D1 nested-array-literal native codegen bug (2026-07-18 census) | **FIXED** earlier (fix round 28dfcc9..2ce1aa3, ragged-literal parity pins); compiler-side, out of std scope. |

---

## Top findings by severity

| # | Severity | Reviewer | Finding | Ref |
|---|---|---|---|---|
| 1 | High | Agda | **`std/graphql`: a `///` doc ending in a backslash makes the emitted block string swallow the definitions after it.** `"""…\"""` — the trailing `\` plus the closing quotes IS GraphQL's one block-string escape, so the string never closes there; `graphql-js` refuses the document. Survives #169: this is a third hole, independent of the zero-field and `Query`-collision cases that PR fixed. | S4.1 |

(The table grows as the pass proceeds; sections below are in reviewer order.)

---

## Lens 1 — Linus Torvalds: taste and performance

(Findings pending; the first audit's measurement stands: the `out = out + x`
emitter accumulator is linear in both engines, so it is not re-litigated here.)

---

## Lens 2 — C systems: ownership and resource discipline

(Findings pending.)

---

## Lens 3 — Rust reviewer: correctness and API taste

(Findings pending.)

---

## Lens 4 — Agda implementer: soundness of the generators

### S4.1 CONFIRMED — High. A doc comment ending in `\` corrupts the whole SDL document

**Not the #169 cases.** PR #169 fixed the zero-field record and the duplicate
name; both re-verified fixed above. This is a THIRD, independent hole in the
same emitter: it is in `gqlDescBlock`'s escaping, not in the member loops or
the name registry, and it reproduces at `aa36ce4` with a one-field record and
no reserved name anywhere.

**The rule it breaks.** GraphQL block strings have exactly one escape sequence:
`\"""` (spec, BlockStringCharacter). `gqlDescBlock` guards a body that ENDS in
`"` (own-line form) and `gqlEscTripleQuote` escapes interior `"""` runs — but a
body that ends in a BACKSLASH is emitted as `"""<body>\"""`, and the parser
reads that trailing `\"""` as the escape, not as the close. The block string
runs on, consuming whatever SDL follows until the next `"""` in the document.

**Repro.** Contract (`api1.vyrn`) — an ordinary doc comment whose last
character is a backslash:

```vyrn
/// Ends with a backslash \
export type Thing = { name: String }

export fn getThing() -> Thing {
    return Thing { name: "x" }
}
```

Driver: `import { sdl } from "std/graphql"` /
`import { sdlText } from sdl("./api1")` / `print(sdlText())`.

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

The `type Thing` and `input ThingInput` definitions are swallowed into the
unterminated block string; the next `"""` in the document (another type's
description opener) closes it, and parsing fails there. A contract whose doc
comment ends in `\` — a Windows path, a line-continuation habit, a TeX fragment
— emits a document no GraphQL parser accepts.

**Root cause and shape of the fix.** `gqlDescBlock`'s own-line rule fires on a
trailing `"` but not on a trailing `\`. A trailing backslash needs the same
treatment: keep it away from the closing delimiter (emit the close on its own
line AND make sure the last body character before a `"""` is never `\`, e.g.
by appending a newline after a body that ends in `\`).

---

## Lens 5 — PL / product theory: coherence and round-trip laws

(Findings pending.)

---

## What this pass did not cover

(Completed at the end of the pass.)
