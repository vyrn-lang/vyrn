# std/strpred

std/strpred — the string predicates and `slice`, written in Vyrn
(RFC-0078 M4b(3); three of the five routed by M4c).

`contains`, `startsWith`, `endsWith`, `slice` and the `byteLength` field were
builtins with three implementations each (Rust in the interpreter, IR the textual
emitter printed, and a fourth the direct wasm backend owed). RFC-0078 listed all
five as writable in Vyrn *today*, and the reason is M4a's finding restated for
strings — the irreducible primitive is not the operation, it is the **view**.
`bytes(s) -> Array<UInt8>` and `s[i] -> UInt8` already give a `String` its byte
reading, and `stringFromBytes(b)` gives the way back, so none of the five needs
to know anything a Vyrn program cannot see.

**Three moved. Two did not, and both refusals are measured rather than deferred.**

- `containsV`, `startsWithV` and `endsWithV` ARE the builtins now: every engine
  calls them, and `strstr` plus two `strncmp` shapes are gone from the emitted
  IR along with three Rust one-liners.
- **`sliceV` IS `slice` now too** (RFC-0079 M3). It was refused by M4c because
  the builtin TRAPPED — `error: slice index out of range` or
  `error: slice splits a UTF-8 character` — and Vyrn had no expression that
  aborts, so the Vyrn version could only say `None` where the builtin ended the
  process. RFC-0079 removed the need for an abort instead of adding one:
  `slice` returns `Result<String, SliceError>`, the caller decides, and the
  three hand-written implementations (a Rust arm, ~50 lines of emitted IR and a
  wasm runtime function) are gone. The `Option` shape was the thing that made
  RFC-0078's "the swap is one wrapper line" estimate wrong — it collapsed two
  distinguishable failures into one `None`; the enum below is what makes it
  right.
- **`byteLength` is a VIEW**, not an operation: it is `strlen`, two instructions
  on every engine, and `consteval` folds it so a refinement predicate like
  `String where value.byteLength >= 3` can be proved at compile time. Routing it
  would make an O(1) read an O(n) heap copy and take that folding away — the
  opposite trade from the one this RFC exists to make.

Every function is still `V`-suffixed, and for the four routed ones the suffix is
now just a second spelling of the same function (the builtin resolves to it
after linking). `sliceV` in particular keeps its name rather than becoming a
bare `slice`: `slice` is a RESERVED word, so a module cannot declare it, which
is the whole reason this convention exists. For `byteLengthV` the suffix is
what keeps it callable beside the builtin it is proved against, which
`examples/strpredbytes.vyrn` still does.

**Bytes, not characters.** A `String` is UTF-8 bytes and every offset and
length here is a byte offset, matching the builtins. That is also why the
predicates are safe to do byte-wise: UTF-8 is self-synchronizing — a needle's
first byte is either ASCII or a lead byte, never a continuation byte, and every
non-boundary offset in the haystack holds a continuation byte — so a valid
needle cannot match at a non-boundary offset. The case that looks dangerous is
unreachable rather than handled.

One measurement worth keeping, since it decided whether the routing was safe to
take: `byteLengthV` is `bytes(s).length`, which ALLOCATES, and `std/vyx` calls
these predicates 97 times over a page. Timed with the generator cache off, the
biggest generator app in the repo went 933 ms -> 951 ms and `examples/vyxdemo`
went 79 ms -> 76 ms. So the allocation does not matter at these needle sizes and
the module was left exactly as the equivalence proof wrote it, rather than
rebuilt on `s.byteLength` for a speed nothing needed.

## byteLengthV

```vyrn
fn byteLengthV(s: String) -> Int64
```

The byte length of `s` — the `s.byteLength` field, as a function.

`bytes` hands over the byte view and an array knows its own length, so this
is the whole thing.

## startsWithV

```vyrn
fn startsWithV(s: String, needle: String) -> Bool
```

Does `s` begin with `needle`? An empty needle is a prefix of everything
(including `""`), matching the builtin.

## endsWithV

```vyrn
fn endsWithV(s: String, needle: String) -> Bool
```

Does `s` end with `needle`? An empty needle is a suffix of everything.

## containsV

```vyrn
fn containsV(s: String, needle: String) -> Bool
```

Does `needle` occur anywhere in `s`? An empty needle occurs at 0, so this is
`true` even for `""`.

The naive scan, which is what the builtin is too at these sizes.
`std/strings`'s `indexOf` is the same loop returning the offset — this is
spelled out rather than built on it so the module stays a leaf that imports
nothing and reaches no builtin except the byte view.

## SliceError

```vyrn
type SliceError = OutOfRange(Int64) | SplitsCharacter(Int64)
```

The two ways a byte range fails to name a substring, each carrying the offset
that failed — `std/`'s first error enum (RFC-0079 §1), and the shape the house
pattern is meant to take: a small per-operation enum, not one shared error type.
`Issue` (RFC-0009) is a different job — it models field validation accumulated
across a value — and is not being replaced.

This is strictly more than the builtin could say. `slice` had two fixed strings
and no way to name the index; `sliceV` had one `None` for both. A caller that
wants neither writes `?? panic("…")` or `?? fallback`.

## sliceV

```vyrn
fn sliceV(s: String, start: Int64, end: Int64) -> Result<String, SliceError>
```

The bytes of `s` from `start` up to `end`. `start`/`end` are BYTE offsets, and
both must land on a UTF-8 character boundary.

`OutOfRange(i)` when `start < 0`, `end > s.byteLength` or `start > end`, `i`
being the offset that failed the check — `start` for a negative start, `end`
for the other two, since an `end` that precedes its `start` is the one that is
too small. `SplitsCharacter(i)` when cut point `i` lands on a UTF-8
continuation byte. The range is checked BEFORE the boundary, which is the order
the three deleted implementations used and the order
`examples/strpredbytes.vyrn` pins.

The boundary check IS written out, unlike the `Option` version this replaces.
That one let `stringFromBytes` refuse the invalid bytes and reported `None`,
which was enough while both failures collapsed to one answer; naming the
offending offset needs the cut points tested individually. It is the same test
the deleted lowerings open-coded — `(b & 0xC0) == 0x80` — and `std/text`'s
`charCountV` spells it the same way. A cut AT `s.byteLength` is a boundary by
definition and is not read: `s[n]` is out of bounds here, where the builtins
could read the NUL terminator.

One divergence, unreachable from ordinary source: `stringFromBytes` rejects a
NUL byte (RFC-0014's rule), and the builtin did not, so slicing a String that
contains a NUL fails here and returned the substring there. No string literal
can hold one — there is no `\0` escape, and the lexer rejects a raw NUL — and
`stringFromBytes` will not build one either, so the byte view is *not* a round
trip only for a String no program can construct through it. That arm reuses
`SplitsCharacter` rather than inventing a third variant for a case no program
can reach, and the boundary checks above are what make the two cut points the
only honest thing left to name.
**`s.byteLength` and `s[i]`, not `byteLengthV(s)` and `bytes(s)`** — the one
place in this module where the difference was measured and mattered. Both of
those allocate a copy of the WHOLE string, and `slice` is called once per token
by `std/scan`, so a scanner over a large source would copy that source once per
token. That is quadratic, and it is the shape of the 240x string-append bug
RFC-0076 found. The byte read `s[i]` is the same O(1) view `startsWithV` and
`containsV` are written on, and `s.byteLength` is `strlen` (and folds).

What remains is a real cost and is recorded rather than chased: the builtin's
body was one `memcpy`, and this is a push loop plus `stringFromBytes`, which
re-walks the UTF-8 DFA over bytes the boundary check above already proved
valid. It is not removable — `stringFromBytes` is the ONLY construction of a
`String` from bytes there is (the census's `View` row) — and it is the honest
price of the runtime being Vyrn. It is also, measurably, not the expensive
part. Generator-heavy `vyrn check` with the gen cache off, min of 8, builtin ->
this function written on `bytes(s)` -> this function written on `s[i]`:
`examples/twdemo` 67 -> 81 -> 68 ms, `examples/vyxdemo` 67 -> 78 -> 66 ms,
`examples/shelf`'s client 160 -> 171 -> 159 ms. The whole-string copy was the
entire regression; the extra UTF-8 walk does not show up at all.
