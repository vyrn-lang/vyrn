# Every body the core does not lower whole, by what blocks it

RFC-0125's last blocker is one number: the core lowers 4 per cent of bodies
whole. This census partitions the other 96 per cent. It measures what each body
waits on, so the next RFC's tracks can be cut by gap family rather than by
file. Nothing was fixed.

## What was measured

A BODY is one frame of one function instance: the core builds it
(`vyrn_lower::core::build`) and either the rows carry it end to end or they do
not. A GAP is a shape among those rows that no emitter reads from the core, or
a construct the core refuses to lower at all (`core::Gap`). `core::gaps` states
the list once. `tests/coredrive.rs` ranks the same tags into its eight classes,
and this census tables them.

`VYRN_GAP_TALLY=<file>` makes every compile append one line per body: the
module, the function, the first gap, every gap of that body, and the command
that ran. A body the rows carry whole reads `-` in both gap fields, so the
denominator of every table below is the tally's own. A body built twice in one
process is one line.

To re-run it, from `examples/`:

```
for f in *.vyrn; do VYRN_GAP_TALLY=/tmp/gaps.tsv \
  ../compiler/target/release/vyrn check "$f" >/dev/null 2>&1; done
```

Run one program per process. A root module has no file name in the core, so the
command is what tells two `main` bodies apart.

Measured on 2026-09-11, head `377cb367` plus the instrument, release build. The
run covers 206 programs in `examples/`, the `bin`, `fullstack`, `shelf` and
`vyxcomp` apps, `site/`, and the `semantics` suite: 79,498 bodies, of which
11,014 are whole (13.9 per cent) and 68,484 are not. Over `examples/` alone the
numbers are 25,694 bodies and 3,544 whole. `coredrive --ignored` asks the same
question over the 168 programs it loads and reports 3,072 whole of 21,556, so
`coredrive` stays the pinned home of the count and this census adds none.

## (a) Bodies by first gap

The first gap is the first one in source order. The percentage is cumulative
over all 79,498 bodies, so the last row plus the whole bodies is 100.

| first gap | bodies | cumulative |
|---|---|---|
| `Call:Scalar:Int32` | 13,922 | 17.5% |
| `Read` | 11,154 | 31.5% |
| `Call:Builtin` | 9,000 | 42.9% |
| `Call:Scalar:UInt32` | 6,960 | 51.6% |
| `Make` | 6,822 | 60.2% |
| `Opaque` | 3,484 | 64.6% |
| `Call:Scalar:UInt64` | 3,331 | 68.8% |
| `Call:Scalar:Int64` | 2,889 | 72.4% |
| `Prim` | 2,818 | 76.0% |
| `Switch` | 2,710 | 79.4% |
| `Call:Ctor` | 1,843 | 81.7% |
| `Drop` | 1,804 | 83.9% |
| `Call:Reserved` | 1,354 | 85.7% |
| `Take` | 114 | 85.8% |
| `Call:Scalar:UInt8` | 67 | 85.9% |
| `Gap:a-call-this-slice-cannot-attribute` | 47 | 85.9% |

The same table by family:

| family | bodies | cumulative |
|---|---|---|
| a scalar conversion | 27,189 | 34.2% |
| a layout read | 11,268 | 48.4% |
| a builtin operation | 10,354 | 61.4% |
| a layout made | 8,684 | 72.3% |
| the row names no value | 3,484 | 76.7% |
| a short circuit | 2,818 | 80.2% |
| the tag | 2,710 | 83.7% |
| a release the driver places | 1,804 | 85.9% |
| the core gives up | 51 | 86.0% |
| a function value | 45 | 86.0% |
| a lambda frame | 42 | 86.1% |
| a declared callee | 19 | 86.1% |
| the receiver handed back | 16 | 86.1% |

## (b) What one family alone buys

A body is whole when every gap of it is closed, so a family's own number is the
count of bodies that wait on that family and on nothing else. The third column
is how many bodies name the family at all, which is the work the track does.

| family | whole if it alone closes | bodies that name it |
|---|---|---|
| a scalar conversion | 16,816 | 37,194 |
| a short circuit | 1,244 | 18,991 |
| a release the driver places | 978 | 13,787 |
| a layout made | 909 | 25,815 |
| a layout read | 647 | 27,094 |
| a builtin operation | 521 | 26,621 |
| the tag | 262 | 9,323 |
| the row names no value | 140 | 16,417 |
| the core gives up | 51 | 51 |
| a function value | 42 | 163 |
| a lambda frame | 15 | 84 |
| the receiver handed back | 13 | 10,754 |
| a declared callee | 1 | 37 |

Most bodies wait on more than one family: 21,639 wait on one, 19,047 on two,
and 466 on nine. So the order the tracks run in decides what each one is worth.
Closing the families in the order that buys the most at each step:

| step | family closed | bodies whole | of all |
|---|---|---|---|
| 0 | nothing | 11,014 | 13.9% |
| 1 | a scalar conversion | 27,830 | 35.0% |
| 2 | a layout made | 34,836 | 43.8% |
| 3 | a short circuit | 41,235 | 51.9% |
| 4 | a builtin operation | 42,967 | 54.0% |
| 5 | a layout read | 49,031 | 61.7% |
| 6 | the row names no value | 54,786 | 68.9% |
| 7 | a release the driver places | 62,148 | 78.2% |
| 8 | the receiver handed back | 69,897 | 87.9% |
| 9 | the tag | 79,187 | 99.6% |
| 10 | a function value | 79,330 | 99.8% |
| 11 | a lambda frame | 79,410 | 99.9% |
| 12 | the core gives up | 79,461 | 100.0% |
| 13 | a declared callee | 79,498 | 100.0% |

## The families, and what each one is

A family is what one track closes in one change to `core.rs` and the emitter's
reading of it.

**A scalar conversion.** `Int32(n)`, `Int64(n)`, `UInt8(n)`, `UInt32(n)`,
`UInt64(n)`, `Float32(n)`, `Float64(n)`. The core states a `Rhs::Call` with
`Callee::Reserved` and the name of the type, where the operation converts
between two scalars that both emitters already write. One `Op` variant closes
it. It is the largest family by every measure and the only one that pays 21
percentage points alone.

`Callee::Scalar` is documented as the kind for exactly this call, and the whole
run produces it zero times. So does `Callee::Projection`. Two of the nine
callee kinds have no reader.

**A layout read.** `Rhs::Read` and `Rhs::Take` of a field, an element or a key.
The row states the place; no emitter reads a place from the core.

**A layout made.** `Rhs::Make` (a record, an array literal, a map literal, a
`where`-checked constructor) and a call to `Callee::Ctor` or `Callee::Named`.
`Make:Array` alone is 6,361 first gaps.

**A builtin operation.** Every other seeded or reserved callee: `bytes` (4,647
first gaps), `@copy`, `@str`, `stringFromBytes`, `@at`, `floatBits`, `readFile`,
`panic`, `@panicAt`, the SIMD names, `print`. The row names the builtin. It does
not state what the builtin does.

**The row names no value.** `Val::Lit(Lit::Opaque)`: a function's name, a type's
name, a nullary constructor used as a value.

**A short circuit.** `Rhs::Prim(Op::Bin(And | Or))`. The row carries both
operands, and an emitter has to write a branch.

**The tag.** `St::Switch`, from `match`, `if let` and `?`.

**A release the driver places.** `St::Drop` and `St::Row` at an exit the driver
does not take.

**The receiver handed back.** A `Rhs::Call` with `write_back`: `out.push(v)`
hands the buffer back and a store puts it back.

**A function value, a lambda frame, a declared callee, the core gives up.** Four
small families. The last is the only hard one: `core::build` returns `Err(Gap)`
for 51 bodies, 46 of them a call to `__vyrnGenNextInt`, `__vyrnGenNextStr` or
`__vyrnGenReflect`, three an expression the checker did not type, and one a rule
the program breaks.

## The three families that are sugar

A sugar family is closed by a parser desugar and needs no new core row.

- **A short circuit.** `a && b` is `if a { b } else { false }`. The parser can
  write the branch, the row disappears, and 1,244 bodies become whole with no
  change to any emitter.
- **The receiver handed back.** `out.push(v)` is `out = push(out, v)`. A store
  after the call is a row both emitters read.
- **The row names no value**, in part. A nullary constructor used as a value is
  a `Make` of a fieldless variant. A function's name and a type's name are not
  sugar: they need a value the row can name.

`if let` and `?` are desugared to `match` by the parser already (RFC-0121), so
the tag is not sugar. It is one row an emitter must learn to read.

## What this does not say

The tally states the core's gaps. `Fn_::core_walkable` in `direct.rs` screens a
body further: it refuses one whose locals are not scalars, and `coredrive`
counts that as its scalar-only column. A body with no gap here can still fall to
that screen, which is why `coredrive` reports 953 bodies emitted from the core
where 3,072 have no gap.

No AST arm in `direct::FORMS` reaches zero on one family. An expression arm is
downstream of the statement that holds it, and a statement arm goes when every
body holding its form is whole. The four statement arms that read no core row
today are `Stmt::ForIn`, `Stmt::IfLet`, `Stmt::Drop` and a statement of another
form. Their payers are, in order, a layout read with a builtin operation, the
tag, a release the driver places, and the tag again.

## What M7's first track changed (2026-09-11)

The layout-made track (`track-gm`) gave the emitter a reader for two of the
tags above, so `core::gaps` stopped naming them and every table on this page is
the state BEFORE that. `Make:<record>`, `Make:Array` and `Call:Ctor:<variant>`
are no longer gaps; `Make:Map`, `Make:Try` and every other callee kind still
are. Over `examples/` the tally reads 3,685 bodies whole where it read 3,544,
and `coredrive --ignored` reads 3,180 with no gap where it read 3,072.

The tally's denominator moved too, from 25,694 lines to 25,550, and that is the
instrument rather than the corpus: a line is deduplicated whole, so two
instances that share a spelling and differed only in a tag this track retired
are now one line. Read the denominator as lines, not as bodies, whenever the
gap list itself has changed.
