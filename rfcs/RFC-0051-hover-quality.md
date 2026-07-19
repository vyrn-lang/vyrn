# RFC-0051 — Hover Quality: Docs, Members, Record Structure, Class Precision

- **Status:** Draft (design locked)
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
