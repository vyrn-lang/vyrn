# RFC-0056 — `SmallArray<T, N>`: Small-Buffer Collections

- **Status:** Implemented
- **Depends on:** RFC-0011 (Array element ops — the API contract this
  clones), RFC-0055 (benchmarking — the measurement rail; the win must be
  *shown*, not asserted), the leak/race hardening arc (drop discipline),
  RFC-0002 §5 generics (monomorphization — the engine that makes per-N
  layouts free)
- **Evidence (user):** "also something like rust-smallvec" — a collection
  whose first N elements live inline (no heap allocation), spilling to the
  heap only past N. The dominant systems-code pattern: most arrays are
  tiny, and the allocator call is the cost.

---

## 1. Integer type arguments (scoped, not const generics)

`SmallArray<T, N>` needs a number in a type. Vyrn gets **integer literal
type arguments** as a *scoped* grammar/type-system addition:

- The type grammar accepts a non-negative integer literal wherever a type
  argument may appear. It is carried as a distinct arm (e.g.
  `Type::ConstInt(u64)`), participates in display (`SmallArray<Int64, 8>`),
  equality, and monomorphization keys.
- **Only `SmallArray` accepts one** in v1: any other type constructor with
  an integer argument is a checker error (`type X does not take an integer
  argument`). No expressions, no const params on user generics, no
  arithmetic — a literal or nothing. General const generics remain future
  work; this RFC deliberately does not open that door.
- Bounds: `1 <= N <= 64`, checker-enforced (`smallArray capacity must be
  between 1 and 64`). Keeps worst-case stack/inline footprint sane and
  bounded (a moved SmallArray copies its inline slots).

## 2. The type

`SmallArray<T, N>` is a builtin generic like `Array<T>`:

- **API-identical to `Array<T>`** — the whole point is drop-in: `push`,
  `pop`, `swapRemove`, `a[i]`, `a[i] = v`, `length`, `for x in xs`,
  contextual `[]`/`[a, b, c]` literals against a `SmallArray` typed slot,
  `drop xs`, `?`-prop and region rules — all with the **same canonical
  trap wording** as Array (`array index N out of bounds`, etc.). Element
  type rules (validated types, sized ints) apply unchanged.
- A literal with more than N elements against a `SmallArray<T, N>` slot is
  a **checker error** (the capacity is known); *pushing* past N at runtime
  spills — that's the feature.

### Representation (locked)

`{ len: i64, cap: i64, data: ptr, inline: [N x T] }`, with **`cap` as the
state discriminant**:

- `cap == N` → inline state: elements live in `inline`, `data` is null and
  never read.
- `cap > N` → spilled: elements live at `data` (heap), `inline` is dead.
- Every element access branches on the state to pick the base pointer —
  that branch is the well-known smallvec cost and is accepted.
- **Spill**: a push at `len == cap == N` allocates `2N` on the heap, copies
  the inline slots, and the array **never un-spills** (pop below N stays on
  the heap — smallvec semantics, no surprise re-copies).
- **Move copies the struct** (inline slots included) — unlike Array's
  3-word move. Movecheck semantics are unchanged (moved-from is dead
  either way); only the copy size differs.
- **Drop frees iff spilled.** The leak-hardening free accounting must
  balance in both states, on every path drop can occur today (scope end,
  `drop`, region exit, `?`-prop unwind, spawn isolation).

### Boundaries

- `xs.toArray() -> Array<T>` — the one explicit conversion (copies out).
  No implicit coercion either direction in v1.
- Never crosses `extern`; not part of the JSON codec / `schemaOf` /
  `moduleInterface` reachable-type closure (a contract type should be
  `Array<T>`; using `SmallArray` there is a named checker error, not a
  silent hole).

## 3. All three backends, full parity

Interp, native, wasm — byte-identical including traps, across: fill to
exactly N, push N+1 (spill boundary), pop back below N (stays spilled),
index traps in both states, drop/leak accounting in both states, move
then use-after-move rejection, nested `SmallArray` in records/enums.

## 4. The measurement story

`examples/smallarray.vyrn`: a parity-citizen example, PLUS benches (RFC-0055)
comparing `Array<Int64>` vs `SmallArray<Int64, 16>` for a push-16/drain
workload — the numbers go in the as-landed notes (they will be honest:
inline wins on allocation avoidance, loses the per-access branch; the
notes must show both a winning and a losing shape, divan-style honesty).

## 5. Editor & tooling

- fmt: `SmallArray<Int64, 8>` formats stably (source-tightness rule covers
  the int argument); safety invariant holds.
- LSP: hover shows the full type incl. N; completion offers the same
  member set as Array.
- `///` docs on the builtin surface like other builtins.

## Verification

1. Spill-boundary matrix (§3 list) — three-way parity, byte-identical.
2. Leak accounting: RUNTIME_FREES-style balance in inline and spilled
   states on scope/`drop`/region/`?`/spawn paths.
3. Checker: int-arg on non-SmallArray rejected; N out of 1..=64 rejected;
   oversized literal rejected; contract-boundary use rejected — all with
   pinned wording.
4. Monomorphization: `SmallArray<Int64, 4>` and `SmallArray<Int64, 8>`
   coexist; `SmallArray<SmallArray<Int64, 2>, 2>` works (nested inline).
5. Bench example runs (`--check` in CI, real numbers in notes); full
   suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy.

## Out of scope

General const generics (user types with integer params), un-spilling,
`insert`/`remove` (Array doesn't have them either), a shared
Array/SmallArray protocol, transparent SBO on `Array<T>` itself
(rejected: ABI ripple through wasm/extern/drop hardening for an
invisible, uncontrollable change), and integer-argument *inference*
(`SmallArray<Int64, _>`).

## As landed

Shipped as specified — the locked representation
`{ i64 len, i64 cap, ptr data, [N x T] inline }` with `cap` the state
discriminant, `1 <= N <= 64`, spill at `len == cap == N` to `2N` (never
un-spills), and byte-identical interp/native/wasm output including traps.

### What moved where

- **`ast.rs`** — two new `Type` arms: `SmallArray(Box<Type>, usize)` (the
  builtin) and `ConstInt(u64)` (an integer literal used as a type argument).
  Both display (`SmallArray<Int64, 8>`, `8`) and derive `Eq`.
- **`parser.rs`** — `type_()` now accepts a non-negative integer literal as a
  type argument (→ `ConstInt`), so a stray one on any constructor reaches the
  checker rather than being a parse error; a dedicated `SmallArray<T, N>` arm
  reads the capacity literal directly. `.toArray()` maps to the method-only
  internal name `@toArray` (a free `toArray(x)` stays an unknown call).
- **`types.rs`** — `substitute` recurses into `SmallArray`'s element.
- **`checker.rs`** — integer-argument rule (`type X does not take an integer
  argument`, checked before arity so the diagnostic is precise), the `1..=64`
  bound (`smallArray capacity must be between 1 and 64`), oversized-literal
  rejection, and `SmallArray` threaded through covariance/`assignable`,
  `contains_fn`/`contains_heap`/`has_nested_wrap`, `ensure_type_exists`,
  `.length`, `push` (returns the same kind), `at`, `alen`, `for`-in,
  index-set, `mut_array_receiver` (`pop`/`swapRemove`), `unify`, and the new
  `@toArray`. Contract-boundary use is a named error via `codec::encodable`/
  `decodable` (`SmallArray` falls through to the "not codable" reject).
- **`interp.rs`** — a `SmallArray` is a `Val::Array` (the spill is invisible at
  the value level), so ops, `for`-in, literals, and traps reuse Array's paths;
  `@toArray` is the identity; element/coerce boundaries accept `SmallArray`.
- **`own.rs`** — new `DropKind::FreeSmallArr`; a `SmallArray` binding is tracked
  as owned (free `data`, null while inline) and escape analysis un-tracks a
  returned/aliased/`drop`-ed one, so it is freed exactly once.
- **`codegen (vyrn-codegen)`** — `llt` (the inline-aggregate struct), the
  `array_n_to_smallarray` / empty-`[]` literal lowerings, `gen_smallarray_push`
  (the full spill state machine), slot-based `pop`/`swapRemove`/index-set,
  value-based `at`/`for`-in/`toArray`, `.length`/`alen`, and the
  `FreeSmallArr` drop (frees the `data` pointer at byte offset 16 —
  `free(null)` is the inline no-op). Helpers `sa_ll`/`sa_slot_base`/
  `sa_value_base_len` factor the base-pointer state branch.
- **`loader.rs`/`symbols.rs`/`schema_reflect.rs`** — namespace rewrite +
  reachable-type walks recurse into `SmallArray`; LSP hover shows the full type
  incl. `N`, member completion offers Array's set plus `toArray`, `.length` is
  offered.

### Deviations

1. **`memcpy` → `llvm.memcpy` intrinsic.** libc `memcpy` has an i32 `size_t` on
   wasm32 but i64 on x86-64; declaring `@memcpy(ptr, ptr, i64)` tripped
   wasmtime's signature check. Switched the two copies (spill-from-inline,
   `toArray`) to the target-independent `@llvm.memcpy.p0.p0.i64` intrinsic,
   which lowers correctly on both targets (a `memory.copy` on wasm). SmallArray
   never crosses `extern`, so keeping the copy internal to generated code is
   sound — exactly the escape hatch the RFC anticipated.
2. **`xs[i]` on a plain variable reads through the binding slot**, not a spilled
   copy of the value. The value-based read (any other receiver, `for`-in,
   `toArray`) still spills the aggregate to a temp to address the inline buffer;
   for a `Var` receiver that copy is pure overhead, so the common indexed-read
   loop pays only the state branch. Purely an optimization — same observable
   result; without it the "losing" bench was ~20x (a 144-byte struct copy per
   access) instead of the ~10% the branch actually costs.

No silent redesigns; the representation, bounds, wording, and boundary rules
are exactly as locked.

### Benches (native, `vyrn bench examples/smallarray.vyrn`) — win AND loss

Divan-style honesty, both shapes shown (min times):

| workload                          | SmallArray<Int64,16> | Array<Int64> | result |
|-----------------------------------|----------------------|--------------|--------|
| push 16 (fill; no realloc)        | **57 ns**            | 94 ns        | SmallArray ~1.65x faster — the inline buffer skips the allocator (Array mallocs + grows 0→4→8→16) |
| indexed sum (1024 reads)          | 106 ns               | **96 ns**    | SmallArray ~10% slower — every `xs[i]` pays the `cap == N ?` state branch to pick the base pointer |

The win is allocation avoidance; the loss is the per-access branch — exactly the
smallvec trade-off, measured not asserted.

### Verification

- Spill-boundary matrix (fill-to-N, push N+1, pop-below-N-stays-spilled, index
  traps in both states, drop/leak in both states, move + use-after-move,
  nested `SmallArray<SmallArray<Int64,2>,2>`, `SmallArray` in a record and an
  enum) — three-way byte-identical, exercised by `examples/smallarray.vyrn`.
- Checker rejections pinned (int-arg on non-SmallArray, N=0, N=65, oversized
  literal, contract-boundary use, coexisting `N=4`+`N=8`, nested) — 8 tests.
- Leak accounting balanced in both states / explicit + auto-drop / two
  coexisting capacities — 5 codegen `free`-count tests.
- Suites: workspace `cargo test --workspace` green (all crates); `vyrn-lsp`
  40 tests green; three-way parity `cargo test -p vyrn-cli --test parity --
  --ignored` green (77 examples incl. `smallarray.vyrn`); `vyrn fmt --check`
  clean on the example (the int type argument round-trips); 0 new clippy
  warnings.
- `vyrn-lsp.exe` rebuilt (release) and redeployed to
  `editor/vscode/server/vyrn-lsp.exe`; SHA-256 verified equal:
  `04A702F4EFEC771F33BC7402DBB6A5BF69818331A975DC19446E891DDFB10633`.
