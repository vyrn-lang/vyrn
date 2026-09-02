# std/runtime

std/runtime — the runtime in Vyrn (RFC-0125 §2.4, PLAN-0125-runtime).

The compiler links this module into every program it loads; nothing
imports it. It is the one module in the audience of `std/mem`, and the
loader declares that audience itself (PLAN-0125-runtime §3.2). A user
import of `std/runtime` or of `std/mem` is refused.

The families move in here one at a time, in PLAN-0125-runtime §6's order.
Each function keeps the byte-level semantics of the copy it replaces, and
the wasm emitter calls it where it used to call its own hand-emitted copy.
The names are the language's; the plan's names (`strlen`, `int_str`, ..)
are the runtime symbols they stand in for.

**Addresses.** A `String` reaches these functions as the address of its
bytes, an `Int32`, because the emitter's value for a `String` IS that
address and every consumer here scans for the NUL. Nothing outside this
module holds an address, which is the whole of PLAN-0125-runtime §3.3.

**Step 1, the pure strings** (PLAN-0125-runtime §6): `strLen`, `strCmp`,
`starts`, `intStr`, `parseI64`, `strI64`, `utf8Valid`, `lineAt`, `colAt`
and `regexRun`. Their C twins stay in the shim until step 3 moves the
native route.

**Call depth.** The emitter does not count these frames against the
language's call-depth budget: the hand-emitted copies had no prologue, and
a program that traps at the limit must trap where it did.

## strLen

```vyrn
fn strLen(p: Int32) -> Int32
```

`strlen`: the bytes before the NUL at `p`.

## strCmp

```vyrn
fn strCmp(a: Int32, b: Int32) -> Int32
```

`strcmp`: byte order, unsigned, which is what a `String` comparison is
(RFC-0022) since a `String` is UTF-8 bytes. Negative, zero or positive.

## starts

```vyrn
fn starts(a: Int32, b: Int32) -> Bool
```

`starts`: whether NUL-terminated `a` begins with NUL-terminated `b`.

## intStr

```vyrn
fn intStr(v: Int64, signed: Bool) -> String
```

`int_str`: the decimal digits of `v`, read as two's complement when
`signed` and as a `UInt64` otherwise. The result is a fresh `String` the
caller owns, which is what `@str` promises (`own.rs`, `DropKind::FreeStr`).

The magnitude is taken in `UInt64`, where `0 - u` of `Int64.min` is
exactly 2^63: the same unsigned digit loop the hand-emitted copy ran.

## parseI64

```vyrn
fn parseI64(s: Int32) -> Option<Int64>
```

`parse_i64`: RFC-0014's `parse`. An optional `-`, then digits, ALL of them
consumed; anything else is `None`. There is no overflow case on purpose:
`acc * 10 + d` wraps, so `"9223372036854775808"` is `Int64.min` on every
engine (`examples/numbytes.vyrn` pins the table).

Not `strI64` below, which reads `+` and stops at the first byte that is
not a digit — that is `strtoll`'s contract for an injected value.

## strI64

```vyrn
fn strI64(p: Int32) -> Int64
```

`str_i64`: `strtoll(p, 0, 10)` for everything an injected value can be —
an optional sign, then decimal digits, stopping at the first byte that is
not one. No whitespace skip and no clamp: the only callers are
`VYRN_FIXED_TIME` and `VYRN_FIXED_SEED`, which the harness writes as bare
decimals.

## utf8Valid

```vyrn
fn utf8Valid(s: Int32, len: Int32, table: Int32) -> Bool
```

`utf8valid`: whether the `len` bytes at `s` are valid UTF-8, by Björn
Höhrmann's DFA over `table` — the one table both compiled backends emit
(`utf8d_table`): 256 byte classes, then 9 states × 12 classes of
transitions. State 0 accepts, 12 rejects, and every rejecting transition
stays at 12, so the loop never needs an early exit. Rust's `from_utf8`
accepts exactly the same strings.

The ASCII prefix goes eight bytes at a time (RFC-0125 §1.5b): a word with
no high bit is eight steps the DFA would take from state 0 to state 0.

## lineAt

```vyrn
fn lineAt(d: Int32, len: Int64, off: Int64) -> Int64
```

`line_at`: the 1-based line of byte offset `off` in the `len` bytes at
`d`. `off` is clamped to `len` and not below zero: a negative `off` never
enters the loop, which is line 1, the interpreter's `.max(0)`.

## colAt

```vyrn
fn colAt(d: Int32, len: Int64, off: Int64) -> Int64
```

`col_at`: the 1-based column of byte offset `off`, and a column counts
BYTES (RFC-0078 M4b(2)): the `x` in `éx` is column 3. `off` is the cursor,
walked down to the byte after the previous LF.

## regexRun

```vyrn
fn regexRun(s: Int32, table: Int32, start: Int32, accept: Int32) -> Bool
```

`regex_run`: whether the NUL-terminated bytes at `s` match the DFA
`=~` compiled (RFC-0046). Every state has all 256 transitions and a dead
state absorbs a non-match, so the walk has no conditional but the end of
the string: a full match is "the state the last byte left us in accepts".
`table` is `states × 256` four-byte entries, `accept` one byte per state.

The byte is read unsigned: a signed read would turn a UTF-8 continuation
byte into a negative index and answer wrongly rather than trap.
