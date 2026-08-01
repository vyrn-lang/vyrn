# RFC-0083 — Portable SIMD, Without `unsafe` and Without Breaking Parity

- **Status:** **Accepted.** **M1 shipped** (`examples/simd.vyrn`, three-engine
  byte-identical including NaN — see "As landed"). Later milestones are gated on
  M1's parity result, in the manner RFC-0081 established.
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

### As landed (M1)

**The NaN question is answered, and the answer is not the one this section
braced for.** The three engines agree byte-for-byte on `±0.0`, on a `Float32`
subnormal, on `±Infinity`, and on NaN in every lane position through every one of
the four operators. The reason is not that the propagation rules match — they
still do not. It is that **a NaN's identity is not observable in Vyrn**: the
six-decimal formatter RFC-0081 moved to `std/num` reads the exponent and fraction
fields and answers `NaN` for any payload and either sign, so wasm's loose rule,
LLVM's and Rust's `f32`'s cannot be told apart by a program. A lane read prints
through that same path — `Float32` widens to `Float64` rather than having a
formatter of its own — so nothing new had to be written to make the columns line
up.

The bit patterns were checked separately and also agree, `0xFFF8000000000000` for
every NaN produced, including through a multiplication by a negative. **That
agreement is a property of this host, not of the three specifications**, and the
pin deliberately does not depend on it: all three engines are running the same
x86 hardware, and a pin on the payload would be a pin on the machine.

Four smaller corrections:

- **There is no exponent literal**, so the subnormal in the pin is squared into
  existence from `1e-20` written out in full. `sub / sub` is then a
  flush-to-zero probe worth more than the value itself: an engine that flushed
  denormals would compute `0/0` and say `NaN` where all three say `1`.
- **`<4 x float>` is not only the textual backend's spelling.** The direct wasm
  backend derives its representation from `llt_of`'s string, so reading `v128`
  off that one line keeps the lowering decision in one place, exactly as every
  other `Repr` does. The table above reads as though the two backends were
  independent; they share this.
- **`F32x4.splat(x)` is desugared in the parser**, beside `toString` → `@str`,
  because its receiver is a type *name* and not a value. Dropping it there is
  what keeps `F32x4` out of the expression grammar entirely — nothing downstream
  ever sees a bare `F32x4` variable to fail to resolve. One internal name per
  width, so M3's `F64x2.splat` is a second arm rather than a receiver three
  backends have to decode.
- **The census (RFC-0078) gains three `View` rows and no operation.** The
  constructor, the splat and the lane read are the whole of the representation;
  lane-wise `+` is a `BinOp` and never reaches the interpreter's `Call` dispatch,
  so there is nothing here to route into Vyrn later — only a type the language
  cannot otherwise name.

What did *not* happen is worth recording too, because it was the stated risk that
a value type which is neither a scalar nor a container would collide with
something. It collided with nothing: `own.rs`, `movecheck.rs` and the drop
analysis needed no edit at all, which is the "it is a value" claim above actually
holding rather than being asserted. Two matches in the compiler were exhaustive
over `Type` and both were in the textual backend.

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
