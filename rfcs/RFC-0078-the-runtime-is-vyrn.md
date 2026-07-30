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
  | C functions in the runtime shim | 80 |
  | of those, the JSON DOM alone | 49 — but SHARED writer/reader, so M2 retires 11 and M3 the other 38 (see "M2, as landed") |
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
  implementation already written and parity-tested. Success is 49 shim functions
  deleted and `toJson` working on the direct backend without a line of new wasm
  lowering. Bytes must be pinned before the swap, not compared after.
  **Blocked, measured, not attempted — see "M2, as landed" and "M2a, as landed".
  The library prerequisite is DONE: `std/json` is now the tree plus the writer,
  importing nothing, and the reader lives in `std/jsonread`. What remains is three
  compiler mechanisms — an injected import with `foreign_renames` seeded to
  reserved spellings, synthesized per-type encoder functions in the linked
  `Program`, and the type-directed walk as a shared AST builder — plus two escape
  literals repinned. Number formatting is NOT among them (measured: `toString()`
  already matches the shim byte for byte).**
- **M3 — `fromJson`.** The parser half, same shape, plus RFC-0018's `Issue`
  accumulation.
- **M4 — the string and number tier.** `chars` (UTF-8 decode), `parse`, the `%f`
  formatter `f64_str` (~300 lines of emitted wasm that should be ~40 lines of
  Vyrn), `stringFromBytes`.
- **M5 — the interpreter's fast paths become caches.** Each Rust arm either
  delegates to the Vyrn definition or is documented as an optimization that
  parity proves equivalent. The count of Rust arms should fall.

## Acceptance

- Parity green throughout — every milestone is byte-identical on stdout, stderr
  and exit code, or it does not land.
- The shim's function count falls, measurably, per milestone. It is 80 today.
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
