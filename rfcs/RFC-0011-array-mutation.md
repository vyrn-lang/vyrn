# RFC-0011 — In-Place Array Mutation

- **Status:** Implemented
- **Depends on:** RFC-0002 (arrays, records), RFC-0003 (validated types),
  RFC-0004 (ownership — arrays are owned heap values)

> **Motivation.** The ECS feasibility study (`examples/ecs.vela`, ROADMAP "ECS
> notes") found that `Array<T>` storage is already right — one contiguous
> `{ ptr, len, cap }` heap buffer, `a[i]` is a stride-indexed load — but the
> mutation surface is write-append-only: `[]` / `push` / `a[i]` read /
> `.length` / `drop`. There is no way to overwrite an element, remove one, or
> shrink. Every mutable-collection workload (an ECS, a worklist, a sieve)
> currently rebuilds whole arrays per step. This RFC adds the three missing
> operations.

---

## The surface

```vela
let mut xs: Array<Int64> = [10, 20, 30]

xs[1] = 25                 // 1. element store (in place)
let last = xs.pop()        // 2. remove + return the last element: Option<T>
let mid = xs.swapRemove(0) // 3. O(1) unordered remove: move last into slot i
```

1. **`a[i] = v`** — store `v` into element `i`. Traps on `i` out of bounds
   with the existing wording (`error: array index %lld out of bounds`,
   identical to the read path). `v` coerces into the element type exactly like
   a `push` argument — so a validated element type auto-validates on store,
   at compile time when `v` is provably constant (RFC-0003 `prove_coercion`),
   at runtime otherwise.
2. **`a.pop()`** — removes and returns the last element as `Option<T>`
   (`None` on an empty array). Never traps.
3. **`a.swapRemove(i)`** — returns element `i` after moving the *last*
   element into its slot and shrinking by one. O(1), does not preserve order —
   the canonical ECS despawn. Traps on out-of-bounds (same wording as reads).

All three require the array binding to be `mut` (same rule as `push` and
assignment). All three are subject-first methods / index syntax — no new free
builtins (RFC precedent: the 2026-07-16 surface migration).

## Semantics

- **Types.** `a[i] = v`: `a: Array<T>` (or `ArrayN<T, N>` — see below), `i`
  coerces to `Int64`, `v` coerces to `T` (validated-type checks included).
  `pop(): Option<T>`. `swapRemove(i): T`.
- **`ArrayN` (fixed-size stack arrays).** `a[i] = v` is allowed — a store into
  the stack slot. `pop`/`swapRemove` are **not** (a fixed-size array cannot
  shrink); the checker rejects them with a message naming `Array<T>`.
- **Ownership.** The element type domain today is scalar-ish (heap-element
  arrays — `Array<String>` — follow the same rules `push` already applies).
  An overwritten heap element is **not** freed by the store (a safe leak,
  consistent with the ownership analysis's conservative stance elsewhere);
  `pop`/`swapRemove` transfer ownership of the returned element to the caller.
  Movecheck treats `v` in `a[i] = v` as consumed, exactly like a `push`
  argument.
- **Validated elements.** `Array<Age>` + `xs[0] = 5` is a **compile-time**
  error (constant provably violates `Age`); `xs[0] = n` validates at runtime
  and traps with the existing validation wording on failure.
- **`a[i].field = v`.** Record-field write-through — see the **Addendum**
  below; implemented as exactly the read-copy-store idiom this bullet used to
  prescribe.

## The three backends

- **Checker:** new statement form (index-assign) type-checked as above; `pop`/
  `swapRemove` added to the builtin-method table (and to the LSP member
  tables, so `.pop` completes and hovers).
- **Interpreter:** direct `Vec` operations on the array value; `swapRemove` is
  `swap_remove`. Traps use the canonical byte-identical messages.
- **Codegen (native + wasm, same IR):** element store = the read path's
  bounds-check + `getelementptr` + `store`; `pop` = len-check, load, len
  decrement; `swapRemove` = bounds-check, load `i`, load `len-1`, store into
  `i`, len decrement. No new runtime functions — all inline IR next to the
  existing array helpers.

**Parity gate:** all three operations land with paired interp/codegen tests
plus example coverage; `ecs.vela` is rewritten from rebuild-compaction to
`swapRemove` despawn and stays in the three-way parity corpus.

## Addendum (implemented) — `a[i].field = v` write-through

Writing a record field back *through* an array element is sugar for the
copy-modify-store idiom, so it is exactly and only that:

```vela
let mut ps: Array<Point> = [Point { x: 1, y: 2 }]
ps[0].x = 9        // sugar for:
                   //   let mut ps[] = ps[0]   (load element 0 — bounds-checked)
                   //   ps[].x = 9             (set field on the copy)
                   //   ps[0] = ps[]           (store the copy back into slot 0)
```

- **Desugar (parser).** `ps[i].f = v` lowers in the parser to those three
  existing statements (a `let mut` of an unspellable element copy named `ps[]`,
  a `SetField` on it, and an `IndexSet` back into slot `i`). No new AST node, no
  new checker/interpreter/codegen path — it inherits every rule and the
  byte-identical behavior of the three legs it is built from. This is why the
  three backends stay in lockstep for free.
- **Semantics.** The load is the read path's bounds check (so `ps[i].f = v`
  traps with the same `array index %lld out of bounds` wording on an
  out-of-range `i`); the field write follows **`SetField`'s rules** — allowed on
  a plain record element, and **rejected on/into validated data** with the exact
  `SetField` wording (a predicated field type, or a predicated record type,
  cannot be mutated in place); the store follows `IndexSet` (the array must be
  `mut`, `v` coerces into the field type). The RHS is evaluated against the
  *pre-write* element, exactly as the hand-written idiom would.
- **Ownership.** The element copy is a value; `at(a, i)` produces no owned heap
  handle, so the temporary never enters drop analysis (no double-free, no leak
  beyond the store's existing "overwritten element is not freed" stance).
- **One level only (v1).** `ps[i].f.g = v` is a compile error (a single field
  write-through); deeper paths, and a non-variable array receiver
  (`f()[i].x = v`), are rejected in the parser.

Covered by `examples/arrays.vela` (a `Point` component array with field
write-through) and in the three-way parity corpus.

## Out of scope (future)

`insert`/`remove` (order-preserving, O(n)), `truncate`, `clear`, slices/views,
and multi-level element write-through (`a[i].f.g = v`). Each is additive; none
blocks the ECS use case.
