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

### As landed

`f64Str` is in `std/num`, beside `parseFloat64` and over the same `Dec`-shaped
arithmetic run backwards. Nothing is deleted and nothing is routed to it.

**Correctness held on the first run, on every engine.** It is pinned three ways:
the module's own `test` blocks on the values the RFC named, `f64Str` against
Rust's `{:.6}` over 850 bit patterns (`tests/numbers.rs`, which reaches
subnormals, both zeros and every exponent because a bit pattern can name values
no literal in this language can), and — the one that matters — 400 doubles
formatted twice inside one program and compared against **each engine's own**
`@str`, on interpreter, native and wasm (`tests/parity.rs`). Zero mismatches in
all three. The wasm column is asserted present rather than skipped when
`wasmtime()` returns `None`, because a green run that never tested wasm proves
nothing about the 511 lines.

One departure from `f64_str`'s algorithm, and it is what makes the loop
affordable: `f64_str` multiplies by two or by five `reps` times and `reps` reaches
1074, so this puts in the largest chunk of the power that keeps
`limb × mul + carry` inside an `Int64` (`2^40`, or `5^18`). That is `halveBy`'s
trick in the other direction and worth about the same.

**"Reuse `Dec`, `tidy`, `halveBy`, `twiceBy`" was wrong, and worth saying why.**
The two directions have the same *shape* — a digit array and an exponent — and
opposite *invariants*. `tidy` caps at 800 digits and sets a sticky flag, which is
sound for parsing (past 800 digits nothing but a tie can change which double you
land on, and the flag decides the tie) and unsound for formatting, where a
subnormal's exact expansion is 1074 digits and every one of them is a digit that
must be examined before the sixth place is settled. `halveBy` and `twiceBy` also
carry one decimal digit per `Int64`; base-10^6 limbs do six, and given the
interpreter number below, six-at-a-time was not optional. So this shares the
file and the idiom with `parseFloat64`, not its functions.

**Gate 1 — the microbenchmark.** 200,000 formats, an identical loop without the
format subtracted, minimum of five runs:

| engine | builtin `@str` | `f64Str` | |
|---|---|---|---|
| interpreter | 314 ns | 56,700 ns | **180x** |
| native | 240 ns | 750 ns | 3.1x |
| direct wasm | 246 ns | 721 ns | 2.9x |

The compiled bar was 330 ns hand-written and the RFC expected "somewhat slower";
3x is that, and it is the price of call overhead, bounds checks and three heap
arrays where the hand-written version has a frame. The interpreter is a different
finding: 180x is not a defect (`push` was checked and is O(1); the digit loop is
linear) but the arithmetic of the answer, run one `Val` at a time, against a
`format!` that is one call into Rust.

**Gate 2 — the existing corpus, which is the gate that matters.** Instrumented
rather than assumed — the interpreter's float arm was made to count itself:

- `vyrn check` with the gen cache off on `twdemo`, `vyxdemo` and `shelf`'s client
  formats **zero** floats. No generator in `std/` turns a `Float64` into text at
  all, so the compile path cannot move, and there is no before-and-after to
  report because there is no difference to measure.
- The entire 60-program example corpus performs **46** float formats in total,
  across every program in it.
- The shape that WOULD show it — 200,000 `print(x)` of a float, output to a file
  — is dominated by the write, not the format: native 2621 → 2620 ms, wasm
  2626 → 2655 ms (+1%), both inside run-to-run noise. **The interpreter goes
  2215 → 13653 ms, 6.2x**, which is the one real-program regression M1 found.

So the gate passes for native and for direct wasm — outright, on both numbers —
and fails for the interpreter on a program that prints floats and does nothing
else, while being invisible to everything in the repo today.

That splits M2 in a way the RFC did not anticipate, and the split is worth
stating plainly: the two implementations that are *algorithms* — the native
`printf` selection and the 511 hand-written lines — cost 3x on a microbenchmark
and nothing observable in a program, and should go. The interpreter's arm is not
an algorithm; it is `format!("{f:.6}")`, one line, and it is the **oracle** the
other two were made to match. Replacing it buys one line and costs 6x on
interpreted float printing.

## M2 — delete the three

Only after M1's gate. Route `@str`'s float case to `num$f64Str`; delete the
interpreter's arm, the native path's `printf` call, and `direct.rs`'s 511 lines
plus its `Rt` slot. The census row leaves `Measured`.

**M1's numbers amend this.** The gate passed for native and wasm and failed for
the interpreter, so the three deletions are no longer one decision. Deleting the
`printf` selection and the 511 lines is bought and paid for. Deleting the
interpreter's `format!("{f:.6}")` costs 6.2x on an interpreted program that
prints floats, and what it buys is one line — a line which is also the oracle
`tests/numbers.rs` compares against. Two implementations where one is the
reference for the other and a differential test says so is a different
arrangement from three peers that must agree, and it is the arrangement M1's
numbers argue for.

The `Rt` slot removal is the one real hazard and it is known: every runtime
function in `direct.rs` is `base + n` and the emission order must match, so
removing a slot renumbers the table. RFC-0078 flagged this same hazard for
`charCount` and it was survivable there.

## What this does not touch

**Integer `toString`.** `@str` covers more than floats; only the float case is
an algorithm, and only the float case is written three ways.

**`@concat`**, the other `Measured` row. Same category, different question, and
its cost has not been re-measured.
