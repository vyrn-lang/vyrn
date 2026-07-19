# RFC-0050 — LSP: Scope-Aware Highlight, Import-Path Definition, Namespace Colour

- **Status:** Implemented
- **Status (history):** Draft (design locked)
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

## As landed

LSP-only + read-only frontend queries; no change to compilation, emitted code,
or parity. 926 workspace tests unchanged; LSP e2e 25 → 28 (+3, all default-run);
0 warnings. Deployed `editor/vscode/server/vyrn-lsp.exe` hash-verified equal to
the fresh release build.

**§1 documentHighlight (the headline).** New read-only frontend query
`vyrn_frontend::references(&Analysis, line, col) -> Vec<RefRange>` (in
`symbols.rs`), mirroring `resolve`'s precedence — local → namespace → top-level
symbol — to return a binding's ACTUAL occurrences:

- A token in **member position** (immediately after a `.`) highlights the same
  member accessed through the same-named receiver (namespace exports, record
  fields, builtin methods), receiver-scoped.
- A **local** (param/let/for-var) highlights only the uses that resolve to *that*
  binding, and only within the same function — computed per candidate token by
  the same "latest same-named binding at or before this line" rule `resolve`
  uses, so an out-of-scope same-named binding in another function is excluded.
- A **namespace** binding highlights its bare-`ns` occurrences (import binding +
  every `ns.` qualifier), skipping positions shadowed by a local.
- A **top-level symbol** highlights its real references, excluding positions
  where an in-scope local shadows the name.
- Declaration occurrence → `Write`, uses → `Read`. Unresolved token → empty
  `Some([])` so VS Code does not word-match. Comments are never lexed to tokens,
  so they never appear. `.vyx` maps synth-module references back through the
  origin regions (`vyx_highlights`, the inverse of `vyx_semantic_tokens`).

Driver transcript over the DEPLOYED binary (real `file:///N:/...` URIs), on a
fixture where `count` is a `tally` local, a comment word, AND an `other()`
binding:

```
[§1] documentHighlight on `count` (decl line 5):
   line 5 col 13 kind=3(Write)
   line 8 col 9  kind=2(Read)
   line 8 col 17 kind=2(Read)
   line 10 col 12 kind=2(Read)
   lines: [5, 8, 10]   # comment line 14 and other()'s count (15,16) ABSENT
```

And on `examples/namespace.vyrn`, highlighting `colorName`'s param `c` returns
only lines 19 (Write) + 20 (Read) — `main`'s for-var `c` on lines 35–37 is
absent (scope-aware). Against the OLD deployed binary the same requests returned
`result: NULL` (no provider → VS Code fell back to word-match, hitting the
comment and the out-of-scope binding).

**§2 definition on import path.** `resolve_spec` in `loader.rs` made `pub` (a
read-only reuse — no second copy of the path logic); new frontend query
`import_spec_at(source, line, col)` identifies the specifier string under the
cursor by *statement span* (so multi-line `import {\n..\n} from "path"` works and
generator-call string args are covered). The server's `import_path_definition`
resolves it and returns a top-of-file `Location`; a directory arg falls back to
an entry file inside it (else the dir). Remote/uncached specs → nothing, quietly.
Driver transcript (deployed binary):

```
[§2] definition on "./lib/shapes" (namespace.vyrn:15)
   -> file:///N:/lang/examples/lib/shapes.vyrn
[§2] definition on "std/time" (clock.vyrn:23)
   -> file:///N:/lang/std/time.vyrn
```

(OLD binary: both returned `null` — Ctrl+Click did nothing.)

**§3 namespace colour — verdict: THEME, not a server bug.** The classifier
already returns `namespace` (legend index 0) for both the `import * as ns`
binding token and the `ns.` qualifier — confirmed by driving semanticTokens on
the OLD binary too (it returned type index `0` before any change) and on the new
one. `classify_token` consults the `NamespaceInfo` index (step 3) BEFORE the
type/symbol fallthrough, so precedence was already correct. NO code change was
made for §3; the green the user sees is their theme colouring `namespace` like
`type` (common and standard). A token test pins the classification either way.
Driver transcript (deployed binary):

```
[§3] namespace.vyrn line15 `shapes` binding @col13  -> type 0 (namespace)
[§3] namespace.vyrn line29 `shapes` qualifier @col27 -> type 0 (namespace)
```

**Tests.** `rfc0050_document_highlight_is_scope_aware` (advertisement, Write/Read
kinds, comment + out-of-scope exclusion, empty-not-null on a keyword),
`rfc0050_definition_on_import_path` (`./store`, `std/time`, plus the identifier
path still works), `rfc0050_namespace_binding_classifies_as_namespace` (binding +
qualifier → `namespace`).

## Out of scope

`textDocument/references` (find-all-references across files — a superset
of §1; add if demanded, but §1 fixes the visible annoyance), rename,
call hierarchy, the server's single-threaded blocking on heavy `.vyx`
re-analysis (a separate async-server change — noted as the likely `.vyx`
lag cause, tracked independently), inlay hints.
