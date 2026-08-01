# RFC-0083 — Portable SIMD, Without `unsafe` and Without Breaking Parity

- **Status:** **Accepted.** M1 not started. Later milestones are gated on M1's
  parity result, in the manner RFC-0081 established.
- **Depends on:** RFC-0077 (the direct wasm backend — `wasm-encoder` is what
  emits the instructions), RFC-0078 (the census — `View` is the category these
  join), RFC-0082 (the capability boundary this narrows by one row)
- **Evidence:** the vendored `wasm-encoder 0.221.3` already carries **246** SIMD
  instruction variants (`F32x4Add`, `V128Load`, `F32x4ExtractLane`, …), and the
  textual backend's LLVM has first-class vector types. Neither backend needs a
  new dependency.

## Why this is not an `unsafe` question

RFC-0082 recorded SIMD under "uses of `unsafe` that reach past the abstraction".
That was wrong, and the correction is the reason this RFC exists: **Rust's
*portable* SIMD (`core::simd`) is a safe API.** Only the platform-specific
intrinsics (`_mm_add_ps`) are unsafe, and those are unsafe because they are
per-architecture, not because vectors are.

Every operation proposed here is **total**: lane-wise arithmetic on a fixed-width
vector cannot trap, cannot alias, and cannot observe uninitialized memory. A lane
index is a compile-time constant the checker verifies against the lane count,
which `consteval` already does for refinement predicates. Nothing acquires the
power to violate ownership, drops or validation.

## The constraint that shapes the whole design

Vyrn's invariant is byte-identical output across interpreter, native and wasm,
including the sixth decimal place of a float. That has a hard consequence:

**Auto-vectorising floating-point reductions is permanently unavailable.** A
vectorised `s = s + xs[i]` sums lanes in a different order, and floating-point
addition is not associative, so the result differs in the last bits. That is what
`-ffast-math` permits and Vyrn cannot. This is not a gap to close later; it is
what the core promise costs.

**Explicit SIMD does not have that problem, and that is the whole point.**
`F32x4` addition is four *independent* IEEE-754 single-precision additions, lane
*i* of the result depending only on lane *i* of the operands. No reassociation
happens, so an interpreter emulating it lane-by-lane in a loop produces bit-identical
results to a hardware `f32x4.add`. Determinism is what makes this expressible at
all, and it is why the design is a library of exact operations rather than a
compiler pass.

Integer auto-vectorisation is a separate matter and remains available — integer
addition *is* associative — but it is an optimisation question, not this RFC's.

## What M1 builds

One type, four operations, three engines. The point of M1 is to prove the whole
pipeline end to end, not to be useful.

```vyrn
let a = F32x4(1.0, 2.0, 3.0, 4.0)
let b = F32x4.splat(0.5)
let c = a + b
print(c.lane(0))        // 1.5
```

- **`F32x4`** — a value type of four `Float32` lanes. It is a *value*, not a
  container: no heap, no ownership beyond a scalar's, nothing to drop.
- **Construction**: `F32x4(a, b, c, d)` and `F32x4.splat(x)`.
- **Lane read**: `v.lane(k)` where `k` is a literal in `0..3`. **A non-constant
  or out-of-range index is a compile error, not a trap** — that is what keeps the
  operation total and is the reason no bounds check is emitted.
- **Arithmetic**: `+`, `-`, `*`, `/`, lane-wise.

Lowering, all of which exists:

| engine | how |
|---|---|
| interpreter | a new `Val` variant holding `[f32; 4]`, operating lane-by-lane |
| textual | `<4 x float>`, `fadd`/`fsub`/`fmul`/`fdiv`, `extractelement` |
| direct wasm | `V128Const`, `F32x4Add`/`Sub`/`Mul`/`Div`, `F32x4ExtractLane` |

### The pin, and the risk it exists to catch

A three-engine parity example that constructs, splats, does all four operations
and reads every lane — **including the values where engines historically
disagree**: `±0.0`, denormals, `±Infinity`, and **NaN**.

NaN is the real risk and it should be stated plainly rather than discovered.
Wasm specifies NaN propagation loosely (an implementation may return any NaN with
the sign bit unspecified in some cases), LLVM has its own rules, and Rust's `f32`
has a third. This project has already been bitten once in this exact area — RFC-0081
found native's `fcmp one` answering the wrong thing for NaN operands, and fixed it
to `une`. **If the three engines cannot be made to agree on NaN lanes, M1 stops
and reports**, and the honest outcome may be that the arithmetic ships and NaN
handling is specified separately. Nothing here is worth a divergence.

## Milestones after M1, each gated on the one before

- **M2 — memory and comparison.** Load/store four lanes from an
  `Array<Float32>` at an index (**bounds-checked once for the whole vector**, not
  per lane), lane-wise comparisons producing a mask, `min`/`max`/`abs`/`sqrt`.
  This is where SIMD becomes useful rather than demonstrated.
- **M3 — the other widths.** `I32x4`, `F64x2`, `I64x2`. Mechanical once M1 and
  M2 have settled the shape, and worth doing only if something wants them.

## What this does not decide

**Auto-vectorisation of integer loops.** Free from LLVM at `-O2` and from
Cranelift for wasm, and blocked mainly by the redundant bounds check in loop
bodies — `wcond` tests `i < len`, then the body immediately re-tests `i >= len`
against the same length. That is an optimisation milestone with no language
surface, and it belongs to whoever measures it.

**Wider vectors.** 256- and 512-bit have no portable wasm equivalent; wasm's SIMD
is fixed at 128 bits. Widening would mean per-target code, which is the
per-architecture problem that makes Rust's intrinsics unsafe.

**Runtime feature detection.** Wasm SIMD is a validation-time capability, not a
runtime one: a module either uses `v128` or does not, and a host that lacks it
refuses the whole module. There is no `is_x86_feature_detected!` shape here to
design.
