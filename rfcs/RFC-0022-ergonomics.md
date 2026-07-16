# RFC-0022 — Ergonomics Batch: `else if`, String Ordering, Byte Indexing, Import Aliasing

- **Status:** Draft — approved for implementation
- **Depends on:** nothing new — four small, evidence-backed gaps
- **Evidence:** every item was demanded by real code written this cycle —
  the std/i18n JSON+ICU parsers (RFC-0020 M2) and the std/rpc generators
  (RFC-0019). No speculative surface.

---

## 1. `else if`

```vyrn
if b[i] == 123 { ... } else if b[i] == 34 { ... } else { ... }
```

Today `else` demands a block, forcing nested `else { if ... }` staircases —
the i18n parsers are full of them, and generators must *emit* them. Parser:
`else` followed by `if` chains directly (the universal semantics — sugar
for the nested form, zero new AST if the parser simply parses the `if` as
the else-block's single statement... but pick the representation that keeps
diagnostics lines honest). Formatter: canonical `} else if cond {` on one
line; the corpus reformat migrates existing staircases. All three backends
unaffected (it is the same AST or a trivial desugar).

## 2. String ordering: `<` `<=` `>` `>=` on (String, String)

Byte-wise lexicographic comparison (what `memcmp`/Rust `Ord` both already
do — parity for free), returning `Bool`. **Documented as byte order, not
locale collation** (consistent with `s.length` counting bytes; collation is
a future i18n-library concern, never an operator). The i18n generator's
hand-rolled `strGreater` gets deleted. Checker: extend the comparison
operators' type rule; interp: Rust `cmp`; codegen: `strcmp` via the shim
(64-bit-clean wrapper if needed) — canonical semantics identical.

## 3. String indexing returns `UInt8`

`s[i]` today yields `Int64`, while `bytes(s)` yields `Array<UInt8>` — so
byte-level code can't mix the two and everything migrates to `bytes(s)`.
Align: **`s[i] : UInt8`** (a byte — consistent with `.length` in bytes and
`bytes(s)`; OOB traps with the existing string-index wording). Migration:
sweep the corpus/tests for `s[i]` uses whose arithmetic expects Int64 (mixed
arithmetic already requires explicit conversion — `Int64(s[i])` where
needed). This is a breaking change to an inconsistency, pre-1.0, done once.

## 4. Import aliasing

```vyrn
import { getUser as fetchUser, User } from "./api"
```

Needed concretely by `rpcInProcess` (RFC-0019 deviation: its stubs could not
share names with the real functions in the flat program namespace — with
aliasing, the generated module imports `getUser as getUser__real` and the
stub takes the real name, erasing the `call<Proc>` wart). Semantics: the
alias is the importing module's local name (visibility, collision checks,
movecheck — everything keys on the alias); the loader records alias →
original for linking; enums/protocols imported under an alias bring their
variants/methods under their own (unaliased) names as today. LSP: hover on
the alias shows the original's signature (`fetchUser — alias of getUser`),
go-to-definition jumps to the original; completion offers the alias.
Formatter: ` as ` spacing. `std/rpc`'s `rpcInProcess` is updated to emit
same-named stubs via aliasing (removing the documented deviation), with its
tests adjusted.

## Out of scope

`else`-less `if` expressions, locale-aware collation, string slicing
(`s[a..b]` — wants a slices design), re-exports (`export { x } from`),
wildcard imports. Each is a separate conversation.
