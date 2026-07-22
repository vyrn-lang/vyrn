# std/random

std/random — deterministic pseudo-randomness with a host-seeded escape
(RFC-0043).

Randomness splits cleanly: a PURE, value-type generator (`Rng`) that is
always a parity citizen, and ONE host extern (`randomSeed`) for an
unpredictable starting seed. There is deliberately no ambient `random()` —
that would be an implicit host effect everywhere and a parity hazard. You
thread the generator explicitly: each draw returns the value AND the advanced
`Rng`, so nothing is hidden and a seeded run is fully reproducible.

The generator is **SplitMix64** — the standard bit-mixing PRNG (Vigna's
reference constants). Each `nextInt` advances the 64-bit state by the golden
ratio increment, then runs the state through two xor-shift-multiply rounds
and a final xor-shift to produce a well-distributed 64-bit output:
`z = (z ^ (z >> 30)) * C1; z = (z ^ (z >> 27)) * C2; z ^ (z >> 31)`. It
passes the usual statistical batteries, has no bad seeds (any 64-bit state is
valid), and is deterministic across every backend — the mixing is expressed
with the bitwise operators (RFC-0045), which undoes the RFC-0043 MINSTD
downgrade (that era had no `^`/`>>`, so only a multiplicative-congruential
generator was writable). It is not cryptographic; use it for shuffles,
sampling, and reproducible tests, not for secrets.

## Rng

```vyrn
type Rng = { state: Int64 }
```

PRNG state — a value type (no ambient/hidden generator). Copy it to fork a
reproducible stream.

## Draw

```vyrn
type Draw = { value: Int64, rng: Rng }
```

One draw: the value produced AND the advanced generator, threaded back by the
caller (Vyrn has no tuples, so the two results ride a small record).

## randomSeed

```vyrn
fn randomSeed() -> Int64
```

An unpredictable Int64 seed from the host CSPRNG. HOST EFFECT (forbidden in
generators/comptime; cannot cross a `spawn` boundary). Isolated here so 99%
of randomness — the pure `Rng` below — stays deterministic and host-free.

## seededRng

```vyrn
fn seededRng(seed: Int64) -> Rng
```

A generator seeded by any `Int64` — fully deterministic and parity-safe.
SplitMix64 has no bad states, so the seed IS the initial state directly (no
folding needed; negative seeds are fine — the state is a 64-bit word).

## nextInt

```vyrn
fn nextInt(rng: Rng) -> Draw
```

Advance the generator once via SplitMix64. Returns the mixed 64-bit `value`
(the full `Int64` range, incl. negatives) AND the advanced `.rng` to thread
into the next draw. Pure (constants inlined — no module state), so it runs in
generators/comptime too. The mixing runs in `UInt64` (unsigned `>>` = logical,
wrapping `*`), with the state threaded as its `Int64` bit pattern.

## nextInRange

```vyrn
fn nextInRange(rng: Rng, lo: Int64, hi: Int64) -> Draw
```

A draw in the inclusive range `[lo, hi]` (returns `lo` if the range is empty).
Uses unsigned modulo reduction over the full 64-bit output — a slight low-bias
for very wide ranges, acceptable for v1 (sampling/tests, not cryptography).
