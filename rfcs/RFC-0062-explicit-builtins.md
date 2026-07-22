# RFC-0062 — Explicit Builtin Imports + Constructor Highlighting

- **Status:** Locked design
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
