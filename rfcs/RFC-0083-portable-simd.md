# RFC-0083 — Portable SIMD, Without `unsafe` and Without Breaking Parity

- **Status:** **Accepted.** **M1, M2 and M3 shipped** (`examples/simd.vyrn`,
  `examples/simdmem.vyrn`, `examples/simdround.vyrn`, `examples/simdint.vyrn`,
  three-engine byte-identical including NaN, the exact halves and both ends of
  `Int32` — see the three "As landed" notes). M3 took **`I32x4` only**, on the
  licence the milestone gave itself; `F64x2` and `I64x2` are priced and refused
  in its note.
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
- **M3 — the other widths.** `I32x4`, `F64x2`, `I64x2`. **Not mechanical — this
  line said "mechanical once M1 and M2 have settled the shape" and that is
  wrong.** The operator set differs by lane type, verified against the encoder
  rather than assumed:

  | | floats | integers |
  |---|---|---|
  | `/` | `F32x4Div`, `F64x2Div` | **does not exist** — there is no `I32x4Div`, because no hardware has SIMD integer divide |
  | `min`/`max` | one each | **two each** — `MinS`/`MinU`/`MaxS`/`MaxU`, and only at i8/i16/i32; `I64x2` has none at all |
  | saturating `+`/`-` | none | `AddSat`/`SubSat`, signed and unsigned, **i8 and i16 only** — not i32 |
  | `sqrt`, `ceil`, `floor`, `trunc`, `nearest` | yes | none |

  So `I32x4` is not `F32x4` with a different lane type: it loses `/`, gains a
  signedness question on `min`/`max` that Vyrn's `Int32` vs `UInt32` can answer,
  and `I64x2` is thinner still. Whoever takes M3 should price each width
  separately and is free to ship one. **Taken, `I32x4` only — and the pricing
  went further than this table expects: `min`/`max` do not ship either.** See
  the "As landed — M3" note.

  Two further findings from the same pass, both worth having before M3 starts:

  - **`v128` is a single wasm type whose lane interpretation belongs to the
    *instruction*, not the value.** Vyrn instead makes each width its own type,
    which is a checker-level choice and the right one — it is what stops
    `F32x4.min` being applied to integer lanes. The consequence is that
    reinterpreting the same 128 bits across widths is free in wasm and needs an
    explicit Vyrn operation to be expressible at all. None is proposed; it is
    named so its absence is a decision.
  - **`Mask32x4` has no reduction, and wasm has three.** `V128AnyTrue`,
    `AllTrue` and `Bitmask` are exactly the "did any lane pass / did every lane
    pass" question a mask exists to answer, and M2 shipped `.lane(k)` without
    them. That is a gap in the *current* surface rather than an M3 item, and it
    is the cheapest useful thing left in this RFC. **Closed** — `anyTrue` and
    `allTrue` are in M2's "As landed" above, `bitmask` is refused there with its
    reason, and M3's `Mask64x2` inherits both the spelling and the
    closed-inhabitants argument the lowering rests on.

  Also noted and deliberately not taken: `F32x4PMin`/`PMax` exist beside
  `Min`/`Max`. They are the "pseudo-minimum" pair with different NaN behaviour,
  so they are a *different operation*, not a faster spelling — M2's IEEE-754-2019
  `minimum`/`maximum` choice stands and this is only recorded so nobody
  "optimises" one into the other. The **relaxed** family (`RelaxedMadd`,
  `RelaxedMin`, …) is the same trap with the arguing already done for it: see
  the M2 note below, where it is refused permanently rather than deferred.

### As landed (M2)

```vyrn
let v = F32x4.load(xs, i)          // xs: Array<Float32>, ONE bounds check
F32x4.store(xs, i, v)              // the same four, written back
let m = a < b                      // Mask32x4 — `< <= > >= == !=`, never Bool
print(m.lane(0))                   // true
print(m.anyTrue())  print(m.allTrue())   // the whole mask, as one Bool
F32x4.min(a, b)  F32x4.max(a, b)  F32x4.abs(v)  F32x4.sqrt(v)
```

Three engines byte-identical across `examples/simdmem.vyrn` (every operation
against `±0.0`, a `Float32` subnormal, `±Infinity` and NaN in every operand
position), `examples/simdbench.vyrn`, and both trap examples — stdout, stderr
and exit code. `wasmtime -W simd=n,relaxed-simd=n` rejects the modules with
`SIMD support is not enabled`, which is the positive proof these are really the
vector instructions and not a scalar emulation the parity sweep would have been
just as happy with.

**The mask is its own type, `Mask32x4`, and the reason is what the alternative
cannot say.** The bit pattern is the conventional one on both backends —
`<4 x i32>` of all-ones/all-zeros textually, a `v128` in wasm — so the decision
is not about representation. It is that an `I32x4` mask would be a type a program
can *build*, and `select(I32x4(7,7,7,7), a, b)` has no answer the three engines
agree on for free: wasm's `v128.bitselect` is bit-wise and would mix the two
operands' mantissas, LLVM's `select` wants an `<4 x i1>` and would read only the
low bit, and an interpreter would have to pick one of those to imitate. A type
with no other inhabitants costs one enum variant; normalising an arbitrary
pattern costs an instruction on every use and a decision on every engine.
`<4 x i1>` was rejected as the *textual* representation for a separate reason: a
mask crosses function boundaries here like any other value, and `<4 x i1>` is a
strange ABI type (packed to `i4` in places). The `sext`/`icmp ne` pair that
choosing `<4 x i32>` costs is folded away by `-O2` at every use.

**The reductions, `m.anyTrue()` and `m.allTrue()`, complete this surface.** M2
shipped `.lane(k)` and nothing else, so the only way to ask the question a mask
exists for was four lane reads and three `&&`. They are **value methods, not
`Mask32x4.anyTrue(m)`**, and the choice follows the rule stated above rather than
adding a third convention: the type name is where an operation lives when
something *else* exports the name (`min`, `max`, `abs` are `std/math`, and
`math.min(a, b)` reaches the parser in the same shape), and nothing exports
these. That is the same reason `lane` is a value method. They are the wasm
instructions' names rather than Rust's `any`/`all` because `any` and `all` are
exactly the two names a future `std/arrays` predicate would want, and taking them
here would be taking them globally.

| engine | how |
|---|---|
| interpreter | a fold over the four `bool`s — the reference answer |
| textual | `icmp ne <4 x i32>`, then `llvm.vector.reduce.or`/`.and.v4i1` |
| direct wasm | `v128.any_true` / `i32x4.all_true` |

**The ambiguity the mask type exists to prevent does not come back, but only
because of a property worth naming.** `v128.any_true` is *whole-vector* — any bit
set anywhere — while `i32x4.all_true` is per lane, and the other two engines are
per lane at both. They agree because a `Mask32x4` lane is all-ones or all-zeros
and **no program can build one that is neither**, which is the same closed set of
inhabitants that made the distinct type worth its enum variant. A mask that could
hold a partial lane would split the engines here first, before it split them at
`select`. There is no `i32x4.any_true` to reach for instead: the encoder carries
exactly one any-true, at `v128` width. Natively the lowering is deliberately not
the tempting one — `bitcast` to `i128` and compare against `-1` — for the reason
the mask lane read already gives: the all-ones encoding is how a mask is *stored*,
not what it means, and `-O2` folds the readable spelling to the same `movmskps`.

Three engines byte-identical on all-true, all-false, one true lane at a time and
one false lane at a time (the lane-order and lane-*width* pin — a reduction
reading the wrong width answers all-true and all-false correctly and diverges only
there), and on masks built from NaN comparisons in every operand position.
**NaN-derived masks are not empty masks**: `NaN != NaN` is true, so `nv != nv`
reduces to true on both counts while `nv == nv` and `nv < nv` reduce to false on
both, and all three engines agree on every one.

**`bitmask` was considered and is not shipped**, which is a decision and not an
omission. `i32x4.bitmask` gathers each lane's MSB into an `i32`, and that number's
meaning *is* the lane-to-bit order — lane 0 in bit 0 — so shipping it would make a
layout fact part of the language surface, where `anyTrue`/`allTrue` expose none.
The RFC has already recorded the mirror of this for reinterpretation across
widths: lane order is free in wasm and needs an explicit Vyrn operation to be
expressible at all, and none is proposed. Nor is there anything to *do* with the
integer once you have it — Vyrn has no popcount and no computed jump, so every
use reduces to `!= 0` (which is `anyTrue`) or `== 15` (which is `allTrue`), both
already spelled. "wasm has it" is the argument RFC-0078's census exists to
refuse.

The census gains two `Measured` rows, 72 → 74, and they are **the first whose
ratio depends on the input distribution rather than on the workload**. The Vyrn
implementation is `||`/`&&` over four lane reads, which short-circuits: over
`simdbench.vyrn`'s monotonic array the chain bails at lane 0 almost every pass and
the branch is predicted perfectly, so the builtins win only 1.3x (`anyTrue`) and
2.3x (`allTrue`) natively. Rebuild `data` to return unpredictable lanes and the
same benches say **2.5x** (1356 ms against 543 ms) and **2.4x** (1170 ms against
481 ms); wasm is 1.2x either way. The rows quote the unpredictable number, because
1.3x is the bar `select` already failed to clear and sorted input is the unusual
case for a predicate — a row that quoted the friendly number would be claiming a
justification the general case does not support.

**What each engine does for NaN, measured rather than assumed.**

| | `min(NaN, 1.0)` | `min(1.0, NaN)` | `min(-0.0, +0.0)` | `NaN < 1.0` | `NaN != NaN` |
|---|---|---|---|---|---|
| wasm `f32x4.min` | NaN | NaN | `-0.0` | false | true |
| LLVM `llvm.minimum` | NaN | NaN | `-0.0` | — | — |
| LLVM `llvm.minnum` | **1.0** | **1.0** | either | — | — |
| Rust `f32::min` | **1.0** | **1.0** | either | — | — |
| `fcmp olt` / `une` | — | — | — | false | true |

The rule shipped is **IEEE-754-2019 `minimum`/`maximum`: NaN in either operand
propagates, and `-0.0` orders strictly below `+0.0`** — wasm's, because wasm is
the engine with no choice. Native calls `llvm.minimum` (not `llvm.minnum`) and
the interpreter spells the rule out by hand (not `f32::min`); a structural test,
`min_and_max_lower_to_the_nan_propagating_intrinsic`, pins the intrinsic name so
a future "simplification" to `minnum` fails in the default suite rather than only
under `--ignored` parity.

**This is the case M1 handed forward and it is genuinely different from M1's.**
M1's agreement came free because a NaN's *identity* is unobservable — the
six-decimal formatter answers `NaN` for any payload and either sign. `min` is not
of that kind: `minNum` and `minimum` disagree about *which operand comes back*,
so `min(NaN, 1.0)` prints `1.000000` under one and `NaN` under the other, and the
formatter shows it. Left to defaults the three engines would have split
two-to-one. Comparison needed no such intervention: wasm's `f32x4.lt`..`ge`/`eq`
are already the ordered predicates and `f32x4.ne` the unordered one, which is the
`fcmp olt`/`fcmp une` pairing RFC-0081 corrected at scalar width — inherited, but
pinned rather than assumed. `abs` clears the sign bit on all three (a bit
operation, so there is no NaN question), and `sqrt` is correctly-rounded IEEE on
all three, `-0.0` sign kept and subnormals not flushed.

**The bounds check is signed, and that is not a stylistic choice.** The scalar
path gets both ends from one unsigned compare, because a negative index reads as
a huge one. A span cannot: `i + 4` on that huge value wraps back into range and
would let the access through. So the check is `i < 0 || i > len - 4`, where
`len - 4` cannot wrap because `len >= 0` — two compares, **one branch**. The
reported index is the first lane actually out of range (`i + 3` when the tail
overruns, `i` when the head is negative), computed inside the trap block so the
hot path pays nothing for it; naming `i` alone would name an element that exists.
The wording is the scalar one, since a vector access is still an array access.
`vyrn-cli/tests/simd.rs` counts the checks in the IR — one for a vector load,
**four** for the same four elements read scalarly, which is the amortisation
stated as a number rather than as a claim.

**The surface lives on the type name, and that is a parser constraint rather than
a taste.** A value-receiver method (`v.min(w)`) is a *global* rename in the
parser's method table, and `min`/`max`/`abs` are `std/math` exports that
`math.min(a, b)` reaches in exactly the same AST shape — renaming them would
break namespace calls. So `F32x4.<anything>(..)` became one arm that drops the
type-name receiver and prefixes `@f32x4`, replacing M1's `splat`-specific guard;
M3's `F64x2.*` is a table entry, not new machinery. `lane` stays a value method
because M1 made it one and nothing exports that name. Comparison is the
*operators* for the same reason: six arms instead of six global renames, and
`if a < b` on vectors fails with "condition must be `Bool`".

**M2's finding: `select` is not a builtin, and the census is why.** A
`v128.bitselect` / `select <4 x i1>` lowering was built, worked, and was then
deleted. Written in Vyrn on `m.lane(k)` and `if` it measures **1.1x native**
(7.08 µs against 6.33 µs over 65536 lanes) and **1.06x wasm** (282 ms against
268 ms over 400 000 passes) — both optimizers put the four branches back together
into one blend. RFC-0078's census asks every Rust primitive to say why it is one,
and "it is 6% faster" is not an answer; `examples/simdmem.vyrn` defines its own
`select` in six lines. The measurement is `examples/simdbench.vyrn`, which also
caught its own first version measuring nothing: without a `blackBox` on the
threshold the whole pass is loop-invariant and LLVM hoists it, so the benchmark
timed 64 float additions and reported confident ratios to match.

The census gains six rows and 64 → 70. `@f32x4Load`/`@f32x4Store` are **Memory**
(array access with a bounds trap, exactly as `at` is). `@f32x4Min`/`Max`/`Abs`
are **Measured** — all three *are* writable in Vyrn, `simdbench.vyrn` holds the
implementations and `main` checks all three engines agree with the builtins, at
3.6x / 3.7x / 1.0x native. `abs`'s row is the interesting one: natively LLVM
recognises the `Float64`-widen / mask / narrow round-trip and emits the same
code, so the number that refuses it is **Cranelift's 3.5x** — the first census
row decided by the wasm column. `@f32x4Sqrt` is **Semantics**: it is not movable
at all, because no finite sequence of Vyrn arithmetic is the correctly-rounded
IEEE result and a Newton iteration differing in the last bits is, under this
project's promise, a different program. The comparison operators are `BinOp`s
like M1's arithmetic, so the mask cost no rows.

**The surface M2 left half-finished, and the one genuine hole in it.** A mask
could be *produced* and *reduced* and never **combined**: `(a < b) && (c < d)`
was inexpressible, because `&&` is a `Bool` operator and nothing joined two
masks. That is the difference between expressing a predicate and expressing a
predicate with two conditions, and it was the only gap here that changed what a
program can say rather than how conveniently it says it.

**The spelling is `& | ^ ~`, and `&&`/`||`/`!` were rejected for a reason that
is about promises rather than taste.** Vyrn's `&&` and `||` **short-circuit** —
the right operand may not be evaluated at all — and a lane-wise combination
evaluates both sides always, four times over. Borrowing that spelling would
advertise a control-flow property the operation does not have, in the one place
a reader would most want to rely on it (`expensive(x) && cheap(y)`). The bitwise
family promises only "both sides, always", which is exactly what this is; it is
what RFC-0045 already means on integers, and it is already how a mask is stored
on both backends. Methods (`m.and(n)`) were the other candidate and lose twice:
`and`/`or`/`not` are three global renames in the parser's method table for names
a future `std/bool` or `std/arrays` would want, and every other lane-wise
operation on this type — arithmetic, all six comparisons — is already an
operator. `v128.andnot` exists and is deliberately given no spelling: `a & ~b`
is one instruction more and nothing measured asked for it.

| engine | how |
|---|---|
| interpreter | `&&`/`\|\|`/`!=` over four `bool`s — the reference answer |
| textual | `and`/`or`/`xor <4 x i32>`, and `xor` against all-ones for `~` |
| direct wasm | `v128.and`/`or`/`xor`/`not` |

The textual backend needed **no code at all** for the binary three: a
`Mask32x4`'s `llt` is `<4 x i32>`, the integer arm already emits `and {ll}`, and
the result type is the operand type. That is the mask's representation decision
paying for itself a second time. The wasm side does need its own arm, because
`v128.*` are bit operations on 128 bits with no lane width — which costs nothing
here for the closed-inhabitants reason `any_true` already leans on, and which is
pinned by **De Morgan lane-wise** in `examples/simdmem.vyrn`: a whole-vector
complement satisfies `~(m & n) == ~m | ~n` for all-true and all-false and
diverges exactly at a mixed mask.

**Three smaller completions, in the order they bite.**

- **`v.replaceLane(k, x)`** — `lane` read and nothing wrote, so building a vector
  from computed values meant going back through the four-argument constructor to
  change one lane. Same constant-index rule, same absence of a bounds check. A
  *value* method rather than `F32x4.replaceLane(v, k, x)`, which is the rule this
  RFC already stated read the other way: the type name is where an operation
  lives when something *else* exports the name, and nothing exports this one —
  the same two reasons `lane` is a value method. Masks are refused: a mask lane
  write would be a second way to *build* a mask, and while a `Bool` source keeps
  every lane all-ones or all-zeros (so the closed-inhabitants argument would
  survive it), nothing needs it.
- **`-v`** — previously `F32x4.splat(0.0) - v`, **which is a different
  function**. Negation flips the sign bit; the subtraction does not: `0.0 - 0.0`
  is `+0.0` where `-(+0.0)` is `-0.0`. `examples/simd.vyrn` prints both lines so
  the difference is output rather than a claim. It is `fneg <4 x float>` /
  `f32x4.neg` / Rust's unary `-`, all three of which are IEEE `negate`.
- **`ceil`/`floor`/`trunc`/`nearest`** — on the type name, one step ahead of the
  `min`/`max`/`abs` rule rather than behind it: nothing exports `ceil` *today*,
  but those are exactly the names a float section of `std/math` would want, and
  taking a value-method name is taking it globally. The three toward-a-direction
  rounds differ from each other only in sign handling, so every one is pinned
  against a negative as well.

**`nearest` is roundTiesToEven, and this is the `minnum` bug one operation
over.** wasm's `f32x4.nearest` is roundTiesToEven. LLVM's `llvm.round` and Rust's
`f32::round` are roundTiesAwayFromZero — a **different function**, not a faster
spelling of one — and they agree everywhere except at an exact half, which is
precisely where a wrong choice hides. Measured before anything was chosen rather
than after:

| on `<0.5, 1.5, 2.5, -2.5>` | result |
|---|---|
| wasmtime `f32x4.nearest` | `0 2 2 -2` |
| `llvm.roundeven.v4f32` | `0 2 2 -2` |
| `llvm.rint.v4f32` | `0 2 2 -2` |
| **`llvm.round.v4f32`** | **`1 2 3 -3`** |
| Rust `f32::round_ties_even` | `0 2 2 -2` |
| **Rust `f32::round`** | **`1 2 3 -3`** |

Left to defaults the three engines would have split two-to-one, exactly as they
did for `min`. All three were pointed at roundTiesToEven instead, and
`examples/simdround.vyrn` leads with the halves — `0.5 1.5 2.5 3.5` and
`4.5 5.5 6.5 7.5`, both signs — because that is the only place the two rules
differ. `nearest_lowers_to_ties_to_even_and_not_to_ties_away` in
`vyrn-cli/tests/simd.rs` pins the intrinsic name so a "simplification" to
`llvm.round` fails in the default suite rather than only under `--ignored`
parity, which is the shape `min_and_max_lower_to_the_nan_propagating_intrinsic`
already has.

**The intrinsic emitted is `llvm.rint` and not the one that names the rule, and
the reason is linking rather than semantics.** `llvm.roundeven.v4f32` has no
lowering on baseline x86-64 — `roundps` is SSE4.1 and `vyrn build` passes clang
no `-march` — so it scalarizes to four calls to `roundevenf`, a C23 symbol the
MSVC UCRT does not ship, and the **link fails outright**. `llvm.rint` lowers to
`rintf`, which every libc here has, and under the default rounding mode it *is*
roundTiesToEven. Vyrn has no `fenv` surface; a host that changed the rounding
mode behind an `extern` would already have moved every `fadd` in the program, so
this adds no hole that was not there. If the native baseline is ever raised, the
halves above are what will say immediately whether the switch back was right.

**The four roundings are the first census block the *wasm* column decides
outright, and one of them the builtin loses.** The same scalarization applies to
`ceil`/`floor`/`trunc`: each is four libc calls natively, read out of the
assembly. So against `examples/simdbench.vyrn`'s Vyrn implementations, per 65536
lanes, `ceil` is **1.0x** (53.4 µs against 50.3 µs) and `floor` **1.0x** (54.2
against 50.2); `nearest` is 1.9x (53.8 against 101.7, ties-to-even by hand being
twenty lines); and **`trunc` is 0.43x — the builtin is 2.3x *slower*** (102.5 µs
against 44.4 µs), because four `truncf` calls lose to the inlined `cvttss2si`
round-trip LLVM compiles the Vyrn version into. On wasm the same four are 7.4x,
8.2x, 9.3x and 4.9x, Cranelift emitting the instruction. `abs` was recorded above
as "the first census row decided by the wasm column"; this is four more, and the
first where the native number is not merely unhelpful but negative. **The upgrade
path is a native baseline of `x86-64-v2`**, which would make all four one
instruction — a project-wide ISA decision (Penryn, 2008), not this RFC's to take.
`trunc` ships anyway: three roundings and a hand-written fourth is a surface with
a hole in it, and the Vyrn version needs `floatBits` to keep the sign of a zero
(`Int64(-0.5)` is `0`, whose `Float32` is `+0.0`).

**Relaxed SIMD must never be added, and it is the fastest thing on the list.**
`F32x4RelaxedMadd`, `RelaxedNmadd`, `RelaxedMin` and `RelaxedMax` are in the
encoder beside everything else here, and a relaxed multiply-add is the single
biggest win available in this instruction set. They are **deliberately
implementation-defined**: a relaxed madd *may or may not* fuse, so the result
differs in the last bits by host, and `RelaxedMin`/`Max` leave the NaN and
signed-zero behaviour to the engine — the exact question M2 had to answer by
hand for `min`. That is categorically incompatible with byte-identical parity,
for the same reason auto-vectorised float reductions are: not "hard to get
right", but *specified* to be allowed to differ. This paragraph exists because
someone will reach for them, and "wasm has it, and it is faster" is the argument
RFC-0078's census exists to refuse.

The five new census rows are `@replaceLane` (`View`, beside `@lane` — a lane
written instead of read) and the four roundings (`Measured`, with the numbers
above), 74 → 79. **The mask combinators and `-v` cost no rows at all**: they are
a `BinOp` and a `UnOp`, which the interpreter's `Call` dispatch never sees — the
same reason M2's comparison operators did not appear there either.

Three engines byte-identical across `examples/simd.vyrn` (now including
`replaceLane`, `-v`, and `-v` beside `splat(0.0) - v`), `examples/simdmem.vyrn`
(the combinators, De Morgan lane-wise, `^` against itself and against its
complement, and NaN-derived masks through all four), the new
`examples/simdround.vyrn` (the halves both signs, either side of a half by one
ulp, `±0.0` through every round, NaN in each lane position one at a time, `±∞`,
a subnormal of each sign, and the range above 2^23 where a `Float64` round-trip
would fail) and `examples/simdbench.vyrn` — stdout, stderr and exit code.
`wasmtime -W simd=n,relaxed-simd=n` still rejects every one of the modules.

Two smaller things the RFC above got wrong or left out:

- **The milestone list says "lane-wise comparisons producing a mask" as though
  the mask were an implementation detail.** It is the one type decision in the
  whole RFC, and M3's `F64x2` will need `Mask64x2` — two lanes, and a `v128`
  again. The naming convention (`Mask32x4` after Rust's `mask32x4`) is set here.
- **"`min`/`max`/`abs`/`sqrt`" reads as four operations of one kind.** They are
  three kinds: two that needed a semantics chosen for them, one that is a bit
  operation with no question to answer, and one that is the only irreducible
  primitive in the whole RFC.

### As landed — M3 (`I32x4` only)

```vyrn
let a = I32x4(1, 2, 3, 4)          // + - * (no /), - and ~ unary
let b = I32x4.splat(10)            // & | ^ DIRECTLY, not through a mask
let m = a < b                      // Mask32x4 — the same one F32x4 yields
let v = I32x4.load(xs, i)          // xs: Array<Int32>, ONE bounds check
I32x4.store(xs, i, v)
a.lane(0)  a.replaceLane(2, 99)    // the same constant-index rule
```

`F64x2` and `I64x2` are **not shipped**, on the licence this milestone gave
itself ("free to ship one"), and each for its own reason. `I64x2` has no
`min`/`max` at all, no `MulHigh`, and no `AllTrue` before the relaxed proposal —
the RFC already called it the thinnest, and after M3's finding below there is
nothing left in it but the representation. `F64x2` would be the substantial one
and it needs a `Mask64x2` (M2's note, still correct: two lanes, still a `v128`),
so it is a milestone with a type decision in it rather than a table entry.

**The signedness question is answered by the lane type, and the answer is that
there is only one right instruction rather than a choice between two.** The
lanes are `Int32`, so `i32x4.lt_s` is the comparison and `lt_u` is not an
alternative spelling — it is the operation a `U32x4` would name. Reaching the
unsigned half needs a second *type*, because the choice belongs to the operand
and not to the operation; a `minUnsigned` beside a `min` would be two answers to
a question the type had already answered. `U32x4` is therefore named and not
proposed, in the shape this RFC uses for reinterpretation across widths. What
makes this checkable rather than asserted is that **the two rules disagree only
across the sign bit**: `Int32.min < 1` is true signed and false unsigned, and
`examples/simdint.vyrn` prints that lane through every comparison.

**The mask is `Mask32x4`, shared and not duplicated.** M2's note warns that
`F64x2` would need a `Mask64x2`, and the reason is *lane width* — which is
exactly what `I32x4` does not change. Four lanes of 32 bits produce four answers
in a `<4 x i32>` / `v128` of all-ones and all-zeros, bit for bit what a float
comparison produces, so the closed-inhabitants argument `anyTrue`/`allTrue` rest
on carries over unchanged rather than being re-argued. The consequence is that
`(edge < ones) & (f < F32x4.splat(2.5))` type-checks, which the example prints:
a mask is four booleans about four lanes, and nothing in it remembers what
compared them.

**The wrap matches the scalar, and it was never at risk of not doing so.** wasm
has saturating adds, but only at i8 and i16, so at this width there was nothing
to pick wrongly — `i32x4.add`, `add <4 x i32>` and `i32::wrapping_add` all wrap,
and the only way to break it would be an `nsw` flag on the textual side, which
`integer_lane_compare_is_signed_and_the_add_does_not_promise_no_overflow` pins in
the default suite along with the signed compare. `-Int32.min` is `Int32.min` on
all three; the example prints the scalar `Int32` answer on the line above each
vector one, so a vector that saturated where the scalar wraps would be two
different numbers rather than a claim.

**What M3 refuses, and the number that refuses it.** `i32x4.min_s`, `max_s` and
`abs` all exist in the encoder, all three were built end to end, and all three
were **deleted**. Natively LLVM compiles the Vyrn `if a < b` into the same
`pminsd`: 5.98 µs against 5.98 µs per 65536 lanes, the builtin marginally
*behind*. On wasm, over 200 M lanes, the builtin walk is 139 ms and the Vyrn one
146 ms — **1.05x**, with `max` at 1.14x and `abs` at 1.04x. `select` was refused
at 1.06x, so this is the bar the RFC had already set. The reason the two widths
answer differently is worth stating because it generalises: what earns
`F32x4.min` its row is the NaN rule and the signed zero, twenty lines of
`floatBits` a program would have to get right; an integer `min` is one
comparison, and there is nothing there for a builtin to be faster at.

**So M3 adds four census rows and not one is `Measured`, 79 → 83.** `I32x4` and
`@i32x4Splat` are `View` (the representation, as M1's three were), `@i32x4Load`
and `@i32x4Store` are `Memory` (a bounds trap, as `at` is). `@lane` and
`@replaceLane` serve both widths from one arm each — a lane accessor is about the
lane *index*, and that rule did not change. Every operator is a `BinOp` or a
`UnOp` the interpreter's `Call` dispatch never sees, which is why `+ - * & | ^ ~
-` and all six comparisons cost nothing here, exactly as M1's arithmetic and M2's
comparisons did. A whole width for four rows is what it looks like when the
census is asked before the arms are written rather than after.

**The measurement method changed, and that is a finding about the earlier rows
rather than only about these.** Cranelift does not inline across wasm functions,
and the Vyrn half of every benchmark in `examples/simdbench.vyrn` is spelled as
four calls to a one-lane helper. For a long helper that is a rounding error; for
a one-comparison helper it is the entire measurement. `I32x4.min` reads **2.0x**
wasm with the helper spelling and **1.05x** written inline, and only the second
number is about the instruction. Re-timing M2's `@f32x4Abs` the same way says the
same thing: 152 ms builtin, 415 ms via the helper (2.7x), **156 ms written inline
(1.03x)** — so the row that reads "1.0x native but 3.5x wasm ... which is what
keeps it" is quoting a call Cranelift did not inline. **That row is left alone
here on purpose**: deleting a shipped builtin is a language change that owes its
own milestone and its own parity run, not a side effect of M3. It is recorded so
the next reader has the number, and the four rounding rows deserve the same
re-timing (their Vyrn halves are twenty lines, so they are the likeliest to
survive it).

**Three engines byte-identical on `examples/simdint.vyrn`** — construction,
splat, both lane accessors with every lane replaced in turn (the lane-order pin),
`+ - *` and `-` at both ends of the range with the scalar `Int32` printed beside
each, `65536 * 65536` and `Int32.max * Int32.max` (both unrepresentable, so both
are the wrap and nothing else), `-Int32.min`, all six comparisons against
`Int32.min` and `Int32.max`, both mask reductions, a mask combined across the two
widths, `& | ^ ~` including De Morgan lane-wise and `~` across the sign bit, and
a load/store round trip carrying the boundary values through memory (where a
sign-extending load would go wrong and nowhere else). The full sweep is 102
examples, three engines, stdout/stderr/exit code, run serially with the wasm
column confirmed present — the harness prints `no wasmtime` when it is not, and
did not. `wasmtime -W simd=n,relaxed-simd=n` rejects the module with `SIMD
support is not enabled`.

Four smaller things, in the order they bite:

- **No `/`, and it reports itself by name.** `Div` on two `I32x4`s gets its own
  checker arm rather than falling through to "arithmetic needs matching numeric
  operands", which would be a confusing thing to say about two operands that
  plainly match. There is no `i32x4.div` because no hardware has SIMD integer
  divide.
- **`<<` and `>>` are refused, and this is the one refusal that is about
  semantics rather than a ratio.** wasm's `i32x4.shl` **masks** the count mod 32,
  LLVM's `shl <4 x i32>` is **poison** past the width, and Vyrn's scalar `<<`
  **traps** (RFC-0045). Three answers to one question, and the vector spelling of
  an operation must not mean something different from the scalar one. Demanding a
  constant count — `lane`'s rule, which would make it total — works, and would
  also make it the only binary operator in the language whose right operand must
  be a literal. Nothing measured asked for it.
- **No conversion between the widths.** `i32x4.trunc_sat_f32x4_s` and
  `f32x4.convert_i32x4_s` exist and are a *conversion*, not the free
  reinterpretation this RFC already recorded as needing an explicit operation.
  Neither is proposed; named so the absence is a decision.
- **No second trap example.** `@i32x4Load`/`@i32x4Store` share the float pair's
  bounds check literally — one `if` in each backend, since the element stride is
  4 either way — so `examples/simdoob.vyrn` already covers both branches. A
  duplicate would pin the same code twice and read as though it pinned more.

One correction to the census's own scanner. The store guard is
`if name == "@f32x4Store" || name == "@i32x4Store"` — one body, two widths — and
the scan in `primitives.rs` anchored on the literal `if name == "`, so it saw
only the first and reported the second as a stale row. The needle is now
`name == "`; every match in that region is a guard, and the existing assertion is
what says so if that ever stops being true.

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
