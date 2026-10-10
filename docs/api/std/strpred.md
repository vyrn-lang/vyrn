# std/strpred

std/strpred: the string predicates and `slice`, written on the byte view
(`bytes`, `s[i]`, `stringFromBytes`).

Offsets and lengths are bytes. A byte-wise scan is safe on UTF-8: a valid
needle starts with an ASCII or a lead byte, so it cannot match at a
continuation byte. The functions read the `byteLength` field, never
`byteLengthV`: the field is O(1) and folds at compile time, while
`byteLengthV` copies the string (4.5 ns against 72.5 ns on 47 bytes).

## byteLengthV

```vyrn
fn byteLengthV(s: String) -> Int64
```

The `s.byteLength` field as a function.

## startsWith

```vyrn
fn startsWith(s: String, needle: String) -> Bool
```

Does `s` begin with `needle`? An empty needle is a prefix of everything.

## endsWith

```vyrn
fn endsWith(s: String, needle: String) -> Bool
```

Does `s` end with `needle`? An empty needle is a suffix of everything.

## skipTable

```vyrn
fn skipTable(needle: String, haystackBytes: Int64) -> Array<Int64>
```

Boyer-Moore-Horspool's bad-character table for `needle`: for each byte, how
far a window may jump when it ends on that byte. A plain `Array`, not a
record: a field read per byte costs the interpreter measurably, and an
array argument is shared copy-on-write.

## findPlain

```vyrn
fn findPlain(s: String, needle: String, from: Int64 where value >= 0) -> Int64
```

The first occurrence of `needle` at or after `from`, or -1. O(n*m).

## worthPreparing

```vyrn
fn worthPreparing(needle: String, haystackBytes: Int64) -> Bool
```

Whether a table for `needle` pays over a haystack of that size. Callers ask
before building the table: `std/vyx` calls `contains` on short strings
millions of times, and an allocation per call cost it about 10%.

## findSkipping

```vyrn
fn findSkipping(s: String, needle: String, from: Int64, skip: Array<Int64>) -> Int64
```

[`findPlain`] with a table from [`skipTable`]: it compares from the end of
the window and, on a mismatch, skips by the table entry for the window's
last byte, so it visits about `n / m` positions.

## contains

```vyrn
fn contains(s: String, needle: String) -> Bool
```

Whether `needle` occurs in `s`; an empty needle occurs everywhere.

## SliceError

```vyrn
type SliceError = OutOfRange(Int64) | SplitsCharacter(Int64)
```

The two ways a byte range fails to name a substring, each carrying the
offset that failed.

## slice

```vyrn
fn slice(s: String, start: Int64, end: Int64) -> Result<String, SliceError>
```

The bytes of `s` from `start` up to `end`, both byte offsets on UTF-8
character boundaries.

`OutOfRange(start)` when `start < 0`; `OutOfRange(end)` when
`end > s.byteLength` or `start > end`. `SplitsCharacter(i)` when cut point
`i` lands on a continuation byte. The range is checked before the boundary,
as `examples/strpredbytes.vyrn` pins. A string that holds NUL, which no
program can construct, returns `SplitsCharacter(start)`.

It reads `s[i]` and `s.byteLength`, never `bytes(s)`: `std/scan` slices once
per token, so a whole-string copy per call would be quadratic.
