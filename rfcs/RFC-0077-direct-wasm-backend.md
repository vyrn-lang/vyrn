# RFC-0077 — A Direct Wasm Backend: Stop Going Through LLVM

- **Status:** Draft
- **Depends on:** RFC-0076 (generators as wasm; the shared runtime shim and the
  memory map it established), RFC-0012 (the `extern` ABI), RFC-0037
  (defunctionalized closures — the reason no function table is needed)
- **Evidence (measured, this repo):** compiling generators, cold, `examples/bin`:

  | phase | time |
  |---|---|
  | clang (6 artifacts) | **1,974 ms** |
  | cranelift | 1,129 ms → 250 ms once `parallel-compilation` was turned on |
  | text-IR parsing alone, one 2.87 MB module | 102 ms |
  | IR → wasm isel | 570 ms |
  | wasm-ld link | 67 ms |

  Essentially **all** of the clang half is work a direct emitter would not do.

---

## The problem

`vyrn build --target wasm` and RFC-0076's generation engine both take the same
route: Vyrn AST → textual LLVM IR → clang → wasm. That was the right call when
the wasm target was new — the textual backend already existed for native, and
reusing it meant one emitter instead of two.

It has two costs, and only one of them is speed.

**The dependency.** `find_clang()` returning `None` makes the generation engine
decline, so a `.vyx` keystroke is 54 ms or 250 ms depending on whether someone
installed a C toolchain. Same compiler, same source, same semantics, four-fold
difference in behaviour from the environment. A language should not have that
shape.

**The double compile.** We run two compilers back to back and throw the first
one's output away — the wasm bytes go straight into cranelift and are never read
again except as a cache blob. And clang is given **no `-O` flag at all** on this
path, so it is not even optimizing; it is a very expensive translator.

## The change

Emit wasm directly from the same traversal that emits LLVM IR today, and delete
the LLVM path for `--target wasm`. Native keeps its textual-IR route to clang
unchanged.

Nothing about the language changes.

## Why this is safe HERE

The same reason RFC-0076 was safe. The sacred invariant is **interp == native ==
wasm, byte-identical including traps**, asserted over every example on every
commit. A new wasm backend is exactly the kind of change that invariant was built
to catch: it either produces the same observable behaviour as the interpreter and
the native build, or parity goes red.

That is also why this RFC *replaces* rather than *adds*. A second wasm backend
kept beside the first is ungated by construction — and this repo has already run
that experiment. `vyrn-codegen-llvm` was a second native backend: three commits
ever, twenty-five commits of language work landed past it, and it rotted to
unbuildable in twelve days without anyone noticing (deleted in `b1eef04`). The
three *engines* did not drift over the same period, because parity checks them
for about 58 seconds a run. **Gated multiplicity costs seconds and stays true;
ungated multiplicity looks free and becomes a lie in the repo.**

## What makes this tractable

A spike measured the shape of the job rather than guessing at it:

- **No relooper.** Control flow is generated straight from `if`/`while`/`for`/
  `match` AST nodes, and `break`/`continue` target only the innermost loop —
  which is literally a wasm `br <depth>`. Measured: **zero phis in any loop
  header**. Of 843 phis in a real artifact, 812 are single-join diamond merges
  (`&&`, `||`, if-expressions, `match`) that become a `block (result T)` or one
  scratch local; the remaining 31 are in the hand-written IR prelude, not
  emitter output.
- **No function table.** Zero indirect calls and zero function-addresses-as-values
  in the artifacts checked — RFC-0037's defunctionalization holds, so stored `fn`
  values already go through a synthesized `switch` into direct calls.
- **No generics.** Monomorphization runs before any instruction is emitted.
- **A small, fixed boundary.** 58 external symbols in the largest real artifact:
  48 `__vyrn_*`, 9 libc, 1 intrinsic.
- **The traversal is reused.** Monomorphization, defunctionalization, drop
  insertion and pattern lowering all stay; the work is the `format!` tail of
  ~969 emit sites.

Estimated 3,500–5,500 lines against the 9,465 that are emitter today.

## The ABI decision

The one part with real design risk is aggregates: 367 of 526 functions take or
return LLVM value aggregates, with 5,450 `extractvalue`/`insertvalue` and nesting
19 deep.

**The simplification: only the boundary has to match C.** Vyrn-internal calls can
use whatever ABI we choose, so aggregates go through a shadow stack uniformly —
always correct, boring, optimizable later. The C wasm ABI question then shrinks
to 58 known signatures, and only those types need to agree with clang's layout.

Note some aggregates genuinely do cross: `__vyrn_args()` returns the
`{ptr, i64, i64}` growable-array triple. So the layout engine's answers must
match clang's for boundary types, and that is checkable directly — emit a C
program printing `sizeof`/`offsetof` for each and compare.

## The memory map

RFC-0076's shared shim already established one, and a directly-emitted module
must fit it: the shim's data and heap live above 16 MB with its stack growing
down just below that base, while the generated module keeps its stack at address
zero so overflow traps instead of eating the shim's frames. A direct emitter owns
its own `__stack_pointer` global and data placement, and inherits the same two
invariants — statics below the halfway mark, no import the shim does not export.

## Milestones

- **M0 — layout and ABI.** A layout engine (size, align, field offset) for every
  Vyrn type, verified against clang for boundary types; the shadow-stack
  aggregate convention written down; the memory map for a directly-emitted
  module. No emitter yet. **This is the kill point** — if layout does not
  reproduce clang's offsets, or the aggregate convention does not fall out
  cleanly, the RFC stops here having cost one milestone.
- **M1 — module encoder.** Sections, functions, locals, memory, globals,
  imports/exports, via `wasm-encoder`. Validated by `Module::new`.
- **M2 — lowering.** The emit sites. Structured control flow straight from the
  AST. Before starting, re-verify the "no phis in loop headers, no indirect
  calls" measurement across every example rather than the two sampled.
- **M3 — varargs.** 377 `printf`-family sites, and wasm has none; fixed-arity
  shim wrappers generated from the call shapes the emitter actually sees.
- **M4 — the prelude.** The 1,080-line hand-written IR prelude moves into the C
  shim, which RFC-0076 M6 already compiles once.
- **M5 — delete the LLVM wasm path.** Not before, and not left behind a flag.

## Acceptance

- Parity green — interp == native == wasm, byte-identical including traps — with
  the wasm column produced by the direct backend, over every example.
- The RFC-0076 cross-engine gate green: every generator emits byte-identical
  source under both engines.
- `vyrn build --target wasm` needs no clang, no wasi sysroot, no builtins
  archive. The generation engine stops declining on a machine without a C
  toolchain.
- `VYRN_WASM_BACKEND` does not survive M5.

## Risks, honestly

**A transition period with two wasm backends.** Mitigated by running parity
twice during it — once per backend — which roughly doubles one CI job for the
duration, and by M5 being a deletion rather than a default flip.

**A wrong layout is a silent miscompile**, not a link error. This is why M0 is
verified against clang directly rather than by inspection, and why boundary types
get their own test.

**The shadow stack's "never address-taken" analysis** is the other
silent-miscompile class — 3,085 allocas today. Being conservative costs stack
traffic, which is measurable; being wrong costs correctness, which is not
recoverable. Bias hard toward the stack.

**Scope creep into native.** This RFC does not touch the native path. The
textual-IR backend stays exactly as it is, and keeps its own parity column.

---

## M0, as landed

The kill point passed. Nothing here was fatal, and two of this RFC's own
assumptions turned out to be wrong in the same direction — the job is smaller
than it was written up as.

Everything below was measured over the emitted IR of all 81 examples
(`vyrn emit-ir`), not over the two artifacts the original spike sampled.

### The layout engine

`vyrn-codegen/src/layout.rs`. It takes the LLVM type **string** rather than a
`Type`, because there is exactly one function in the crate that maps a Vyrn type
to a shape — `llt`, plus its helpers `enum_ll`/`sa_ll` — and consuming what that
prints means layout cannot drift from lowering. Adding a case to `llt` changes
what the layout engine sees, automatically. A second match on `Type` would have
been two sources of truth for one fact, which is the mistake this RFC exists to
avoid making twice. The type → layout path is `llt` ∘ `of_ll`.

Two things fall out of that choice. A string is finite, so a record type that
somehow referred to itself could not hang the engine — it could not have been
printed. And `llt`'s output is what the emitter reads for allocas and GEPs
anyway, so nothing is parsed that was not going to be produced.

`layout::SHAPES` is the shape universe — every string `llt` can print, at the
element widths that exercise the padding cases. A test in `vyrn-codegen`
asserts those strings are the ones `llt` actually prints, so the list cannot rot
into a lie about what is covered.

### Verified against clang: no disagreements

`vyrn-codegen/tests/layout_vs_clang.rs`. Every shape is transcribed
mechanically to a C struct (`ptr` → `void*`, `i64` → `long long`, `i1` →
`_Bool`, `[N x T]` → `T m[N]`, built around the declarator so `[2 x ptr]` comes
out `void* x[2]`), compiled with `--target=wasm32-wasip1` and the sysroot
`vyrn build --target wasm` uses, run under wasmtime, and diffed.

**106 numbers over 28 shapes, zero disagreements.** The transcription is
generated rather than hand-written, because a hand-written C struct is a second
chance to make the same mistake in both places; a separate test checks the
transcription itself. Skips loudly when clang or wasmtime is absent, same
posture as the parity harness.

The one that mattered: `{ ptr, i64, i64 }` — the growable-array triple — is 24
bytes with a 4-byte hole after the pointer, not 20, because wasm32's data layout
says `i64:64`. That is the whole reason this was checked rather than reasoned
about.

### The boundary is not what this RFC said it was

> Note some aggregates genuinely do cross: `__vyrn_args()` returns the
> `{ptr, i64, i64}` growable-array triple.

It does not cross. `@__vyrn_args` is a `define` in the hand-written IO prelude,
not a `declare` — it is Vyrn-internal code that happens to be written in IR.
Measured over all 81 examples:

| | count |
|---|---|
| `declare`s with an aggregate anywhere in the signature | **0** |
| `byval` / `sret` attributes | **0** |
| distinct external symbols (union; 75 in the largest single example) | 75 |

So **no aggregate crosses the C boundary by value, at all.** The C wasm ABI
question does not shrink to 58 signatures; it disappears. Every external
signature is scalars and pointers.

What does have to agree with C is one struct, reached through a pointer: the
emitted code builds a `Map<String, V>` and the shim's `__vyrn_map_reserve` /
`__vyrn_map_remove_at` grow it in place through a `VMap*`. That agreement is
now the last case in the clang test, checked against what `llt` gives a `Map`.

One real mismatch to carry into M2, found while enumerating: the emitter
declares `ptr @__vyrn_vj_bool(i1)` while the shim defines
`VJ* __vyrn_vj_bool(int)`. LLVM reconciles that silently today by widening `i1`
to `i32` in the wasm ABI. A direct emitter has to widen it itself — the boundary
takes `i32`, and `i1` is an LLVM fiction that does not exist in wasm. (The other
`i1` on a `declare` is `llvm.memcpy`'s volatile flag, which becomes
`memory.copy` and vanishes.)

### The aggregate ABI

Confirmed, with one correction to how it was described.

Shadow stack: a region of linear memory addressed by a mutable `i32` global,
`__stack_pointer`. Prologue subtracts the frame size, epilogue restores. Every
aggregate lives in a frame slot; on the wasm value stack an aggregate is always
an `i32` address, never a value. Scalars stay in wasm locals.

It covers everything the emitter produces, and mostly because the emitter is
already written this way:

- **Frames are statically sized.** 17,270 allocas across the examples, **zero**
  of them dynamically sized. Every frame size is a compile-time constant.
- **The frame already exists.** `Gen::allocas` is the entry-block alloca list;
  the direct emitter assigns each entry an offset via `of_ll` instead of a name.
  This is not new machinery, it is the same list with arithmetic done to it.
- **By-value parameter semantics come free.** Every parameter is *already*
  copied into a fresh alloca slot in the function prologue
  (`lib.rs`, "store each incoming param into a fresh alloca slot"). Passing an
  aggregate argument by address turns that existing store into a `memory.copy`
  from the caller's slot — the copy that keeps by-value semantics honest was
  always there, so the convention costs nothing it was not already paying.
- **`modify` parameters are already by-pointer**, with explicit copy-in and
  copy-out (`Capability::Modify`). The convention is in the emitter today.
- **Aggregate returns**: caller allocates the slot in its own frame and passes
  the address as a hidden leading `i32`; the wasm function returns nothing. 2,135
  of 6,556 emitted functions return an aggregate, matching the spike's 367/526
  ratio on a bigger corpus.

The correction is where the risk sits. It is not calls — it is **joins**. wasm
has no aggregate values, so `phi` of an aggregate has no representation, and
there are **149** of them (`{ i1, i64, i64 }` 88, `{ ptr, i64, i64 }` 22, the
rest nested records up to 4 deep in this corpus, 19 in the spike's generator
artifact). The convention that covers them is *destination-first*: allocate the
join's slot before the branch, and have each arm store into that slot rather
than produce a value. That is a shape constraint on the M2 traversal, and it is
the thing to design for rather than discover.

The case that would have been genuinely awkward — `select` on an aggregate,
which has no branch to hang the stores on — **does not occur** (0 measured).
Neither do indirect calls (0), confirming RFC-0037 still holds.

### The memory map

Measured, not assumed: a C program printing `&local`, `&global` and `malloc()`
under each link's flags, run under wasmtime.

```
addr 0          ┐ generated module's shadow stack, 64 KB, growing DOWN from
                │ 65,536 — a push past 0 wraps to 0xFFFFFFF8 and traps
65,536          ┤ generated module's data segments
   … must end below 8 MB (SHIM_BASE / 2) …
16,777,216      ┤ shim's __stack_pointer; shim frames grow DOWN from here
16,777,216 +    ┘ shim's data, then the single shared malloc heap
```

Confirmed by measurement: `wasm32-wasip1` links `--stack-first` by default
(stack at 65,528 with or without the explicit flag), and the shim's
`--global-base=16M -z stack-size=16M` puts its stack at 16,777,208 with data
above. The gap between 64 KB of generated statics and 16 MB is what keeps the
shim's downward-growing frames from reaching them — it would have to recurse
through 8 MB of C to get there.

A directly-emitted module reproduces exactly this: a mutable `i32` global
`__stack_pointer` initialized to 65,536, data segments placed from 65,536 up.
Nothing about the map has to change, which was the point of checking.

Both RFC-0076 invariants get *easier*, not harder:

- **Statics below the halfway mark.** `compile_split` reads `data_end` back out
  of the linked bytes today. A direct emitter chose those offsets, so it knows
  the number before it writes it.
- **No import the shim does not export.** Today this is also read back out of
  the bytes, and has to be, because `--import-undefined` turns every symbol the
  module got wrong into a plausible-looking import. A direct emitter builds the
  import section itself: the check moves from forensics to construction.

Address 0 is inside the generated module's stack, so a null pointer is a
valid address there. Nothing relies on it not being — the one null sentinel in
the runtime (`@__vyrn_bytes_dup`) is compared, never dereferenced — but M2 must
not introduce a null check that assumes a trap.

### What M1 inherits

`layout::of_ll` and `layout::SHAPES`, a clang comparison that will catch a
layout drift years from now, and three facts worth not re-deriving: frames are
statically sized, the aggregate convention is already half-implemented in the
prologue, and the memory map needs no negotiation. The one design constraint to
carry forward is destination-first lowering at joins.
