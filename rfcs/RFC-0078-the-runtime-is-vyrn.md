# RFC-0078 — The Runtime Is Vyrn

- **Status:** Draft
- **Depends on:** RFC-0014 (the I/O builtins and their canonical wording),
  RFC-0018 / RFC-0059 (`fromJson`/`toJson`, and `std/json`, which already
  implements JSON *in Vyrn*), RFC-0077 (the direct wasm backend, which is what
  made the duplication impossible to keep ignoring)
- **Evidence (counted, this repo):**

  | | |
  |---|---|
  | builtins the checker knows | 40 |
  | Rust implementations in the interpreter | 50 |
  | C functions in the runtime shim | 80 — now 74, M2b having deleted six (see "M2b, as landed") |
  | of those, the JSON DOM alone | 49 — but SHARED writer/reader, so M2 retired SIX (not 11: the parser's unescaper shares the buffer) and M3 takes **32** (not 38 — see "M3, as measured", which also states its counting method, since this row has now been wrong three times) |
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
- **M3 — `fromJson`. BLOCKED ON M1, AND SEQUENCED AFTER M4 — see "M3, as
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
  M4b — `chars`, `stringFromBytes`, and the `%f` direction — is untouched.
- **M5 — the interpreter's fast paths become caches.** Each Rust arm either
  delegates to the Vyrn definition or is documented as an optimization that
  parity proves equivalent. The count of Rust arms should fall.

## Acceptance

- Parity green throughout — every milestone is byte-identical on stdout, stderr
  and exit code, or it does not land.
- The shim's function count falls, measurably, per milestone. It was 80 when this
  was written; M2b took it to 74, and took eleven `declare` lines with it.
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
