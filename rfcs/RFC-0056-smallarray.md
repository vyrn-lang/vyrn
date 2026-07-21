# RFC-0056 — `SmallArray<T, N>`: Small-Buffer Collections

- **Status:** Locked design
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
