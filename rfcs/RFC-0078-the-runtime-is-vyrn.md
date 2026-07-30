# RFC-0078 — The Runtime Is Vyrn

- **Status:** **Complete as scoped** (M1–M5 landed). The claim "everything above
  the primitive core is written in Vyrn" now holds for everything the core turned
  out not to contain; the core itself is stated in "M1 + M5, as landed" and two
  named language decisions — a raw-memory view, and an abort primitive — are what
  a *larger* core would need. Neither is opened here, and neither is this RFC's to
  make.
- **Depends on:** RFC-0014 (the I/O builtins and their canonical wording),
  RFC-0018 / RFC-0059 (`fromJson`/`toJson`, and `std/json`, which already
  implements JSON *in Vyrn*), RFC-0077 (the direct wasm backend, which is what
  made the duplication impossible to keep ignoring)
- **Evidence (counted, this repo):**

  | | |
  |---|---|
  | builtins the checker knows | 40 as first counted; **42** reserved names still have a Rust implementation, plus 4 shadowable gen-only ones (RFC-0054) — see the census |
  | Rust implementations in the interpreter | 50 as first counted; **62 by the census's stated method** at M1+M5, which is a larger number for a smaller core: it counts the 13 `@`-prefixed desugars and the 11 pre-dispatch guards that a count of *builtins* does not. The comparable figure is 42. |
  | C functions in the runtime shim | 80 as first counted; 94 (65 exported, 29 static) by the method M4a states and M4c reproduces; **47 (35 exported, 12 static)** after M3's swap. M2b took the exported boundary from 74 to 70; M3-as-measured deleted one `static`; M4c deleted one exported (`__vyrn_strncmp`); M3-as-landed deleted **47** and halved the file. |
  | of those, the JSON DOM alone | 49 as first claimed — but SHARED writer/reader, so M2 retired SIX (not 11: the parser's unescaper shares the buffer) and M3 retired **47** (not 38, and not the 32 M3-as-measured predicted: its `static` count was 5 where the section holds 17 — see "M3, as landed", which states the method that reproduces the 94 baseline) |
  | `std/json.vyrn` — a JSON reader and writer, in Vyrn | 752 lines |
  | interp + shim + textual emitter + direct emitter | 27,334 lines |

---

## The problem

A builtin is implemented **once per execution engine**:

| engine | where `toJson` lives |
|---|---|
| interpreter | Rust, in `interp.rs` |
| native | C, in `RUNTIME_SHIM`, reached by a `call` the textual emitter prints |
| wasm (LLVM path) | the same C |
| wasm (direct path) | must be re-expressed as emitted wasm |

And JSON is implemented a *fourth* time, in Vyrn, in `std/json.vyrn` — 752 lines
with `parseJson` and `emit` — used by different callers.

Parity has kept these honest, which is exactly why the cost stayed invisible.
The copies do not drift, so nothing ever breaks; they simply have to be written,
reviewed and maintained N times. RFC-0077 made that unignorable: every remaining
milestone is re-expressing a piece of the C shim as wasm, and the direct backend
is already 6,377 lines.

This session found four instances of the duplication leaking, all of them found
by *porting* rather than by using the code:

- `direct.rs` held private copies of two `stringFromBytes` messages (M2g), later
  single-sourced as `IO_MESSAGES` (M2j).
- `emit_validation` held a byte-for-byte copy of the binding walk that
  `emit_predicate_cond`'s own comment claimed to be the only site of (M2d).
- `mangle_name` is not injective, so two instantiations can collide on one
  symbol and the textual driver silently skips the second (M2e).
- The shim's `__vyrn_now_millis` was `getenv` + `timespec_get` — a C wrapper the
  wasm backend was paying a toolchain to call `environ_get` through (M2j).

## The change

**Everything above the primitive core is written in Vyrn.**

RFC-0077 M2j measured the core exactly: a directly-emitted module's entire
import list is **twelve WASI functions plus `__vyrn_malloc`**. You cannot write
the allocator without a memory-growth primitive, and you cannot write `fd_write`
in terms of itself. Everything else — JSON, UTF-8 decoding, string formatting,
number parsing, the map, the logger — is a library that is in C for historical
reasons, not for necessity.

So:

- A small, named set of **primitives** each backend must provide: memory growth,
  and the syscall surface. Nothing else.
- A **core tier in Vyrn** above the primitives, using only them.
- `std/` above that, as it already is.
- A builtin becomes, at most, a **type-directed compiler part plus a call into
  Vyrn**.

That last point is the whole design, and `toJson` shows it cleanly. Today:

```
emit_encode(v, ty)      -> build a DOM from the static type   [needs the compiler]
__vyrn_vj_encode(node)  -> serialize the DOM to text          [does not]
```

Only the first half requires compiler knowledge — walking a record's fields and
an enum's tag according to the value's static type. The second half already
exists in Vyrn as `std/json:emit`. Rewiring `toJson` to produce a `Json` value
and call it retires **11 of the shim's 80 functions** — not 49, as this
document first claimed: the `__vyrn_vj_*` DOM is shared with the *parser*, so 49
is the pair's total and M3 takes the other 38. And it hands the direct backend
`toJson` for free only once the writer is importable without the reader.

## Why this is safe HERE

The same invariant that made RFC-0076 and RFC-0077 safe: **interp == native ==
wasm, byte-identical including traps**, over every example on every commit. A
runtime written in Vyrn is checked by the suite that already exists, on the same
terms as user code — and unlike the C shim, it is checked *three ways* rather
than trusted once.

It also inverts today's arrangement in a useful way. A Rust fast path in the
interpreter is perfectly fine **provided the Vyrn implementation is the
definition and parity proves the fast path agrees with it.** That converts three
definitions into one definition plus two optional caches, which is the difference
between multiplicity that must be maintained and multiplicity that can be deleted
at any time.

## What this is not

- Not a rewrite of the compiler. The frontend, checker and emitters are unaffected.
- Not the removal of `extern` or the C shim. Native keeps a shim for its
  primitives; it just stops holding a JSON parser.
- Not a performance project. It will make some interpreted paths slower, and
  RFC-0076 already moved the latency-critical case (generation) to compiled wasm.
- Not self-hosting. It is the part of self-hosting that pays for itself
  immediately, done deliberately rather than as a side effect.

## Milestones

- **M1 — name the primitives.** Enumerate the irreducible set and give it a
  single declaration both backends and the interpreter read. RFC-0077 M2j's
  twelve WASI imports plus `__vyrn_malloc` is the starting list; confirm it
  against the interpreter's Rust arms, which may need primitives wasm does not.
  **DONE, jointly with M5 — see "M1 + M5, as landed".** It needed every later
  milestone first: the starting list was short by three *categories*, not by three
  entries, and each was found by a milestone walking into it. The single
  declaration is a table checked against the code by a test rather than a list
  that can rot, and the census's one finding is that **one** of the 62 remaining
  Rust arms has no reason to be a primitive at all.
- **M2 — `toJson` through `std/json`.** The largest item, with its Vyrn
  implementation already written and parity-tested. Bytes must be pinned before the
  swap, not compared after.
  **DONE (M2a the library split, M2b the swap) — see the three notes below.** The
  acceptance line as written was wrong twice: it is SIX shim functions, not 49
  (49 is the writer/reader pair's total, and four of the eleven M2's own note
  claimed are shared with the parser), and "no line of new wasm lowering" held for
  `toJson` but needed two general fixes the direct backend was missing anyway. The
  ladder went 39/81 -> 43/81. Number formatting needed nothing: `toString()` already
  matches the shim byte for byte.
- **M3 — `fromJson`. DONE — see "M3, as landed".** It reads through
  `std/jsonread` and decodes through a per-type walk generated as Vyrn, on the
  interpreter, native and both wasm backends, retiring **47** C functions (half the
  shim) and raising RFC-0077's ladder 49/87 -> 54/87. The `Option<T>` decoder shape
  the note below predicts does not exist in v0.1 (`Option<Option<U>>` is refused, and
  a bare `Option<U>` IS a decode target), so a decoder answers in a 0-or-1
  `Array<T>`. En route it found a leading-zero laxity in the C reader the strictness
  ruling had not named, and a rounding-carry bug in M4a's own `parseFloat64`.
  The historical analysis follows.
  **BLOCKED ON M1, AND SEQUENCED AFTER M4 — see "M3, as
  measured".** Three things make it not a mirror of M2: `Float64` has no decode
  expression in Vyrn at all (`Float64("1.5")` is a check error, `parse` is
  `String -> Option<Int64>`), so text→number is a missing PRIMITIVE; a refined
  type cannot hold a value that failed its own predicate, so an accumulating
  decode needs decoders returning `Option<T>` rather than M2b's walk shape; and
  the two readers disagree semantically, which needs a ruling before any swap.
  The parser half, plus RFC-0018's `Issue`
  accumulation.
  **NOT DONE, and not "same shape" — see "M3, as measured" below.** The bytes are
  pinned (`examples/jsondecbytes.vyrn`, failure shapes included) but the swap is
  blocked on a primitive that does not exist and is on no milestone's list: there is
  no `String -> Float64` in the language, so a decoder cannot be written in Vyrn at
  all. M3 moves after M4, and M1's list is short by one entry.
- **M4 — the string and number tier.** `chars` (UTF-8 decode), `parse`, the `%f`
  formatter `f64_str` (~300 lines of emitted wasm that should be ~40 lines of
  Vyrn), `stringFromBytes`.
  **M4a (the number half) DONE — see "M4a, as landed".** Text -> number is a
  LIBRARY (`std/num`), standing on two irreducible primitives (`floatBits` /
  `floatFromBits`) rather than on a `parseFloat` builtin. M3's block is lifted.
  `f64_str` was NOT retired and the reason is measured rather than deferred.
  **M4b DONE as three equivalence proofs (`std/codecs`, `std/text`, `std/strpred`)
  and M4c DONE as the swap — see "M4c, as landed".** Ten of the fourteen builtins
  those modules cover now route into Vyrn on every engine, deleting 603 lines of
  emitted LLVM IR, 292 lines of Rust and one C function, and raising RFC-0077's
  ladder 46/84 -> 49/87. Four are refused with reasons: `slice` needs an abort
  primitive, `byteLength` is a compile-time-folded view, and `lineAt`/`colAt` are
  the interpreter cache M5 owns. `stringFromBytes` and the `%f` direction remain.
- **M5 — the interpreter's fast paths become caches.** Each Rust arm either
  delegates to the Vyrn definition or is documented as an optimization that
  parity proves equivalent. The count of Rust arms should fall.
  **Partly pre-empted, and narrowed — see "M4c, as landed" and "M3, as landed".**
  M2b, M4c and M3 deleted twelve Rust arms outright rather than keeping any of them
  as a cache, so what is left for M5 is the caches that are DELIBERATE:
  `lineAt`/`colAt`'s memoized line-start table, which a Vyrn library cannot hold
  because generators may not touch module state.
  **DONE as scoped, jointly with M1 — see "M1 + M5, as landed".** The count fell
  before M5 was taken, so what M5 could still contribute was not another deletion
  but the *statement* that the remainder is justified. There is exactly one
  deliberate cache (`lineAt`/`colAt`), it is documented as one, and parity proves
  it agrees with `std/text`'s `lineAtV`/`colAtV` at every offset of twelve buffers.
  Two builtins are refused on a measured cost (`@str`, `@concat`) and one is
  refused for no reason at all, which is the finding.

## Acceptance

- Parity green throughout — every milestone is byte-identical on stdout, stderr
  and exit code, or it does not land.
- The shim's function count falls, measurably, per milestone. It was 80 when this
  was written; M2b took it to 74, and took eleven `declare` lines with it; M4c took
  one more (`__vyrn_strncmp`) and two more `declare` lines; **M3 took 47 and 22**,
  which is the largest single drop and takes the file below half its size. **A milestone that
  retires no builtin reports a zero rather than a smaller number reached by counting
  differently** — M3, M4a and M4b each did, and each stated its method.
- RFC-0077's ladder does not regress, and rises where a builtin becomes Vyrn.
- No builtin has two *definitions*. Caches are allowed; second opinions are not.

## Risks, honestly

**Byte-exactness during migration.** `std/json:emit` must produce what
`__vyrn_vj_encode` produces — key order, escaping, number formatting. Parity will
catch a difference, but the bytes must be pinned *before* the swap so the test
proves equality rather than describing whatever came out.

**Layering, and bootstrapping.** The core tier may use only primitives, or it
will recurse into itself. `std/json` uses `String`, `Array` and `Map`, all of
which need the allocator — so the tiering has to be real, not aspirational.

**The interpreter gets slower** where a Rust arm becomes an interpreted Vyrn
call. Mitigated by M5's framing (fast paths as caches, not definitions) and by
RFC-0076 having already moved generation off the interpreter.

**A partial migration is worse than either end state**, because a builtin that is
half-Vyrn and half-C has two definitions and a seam. Each milestone must move a
whole builtin or none of it.

**This is the second RFC in flight.** RFC-0077 is at 39/80 and its remaining tail
is mostly the builtins this RFC would delete rather than lower. That is an
argument for sequencing, not for doing both at once — see the note below.

## Relationship to RFC-0077

They point at the same work from opposite ends. RFC-0077's remaining ladder is
dominated by builtins: `toJson` 6, `fromJson` 2, `chars`, `parse`, `logger`,
`cell`/`get`/`set`. Every one of those is a piece of C runtime awaiting a wasm
expression.

**If RFC-0078 lands first, RFC-0077 does not have to write them at all.** They
become Vyrn that the direct backend already knows how to compile. The 300-line
`f64_str` M2h wrote is the cautionary example: it exists because there was no
`snprintf` to call, and it would be ~40 lines of Vyrn that every backend shares.

The recommended sequence is therefore RFC-0078 M2–M4 *before* RFC-0077's builtin
tail, with RFC-0077's non-builtin rows (`Match` on strings, `if let`, `?`,
RFC-0023, `spawn`, `region`) proceeding independently.

---

## M2, as landed: the bytes are pinned, the swap is not made

M2 did not land as written. Two of its premises measured true, one measured
arithmetically wrong, and one measured *inverted* — so what landed is the
prerequisite (the bytes, committed as a test) plus the corrections below.

### The bytes: two disagreements, both legal, both harmless

`examples/jsonbytes.vyrn` walks the surface where two JSON writers can differ —
all 32 control bytes, quote/backslash/slash, raw multi-byte UTF-8, every sized
integer at its bounds, `Float64`/`Float32`, declaration field order, the
`None`-omission — prints it from `main` so parity covers interp == native == wasm,
and pins the rows as literals in `test` blocks. The literals are the C shim's
current answer, deliberately: pinning *after* the swap proves whatever came out.

Run before touching anything, it produced exactly two disagreements out of the
whole surface. Bytes `0x08` and `0x0c`: `vsb_escape` writes the long `\u0008` /
`\u000c`, `std/json`'s `emitString` writes the short `\b` / `\f`. Both are valid
JSON and both parse back to the same string, so this is a choice.

**`std/json` is the one to keep.** Not because short forms are better JSON — they
are equivalent — but because of who the callers are. `std/json`'s `emit` has
committed consumers (`std/tw`, `std/i18n`, `std/openapi` and their golden
outputs); `toJson`'s long form has no caller in the repo that can observe it,
since no example, app or std module encodes a string containing a backspace.
Changing `toJson` moves nothing; changing `std/json` moves generator output. So
the swap, when it happens, flips two lines of `examples/jsonbytes.vyrn` and
nothing else.

Two other things the run settled, both of them risks the RFC listed:

- **Numbers are not at risk at all.** `JNum` carries raw validated text, so number
  formatting stays exactly where it is — in the compiler, which is the only place
  that knows an `Int8` from a `UInt64`. `%f`'s six fixed places, `1.500000` and
  `-0.333333` and `340282346638528859811704183484516925440.000000`, are pinned as
  literals and unaffected by the swap.
- **NUL never reaches the encoder.** `stringFromBytes` rejects it first
  (RFC-0014's rule), which is why the shim's NUL-terminated C strings never had to
  answer for truncating a string mid-escape. `std/json` handles the byte anyway.

### 49 is not M2's number — it is M2 plus M3's

The RFC's acceptance criterion, "49 shim functions deleted", cannot be met by M2
alone, and this is arithmetic rather than judgement. The `__vyrn_vj_*` DOM is
shared by both halves: the *parser* builds nodes through `vj_str`, `vj_bool`,
`vj_null`, `vj_num_text`, `vj_set` and `vj_push`, and `fromJson` reads them back
through `vj_kind`, `vj_get`, `vj_len`, `vj_at`, `vj_obj_len`, `vj_obj_key`,
`vj_obj_at`, `vj_str_get`, `vj_asint` and `vj_asfloat`. None of those can go while
`fromJson` is still C.

What M2 alone retires is the serializer and the encode-only number constructors:
`__vyrn_vj_encode`, `__vyrn_vj_write`, `vsb_init`/`vsb_ensure`/`vsb_putc`/
`vsb_puts`/`vsb_escape`, and `vj_int`/`vj_uint`/`vj_float`/`vj_num_text` — **11 of
80**. The DOM itself falls with M3, and 49 is the pair's total. The per-milestone
acceptance line should say 11 then 38, not 49 then 0.

### Layering: clean, and checked rather than assumed

`std/json` imports nothing. Its writer path reaches only `bytes`,
`stringFromBytes`, `Array.push`, `+` and `match` — no `toJson`, so a `toJson` that
calls it cannot recurse. The six std modules that DO call `toJson` (`std/storage`,
`std/rpc`, `std/html`, `std/ui`, `std/vyx`, `std/connect`) all sit above it. The
tiering M2 needs is therefore already real, not aspirational.

### The premise is true for the writer, and the module is the problem

Measured, not argued: lift `Json`, `JsonField`, `hex2`, `emitString`, `emitArr`,
`emitObj` and `emit` into a standalone module, and `VYRN_WASM_BACKEND=direct`
compiles it and wasmtime runs it, producing the same bytes as the interpreter.
**The writer half of `std/json` already runs on the direct backend today**, with
no new lowering. That is the RFC's central claim, and it holds.

What does not hold is the sequencing. `import { emit } from "std/json"` links the
whole module, and the *reader* is not compilable by the direct backend: it needed
a branch yielding `Ok(..)`/`Err(..)` (`std/json.vyrn:168`, landed alongside this
note), and it still needs `?` (`std/json.vyrn:202`) and `if let`. Those are
RFC-0077's own non-builtin rows. So:

> **"If RFC-0078 lands first, RFC-0077 does not have to write them at all" is
> false for M2 as written.** M2 does not remove a dependency on RFC-0077; it
> exchanges six builtin rows for two general ones, and it cannot land until they
> exist — unless the writer moves into a module the reader does not come with.

That last clause is the cheap way out and the recommendation: split `std/json`'s
writer into its own module which `std/json` re-exports. It costs a file, it is
what `toJson` should link anyway (a serializer has no business dragging in a
parser), and it makes M2 independent of RFC-0077 again — which is what the RFC
claimed for free and can have for one file.

### Linking a builtin to a module: there is no prelude, and co-naming bites

`std/` modules are linked by import and only by import. There is no prelude, no
always-linked module, and `builtin_alias_exports`'s `std/result` / `std/option`
rows are validated no-ops rather than real links. So a builtin that calls into a
module needs an **implicit import**, injected into every module that mentions
`toJson`.

That import must bind names the user cannot take, and the reason is specific:
RFC-0022 resolves a co-naming collision by renaming the **foreign** decl to free
the name for the local one. A user module that defines its own `emit` and calls
`toJson` would therefore have the desugar's `emit` call resolve to *their* `emit`,
silently. The implicit import has to alias to `@`-prefixed internal names — the
convention the existing builtin desugars already use (`@concat`, `@list`, `@pop`)
and which no source can spell.

### The shape M2 should take, when it is taken

`schemaOf` is the precedent, and it is already three-way proven: it builds an
`Expr` from a type declaration and hands it to `gen_expr`, so the interpreter, the
textual emitter and the direct emitter all compile the same tree and none of them
knows about `schemaOf`. `toJson` should be the same thing one size up:

- the type-directed walk becomes an AST desugar in `vyrn-frontend`, beside
  `schema_struct_lit`, producing `@jsonEmit(@enc_T(x))`;
- self-referential types need per-named-type encoder *functions* synthesized into
  the linked `Program` — the AST analogue of the IR's `__vyrn_enc_{n}`, which
  exists for exactly that reason;
- plus the aliased implicit import, and the writer module above.

Then `emit_encode`, `emit_encode_enum_body`, `emit_encode_result` and their IR
number formatting all delete, the direct backend gets `toJson` without a line of
lowering, and the interpreter's `encode_val` becomes M5's cache-or-delete question
rather than a second definition.

### Gates, at this note

1212 workspace tests (was 1210), parity green three ways over 81 examples
including the new one, the RFC-0077 ladder unchanged at 39/81, genwasm green. The
shim is still at 80 functions: M2's payoff is real, but it is 11, and it is not
collectable until the writer has a module of its own.

---

## M2a, as landed: the writer has its module, and the split had to run backwards

The prerequisite the note above asked for is done. What it asked for *specifically*
— a new leaf module holding the writer, re-exported by `std/json` — is not what
landed, because re-export does not exist and is not cheap.

### There is no re-export, so the split inverted

`link` accepts `import { X } from "M"` only when `X` is **declared** in `M` and
exported (`loader.rs:2442`, against an `owner` map keyed by decl name). An imported
name is not a declaration, so a module cannot pass a name through. Wrapper
functions would cover `emit`/`emitPretty`/`jsonEq`, but not the `Json` and
`JsonField` *types*: an alias `type Json = JsonW` registers no variants (`link`
harvests those from `Type::Enum` in the decl's own base), so `JObj(..)` would stop
resolving at every consumer. Adding a real `export … from` form is parser plus
checker plus LSP work to avoid editing six import lines.

So `std/json` **keeps** the tree and the writer and now imports nothing, and the
reader moved out to `std/jsonread`, which imports the tree. Same one-directional
layering, same property `toJson` needs — a serializing caller links the writer
alone — reached by moving the half with fewer consumers. `emit`, `emitPretty`,
`jsonEq`, `Json` and `JsonField` still come from `std/json` unchanged, so no
generator's JSON output moves; only `parseJson`'s home changed, costing four std
modules and the two generated-module import lines in `std/openapi` / `std/ui` one
extra import each.

Proven rather than asserted, on the real module rather than a lifted spike: a
program whose only import is `import { Json, JsonField, emit } from "std/json"`
compiles under `VYRN_WASM_BACKEND=direct` and wasmtime prints the same bytes as the
interpreter. The reader is no longer in the link, so its `?` and `if let` are no
longer M2's problem.

### Two measurements that shrink what is left

**Number formatting needs nothing at all — not even M4.** The note above said
numbers "stay exactly where they are, in the compiler". Measured, they do not have
to: `toString()` on every numeric type already produces byte-identical text to the
shim's `__vyrn_vj_int` / `__vyrn_vj_uint` / `__vyrn_vj_float`, including
`18446744073709551615` unsigned-exact and `%f`'s six fixed places (`1.500000`,
`-0.333333`). `JNum(x.toString())` is the whole number story, so M4's `f64_str` is
**not** a prerequisite for M2 in either direction.

**The aliasing mechanism already exists, and it is one map.** The note said the
implicit import "has to alias to `@`-prefixed internal names" without saying how.
`resolve_aliases`'s `foreign_renames` is keyed by `(module, name)` and mints ONE
program-wide symbol, which every selective importer and every `ns.member` then
resolves to — so seeding it with reserved spellings for the writer's exports is the
whole mechanism, not a new import form.

The hazard it defends against is now measured with a symbol name rather than
argued. A program that declares its own `fn emit` while a sibling module imports
`std/json`'s emits **`@vyrn_emit` for the user's and `@vyrn_emit__from0` for
`std/json`'s** — the FOREIGN decl is the one that moved. A desugar referencing a
bare `emit` would have called the user's function.

### What M2 still needs, in order

Three mechanisms that do not exist, which is why M2 is still not attempted:

1. **An injected import.** The loader walks imports from the root; a module the
   user never mentioned has to enter that worklist. Plus the `foreign_renames`
   seeding above, including the `Json` enum's variant names.
2. **Synthesized encoder functions in the linked `Program`.** Self-referential types
   make the AST walk non-terminating without them — the AST analogue of the IR's
   `__vyrn_enc_{n}`, which exists for exactly that reason. They are
   declaration-directed, so `link` knows enough to build them; the open question is
   whether generated AST clears the checker, movecheck and dropcheck as written.
3. **The type-directed walk as a shared AST builder.** `schemaOf` is the precedent
   but a weaker one than it looks: it is directed by a type *name*, which the
   frontend has, whereas `toJson(x)` needs the static type of an arbitrary
   expression. The lazy version of the precedent is a frontend
   `json_encode_expr(expr, ty) -> Expr` that all three engines call at the point
   where each already knows `ty` — one definition of the walk, three five-line call
   sites, and no wasm lowering.

`emit_encode` and its `__vyrn_vj_*` calls therefore stay, and the shim stays at 80.
M2's payoff is unchanged at 11 functions and now unblocked on the library side.

### Gates, at this note

1213 workspace tests (was 1212 — `tests/json.rs` now runs both modules, since
`vyrn test` runs one module's blocks and the point of the split is that the writer
links without the reader), parity green three ways over 81 examples, the RFC-0077
ladder unchanged at 39/81, genwasm green, `doc --std --verify` clean with
`docs/api/std/jsonread.md` added.

---

## M2b, as landed: the swap, and the three mechanisms it needed

M2 is **done**. `toJson` renders through `std/json`'s `emit` on the interpreter,
the native backend and both wasm backends, byte-identically, and no engine holds a
JSON encoder any more. All three mechanisms M2a said did not exist now do, and two
of the three are general — the next builtin RFC-0078 moves reuses them unchanged.

### 1. The injected import, and why the reserved spellings are the whole trick

A program whose `program_ref_names` contains `toJson` gets `std/json` visited after
the root walk finishes. Conditional on the mention: injecting unconditionally would
put fifteen functions into every binary in the repo for a builtin most programs
never touch.

Then every declaration of the injected module is renamed to a reserved spelling —
`json$emit`, `json$Json`, `json$JStr` — **unconditionally rather than on
collision**. `$` is not an identifier character (the lexer takes
`is_alphanumeric() || '_'`), so no source can spell one, and that is the entire
defence. It makes two failures unreachable rather than unlikely:

- `link`'s program-wide uniqueness check cannot fire. A program with its own
  `fn emit` would otherwise be "defined in both `main.vyrn` and `std/json.vyrn`" —
  an error naming a module the user never imported and cannot remove.
- the desugar's call cannot be captured. RFC-0022 resolves co-naming by renaming
  the FOREIGN decl, so a generated call to a bare `emit` would have gone to the
  user's function, silently.

**Variants had to be renamed too, and that was not in M2a's plan.** A program whose
own enum has a `JStr` variant is rejected TODAY the moment `std/json` is in the
link — "function `JStr` is defined in `std/json.vyrn` but not imported here",
reported against the module that declared it — so injection would have turned a
legal program into an error about a module it never mentioned. Pass 3 already
rewrote every *reference* (a constructor call and a `match` pattern both go through
the rename map); what it did not touch is the variant list in the decl's own
`Type::Enum`, and a hand importer's variant references, which are references rather
than import names. Both are covered now, the second per importing module so a
module that does not import `std/json` keeps whatever `JObj` means to it.

The test is the collision: one program declaring `emit`, `hex2`, `emitString`, a
type `Json` and a `JStr` variant, calling `toJson`, asserting the user's names win
on every line and the builtin still answers.

### 2. The shared walk generates SOURCE, and that was the right laziness

`vyrn-frontend/src/jsonenc.rs` emits ordinary Vyrn text and hands it to the parser.
Hand-building `Expr` trees is a hundred lines of noise per shape, and the generated
text is inspectable when something goes wrong — which it did, twice, and both times
the text was the diagnosis. The only thing it cannot spell is the injected module's
own names, so the text uses `VyrnRt_` placeholders and one rename pass folds
`VyrnRt_X` into `json$X`. One rule, and it also covers encoding a `Json` value.

Each engine calls `jsonenc::encode_expr(arg, ty, line)` at the point where it
already knows `ty` and lowers the result with its ordinary expression path. Five
lines each. The direct wasm backend's whole `toJson` is:

```rust
"toJson" if args.len() == 1 => {
    let ty = self.peek(&args[0], line)?;
    let e = vyrn_frontend::jsonenc::encode_expr(args[0].clone(), &ty, line);
    return self.expr(m, b, &e);
}
```

### 3. Synthesized encoders, and where they could possibly go

One function per distinct type, memoized on the type, because a self-referential
type — `type Node = { kids: Array<Node> }`, which `toJson` encodes today — makes an
inline AST walk non-terminating. Recursion becomes a call. It fixes evaluation order
for free: the value is a parameter, so `toJson(f())` calls `f` once.

The hard part was not building them; it was finding a place to put them, and this is
the finding M2a was circling. Three stages could conceivably host it and each is
missing exactly one half:

| | knows a `toJson` argument's type | can add a function to the linked program |
|---|---|---|
| the loader | no — it has declarations, not expression types | yes |
| the checker | **yes** | no — it walks `&Program`, and threading `&mut` through 427 KB to rewrite in place is not a refactor, it is a rewrite |
| the engines | yes | no — all three build their function table ONCE, from a `&Program`, with borrowed lifetimes |

So the checker COLLECTS (a `Vec<Type>`, the `StoredFnEffects` precedent) and
`lib::check_and_synthesize` — between check and movecheck, the one point with both
halves — appends. The engines then name the encoder for the type they computed, and
they agree with the checker because the divergence M2a feared is unreachable:
`toJson(Ok(1))` does not compile at all ("cannot infer the type of `Ok(..)`"), so a
`Result` reaching `toJson` always has a real annotated type.

`check_and_synthesize` has two callers, and the second was a bug: a generator
re-loaded as its own root (RFC-0021) skipped the synthesis, so RFC-0076 compiled a
module calling a function nobody emitted. `genwasm` caught it — the tier that exists
to catch it.

### Does generated AST clear the checker, movecheck and dropcheck?

**Yes, with no carve-out anywhere.** This was M2a's open question and M2b's kill
criterion, so it is worth stating precisely: the encoders are appended AFTER the
check and BEFORE movecheck, so the checker never sees them (it produced the types
they are built from) while movecheck, the ownership/drop analysis and all three
lowerings treat them as source they cannot tell apart from the user's.

The property that made it work is one measurement: **passing a record to a function
does not consume it.** An encoder takes its value by parameter, and the caller's
binding survives — `encPoint(p)`, then `p.name`, then `toJson(p)` all hold. Had
parameters moved, the whole shape would have been unbuildable, since `toJson(x)`
must not consume `x`.

Two bugs only running could find, both in the first hour:

- A placeholder written inline instead of through the registrar never reached the
  rename map, so the RETURN type stayed unrenamed, lowered to `void`, and clang
  rejected the caller with `call ptr @vyrn_json$emit({ i64, i64 } )` — an argument
  that had vanished. It hid because the `Array` body happens to register the same
  name, so any program encoding an array looked correct. Pinned by a test asserting
  every encoder returns the tree type.
- The generator path above.

### The bytes: exactly the two rows M2 predicted

`examples/jsonbytes.vyrn` pinned the whole surface BEFORE the swap, against the C.
Two rows moved: `0x08` and `0x0c`, the long `\u0008`/`\u000c` becoming the short
`\b`/`\f`. `std/json` won them because its `emit` has committed consumers
(`std/tw`, `std/i18n`, `std/openapi` and their golden outputs) and the long form had
no caller in the repo that could observe it. Everything else — 32 control bytes,
raw multi-byte UTF-8, every sized integer at its bounds, `Float64`/`Float32`, `%f`'s
six fixed places, declaration field order, the `None`-omission — is byte-identical,
which is what pinning first buys: the diff is a statement about which writer won.

### RFC-0077's ladder: 39/81 -> 43/81, and the two rows that were not `toJson`

`domdemo`, `htmltree`, `patchdemo`, `jsonbytes`. Two things were needed and neither
is `toJson` lowering:

- `peek` could not type a `match` arm yielding a user enum variant. That is a
  GENERAL gap — any such arm failed on the direct backend — and the fix is five
  lines against the variant table, refusing an ambiguous name rather than guessing.
- the generated code first spelled the `None`-field omission as `if let`, which is
  one of RFC-0077's own unlowered rows, and would have put every example with an
  `Option` field behind it. `push` returns the array, so
  `fs = match v.f { Some(x) => fs.push(..), None => fs }` does the same job and asks
  for nothing.

### Corrections to the arithmetic above

**The shim retires SIX functions, not eleven.** M2's note attributed the whole
growable buffer to the encoder; the PARSER's string unescaper shares it, so
`vsb_init`/`_ensure`/`_putc`/`_puts` stay, and `__vyrn_vj_num_text` stays because
the parser builds nodes with it. What went is `vsb_escape`, `__vyrn_vj_write`,
`__vyrn_vj_encode` and the three number constructors — the exported boundary from
74 to 70.

**Eleven `declare` lines went, though**, because the DOM BUILDERS are now
unreferenced by anything either emitter writes; they survive inside the shim only
as the C parser's internals. With them went the boundary's only sub-i32 argument
(`declare ptr @__vyrn_vj_bool(i1)`, M0's named widening), so `imports_vs_shim`'s
`i1` case becomes an assertion that none remains, and `shim_link`'s live link proof
reads its bytes back through `__vyrn_charcount` instead of through the shim's JSON
writer. The per-enum variant-name table went too: it existed so a nullary enum could
read its name in O(1) from IR, and had no other reader.

Deleted, in Rust: `encode_val`/`encode_variant` (142 lines) from the interpreter,
`emit_encode` and its three friends (362 lines) from the textual emitter. The
interpreter's arm is **gone rather than kept as a cache** — M5's question does not
arise for `toJson`.

### The one new seam, stated plainly

`toJson` now needs a std root, because its serializer lives there. Every CLI command
has one. `vyrn_frontend::run(source)` / `check(source)` — a single source with no
resolver — does not, so five interpreter tests moved to the loader path with the
real `std/json` text, and two codegen IR tests stopped calling a builtin they were
only using to reach the IR they pin. Codegen refuses loudly (naming the type and the
reason) rather than emitting a call to a function that is not there.

Known limits, both pre-existing in effect: an ANONYMOUS enum has no source spelling
(`Display` renders `enum { A | B }`), so it gets no encoder — and `Map` bodies use
`v.keys()` and `v[k]`, neither of which the direct backend lowers yet, so a Map
example stays blocked there for RFC-0077's reasons rather than this RFC's.

### Gates, at this note

1215 workspace tests (was 1213), parity green three ways over 81 examples including
the two repinned rows, the RFC-0077 ladder at **43/81** (was 39), genwasm green,
`doc --std --verify` clean. The shim is at 70 exported functions.

**M3 (`fromJson`) inherits all three mechanisms.** The injected import and the
shared-walk seam are general; what M3 adds is the reader (`std/jsonread`, which
still wants `?` and `if let`) and RFC-0018's `Issue` accumulation. The remaining 38
`__vyrn_vj_*` DOM functions fall with it. *(That sentence is optimistic on both
counts — see M3's note.)*

---

## M3, as measured: the bytes are pinned, and the swap is blocked on a primitive

**M3 did not land, and should not be attempted in M2b's shape.** What landed is the
prerequisite — the decode surface pinned as a test, weighted towards the failure
shapes — plus one dead C function and the four measurements below. Every one was
taken by *running*, not by reading, and two of them contradict something this
document already asserts.

### The pin

`examples/jsondecbytes.vyrn` is `jsonbytes.vyrn`'s mirror for the other direction:
it decodes into a refined scalar, a nested record with an array and an `Option`, a
nullary enum, a payload enum and every decodable scalar; prints from `main` so
parity covers interp == native == wasm; and pins ten rows as literals in `test`
blocks, wired into `tests/json.rs` so they are not decoration.

It is deliberately weighted towards **failures**, because that is where two readers
differ and nothing else in the suite looks there. The load-bearing pin is the ORDER
issues accumulate in: three failures across two array elements, reported in the
element-then-declaration walk (`kids[1]`'s missing `name` before its out-of-range
`age`) rather than in order of discovery. A reader reporting the same set in another
sequence is not a drop-in replacement, and only a pin written *before* the swap can
say so.

### 1. `Float64` has no decode expression in Vyrn — and that is a missing PRIMITIVE

M2a measured the encode direction free: `toString()` already produced the shim's
bytes, so numbers needed nothing and M4 was not a prerequisite. This document then
warned to "check rather than assume the symmetry holds" for parsing. It does not
hold, and not by a little:

- `Float64("1.5")` is a **check error** — "`Float64(..)` converts a number, found
  String". There is no numeric conversion from `String` in the language.
- `parse` is `String -> Option<Int64>` and nothing else.

So a decoder expressed in Vyrn cannot turn `JNum`'s raw text into a `Float64` or a
`Float32` at all. No example decodes a float today, but `codec::decodable` admits
them and all three engines implement them, so dropping the row is a language
regression — and keeping the C path for floats alone is precisely the half-Vyrn,
half-C builtin this document names as worse than either end state.

**M4 does not fix this either.** M4's list is `chars`, `parse`, `f64_str`,
`stringFromBytes` — the direction *out* of numbers. Text -> `f64` is not on it, and
it is not the ~40 lines of Vyrn the `f64_str` row estimates: correct decimal-to-binary
rounding is the hard direction, and the shim gets it for free by calling libc.
Either the primitive set (M1) grows an entry, or someone writes an
arbitrary-precision conversion in Vyrn and proves it byte-identical to `strtod`.
**That is the sequencing finding: M3 is blocked on M1, not on M4.**

The same shape in miniature: an exact `UInt64` above `Int64::MAX`
(`18446744073709551615`, which both JSON pins carry) cannot come back through
`parse`'s `Option<Int64>`. That one IS hand-rollable with wrapping arithmetic and
explicit overflow detection; it is work, not a wall.

### 2. A refined type cannot hold a value that failed its own predicate

RFC-0018 decode does not stop at the first problem — it accumulates. So a decoder
must carry *some* value for a field that failed and keep walking. The IR does this
with a zeroed slot, which no Vyrn program can spell:

```
fn bad() -> Age { let mut n = 0; return n }   // error: validation failed for `Age`
```

Measured, at runtime, on the interpreter. Automatic validation at every value
boundary (the property that makes refinements trustworthy) is exactly what makes the
accumulating decoder inexpressible as written.

There is a design that works — every decoder returns `Option<T>`, pushes its own
issues, and a composite constructs only when all its parts are `Some` — and it
preserves issue order for free, since the field walk is unchanged. But it is a
DIFFERENT shape from M2b's encoder walk, not a mirror of it, and it has to be stated
before it is built. M2b's "the next builtin reuses these mechanisms unchanged" holds
for the injected import and the source-generating walk; it does not hold for the
walk's *shape*.

### 3. The two readers disagree about which documents parse, in opposite directions

M2's swap moved two rows and both were cosmetic — `\b` versus `\u0008`, the same
string either way. These are not. Pinned in the example as today's answers:

| input | `fromJson` today | `std/jsonread` |
|---|---|---|
| `"😀"` (surrogate pair) | **rejected** — `unexpected character at position 11` | **accepted**, decodes 😀 (its own test asserts it) |
| duplicate key `{"s":"a","s":"b"}` | **accepted**, first wins | **rejected**, naming the key |
| every parse error | `<reason> at position N` (0-based byte) | `line N, col M: <reason>` (1-based) |

`codec::parse` decodes each `\uXXXX` independently through a `char`, so a surrogate
half is not representable and the pair fails; `std/jsonread` pairs them. In the other
direction `codec::parse` keeps duplicate members and `get` takes the first, while
`std/jsonread` rejects. So the swap does not just re-word `json.parse`: it changes
which documents decode at all, one direction of which turns working programs into
failures. That is a semantic ruling this RFC has to make explicitly, and it wants
the repo's stored JSON (`examples/bin/data/`) checked against it first.

### 4. One dead C function, deleted

M2b's note says `vsb_init`/`_ensure`/`_putc`/`_puts` all stay because the parser's
string unescaper shares the buffer. Three do. `vsb_puts` appends a whole string,
which only the deleted serializer did — the unescaper goes one byte at a time
through `vsb_putc` — so it has had no caller since M2b and is deleted here.

### The count, with the method stated, because this document has been wrong twice

Counting C function definitions in `RUNTIME_SHIM` by regex, treating `static` as
internal and everything else as the exported boundary:

| | |
|---|---|
| C function definitions in the shim | 95 (66 exported, 29 static) |
| of those, JSON | **32** — 27 exported, 5 static |
| of the 27, still called by an emitter | **19** |

So **M3 would retire 32 C functions, not 38**, and the 38 above is the third estimate
this document has gotten wrong. The other eight exported JSON functions
(`vj_arr`, `vj_bool`, `vj_null`, `vj_obj`, `vj_push`, `vj_set`, `vj_str`,
`vj_kindname`) have no emitter caller since M2b but are still non-static, so they sit
on the boundary doing nothing — they can be made `static` today, independently of M3.

**Deleted in this milestone: one** (`vsb_puts`). Not 32, and not zero.

Note also that 66 exported is not the 70 the M2b note reports; the difference is
method, not drift (this count excludes `static` definitions whose names begin with
`__vyrn_`, of which the JSON section alone has two). Whoever counts next should say
how.

### What M3 should be, when it is taken

In order, and none of it is the shape M2b's success suggests:

1. **A text -> number primitive**, named in M1 alongside memory growth and the
   syscalls. Without it no decoder can be written in Vyrn at all.
2. **The ruling on strictness**, written down: surrogate pairs, duplicate keys, and
   the `json.parse` wording, with the repo's stored JSON checked against it.
3. **The `Option<T>`-returning decoder shape**, replacing the mirror-of-M2b
   assumption.

Until (1) exists, the honest position is that `fromJson` moves *after* M4 rather than
before it, and M4's own list is short by one entry.

### Gates, at this note

1216 workspace tests (was 1215 — the new pin's row in `tests/json.rs`), parity green
three ways over 82 examples including the new one, the RFC-0077 ladder unchanged at
45/82 (`jsondecbytes` does not build on the direct backend, because `fromJson` is one
of the rows RFC-0077 has not lowered and this milestone did not remove), genwasm
green, `doc --std --verify` clean. The shim's exported boundary is **unchanged** —
`vsb_puts` was `static`, so deleting it moves the total (96 -> 95) and not the
boundary. A milestone that retires no builtin should not move that number, and this
one did not.

---

## M4a, as landed: text -> number is a library, and the primitive is a bitcast

M3 stopped because a decoder written in Vyrn cannot turn `JNum`'s raw text into a
`Float64` — there is no expression in the language that does text -> float. M4a
makes that expression exist, and the decision it turns on is the one this
milestone was created to make.

### The decision: a library, and the primitive is not the conversion

**Text -> number is a LIBRARY.** `std/num` is 250 lines of ordinary Vyrn, and no
engine implements any part of it. What went into the compiler is smaller and of a
different kind: **`floatBits(Float64) -> UInt64` and
`floatFromBits(UInt64) -> Float64`**, the two IEEE-754 bit views. They are
`f64::to_bits` in the interpreter, `bitcast double to i64` in the textual emitter
and `i64.reinterpret_f64` in the direct wasm backend — one instruction each, and
on the direct backend not even that, since the value stack already holds the bits.

The argument for "primitive" was that the shim gets `strtod` from libc for free.
It does — and the direct wasm backend does not, which is the whole point. A
`parseFloat` builtin would have to grow a correctly-rounded decimal-to-binary
conversion in hand-emitted `wasm-encoder` calls, next to the ~300 lines M2h
already wrote for the other direction, and then that conversion would exist three
times. Two reinterpretation instructions buy the way out of writing it even once
in wasm.

The general form of the finding, which is worth more than the decision: **the
irreducible primitive was not the operation, it was the missing VIEW.** Nothing in
the language could construct a `Float64` from anything except another number, so
every text -> float route had to be a builtin. Give Vyrn the bits and the
operation stops needing to be primitive at all — in either direction.

### The primitive list, which is M1 discharged for numbers

RFC-0077 M2j measured a directly-emitted module's import list as **twelve WASI
functions plus `__vyrn_malloc`**. M3 proved that list incomplete. It now reads:

| kind | primitives |
|---|---|
| memory | `__vyrn_malloc` |
| syscalls | the twelve WASI imports (RFC-0077 M2j) |
| representation | `floatBits`, `floatFromBits` |

Three kinds, and the third is new as a *category*: a primitive that performs no
work and calls nothing, existing only because a value has two readings and the
language could name one of them. M4b's `chars` and `stringFromBytes` should be
checked against the same question before they are assumed to need a builtin —
`bytes` already exists, so the string half may already have its view.

### Correctly rounded, and how that is known rather than claimed

The conversion is exact, not floating point. The decimal stays a digit array and
is scaled by powers of two until it lies in `[1/2, 1)`; from there repeated
doubling hands over one mantissa bit at a time and the leftover fraction decides
the rounding — above a half up, below down, exactly a half to even, and a
truncated tail turns a tie into a round-up because the true value is then strictly
greater. Subnormals are not a special case but a smaller bit budget; overflow is
an exponent field that ran past its maximum. The only `Float64` in the file is the
one assembled from bits at the end. It is M2h's `%f` algorithm run backwards,
which is why "library" was the right answer rather than a hopeful one.

"Correctly rounded" is checked against an oracle rather than asserted.
`tests/numbers.rs` compares `parseFloat64` against Rust's own
`str::parse::<f64>()` **bit for bit over 302 inputs**: the exact ties at `2^53`,
the two literals that hung PHP and Java, both ends of the subnormal range and half
the smallest one, the largest finite double and the first value past it, a
900-digit significand that exercises the truncation flag, sixteen refusals, and
220 deterministic pseudorandom decimals. All 302 agree. Flipping ties-to-even to
ties-to-odd breaks three of them, so the oracle bites rather than nodding.

The naive version scaled one bit at a time and took 0.8 s to parse `1e308` on the
interpreter. Scaling a whole 32 bits per pass — the remainder stays under `2^32`,
so `r * 10 + digit` still fits an `Int64` — took the module's suite from 2.9 s to
0.44 s. Worth recording because it is the same trade M2h made in the other
direction and reached the opposite way: M2h used base 10^6 limbs to make the
scaling one loop, this uses base 10 digits and scales 32 bits at a time.

### Pinning first found a bug in a third engine, again

`examples/numbytes.vyrn` walks every numeric conversion the language has and pins
twenty-eight rows as literals BEFORE anything moved. Three rows would not have
been guessed and one of them was a defect:

- `parse` **wraps** on overflow rather than declining, so
  `parse("18446744073709551615")` is `Some(-1)`. That is the smaller half of what
  M3 measured as blocking, and `std/num`'s `parseInt64`/`parseUInt64` are the
  answer to it — they refuse, exactly.
- `9.9999995` looks like a tie and is not one, so it is `9.999999`. `UInt8(300.7)`
  is `44` and not `255`, because the float saturates into 64 bits and the
  narrowing then wraps.
- **Float -> integer was POISON on the native backend.** `Int64(10^300)` printed
  `Int64.min` natively against `Int64.max` under the interpreter; `UInt64(-1.5)`
  printed `UInt64.max` against `0`; `Int64(NaN)` printed `Int64.min` against `0`.
  A bare `fptosi`/`fptoui` is undefined out of range and `convert_val`'s own
  comment said "unspecified (as in C/LLVM)" — but the interpreter is Rust's `as`,
  which saturates, and M2h had already made the direct wasm backend match the
  interpreter. Native was alone, and no example had ever converted an
  out-of-range float. Fixed with `llvm.fptosi.sat`/`llvm.fptoui.sat` into 64 bits
  and a wrapping `trunc`, which is the interpreter's two steps one for one.

That is the fourth milestone in a row where pinning the boring rows first found
something only running could catch, and the second where the defect was in the
engine nobody suspected.

### One general backend bug, found by putting a float somewhere new

`Option<Float64>` classified its payload as `Word::Ext` — the narrow-INTEGER case
— and emitted `i64.extend_i32_u` against an `f64` on the stack. The direct backend
produced a module **wasmtime refused to load**, rather than the "unsupported"
diagnostic it produces for everything it cannot lower. Nothing in the corpus had
ever put a float in a sum payload. Fixed generally for `Option` and `Result`,
`Float64` and `Float32`, by reinterpreting the bits into the word.

### `f64_str` was not retired, and this is the measurement rather than a deferral

The brief allowed retiring it if it fell out. It does not, and the reason is not
difficulty:

- `float_str` is **511 lines** of `direct.rs` including its doc, with **two** call
  sites: `print` of a float and `.toString()` on one.
- It is one of THREE implementations. The interpreter formats with `{:.6}` and the
  native build selects a format string around `printf("%f")`. RFC-0078's own rule
  is whole builtin or none, so retiring `f64_str` means retiring all three.
- Which makes float printing a call into a linked Vyrn module, through M2b's
  injected-import mechanism, on every program that prints a float — a far larger
  blast radius than `toJson`, which most programs never mention.

So it is a milestone, not a corollary, and it needs its own pin of `print` before
it is attempted. What M4a leaves behind is that the milestone is now *possible*:
`floatBits` is exactly the primitive a Vyrn `%f` needs, and it exists on every
engine. M2h's pinned cases all still pass, unchanged —
`six_decimals_of_a_float_are_the_exact_ones` is green, and
`examples/numbytes.vyrn` pins the same values a second time under the interpreter
and the native build.

`parse` itself also stays a builtin, with its two implementations, and that is a
decision rather than an oversight: `std/num`'s integer parsers are not a second
definition of it because they behave observably differently (refusing where
`parse` wraps), so folding `parse` into `std/num` is a language change — the
overflow semantics of every existing caller — and not a mechanical move. It is
named here so the next milestone does not have to rediscover it.

### The count, with the method stated, because this document has now been wrong four times

Counting C function definitions in `RUNTIME_SHIM`, **including one-line
definitions** (`double __vyrn_vj_asfloat(VJ* v) { return strtod(v->text, 0); }` is
a definition and the obvious regex misses it), treating `static` as internal and
everything else as the exported boundary:

| | |
|---|---|
| C function definitions in the shim | 95 (66 exported, 29 static) |
| deleted in this milestone | **0** |

Unchanged, and it should be: M4a moved no builtin out of C. It added a capability
that had no implementation anywhere, and the two primitives it did add are in the
emitters rather than in the shim — `bitcast` and `i64.reinterpret_f64` need no C
at all. A milestone that retires no builtin must not move that number, and the
honest report is a zero rather than a smaller number reached by counting
differently. (The method above is stated precisely enough to reproduce: it yields
M3's 95 / 66 / 29 exactly, which is how it was checked.)

### Gates, at this note

1219 workspace tests (was 1216 — `std/num`'s unit blocks, the numeric pins and the
differential test against Rust's parser), parity green three ways over 84 examples
including both new ones, the RFC-0077 ladder at **46/84** (was 45/82 — `numparse`
passes, `numbytes` does not, because it exercises the `parse` builtin this
milestone deliberately did not move), genwasm green, `doc --std --verify` clean
with `docs/api/std/num.md` added.

**M3 is unblocked.** What it now needs is the two items its own note listed after
the primitive: the ruling on strictness (surrogate pairs, duplicate keys, the
`json.parse` wording), and the `Option<T>`-returning decoder shape. Neither is a
missing primitive, and `std/jsonread` can `import { parseFloat64 } from "std/num"`
today.

---

## M4b(3), as landed: the predicates were already writable, and `slice` needs an abort

`std/strpred` is `contains`, `startsWith`, `endsWith`, `slice` and `byteLength` —
five builtins, three implementations each — written as 140 lines of ordinary Vyrn
on nothing but the byte view. Nothing is swapped: the builtins still exist and are
still what user code calls, and what landed is the equivalence proof, which is the
order M2 established (pin first, or the test describes whatever came out).

### M4a's question, asked of the string half, answers itself

M4a's note said to check `chars` and `stringFromBytes` against "is the missing
piece an operation or a VIEW?" before assuming a builtin, and observed that
`bytes` may already be the string half's view. For these five it is, and there was
nothing left to add: `bytes(s) -> Array<UInt8>`, `s[i] -> UInt8` and
`stringFromBytes(b) -> Result<String, String>` are the whole substrate. **Zero new
primitives.** That is the cheapest milestone in this RFC so far, and it is cheap
because M4a already paid for the category.

The predicates in particular were nearly written already: `std/strings`'s
`indexOf` / `lastIndexOf` (RFC-0046) are the same byte-wise scan returning the
offset, so `contains` has effectively existed in Vyrn since RFC-0046 and nobody
noticed the builtin was redundant. `std/strpred` spells the loop out rather than
importing `indexOf`, for one reason worth recording: `indexOf` reaches
`s.byteLength`, which is one of the five builtins under replacement, so building
on it would have produced exactly the half-Vyrn/half-builtin seam this document
names as worse than either end state. The module imports nothing.

### The UTF-8 hazard in a byte-wise search is unreachable, not handled

The brief asked for "a needle that matches only at a non-boundary byte offset
inside a multi-byte codepoint". **No such input exists, and the reason is a
proof rather than a test.** UTF-8 is self-synchronizing: a valid needle's first
byte is ASCII or a lead byte, never a continuation byte, and every non-boundary
offset in a valid haystack holds a continuation byte. So a byte-wise scan over two
valid `String`s cannot report a match inside a character — the case does not need
a check. Nor can it be *written*: a needle consisting of a bare continuation byte
is not constructible, since there is no such string literal and `stringFromBytes`
refuses the bytes. The nearest thing a `String` can be is `"©"` (C2 A9) against
`"é"` (C3 A9) — the same trailing byte, a different character — and it is pinned,
`false` both ways.

### `slice`'s two traps, and the one primitive that is genuinely missing

Reproduced verbatim from the single source
(`interp.rs`, `@.trap.slice*` in `codegen/lib.rs`, `direct.rs`'s interned strings),
both rendered by the CLI as `error: <msg>` with exit code 1:

| condition | message |
|---|---|
| `start < 0`, `end > byteLength`, or `start > end` | `error: slice index out of range` |
| either cut point inside a multi-byte character | `error: slice splits a UTF-8 character` |

The range is checked **first** on every engine, so a mid-character offset combined
with an out-of-range one reports out-of-range; a negative `end` is caught by
`start > end` rather than by a lower bound. Both are pinned.

**A mid-codepoint boundary traps — it does not produce invalid UTF-8.** That was
the brief's open question and it had to be run to answer: `slice` refuses the
range outright rather than handing back a truncated character.

Which is where the one real gap is. **Vyrn has no expression that aborts with a
message** — no `panic`, no `abort` — so a Vyrn `slice` cannot be
observationally equal to the builtin. `sliceV` returns `Option<String>` and `None`
means "the builtin would trap here". Asked in M4a's terms, this gap is neither an
operation nor a view: it is a *control* primitive, and it is irreducible in the
same way memory growth is (every backend already has one — `exit`, `unreachable`).
It also belongs on M1's list, which currently reads memory + syscalls +
representation and has no entry for aborting. The `Option` is the honest shape for
a partial function either way — it is the house idiom for the rest of `std/` — so a
trapping wrapper is one line on top of `sliceV` the day the primitive exists, and
M4b(3) does not need it.

The boundary check is not written out, and that is the pleasing part: a cut at a
non-boundary offset either starts the range on a continuation byte or ends it on a
truncated character, both of which are invalid UTF-8, so `stringFromBytes` refuses
exactly the ranges `is_char_boundary` refuses. `sliceV` gets the second trap's
condition for free from the view.

### One asymmetry in the byte view, worth naming before something trips on it

`bytes` and `stringFromBytes` are **not** a round trip. `stringFromBytes` rejects a
NUL byte (RFC-0014's rule) and `slice` does not, so `sliceV` of a NUL-containing
`String` is `None` where `slice` returns the substring. It is unreachable from
ordinary source — there is no `\0` escape (`unknown escape \0`), and
`stringFromBytes` will not build such a string either — but the lexer does accept a
raw NUL byte in a literal, so the divergence is reachable by a file no one would
write rather than by no file at all. Any future *swap* of `slice` has to answer it;
the proof only has to state it.

### The proof shape: the builtin is the oracle, in the same process

`examples/strpredbytes.vyrn` calls each Vyrn version beside the builtin it would
replace and prints both answers on one line, so a disagreement is a visible diff
rather than a silent `false`: 21 predicate rows (empty needle, needle longer than
haystack, needle == haystack, overlapping `"aaa"` in `"aaaa"`, multi-byte needles at
2/3/4 bytes at every position, the C2-A9/C3-A9 near-miss), ten legal slices
(`start == end` at both ends, the whole string, exact character boundaries in 2-,
3- and 4-byte characters, the empty string), eight trapping ranges through `sliceV`,
and `byteLength` over empty/ASCII/multi-byte plus a string built by
`stringFromBytes` rather than from a literal. Printing from `main` puts all of it
through parity, so the Vyrn versions are checked interp == native == wasm as well as
against the builtins; five `test` blocks assert the agreement.

`tests/strpred.rs` covers the one thing a single program cannot, since a trap ends
the process: one program per trapping range, printing `sliceV`'s answer and *then*
calling the builtin, so a single run pins both halves of the pairing — stdout
`None`, stderr the canonical message, exit 1 — over ten ranges across both traps.

**No disagreement was found anywhere.** Every predicate row, every legal slice and
every byte length matched on the first run, which is worth stating because it is the
first milestone in this arc where pinning the boring rows first did *not* uncover a
defect in some third engine. The five builtins agree with their Vyrn definitions
exactly, so a swap would be observable only in `slice`'s trap — and only because
Vyrn cannot trap.

### Gates, at this note

1222 workspace tests from this milestone's three rows (the suite reported 1227 with
two sibling M4b modules present in the tree), `strpredbytes.vyrn` green three ways
in parity, `doc --std --verify` clean with `docs/api/std/strpred.md` added. The
shim is unchanged at 95 C definitions / 66 exported, and it should be: M4b(3)
retires no builtin, it proves five of them redundant.

---

## M4b(1), as landed: the codecs needed nothing, and the builtin they answer to is wrong

`hexEncode`, `hexDecode`, `base64Encode`, `base64Decode`, `urlEncode` and
`urlDecode` are now written in Vyrn, in `std/codecs` (350 lines), and proved equal
to the builtins over 6,354 comparisons. Nothing is swapped: the builtins still
exist, the Vyrn versions carry a `V` suffix, and the equivalence is a committed
test rather than a claim made after a deletion.

### The decision: no primitive, no ruling, no mechanism

M4a needed two primitives (`floatBits` / `floatFromBits`). M3 needs a semantic
ruling before it can move. These six needed **neither**, and that is the finding
worth the milestone: `bytes(s)`, `stringFromBytes(..)`, RFC-0045's bitwise
operators and a `while` loop are the entire toolkit. Every one of the six was
written first-try and its hand-picked pins passed on the first run — the only
milestone here so far where the language was already sufficient.

They are also the cheapest possible test of M2b's injection mechanism when the
swap is taken, because they need **no type-directed part at all**. `toJson` needed
a shared AST walk and synthesized per-type encoders because it reads a value's
static type; `hexEncode(s)` is `String -> String`. The compiler part of the swap
is an injected import and a renamed call, which M2b already built and which
nothing else has to be written for.

Measured rather than assumed, the way M2a measured the writer split: a program
whose only import is `std/codecs` compiles under `VYRN_WASM_BACKEND=direct` and
wasmtime prints what the interpreter prints. The module imports nothing, so its
layering is trivially clean. Meanwhile the direct backend refuses
`examples/codecbytes.vyrn` at `hexEncode` — "no lowering for the call" — which is
the RFC-0077 relationship in one line: six unlowered builtin rows, or a library
that already runs.

### The proof shape: the builtin as oracle, before any deletion

`tests/codecs.rs` generates one Vyrn program that calls **both** implementations on
every input in a corpus and prints a line only where they disagree — 282 encoder
inputs and 1,554 decoder inputs, 6,354 comparisons. The corpus is the surface where
two codecs can differ: every byte a `String` can hold, all three base64 padding
residues, every alphabet digit, every printable ASCII byte in each of a group's
four positions, `=X` and padding before the final group, odd hex lengths, non-hex
digits, both hex cases, truncated and non-hex percent escapes, every `%XX` in both
cases, the unreserved set, and decoded bytes that are not UTF-8.

Two details are deliberate. Payloads are rendered through the *builtin* `hexEncode`
so a mismatch line is ASCII and the test's own reporting does not depend on the
code under test. And the expected divergence is recognised **by rule** rather than
by an enumerated allow-list, so a new divergence cannot hide inside a stale list;
the test also asserts the class is non-empty, so the rule is exercised rather than
vacuous. Mutation-checked: making `urlEncode`'s hex lowercase produces 86
disagreements, so the harness bites.

Because the comparison is written as "both, on the same input", it does not have to
be rewritten when the swap lands. With the builtins gone it becomes the regression
pin for what replaced them.

### The finding: decoding a NUL is a latent parity bug in the builtin

One class of disagreement, and it is the builtin's. A decoder whose bytes contain
`0x00` — `hexDecode("00")`, `base64Decode("AA==")`, `urlDecode("%00")` — answers
`Some` today, and **answers it differently on each engine**:

| engine | `hexDecode("00")` |
|---|---|
| interpreter | `Some` of a Rust `String` holding the NUL byte |
| native | `Some` of a `char*` that `__vyrn_strlen` truncates at that byte |

The IR decoders write a NUL terminator and return a pointer; the interpreter keeps
a length-carrying `String`. No example decodes a NUL, so parity has never looked at
the row, which is the fifth milestone in a row where writing the boring pins first
found something only running could catch. `std/codecs` returns `None`, because
RFC-0014 forbids a NUL inside a `String` and `stringFromBytes` enforces it — and
`None` is the answer that is the same on all three engines. **So the swap is a bug
fix, not just a deletion**, and it is pinned as a divergence in
`std/codecs.vyrn`'s own `test` block rather than papered over.

A smaller thing the corpus settled: "every byte 0..255" is not reachable through an
encoder at all. `0x00` is forbidden, and `0xC0`, `0xC1` and `0xF5`..`0xFF` cannot
appear in valid UTF-8 — so the full byte range is only an input to the *decoders*,
where the corpus does hit all 256.

### What the swap will retire, counted

Nothing in C, and that is the point: these six are the only builtins with **no C
shim implementation at all**. What they have instead is ~494 lines of hand-written
LLVM IR in `vyrn-codegen` (six codec functions plus `__vyrn_hexdigit`,
`__vyrn_hexdigit_uc` and `__vyrn_hexval`; `__vyrn_utf8valid` stays, since
`stringFromBytes` and `chars` call it too) and 159 lines of Rust in `interp.rs`.
653 lines of duplication against 350 lines of Vyrn, and a third implementation the
direct backend would otherwise owe.

Counting the shim by M4a's stated method — C function definitions in
`RUNTIME_SHIM`, `static` treated as internal:

| | |
|---|---|
| C function definitions in the shim | 95 (66 exported, 29 static) |
| deleted in this milestone | **0** |

Unchanged, and it must be: M4b(1) moved no builtin. A milestone that retires none
reports a zero.

### Gates, at this note

`std/codecs`'s four `test` blocks, the example's rows on the interpreter and the
6,354-comparison oracle are three new workspace rows; the suite reported 1230
passed / 0 failed with all three sibling M4b modules present in the tree.
`examples/codecbytes.vyrn` is green three ways in parity (interp == native ==
wasm). `doc --std --verify` clean with `docs/api/std/codecs.md` added. The
RFC-0077 ladder is untouched — no builtin moved, and the example calls the ones
the direct backend has not lowered.

---

## M4b(2), as landed: `chars`, `lineAt` and `colAt`, and the view was already there

The three text builtins are written as ordinary Vyrn in `std/text` —
`decodeUtf8`/`charsV` for the codepoints, `lineAtV`/`colAtV` for the 1-based line
and column of a byte offset. Nothing is retired, deliberately: the builtins are
what the Vyrn versions are proved *against*, and equivalence has to exist before a
deletion can be safe. When the swap lands, `tests/text.rs` becomes the regression
pin without being rewritten.

### M4a's question, asked again, and answered the other way

M4a's general finding was that the irreducible primitive is a missing **view**
rather than a missing operation: nothing in the language could read a `Float64`'s
bits, so every text -> float route had to be a builtin, and two reinterpretation
instructions removed the need to write the conversion at all. M4a then asked that
`chars` and `stringFromBytes` be checked against the same question before a
builtin was assumed.

**The string half needs no new view, and it already had the right one.** `bytes(s)`
exposes a `String` as its UTF-8 bytes and `stringFromBytes` is the validated
inverse, so `std/text` needed **no compiler change of any kind** — not one line in
the checker, the interpreter, either emitter or the shim. That is the cleanest form
of the RFC's thesis so far: `std/num` cost two primitives, this cost zero.

The line/column pair is a different case and worth separating. Those builtins do
not exist because the operation was inexpressible — the loop is four lines of Vyrn
— but because it is O(offset) and a scanner asks once per node, which cost
`std/vyx` 122 ms of a 291 ms page compile. The interpreter memoizes a line-start
table per buffer; **the native shim does not, and counts exactly as the Vyrn
version does.** So the Vyrn implementation is not slower than every engine, it is
slower than one of them and identical to the other, and retiring the builtin would
be a decision about the interpreter's cache rather than about capability. That is
M5's question arriving early.

### The oracle, and where it actually bites

`tests/text.rs` runs the builtin as the oracle over three corpora, all of them
generated so the Rust side never reimplements UTF-8 a fifth time — it asserts on
`ok` lines from a Vyrn comparator.

- **Valid codepoints:** every scalar below U+0800 exhaustively (both one- and
  two-byte forms, byte for byte), sampled through the BMP and the astral planes,
  plus each encoding boundary spelled out so no step size can skip one; then the
  same codepoints in multi-codepoint buffers, since a decoder that resynchronizes
  wrongly only shows up on a sequence. `decodeUtf8`, `charsV` and `chars` must all
  three agree per row.
- **Malformed bytes — the half that matters:** ~1,400 buffers where the only
  observable is accept/reject, because a `String` cannot hold invalid UTF-8 and so
  `chars` never sees a bad byte. Every lead byte `0xC0..0xFF` against ten
  continuation bytes chosen to straddle each boundary the encoding cares about
  (0x7F/0x80, 0x8F/0x90, 0x9F/0xA0, 0xBF/0xC0) at widths two, three and four; the
  surrogate range encoded as if it were a scalar; overlong forms of the same value
  at every width; every proper prefix of a valid sequence, and each prefix followed
  by valid text; and the five-byte forms UTF-8 originally allowed.
- **Line and column:** every offset from -3 to `len + 3` of twelve buffers,
  including CRLF, an empty buffer, nothing but newlines, and multi-byte text.
  Every offset rather than a chosen few precisely because the two engines compute
  the answer differently — a binary search over a memoized line-start table
  against a backward walk to the previous LF — so a third implementation that
  agrees at offset 0 and disagrees at the byte after the last newline is the
  failure mode. The clamping past the end and below zero is behaviour nothing else
  in the suite pinned.

The corpus proves it can fail rather than only that it passes: widening 0xED's
first-continuation bound from 0x9F to 0xBF (the surrogate check, the single
easiest thing to get wrong) fails three of the five rows, naming the exact byte
strings.

### A column counts BYTES, and the wrapper's doc says otherwise

Measured off the builtin rather than assumed, because it is not obvious and both
answers are defensible. The interpreter computes `off - lineStart + 1` and the shim
walks bytes backwards, so both count **bytes**: the `x` in `éx` is column 3, not
column 2. That is asserted on its own line in `tests/text.rs` rather than left
implied by 400 green rows.

`std/vyx`'s wrapper (`std/vyx.vyrn:165`) documents `colAt` as "chars since the last
LF", which is wrong for any line containing non-ASCII text. Nothing is broken by it
— RFC-0033 origin directives feed a C-style `#line`, where byte columns are the
convention — but the comment is a wrong statement about a shared builtin and should
be corrected when `std/vyx` is next touched.

### The one disagreement, and it is not about UTF-8

`decodeUtf8` accepts `0x00` and returns codepoint 0; `stringFromBytes` refuses the
buffer with `bytes contain a NUL byte`. That is RFC-0014's rule — a Vyrn `String` is
NUL-terminated, so it could not carry one — and not a decoding question, so NUL is
excluded from the cross product and pinned as its own row in both modules' `test`
blocks. Stating it explicitly is the point: an implementation that "agreed with the
oracle everywhere" would have had to be wrong about UTF-8 to do so.

### One general native bug, found by putting an array literal somewhere new

`examples/textbytes.vyrn` would not build natively, and the reason is nothing to do
with text:

```
error: '%t1' defined with type '[2 x i64]' but expected '{ ptr, i64, i64 }'
  store { ptr, i64, i64 } %t1, ptr %b.addr2
```

**An array literal whose element type is a sized integer lowers in the textual
emitter as a fixed-size `[n x i64]` aggregate, and is then stored into — or passed
as — the `{ ptr, i64, i64 }` heap-array triple.** clang refuses the module.
Reproduced at two words: `let b: Array<UInt8> = [65, 66]` fails, and so does
`let b: Array<Int32> = [1, 2]`; `Array<Int64>` is fine, and an empty literal plus
`push` is fine. The interpreter and both wasm backends accept the literal, so
**parity is what found it** — the same shape as M4a's `Option<Float64>` payload and
its poison float -> int conversions, and the third milestone running where the
defect was in an engine nobody suspected.

Nothing in the corpus had ever written a sized-integer array literal: `bytes(s)`
produces the array everywhere it is used, `std/strings` starts from `[]` and
pushes, and the one place byte literals appear in an array (RFC-0077 M2g's
`stringFromBytes` test) targets the direct wasm backend. This milestone did not fix
it — the file ownership for M4b's three parallel modules put the emitters out of
reach — so the example works around it with a `buf(Array<Int64>) -> Array<UInt8>`
helper, documented at the call site with the reason. **The fix belongs to whoever
next touches `vyrn-codegen`'s array lowering**, and the two-line repro above is the
whole test.

### The count

Unchanged, and it must be: M4b(2) retired no builtin. Counting C function
definitions in `RUNTIME_SHIM` by M4a's stated method (including one-line
definitions, `static` as internal):

| | |
|---|---|
| C function definitions in the shim | 95 (66 exported, 29 static) |
| deleted in this milestone | **0** |

`__vyrn_line_at`, `__vyrn_col_at` and the `chars` lowering all stay. What changed is
that each now has a Vyrn definition proved equal to it, which is the precondition
for M5's cache-or-delete question rather than the question itself.

### Gates, at this note

Workspace tests green with all three sibling M4b modules in the tree (`tests/text.rs`
adds five rows: the two modules' inline `test` blocks plus the three differential
corpora, and they run in 0.2 s — the generated programs are ~4,000 rows and the
interpreter is not the bottleneck anyone expected). Parity green three ways over the
corpus including `examples/textbytes.vyrn`. `doc --std --verify` clean with
`docs/api/std/text.md` added. The RFC-0077 ladder is untouched — no builtin moved.

**What M4b's remaining half needs.** `stringFromBytes` is now the only text builtin
of the four that is not also written in Vyrn, and `decodeUtf8` is most of it: the
missing piece is not validation but the `Array<UInt8> -> String` construction, which
needs a primitive or a view the way `floatFromBits` did. `f64_str` stays where M4a
left it — a milestone with its own pin of `print`, not a corollary.

---

## M4c, as landed: ten of the fourteen route, and four refusals with reasons

M4b's three modules were an equivalence proof: fourteen builtins written in Vyrn and
shown to answer what the builtins answer, with the builtin as the oracle. M4c
performs the swap. **Ten builtins now ARE their Vyrn functions on the interpreter,
the native backend and both wasm paths, and the second and third implementations are
deleted rather than kept as opinions.**

| module | routed | refused |
|---|---|---|
| `std/codecs` | `hexEncode` `hexDecode` `base64Encode` `base64Decode` `urlEncode` `urlDecode` | — |
| `std/text` | `chars` | `lineAt`, `colAt` |
| `std/strpred` | `contains` `startsWith` `endsWith` | `slice`, `byteLength` |

### The mechanism is M2b's, turned into a table rather than copied

M2b's note said "the next builtin RFC-0078 moves reuses these mechanisms unchanged".
For these ten that held literally, and the way it held is the finding: **the whole
compiler part is a rename.**

`loader.rs` grew `RT_MODULES`, a const table of `(spec, reserved prefix, desugared
builtins, routed builtins)`. `Module::injected` became `Option<&'static str>` — the
prefix — because four runtime modules can now be in one link, which a single `bool`
could not represent. The injection block became a loop over the table. Nothing else
about the mechanism changed: declarations are still renamed to `$` spellings
unconditionally, `$` is still unlexable, and the two failures M2b made unreachable
stay unreachable.

`routed_builtin(name) -> Option<&'static str>` is the one function every engine
calls, and each engine's implementation of a routed builtin is now:

```rust
if let Some(rt) = vyrn_frontend::loader::routed_builtin(name) {
    if self.cx.sigs.contains_key(rt) { return self.call(m, b, rt, args, line); }
}
```

Three call sites, five lines each, exactly M2b's shape one size down — `toJson`
needed a shared AST walk and synthesized per-type encoders because it reads a
value's static type, and `hexEncode(s)` is `String -> String`.

**Routing in the engines rather than as an AST pass was the load-bearing choice, and
it is about diagnostics.** The obvious implementation is one rewrite in
`check_and_synthesize`, where M2b already appends encoders — one site instead of
three. It was rejected after measuring what it costs: the checker's builtin arms are
where `hexEncode(5)` becomes "`hexEncode` needs a String, found Int64", and a
program rewritten before the check reports that against `codecs$hexEncodeV`. Routing
inside each engine's existing builtin arm keeps the checker as the **typing** rule
while the **implementation** moves, which is precisely this document's own sentence —
"a builtin becomes, at most, a type-directed compiler part plus a call into Vyrn."
Verified rather than assumed: all three diagnostics still name the builtin.

Two hazards the pre-check rewrite would also have carried, both avoided by not taking
it: `rewrite_expr` renames `Expr::Var` as well as call names, so a local
`let contains = true` would have been rewritten into a function name; and it does not
walk `Program::tests`, where `examples/bin/client/boot.vyrn` alone has forty
`.contains` calls.

One question evaporated on contact. **All fourteen names are reserved** — `fn
contains` is "`contains` is a reserved name" today — so there is no shadowing rule to
preserve and the routing is unconditional.

### What was refused, and why each is a reason rather than a deferral

`slice` was named in advance. The other three were not, and each is refused on the
grounds M4a's note established:

- **`slice` traps** (`error: slice index out of range`, `error: slice splits a UTF-8
  character`) and Vyrn has no expression that aborts, so `sliceV` returns
  `Option<String>`. Routing it would change observable behaviour. M4b(3) called this
  a *control* primitive — a third category beyond "operation vs view" — and it needs
  a language decision, not a milestone. **Blocked on that decision.**
- **`byteLength` is a VIEW.** It is `strlen`: two instructions in the direct backend,
  one line in the interpreter. It is also folded by `consteval`, which is what lets
  `type Name = String where value.byteLength >= 3` be proved at compile time and a
  provably-wrong constant be rejected before it runs. Routing it would turn an O(1)
  read into an O(n) heap copy AND take that folding away — the opposite of the trade
  this RFC exists to make. `byteLength` belongs on M1's representation row beside
  `floatBits`, not on M5's.
- **`lineAt` / `colAt` are a cache question, which is M5's.** M4b(2) already measured
  the shape: they exist because the obvious loop is O(offset) and a scanner asks once
  per node, costing `std/vyx` 122 ms of a 291 ms page compile, and the interpreter
  memoizes a line-start table per buffer that a Vyrn library **cannot** — a generator
  may not touch module state (comptime purity), so the cache has to live below it.
  Routing them uniformly deletes that cache; keeping it means the interpreter holds a
  Rust arm the other engines do not, which this RFC allows ("a Rust fast path is fine
  provided the Vyrn implementation is the definition") but which is a different
  milestone with a different shape. Two builtins and ~20 lines of C, against the one
  cache in the repo a measurement says is load-bearing.

That is four refusals and ten moves, and the refusals are not the cheap ones.
`byteLength` and `lineAt`/`colAt` would have been trivial to route; they are refused
because routing them is wrong, not because it is hard.

### The oracle tests had to be converted, and one of them stopped meaning anything

The hazard M4b built towards and M4c had to discharge: **after the swap, an oracle
comparing the Vyrn implementation to the builtin is `x == x`.** Green forever,
proving nothing. Every one was converted, and the conversions are not uniform because
the three modules are not in the same position.

Converted to a pin, because the builtin and the Vyrn function are now one:

- `tests/codecs.rs`'s 6,354-comparison corpus. Same corpus, same generator; the
  program now calls only the BUILTIN and prints one line per answer, and the test
  asserts the SHA-256 of the transcript against a literal. A digest rather than a
  6,000-line golden file, with spot pins keyed by input so a failure has a readable
  neighbour, and the payload renderer rewritten onto `bytes` rather than `hexEncode`
  so the reporting does not depend on the code under test.
- `tests/text.rs`'s codepoint corpus, the same way, over 5,972 buffers.
- `examples/codecbytes.vyrn` and `examples/strpredbytes.vyrn`, whose `mine == builtin`
  rows became the values themselves plus `test` blocks of literals. `strpredbytes`
  lost a whole block — `predsAgree` asserted that a function equals itself over
  twenty inputs — and its twenty rows became the literal table that replaced it.

**Left as live oracles, because what they compare against did not move:**
`decodeUtf8`'s accept/reject against `stringFromBytes`'s over ~1,400 malformed
buffers, `lineAtV`/`colAtV` against `lineAt`/`colAt` at every offset from -3 to
`len + 3` of twelve buffers, and `sliceV`'s `None` against `slice`'s two traps over
ten ranges in ten processes. That asymmetry is the useful part of refusing four
builtins: half the M4b proof is still a proof.

Each converted test was checked to BITE by mutating the Vyrn implementation:
lowercasing `urlEncode`'s hex (86 rows and three pins), changing base64's 63rd
alphabet entry (`w7/Dvw==` -> `w7-Dvw==`), narrowing `hexVal`'s uppercase bound to
`c < 'F'`, and two off-by-ones in `decodeUtf8`'s three- and four-byte arithmetic. The
`hexVal` mutation is the one worth naming: **every literal pin still passed and only
the digest failed**, which is what says the digest is load-bearing rather than
decoration.

### The NUL fix, measured across the swap

M4b(1) found that a decoder producing `0x00` — `hexDecode("00")`,
`base64Decode("AA==")`, `urlDecode("%00")` — answered `Some` and **did not agree with
itself across engines**: the interpreter kept a Rust `String` holding the byte, the
native path returned a `char*` `__vyrn_strlen` truncated at it. `std/codecs` answers
`None`, which RFC-0014 requires and which is identical everywhere.

The corpus transcript was captured twice, once with the pre-swap binary (`494f883`)
and once after, and diffed line by line:

| | |
|---|---|
| pre-swap digest | `ad39879f2fbcb7df65ce9eb2da7145031af6fa99ccc92046ce9b2a591f926275` |
| post-swap digest | `2c1e8a949d6a051aea91bd9b6ca0fe67b8a8b1c6bb0a6e26ca7b163dfddac675` |
| rows that changed | **16 of 6,354** |
| rows that changed for any reason other than NUL | **0** |

Every one of the 16 is a decoder whose payload contained a genuine `0x00` byte
(`S00`, `S0041`, `S410010`, `S610062`, …) becoming `None`. So the swap is a **bug
fix**, it is scoped to exactly the rows predicted, and four representative rows are
pinned individually rather than left inside the digest. `chars`'s digest, by contrast,
is the SAME value before and after — 5,972 buffers byte for byte.

### The count, with the method stated, because this row has been wrong four times

Counted with `git diff -U0` against `494f883`, deletions only, classified by what the
deleted line was. The shim is counted by M4a's method exactly (C function definitions
in `RUNTIME_SHIM`, one-line definitions included, `static` treated as internal),
which reproduces M3's and M4a's 95 / 66 / 29 on the parent commit — that is how the
method was checked rather than assumed.

| | |
|---|---|
| LLVM IR text deleted from `vyrn-codegen`'s runtime `const`s | **603** — the six codecs 485, the hex-digit helpers 36, `__vyrn_str_chars` 82 |
| Rust deleted from the textual emitter's `gen_call` | **88** |
| Rust deleted from the interpreter | **204** — 160 of codec helpers, 44 of call arms |
| C function definitions in the shim | **94 (65 exported, 29 static)**, was 95 (66, 29) |
| deleted in this milestone, in C | **1** — `__vyrn_strncmp` |
| `declare` lines at the boundary | two fewer (`__vyrn_strncmp`, `strstr`); `shim_link`'s census 57 -> 55 |

**One C function, and that is the honest number.** These builtins were chosen by M4b
precisely because they had no shim implementation — the codecs' 485 lines were IR the
textual emitter printed, not C it called. The single C casualty is `__vyrn_strncmp`,
which `startsWith` and `endsWith` were the only callers of, and it is the first shim
function this RFC has retired since M2b. `strstr` also left the boundary; it is
libc's, so nothing was deleted, but the `declare` and the `__vyrn_gen_libc`
keep-alive entry went.

895 lines of Rust and IR deleted against the 762 lines of Vyrn M4b had already
committed (350 + 270 + 142). The trade is not "less code" and should not be sold as
one — it is **one definition instead of two or three**, plus a fourth the direct
backend no longer owes.

### RFC-0077's ladder: 46/84 -> 49/87, and the reason is the whole relationship

`codecbytes.vyrn`, `strpredbytes.vyrn` and — the interesting one —
`examples/encoding.vyrn`, which has called the codec builtins since RFC-0014 and was
blocked on them and nothing else. The direct backend had **no lowering for any of the
ten**, so this is the RFC-0077 relationship as arithmetic: ten rows that would each
have had to be hand-emitted in `wasm-encoder` became a library the backend already
compiles. `textbytes.vyrn` still fails there, and now for a reason this milestone
chose: it exercises `lineAt`/`colAt`, which were refused.

The corpus grew 84 -> 87 with M4b's three examples, so the honest reading is 46 -> 49
of a larger denominator: without the routing all three new examples would have
failed, making it 46/87.

### The interpreter got measurably no slower, which was not obvious

`std/strpred`'s `byteLengthV` is `bytes(s).length`, which **allocates**, and
`std/vyx` calls these predicates 97 times over a page — so an O(n) copy per
`startsWith` in an interpreted scanner was the plausible regression. Timed with
`VYRN_NO_GEN_CACHE=1`, eight runs, best-of:

| | pre | post |
|---|---|---|
| `examples/vyxdemo.vyrn` | 79 ms | 76 ms |
| `examples/bin` (the largest generator app) | 933 ms | 951 ms |

Two percent on the large one and nothing on the small one. So the module was left
exactly as the equivalence proof wrote it — imports nothing, reaches no builtin but
the byte view — rather than rebuilt on `s.byteLength` for a speed nothing needed.
Recorded because the *decision* was to measure before optimizing, and the measurement
is what kept `std/strpred` identical to the code its proof covers.

### Two properties M2b's tests could not have covered

Both are new with the table and neither follows from M2b's single-module test, so
both are pinned in `tests/codecs.rs`:

- **Four runtime modules in one link.** A program mentioning `toJson`, `hexEncode`,
  `chars` and `contains` injects all four, each renamed to its own prefix.
- **A new module's PRIVATE names.** A program declaring its own `hexDigit`, `hexVal`,
  `decoded`, `ascii`, `b64Val`, `showCps` and `byteLengthV` — every private name
  `std/codecs` has, plus one export each from `std/text` and `std/strpred` — while
  calling all four routed builtins. Every line resolves to the user's function and
  every builtin still answers.

`every_route_is_spelled_with_its_modules_prefix` pins the table itself: each route's
reserved name is its module's prefix plus the export, no builtin is claimed twice,
and `slice` / `lineAt` are asserted absent so a later edit cannot route them by
accident without reading this note.

### Gates, at this note

1230 workspace tests (the same count as M4b's: six deleted with the code they
pinned, four added and two renamed), parity green three ways over 87 examples, the
RFC-0077 ladder at **49/87** and its shim-linked shape likewise 49/87, genwasm green,
`doc --std --verify` clean, `vyrn-lsp` builds, `fmt --check` clean on every edited
`.vyrn`.

**What M4b's remaining half and M5 now need.** `stringFromBytes` is still the only
text builtin of the four with no Vyrn implementation, and `decodeUtf8` is most of it:
the missing piece is the `Array<UInt8> -> String` construction, which wants a
primitive or a view the way `floatFromBits` did. `f64_str` stays where M4a left it.
M5 inherits a smaller question than it was written for — `toJson` and these ten hold
no Rust arm at all, so "the count of Rust arms should fall" has already happened for
eleven builtins, and what is left for M5 is the *deliberate* caches: `lineAt`/`colAt`,
and whether the language grows the abort primitive `slice` needs.

## The strictness ruling M3 asked for

M3 stopped partly because the two readers disagree about which documents parse,
and said a ruling was needed before any swap. Here it is. **`std/jsonread` wins
all three**, and each for its own reason rather than by a blanket preference.

| case | today (C) | `std/jsonread` | ruling |
|---|---|---|---|
| `"😀"` | rejected, `unexpected character at position 11` | accepted, decodes 😀 | **accept.** A surrogate pair is exactly how RFC 8259 escapes an astral codepoint. Rejecting it means Vyrn cannot read valid JSON that any other implementation writes, which is a bug, not strictness. |
| `{"s":"a","s":"b"}` | accepted, first wins | rejected, naming the key | **reject.** RFC 8259 says names SHOULD be unique and leaves duplicates unpredictable. For a *typed decode* the ambiguity is the problem: silently picking one is the only outcome with no diagnosis. RFC-0059 already declared this reader strict; this is that declaration applied. |
| error text | `<reason> at position N`, 0-based byte | `line N, col M: <reason>`, 1-based | **line and column.** A byte offset into a document a human did not write is not actionable. |

Two consequences, stated so they are not surprises.

**The duplicate-key change can break a working program**, and it is the only one
of the three that can. That is acceptable here for the reason RFC-0071 and
RFC-0072 already used: pre-1.0, no users, no compatibility window. It would not
be acceptable later, which is an argument for making the ruling now rather than
after the swap has shipped.

**Every parse-error message changes shape**, so every pin carrying one has to be
recaptured. `examples/jsondecbytes.vyrn` was written failure-shape-first exactly
so this is a visible edit rather than a silent drift — the diff should show the
wording moving and nothing else moving with it.

And the general principle this settles, which applies to every later milestone:
when the Vyrn implementation and the C one disagree, **neither wins by default**.
The question is which is correct, decided case by case and written down. M2
resolved `\b`/`\f` in favour of Vyrn because the C spelling had no observer; M4b
resolved the NUL decoders in favour of Vyrn because C did not even agree with
itself; here the surrogate case goes to Vyrn because C is wrong about JSON, and
the duplicate-key case goes to Vyrn because strict is the ruling this project
already made.

---

## M3, as landed: `fromJson` is Vyrn on all four paths, and the shim halved

M3 is **done**, at the third attempt and after two milestones that existed to
unblock it. `fromJson` reads through `std/jsonread` and decodes through a per-type
walk generated as Vyrn, on the interpreter, the native backend and **both** wasm
backends. No engine holds a JSON reader, a DOM, a number reader or a decode
message assembler any more, and the C shim lost **half of itself**.

Every one of the three things M3's own note said it needed turned out to be
needed, and one of the three was wrong about what would work.

### The shape M3 predicted is not the shape that works, and the reason is one line

M3 said: "every decoder returns `Option<T>`, pushes its own issues, and a
composite constructs only when all its parts are `Some`". The second half of that
is exactly right and the first half **cannot be built**:

```
error: nested Option/Result is not supported in v0.1
```

A bare `Option<U>` **is** a decode target — `Array<Option<Int64>>` and
`Map<String, Option<String>>` decode today, and so does an `Option<U>` enum
payload — so an `Option`-returning decoder needs `Option<Option<U>>` for those,
which the checker refuses outright. Measured, not argued.

So a decoder returns **`Array<T>` with zero or one element**. One convention for
every `T` including `T = Option<U>`, and it reads better than the shape it
replaces:

```vyrn
for n in jsondec$dInt64(v, path, iss) { ... }   // runs iff a value was produced
```

`for` over the result *is* the "did it decode" test, so nothing is unwrapped
anywhere. The cost is an allocation per decoded scalar, which is the trade this
RFC says it is making ("not a performance project") and which parity, the ladder
and the suite all absorbed without a measurable move.

### Two halves again, and the untyped half is 441 lines of Vyrn

Same split as M2b, mirrored:

```
read(s)          -> a `Json` tree                 [std/jsonread — not typed]
decode(tree, T)  -> a value of T, plus `Issue`s   [needs the compiler]
```

`std/jsondec` is the *untyped* half: the kind names, the RFC-0018 `Issue`
vocabulary, the path arithmetic, the tree accessors and the scalar decoders — every
part of `fromJson` that is not directed by a type. It stands on `std/jsonread` for
the reader and `std/num` for `parseInt64`/`parseUInt64`/`parseFloat64`/`parseFloat32`,
which is M4a being spent exactly as M4a predicted.

`vyrn-frontend/src/jsondec.rs` is `jsonenc.rs` run backwards and reuses both of its
mechanisms unchanged: generated SOURCE handed to the parser (there is still no
`Expr` printer, and writing one would be a second spelling of the language), and
one function per distinct type so a self-referential type terminates — `Node = { n:
Int64, kids: Array<Node> }` decodes three levels deep in the corpus.

### The mechanism is one table row, which is what M4c bought

`RT_MODULES` grew an entry for `std/jsondec`, and `std/json`'s `desugared` list
grew `fromJson` because the `Json` tree the decoders walk is declared there. That is
the whole loader change. Both are `desugared` rather than `routed` because both
builtins need a type the table cannot express, and each engine routes at **its own
builtin arm** — M4c's load-bearing choice, kept, and verified rather than assumed:

```
`fromJson`'s second argument must be a String, found Int64
`fromJson` needs a declared type name; `Nope` is not a type
`fromJson` cannot decode into `R` (not a codable type)
```

All three still name the builtin. `fromJson`'s diagnostics are richer and more
user-facing than any codec's, so this mattered more here than at M4c.

### The predicate sharing survived, and it moved down a crate to do it

`emit_predicate_cond`'s comment claims to be the ONE place a predicate is lowered,
shared with the RFC-0018 decode path. After M3 the decode path lowers nothing —
so what is shared had to become the *structure*, and `predicate_binds` moved from
`vyrn-codegen` to `vyrn_frontend::types`, the only crate both the emitters and the
generator can see.

A refined type's decoder calls a synthesized `Bool`-returning function whose
**parameters are that same list** and whose **body is the `where` clause's own
`Expr`**. So the accumulating `validate` check and the trapping one evaluate the
identical expression with the identical bindings, and there is no second spelling
of what `value` means. It is built as AST rather than printed for the same reason
`jsondec` prints everything else: a predicate has no source form to print.

Construction is the pleasing part. `out.push(b)` into an `Array<Age>` performs the
validated coercion, which is the same boundary `Age(n)` goes through — and unlike
`Age(n)` it also works for a refined RECORD base, where there is no `Name(value)`
constructor form at all.

### One capability that needed a new mechanism, and it is tiny

Vyrn has **no anonymous record literal**: `{ c: 1 }` is not an expression, only
`T { c: 1 }` is. A nested inline record type (`type A = { b: { c: Int64 } }`)
decodes today and nothing in the repo writes one — so a decoder for it had no
spelling to construct. A type *position* accepts the anonymous form perfectly well,
so what is synthesized is one transparent `type` alias per shape, used for the
literal and nothing else, appended beside the decoders. Six lines, and the row
still decodes.

### Accumulation order: preserved by construction, and proved by a reversal

The load-bearing pin is the ORDER, and it holds because the generated walk visits a
record's fields in declaration order and an array's elements in index order, which
is what the IR walk and the interpreter's walk both did. `kids[1]`'s missing `name`
is still reported before its out-of-range `age`.

That is not an argument, though, so it is checked twice: the pin, and a mutation.
Reversing the field walk in `jsondec.rs` produces
`validate@kids[0].age | validate@kids[1].age | json.missing@kids[1].name` and the
test names both sequences.

### The strictness ruling, applied — and a FOURTH difference the ruling missed

All three ruled rows behave as ruled: a surrogate pair decodes, a duplicate key is
refused naming the key, and every parse error carries `line N, col M:`.

The corpus found a fourth, which no one had looked for. **The C reader accepted a
leading zero**: `{"v":01}` decoded as `1`. RFC 8259's number grammar is
`0 | [1-9][0-9]*`, so `std/jsonread` is right and this lands as a fix in the same
class as the surrogate case — C was **wrong about JSON**, not merely looser. Eleven
rows of the corpus move on it.

### The digest, the diff, and the one row that was a bug rather than the ruling

The hazard M4c named applies here in full: with the C reader and the IR decoders
gone, a differential test would be `x == x`. So `tests/jsondec.rs` is a PIN — one
generated program over **825 decode inputs across seventeen target types**,
asserted as a SHA-256 plus twenty-eight spot pins keyed by input rather than by
index. Both digests were captured by running that corpus against a **copy of the
pre-swap release binary** (`d41e521`) and against the post-swap one, then diffing
line by line:

| | |
|---|---|
| pre-swap (C `__vyrn_json_parse` + emitted IR) | `6f084f85a50e4ed402129ccb81167d4d932c5417d4eaafd45542c2717e11b8a5` |
| post-swap (`std/jsonread` + generated Vyrn) | `d94e2486f1539d5f9aced50faa71800d5a9ea6e20439c27bbb5c79d2bfcae852` |
| rows that changed | **212 of 825** |
| rows that changed for any reason other than a parse error | **0** |

The 212 are 196 rewordings, 11 leading zeros, 3 duplicate keys and 1 surrogate
pair. No `json.type`, `json.missing` or `validate` row moved, no path moved, no
order moved.

**It was 213 on the first run, and the extra row was a real defect.** `Float64` of
`9223372036854775808` came back as `4611686018427387904` — exactly half.
`std/num`'s `ldexp` spilled a rounding carry out of the top mantissa bit into the
exponent FIELD with an `|`, and `|` is idempotent: right for an even biased
exponent, halved for an odd one. So `parseFloat64("1.9999999999999999999")` was
`1.0` while `0.9999999999999999999` and `3.9999999999999999999` were both correct,
which is why M4a's 302-input oracle — thorough about ties, subnormals and overflow —
never saw it. Fixed here, with twelve inputs of the class added to `tests/numbers.rs`,
where reverting the fix now makes **4 of 314 disagree with Rust's own parser**.

That is the sixth milestone in a row where writing the boring pins first found
something only running could catch, and the third where the defect was in the
component nobody suspected. It is also the first time the defect was in **Vyrn
code this RFC had already landed**, which is the honest cost of the thesis: a
runtime in Vyrn is checked three ways, and it is still code someone has to get
right.

Five mutations were applied and each checked to fail: `dIntRange`'s upper bound off
by one (the `Int8` pin), the field walk reversed (the order pin), `keyOf` accepting
a multi-member object (the one-wire-form pin), `fieldPath` joining without the `.`
(every nested path), and `unsigned_max(16)` widened by one — that last caught by
**the digest alone**, which is what says the digest is load-bearing rather than
decoration.

### The count, with the method stated, because this row has now been wrong five times

Counted with `git diff -U0` against `d41e521`, deletions only, classified by what
the deleted line was. The shim is counted by M4a's method exactly — C function
definitions inside the `RUNTIME_SHIM` raw literal, one-line definitions included,
`static` treated as internal — which reproduces `d41e521`'s **94 / 65 / 29**, and
that is how the method was checked rather than assumed.

| | |
|---|---|
| LLVM-IR-emitting Rust deleted from `vyrn-codegen/src/lib.rs` | **1,282** — the two per-type decoder drivers, `emit_decode` and its seventeen friends, `push_issue`/`push_type_issue`/`str_g`, `build_issue_array`, `collect_codec_strings`/`gather_codec_strings`, and 22 `declare` lines |
| C deleted from `RUNTIME_SHIM` | **341** |
| Rust deleted from the interpreter | **386** — `decode_top`/`decode_val`/`decode_variant`/`run_predicate`/`issue_val`/`type_issue`, and `intn_from_num` |
| C function definitions in the shim | **47 (35 exported, 12 static)**, was 94 (65, 29) |
| **retired in this milestone, in C** | **47 — 30 exported, 17 static** |
| `declare` lines at the boundary | 22 fewer; `shim_link`'s live census **55 -> 33** |

**M3 predicted 32 and it was 47.** The difference is entirely in the `static` count
and it is a counting error in M3's own note, not drift: M3 reported "27 exported, 5
static" for the JSON section, but the section holds **17** statics — the eleven
`vjp_*` recursive-descent functions, the three `vsb_*` growable-buffer functions,
`__vyrn_dup`, `__vyrn_vj_new` and `__vyrn_vj_num_text`. The exported figure was
close (27 predicted, 30 actual: `__vyrn_json_field_path`, `__vyrn_json_index_path`
and `__vyrn_vj_kindname` were not on M3's list). Whoever counts next should say how,
and should count the statics.

Against that, **798 lines of Rust generator plus 441 lines of Vyrn** were added.
The trade is not "less code" and should not be sold as one — 2,009 lines of
implementation deleted against 1,239 added is a real reduction, but the point is
**one definition instead of three**, plus a fourth the direct backend no longer
owes.

### RFC-0077's ladder: 49/87 -> 54/87

`enumarray`, `enumcodec`, `jsoncodec`, `jsondecbytes` and `namespace`. The direct
backend had no lowering for either `fromJson` row, so this is the RFC-0077
relationship as arithmetic once more: two rows that would each have had to be
hand-emitted in `wasm-encoder` became a library the backend already compiles, and
every example whose only blocker was a decode came with them. **`jsondecbytes` is
among them**, so the pin — failure shapes, paths and accumulation order included —
is now checked interp == native == wasm == direct-wasm rather than three ways.

`storage.vyrn` still fails there and now for a reason this milestone did not
create: a branch yielding a call, which is RFC-0077's own row.

### Two seams, stated plainly

**A single-source program cannot decode.** `fromJson` needs a std root, exactly as
`toJson` has since M2b, so `vyrn_frontend::run(source)` with no resolver refuses by
name (`` `fromJson` needs the Vyrn runtime: no decoder for `U` ``). Nine interpreter
tests and one loader test moved to the loader path with the real `std/` text; the
codegen test that pinned `@__vyrn_dec_Shape` now pins the refusal instead.
`loader::tests::run_multi` gained `check_and_synthesize` in place of a bare check,
because since M2b a *linked* program is not a *runnable* one.

**`Map` decode is still blocked on the direct backend**, for RFC-0077's reasons
rather than this RFC's: the generated body uses `m[k] = v`, which that backend does
not lower yet. Unchanged from before the swap.

### Gates, at this note

1,233 workspace tests (was 1,230 — the corpus, `std/jsondec`'s unit blocks, and the
`jsondec` generator's own two), parity green three ways over 87 examples including
the repinned `jsondecbytes`, the RFC-0077 ladder at **54/87** and its shim-linked
shape likewise, genwasm green, `doc --std --verify` clean with
`docs/api/std/jsondec.md` added, `vyrn-lsp` builds, `fmt --check` clean on every
edited `.vyrn`. The shim is at **47 C definitions / 35 exported**.

**What is left of this RFC.** M4b's remaining half is unchanged: `stringFromBytes`
wants an `Array<UInt8> -> String` construction the way `floatFromBits` did, and
`f64_str` stays where M4a left it. M5 is unchanged too — `lineAt`/`colAt`'s
deliberate cache, and whether the language grows the abort primitive `slice` needs.
`std/json`'s tree, `std/jsonread`, `std/jsondec` and `std/num` are now the entire
JSON and number runtime, in Vyrn, and the shim has no opinion about either.

---

## M1 + M5, as landed: the census, and the boundary it defines

M1 was "name the primitives" and was never taken as its own milestone, because
every later one discovered a piece of the answer. M5 was "the interpreter's fast
paths become caches, and the count of Rust arms should fall" — and the count
fell before M5 was reached, twelve arms at a time, in M2b, M4c and M3.

So what was actually missing was neither: it was a **census**. A single statement
of why each remaining builtin is a builtin, checkable against the code. That is
what landed. Nothing moved, and that is the honest result rather than a shortfall
— two candidates were checked for movement and both are refused with reasons, and
the one arm that has no reason is named as a finding rather than swapped in a
milestone whose job was to draw the boundary.

### The census is a test, not a table in a document

`vyrn-frontend/tests/primitives.rs` holds the census as a `const` beside an
extractor that reads `interp.rs`, and asserts the two agree. A new Rust arm
without a row fails the suite naming the builtin; a row whose arm was routed into
Vyrn fails the other way. `nothing_is_both_censused_and_routed` walks
`RT_MODULES` and asserts no name is in both lists, which is this RFC's acceptance
criterion — "no builtin has two *definitions*" — as an assertion rather than a
sentence. It is the precedent M4c set with
`every_route_is_spelled_with_its_modules_prefix` (which pins the route table and
asserts `slice`/`lineAt` absent), one level up.

Both tests were mutation-checked: deleting the `alen` row fails
`the_census_is_the_code` naming `alen`, and adding a bogus `("parse", …)` route to
`RT_MODULES` fails `nothing_is_both_censused_and_routed` naming `parse`.

### The method, because this document has been wrong about a count five times

The census covers every builtin the **interpreter** implements in Rust on the
`Expr::Call` path, which is where M5's "count of Rust arms" lives. Two regions of
`interp.rs`, both located by content rather than by line number:

- the `if name == "…"` guards between `Expr::Call { name, args, line } => {` and
  `match name.as_str() {` — the builtins handled *before* the arguments are
  evaluated, because they need the AST (`schemaOf`) or must write back through a
  binding (`@pop`);
- the arms of `match name.as_str() {`, up to its `_ => {` fallthrough.

A name counts once, so `"lineAt" | "colAt"` is two and `"trace" | … | "error"` is
five. That yields **62**: 51 arm names in 46 arms, plus 11 guards.

**62 is a larger number than the 50 this document opened with, and the core is
smaller.** The difference is entirely method, and stating it is the point:

| | |
|---|---|
| reserved builtin names with a Rust implementation | **42** |
| shadowable gen-only surface builtins (`raw`, `rawAt`, `render`, `lex` — RFC-0054) | 4 |
| `@`-prefixed internal desugars no source can spell | 13 |
| the compiler's own sum constructors (`Some`, `Ok`, `Err`) | 3 |
| **total** | **62** |

The comparable figure against the header table's 50 is **42**. Three things are
deliberately outside the scan and are named so their absence is not read as an
omission: `byteLength` is an `Expr::Field` read rather than a call (its refusal is
M4c's and is pinned in `vyrn-cli/tests/codecs.rs`); `hostNowMillis` /
`hostMonotonicNanos` / `hostRandomSeed` are `extern` declarations, not builtins,
and are already the arrangement this RFC argues for — `std/time` and `std/random`
are Vyrn above a named syscall; and numeric conversions (`Int32(x)`) are resolved
by `numeric_conv_target` as part of the type system.

### The taxonomy, which was discovered rather than designed

Each of the first seven categories was found by a milestone hitting it. The last
two are not reasons to *be* a primitive — they are the honest labels for arms that
are movable and were not moved.

| # | category | count | what makes it irreducible |
|---|---|---|---|
| 1 | **Memory** | 16 | `__vyrn_malloc` cannot be written without a memory-growth primitive, and every container standing on it — `Array`, `Map`, `SmallArray`, the slot table — needs a **raw-memory view** the language does not have. The largest row, and the first open question below is what it waits on. |
| 2 | **Syscall** | 15 | RFC-0077 M2j measured a directly-emitted module's whole import list: twelve WASI functions. You cannot write `fd_write` in terms of itself. |
| 3 | **Representation (a view, not an operation)** | 4 | M4a's central finding. Nothing could construct a `Float64` from anything but another number, so every text-to-float route had to be a builtin; give Vyrn the *bits* and the operation stops needing to be primitive. `bytes` is the same thing for `String` and `stringFromBytes` is its inverse. |
| 4 | **Control** | 4 | M4b(3)'s finding. Vyrn has no `panic` and no `abort`, so no Vyrn implementation of a *trapping* builtin can be observationally equal. `@join` is the same shape one step over: an expression that waits for another task is not spellable either. The second open question is this row's. |
| 5 | **Compiler-directed** | 17 | Needs the static type of an arbitrary expression, the module graph, or the compiler's own lexer and AST. |
| 6 | **A cache, with a stated reason** | 2 | M5's own row, and the only one. |
| 7 | **The semantics differ observably** | 1 | Moving it is a language change, not a mechanical move. |
| 8 | **Movable, refused on a measured cost** | 2 | Each is owed its own milestone with its own pin, because the blast radius is every program rather than the programs that mention a builtin. |
| 9 | **No reason at all** | 1 | The finding. |

And the rows themselves. The evidence column is one line each; the authority is
the test, which carries the same text beside the category.

**1. Memory (16).** `array` `push` `at` `alen` `afree` `@list` `@toArray` `@pop`
`@swapRemove` — `Array`, its heap triple and the two mutators that write back
through a binding; `@has` `@keys` `@remove` — `Map` (RFC-0028); `cell` `get` `set`
`release` — the slot table's allocation and generation-checked access. `at` and
`@swapRemove` also trap, so they are on row 4's list of things that would need the
abort primitive *as well as* the memory view.

**2. Syscall (15).** `print` (fd_write, stdout); `logger` `trace` `debug` `info`
`warn` `error` (RFC-0008 — fd_write on the configured sink); `args`
(args_sizes_get + args_get); `readLine` (fd_read, stdin); `readFile`
`readFileBytes` `writeFile` `renameFile` `fsyncFile` `listDir` (path_open, fd_read,
fd_write, path_rename, fd_sync, fd_readdir). Worth noticing rather than acting on:
a single `fdWrite(fd, Array<UInt8>)` view would make **seven** of these fifteen a
Vyrn library, since `print` and the logger are the same syscall twice. It is not
proposed here, because a builtin that lets any program write to any descriptor is
an escape from RFC-0014's I/O story rather than a view of a value, and RFC-0014's
canonical error wording is single-sourced in the compiler precisely so the OS's
text can never reach a user.

**3. Representation (4).** `bytes` — `String -> Array<UInt8>`, what all four
runtime modules stand on; `stringFromBytes` — the inverse; `floatBits`
`floatFromBits` — M4a's two IEEE-754 bit views, one instruction each. Plus
`byteLength`, which the scan cannot see because it is a field read.

**4. Control (4).** `slice` (two traps), `assert`, `assertEq` (RFC-0015 — a
failing assertion traps the test), `@join`.

**5. Compiler-directed (17).** `toJson` `fromJson` — and these are the design in
one row: only the *walk* needs the compiler, and the writer, reader and decoders
are Vyrn (`std/json`, `std/jsonread`, `std/jsondec`); `moduleInterface` (the
module graph); `schemaOf` `contractOf` `jsonSchema` (reflection over a
declaration); `value` (boxes a scalar into the interpolation enum by its type);
`blackBox` (an optimizer barrier is a backend property, and the identity in an
interpreter that does not optimize); `raw` `rawAt` `render` `lex` `@codeText`
`@codeSplice` (RFC-0054 code quotes — `lex` is the compiler's own lexer);
`Some` `Ok` `Err` (constructors of the compiler's own `Option` and `Result`).

**6. Cache (2).** `lineAt` `colAt`.

**7. Semantics (1).** `parse`.

**8. Measured (2).** `@str` `@concat`.

**9. Unjustified (1).** `@charCount`.

### What was checked for movement, and refused

Two candidates, both plausible on paper, both refused — and in both cases the
category *is* the reason.

**`logger` is a syscall, three times over.** The five level methods lower to
`fprintf` on a stream, and there is no expression in Vyrn that writes anywhere but
stdout: `print` is the only output the language has, and the logger's whole point
(RFC-0008) is that logs are *not* on stdout. That alone is the wall. Two further
costs, recorded so a later attempt does not rediscover them: the threshold is
**folded at compile time** — with `logging { level: warn }`, a `.debug(..)` call
emits no write at all, and routing would turn a deleted call into a runtime
comparison, which is `byteLength`'s consteval argument in different clothing; and
the `file(..)` sink is a `FILE*` opened once in `@main` and held in a global, while
`writeFile` opens, truncates and closes, so Vyrn cannot express "append to a held
handle" either. `logger` itself is the identity (a handle is its name string) and
could move alone — which would be exactly the half-Vyrn, half-C builtin this
document names as worse than either end state.

**`stringFromBytes` is not waiting for a view — it IS the view.** M4b(2) left it
as the last unmoved text builtin and predicted it "wants a primitive or a view the
way `floatFromBits` did". Asked in M4a's terms the answer is that the prediction
resolves to a tautology: `stringFromBytes` is the *only* `Array<UInt8> -> String`
construction in the language, so a Vyrn implementation would have to call itself.
Its validation half genuinely is expressible — `std/text`'s `decodeUtf8` is the
proof, and it is checked against `stringFromBytes` over ~1,400 malformed buffers —
but a builtin whose validation is Vyrn and whose construction is C has two halves
and a seam. Splitting it would mean adding an *unchecked* `Array<UInt8> -> String`,
which is the raw-memory escape hatch under a different name: it can build a
`String` that is not valid UTF-8, and the whole point of `bytes`/`stringFromBytes`
being a pair is that one direction is total and the other is validated.

So `stringFromBytes` belongs on the representation row beside `floatFromBits`, and
M4b's "remaining half" is closed as a refusal rather than left open. What is
genuinely still open from M4b is `f64_str`, and it is row 8's.

### The one arm that fits no category

**`@charCount`.** `s.charCount()` (RFC-0058) counts Unicode scalar values as the
bytes where `(b & 0xC0) != 0x80`. It is implemented **three times**: a Rust arm in
`interp.rs`, `__vyrn_charcount` in the C shim, and ~30 lines of hand-emitted
`wasm-encoder` in `direct.rs`. It needs no primitive (`s.byteLength` and `s[i]`
are the whole substrate — the same substrate `std/strpred` is built on), it does
not trap, it is not folded by `consteval` the way `byteLength` is, and it is not
hot: **there is exactly one caller in the repository**, `examples/bytecount.vyrn`.

It is not moved here, and the reason is scope rather than difficulty — a census
that swaps a builtin is two milestones in one, and this one's job was to draw the
boundary. Priced, so whoever takes it does not have to:

- `std/text` gains a `charCountV` (five lines) and `RT_MODULES` gains one route
  pair on `std/text`, which is already injected for `chars`.
- Three implementations delete: the interpreter's arm, `gen_call`'s two-line
  lowering plus its `declare`, and `direct.rs`'s emitted loop. The shim goes
  **47 to 46**.
- Two tests need a new witness rather than a fix: `wasm.rs`'s
  `the_boundary_census_is_the_declare_lines` uses `__vyrn_charcount` as its
  signature example, and `string_char_count_lowers_to_charcount_shim` pins the IR
  that would stop being emitted.
- **The one real hazard is `direct.rs`'s `Rt` table**, where every runtime
  function is `base + n` and the emission order must match. Removing
  `charcount: base + 11` renumbers thirty-two entries, and a misnumbering only
  fails loudly where the signatures happen to differ. That is a different risk
  class from a table row, and it is why this is a follow-up rather than a
  postscript.
- **The pin already exists.** `examples/bytecount.vyrn` prints `byteLength`
  beside `charCount()` and is in the parity corpus, so the bytes are pinned
  interp == native == wasm before any swap — the discipline M2 established, for
  once satisfied for free.

### The final boundary, stated

RFC-0078 opened with "everything above the primitive core is written in Vyrn."
The core turned out to be, exactly:

> **The allocator; the twelve WASI syscalls; four representation views (`bytes`,
> `stringFromBytes`, `floatBits`, `floatFromBits`, plus `byteLength` as a field);
> and the compiler parts — a static type, the module graph, or the compiler's own
> lexer.**

Everything else that remains in Rust is there for a reason this document now
names: two trap (plus two more that trap on top of needing memory), one waits for
a task, two are the interpreter's one deliberate cache, one has different
semantics from its library twin, two are refused on a measured cost, and one has
no reason at all.

**What the core does NOT contain, and did when this was written:** JSON — reader,
writer, DOM, decoder, number reader and message assembler; UTF-8 decoding; text to
number, in every width, correctly rounded; six encodings; three string
predicates. Those are `std/json`, `std/jsonread`, `std/jsondec`, `std/num`,
`std/codecs`, `std/text` and `std/strpred` — 2,000-odd lines of ordinary Vyrn,
checked three and four ways by the same suite that checks user code, where there
were previously three or four implementations each.

The claim holds **as scoped**. It does not hold in the sense a reader might hope
for, and the difference is one row of the census: the containers are not Vyrn, and
`Array` is the most-used type in the language. That is not an oversight or a
deferral — it is a language decision this RFC deliberately does not make.

### Two open language questions, recorded and not decided

Both are language-identity decisions. Each belongs in its own RFC, and each now
has accumulated evidence rather than an opinion.

**A. A raw-memory view.** `Array`, `Map`, `SmallArray`, the slot table and the
allocator would be Vyrn if the language could name raw memory — 16 of the 62
census rows, the single largest category, and the one that stands between "the
runtime is Vyrn" and "the runtime is Vyrn including the parts every program
touches". The evidence for it is that this is what "self-hosted" actually
requires, and that M4a's finding generalizes: the irreducible primitive keeps
turning out to be a missing *view* rather than a missing operation, and a raw
memory view is the last one. The evidence against it is the pitch. Vyrn's claim is
that ownership, drops and validation are *checked* — refinement types are
trustworthy precisely because validation happens at every value boundary
(M3 measured that a Vyrn program cannot even spell a value that failed its own
predicate, which is why the accumulating decoder needed a different shape). A raw
pointer view is an escape hatch from all three at once, and the `stringFromBytes`
refusal above is the same question in miniature: an unchecked
`Array<UInt8> -> String` would move one builtin and would also make invalid UTF-8
constructible. Anyone opening this should carry that as the framing: the question
is not "can the containers be Vyrn" — they can — it is **what a checked language
gives up to write its own allocator, and whether an `unsafe`-shaped region is a
price worth paying for a row of the census.**

**B. An abort primitive.** `panic`/`abort` — an expression of type `Never` that
terminates with a message on stderr and a nonzero exit. It unblocks `slice`
directly (M4b(3) wrote `sliceV` returning `Option<String>` and proved it equal to
the builtin everywhere except the trap, over ten ranges in ten processes, so the
swap is one wrapper line the day the primitive exists) and `assert`/`assertEq`
with it. It also makes the *other* trapping builtins expressible in principle:
`at` and `@swapRemove` trap on an out-of-range index, and division by zero,
overflow and the region-depth limit all trap today from the compiler's own
`@.trap.*` globals. The evidence for it is that every backend already has one —
`proc_exit`, `unreachable`, `exit` — so this is naming a capability rather than
building one, and that M4b(3) identified it as a third *kind* of primitive
(neither operation nor view, but **control**) which M1's list had no row for. The
evidence against it is smaller but real: `Option` is the house idiom for a partial
function throughout `std/`, an abort expression invites `slice(s, i, j)!`-shaped
code where a `match` is better, and the canonical trap wording is currently
single-sourced in the compiler where nothing can drift from it — a user-callable
`panic("...")` puts arbitrary text on the channel parity compares byte for byte.
Anyone opening this should decide the wording question first, because it is the
one that touches every existing pin.

### The count, with the method stated

Counted by M4a's method exactly — C function definitions inside the
`RUNTIME_SHIM` raw literal, one-line definitions included, `static` treated as
internal — which reproduces this commit's parent at 47 / 35 / 12, and that is how
the method was checked rather than assumed. The regex is "a line beginning at
column 0 with an identifier and ending in `) {`", which counts *definitions*: the
platform-conditional pairs (`__vyrn_read_file`, `__vyrn_spawn`,
`__vyrn_task_main`) count twice, as they have in every figure since M3.

| | |
|---|---|
| C function definitions in the shim | **47 (35 exported, 12 static)** |
| deleted in this milestone | **0** |

Unchanged, and it must be: M1+M5 moved no builtin. **A milestone that retires none
reports a zero rather than a smaller number reached by counting differently** —
this is the fifth in this RFC to do so, and the first where the zero is the
*result* rather than a side effect, since the milestone's product is the statement
that the remainder is justified.

### Gates, at this note

1,236 workspace tests (was 1,233 — the census's three), parity green three ways
over 87 examples, the RFC-0077 ladder unchanged at **54/87**, genwasm green,
`doc --std --verify` clean, `vyrn-lsp` builds. No `.vyrn` file changed, so
`fmt --check` is untouched. No engine, no shim and no std module was edited: the
whole milestone is a test, a status line and this note, which is the correct
footprint for a milestone whose product is a statement about the code rather than
a change to it.
