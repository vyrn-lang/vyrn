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
  | of those, the JSON DOM alone | 49 |
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
and call it **retires 49 of the shim's 80 functions** and hands the direct
backend `toJson` for free, because `std/json` is ordinary Vyrn that every
backend already compiles.

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
