# RFC-0081 — Float Formatting in Vyrn

- **Status:** **Accepted, with a measurement gate.** M1 is a spike whose number
  decides whether M2 happens at all.
- **Depends on:** RFC-0078 (the census, and the `Measured` refusal this
  reopens), RFC-0077 (the direct backend, which is why the 511 lines exist),
  RFC-0059 (`std/num`, which already carries the arithmetic)
- **Reopens:** RFC-0078's `("@str", Measured, …)` row. That row says *movable,
  refused on a measured cost*. The cost was re-measured and is not there.

## Why this one is worse than `slice`

`slice` was one algorithm written three times. `@str` is **three different
algorithms that must produce identical bytes**:

| engine | implementation |
|---|---|
| interpreter | Rust's `{:.6}` |
| native | C's `printf("%f")` |
| direct wasm | 511 hand-written lines, because wasm has no `%f` |

All three must convert the double's **exact** decimal value and round
half-to-even at the sixth place, so `0.0078125` prints `0.007812` (the kept `2`
is even) and `1e300` prints 301 integer digits. They agree because someone made
them agree, and the parity suite notices when they stop. Nothing structural
holds them together.

That is a strictly worse arrangement than triplication of one algorithm, because
the three cannot be diffed against each other. It is the most fragile thing left
in the compiler.

## The measurement that reopens it

200,000 formats, minimum of three runs, with an identical loop lacking the
`toString` subtracted to isolate the formatting:

| engine | formatting only | per call |
|---|---|---|
| interpreter | 87 ms | ~435 ns |
| native | 65 ms | ~325 ns |
| direct wasm | 66 ms | ~330 ns |

**The hand-written exact-decimal algorithm is within 2% of `printf`.** Whatever
"refused because it is on every print" was protecting, it was not protecting a
fast path — the expensive-looking implementation is already the one running, and
it costs the same as the C library.

## The substrate already exists

Moving this needs **no new primitive**, which is the part that makes it worth
doing now rather than later:

- **`floatBits` and `floatFromBits`** are already census `View` rows (RFC-0078
  M4a). Decomposing a double into sign, mantissa and exponent is available to
  Vyrn today.
- **`std/num` already carries the exact-decimal arithmetic** — `Dec`, `tidy`,
  `halveBy`, `twiceBy` — written for `parseFloat64`. Formatting is the inverse
  direction over the same representation, and `f64_str`'s own doc comment
  describes needing exactly this: multiply by 2 while `E >= 0`, by 5 while
  `E < 0`, then round the tail.

So `f64Str` belongs beside `parseFloat64` in the file that already knows how to
do decimal arithmetic on a binary float.

## M1 — the spike, and the gate

Write `f64Str` in `std/num`. Do **not** delete anything yet.

**Correctness is the whole milestone**, and it is pinned against the three
implementations that exist rather than against a specification:

- `0.0078125` → `0.007812` (round half-to-even keeps the even digit)
- `1e300` → 301 integer digits, exactly
- denormals, including the smallest positive double
- `±0.0` (the sign survives), `NaN`, `±Infinity` — and note the existing
  backends spell these deliberately; match the bytes, do not invent them
- `Float32`, which widens rather than having its own path
- a randomized differential run: N doubles, Vyrn output versus the builtin's,
  byte for byte, on all three engines

**The gate is two numbers, both recorded whatever they say:**

1. The microbenchmark above, re-run against the Vyrn version. The hand-written
   wasm is 330 ns; that is the bar, and it is a *hand-written* bar, so a
   compiled-Vyrn version paying call overhead, bounds checks and digit-array
   allocation is expected to be somewhat slower.
2. **The existing benchmark corpus, before and after** — the gate that actually
   matters, and the one M3 used for `slice` (twdemo 67→68 ms, shelf
   160→159 ms). A microbenchmark regression nobody can observe in a real program
   is not a reason to keep three algorithms.

If M1's numbers are acceptable, M2 follows. If they are not, M1 still pays for
itself: the census row stops saying "refused on a measured cost" and starts
carrying the actual number, which is what it should have said in the first
place.

## M2 — delete the three

Only after M1's gate. Route `@str`'s float case to `num$f64Str`; delete the
interpreter's arm, the native path's `printf` call, and `direct.rs`'s 511 lines
plus its `Rt` slot. The census row leaves `Measured`.

The `Rt` slot removal is the one real hazard and it is known: every runtime
function in `direct.rs` is `base + n` and the emission order must match, so
removing a slot renumbers the table. RFC-0078 flagged this same hazard for
`charCount` and it was survivable there.

## What this does not touch

**Integer `toString`.** `@str` covers more than floats; only the float case is
an algorithm, and only the float case is written three ways.

**`@concat`**, the other `Measured` row. Same category, different question, and
its cost has not been re-measured.
