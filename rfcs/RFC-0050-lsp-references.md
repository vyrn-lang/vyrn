# RFC-0050 — LSP: Scope-Aware Highlight, Import-Path Definition, Namespace Colour

- **Status:** Draft (design locked)
- **Depends on:** RFC-0047 (semantic tokens + the `classify_at` resolver),
  RFC-0027 (namespace bindings), RFC-0033/0049 (the `.vyx` mapping + owner
  discovery — these fixes apply in `.vyx` too, once owned)
- **Evidence (user review, `.vyrn` LSP now confirmed working):** (1)
  `import * as store` colours green (type) though `store` is a namespace;
  (2) Ctrl+Click on the import path `"./store"` does not open the file;
  (3) selecting a variable highlights same-named tokens **out of scope and
  inside comments** — because the server implements **no
  `documentHighlight` provider**, so VS Code falls back to dumb textual
  word-matching.

---

## 1. `textDocument/documentHighlight` (kills the dumb word-match)

Implement a scope-aware highlight provider: given the cursor on an
identifier, return the ranges of **that binding's actual references** —
the same resolution `hover`/`definition` already use — not textual
matches. So placing the cursor on a `let x` highlights its uses within
scope only; a parameter highlights within its function; a top-level
symbol highlights its real references; and **comments and unrelated
same-named bindings are never highlighted**. Register
`document_highlight_provider` in capabilities. Where the token doesn't
resolve to a known binding, return **empty** (no highlights) rather than
letting VS Code fall back to word-match — empty is correct and quiet.

- Kinds: the definition occurrence → `Write`, uses → `Read` (VS Code
  renders them subtly differently; a reasonable default, not load-bearing).
- Works in `.vyx` through the origin map (the highlight ranges map back
  to the input buffer, like hover).

## 2. `textDocument/definition` on an import path string

When the cursor is inside the **source string** of an import — `"./store"`,
`"std/time"`, or a generator-import argument (`i18n("../strings")`,
`rpcClient("./api")`, `componentsThemed("./widgets", "./theme.json")`) —
resolve that specifier through the module loader to its file and return
its `Location` (top of file). So Ctrl+Click on `"./store"` opens
`store.vyrn`; on `"std/time"` opens the std file; on a generator arg
opens the consumed directory's entry (or the dir itself where there's no
single file). Reuses the same resolver the loader uses — no new path
logic. A specifier that doesn't resolve to a local file (a remote/pinned
spec not cached) yields no definition, quietly.

## 3. Namespace colour: verify `import * as ns` classifies as `namespace`

Confirm `classify_at` returns `SemKind::Namespace` (legend index 0) for
an `import * as ns` **binding** and for the `ns` receiver in `ns.member`
uses — NOT `type`. The `NamespaceInfo` index already exists (hover uses
it); the classifier must consult it **before** falling through to the
type/symbol path.

- **If it already returns `namespace`:** the green is the user's theme
  colouring `namespace` like `type` (common, and standard — many themes
  do). Not a server bug; state it plainly in the as-landed notes (no
  code change, and don't fake one).
- **If it returns `type` (the likely bug):** fix the precedence so a
  namespace binding classifies as `namespace`. Pin with a token test on
  `import * as store` → `namespace`.

## Verification (the real path, not just unit tests)

The lesson of the prior editor rounds: a passing scripted unit test is
NOT proof the feature reaches the editor. So verify by **driving the
deployed server binary over stdio with real `file:///N:/...` URIs**
(the way VS Code frames them), opening a real `.vyrn` (and a `.vyx`),
and asserting:

- `documentHighlight` on a `let`/param returns only in-scope reference
  ranges — **zero** ranges inside comments or on an out-of-scope
  same-named binding (the exact user complaint, reproduced then fixed).
- `definition` on `"./store"` returns the `store.vyrn` `Location`; on
  `"std/time"` the std file.
- `semanticTokens` classifies the `import * as store` binding token as
  `namespace` (legend index 0), or the theme-colour finding is recorded.
- LSP e2e tests added for each. Full suite + LSP green, 0 warnings.
- **Rebuild + HASH-VERIFIED redeploy** of `vyrn-lsp.exe` (fresh ==
  deployed) — and report the driver transcript, not just "tests pass".

## Out of scope

`textDocument/references` (find-all-references across files — a superset
of §1; add if demanded, but §1 fixes the visible annoyance), rename,
call hierarchy, the server's single-threaded blocking on heavy `.vyx`
re-analysis (a separate async-server change — noted as the likely `.vyx`
lag cause, tracked independently), inlay hints.
