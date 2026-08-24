# `slice` is 57 percent of the site build, and the comment above it says otherwise

Found in one command, the first time `vyrn run --profile` was pointed at
anything real.

```
vyrn run --profile site/export.vyrn out
```

| function | calls | self | share |
| --- | --- | --- | --- |
| `slice` | 571,770 | 32.374 s | **57.5%** |
| `findPlain` | 102,433 | 2.672 s | 4.7% |
| `findSkipping` | 6,534 | 2.570 s | 4.6% |
| `rfcMentions` | 108 | 1.298 s | 2.3% |
| `step` | 263,083 | 894 ms | 1.6% |

1,065 functions, 4,773,274 calls, 56.3 s of self time. The unprofiled run is
45.5 s, so the flag costs about 24 percent — two clock reads across 4.8 million
calls.

## What `slice` is

`std/strpred.vyrn:301`. It was a builtin; RFC-0046 replaced it with Vyrn. The
body is a byte loop:

```
let mut out: Array<UInt8> = []
let mut i = start
while i < end {
    out.push(s[i])
    i = i + 1
}
return match stringFromBytes(out) { ... }
```

## It is not quadratic, and it is not the push

Both were checked, because both were plausible.

200 slices of an n-byte string: 250 → 0.059 s, 500 → 0.084 s, 1000 → 0.122 s,
2000 → 0.217 s. Linear.

400,000 pushes into a byte array take 225 ms; a read-only loop over the same
bytes takes 286 ms. So a push is not more expensive than reading, and the floor
is about 0.6 µs per interpreted loop iteration.

`slice` averages 56.6 µs per call, which at that floor is about 100 iterations —
a hundred-byte slice. The arithmetic closes. **The cost is that the interpreter
runs about 57 million loop iterations of `slice` while the site builds.**

## The comment above it measured this and got the opposite answer

`std/strpred.vyrn` explains the design at length and ends:

> It is also, measurably, not the expensive part.

with numbers: `examples/twdemo` 67 → 81 → 68 ms, `examples/vyxdemo` 67 → 78 → 66
ms, `examples/shelf`'s client 160 → 171 → 159 ms.

Those numbers are right. The conclusion does not survive a change of scale. On a
program that calls `slice` a few hundred times the extra UTF-8 walk really does
not show up. On one that calls it 571,770 times, the loop is more than half the
run.

The comment is not wrong about what it measured. It is wrong about what follows,
and the difference is three orders of magnitude in call count.

## What this is a decision about

`RECOMMENDATION, NOT A DECISION`, and it is a real one because the obvious fix
is the thing the owner ruled out.

1. **Put the builtin back.** One `memcpy`, and about 32 s off the site build.
   It is also a standard-library operation implemented in a backend, which is
   the rule the owner set: no different implementations for different backends,
   nothing hard-implemented in a backend. A builtin means three of them.
2. **Give the language a bulk array operation** — an append of a byte range, or
   a reserve — so `slice` stays Vyrn but stops being a per-byte loop. One
   language addition, and every scanner in `std/` gets it.
3. **Make the interpreted loop cheaper.** 0.6 µs per iteration is the floor
   every Vyrn loop pays, so this is worth more than `slice`. It is also the
   largest of the three.
4. **Accept it.** The site builds in 45 s and nobody waits on it but CI.

Nothing here is done. The profiler that found it is
`vyrn run --profile`, and the measurement above reproduces in one command.
