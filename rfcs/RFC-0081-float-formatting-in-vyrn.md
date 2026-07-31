# RFC-0081 — Float Formatting in Vyrn

- **Status:** **Shipped** (M1, M2). Two of the three implementations are gone;
  the interpreter's is kept on purpose, as the oracle — see the closing section.
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

### As landed

Two deletions and one deliberate non-deletion, exactly as the section above
amends it. `@str`'s float case and `print`'s both call `num$f64Str` on the
textual emitter and on the direct backend; the interpreter's
`format!("{f:.6}")` is untouched and is now the oracle rather than one of three
peers.

**Gone:** the native `printf("%f")` selection (four sites — `print` and `@str`,
each times `Float64`/`Float32` — with `@.fmt.f`, `@.fmt.lf`, `@.fmt.nan`,
`@.str.nan` and the `fcmp uno` that chose `NaN` over UCRT's `-nan(ind)`), and
`direct.rs`'s 511-line `float_str` with its `Rt` slot and the three interned
words it took. 608 lines deleted against 217 added.

**The `Rt` hazard did not bite, and the reason is worth recording**: the table
already checks itself. Every helper is emitted behind `rt.next_is(m, rt.<slot>)`
and `next_is` asserts the reservation against `Module::next_func`, so a
misnumbering is a panic at the helper rather than a wrong call at runtime — the
silent case the RFC feared was engineered out three milestones ago.

**What the RFC got wrong, and it is the whole shape of the milestone:** "route
`@str`" is not enough. `print` formats a float without going through `@str`, so
routing only `@str` would have left the native build with two float formatters
(one of them `%f` again) and the direct backend calling a function it no longer
had. Both spellings route, which means `std/num` is injected into nearly every
program in the repo — the loader's gate is a mention of `@str` **or** `print`.
Two consequences that were measured rather than assumed:

- **The direct backend sweeps** (`Module::sweep`, RFC-0077), so a program that
  formats no float carries none of it: hello-world wasm went 1263 → 1264 bytes.
  A program that does pay for what it uses (+4.5 KB).
- **The textual emitter does not sweep** — it emits every function of every
  linked module and leaves dead-code removal to clang — so every native binary
  grows by ~25 KB whether or not it formats anything (173 → 199 KB on
  hello-world). That is dead weight in the `.exe` and the knob for it is
  `-ffunction-sections -Wl,--gc-sections` on the link, deliberately not pulled
  here: it changes every native build and this milestone is about deleting an
  algorithm, not about the linker.

Two smaller things the RFC did not anticipate:

- **A `.rodata` pointer must never reach `free`.** `own.rs` frees an `@str`
  result (`DropKind::FreeStr`), which was sound while `@str` always `malloc`'d —
  and `f64Str` returned `"NaN"`, `"inf"` and `"-inf"` as literals. So `f64Str`
  now builds those three out of bytes like every other answer, and says in its
  doc comment that every return is a fresh allocation. This is the one place the
  two directions of RFC-0078's thesis collide: a Vyrn function reached by a
  builtin's desugar inherits the builtin's ownership contract.
- **A runtime module that cannot be READ is now skipped rather than failing the
  load.** The loader already treated an unresolvable std root that way ("the
  diagnostic belongs to whoever needs it"); with `print` in the gate, every
  program with a partial resolver — six in-memory generator tests, and any
  editor serving a subset of the tree — reached it. A module that is present but
  broken still fails the load.

**Verification.** `cargo test --workspace` green; `vyrn-lsp` green separately
(56). Full parity serially: 31 passed, 0 failed, 0 skipped, and the wasm column
is proven rather than assumed three ways — `three_engines`'s assertion that a
`wasm` row exists (M1 put it there for exactly this milestone) passed, the
example harness printed no `no wasmtime` note over its 86 programs, and
`f64str.wasm` is sitting in the parity temp directory. M1's differential tests
pass unchanged; their doc comments now say what two of the three columns mean
after M2, which is that they compare `f64Str` against itself and the interpreter
is the only differential left in that file. `tests/numbers.rs` is unaffected and
is the stronger of the two: 850 bit patterns against Rust's `{:.6}` in Rust,
under the interpreter.

**The numbers, re-measured.** 200,000 formats, an identical loop without the
format subtracted, minimum of five runs, per call:

| engine | before (M1's builtin) | after | |
|---|---|---|---|
| interpreter | 385 ns | 430 ns | unchanged by design (noise + one more module in the link) |
| native | 170 ns | 860 ns | **5.1x** |
| direct wasm | 380 ns | 830 ns | **2.2x** |

The native ratio is worse than M1's 3.1x and the wasm ratio better than its
2.9x, for the same reason in both directions: this is `x.toString()` against
UCRT's `%f` at `-O2` on ordinary magnitudes, where `printf` is at its best and
the 511 hand-written lines were not.

**And the workload M1 could not show, because M1's print benchmark was dominated
by the write** — 50,000 `toJson` of a three-`Float64` record, 150,000 formats,
one line of output:

| engine | before | after | |
|---|---|---|---|
| native | 109 ms | 205 ms | **1.9x** |
| direct wasm | 119 ms | 188 ms | **1.6x** |
| interpreter | 1227 ms | 1297 ms | +6% |

**That is a regression a real program could notice, and it is stated plainly
rather than filed under noise.** A service serialising float-heavy JSON pays
about 640 ns per float on native where it paid 170. In absolute terms a response
carrying a hundred floats moves from 17 µs to 86 µs, which is why this landed
rather than stopping — but it is a real number and the split decision is
revisitable on it. What is *not* revisitable by this number is the direct
backend: its alternative is 511 hand-written lines, and it is the engine that
got 40% of the regression the native one did.

One operational consequence: a `vyrn` binary that cannot find a std root can no
longer compile a program that prints a float. That is the same failure `toJson`
has had since RFC-0078 M2b, and both backends name the missing function rather
than leaving an undefined symbol for the linker.

### The thesis, corrected by its own measurement

This RFC opened by saying three implementations that must agree is worse than
one. M1's numbers say that was the wrong count to object to. **The problem was
never "more than one" — it was "N peers with no reference among them."**

Three peers agree because someone made them agree, and each is as likely to be
the wrong one. Two implementations where one is *designated the oracle* and a
differential test enforces the relation is a different structure, not a weaker
version of the same one. And here it is a better structure for a reason specific
to this problem: exact decimal expansion of a binary float, rounded half-to-even
at the sixth place, has a correctness space of 2^64 and **cannot be pinned
exhaustively**. A differential oracle is how it gets checked at all.

Which exposes the cost of going further than M2 does. If all three engines ran
`f64Str`, the parity suite would go **blind to formatting bugs** — all three
would agree, all three would be wrong, and the invariant that catches everything
else in this compiler would report green. `slice` could go all the way because a
byte loop can be pinned directly. This cannot, so the last implementation is
worth more as an oracle than as a deletion.

The unit of the objection is therefore *unreferenced multiplicity*, not
multiplicity. RFC-0078's census should be read that way too.

## What this does not touch

**Integer `toString`.** `@str` covers more than floats; only the float case is
an algorithm, and only the float case is written three ways.

**`@concat`**, the other `Measured` row. Same category, different question, and
its cost has not been re-measured.

## Allocation failure, made to agree — and where it still cannot

Fixing the wasm `malloc` exposed that the three engines disagreed about genuine
host memory exhaustion. The measurement is `s = s + s` forty times, on one
Windows box with 32 GB of RAM:

| engine | before | after |
|---|---|---|
| interpreter | exit 127, `memory allocation of 68719476736 bytes failed` — Rust's own allocator abort, plus its `RUST_BACKTRACE` note | exit 1, `error: out of memory`, after 110 s |
| native | exit 1, `error: out of memory`, after 109 s | unchanged |
| direct wasm | exit 1, `error: out of memory`, after 1 s | unchanged |

**Two of the three rows are corrections to what this section said before**, and
the corrections point in opposite directions.

**Native was already right.** The earlier note recorded "no output, still
thrashing when killed at 90 s" and inferred that `__vyrn_alloc_check` never
fires because a lazily-committing host does not return NULL. The first half was
a measurement, the second was a guess made from it, and the guess was wrong: the
run was killed twenty seconds before it finished. The shim's NULL check does
fire, with the right words and the right exit code. Nothing was changed here, and
the reason to write it down is that the wrong version of it was an argument for
capping native — which would have been a fabricated invariant defended by a
measurement that had been stopped too early.

**The interpreter is fixed.** It allocated through `Vec` and `String`, which
abort the process when the allocator refuses — Rust's contract, not this
language's. `try_reserve` is the whole mechanism, and it is applied at the sites
where *the amount is a value the program computed*: string concatenation (`a +
b` and the interpolation builtin), the concat accumulator's copy and append, and
`push`. A refusal is now an ordinary Vyrn trap, so the CLI renders it as
`error: out of memory` and exits 1, which is byte-for-byte what the other two
print.

### What is deliberately not covered, and why that is not a half-measure

The interpreter is **not** allocation-safe in general, and no amount of
`try_reserve` would make it so: `Val` is `Clone`, every scope push is a `Vec`
growth, and a fallible reserve at each of those would be ceremony around sizes a
program cannot drive. The four sites above are the ones it can — a program can
ask for a string twice as long or an array one longer, and nothing else in the
interpreter scales with a number the program chose. That is the guarantee, stated
as itself rather than implied to be stronger.

The gap this leaves is narrow and real: `Rc::make_mut` on a shared `Val::Array`
copies the whole vector infallibly, so a `push` on an array with two live
references can still abort where the same `push` on an unshared one traps.
Closing it means a fallible clone of an arbitrary `Val`, which is a different and
much larger change than this one.

### The divergence that remains, which is inherent

The three engines now agree on *what* allocation failure is. They cannot agree on
*when*, and pretending otherwise would be the fabricated invariant:

- A wasm32 memory cannot exceed 4 GiB, so the direct backend fails at a bound
  the compiler knows and can check — one second, deterministically, on any host.
- Native and the interpreter ask the host, and the host answers with its commit
  limit — RAM plus pagefile here, something else on the next machine. Both took
  about 110 s, most of it paging on the copies that succeeded first.

A program that allocates more than 4 GiB therefore *cannot* behave identically
across the three, and capping native or the interpreter to make it look like it
does would trade a true statement about a real limit for a false one about an
invented cap.

### Why this is still not in the parity suite

The wording and exit code are pinned where they can be pinned deterministically:

- The wasm side has two pins in `parity.rs` — one for a refused `memory.grow`
  under a capped memory, one for a `malloc` whose bump pointer would wrap, driven
  through `__vyrn_malloc` with `--invoke` because no Vyrn program can name a size
  larger than the memory it would have to fit in. Both assert the message against
  `RUNTIME_SHIM` itself, so the wasm spelling and the native one cannot drift.
- The interpreter's is a unit test that asks `try_reserve` for more than
  `isize::MAX`, which is refused without the allocator being consulted and is
  therefore the same answer on every machine.

What stays out of the suite is the end-to-end run, and the reason is stronger
than "a test cannot reproducibly exhaust host memory". It also *should not*: a
test that got far enough to be refused would have committed several GiB and
touched them, which takes 110 s here and is an OOM kill rather than a trap on a
CI runner. The bound the compiler owns is tested; the bound the host owns is
measured and written down.
