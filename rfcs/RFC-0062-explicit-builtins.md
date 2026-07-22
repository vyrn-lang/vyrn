# RFC-0062 — Explicit Builtin Imports + Constructor Highlighting

- **Status:** Implemented
- **Depends on:** RFC-0047 (semantic highlighting), RFC-0010 (modules),
  RFC-0027 (`import * as ns`)
- **Evidence (user):** "`return Ok(copyPaste(…))` — type `Ok` doesn't
  highlighted as type, also I wish types were more explicit and imported."

Two halves: a semantic-token bug fix, and an opt-in way to write the
ambient builtins as explicit imports.

---

## 1. Constructor highlighting (bug fix)

`CONSTRUCTOR_BUILTINS` (`Some`/`None`/`Ok`/`Err`) already maps to
`enumMember` in `symbols.rs` step 5 — yet `Ok` in call position renders
unstyled in a real file (`examples/bin/store.vyrn`). So the classifier
never REACHES step 5 for that token (ordering, or the token walk not
visiting call-callee names, or an earlier step claiming it with `None`
result semantics). Diagnose with an LSP driver against the real file,
fix, and pin:

- `Ok`/`Err`/`Some`/`None` classify `enumMember` in EVERY position:
  call (`Ok(x)`), match pattern (`Ok(v) =>`), bare (`None`), nested
  (`Some(Ok(x))`) — in `.vyrn` and inside `.vyx` script/template
  expressions (the RFC-0048 forward map).
- e2e test asserting the token type at an expression call site (the
  user's exact shape), not just in patterns.
- While there: user enum variant constructors in call position must
  classify the same way (verify, pin if not already).

`enumMember` is the correct token type (they are variants, not types);
the fix is that it must actually apply. If the user's theme colors
enumMember like plain text, the LSP can't help that — but the token must
be there.

## 2. `std/option` / `std/result` — explicitness as a choice

```vyrn
import { Result, Ok, Err } from "std/result"
import { Option, Some, None } from "std/option"
```

- Two new std modules whose ONLY job is to name the ambient builtins.
  Importing them is a **validated no-op**: the loader recognizes these
  specifiers, checks the imported names against the module's fixed export
  list (`std/result`: `Result`, `Ok`, `Err`; `std/option`: `Option`,
  `Some`, `None`), errors on anything else (`std/result has no export
  `Foo``), and binds nothing new — the names keep resolving to the
  builtins they already were. `import * as r from "std/result"` is
  REJECTED (namespacing a builtin would create a second spelling —
  `r.Ok` — which is exactly the implicit/explicit split this avoids).
- Ambient use WITHOUT the import stays fully legal — this is opt-in
  style, not a migration. No repo-wide churn; the doc comments and one
  example (`examples/bin/store.vyrn` gains the imports as the showcase,
  since it's the file the user was reading) demonstrate the style.
- fmt: the imports format like any other; LSP: completion/hover for the
  two modules' names, go-to-def on an imported `Ok` targets the import
  line (the local anchor) — matching how other imported names behave.
- Implementation stays in the loader (specifier intercept before file
  resolution — the `std/` root never gains real files for these, so the
  builtins cannot be shadowed or diverge).

## Verification

1. Driver-verified token types for all constructor positions listed,
   before/after, in `.vyrn` AND `.vyx` (VS Code URI form).
2. Import validation: good imports are no-ops (program output unchanged
   — parity on the modified example); bad name / `import *` rejections
   pinned.
3. Full suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy.

## Out of scope

Requiring the imports (ambient stays), a full `std/prelude`, moving any
OTHER builtin behind modules, and theme-side color choices.

## As landed

Two commits of substance plus the showcase + these notes.

### ROOT CAUSE — the classifier was already correct (the RFC's §1 hypothesis was wrong)

The RFC's §1 premise — "the classifier never REACHES step 5 for that
token" — did not hold at HEAD. Driving the **deployed** `vyrn-lsp.exe`
(the exact binary the user runs) over the **real** `examples/bin/store.vyrn`
in VS Code URI form (`file:///n%3A/lang/examples/bin/store.vyrn`) and
decoding `semanticTokens/full` showed `Ok` in call position already served
as `enumMember`:

```
BEFORE (deployed exe, store.vyrn as it was — no explicit import):
   Ok  @L83C20 (return Ok(copyPaste(p)))        -> enumMember
   Ok  @L107C12 (return Ok(store.pastes[...]))  -> enumMember
   Err @L86C12  (return Err("no paste ..."))    -> enumMember
```

Cross-checked against the frontend classifier directly for **every**
position the RFC lists — call `Ok(x)`, pattern `Ok(v) =>`, bare `None`,
nested `Some(Ok(x))` — and for **user-defined** variant constructors
(`Circle(n)` in call position): all `enumMember`, in `.vyrn` and (via the
RFC-0048 forward map) inside `.vyx` `<script>` bodies. `classify_token`
step 5 *was* reached; nothing earlier claimed the token. Step 4 only
intercepts a constructor name when a matching **linked** symbol exists —
and the ambient `Result`/`Option` are parser-injected at line 0 (skipped by
the symbol indexer), so no symbol was ever created for `Ok`/`Err`/`Some`/
`None`.

So `Ok` was already `enumMember` (the RFC agrees this is the *correct*
token type — a variant, not a type). The user's "not highlighted as a
type" is a **theme/rendering** matter (a theme that colours `enumMember`
indistinctly, or the expectation that a constructor should read as its
type), which the RFC explicitly scopes **out**. Per "implement the closest
sound thing and document prominently," §1 landed as: **verify + pin** (two
e2e regression tests, since the value was in guarding the behaviour, not
changing it), plus the genuine, concrete defect §2 fixes.

### §2 — the real defect: writing the explicit import errored

Before this RFC, `import { Result, Ok, Err } from "std/result"` produced a
hard load error (`cannot load .../std/result.vyrn`) — there was no such
file, and nothing intercepted the specifier. The loader now recognizes
`std/result` / `std/option` **before file resolution** (new
`loader::builtin_alias_exports`), validates the imported names against the
fixed export lists, rejects `import * as` (namespacing a builtin would mint
a second spelling — `r.Ok`), and then **drops the import** from the module
so nothing is loaded or linked. The names keep resolving to the ambient
builtins they already were; ambient use without the import is unaffected;
the `std/` root gains no real files, so the builtins can never be shadowed
or diverge. Pinned wording: unknown name → ``std/result has no export `Foo` ``;
each module's export list is distinct (``std/option has no export `Result` ``).

`AFTER` (deployed exe, store.vyrn WITH `import { Result, Ok, Err } from
"std/result"`), driver-verified, diagnostics `[]`:

```
   Ok  @L12C18 / Err @L12C22  (the import specifiers)  -> enumMember
   Ok  @L88C20  (return Ok(copyPaste(p)))              -> enumMember
   Err @L91C12  (return Err("no paste ..."))           -> enumMember
   Ok  @L112C12 (return Ok(store.pastes[...]))         -> enumMember
```

### LSP niceties

`resolve()` gained a last-resort case so `Result`/`Option`/`Ok`/`Err`/
`Some`/`None` **hover** with a one-line description (driver-verified:
hovering `Ok` shows ``Ok(value: T) -> Result<T, E> — the success variant of
the builtin `Result`.``), and the six names join top-level **completion**.
Like every builtin they have no source declaration, so **go-to-definition
has no jump target** (`definition: false`) — this is the one deviation from
§2's prose ("go-to-def on an imported `Ok` targets the import line"):
anchoring a builtin to the import line would require indexing a local
symbol for it, which risks polluting the outline and re-routing
classification through step 4; hovering (which shows what it is) is the
sound subset, and it matches how all other builtins behave.

### Example + deviation

`examples/bin/store.vyrn` — the file the user was reading — gained
`import { Result, Ok, Err } from "std/result"` (it returns `PasteResult`, a
`Result`, and builds it with `Ok`/`Err`). It was NOT given the `std/option`
import, since it uses no `Option`/`Some`/`None`; importing names a file
never uses would be a noisy showcase. Both modules are exercised by the
loader tests; the paired-import style is documented in §2. Program output
is unchanged (the import is stripped before linking) — three-way parity
green on the whole `examples/` set and the `examples/bin` native build.

### Verification

Driver-verified before/after token types (above), `.vyrn` and `.vyx`. New
tests: two LSP e2e (`semantic_tokens_classify_constructors_as_enum_member`,
`semantic_tokens_classify_constructor_in_vyx_script`) and five loader unit
tests (valid no-op runs + ambient-still-works + unknown-name +
distinct-export-list + `import *` rejection). Full workspace `cargo test
--workspace` (1020) + `vyrn-lsp` (42) + three-way parity (`-- --ignored`,
6) green; `vyrn fmt --check` clean; 0 new clippy warnings (37 → 37); LSP
rebuilt (release) and redeployed to `editor/vscode/server/vyrn-lsp.exe`,
SHA-256 hash-verified equal
(`0eb9fe94d7c7db2941df64d7b69b76a55dfd434209c1f1ffbd345a2f74e2f800`).
