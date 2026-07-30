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
- **`slice` traps** — `error: slice index out of range` or
  `error: slice splits a UTF-8 character` — and Vyrn has no expression that aborts
  with a message. There is no `panic` and no `abort`, so `sliceV` returns
  `Option<String>` and `None` means "the builtin would trap here". Routing it
  would change observable behaviour, which is a language decision (a *control*
  primitive — neither an operation nor a view) and not a milestone's to make. A
  trapping wrapper is one line on top of `sliceV` the day the primitive exists.
- **`byteLength` is a VIEW**, not an operation: it is `strlen`, two instructions
  on every engine, and `consteval` folds it so a refinement predicate like
  `String where value.byteLength >= 3` can be proved at compile time. Routing it
  would make an O(1) read an O(n) heap copy and take that folding away — the
  opposite trade from the one this RFC exists to make.

Every function is still `V`-suffixed. For the three routed ones the suffix is
now just a second spelling of the same function (the builtin resolves to it
after linking); for `sliceV` and `byteLengthV` it is what keeps them callable
beside the builtins they are proved against, which
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

## sliceV

```vyrn
fn sliceV(s: String, start: Int64, end: Int64) -> Option<String>
```

The bytes of `s` from `start` up to `end`, or `None` where the `slice` builtin
would trap: `start < 0`, `end > s.byteLength`, `start > end`, or a cut point
inside a multi-byte UTF-8 character.

The boundary check is not written out. A cut at a non-boundary offset either
starts the range on a continuation byte or ends it on a truncated character,
and both are invalid UTF-8 — so `stringFromBytes` refuses exactly the ranges
the builtin's `is_char_boundary` pair refuses, and refusing is what `None`
says. Both engines check the range before the boundary, and so does this.

One divergence, unreachable from ordinary source: `stringFromBytes` rejects a
NUL byte (RFC-0014's rule), and `slice` does not, so slicing a String that
contains a NUL yields `None` here and the substring there. No string literal
can hold one — there is no `\0` escape — and `stringFromBytes` will not build
one either, so the byte view is *not* a round trip only for a String no
program can construct through it.
