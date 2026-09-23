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

## What the release track changed (2026-09-22)

The release family is half closed. `St::Row` has a reader, so `core::gaps`
stops naming it and every table above is the state BEFORE that; `St::Drop` is
still a gap, and it is what the family's row in table (b) now counts.

The track found the instrument's own subject wrong. The core's second build
read `Instance::releases`, a copy the lowering makes before the placer writes
the rows the plan was missing, so the core stated almost none of the releases
it was meant to: over `examples/` a release row reached the emitter for one
binding in twenty. The tables above were measured against that core. So the
family's numbers here are a floor, not a count, and the order they rank is
unchanged only because the rows they were missing were missing from every
family's bodies alike.

Over `examples/` the tally reads 4,089 bodies whole where it read 4,026, and
`coredrive --ignored` reads 3,496 with no gap where it read 3,449, with 322
distinct bodies carried end to end where it read 283. The tally's denominator
moved from 25,694 lines to 26,356, which is the instrument again: a line is
deduplicated whole, and the core states more rows than it did.

The bodies the emitter TAKES stand at 953. `Fn_::core_walkable`'s
`placed.is_empty()` screen is gone and it moved none of them: a released name
is a String or an array, so the clause that asks every name to be a scalar
refuses the same frames. That clause is the layout families' to move.

## What the emitter's screen refuses, third count (2026-09-23)

Measured at `7751ce7b`, before any code moved. A temporary instrument in `Fn_::core_walkable` and the statement screen under it made every refusing clause record its name and pass, so one compile gives each body's first clause and every clause it meets; the instrument is not committed. It ran with `VYRN_GAP_TALLY` over `emit-wat` of the 415 roots of `examples/`, `std/`, `site/` and `compiler/vyrn-cli/tests/`, second pass, in one cache state. One line per body per root: 61,505 compiles, 54,014 taken, 6,858 refused with no gap and 633 with one.

A name the screen refuses is first in 5,766 of the 6,858 and never alone: every such body also fails a statement clause. So the table ranks the first statement clause. "Only" means no other statement clause refused the body; "met" counts the bodies that meet the clause anywhere.

| first statement clause, bodies with no gap | first | only | met | in flight |
|---|---|---|---|---|
| a made array this walk does not build | 1,877 | 600 | 1,924 | `m7-made` |
| a made layout returned into the caller's storage | 1,852 | 1,496 | 2,210 | `m7-made` |
| `@str` of a String temporary | 405 | 390 | 531 | |
| a made record this walk does not build | 334 | 1 | 388 | `m7-made` |
| a move of an enum (`Rhs::Val`) | 273 | 1 | 581 | |
| a made array into a temporary | 246 | 94 | 473 | `m7-made` |
| a store into a name that is an array | 242 | 8 | 579 | `m7-box` |
| a read or take of module state that is an enum | 215 | 9 | 216 | `m7-state` |
| a store into module state that is a String | 205 | 105 | 212 | `m7-state` |
| a `return` of a String, the failing arm of `?` on a `Result` | 148 | 73 | 176 | |
| a switch on a payload binder | 119 | 116 | 120 | this track |
| a read or take of a field that is an array | 94 | 4 | 808 | `m7-hole` |
| a read or take of an element that is a record | 89 | 37 | 190 | |
| a rebuild with no store after it | 85 | 1 | 237 | |
| a made record into a temporary | 82 | 16 | 705 | `m7-made` |
| a store into a field that is a String | 76 | 63 | 97 | |
| a move of an array (`Rhs::Val`) | 57 | 54 | 485 | |
| a store into a map key | 55 | 55 | 93 | |
| a read or take of module state that is an array | 49 | 27 | 64 | `m7-state` |
| a store into a name that is an enum | 44 | 0 | 338 | `m7-box` |
| a read or take of a field that is a String | 41 | 37 | 178 | |
| 46 more clauses | 270 | 146 | | |

Notes: `substring`, compiled in 115 roots, is 115 of the 119 switches on a payload binder, and this track took it. 85 of the 94 array field rows are the take that binds a name out of the field, the hole `m7-hole` states. `m7-writes`' write points were not matched to a row here. The valueless `return` of a `?` on an `Option` was first in 4 bodies and alone in 3; `Switch:Impl` refused no body with no gap, because the gap walk named it first.

| first gap, bodies with a gap | lines | only gap |
|---|---|---|
| `Opaque:Static` | 139 | 109 |
| `Call:Reserved:@codeText` | 138 | 0 |
| a call through a function value (`Call:Value`) | 119 | 118 |
| `Call:Builtin:toJson` | 107 | 100 |
| `Call:Builtin:contractOf` | 80 | 0 |
| `lineAt`, `colAt` and `rawAt` | 76 | 50 |
| `Call:Builtin:fromJson` | 71 | 42 |
| `serveStream`, `logger`, `boxStream` and `unboxStream` | 64 | 18 |
| `Call:Builtin:@at` | 62 | 62 |
| `Call:Reserved:@codeSplice` | 55 | 0 |
| `Lambda` | 41 | 22 |
| `Call:Method:show` | 19 | 19 |
| `Call:Reserved:@remove` | 12 | 0 |
| `Call:Builtin:fromArray` | 11 | 6 |
| `Make:Try` | 11 | 11 |
| `Call:Builtin:jsonSchema` | 10 | 10 |
| `Switch:Impl` | 9 | 0 |
| 26 more tags | 48 | 21 |

The gap tally reads 71,467 lines over the same pass, 70,395 whole and 1,072 with a gap; it counts every frame the core builds, the emitter's screen only the bodies the emitter compiles. At the head `Switch:Impl` is no tag: the three bodies of `examples/` have no gap.

## What each gap tag needs, and the decision it waits on (2026-09-23)

Measured on main `363c013e` with no code moved. `VYRN_GAP_TALLY` ran over `emit-wat` of the 415 roots of `examples/`, `std/`, `site/` and `compiler/vyrn-cli/tests/`, one process per root from the root's directory. A first pass warmed the generator cache and the second was counted; a third, with one tally file per root, read the same 71,798 lines, 70,735 whole and 1,063 with a gap. Tag for tag the first gaps equal the third screen count above, less the 9 lines of `Switch:Impl` that `m7-screen3` retired.

A line is one body per root, so a `std` body counts once for every root that compiles it. "Alone" counts the lines whose every gap is in the group, and "met" the lines that name the group at all. "Spellings" counts each module path and function once; a module that two roots reach by two relative paths is two spellings, so the column is a ceiling on distinct bodies.

| group | tags | first | alone | met | spellings | std / site / examples / tests |
|---|---|---|---|---|---|---|
| function values | `Call:Value:*`, `Lambda`, `Opaque:Static` of a function | 299 | 271 | 340 | 207 | 55 / 15 / 134 / 3 |
| generator host imports | `@codeText`, `@codeSplice`, `render`, `raw`, `rawAt` | 219 | 219 | 219 | 33 | 30 / 0 / 3 / 0 |
| JSON routes | `toJson`, `fromJson` | 178 | 176 | 181 | 144 | 4 / 10 / 129 / 1 |
| contract reflection | `contractOf` and the contract name it reads | 80 | 80 | 80 | 8 | 8 / 0 / 0 / 0 |
| an element of a temporary | `Call:Builtin:@at` | 62 | 62 | 63 | 28 | 14 / 3 / 11 / 0 |
| line and column | `lineAt`, `colAt` | 51 | 51 | 51 | 5 | 4 / 0 / 1 / 0 |
| streams | `fromStep`, `boxStream`, `unboxStream`, `pullAt`, `close`, `fromArray`, `Opaque:Pull` | 45 | 22 | 68 | 43 | 9 / 0 / 30 / 4 |
| method dispatch | `Call:Method:*` | 33 | 28 | 38 | 32 | 0 / 6 / 26 / 0 |
| the map and array tail | `@remove`, `@has`, `@tally`, `@list`, `@toArray` | 26 | 8 | 26 | 26 | 0 / 0 / 26 / 0 |
| logging | `logger`, `@error`, `@warn` | 18 | 18 | 18 | 16 | 0 / 0 / 16 / 0 |
| the served stream | `serveStream` | 18 | 18 | 18 | 4 | 4 / 0 / 0 / 0 |
| type reflection | `jsonSchema`, `schemaOf` | 17 | 17 | 18 | 13 | 0 / 3 / 10 / 0 |
| a checked construction | `Make:Try` | 11 | 11 | 11 | 8 | 0 / 3 / 5 / 0 |

Ten lines are in no group: the 5 refusal witnesses whose gap is a rule the program breaks, which stay a gap by design, one `writeStdout`, and 4 that also name `value` or `@tallyBytes`.

### What each group waits on

**Function values** are two mechanisms, and the core states neither. RFC-0023 specializes a function with `fn`-typed parameters per call target (`direct::Fn_::ho_call`): the instance's signature replaces each such parameter with the target's captures, and a call through the parameter is a direct call (`Fn_::target_call`) read off `fn_binds`. The core builds one instance per type arguments, so its frame for `map<Int64, Int64>` keeps the parameter `f`, which no emitted function has. RFC-0037 stores a function value as the signature's closure enum (`Fn_::build_fnval`), whose tag the emitter's dispatch registry chooses, and a call through it is one call to the signature's dispatcher (`Fn_::fnval_call`). The core states a stored function name as `Opaque:Static`, a lambda as `Op::Closure`, and both calls as `Callee::Value`. The missing facts are the targets and captures of a specialization, and the target of a value made for storage. A signature grep over the 181 distinct call sites finds 86 that call a parameter of the enclosing function; the rest are locals, fields read into a local, lambda captures, and parameters in signatures that span lines (`std/graphql`). Kind (d). No one module accounts for most: `std/http`, `std/arrays`, `std/graphql`, `std/ui` and the generated RPC clients share them.

**Generator host imports** are calls a compiled generator makes to its host while it runs (`Fn_::gen_builtin`: `g.text`, `g.raw_at`, `g.render`, `g.splice`). Nothing is expanded when the generator is compiled, so the group is not kind (b). It is a row kind the core lacks, with `Spec::Lanes` as the precedent: the row names the import, `@codeSplice`'s tag is the operand's static type, which the row carries on the operand's name, and `render` reads its String back through `Fn_::fetch_str`. `std/vyx`, `std/ui`, `std/i18n`, `std/tw`, `std/symbolmap` and `std/icons` hold the `std` bodies. Kind (a).

**The JSON routes** are rewrites to a call of a generated function: `jsonenc::encode_expr` names `json$w<key>` from the argument's static type, and `jsondec::decode_expr` names the target's decoder. The emitter's two arms call the rewrite and then pair the clone the tree embeds with the original (RFC-0114 section 26). A call to a declared function is a row the core states; the missing fact is the name, which the builder can compute from a type it holds. Kind (a), with `Spec::Routes` as the precedent. 129 of the 144 spellings are in `examples/`.

**Contract reflection** is a call to the nullary entry that `vyrn-genwasm` appends per contract, `__vyrnGenContractOf_<C>` (`Fn_::gen_entry`). The core states the builtin with the contract name as `Opaque:Static`, and each of the 80 lines holds that pair and nothing else. Kind (a): the route `moduleInterface` has, with a callee the argument names. `std/vyx`, `std/ui` and `std/rpc` hold all 8 bodies.

**An element of a temporary** is `bytes(s)[i]` or `chars(s)[i]` in 56 of the 63 lines. `Builder::reads_an_element` admits a receiver only where `is_place_read` holds, so the builder states the `@at` as a call. The owned path of `Builder::read_val_inner` binds any receiver through `Builder::place` and releases it after the read, so the rule exists and the heapless read does not ask it. The other 7 lines are a user container's `place at` or a projection, which RFC-0120 resolves. Kind (a).

**Line and column** are stated twice in Vyrn. `runtime$lineAt` and `runtime$colAt` read a pointer and a length, and the arm calls them. `text$lineAtV` and `text$colAtV` read an `Array<UInt8>`, and nothing calls them. The prelude row keeps the builtin for a line-start memo that lived in the interpreter, and no Rust code holds one. `lineAtV` has the prelude row's signature, and `std/text` is linked into every program (`RtModule::always`). Kind (a): a route. `std/vyx`'s `vyxLineAt` and `vyxColAt` are 50 of the 51 lines.

**Streams** are about 500 lines of inline emission over a six-word header, from `Fn_::stream_from_array` to `Fn_::stream_next`. Kind (c): runtime entries the runtime in Vyrn does not have. The `std` bodies are `std/stream`'s combinators and their lambdas, which also make a function value (`fromStep` takes the step), so they wait on that decision too.

**Method dispatch** is decided in section 2.1: a method is a call after dispatch. The builder states `Callee::Method`, and the emitter mangles the impl's name from the receiver's concrete type (`ftypes::impl_method_name`, `protocol_methods`). `m7-screen3` stated the `Fallible` impl calls as `Callee::Fn` by the same function. A generic impl callee waits on `Cx::sigs`, as `m7-screen3` left it. Kind (a).

**The map and array tail** is 8 lines alone. `@remove` is 15 of its 26 lines, and 12 of those are the generated RPC clients, which also call a stored callback. Kind (a): `@remove` on a map is `Spec::Removes` with a key.

**Logging** is `logger(n)`, the identity on a String, and a level call that is written only when the configured level clears it. This census read the arm for `logger` only; a track reads the level write before it starts. Kind (a).

**The served stream** is a trap with a constant sentence (`serve_stream_trap`). It is a `Spec::Traps` row whose message the builder states as a literal. Kind (a). The bodies are `std/http`'s `httpOpen` and `httpOpenSocket`.

**Type reflection** is a compile-time value: `jsonSchema<T>()` is one String, and `schemaOf<T>()` is a record literal (`Fn_::reflected`). The builder can state the first as a literal. The second is a tree, which needs a memo with the lifetime `project::site`'s has. Kind (b).

**A checked construction**, `T?(v)`, has a row (`Ctor::Try`) and no reader. `Fn_::try_construct` evaluates the argument and tests `predicate_holds` in one body. Kind (a): a reader.

### The decisions proposed

The line counts below are estimates from reading the code, not measured diffs.

**Line and column, proposed.** `lineAt` and `colAt` route to `std/text`'s `lineAtV` and `colAtV`, which state the rule the runtime states a second time. The arm, `runtime$lineAt`, `runtime$colAt`, their `Rt` entries and the prelude's note on the memo go in the same commit. Two rows in `RT_MODULES` come and about 70 lines go. The licence is the manifest, where `textbytes` moves, and the site export's time, because `lineAtV` reads a checked element where the runtime reads a raw byte.

**Contract reflection and the JSON routes, proposed.** A builtin whose callee is a function the program declares, named by a type or a declaration the call names, is a call to that function. One frontend function states each name (`contractOf`, `toJson`, `fromJson`), and the builder, the arm and `vyrn-genwasm` ask it. The builder states `Callee::Fn` where the program declares the name and the gap otherwise, as it reads a `Spec::Routes` row. The arm calls the function on its own arguments, so the clone pairing goes. About 40 lines come in `core.rs` and the frontend, and about 20 go from `direct.rs`. The licence is the kernel corpus and the refusal corpus, because a declared callee reads its argument by the declared capability where a builtin stored it, and the manifest for the `examples/` bodies.

**The generator host imports, proposed.** They are one row kind, `Spec::Host`, with the shape of `Spec::Lanes`: the row names the import, one emission function reads the operands' types, and both walks call it. `gen_builtin` splits into that function and the atom stream. About 60 lines in `core.rs` and `direct.rs`. No example's wasm holds a host import, so the manifest cannot witness it; the licence is `vyrn emit-gen` over the corpus and the site export, byte-identical, and the kernel corpus.

**An element of a temporary, proposed.** A heapless element read of a receiver that is not a place binds the receiver to a temporary and releases it after the read, by the rule the owned read states. `reads_an_element` stops asking `is_place_read`, and the two paths read one rule. A few lines in `core.rs`. The licence is the kernel corpus, the manifest and the residue ratchet, because the temporary's release moves into the rows.

**The small readers, proposed.** `serveStream` is a `Spec::Traps` row with its sentence as a literal, and `serve_stream_trap` moves to the frontend. `jsonSchema<T>()` is a String literal the builder states; `schemaOf` waits for a memo. `Ctor::Try` gets a reader, and `try_construct` splits so that the arm and the reader call one check. A method is a `Callee::Fn` call to the impl `impl_method_name` names, as the `Fallible` switch states it. Each is under 30 lines and closes 11 to 33 lines of the tally.

**Function values, for the lead to decide.** The core states both mechanisms, and the emitter's registry stays the one home of a tag. A function value made for storage is a `Make` of the signature's closure enum, with the target named and the captures as parts, and the emitter reads it through `build_fnval`. A call through a stored value stays `Callee::Value`, read as the call to the signature's dispatcher. A specialization is an instance the core builds per target: a `fn`-typed parameter becomes the target's captures, a call through it is `Callee::Fn` to the target, and a function name or a lambda in argument position states no value. The stored half is about 80 lines. The specialization half moves the per-target instance from `ho_call` into the lowering's instance enumeration, about 250 lines against as many that leave `direct.rs` when its arm reads zero. The other choice, dispatching every call through the closure enum and deleting the specialization, changes the cost of every higher-order call, so a bench number decides it and this census does not. Streams follow this decision and then move into the runtime in Vyrn.

### The order

By lines of the tally made whole per line written. The third column counts a line once every group it names is closed, so step 7 also takes the lines that wait on a function value and on a group before it.

| step | group | lines whole | cumulative |
|---|---|---|---|
| 1 | line and column, which deletes lines | 51 | 51 |
| 2 | contract reflection | 80 | 131 |
| 3 | JSON routes | 176 | 307 |
| 4 | an element of a temporary | 62 | 369 |
| 5 | generator host imports | 219 | 588 |
| 6 | the served stream, type reflection, the checked construction, method dispatch, logging | 93 | 681 |
| 7 | function values | 282 | 963 |
| 8 | streams and the map and array tail | 90 | 1,053 |


Step 6 counts the 7 lines that call `schemaOf`, which wait for its memo.

## What the emitter's screen refuses, fourth count (2026-09-23)

Measured on main at `3af4ca52`, before any code of `m7-tail` moved, and again at that track's head. A temporary instrument in `Fn_::core_walkable` and the statement screen under it made every refusing clause record its name and pass, so one compile gives each body's first clause and every clause it meets; the instrument is not committed. It ran over `emit-wat` of the 174 programs of `examples/` that emit, second pass, one line per body per program: on main 1,281 bodies refused, 1,083 of them with no gap; at the head 1,054 and 856. This count replaces the third one, which `m7-screen3` took over the 415 roots at `7751ce7b`.

"Only" means no other statement clause refused the body; "met" counts the bodies that meet the clause anywhere; the first three counts are main's. A clause the instrument read "with no type" is a place whose type `Fn_::core_place_ty` does not find: for a field read, a field of a layout element; for a map key store, a map whose value type the walk does not find. The last column is who holds the clause.

| first statement clause, bodies with no gap | first | only | met | first at the head | held by |
|---|---|---|---|---|---|
| a take of a field that is an array | 206 | 0 | 215 | 30 | taken here, in part |
| a store into a name that is a record | 177 | 2 | 184 | 178 | `m7-store` |
| `@str` of a String temporary | 94 | 86 | 102 | 94 | `m7-str` |
| a move of an enum (Rhs::Val) | 88 | 1 | 161 | 107 | `m7-move` |
| a store into a name that is an array | 80 | 18 | 134 | 80 | `m7-store` |
| a return of a String from an enum result | 50 | 22 | 50 | 50 | `m7-screen3` |
| discarded: a move of unit (Rhs::Val) | 48 | 29 | 124 | 0 | taken here |
| a move of an array (Rhs::Val) | 48 | 17 | 121 | 48 | `m7-move` |
| a switch on an enum with no place (named) | 29 | 28 | 34 | 29 | `m7-screen3` |
| a call (Fn) with no signature | 29 | 22 | 35 | 18 | left here |
| an operator on vectors | 27 | 6 | 29 | 0 | taken here |
| a rebuild @push with no store after it | 23 | 14 | 54 | 23 | `m7-store` |
| a read of a field whose base is a layout element | 23 | 14 | 23 | 23 | `m7-read` |
| a store into a field that is a String | 19 | 15 | 22 | 19 | `m7-store` |
| a store into a name that is an enum | 16 | 0 | 98 | 16 | `m7-store` |
| a store into a map key whose type the walk does not find | 15 | 13 | 20 | 15 | `m7-store` |
| a read of an element that is an enum | 14 | 14 | 30 | 14 | left here |
| a rebuild @append with no store after it | 14 | 0 | 14 | 14 | `m7-store` |
| a read of an element that is a record | 10 | 3 | 19 | 10 | `m7-read` |
| a take of a field that is a String | 10 | 2 | 22 | 10 | `m7-read` |
| a move of a record (Rhs::Val) | 5 | 4 | 9 | 3 | taken here, in part |
| a read of a field that is an array | 5 | 1 | 198 | 5 | left here |
| a read of module state that is a record | 5 | 0 | 9 | 5 | left here |
| a store into a map key of module state whose type the walk does not find | 4 | 4 | 4 | 4 | `m7-store` |
| a valueless return | 4 | 3 | 4 | 4 | `m7-screen3` |
| a store into a name that is unit | 4 | 0 | 11 | 4 | `m7-store` |
| a read of an element of module state whose base is a layout element | 4 | 0 | 4 | 4 | `m7-read` |
| a discarded removal of a layout | 3 | 3 | 3 | 3 | left here |
| a store into module state that is an enum | 3 | 3 | 3 | 3 | `m7-store` |
| a take of a field that is an enum | 3 | 1 | 9 | 2 | left here |
| a take of a field of module state that is an array | 3 | 0 | 4 | 3 | left here |
| a take of module state that is an array | 3 | 0 | 3 | 3 | left here |
| a discarded lane store | 2 | 1 | 4 | 0 | taken here |
| a store into a name that is a vector | 0 | 0 | 19 | 19 | `m7-store` |
| 36 more clauses | 15 | 8 | | 16 | |

A name the screen refuses is never alone: every such body also fails a statement clause. The names the name clause refused on main, bodies with no gap, met: an array 598 (321 a name the core minted, 277 a `let`), an enum 254, Unit 124, all minted and 11 at the head, where an arm stores a Unit value into the join, a record 66, a map 3 and a small array 2.

The 198 bodies with a gap are the same on main and at the head, by first gap: `Opaque:Static` 43, `toJson` 19, `Lambda` 19, `fromJson` 18, `@at` 12, a call through a function value 28 over nine names, `logger` 6, `@tally` 6, `Make:Try` 5, `jsonSchema` 5, `fromArray` 5, `serveStream` 4, `Switch:Impl` 3, `@has` 3, `lineAt` 3, `@list` 3, and 13 more tags with 16 bodies between them.

## What the tracks since have changed

Each track below retired tags, so every table above is the state before them
and the `Read` row's own tally has moved.

The short-circuit track (`m7-shortcircuit`) took `Prim:And` and `Prim:Or` out
of `core::gaps`: the core writes the branch, so the family has no tag left.

The layout-read track (`m7-read`) gave the emitter a reader for a place
(`direct::Fn_::core_read`), and `Read` and `Take` now name only the kinds it
does not address. Over `examples/` the tally splits the family into 6,079 field
reads, 2,360 element reads, 459 name reads, 158 global reads and 17 key reads,
with 323 field takes, 4 global takes and 1 element take; what is left is
`Read:Elem`, `Read:Key` and `Take:Elem`. Over `examples/` the tally reads 4,238
lines whole where it read 4,026, and `coredrive --ignored` reads 3,605 with no
gap where it read 3,449.

The opaque track (`m7-opaque`) closed the nullary constructor, which the page
above called sugar and which was the whole of what the family alone blocked:
the row states a `Callee::Ctor` call with no parts. What is left of the family
carries a kind, so `Opaque` is now `Opaque:Exit`, `Opaque:Index`,
`Opaque:Trapped`, `Opaque:Static` and `Opaque:Unbound`. Over `examples/` they
read 1,430 lines, 1,430, 1,428, 56 and 0, and the bodies the family alone
blocks fall from 786 to 6. Over `examples/` the tally reads 16,165 lines whole
where it read 15,406, and `coredrive --ignored` reads 14,495 with no gap where
it read 13,798.

The tag track (`m7-tag`) closed the family but for one shape. `core::Arm`
names the tag its arm tests, `direct.rs` chooses the arm off that, and
`core::gaps` stops naming `Switch`, so every table above is the state
BEFORE it. What is left is `?` on a declared `Fallible` (RFC-0080 M3), whose
arms the impl's own `failed` predicate picks: the row carries no call,
`Test::Impl` says so, and the family's row in table (b) now counts those 3
bodies alone.

The class does not empty, it partitions. Of the 801 bodies `coredrive` ranked
under the tag, 313 fall to a layout, 383 to a callee, 74 to a release, 28 become
whole and 3 stay. Over `examples/` the tally reads 15,268 bodies whole where it
read 15,174, and the lines that name a tag at all fall from 2,600 to 3;
`coredrive --ignored` reads 13,651 with no gap where it read 13,623, with 415
distinct bodies carried end to end where it read 393. The bodies the emitter
TAKES stand at 953.

The corpus cannot witness this family. Each of the 156 `if let`s in `examples/`
waits on another family too, so the AST arm's count does not move. The witness
is `coredrive`'s `SHAPES`: one `if let` the rows carry, compiled both ways and
asserted byte for byte.

The `for` head track (`m7-forhead`) retired `Opaque:Exit`, `Opaque:Index` and
`Read:Elem`. The builder states the length, the counter, the exit test and the
element read at the counter as rows, and `direct::Fn_::core_addr` addresses an
element. A stream's head keeps one kind, `Opaque:Pull`, at 21 lines over
`examples/`. Over `examples/` the tally reads 17,353 lines whole where it read
17,039. No `for` statement is emitted from the rows yet: its borrow binds an
array, which the frame screen does not admit.

The key track (`m7-key`) retired `Read:Key`, `Call:Builtin:print` and `Call:Builtin:@str`. The row keeps the key's place, because the kernel's alias of `m[..]` is a refusal, and the emitter reads it through the runtime's lookup; `print` and `@str` are one `Spec::Renders` row each, rendered at the operand's own type. Over `examples/` the tally reads 17,488 lines whole where it read 17,353, and the three tags fall from 16, 276 and 740 lines to 0. `coredrive --ignored` reads 15,393 with no gap where it read 15,310, with 777 distinct bodies carried end to end where it read 735, and the emitter takes 2,591 bodies where it took 2,575. `Opaque:Pull` stays at 21 lines, because a stream's pull writes the loop variable's place and no row states that, and `Switch:Impl` at 3 bodies, each waiting on `Drop` too.

The drop track (`m7-drop`) retired `Drop`, the last half of the release family. The emitter reads a `St::Drop` off its row at the place the walk gave the name, so `core::gaps` stops naming it and `coredrive` has no release class. Before the reader, the tally named the kind: over `examples/` a release was the first gap of 1,532 lines and the only gap of 1,056, and in 1,272 of the first gaps it freed a temporary, the reading site's own or an argument's. Over `examples/` the tally reads 18,468 lines whole where it read 17,447, and `coredrive --ignored` reads 16,131 with no gap where it read 15,393, with 1,006 distinct bodies carried end to end where it read 777. The emitter takes 13,384 bodies where it took 13,369: a body that waited on a release alone now stops at the frame's name clause or at another family's statement, and at no drop row.

The panic track (`m7-panic`) retired `Call:Builtin:panic` and `Call:Reserved:@panicAt`. Both are one `Spec::Traps` row, and the emitter writes the line through the same writer the arm calls, before the `trap` the builder already stated. An unaudited build states no audit hook, so a body that holds one is no longer refused at the call. Over `examples/` the tally reads 18,955 lines whole where it read 18,468, the two tags fall from 440 and 150 first gaps to 0, and `Opaque:Trapped` falls from 1,202 lines to 710; what stays is the value after a `panic` in value position. `coredrive --ignored` reads 16,563 with no gap where it read 16,131, with 1,044 distinct bodies carried end to end where it read 1,006, and the emitter takes 14,283 bodies where it took 13,384. The runtime's allocator, `runtime$malloc`, `runtime$free` and `runtime$arenaAlloc`, is among them; `runtime$intStr` waits on `@push`, `@at` and `stringFromBytes`.

The rebuilding-builtin track (`m7-rebuild`) retired `Call:Builtin:@push`, `@reserve`, `@clear`, `@append`, `@copyFrom`, `bytes` and `stringFromBytes`, and most of `@at`. The five rebuilding builtins are one `Spec::Rebuilds` row: the runtime writes the new header into the receiver, so the store after the call writes nothing. `bytes` and `stringFromBytes` are one `Spec::Builds` row, landed through the out-pointer. A heapless element read of a builtin array is a place read, and nothing after a `trap` in its list is a gap, so `Opaque:Trapped` is gone too. Over `examples/` the tally reads 23,487 lines whole where it read 18,891, of 25,018. `@at` keeps 659 first gaps: a String byte, a map entry, a projection and an owned element.

The alias track (`m7-alias`) changed no gap: every body it moved was whole in the rows already and stood down at the emitter's screen. A borrow bound by a read of a layout holds the place's address, a length is a header read, a String operator reads the rows, and a layout crosses a `modify` or `consume` parameter as its address. Over `examples/` a read of a layout was the first name the screen refused in 1,365 bodies and is in 30. Rebased onto the rebuild track, `coredrive --ignored` reads 20,380 with no gap and 1,377 carried end to end, both unmoved, and the emitter takes 15,304 bodies where it took 14,874. The kernel gained one rule on the way: a `modify` argument ends every borrow that reads what it names.
The validated-type track (`m7-where`) retired `Call:Named`. A value crossing into a validated type at a `let` or a store into a name is that type's constructor row, and the emitter reads the row with the check the arm calls, so a name of a validated type has its base's place. Over `examples/` the tally reads 23,492 lines whole where it read 23,487, of 25,018, and `Call:Named` reads 0 first gaps where it read 7.

The second screen track (`m7-screen2`) changed no gap. It counted the emitter's screen over the gate list: of 110,653 whole-body compiles, 40,710 stood down, and the largest clause alone is the `while` whose header the arm hoists, 8,398 compiles. A String read out of a field, an element or module state now passes the screen, since the walk already loads any value in one wasm local; `coredrive --ignored` reads 15,467 bodies taken where it read 15,342, with 20,385 whole and 1,381 carried end to end, both unmoved. `@at` stays the largest first gap over the gate list, 5,766 lines, and `@concat` the second, 3,819.

The heapless-layout track (`m7-modify`) changed no gap. A `let` of a read of a layout that owns no heap copies its bytes into the binding's slot, and a `for` head's borrow or a scrutinee the core minted holds the place's address; the emitter reads both off the read row. Over `examples/` 26 bodies were refused first at a heapless layout read: one was a copy the reader bound, 11 were minted names, 8 payload binders, 4 a projection's element read and two not classed. No body was refused first at a scalar `modify` argument, because the corpus hands `modify` only layouts. Rebased onto main, `coredrive --ignored` reads 20,385 with no gap and 1,381 carried end to end, both unmoved, and the emitter takes 15,470 bodies where it took 15,467.

The call-result track (`m7-result`) changed no gap: every body it moved was whole in the rows and stood down at the emitter's screen. A user function's layout result was already landed; the call results the screen still refused were `x.copy()` of a layout, a variant with a layout payload, a `push` of a String and a temporary with no binding. A declared `copy` is a call after dispatch, a temporary's name holds the address its call wrote, and the core walk gives a dead temporary's slots back as the arm does. Over `examples/` a call result was the first name the screen refused in 852 bodies and is in 144. `coredrive --ignored` reads 20,380 with no gap and 1,377 carried end to end, both unmoved, and the emitter takes 15,584 bodies where it took 15,304; rebased onto main, 15,784 where it took 15,470.

The binder track (`m7-binder`) changed no gap. A payload binder whose type is a layout holds the payload's address inside the scrutinee where the construct does not own it, and the core states it as a read out of the scrutinee, so the kernel refuses a write to the scrutinee while the binder lives; where the construct owns the scrutinee the binder moves out into a slot. Over `examples/` a payload binder was the first name the screen refused in 811 bodies and is in 0; 136 of them are taken, and the rest wait on the `Never` temporary a `panic` in value position leaves (245), a heap layout temporary (148) and the statement screen (282). `coredrive --ignored` reads 20,385 with no gap and 1,381 carried end to end, both unmoved, and the emitter takes 15,957 bodies where it took 15,781, on `m7-result`.

The slot track (`m7-slot`) changed no gap. A name holds its frame slot from its `let` to the last row that names it, `core::extent_ends` states that once from the rows, and a row gives back the slots it took for its own work; `m7-result`'s reset of a dead temporary went with it. The emitter's screen reads a made layout that binds no name of the reader's, and `fieldstore.rs`'s frame pins stay at 16 and 48. `coredrive --ignored` reads 16,012 bodies taken where it read 15,957, with 20,385 whole and 1,381 carried end to end, both unmoved. Over `examples/` no function's frame grows and 36 shrink.
The hoisted-header track (`m7-hoist`) changed no gap: every body it moved was whole in the rows already and stood down at the emitter's screen, which kept any body with a `while` the arm could hoist. The builder binds the header of a container the loop indexes and never writes, as a borrow the loop walks, and the emitter takes it apart once, for a `while` and a `for` alike; `kernel::writes` is the proof, where the arm's was the syntax. `coredrive --ignored` reads 20,385 with no gap and 1,381 carried end to end, both unmoved, and the emitter takes 17,700 bodies where it took 16,012, on `m7-slot`. `Stmt::While` on the arm falls from 8,546 to 3,785. The arm's own hoist stays for the `while`s it still emits.

The trap track (`m7-never`) changed no gap: `Opaque:Trapped` was already out of the tally, because every reader stopped at a `trap`. The builder no longer writes rows after one. A `panic` in value position leaves no `Never` temporary, and one cut ends every list of a finished body at its `trap`, so the join reads the other arm alone. Over `examples/` a `Never` temporary was the first name the emitter's screen refused in 378 bodies and is in none; those bodies then stop at the aggregate scrutinee of `match stringFromBytes(..)`, 202, and at the hoisted header, 174. `coredrive --ignored` reads 20,385 with no gap and 1,381 carried end to end, both unmoved, and the emitter takes 17,701 bodies where it took 17,700 on main at `c190b869`; the one is `Fallible__Http__success`, which the binder track had cleared of every other clause.
The String accumulator track (`m7-append`) changed no gap. `s = s + a + b` on an accumulator the whitelist admits is one `@strAppend` row, a `Spec::Rebuilds` row, and the store after it, and the accumulator's `let` gives the name its ownership word, held for the name's extent; the emitter reads the pair as it reads `@push`, and a plain store into a String name frees what it replaces where the row says so. Over `examples/` the String store was the first clause the screen refused in 571 bodies and the only one in 569, and is in none. On main after `m7-never`, `coredrive --ignored` reads 20,385 with no gap and 1,381 carried end to end, both unmoved, and the emitter takes 18,122 bodies where it took 17,701. The arm still appends in place 145 times over `examples/`, in bodies the screen refuses for another clause or that carry a gap, in two `gen fn` bodies, and once into module state.
The builtin-row track (`m7-rows`) retired `Call:Builtin:@concat`, `Make:Map`, the I/O builtins, `parse`, `listDir`, `@pop`, `@swapRemove`, `@keys`, the SIMD builtins, `moduleInterface`, `lex` and `@charCount`, and most of what was left of `@at`. `@concat` is the String `+` row the core already states, a String's byte is an element read, and a map entry whose `Option` owns no heap is a key read. The rest are rows of four kinds, `Spec::Builds`, `Spec::Removes`, `Spec::Lanes` and `Spec::Routes`, and each kind's emission is one function both walks call; a routed builtin is read as a call to the function its row names. Rebased onto `m7-append`, over `examples/` the tally reads 24,719 lines whole where it read 23,492, of 25,018. `coredrive --ignored` reads 21,330 with no gap where it read 20,385, with 1,642 distinct bodies carried end to end where it read 1,381, and the emitter takes 18,591 bodies where it took 18,122. `@at` keeps 8 first gaps under `coredrive`: a user container's `place at` and a temporary receiver. `contractOf` stays a gap, because its argument is a contract name the row carries as `Opaque:Static`.

The scrutinee track (`m7-scrutinee`) changed no gap. A temporary a `match`, an `if let`, a `?` or a `for` is keyed by holds the address its call wrote, as an unbound one does, and the switch, the loop and the release at the construct's exit read that place. Over `examples/` the keyed temporary was the first clause the screen refused in 1,287 bodies and is in 0; 1,225 of them are taken, `runtime$intStr` among them, 27 wait on a switch on a payload binder and 24 on the valueless `return` of a `?` on an `Option`. The walk reads a `for` container's buffer-only release where the arm reads it, which three programs ran apart without, and gives a binder's slot back at its arm's end. Rebased onto `m7-rows`, `coredrive --ignored` reads 21,330 with no gap and 1,642 carried end to end, both unmoved, and the emitter takes 19,622 bodies where it took 18,591.

The third screen track (`m7-screen3`) retired `Switch:Impl`. A `?` on a declared `Fallible` states the impl's `isSuccess` call and its negation before the switch, and the failing arm tests that name (`Test::Holds`); the two impl calls are `Callee::Fn`, functions the program declares, where `Callee::Method` read as a callee gap. The failing arm of a `?` on an `Option` returns `None` of the frame's result, the failing arm of a `?` on a `Result` returns `Err` of its error binder, taken, and the emitter's screen reads a switch on a payload binder as a switch on a place. `core::cut` ends a list at an `if` or a `switch` whose every arm traps, which `substring`'s nested `match` needed once the screen admitted it. `coredrive --ignored` reads 21,333 bodies whole where it read 21,330 and 1,643 carried end to end where it read 1,642, and the emitter takes 20,213 of 21,512 where it took 20,169 on main `8f290457`. The section above tables what the screen refused before the track moved: made layouts lead, then `@str` of a String temporary and the failing `return` of a `?` on a `Result`, which the track took.

The displaced-value track (`m7-box`) changed no gap. A store into an owned place released buffers at flat offsets and payload boxes, so a box's contents, an element and a String in a payload word leaked at every store the core marks as releasing: 15 blocks over the eleven stores of `a_store_releases_the_whole_value_it_displaces`, 0 under the head. The emitter keeps the displaced value whole and runs the release a `let` exit runs on it, so the release row has one reader for a drop and a store. A store runs the declared `release` of what it displaces; a stream is the one value a store leaves alone. `coredrive --ignored` is unmoved at 21,330 whole, 1,642 carried end to end and 19,622 taken.

The write-points track (`m7-writes`) changed no gap. The judgment ends a borrow at the write points `kernel::writes_of` lists for a row, and the builder's hoist asks the same list, so the rows the kernel writes are stated once. A store into an element of a container a `while` walks ends no header, so the header of a loop that writes its elements is hoisted too: `runtime$intStr`, `num$f64Str`, `sha1`, `editDistance` and `piDigits` read it once before the loop, in 107 manifest rows. `coredrive --ignored` reads 21,330 with no gap, 1,642 carried end to end and 19,622 bodies taken, all unmoved, because each body that moved was taken already. A `for` keeps its refusal of an element store, because its loop variable is an address into the element the store may free.

The made-layout track (`m7-made`) changed no gap: every body it moved was whole in the rows and stood down at the emitter's screen. A layout made for one part of a record or an array is held until its parent is built and written at the part's offset, as the arm builds a nested literal, and a part a reader bound is copied there; an empty array literal has no part to place. Over `examples/` a made layout was the first name the screen refused in 1,295 bodies and is in 276, of which 216 are a layout part made into a temporary with a slot of its own, mostly a call result. The emitter's argument screen also refuses a borrow handed to `consume`, because the core drops the scrutinee whole after it (`std/vyx`'s `vyxProcessElem`). `coredrive --ignored` reads 21,330 with no gap and 1,642 carried end to end, both unmoved, and the emitter takes 20,005 bodies where it took 19,622.
The module-state track (`m7-state`) changed no gap. A call ends every borrow of the globals its callee stores into, by the effect judgment's write half kept by place and read through `kernel::writes_of`, so a layout read from module state holds the global's fixed address, a `match` and a `for` over module state read it there, and a String store into module state and a module-state accumulator's `@strAppend` row are the core walk's. Over `examples/` a module-state clause was the first the emitter's screen refused in 71 bodies and is in 31: the String store 31 to 0, a scrutinee 10 to 0, a named layout read 6 to 5, a `for` head 10 to 9. On `m7-made`, `coredrive --ignored` reads 21,330 with no gap and 1,642 carried end to end, both unmoved, and the emitter takes 20,034 bodies where it took 20,005. The builder hoists no module-state container, 4 loops over `examples/`.
The hole track (`m7-hole`) changed no gap. A payload binder read out of a scrutinee the frame keeps and handed to a `consume` parameter leaves a hole in the scrutinee, spelled `.Variant.i`; an edge release carries the holes its edge has, and the enum release walk skips a payload hole on its live tag. Four shapes that freed a payload twice under both walks run clean under the free audit. `coredrive --ignored` reads 21,330 with no gap, 1,642 carried end to end, and 20,034 bodies taken, all unmoved, and no example's wasm moved. A payload out of a value whose type declares `release` may not be handed on, and the kernel refuses it at the take; `vyxProcessElem` consumes its node instead. An edge release runs a declared `release`.

The module-state hoist track (`m7-statehoist`) changed no gap. After the effect judgment, `augment` builds again every body that hoisted a header and calls a function that stores into module state, and `kernel::writes` answers for a global by the rule for a name, so a `while` hoists the header of module state that no call of the loop stores into. The 4 loops over `examples/` that read module state are hoisted, and 5 bodies are built twice. A refusal of a hoisted borrow of module state names the `let` in its fix again. `coredrive --ignored` reads 21,330 with no gap, 1,642 carried end to end and 20,034 bodies taken, all unmoved, and 2 manifest rows moved.

The part track (`m7-part`) changed no gap: every body it moved was whole in the rows and stood down at the emitter's screen. A part of a record, an array or a variant's boxed payload is written at its offset in the parent's storage where its own row stands, whether a call, a variant or a literal makes it; the parent's storage, a heap array's buffer or the box is taken at its first such part, and a parent that is itself a part takes its offset in its own parent. Over `examples/` a part in a temporary was the first clause the screen refused in 216 bodies and is in none. The held row of `m7-made` stated the same rule for the literals it could hold, and it went, with no frame and no taken body moved. `coredrive --ignored` reads 21,330 with no gap and 1,642 carried end to end, both unmoved, and the emitter takes 20,169 bodies where it took 20,034.

The layout-read track (`m7-read`) changed no gap: every body it moved was whole in the rows and stood down at the emitter's screen. An owned `for` variable over a container the loop alone owns binds the element's address in the buffer, as a borrow does, and a `consume` of it hands the element on; a take of a String out of a field loads the pointer the field held, and the release rows carry the hole. Over `examples/` a read or take of an element that is a record was the first clause the screen refused in 10 bodies and is in 1, and a read or take of a field that is a String in 22 and is in 0. `coredrive --ignored` reads 21,333 with no gap and 1,643 carried end to end, both unmoved, and the emitter takes 20,231 bodies where it took 20,213, on main at `363c013e`. A `for` over a container it alone owns that leaves by `return` leaks the elements it never reached, on main and under both walks.

The early-exit track (`m7-forexit`) changed no gap. A `for` whose every element leaves through the loop variable frees its buffer alone, and a `return`, a `?` or a `break` out of it leaked the elements from the counter to the end under both walks. The builder states their release as rows before the exit's own, a loop that reads each unreached element and drops it, and `Facts::unreached` names the exits that owe it for the AST walk. Over `examples/` 11 exits owe it, all `return` in generated decoders, and 10 manifest rows moved. `coredrive --ignored` reads 21,330 with no gap, 1,642 carried end to end and 20,169 bodies taken, all unmoved.

The small-clause track (`m7-tail`) changed no gap. It counted every clause the emitter's screen refuses on main, which is the fourth count above, and took the clauses no other track holds, in order of bodies. A vector is an arithmetic operand; a `match` statement's Unit join, which no row names once the builder states no `do` of a bare value, needs no place; a discarded lane builtin is typed by the row's producer type; an `extern` call is `Fn_::extern_call`, which both walks call; a layout taken out of a field into a part of a literal moves its header to the part's offset, the hole already in the root's release; and `let mut b = a` of a heapless record is `core_copies`' copy. On the way it fixed a float literal left of a `Float32` operand, which the core walk typed `Float64` into invalid wasm. The screen refuses 1,054 bodies where it refused 1,281, 856 with no gap. On `3af4ca52` the emitter took 20,381 bodies where it took 20,169; rebased onto `81065b21`, `coredrive --ignored` reads 21,333 with no gap and 1,643 carried end to end, both unmoved, and the emitter takes 20,444 bodies where it took 20,231.

The host-import track (`m7-host`) retired `Call:Reserved:@codeText`, `@codeSplice`, `raw`, `rawAt` and `render`. They are one `Spec::Host` row, and the emitter reads it through `Fn_::host`, which the arm calls too; `Code + Code`, the host's `concat` import, is read there as well. Over the 415 roots with the generator cache off, the tally reads 71,406 lines whole where it read 71,180, of 72,353, and the 226 lines that named a host import, 39 spellings, read whole. `coredrive --ignored` reads 21,340 with no gap and 1,648 carried end to end where it read 21,333 and 1,643, and the emitter takes 20,444 bodies, unmoved. Under `emit-gen` the arm still writes 8 host calls, all in `std/icons`' `glyphSource`, which waits on `@str` of a String temporary.

The move track (`m7-move`) changed no gap. A layout bound by moving an owned name takes that name's place and slot, with no copy, and the release placed for the value is the binding's. A take out of a field or module state into a `let` copies into a slot, as the arm's `let` does, except a take that a rebuild hands back to its place, which holds the place's address, so `s.keys.push(k)` pushes in place as the arm does. Over `examples/` a move or a take was the first name the screen refused in 90 bodies and is in 2, and 44 of the 90 pass the screen. `coredrive --ignored` reads 21,330 with no gap and 1,642 carried end to end, both unmoved, and the emitter takes 20,474 bodies where it took 20,444. An element store inside a `while` reads the header hoisted for its container, in the same 107 manifest rows. A store into a nested place, `b[i].vx = v` or `s.slots[i] = v`, is one store into its path, where the rows stated a read of the element, a store into its field and a store of the element back; the emitter takes 20,480 bodies, nbody's `advance` and `offsetMomentum` among them. `ksAdd` stays on the arm in 19 programs, at `s.slots = slots`.
