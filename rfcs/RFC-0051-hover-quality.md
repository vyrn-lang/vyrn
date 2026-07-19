# RFC-0051 — Hover Quality: Docs, Members, Record Structure, Class Precision

- **Status:** Implemented (2026-07-20)
- **Depends on:** RFC-0047 (semantic tokens + the classifier), RFC-0042
  (class hover), RFC-0033/0048/0049 (`.vyx` origin mapping + owner
  discovery — now working after the drive-letter path fix)
- **Evidence (user review, measured by probing the deployed server):**

  | probe | today |
  |---|---|
  | `format` (has a `///` doc) | `fn format(i: Instant) -> String` — **no doc** |
  | `error` (a `PageError` param) | `error: PageError` — **no structure, no doc** |
  | `error.status`, `error.message` | **null** |
  | `t.appTagline()` (namespace member) | **null** |
  | class at `mr-2`'s column | returns **`hover:text-brand-600`** (wrong token) |

  The AST already carries `doc: Option<String>` on every declaration (the
  parser attaches `///`), but the LSP `Symbol` has no `doc` field — so
  **hover has never rendered a doc comment**, in any file. That also means
  RFC-0020's "hover an i18n key to see its translation" never actually
  worked, since it relied on the generated `///`.

---

## 1. Render `///` docs in hover (global)

Plumb `decl.doc` through: add `doc: Option<String>` to the LSP `Symbol`
(and the member/namespace-member info), populate it wherever symbols are
indexed (own module, imported/cross-file, and **generator-synthesized**
modules), and render it in the hover markdown beneath the signature:

```
fn format(i: Instant) -> String

Human-readable UTC timestamp `YYYY-MM-DD HH:MM:SS` (a parity citizen: pure).
```

Docs are markdown already, so they pass through verbatim into the
`MarkupContent`. Consequences that come free: hovering an i18n key shows
its **translation** (RFC-0020's promise, finally real); hovering a `std`
function shows its documentation; hovering a user type shows its doc.

## 2. Member hover: `error.status`, `t.appTagline`, record fields

Hovering the member segment after a `.` must resolve and show the
member's type/signature (+ doc from §1). Cover the three receiver kinds
the corpus uses:

- **Record field** (`error.status`) → `status: Int64` + the field's doc.
- **Namespace member** (`t.appTagline`) → the export's signature + doc
  (the namespace path already exists for `.vyrn`; make it fire in `.vyx`
  templates too).
- **Builtin/protocol method** (`s.length`, `xs.push`) → its signature.

This must work in **`.vyx` templates** (where it is currently null) as
well as `.vyrn` — i.e. the member resolution has to run on the mapped
synth position, not be skipped when the origin lands mid-expression.

## 3. Record/enum structure in a value's hover

Hovering a value whose type is a user record or enum shows the type's
**shape**, not just its name:

```
error: PageError

type PageError = { status: Int64, message: String }
<the type's /// doc, if any>
```

Keep it bounded — the declaration line(s) as written (the existing
`TypeInfo.source`-style rendering), not a recursive expansion.

## 4. Class hover picks the token actually under the cursor

`class="mr-2 hover:text-brand-600"` currently returns the *last* class's
CSS regardless of cursor column. Fix the token-under-cursor computation
for class values so hovering `mr-2` shows `.mr-2 { margin-right: … }` and
hovering `hover:text-brand-600` shows its rule. Same fix applies to class
**completion**'s replace-range (it already uses a token range — verify
they agree).

## Verification (probe the exact positions, before/after)

Drive the **deployed** binary with the VS Code URI form
(`file:///n%3A/…`) over `examples/bin/routes/error.vyx` and
`routes/index.vyx`, and report a before/after table for exactly these:
`format` (doc), `listPastes` (doc), `error` (structure), `error.status`,
`error.message`, `t.appTagline`, class at the `mr-2` column, class at the
`hover:text-brand-600` column. Every "null"/"no doc"/"wrong token" above
must become correct.

- LSP e2e tests for each (member hover in a `.vyx`, doc in hover, record
  structure, class token precision).
- Full suite + LSP green, 0 warnings; parity unaffected (editor-only +
  read-only frontend additions).
- **Rebuild + HASH-VERIFIED redeploy** (fresh == deployed, both reported).

## Out of scope

Recursive type expansion in hover (one level), doc rendering for
parameters individually (the signature carries them), markdown link
resolution inside docs, hover on operators/keywords, inlay hints.

---

## As landed (2026-07-20)

Editor-facing only: `vyrn-frontend/src/symbols.rs` (+ one additive loader
query) and `vyrn-lsp`. No change to compilation, emitted code, or parity.

### §1 — docs (the plumbing)

`Symbol` and `Completion` gained `doc: Option<String>`, populated from the
AST's `decl.doc` at **every** indexing site: `index_symbols` (functions,
module state, `impl` methods, protocols, type decls, tests),
`index_imported_symbols` (cross-file), and `namespace_members`. `resolve`
renders it through one helper, `with_doc(detail, doc)` — signature, blank
line, doc verbatim (it is already markdown). The LSP also forwards it as a
completion item's `documentation`. Kinds the AST carries no doc on (enum
variants, protocol method signatures) stay `None`.

### §2 — member hover, and why `.vyx` was null

Two independent causes, neither of them `.vyx`-specific in the end:

1. **`resolve` had no member path at all.** `error.status` was null in a
   `.vyrn` too — the resolver went local → `ns.member` → namespace →
   top-level → builtin, and a record field is none of those. The fix
   answers a member token from `member_completions` (the same table `.`
   completion uses, so hover and completion can never disagree), gated on
   `is_member_position`. A user `impl`/protocol method still keeps its
   declaration site, so go-to-definition on `x.show()` is unaffected.
2. **A generated namespace had zero members.** `namespace_members` resolved
   its target by `resolver.read(target)`, but a generator module's key is a
   *banner* (`generated by i18n(..) at ..`), not a path — so the read failed
   and `t.appTagline` resolved to nothing. `index_namespaces` now uses a new
   additive loader query, `module_graph_with_sources`, and parses the
   module's synthesized source. Members of a generated module carry no
   `file` (nothing on disk to jump to) but hover fully — which is how
   RFC-0020's promise finally lands: the doc on the generated function *is*
   the translation.

The `.vyx` forward mapping itself was already correct: the cursor maps into
the generated line column-exactly, so once `resolve` learned members, the
template got them for free.

### §3 — record/enum structure

A local/param/let whose type names a user type (directly, or as an
`Array`/`Map` element) appends that type's declaration as a fenced `vyrn`
block plus the type's own doc — one level, as written, no recursion.

### §4 — class token precision

Could not be reproduced at HEAD: a column-by-column sweep of
`class="mr-2 hover:text-brand-600"` in `examples/bin/routes/error.vyx`
(cols 19–53, deployed binary) returns `mr-2` for exactly the `mr-2` columns
and `hover:text-brand-600` for its own — the wrong-token symptom was a
casualty of the `.vyx` path-key mismatch fixed in c820396 (the whole file
was mapping through a stale/rejected region). It is now locked by a test
that also asserts class **completion**'s replace-range starts at the same
token boundary hover uses.

### Before / after (deployed binary, VS Code URI form `file:///n%3A/…`)

| probe (0-based line, char) | before | after |
|---|---|---|
| `format` — index.vyx (6,11) | `fn format(i: Instant) -> String` | same + its doc: "Human-readable UTC timestamp YYYY-MM-DD HH:MM:SS (a parity citizen: pure)." |
| `listPastes` — index.vyx (4,11) | `fn listPastes() -> PasteList` | same + its doc: "List every paste, most-recent first. Zero-param => POST with an empty body." |
| `t.appTagline` — index.vyx (31,26) | **null** | `fn appTagline() -> String` + the translation "Paste text, get a short link. Persisted to disk." + a "via namespace t" note |
| `error` — error.vyx (2,11) | `error: PageError` | same + `type PageError = { status: Int64, message: String }` + the type's doc |
| `error.status` — error.vyx (2,17) | **null** | `status: Int64` |
| `error.message` — error.vyx (2,40) | **null** | `message: String` |
| class at (3,27) — `mr-2` | `.mr-2 {margin-right:0.5rem}` | unchanged (correct) |
| class at (3,32)/(3,40) — `hover:text-brand-600` | its own rule | unchanged (correct) |

Note: the RFC's draft probe list put `mr-2` at char 32; in the file as
written `mr-2` spans chars 26–29 and `hover:text-brand-600` spans 31–50, so
both columns were swept rather than assumed.

### Verification

- 926 workspace tests, 0 warnings; 32 LSP e2e tests (+4: doc in hover,
  generated-namespace member hover, member + structure hover inside a
  `.vyx`, class hover/completion token agreement) plus the ignored live
  transcript; `--ignored` parity suite green (5 passed).
- Rebuilt + redeployed `editor/vscode/server/vyrn-lsp.exe`, hash-verified
  equal to the fresh release build
  (`3C15A969DC662397A73478EDC95AEF113BBA255AC141304903225075E49A6171`).

### Known bounds

Record **fields** carry no `///` doc because `ast::Field` has none (adding
one is a parser change, out of scope here). A module-state `let`'s hover
does not expand its type's structure — only locals/params do, since only
they retain a `Type` in the index.
