# RFC-0089 — Mutable Value Semantics

- **Status:** Proposed. Supersedes RFC-0088 if accepted; RFC-0088's M1
  (make it visible) survives unchanged as this RFC's M0.
- **Depends on:** RFC-0004 §1 (the capability surface — this RFC finishes it),
  RFC-0086 (the compiler asks the type), RFC-0087 (the census)
- **Premise:** backwards compatibility is not required. The corpus migrates.

---

## The question this answers

RFC-0088 keeps today's semantics and extends the ownership *analysis* until it
covers more cases. That is the compatible answer. This RFC is the answer when
compatibility is off the table:

> **Stop inferring ownership. Define it.**

Today a Vyrn program has no memory semantics. It has a best-effort analysis
(`own.rs`) that frees what it can prove and silently leaks the rest. "Safe leak"
appears fourteen times in the census because it is the *defined fallback* of the
model. No amount of extending the analysis removes the fallback — a cleverer
guesser is still a guesser, and its failure mode is still silent.

The alternative: every heap value has exactly one owning place, established by
the language definition rather than discovered by analysis. A program that would
leak does not compile with a leak. It either compiles with known reclamation or
it errors with a named fix.

---

## The design, in five rules

### 1. Values are values

A type that transitively owns heap **moves** on assignment, argument passing and
return (RFC-0088's rule, kept). Everything else copies, as today. `let t = s` on
a `String` moves; `print(s)` after it is a compile error naming the move.

### 2. The capabilities are the calling conventions

RFC-0004 §1 already wrote the table. It becomes enforced, not surface:

| convention | today | becomes |
|---|---|---|
| `read` (default) | nothing | callee may observe; **may not retain, store or return** the value |
| `modify` | nothing | exclusive in-place access; same no-retain rule |
| `consume` | move-checked | sink — ownership transfers in, callee releases or forwards |
| `share` | nothing | `read` across a `spawn` boundary, as RFC-0004 intended |

`read` and `modify` are **second-class**: a borrowed value cannot be stored in a
field, captured by an escaping closure, put in a container, or returned. That
one restriction is what makes the whole thing work without lifetime annotations
— a borrow that cannot escape needs no lifetime, because its lifetime is the
call. This is the published result Vyrn's design has been circling: mutable
value semantics gives whole-program memory safety with zero annotation beyond
the conventions themselves.

### 3. A function returns an owned value. Always.

No borrowed returns. `fn id(s: String) -> String { return s }` must take
`consume s` (and forward ownership) or return `s.copy()`. The compiler says
which two fixes exist.

### 4. A place owns its contents

RFC-0088 M5, kept: storing into a place releases the old contents; releasing an
aggregate releases its places; `drop` is deep.

### 5. `Ref<T>` is the one runtime mechanism, and the only one

Aliasing that must be stored goes through `Ref<T>`, exactly as RFC-0004 §5.2
decided. Nothing else in the language has a runtime check. The model is
**completely compile-time except where the program explicitly wrote `cell`** —
and §5.3's measurement says the corpus already writes `cell` only where aliasing
is real.

---

## Why this is meaningfully better, not differently shaped

**`own.rs` is deleted, not extended.** The escape walker, the safe-read list,
the `owned_fns` fixpoint, `transfers`, `expr_type` — all of it exists to *infer*
what rules 2 and 3 now *declare*. What replaces it is a checker: signatures in,
diagnostics out, no fixpoint, no under-approximation, no leak-when-unsure.
Checking a declaration is smaller and stronger than inferring its absence. This
is RFC-0086's thesis taken to its end: the compiler asks the signature.

**The census, re-run against the five rules:**

| gap | outcome |
|---|---|
| §2a six forms never transfer | gone — transfer is rule 1, not an expression list |
| §2b `builtin_producers` hand list | gone — builtins get signatures like everything else |
| §2c `mut` String untracked | gone — rule 4 |
| §3/§14/§15/§16/U4 aggregates leak payloads | gone — rule 4 |
| §4/P1 overwrite leaks, 12.2 GB accumulator | gone — rule 4; and with no aliasing, `s = s + x` reallocs in place **everywhere**, including module state |
| §7 linearity hardcoded to `Stream` | gone — a `Stream` owns heap, so it moves; RFC-0086 M3 stops existing |
| §9a/§9b/U10 the extern boundary | gone by fiat — rule 3 means a returned String is always the caller's to free; a stored parameter is refused by rule 2 |
| U2 no way to copy | `copy` ships as the named fix in every move diagnostic |
| U7 `consume` and linearity are two things | one thing — rule 2 |
| U8 a declared container cannot be read | gone — `read self` is the receiver convention, and the safe-read list dies with `own.rs` |
| §6/U5 use-after-release traps at runtime | confined to explicit `Ref`, the declared aliasing tool |
| P2 String scans | String carries `{len, cap}` — see "M1a as landed" below |

Three undesigned items in RFC-0087 (inferred regions, region borrowing, the
compile-time story for aliasing) shrink to one: `Ref<T>` remains the runtime
island, by choice, with the trap as its documented failure mode.

**And the model becomes teachable.** Today the answer to "is this freed?" is
"depends what the analysis proved." Under the five rules the answer is a
sentence: *you own what you hold; calls borrow unless marked `consume`; returns
are yours; use `copy` to keep both; use `cell` to share.* U1's printer is still
worth building (M0), but the thing it prints becomes a rule, not a report.

---

## What it costs, stated plainly

- **Move errors are the new error surface.** RFC-0004 §2's worry. Contained by
  the same line as RFC-0088: scalars, records of scalars, `Option<Int64>`,
  `Ref<T>` copy freely — the surface only exists where heap is owned. The
  measured alternative is the 12.2 GB cliff with no diagnostic.
- **The corpus migrates.** ~124 examples, `std/`, the generators. Mechanical in
  the common case (`return s` → `consume` or `.copy()`); this is the price of
  no-compat and it is paid once.
- **Escaping closures capture by move or copy, declared at the capture.**
  RFC-0037's stored closures cannot hold borrows; non-escaping lambdas
  (`map`/`filter` arguments) borrow freely. The checker distinguishes them the
  way `own.rs` already does — except now it refuses instead of leaking.
- **Generics:** whether `T` moves is known at monomorphization; the convention
  checker runs per instance (Vyrn already monomorphizes everything). The one
  RFC-0088 open question closes.
- **`movecheck` gets types.** Unavoidable in any variant of this work; it is
  the bulk of M2.

---

## Milestones

- **M0 = RFC-0088 M1.** The instrument: `vyrn why --memory`, memory tests over
  the census shapes, alloc/concat/`get` benchmarks. Lands before any semantic
  change, and its counts (how many corpus sites move, how many need `copy`)
  are the go/no-go evidence for M2.
- **M1 — `copy`, and String as a value triple.** Both backends and the
  interpreter. P1 and P2 die here even before the conventions land.
- **M2 — the convention checker.** `movecheck` grows types and enforces rules
  1–3. `own.rs` shrinks to the `Owned`-table lookup (RFC-0086 M1's part) plus
  drop-site emission. The corpus migrates in the same change, module by module.
- **M3 — places (RFC-0088 M5) and the boundary (M6).** Store-releases,
  deep drop, export ABI ownership by rule 3.

---

## M1a as landed — the String header

This RFC sketched `String` as a value triple `{ptr, len, cap}`, "like `Array`
already is". Implementation measured that and chose differently: the two words
sit **behind** the pointer, not beside it. A `String` is still one word, and it
still addresses NUL-terminated UTF-8, so the extern ABI and every C sink are
unchanged. The sixteen bytes in front of it (eight on wasm32 — two
pointer-sized words on each target) hold `len` and `cap`.

Three measurements decided it.

1. **An `Option<String>` payload is one word.** A three-word String does not
   fit, so `Some(s)` would box — one `malloc` per construction. That moves the
   `optionString` row of the census baseline (`compiler/vyrn-cli/tests/memory.rs`
   §14), which Phase 2 was required not to move. `Result`, user enum payloads,
   `Map<String, V>` keys and `Array<String>` elements all carry the same
   assumption, and a triple widens every one of them.
2. **A triple goes stale under aliasing; a header cannot.** The RFC's argument
   for the triple was that rule 1 forbids the aliasing that would break it. Rule
   1 is M2. Until it lands, `let t = s` is legal, and two triples would hold two
   lengths for one buffer. Two pointers to one header hold one length.
3. **The extern boundary needs no conversion at all.** A `String` crosses to JS
   as a `ptr` (RFC-0012 M2) and still does. `wasi-min.js` writes the header when
   it allocates and subtracts it when it frees; nothing else in the language
   sees the boundary.

`cap == 0` means static: a literal in the data segment, never `realloc`'d, never
freed. That is the fact RFC-0077 M6 said a drop site could not recover, and it
is now a field rather than an inference.

What P2 asked for is delivered: `byteLength` is a load, `a + b` reads two
lengths instead of scanning two operands, and `str_append`'s shadow `(len, cap)`
is gone — the header IS that pair. One word of shadow survives per accumulator,
holding a question the header cannot answer: did this path allocate the buffer?
That word is ownership, which is M2's subject, and it retires with M2.

Measured on the M0 benchmarks (`examples/membench.vyrn`, medians):

| row | before | after |
|---|---|---|
| `byteLength` of 10 KB × 500 | 149.08 µs | 5.14 µs |
| string concat, 1000 fresh buffers | 29.14 µs | 18.99 µs |
| string append spine, 1000 appends | 6.13 µs | 4.54 µs |
| array push churn, 1000 pushes | 540 ns | 541 ns |

The cost is code and data size on wasm: `fib.wasm` grew from 1,334 to 1,590
bytes, and `domdemo.wasm` from 25,966 to 27,630.

## Rejected

- **RFC-0088 alone** — keeps the silent-leak fallback as defined behavior;
  every future language feature must remember to extend the analysis or it
  leaks. The census *is* the record of how that goes.
- **Full region inference (MLKit/Tofte-Talpin)** — regions in every type,
  written or inferred; priced earlier and the price is the annotation surface
  Vyrn refuses, plus module-state values have no region.
- **Linear-everything (Austral)** — sound, and the annotation burden lands on
  the 90% case instead of the 10%.
- **Refcounting the aggregates** — runtime cost on the common path, and more
  invisible, which is the wrong direction for the whole of Part II.
