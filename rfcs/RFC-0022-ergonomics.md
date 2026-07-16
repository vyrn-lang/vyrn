# RFC-0022 — Ergonomics Batch: `else if`, String Ordering, Byte Indexing, Import Aliasing

- **Status:** Implemented
- **Depends on:** nothing new — four small, evidence-backed gaps
- **Evidence:** every item was demanded by real code written this cycle —
  the std/i18n JSON+ICU parsers (RFC-0020 M2) and the std/rpc generators
  (RFC-0019). No speculative surface.

## Implementation notes

1. **`else if`** — parser only: after `else`, an `if` is parsed recursively as
   the sole statement of a synthesized else-block (zero new AST; the chained
   `if` keeps its own line for diagnostics). The token formatter already prints
   `} else if cond {` canonically (it never joins lines). The std/i18n parsers'
   17 pure `else { if }` staircases and its plural/select generator were
   migrated to `else if`.
2. **String ordering** — `< <= > >=` on `(String, String) -> Bool`, byte-wise
   lexicographic (byte order, **not** locale collation). Interp uses `str` byte
   `Ord`; codegen reuses `strcmp` and tests its sign against 0 with a signed
   `icmp`. std/i18n's hand-rolled `strGreater` was deleted for the operator.
3. **`s[i]` → `UInt8`** — the string-index result type changed from `Int64` to
   `UInt8`, matching `bytes(s): Array<UInt8>`. Mixed arithmetic needs an
   explicit `Int64(s[i])`. OOB trap wording unchanged. (The corpus already did
   byte work via `bytes(s)`, so churn was limited to a handful of tests.)
4. **Import aliasing** — `import { X as Y }`; `as` is contextual. The loader's
   `resolve_aliases` pass folds aliases into the one flat namespace before the
   register/visibility/merge stages, which stay alias-unaware: it rewrites
   references to the resolved decl, keys collision/visibility on the alias, and
   hides the original unless also defined/imported. **Co-naming** (a module
   importing `X as Y` while defining its own `X` — the RPC stub) is resolved by
   renaming the foreign decl to a fresh unique symbol program-wide, freeing the
   name for the local stub. LSP hover shows `— alias of <original>` and
   go-to-def jumps to the source. `std/rpc`'s `rpcInProcess` now emits same-named
   `<proc>` stubs via `getUser as getUser__real`, removing the RFC-0019 deviation.

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
