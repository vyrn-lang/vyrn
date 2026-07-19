# RFC-0045 — Bitwise Operators

- **Status:** Draft (design locked)
- **Depends on:** the sized integer types (Int8/16/32/64, UInt8/16/32/64
  — the operand domain), RFC-0043 (`std/random` — the MINSTD downgrade
  this lets us undo), the numeric trap discipline (canonical wording,
  overflow behaviour)
- **Evidence (recurring, two hard hits):** the bin dogfood couldn't write
  FNV-1a (used a weaker polynomial hash) and RFC-0043 couldn't write
  `SplitMix64`/PCG (shipped MINSTD/Lehmer, a genuine randomness-quality
  downgrade) — both because Vyrn has **no `& | ^ ~ << >>`**. Hashing, real
  PRNGs, bit flags, and byte/protocol manipulation all need them; the gap
  is no longer minor.

---

## Surface

Six operators on the sized integer types (NOT `Bool` — booleans keep
`&&`/`||`/`!`; NOT the unsized `Int`/`Float`, which don't exist):

| op | name | operands | result |
|----|------|----------|--------|
| `a & b` | and | same integer type | that type |
| `a \| b` | or | same integer type | that type |
| `a ^ b` | xor | same integer type | that type |
| `~a` | complement (unary) | integer type | that type |
| `a << b` | shift left | `a: intT`, `b: same intT` | `intT` |
| `a >> b` | shift right | `a: intT`, `b: same intT` | `intT` |

- **Type rule:** binary bitwise ops require **both operands the same
  integer type** (no implicit widening — Vyrn's existing sized-int
  discipline; a mismatch is the same named error a `+` mismatch gives).
  The shift amount is the same integer type as the shifted value (a
  literal is fine: `x << 3`).
- **`~` width:** complement is within the operand's width — `~0u8 == 255`,
  `~x` on `Int32` flips 32 bits. No promotion.
- **Signed `>>` is arithmetic** (sign-extends); **unsigned `>>` is
  logical** (zero-fills) — the standard, and the reason both exist as one
  spelling keyed on operand signedness. `<<` is the same bit shift for
  both (the sign bit is the caller's concern).

## Shift-amount safety (locked)

A shift by an amount **`>= the operand's bit width`** (or negative, if a
signed shift amount) **traps** — the canonical numeric-trap protocol (one
`error: shift amount out of range` line to stderr, `exit(1)`), byte-
identical across backends. This matches Vyrn's trap-on-invalid ethos
(C's UB and x86's silent masking are both rejected — a `u8 << 8` is a bug,
not a defined zero). Constant shift amounts out of range are a
**compile-time** error (the const-eval path), not a runtime trap.

## Precedence (locked — the C footgun is designed out)

The whole bitwise family binds **tighter than comparison** — so
`x & mask == 0` parses as `(x & mask) == 0` (the intended reading), never
C's `x & (mask == 0)`. Within the family, standard relative order:

```
unary  ~   (with unary -, !)
  <<  >>              (shift, just below multiplicative)
  &                   (and)
  ^                   (xor)
  |                   (or)
—— all above ——
  ==  !=  <  <=  >  >=   (comparison)
  &&                   (logical and)
  ||                   (logical or)
```

So `a | b & c` is `a | (b & c)`, `a << 2 + 1` is `a << (2 + 1)`? No —
shift is **below** multiplicative/additive, so `a << 2 + 1` is
`a << (2 + 1)` only if additive binds tighter; lock it: **`<< >>` bind
looser than `+ -`** (so `a + b << c` = `(a + b) << c`), matching the
"shifts are coarse" intuition. `fmt` spaces all binary bitwise ops like
other binary operators; `~` hugs its operand like unary `-`.

## Mechanism

- **Lexer:** new tokens `&` `|` `^` `~` `<<` `>>`. **The `>>`
  disambiguation** is the real work: in *type* position `>>` closes two
  generics (`Array<Array<T>>`), in *expression* position it is a shift —
  the parser already tracks type-vs-expression context (the same place
  the `>=`/generic tension lives); `<<`/`>>` are lexed as shift tokens
  only in expression context, split into `>` `>` when closing generics.
  Pin both (`Array<Array<Int64>>` still parses; `a >> b` shifts).
- **Checker:** operand-type rule above; const-eval folds constant bitwise
  ops (and rejects const out-of-range shifts).
- **Interp + codegen:** map to the obvious ops — interp does wrapping
  integer math at the operand width; codegen emits LLVM `and`/`or`/`xor`,
  `shl`, `lshr` (unsigned `>>`) / `ashr` (signed `>>`), and `~a` as
  `xor a, -1` at width; the shift-range trap is a compare + the standard
  trap branch (like the existing overflow checks). Byte-identical three
  ways.

## Consumers (the proof — undo the downgrades)

- **`std/random`**: replace MINSTD with real **`SplitMix64`** (the
  RFC-0043 wall) — `z = (z ^ (z >> 30)) * C1; …` now expressible; the
  `Rng` surface is unchanged, only the algorithm improves (its parity
  citizen re-pinned to the new sequence).
- **`std/hash`** (new, small): **FNV-1a** (`h = (h ^ byte) * prime`) over
  `Array<UInt8>` / `String` bytes — the hash bin hand-rolled without xor.
  bin's content-addressed id derivation switches to it (ids change once —
  they're content hashes, not persisted keys, so no migration; note it).
- **`examples/bits.vyrn`** parity citizen: every operator, signed vs
  unsigned `>>`, `~` at each width, a masked-flags example, and a
  shift-out-of-range **trap** (canonical wording, three-way).

## Out of scope

Bitwise on `Bool` (use `&&`/`||`), rotate operators (`rotl`/`rotr` — a
`std` helper via shifts+or if wanted), arbitrary-width/bignum integers,
`Int128`, bit-set/bitvector types, endianness helpers (a separate
`std/bytes` concern), compound assignment (`&=` `|=` `<<=` — a uniform
compound-assign round across all operators, if ever, not bitwise-only).
