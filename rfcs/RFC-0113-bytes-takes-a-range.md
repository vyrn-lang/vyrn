# RFC-0113 — `bytes` Takes a Range

- **Status:** Accepted. Implemented in this branch.
- **Closes:** [rfcs/census/slice-is-half-the-site-build.md](census/slice-is-half-the-site-build.md).

## The change in one line

`bytes(s)` answers a string's bytes; `bytes(s, start, end)` answers a byte range
of it.

## Why

`std/strpred`'s `slice` copied its range a byte at a time:

```vyrn
let mut out: Array<UInt8> = []
let mut i = start
while i < end {
    out.push(s[i])
    i = i + 1
}
```

Every string slice on the site goes through it — `substring` is a thin wrapper —
and `vyrn run --profile site/export.vyrn` put it at **57.5% of the whole build**,
32.374 s across 571,770 calls.

The loop is the cost, not the copy. Measured under the interpreter, for the same
ninety bytes:

| shape | per call |
| --- | --- |
| the `while` loop, then `stringFromBytes` | **19.6 µs** |
| one `bytes` call, then `stringFromBytes` | **1.03 µs** |

Nineteen times, and the second measurement copies *more* bytes — a hundred
against ninety — because the stand-in for the range was the whole string.

## Results

| | before | after |
| --- | --- | --- |
| `slice`, in the site profile | 32.374 s (57.5%) | **1.293 s (9.2%)** |
| `vyrn run site/export.vyrn` | 26.3 s | **12.75 s** |
| `vyrn test site/export.vyrn` | — | **16.6 s** |

Best-of-three, interleaved in one window, which is the only way this project
measures a change: an earlier attempt at something else read a 16% *improvement*
as a 17% regression against a twenty-minute-old baseline.

The profile is flat now. `slice` at 9.2% sits beside `findPlain` at 8.9% and
`findSkipping` at 8.2%; there is no dominant cost left in the site build.

## Why this shape and not the others

The census listed four options. This is its second, and the ranking held up.

**Not a `slice` builtin.** That was option 1, and it is on the wrong side of the
standing rule: `slice` is expressible in Vyrn — it *was* Vyrn — so moving it into
three backends would be three implementations of a standard-library function.
RFC-0111 drew the same line in the other direction: `writeStdout` is a builtin
because nothing in Vyrn can hand bytes to a descriptor.

`bytes` was already a builtin with three engine bodies. This adds an arity to it,
not a member to the class.

**Not `@substr`.** An unspellable primitive that copied a String range directly
would save the second copy and the redundant UTF-8 validation — the interior of a
valid string between two verified boundaries is valid by construction, so
`stringFromBytes` re-checks what `slice` has already proved. It was written and
then withdrawn: `@`-names cannot be *called* from source, only produced by
desugaring, so `std/strpred` could not have used it without new syntax to
desugar from. The measurement made the question academic — 1.03 µs is not where
the remaining time is.

**Not "make the interpreted loop cheaper."** That was option 3 and the census
called it worth more than `slice`, which is still true and still the bigger job.
This one was a line.

## What it does and does not check

`bytes` answers BYTES. The range form checks only that the range exists, and
traps otherwise with `string index {i} out of bounds` — the wording `s[i]`
already uses, so the trap catalogue does not grow and no engine gained a message
the other two would have to match.

It does **not** check character boundaries. A caller asking for bytes 1..3 of a
multi-byte character gets those two bytes, because that is what it asked for.
`substring` is the function that answers a `String` and refuses a boundary inside
a character, and it is written on top of this — every decision `slice` made
before still happens in `slice`.

## Parity

One implementation per engine, verified three ways by
`examples/bytesrange.vyrn`: both arities, an empty range, a one-byte range, a
range that splits a character, and the round trip through `substring`. All three
print identically, including the multi-byte cases, and the out-of-range trap
matches byte for byte with exit code 1 on all three.

The direct wasm backend has ONE arm for both arities: the three-argument form
differs only in where the copy starts and how long it is, and `MemoryCopy` does
not care which. The textual backend has one helper for both, with
`__vyrn_str_bytes` calling `__vyrn_str_bytes_range(s, 0, len)` — the copy loop
exists once.
