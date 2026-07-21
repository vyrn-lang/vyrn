# RFC-0054 — Code Quotes: Structured Emission and Real Scanning for Generators

- **Status:** Locked design
- **Depends on:** RFC-0007 (tagged templates — the mechanism this rides),
  RFC-0021 (`gen fn` + comptime interpreter + gen cache), RFC-0033 (origin
  maps — quotes now *emit* the directives it consumes), RFC-0053 (lex/parse
  remapping — quotes shrink the class it exists for)
- **Evidence (user):** "those generators looks terrible and error-prone tbh."
  The repo-wide audit agrees, with a systemic split: generators consuming
  **structured reflection** (`std/rpc`, `std/connect`, `std/openapi`) came
  back clean; every generator hand-rolling **text in / text out** shipped
  confirmed bugs — six `std/vyx` scanner miscompiles (a comment containing
  `props` broke the parser; literal `{ a }` silently became interpolation),
  `std/graphql` emitting invalid SDL (unescaped `"""` descriptions),
  `std/tw`'s breakpoint soundness hole + CSS injection, `std/i18n` eating
  ICU apostrophes and parameters, and the RFC-0053 `unexpected character
  '\'` dead-end.

Two failure axes, two fixes:

- **Output** — string concatenation with hand escaping → **`vyrn"…"` code
  quotes**: skeleton parsed when the *generator* is compiled, splices
  escaped/validated by grammatical context, origins emitted automatically.
- **Input** — five private byte-walkers with no comment/string awareness →
  **`lex()`** (the compiler's real lexer, exposed to gen code) and
  **`std/scan`** (one shared comment/string-aware cursor for foreign text).

The language already solved this problem class once: tagged templates exist
so `sql"…\{x}"` cannot be injected. This RFC applies the same mechanism to
the generators' own output. **Strings are data, never code** — the type
system, not discipline, prevents injection.

---

## 1. The `Code` type

A new builtin opaque type, **gen-context only** (same restriction and
wording style as `moduleInterface`): using it outside generation is a
compile error. It represents a fragment of Vyrn source text plus its origin
spans and is what quotes evaluate to.

- `Code + Code` concatenates fragments (origins merge).
- `render(c: Code) -> String` produces the final text with `//@origin`
  directives inserted automatically around origin-carrying regions and
  `//@origin end` after them (RFC-0033 format, unchanged — the loader and
  `OriginMaps` need **zero** changes).
- A `gen fn` may return `Code` directly; the loader renders it. Returning
  `String` keeps working (migration is incremental).
- Because everything here is gen-only, **no backend is touched**: parser +
  checker + comptime interpreter only. Parity cannot move.

## 2. `vyrn"…"` quotes

`vyrn` becomes the second compiler-recognized template tag (precedent: the
parser already special-cases `template`). `vyrn"""…"""` multi-line form is
the common case. Gen-context only.

### Skeleton validation — at the generator's compile time

The literal parts (the skeleton) are validated **when the generator itself
is checked**, not when it runs: each hole is substituted with a synthetic
placeholder identifier (`__vyrn_holeN` — the reserved internal namespace),
and the result must parse in one of four modes, tried in order:
**declaration list → statement list → expression → type**. Failure is an
ordinary diagnostic *in the generator's file*, at the line/col inside the
literal (map through the template's part offsets). A typo in generator
boilerplate is no longer a runtime "unexpected character in generated
code" — RFC-0053's remapping remains only for `raw` user text, where it is
exact.

The successful parse also tells us each hole's **grammatical context**.

### Splice rules (locked)

| hole context | value type | result |
|---|---|---|
| expression position | `String` | escaped Vyrn **string literal** (data, never code) |
| expression position | `Int*/Float*/Bool` | the literal |
| expression position | `Code` | verbatim structural splice (already-validated code, origins carried) |
| identifier / identifier-fragment position | `String` | validated: `[A-Za-z_][A-Za-z0-9_]*` fragment, non-keyword result — else a comptime error naming the generator and the hole |
| type position | `String` | validated identifier (same rule) |
| type / declaration position | `Code` | verbatim splice |

Identifier-**fragment** context is detected textually: a hole immediately
adjacent to word characters in the skeleton (`route_\{name}`) merges into
one identifier; the value is validated as a fragment (`[A-Za-z0-9_]*`,
non-empty). This is how generators build derived names today and must not
regress.

There is deliberately **no** way to splice a `String` as code. The graphql
SDL bug, the tw injection, and the regex injection become type errors.

Literal `\{` in emitted output (generated code that itself interpolates) is
written `\\{` in the skeleton, exactly as in every other template today.

### Escape hatches (the trust boundary, made visible)

- `rawAt(text: String, path: String, line: Int64, col: Int64) -> Code` —
  splice user-authored text (a `.vyx` template expression) as code,
  **carrying its origin**. Render wraps it in `//@origin path:line:col`.
  This replaces every hand-written origin directive in `std/vyx` — the
  single most error-prone convention in the system becomes automatic.
- `raw(text: String) -> Code` — origin-less verbatim splice. Exists only so
  146 KB of `std/vyx` can migrate incrementally; new code should not use it
  and the doc comment says so.

## 3. Input: `lex()` and `std/scan`

- `lex(source: String) -> Array<Token>` — gen-only builtin running the
  compiler's **real lexer** in non-fatal mode. `Token` is a builtin record:
  `{ kind: String, text: String, line: Int64, col: Int64 }` (kind is the
  canonical token-name string; an `error` kind carries unlexable bytes
  rather than trapping, since generators scan work-in-progress text).
  `std/vyx` script sections get tokenized by the same lexer that later
  compiles them: "a comment containing `props` broke the parser" becomes
  structurally impossible.
- `std/scan` — a pure-Vyrn module: a cursor over foreign text (CSS, ICU
  messages, HTML templates) that is comment- and string-aware
  (configurable comment markers + quote kinds + escape char), with the
  operations the audit showed every generator hand-rolls badly:
  `skipWs`, `until`, `balanced(open, close)`, `quotedString`, `ident`,
  line/col tracking. Written once, tested once.

## 4. Migration

- **M3 (this RFC): pilot on the guilty-but-small.**
  - `std/graphql`: full migration — emission through quotes (SDL string
    escaping by construction; the invalid-SDL bug becomes unrepresentable),
    scanning via `std/scan` where it walks text.
  - `std/i18n`: ICU message scanning via `std/scan` (apostrophe/brace rules
    in one audited place), Vyrn emission through quotes.
- **M4 (separate dispatch, after M1–M3 verify):** `std/vyx` (script
  sections via `lex()`, emission via quotes, hand-written `//@origin`
  plumbing deleted in favor of `rawAt`), `std/tw` (quotes for Vyrn
  emission + one CSS-escaping choke point via `std/scan` helpers),
  `std/ui`. The clean reflection generators (`rpc`, `connect`, `openapi`)
  do not change.

Migrated generators' output for the existing examples should be
**byte-identical** wherever feasible (`vyrn emit-gen` goldens before/after);
where auto-origins improve on hand-written directives, the diff is reviewed
and the improvement stated, never silent.

## 5. Verification

1. Splice-rule unit tests including **injection attempts**: a String value
   of `ev"; dropTables(); "` in expression position renders as an inert
   string literal; `a b` in identifier position is a comptime error naming
   the generator.
2. Skeleton-error mapping: a broken skeleton reports in the generator's
   file at the literal's line/col — via CLI and LSP (VS Code URI form
   `file:///n%3A/…`).
3. `render` auto-origin output round-trips through `OriginMaps` — a check
   error inside `rawAt` text maps to the recorded path:line:col.
4. `lex()` agrees with the compiler's lexer on a corpus including the six
   audit reproducers (comment containing `props`, `</script>` in a string,
   `{ a }` literal text, …).
5. Pilot generators: existing behavior tests green, emit-gen goldens
   byte-compared, the graphql SDL escaping bug pinned by a test that fails
   on the old code.
6. Full suite + LSP + three-way parity green, 0 clippy warnings. Rebuild +
   **hash-verified** LSP redeploy (fresh == deployed, both hashes reported).

## Out of scope

Deleting `raw` (needs the M4 migration finished first), quote support
outside gen context, quoting *patterns*/hygiene/macro-expansion semantics
(quotes are templates, not macros), attributing skeleton text to the
generator's own source lines in origin maps, and any change to how the
gen cache keys content (RFC-0053 §2 proved it correct).
