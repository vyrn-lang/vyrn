# RFC-0054 — Code Quotes: Structured Emission and Real Scanning for Generators

- **Status:** Implemented — see the As landed section below.
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

---

## As landed

### M1 — the `Code` type and `vyrn"…"` quotes (where it moved)

- **Lexer** (`lexer.rs`): `"""…"""` triple-quoted strings were added to BOTH the
  value lexer (`lex`) and the trivia lexer (`lex_with_trivia`, so `fmt` is
  unaffected). Inside a triple quote a lone `"`/`""` is literal; `\{…}`
  interpolation and every escape work exactly as in a plain string, only the
  terminator differs. A source **CRLF is normalized to LF inside string/template
  literals** so a `vyrn"""…"""` skeleton (and any multi-line string) carries
  byte-identical bytes on a CRLF or LF checkout — three-way parity never depends
  on the OS (a latent cross-platform bug this fixed). `token_name_and_text` maps a
  `Tok` to the `(kind, text)` pair `lex()` returns.
- **Parser** (`parser.rs`): `vyrn` is the second compiler-recognized template tag
  (after `template`). `code_quote` builds the skeleton (holes → `__vyrn_holeN`),
  validates it parses in one of four modes (decl-list → stmt-list → expr → type)
  via fresh sub-parsers, and classifies each hole (`hole_context` → `0` expression,
  `1` identifier fragment by textual adjacency, `2` standalone identifier/type by a
  string-literal substitution probe). The quote lowers to a
  `@codeText(part) + @codeSplice(hole, ctx) + …` chain. A hole-less `vyrn"…"` is
  the one tag whose plain-string form is meaningful. A skeleton that parses in no
  mode is an ordinary parse diagnostic in the generator's file, mapped to the
  literal's line via the wrapped-body error line.
- **Interp** (`interp.rs`): `Val::Code(Vec<CodePiece>)` — `Text` or origin-carrying
  pieces. `code_splice` applies the RFC-0054 table (String→escaped literal in expr
  position via `escape_string_literal`, String→validated bare identifier in ident
  position via `is_bare_identifier` — the real lexer decides non-keyword-ness,
  Code verbatim, numbers/bools literals); `render_code` inserts `//@origin`
  brackets around origin pieces (RFC-0033 format, zero loader/OriginMaps changes);
  `lex_tokens` runs the real lexer non-fatally (an `error` kind token, never a
  trap). `Code + Code` in `binop`. A `gen fn` returning `Code` is rendered by
  `generate`.
- **Checker** (`checker.rs`): an `in_gen` flag (set per function AND for signature
  validation) gates `Code`/`Token`/`render`/`rawAt`/`raw`/`lex`/`vyrn"…"` to
  generation context — **using them outside generation is a compile error**, which
  is what keeps them out of every backend (a `gen fn` body is never emitted). The
  surface names (`render`/`rawAt`/`raw`/`lex`) are common words, so they are NOT
  reserved: a same-named user function or binding shadows the builtin (in checker
  and interp).
- **`Code`/`Token` are magic, not injected** (`types.rs`): `resolve` maps
  `Named("Code")` to itself (opaque) and `Named("Token")` to its record shape
  `{kind,text,line,col}`, **only when the user has not declared that name** — a
  user `type Code`/`type Token` wins. This is the fix for two real collisions
  three-way parity caught: an unknown `Named` used to resolve to `Unit` (so `Code`
  silently became `Unit`), and an injected line-0 `Token` decl collided with
  `examples/consume.vyrn`'s `type Token` ("defined twice").

### Locked-design deviations (with justification)

1. **The interpreter does not trap Code builtins at runtime.** The RFC framed
   gen-only "same as `moduleInterface`", whose interp arm traps a runtime call.
   As landed, gen-only is enforced **only by the checker** (`in_gen`); the interp
   runs `render`/`raw`/`lex`/quotes anywhere. Justification: those operations are
   *pure* (unlike `moduleInterface`, which needs the generation resolver), the
   checker already keeps them out of non-gen source (and thus out of backends),
   and RFC-0021 promises a `gen fn` is "callable at runtime … for testing" — a
   runtime trap would make a `gen fn` emission helper (e.g. `std/i18n`'s `strLit`)
   untestable by its own `test` blocks. This is strictly sounder, not a loosening
   of the compile-time guarantee.
2. **`std/i18n` emission migrated via `gen fn` on the whole module, not just the
   escape helper.** The RFC's picture is "emission through quotes". A code quote
   is legal only in a generation context, so the escape choke point `strLit`
   became `gen fn strLit(raw) = render(vyrn"\{raw}")`. But a `gen fn` body is not
   emitted into a linked binary, so a plain-fn caller of `strLit` (emitted as dead
   code) left an undefined `@vyrn_strLit` at native link (three-way parity caught
   this). Since the entire `std/i18n` module is generation-only, every helper was
   marked `gen fn` — nothing is emitted, no dangling reference, and the emission
   still runs interpreted at generation and in the runtime `test` blocks.
3. **`std/graphql` and `std/i18n` do not route their foreign-text scanning through
   `std/scan`.** `std/scan` (a comment/string-aware **whitespace cursor**) does
   not model GraphQL's need to split on top-level commas while nested inside
   *four* bracket kinds (`<>()[]{}`) simultaneously, nor ICU's apostrophe-quoting
   (`'` quotes the next special char, `''` is a literal apostrophe) and `#`-in-
   plural rules. Forcing either onto the shared cursor would regress escaping bugs
   these generators had **already fixed and tested** (graphql's `"""`-description
   escaping in 28dfcc9; i18n's apostrophe/parameter handling in `icuApostrophe`).
   The sound choice was to migrate the actual bug surface — the hand-rolled Vyrn-
   string escaping (`gqlEscBody`, `strLit`/`escSecond`) — to compiler-guaranteed
   code-quote escaping, and leave each generator's audited, dialect-specific
   scanner where its tests live. `std/scan` ships, is tested, and is exercised by
   `examples/scan.vyrn`; it is the shared cursor for foreign text that DOES fit
   its model (and for the M4 generators).

### The graphql SDL-escaping bug — before / after

The RFC cites `std/graphql` "emitting invalid SDL (unescaped `"""` descriptions)".
That **SDL-level** bug was already fixed before this RFC (commit 28dfcc9:
`gqlEscTripleQuote`/`gqlDescBlock`, pinned by
`graphql_sdl_escapes_descriptions_and_splits_string_aware`), and it stays green
under the new emission. What RFC-0054 changed is the **Vyrn-level** baking: the
final SDL string was hand-escaped into a `sdlText()` literal by the `gqlEscBody`
byte loop —

```
// BEFORE
out = out + "    return \"" + gqlEscBody(doc) + "\"\n}\n"

// AFTER
return render(vyrn"""… export fn sdlText() -> String {
    return \{doc}
}
""")
```

The `\{doc}` splice is in expression position, so the compiler's own escaping bakes
`doc` as a string literal — a mis-escape corrupting the baked SDL is
unrepresentable, and `gqlEscBody` is deleted. `emit-gen` output is **byte-identical**
(verified by stashed old-vs-new diff on a graphql fixture; the escaping sets match
exactly). `std/i18n`'s `strLit`/`escSecond` byte loop was replaced the same way
(`i18n_translation_with_quotes_and_backslashes_bakes_losslessly`), also
byte-identical `emit-gen`.

### `render` auto-origins

No pilot uses `rawAt` (it is the M4 `std/vyx` story), so no example's directives
changed — `emit-gen` goldens are byte-identical, nothing silent. The auto-origin
round-trip is proven directly by `rawat_origin_maps_a_check_error_back_to_the_source`
(a check error inside `rawAt` text maps to the recorded `path:line:col` through
`OriginMaps`).

### Tests / verification

- **Frontend**: splice-rule unit tests incl. injection (`ev"; …"` in expr → inert
  literal; `a b`/`fn` in ident → error), `render` origin brackets, `lex()`
  agreement on the audit reproducers (a comment with `props`, `</script>` in a
  string, literal `{ a }`) and non-fatal `error` tokens.
- **CLI** (`codequotes.rs`): emit-gen escaping, injection→inert, skeleton error in
  the generator's file, bad-identifier splice naming the generator, `rawAt`
  origin round-trip, the `std/scan` example, and a lossless i18n bake.
- **LSP** (`lsp_e2e.rs`): a broken skeleton publishes in the generator's `.vyrn`
  over stdio in the VS Code URI form `file:///c%3A/…`; semantic tokens do not
  crash on the `vyrn"…"` tag.
- **Counts**: 946 workspace tests (5 ignored), 39 vyrn-lsp tests (1 ignored),
  0 clippy warnings, `vyrn fmt --check` clean; three-way parity green (5 suites,
  76-example corpus incl. the new `examples/scan.vyrn`) — the change moves WHERE a
  diagnostic is reported and adds gen-only emission, never any example's runtime
  output.
- **LSP redeploy (hash-verified)**: the fresh
  `compiler/vyrn-lsp/target/release/vyrn-lsp.exe` and the deployed
  `editor/vscode/server/vyrn-lsp.exe` are both SHA-256
  `f3799261f7df428e934c0c32738141b9f513c9fda6b9b58047f635da5f7daa3f`.

### M4a — `std/scan` block comments, `std/tw`, `std/ui`

The M4 dispatch, part a (`std/vyx` is the separate part b). No compiler code
changed — this is entirely `std/` + tests, riding the M1–M3 machinery.

**`std/scan` — `/* */` block comments.** `Scanner` gained `blockOpen`/`blockClose`
(non-nesting, the CSS/C rule; `blockOpen == ""` disables), honored by `skipWs`,
`skipUnit` (so `balanced`/`until`/`untilStr` inherit it), and `balanced` directly:
a delimiter hiding inside a `/* */` comment never ends a scan early. A
`cssScanner(src)` convenience presets `/* */` + `"`/`'` strings; the full `scanner`
constructor gained the two markers (its one caller, `examples/scan.vyrn`, updated).
The example — a three-way parity citizen — was extended with two CSS block-comment
cases, and `std/scan` gained 7 inline tests plus a CLI runner
(`std_scan_unit_tests_run_green`).

**`std/tw` — emission onto quotes + ONE CSS choke point.**
- The hand-rolled Vyrn escaper (`twEscSecond`/`twEscBody`) is deleted; `css()` is
  baked through a `vyrn"""…"""` code quote (`twEmitCss`, the graphql `sdlText`
  pattern — `\{rawCss}` in expression position, compiler-escaped), and `twStrLit =
  render(vyrn"\{raw}")` is the single Vyrn-emission escape choke point. The stylesheet
  is now assembled raw (the per-fragment `esc` path is gone) and baked once.
- ALL CSS safety flows through ONE gate, `twSheetSafetyErrors`, run as the FIRST act
  of `twBuildModule` — the sole producer of both the stylesheet and the `TwClass`/`Tw`
  token grammar. There is no path to CSS that skips validation, so the two audited
  holes are **structurally** impossible rather than prevented by remembering to call
  four separate passes in `tw()` (which now only parses + rejects unknown keys).
- The value half of the choke point scans each leaf through the new `std/scan`
  `cssScanner` (`twCssSingleToken`): an embedded `/* */` comment or a second token can
  never ride into a rule body, independent of the character grammar.
- Emission subtree marked `gen fn` (`twStrLit`/`twEmitCss`/`twBuildModule`) per risk 2.

**`std/ui` — emission onto quotes.**
- `uiEscSecond` + byte loop → `uiStrLit = render(vyrn"\{raw}")` (the escape choke
  point: a page path / url pattern / static segment spliced as a Vyrn string can no
  longer inject code).
- The three static runtime blocks (`uiFixedRuntime`, `uiHeadRuntime`,
  `uiErrorRuntime`) — pure emitted Vyrn — became hole-less `vyrn"""…"""` quotes, so the
  router glue is validated as Vyrn when `std/ui` is compiled, not re-lexed at
  generation time.
- The whole emission subtree (64 helpers) is marked `gen fn` (risk 2: a plain-fn
  caller of a quote-bearing helper dangles an `@vyrn_*` symbol at native link — the
  i18n lesson); only the runtime `PageError` constructors stay plain `fn`.

#### The two `std/tw` bugs — before / after

Both were already fixed by explicit validation passes (commit `1683e32`) and pinned
by `a_forging_breakpoint_key_fails_generation` / `a_css_injecting_value_fails_generation`
in `tw.rs`. M4a re-homes that safety into the structural choke point above and keeps
the pins. Demonstrated before/after against the genuinely-buggy pre-validation code
(`594e3ef`, which lacks the validators):

- **BEFORE** (`594e3ef` `std/tw.vyrn` swapped in): both pins FAIL — a `"red} body
  {display:none"` leaf value generated a stylesheet with the injected block (CSS
  injection), and an `"ev|xhack"` breakpoint key made `theme.cls("evbg-white")`
  COMPILE (the token-grammar forgery / soundness hole).
- **AFTER** (M4a): both pins PASS — `twSheetSafetyErrors`, the mandatory gate of the
  sole CSS producer, returns `TW_UNSAFE_VALUE__colors_evil` / `TW_UNSAFE_BREAKPOINT__ev_xhack`
  and no module is emitted. Injection and forgery are now unreachable, not merely
  unshipped.

#### Golden-diff review

`vyrn emit-gen` before (base `e3ddcaa`) vs after, for every example using `tw`/`ui`:
`twdemo`, `pagesdemo`, `shelf/server`, `shelf/view`, `bin/server`, `fullstack/server`
— all **byte-identical** (the head + error runtime blocks are exercised by
`bin`/`shelf`, so the static-block quote conversion is validated, not merely
plausible). The escape choke points reproduce the former hand-escaping exactly (the
M1 i18n/graphql result), and no example uses `rawAt`, so no origin directive moved.
Nothing silent — there is nothing to describe because there is no diff.

#### Deviations (with justification)

1. **The type/regex scaffold stays string concatenation, not quotes.** `std/tw`'s
   `export type Tw = String where value =~ "(…)"` and `std/ui`'s `RoutePath` are still
   built with `+`, exactly as `std/i18n`'s `TransKey` is (M1 shipped it that way). A
   code-quote hole in `value =~ __vyrn_holeN` position is not a valid skeleton parse,
   and the RFC's injection-safety property is delivered in full by the **escape choke
   point** (a String is baked as data, never code) — the scaffold carries only
   compiler-validated regex fragments, never untrusted text. This mirrors M1
   deviation 2's shape: migrate the actual bug surface (escaping), leave the audited
   structural scaffold where its bytes are proven.
2. **`std/tw`'s "one CSS choke point" is a mandatory gate in the sole emitter**, not a
   value-threading sheet builder. Threading a `{text, err}` accumulator through the
   divide-and-conquer CSS assembly would have churned `css()`'s bytes; a gate that the
   only CSS producer runs first gives the same structural guarantee (no CSS without
   validation) while keeping every golden byte-identical.
3. **`std/ui` does not use `rawAt`.** `rawAt` carries the origin of *user-authored*
   text spliced as code (the `std/vyx`/M4b story). `std/ui` splices no user text as
   code — its router glue is entirely derived, and the region-level RFC-0033 origin
   exists only to attribute a check error to the page file, so column-exact origins
   (risk 1) would be meaningless. Kept deliberately and documented in `uiEmitRoute`.

#### Tests / verification

- **Counts**: 947 workspace tests (5 ignored), 39 vyrn-lsp tests (1 ignored),
  three-way parity green (5 suites, incl. the extended `examples/scan.vyrn`), `vyrn
  fmt --check` clean on every touched `.vyrn`. Unit tests: `std/scan` 7, `std/tw` 18
  (+1 pinning the scan-based value gate), `std/ui` 6.
- **clippy**: 0 warnings introduced. (The workspace shows 52 warnings under the
  current clippy 1.95 toolchain, byte-for-byte identical on the base `e3ddcaa` —
  pre-existing toolchain drift in compiler source, none in any file M4a touched.)
- **LSP redeploy — NOT needed (std- and test-only change).** No `compiler/` source
  changed (only `compiler/vyrn-cli/tests/*.rs` test files, which do not build into the
  LSP); the frontend and the LSP binary are untouched, so the deployed
  `editor/vscode/server/vyrn-lsp.exe` still hashes
  `f3799261f7df428e934c0c32738141b9f513c9fda6b9b58047f635da5f7daa3f`. `std/` ships with
  the repo and the LSP picks it up live.
