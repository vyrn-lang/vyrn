# std/random

std/random: deterministic pseudo-randomness with a host-seeded escape.

The generator is a value (`Rng`): each draw returns the value and the
advanced `Rng`, so a seeded run reproduces on every engine. There is no
ambient `random()`; `randomSeed` is the one host effect. The algorithm is
SplitMix64. It is not cryptographic: use it for shuffles, sampling and
tests, not for secrets.

## Rng

```vyrn
type Rng = { state: UInt64 }
```

PRNG state. Copy it to fork a reproducible stream.

## Draw

```vyrn
type Draw = { value: Int64, rng: Rng }
```

One draw: the value and the advanced generator.

## randomSeed

```vyrn
fn randomSeed() -> Int64
```

An unpredictable seed from the host CSPRNG. A host effect, so generators and
comptime code cannot call it.

## seededRng

```vyrn
fn seededRng(seed: Int64) -> Rng
```

A generator seeded by any `Int64`. SplitMix64 has no bad states, so the
seed's bit pattern is the initial state.

## nextInt

```vyrn
fn nextInt(rng: Rng) -> Draw
```

Advances the generator once: `value` covers the full `Int64` range. Pure, so
generators and comptime code can call it.

## nextInRange

```vyrn
fn nextInRange(rng: Rng, lo: Int64, hi: Int64) -> Draw
```

A draw in the inclusive range `[lo, hi]`, or `lo` if the range is empty.
Modulo reduction, so a very wide range has a slight low bias.

The span is `UInt64` because `hi - lo + 1` reaches 2^64. That wraps to 0,
which means the full range: every draw lands inside it and the reduction is
skipped.
