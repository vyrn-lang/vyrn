# RFC-0077 — A Direct Wasm Backend: Stop Going Through LLVM

- **Status:** IMPLEMENTED (M0–M2p, M5; M3 and M4 struck — see their lines below)
- **Depends on:** RFC-0076 (generators as wasm; the shared runtime shim and the
  memory map it established), RFC-0012 (the `extern` ABI), RFC-0037
  (defunctionalized closures — the reason *closures* need no function table),
  RFC-0025 (`spawn`, which the M2a pre-flight read as the reason a small one is
  needed anyway — and M2m found it is not; see "M2m, as landed")
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

> **No longer true, and it took until RFC-0076 M7.** M5 below deleted the LLVM path
> for `vyrn build --target wasm` and closed by saying "the generation engine no
> longer declines on a machine without a C toolchain" — which was not true of
> anything M5 changed. `compile_to_wasm` was still calling `emit_gen_host` and
> shelling out, and with a poisoned clang the keystroke measured **312 ms** against
> 72 ms, i.e. exactly the four-fold difference this paragraph names, three
> milestones after it was declared gone. RFC-0076 M7 pointed the generation engine
> at this backend and deleted the textual gen-host emitter: the keystroke with no C
> toolchain is now **92 ms**, and a cold `.vyx` `didOpen` went from 2,733 ms to
> 804 ms because 1,665 ms of clang per `emit-gen` became 50 ms of emission. Sixty
> lines of import declarations were what stood between the two.

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
- **No function table** — *wrong, and the M2a pre-flight caught it.* Indirect
  calls really are zero, and RFC-0037's defunctionalization really does route
  stored `fn` values through a synthesized `switch` into direct calls. But
  function-addresses-as-values is **9**, not 0: RFC-0025's `spawn` hands the shim
  a thunk symbol (`call @__vyrn_spawn(ptr @__vyrn_task_*, ptr)`) and the shim
  calls it, which in wasm needs a table element, `ref.func` and `call_indirect`.
  Bounded and enumerable — 9 sites across 3 examples, all syntactic — but a
  milestone that had assumed this away would have discovered it at the end. M2m
  lowered both function-value features and added none: it is still only `spawn`.

- **No function table** — right after all, but for a reason neither this line nor
  the pre-flight that "corrected" it had. Indirect calls really are zero, and
  RFC-0037's defunctionalization really does route stored `fn` values through a
  synthesized `switch` into direct calls. Function-addresses-as-values is **9**,
  and the M2a pre-flight concluded those 9 needed a table element, `ref.func` and
  `call_indirect`. They do not: all nine are `spawn`, and on wasm the shim
  IMMEDIATELY calls the pointer it was handed, because wasm has no threads. A
  backend that emits the eager path itself never forms a pointer. See "M2m, as
  landed" — the finished module has no table section and no element section.
- **No generics** — *wrong, and M2e is where it was corrected.* Monomorphization
  does not run before any instruction is emitted, in either backend: a
  specialization is **discovered** at a call site as a side effect of emitting
  the body containing it (`Gen::instantiations`, drained by the driver). So a
  direct backend cannot consume monomorphized code; it needs the same
  interleaved shape, and it needs one more thing the textual one does not,
  because a wasm call names a function *index* rather than a symbol. See
  "M2e, as landed" — and "M2m, as landed", where the second (higher-order)
  worklist arrived and turned out to be the same one, while RFC-0037's dispatchers
  turned the index discipline into an encoder reservation.
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
  imports/exports, via `wasm-encoder`. Validated by RUNNING the output under a
  `wasmtime` binary — not by `wasmtime::Module::new`, as an earlier draft of
  this line said: wasmtime lives in the excluded `vyrn-genwasm` precisely so
  `vyrn-codegen` builds with no LLVM, clang or sysroot, and M0 already
  established shelling out via `VYRN_WASMTIME` as the posture.
- **M2 — lowering.** The emit sites. Structured control flow straight from the
  AST. Before starting, re-verify the "no phis in loop headers, no indirect
  calls" measurement across every example rather than the two sampled.
- **M3 — varargs. STRUCK; see "M2j, as landed".** The premise was 377
  `printf`-family sites against a wasm target that has no varargs. Every one of
  them is the *textual* emitter's, and M5 deletes that path for wasm; the direct
  backend emits zero, in either link shape, because it cannot import a variadic
  function and so was never able to plan on one. There are no call shapes to
  generate wrappers from.
- **M4 — the prelude. STRUCK; see "M5, as landed".** "The 1,080-line hand-written
  IR prelude moves into the C shim" was the plan for a backend that reached the
  shim. This one does not: M2b through M2h emitted a runtime of its own, M2i
  measured the split making a module *larger* rather than smaller, and M2p's sweep
  left the prelude's cost at 290 bytes of code in `fib.wasm`. Moving it into C
  would reintroduce the clang dependency the criterion below forbids, to make
  modules bigger. There is nothing left of the milestone to do.
- **M5 — delete the LLVM wasm path.** Not before, and not left behind a flag.
  Landed; see "M5, as landed".

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

### What M1 inherited

`layout::of_ll` and `layout::SHAPES`, a clang comparison that will catch a
layout drift years from now, and three facts worth not re-deriving: frames are
statically sized, the aggregate convention is already half-implemented in the
prologue, and the memory map needs no negotiation. The one design constraint to
carry forward is destination-first lowering at joins.

---

## M1, as landed

`vyrn-codegen/src/wasm.rs`. Sections, imports, exports, the memory, the
`__stack_pointer` global, the data pool, and the shadow-stack prologue and
epilogue around a body someone else emits. No lowering — that is M2, and the
bodies here are the three-instruction kind whose only job is to prove the frame
is real.

Two of the encoder's decisions are constraints rather than choices, so they live
in the type rather than in a convention:

- **Section order is framed once.** wasm fixes the order of its sections and
  `wasm-encoder` will emit them in whatever order it is called in, so the
  sections are accumulated as fields and written out in `finish` — one list, in
  one place, instead of an ordering that emerges from the traversal. A unit test
  reads the ids back off the finished bytes and asserts they ascend.
- **Imports must precede definitions**, because they share one function index
  space. `Module::import` after `Module::func` panics with that sentence, which
  is cheaper than debugging an off-by-N in every call in the module.

### The import audit: one mismatch, and it is the only one

`vyrn-codegen/tests/imports_vs_shim.rs` walks both sides and diffs them — the
`declare` lines the emitter prints, mapped through `wasm::abi`, against the C
definitions parsed out of `RUNTIME_SHIM`, mapped through the C ABI for wasm32.
Neither side is transcribed, for the reason M0's clang test gives.

**68 signatures agree with the definitions they call.** Three are variadic
(`printf`, `fprintf`, `__vyrn_snprintf`) and are M3's milestone, not a
mismatch. `llvm.memcpy` is an intrinsic, not an import — it becomes
`memory.copy`. Every declared `__vyrn_*` resolved to a definition; a dangling one
would have failed the test rather than become a plausible-looking import.

The only mismatch is the one M0 found, and the sweep proves it is the only one:
`__vyrn_vj_bool` is the sole `declare` with a sub-`i32` parameter anywhere in the
boundary, so `abi`'s widening has exactly one site to be right about. A second
test pins that count, because the sweep passes whether there is one widening or
fifty.

The class of bug worth naming, since it did not occur: `size_t` is 4 bytes on
wasm32 and 8 natively, so a `declare` handing `i64` to a libc function taking one
would be wrong on exactly one target. The three that would have — `strlen`,
`strncmp`, `snprintf` — are already wrapped as `__vyrn_*` so the IR can stay
64-bit. The libc expectations are written as C in that test and mapped through
the same function the shim's definitions go through, so the two sides cannot
disagree about what `int` means.

The `vyrn_gen` namespace (RFC-0076's nine `CODE_IMPORTS`) is not in the sweep:
its ground truth is `func_wrap` in the excluded `vyrn-genwasm`, which this crate
cannot see. Checked by hand — all nine agree — and the cross-engine gate already
covers them behaviourally.

### What ran under wasmtime

`vyrn-codegen/tests/wasm_runs.rs`. The RFC said M1 would be "validated by
`Module::new`", which is wrong twice over: that is `wasmtime::Module::new`, and
wasmtime lives in the excluded `vyrn-genwasm` because keeping `vyrn-codegen`
buildable with no LLVM, no clang and no wasi sysroot is the property the
workspace defends. So M1 does what M0 did — shells out to a `wasmtime` binary,
skips loudly without one.

Running is the better check anyway. Validation says the sections are well
formed; a module that runs and prints the right bytes says the section order,
the memory map, the stack pointer's initial value and the prologue/epilogue pair
are *simultaneously* correct.

1. `_start` calls an imported `proc_exit(7)`, exits 7.
2. A string in the data segment, an iovec built in the shadow-stack frame,
   `fd_write` to stdout — data placement, frame and import in one assertion. A
   wrong data address prints garbage; a wrong frame overwrites the string.
3. **The round trip M0 could only make on paper.** One function writes an
   `{ ptr, i64, i64 }` into a frame slot at the offsets `of_ll` computed; a
   *different* function reads them back at those offsets, re-packs them
   contiguously, and the raw bytes go to stdout. Two functions disagreeing about
   an offset is the silent miscompile this RFC keeps warning about, made loud —
   and the 4-byte hole after the pointer is in the output, still zero, which is
   the number clang gave us in M0.
4. A frame larger than the 64 KB below `STACK_TOP` underflows past 0, wraps to
   near `0xFFFFFFFF`, and traps — `--stack-first`'s safety property asserted
   rather than assumed.

### Nothing contradicted the memory map

`STACK_TOP` = `DATA_BASE` = 65,536 and `STATICS_LIMIT` = 8 MB went in as
measured. Two things it was simply silent about, decided here:

- **Frames round to 16 bytes.** clang keeps the wasm32 stack pointer
  16-aligned; matching costs nothing and means a frame base is always aligned
  for the widest thing `layout` can put in it.
- **`data_end` bounds the memory's minimum size**, one page past everything
  static. `compile_split` reads that number back out of linked bytes today; the
  encoder returns it from `Module::data_end` before writing anything, which is
  the "forensics to construction" move M0 predicted, now real.

`toolchain::find_wasmtime_from` also landed, because the wasmtime lookup was
about to have a third copy.

### What M2 inherits

`wasm::Module`, `wasm::abi`, and one constraint that is easy to violate and
silent when violated: **a body must not emit `return`**, because it would jump
past the epilogue and leak the frame for the rest of the program. Returns go
through a `br` to the function's outermost block, or the epilogue moves into a
helper the return path calls. That sits beside M0's destination-first rule at
joins as the second shape constraint on the M2 traversal.

---

## M2a — the pre-flight

M2 said to re-verify the no-relooper measurement across every example before
writing lowering, rather than trusting the two artifacts the spike sampled. That
was worth doing: the corpus holds one exception to each of the two claims, and
one of them is a real correction to this RFC.

Measured over the emitted IR of all 81 examples (`vyrn emit-ir`), with the
hand-written IR runtime separated from emitter output — the runtime is 31
functions copied into every module, so leaving it in triples the counts and
hides everything interesting:

| | emitter | hand-written runtime |
|---|---|---|
| functions | 4,045 | 2,511 |
| basic blocks | 66,992 | 9,801 |
| loop headers (back-edge targets) | 2,824 | 1,296 |
| `phi` | 2,990 | 2,511 |
| **`phi` in a loop header** | **10** | 2,025 |
| aggregate `phi` | **149** | 0 |
| aggregate `select` | **0** | 0 |
| calls | 83,098 | 5,589 |
| **indirect calls** | **0** | **0** |
| function address as a value | **9** | 0 |

`indirectbr`, `blockaddress`, `invoke` and `landingpad`: zero everywhere.

### No relooper. The ten phis are one canned builtin, not control flow

Every phi in a loop header that the emitter produced is in the same five blocks:
`parse.loop`, from the `parse(String) -> Option<Int64>` builtin, which the
emitter writes as a fixed six-block routine with two induction phis it
backpatches itself (`lib.rs`, "Backpatch the loop phis' back-edge values"). Two
phis at five call sites across four examples — `argsdemo`, `input` (twice),
`stringops`, `vlog`.

That is not the case the relooper question is about. Nothing *derived from an
AST loop* has a phi in its header, because the emitter keeps every user variable
in an alloca and lets `mem2reg` do the SSA later; the direct backend keeps them
in frame slots and lets nothing do it. `parse` is a hand-written subroutine that
happens to be spelled in the emitter rather than in the runtime string, and its
two phis are two wasm locals. The claim M2 rests on holds.

### The aggregate joins are wider than "diamond", and it does not matter

149 aggregate phis, exactly the number M0 measured, none of them in a loop
header, and aggregate `select` is still 0 — so every one has a branch to hang
stores on. But they are not all two-way: 103 have two incoming edges, 46 have
between four and seven (`match` over an enum with many arms is an n-way join,
not a diamond).

Destination-first is indifferent to the arity: allocate the join's slot before
the branch, have each arm store into it. A relooper-free traversal would have
cared, because it would have had to reconstruct the join; this one does not.

### The correction: there IS a function table, and it is `spawn` — WRONG; see M2m

> **No function table.** Zero indirect calls and zero function-addresses-as-values
> in the artifacts checked.

The first half is true — zero indirect calls, in 88,687. The second half is not.
Nine sites take a function's address:

```
%t2 = call ptr @__vyrn_spawn(ptr @__vyrn_task_vyrn_fib, ptr %t0)
```

RFC-0025 `spawn` hands the shim a per-spawn-site thunk symbol plus a heap frame,
and the shim calls it. The emitter's own comment is why the spike missed this —
"the thunk symbol is a C-boundary detail, not a Vyrn-level function value: every
`call` still names a symbol" — and that is exactly right about the *emitter*.
It is wrong about wasm, where a callee reached through a pointer needs a table
entry, `ref.func`, and `call_indirect` on the shim's side.

It is bounded and enumerable rather than open: 9 sites in 3 examples
(`concurrency` 4, `parallel` 4, `controlflow` 1), one table element each, all
known at emit time because a spawn site is a syntactic construct. Defunctionalized
closures (RFC-0037) still need no table, which was the load-bearing half of the
claim. But "no function table" is not true of the finished backend, and a
milestone that assumed it would have discovered that at the end instead of here.

**M2m: the last two sentences are wrong.** Counting the nine addresses was right;
concluding a table from them was not. What the shim *does* with the pointer on
this target was never checked, and it is `thunk(frame)` on the next line — wasm
has no threads. So the nine are nine eager calls whose callee is named
syntactically, and a backend that emits the eager path forms no pointer at all.
"No function table" is true of the finished backend; the pre-flight's own
instruction (measure rather than reason) was followed on the count and skipped on
the consequence.

### Verdict

The design survives. No relooper, no reconstruction of joins, and — after
M2m — no table either.

**Cashed in M2m.** Both function-value features are lowered now and neither needed
a table: an RFC-0023 target is a compile-time function index, and an RFC-0037
stored value is a tag the signature's dispatcher switches on. The nine sites here
are still exactly `spawn`'s.

---

## M2a, as landed

One example, all the way through. `examples/fib.vyrn` — functions, recursion,
`if`, comparison, `print`, `return`, an exit code — compiles under
`VYRN_WASM_BACKEND=direct`, runs under wasmtime, and prints `55\n` and exits 55
byte-identically to `vyrn run`. `vyrn-codegen/src/direct.rs`, ~560 lines
including the gap reporting.

The point was never fib. It was to find out whether the pipeline M0 and M1 built
actually closes, and to leave behind the instrument that measures the rest.

### What the constraints cost, now that they are code

Both of them cost bookkeeping, and neither cost a redesign.

**A body must not emit `return`.** The body is wrapped in one `block` whose
result type is the function's, and `return` is `br <depth>` to it. `depth`
therefore has to be exactly right — every `if` that opens a wasm block increments
it — which is the kind of counter that is silently wrong rather than loudly
wrong, so it lives in the lowering context beside the scope stack rather than
being recomputed. The one pleasant surprise: `Frame::epilogue` is
stack-neutral (it pushes two values and pops two), so the returned value can sit
underneath it and no helper function was needed.

**Destination-first at joins** is not exercised yet — M2a produces no
aggregates — but nothing it did forecloses it, because scalars are wasm locals
and the frame is already allocated and addressable beneath them. `print` uses
the frame (digits backwards from the end of a 32-byte buffer, then an iovec
pointing at wherever it stopped), so the convention is exercised rather than only
written down.

**Widening.** No boundary crossing yet beyond WASI's own `fd_write`/`proc_exit`,
which are `i32` throughout. `abi`'s one widening site stays M1's.

`print` is emitted as wasm rather than deferred: it is `printf("%lld\n")` today
and varargs are M3. Unsigned division throughout, so `Int64.min` — whose negation
is itself — prints its digits instead of wrapping to nothing. Checked against the
interpreter on 0, −7, both `Int64` extremes and a round number.

### The ladder

`vyrn-cli/tests/directwasm.rs`, `#[ignore]`d beside `parity`, sharing its harness
rather than copying it: `examples_dir`, `run_io`'s conventions (cwd, `.stdin` and
`.args` fixtures, the RFC-0043 fixed clock and seed), `norm`, `runtime_err` and
the exclusion lists moved to `tests/common/mod.rs`, which both tiers include. Two
tiers disagreeing about what "the same run" means is the one way this number
could stop being about the backend.

It needs **only a `wasmtime` binary** — no clang, no sysroot, no builtins
archive. That is this RFC's acceptance criterion, asserted by the shape of the
test years before the criterion is met.

**A committed list, not a count.** `PASSING` names the examples that work, and
the run fails if any of them stops working, reporting what it is now blocked on.
A count would let the set churn silently — one example starts passing while
another regresses and the number does not move. An example that passes *without*
being listed prints a line asking to be added and does not fail: a burndown whose
every widening commit is red until it is finished is a burndown nobody runs.

### 2 of 80

`fib.vyrn` and `testing.vyrn` (whose `main` is a two-`if` `clamp` and a `print`).
80 rather than 81 is parity's denominator: `validate_compile.vyrn` never builds
on any backend and `externdemo.vyrn` needs a browser.

The blocker list is not what a 2/80 suggests. It is not 44 different problems:

| blocked on | count |
|---|---|
| a non-scalar type in a signature (`String` 23, `Array<..>` 11, records and validated names 15, `Option`/`Result`/`Ref` 9, `Map`/`SmallArray`/`RpcReply` 4) | **62** |
| a builtin with no lowering (`args`, `logger`, `newScanner`, `jsonSchema`, `handle`) | 5 |
| `while` | 2 |
| module state (RFC-0013 top-level `let`) | 2 |
| `spawn`, `region` | 2 |
| generics, floats, sized ints, bitwise, a Unit value in a value position | 5 |

So M2b is the aggregate ABI and the `String` representation, not control flow —
62 of 78 examples stop at the same wall, and the pre-flight already said that
wall has no relooper behind it. Structured control flow is 2 examples' worth of
work.

### One number worth recording, and its caveat

`fib.vyrn` to wasm: **19 ms and 383 bytes** direct, **190 ms and 277,438 bytes**
through clang (five builds each, warm).

The caveat is large. fib is the smallest interesting program, most of those 277 KB
is wasi-libc and the IR prelude, and M3 and M4 will add some of both back. The
honest claim is only that the double compile this RFC exists to delete is
measurable at the smallest possible scale, which is where it should be hardest
to see.

### What M2b inherits

A working pipeline, a switch that fails loudly, and a list of 78 examples sorted
by what stops them. The first item on it is worth 62.

---

## M2b, as landed

The wall M2a measured was one wall. It is gone, and what is behind it is a
different list rather than the same one — which is what a burndown is for.

`vyrn-codegen/src/direct.rs`, ~1,700 lines. **8 of 80**, from 2.

Eight is not the interesting number. 62 of 78 examples used to stop at *a
non-scalar type in a signature*; zero do now. The type system reaches the
backend, and what stops an example today is a construct that has to be written
(`for`, an array literal, a `where` refinement) rather than a shape the backend
could not describe.

### The aggregate ABI survived contact with code, unchanged

M0 wrote down four rules. All four are now emitted, and none of them needed a
fifth:

- On the operand stack an aggregate is **always** the `i32` address of a frame
  slot, never a value.
- A parameter is an address the callee copies out of in its prologue. M0 said
  this would be free because the LLVM emitter already stores every parameter
  into a fresh alloca; it was.
- A return is a hidden leading `i32` the callee writes through, and the wasm
  function returns nothing.
- A join allocates its slot **before** the branch and each arm copies into it.

No shape needed a case outside those: records (including nested and
width-subtyped), `Option`/`Result`, user enums, and the `{ i64, i64 }` pair a
`Ref` is. Field offsets are `layout::of_ll` ∘ `llt` at every site — nothing in
this milestone computes an offset.

### The n-way joins cost what the diamonds cost

M0's warning, and the one thing worth reporting as a result rather than as
work. 46 of the 149 aggregate joins have four to seven incoming edges.

A `match` lowers to a chain of `if`s inside one `block`, each arm leaving by
`br` to it. A scalar result rides the branch; an aggregate one is copied into a
slot allocated before the first tag test. **There is no arm count anywhere in
the lowering** — no phi to reconstruct, no join to rediscover — so seven arms
cost seven `if`s and nothing else. Aggregate `select` remains 0, so the case
with no branch to hang stores on never arose.

The one thing destination-first *does* cost is that the arms must agree on a
type before either is emitted, since the slot is allocated first. That is a
small `peek` typer, and every arm is then checked against its answer by the
ordinary conversion path — so a wrong prediction is a compile error, not a
miscompile.

### `String` was the easy half of its own bucket

A `String` is a NUL-terminated `ptr`, i.e. a **scalar**. The 23 examples it
blocked were never blocked by its representation; they were blocked by what you
can do with one. So the module emits `strlen`, `strcmp`, `int_str` and `concat`
— which between them are `print`, `==`, `+` and `"\{x}"` — over a bump `malloc`
of its own.

That allocator is the milestone's one deliberate corner. RFC-0076's shim owns
the real one, but `vyrn build --target wasm` produces ONE module with no shim
beside it, and M4 is where the prelude moves. Until then: a second mutable
global, initialized past the statics by the encoder (the number M0 predicted a
direct emitter would know by construction), and **it never frees**. Nothing
observable depends on that — a free is not something a program can print — and
`Stmt::Drop` is already in the AST for when it does.

### Two things are refused because getting them wrong is silent

Both were found by the ladder, and both are the class this RFC keeps naming:

- A **validated type** (`type Age = Int64 where value >= 0`) has the *same
  representation* as its base. It would have lowered perfectly and simply never
  checked the refinement — a wrong program, not a missing one. 14 examples.
- A **`modify` parameter** is by-pointer with copy-out. Passed like a `read`
  one it compiles cleanly and throws the callee's writes away; `modify.vyrn`
  printed zeroes where the interpreter printed 115. 4 examples.

A type-level gap walk (depth-bounded, because a record may hold a `Ref` to its
own type) catches the first anywhere it hides, including inside a record field.

### Two structural changes, both to stop a second source of truth

**`llt` is now the free `llt_of`.** M0 chose to parse the LLVM type *string*
precisely so layout could not drift from lowering; that argument applies twice
over to a second backend, so `Gen::llt` and `direct` now call one function. A
second match on `Type` would have been the mistake this RFC exists to avoid
making twice.

**`wasm::Frame` buffers its instructions.** The prologue depends on a frame size
that only the finished body knows. The alternative was a sizing pre-pass over
the AST — a second traversal that has to agree with the first about which
expressions need a slot, which is the same class of bug wearing a different
hat. Buffering also made locals dynamic, which deleted M2a's `collect_lets`
and its shallow `infer` entirely.

### The bug worth recording

`print` wrote its string and its newline as two iovecs of one `fd_write`, and
only the first arrived. A short write is legal and the return value was being
dropped — the emitted code was *correct wasm* and *wrong I/O*, which no
validator was ever going to say. Every byte now leaves through one `write_all`
that retries, rather than three call sites that each have to remember to.

M2a's `print_i64` had the same latent exposure and never showed it, because 21
bytes never short-write.

### Nothing contradicted M0, M1 or M2a

The destination-first rule, the no-`return`-in-a-body rule, the widening rule
and the memory map all went in as written. The one M2a note that is now
outdated is its size measurement: `fib.vyrn` is **944 bytes**, not 383, because
every module carries the ~500-byte emitted runtime whether it calls it or not.
Still three orders of magnitude under the 277,438 the clang path produces, and
M4 is where most of it leaves again.

### 8 of 80, regrouped

| blocked on | n |
|---|---|
| `Array<T>` and what you do with one (array literals 8, `for` 7, `.length` 3, `at` 2, `chars` 1) | **21** |
| a `where` refinement (`Age`, `Count`, `Percent`, `Username`, …) | 14 |
| builtins with no lowering (`args`, `cell`/`set`/`get`, `logger`, `readFile`, `readLine`, `jsonSchema`, `schemaOf`, a map literal) | 12 |
| generics — monomorphization runs inside `Gen`, not before it | 10 |
| a `modify` parameter | 4 |
| module state (RFC-0013 top-level `let`) | 3 |
| `spawn` 2, `region` 1, `if let` 1 | 4 |
| floats, sized-int arithmetic, bitwise | 3 |

The first row is one feature — a growable array and its runtime — and it is
worth 21. That is M2c's first item, and it is the same shape of argument M2a
made about the type wall, which turned out to be right.

---

## M2c, as landed

`Array<T>` and `for`. **14 of 80**, from 8 — and `for` is no longer the first
blocker of anything, nor is an array literal, `.length` or an index.

The bucket was worth 21 and paid 6, which is the honest number and the reason
the ladder is a list. The other 15 moved on to a different blocker rather than
passing: `toJson` (4), a `where` refinement, `stringFromBytes`, `parse`,
`@charCount`, module state. Every one of them now stops somewhere *after* the
array code, which is what the burndown was measuring.

### The shadow-stack convention needed no fifth rule, but arrays needed one fact

A growable `Array<T>` is the `{ptr, i64, i64}` triple in a frame slot, so it is
an aggregate like every other and M2b's four rules cover it unchanged. What is
new is that the **buffer** is not — it is heap, it moves, and it outlives the
slot. Keeping those two facts apart is the whole of this milestone.

`Walk` is where they meet: it takes an indexable value apart into *locals* —
data pointer and length — and everything downstream indexes off the snapshot.
That is not an optimization. The LLVM backend gets the same snapshot for free by
holding an SSA aggregate, and it is why a `for` whose body grows the array it is
walking keeps walking the buffer it started on rather than following the
reallocation to a new one. All three engines agree, because the interpreter
iterates a copy. Checked directly rather than reasoned about: a loop that pushes
on every turn sums the two elements it started with and finishes at length four,
identically under both.

Aliasing came out where M2b left it. Indexing an `Array<Record>` yields the
*interior address*, exactly as a record field access does, and every place that
stores — a `let`, an argument, a `return`, a `match` arm — was already copying.
So `pts[i].field = v`, which the parser desugars into a three-statement
copy-modify-store, is sound without the backend knowing the desugar exists.

### `continue` is the one place structured control flow cost something

A `while` re-tests its condition, so `continue` can branch to the `loop` itself.
A `for` does not: it steps its index in a latch, and branching to the loop would
re-enter the body on the same element forever.

So the body goes inside an **inner `block`**, and `continue` leaves that —
landing on the increment, which is what falling off the end does too. `break` is
still the outer block. One extra block, no relooper, and no phi anywhere: the
pre-flight's claim survives its first real loop. A `return` out of a `for` nested
inside a `while` still reaches the epilogue, which is the M1 rule being exercised
by the construct most likely to break it.

### One conversion, not one per literal position

An array literal is always the fixed `[N x T]` shape — the same thing the LLVM
backend builds — and reaches the heap through a single `ArrayN → Array`
conversion in `expr_as`. So a literal in a `let`, an argument, a `return`, a
record field, an enum payload or a `match` arm all heapify by the same code.

Copying there is what makes it sound rather than merely convenient: the triple
outlives the frame slot the literal was built in, and `push` will reallocate the
buffer it is handed. The empty `[]` is the one exception, because it has no
element to be typed by — it can only be the empty triple its expected type
names, which is a gap when nothing expects one.

### Refused, for the usual reason

- A **`SmallArray<T, N>`** (RFC-0056) is a four-field header with an inline
  buffer and two live states. Its first field is a length where a triple's is a
  pointer, so reading one as a triple compiles cleanly and indexes garbage. 1
  example, and it now says so.
- A **`Map` index**, for the same shape of reason.
- An **`Option` of a two-word payload** out of `pop`: `build_sum2` copies those
  16 bytes whole rather than encoding one word, and the encode path has no
  second word to write.

### The corner that is the allocator's, not the array's

`push` grows by allocating and copying rather than `realloc`ing, because this
backend's allocator is still M2b's bump pointer that never frees. The abandoned
buffer is that decision's cost, not a new one, and M4 is where the shim's real
allocator arrives and this line deletes itself.

### The one message with a number in it

`error: array index 7 out of bounds` has the index in the *middle*, so it cannot
be one interned string: it is `trap_idx(prefix, i, suffix)`, three writes and the
existing `int_str`. That is also the one runtime message no example reaches — a
bounds check that never fires reads exactly like one that fires with the wrong
wording — so it has its own test, both spellings, compared against the
interpreter's stdout, stderr and exit code.

### Nothing contradicted M0, M1, M2a or M2b

Every offset is still `layout::of_ll ∘ llt`, including the element stride, which
is a size: `of_ll` rounds a shape up to its own alignment, so a size IS a stride
and nothing had to compute one. Destination-first, no-`return`-in-a-body, the
widening rule and the memory map all went in as written.

### 14 of 80, regrouped

| blocked on | n |
|---|---|
| a `where` refinement | 14 |
| generics — monomorphization runs inside `Gen`, not before it | 10 |
| builtins with no lowering (`toJson` 4, `args`, `cell`/`set`/`get` 4, `logger`, `readFile`, `readLine`, `jsonSchema`, `schemaOf`, `chars`, `stringFromBytes` 2, `parse`, `@charCount`, a map literal) | 19 |
| a `modify` parameter | 4 |
| module state (RFC-0013 top-level `let`) | 5 |
| `spawn` 2, `region` 1, `if let` 1 | 4 |
| floats, sized-int arithmetic and conversion, bitwise | 4 |
| a `Map` or `SmallArray` index — refused above | 2 |
| a `T` conversion inside a generic payload | 1 |

The two rows worth 24 are both single features, and neither is control flow.
A `where` refinement is a check to emit at a coercion — the one thing M2b
refused precisely because it would otherwise be silent — and generics are a
monomorphization pass that already exists in the LLVM emitter and runs on the
wrong side of the boundary. Builtins are 19 but they are 14 different things,
which is the row that does not compress.

## M2d — what stopped it before it started

Three dispatches of M2d died on transient server errors without touching the
tree. Reading the code to write a better brief turned up the thing that actually
matters, and it reframes the milestone.

**`coerce` appears zero times in `direct.rs`.** The direct backend has no
coercion concept. It lowers a value when `repr()` already agrees on both sides,
and `ty_gap` refuses everything that would need reconciling — which is precisely
why validated types, `modify` parameters, `SmallArray`, `Map` indexing and a
two-word `Option` payload are all gaps rather than bugs. The refusals were not
five separate omissions; they are one absence wearing five hats.

So M2d is not "emit the check at the coerce site". There is no coerce site. The
LLVM backend's `coerce` (`lib.rs` ~2406) does four things at once:

1. runs a `where` predicate and traps — the validation this milestone wants
2. re-tags a function value between fn-typed spellings (RFC-0037)
3. re-materializes an `Option`/`Result` whose payload representation changed
4. numeric conversion between widths

Item 3 is the same shape as the `Option`-of-two-word-payload refusal from M2c,
and item 4 is the sized-int arithmetic blocker. **A coercion path in the direct
backend is therefore worth more than the 14 examples `where` refinements
account for** — it is the common cause under several rows of the blocker table.

The decision about *when* validation is required stays where it is
(`Type::Named`, `from != to`, `predicate.is_some()`, minus
`finite::string_flow_proven`) and must be extracted into a function both
backends call rather than written a second time. Same for the trap wording,
which is byte-identical today:

- record base: ``error: validation failed: `{name}` violates its `where` clause``
- scalar base: ``error: validation failed for `{name}` ``

And `emit_predicate_cond` carries a comment saying it is deliberately the ONE
place a predicate is lowered, shared with the RFC-0018 JSON decode path "so the
two never drift". A direct backend that lowers predicates its own way breaks
that property silently.

**Revised plan: M2d becomes the coercion path, with validation as its first
client.** The gap list shrinks by more than the refinement row, and the pieces
that would otherwise be built twice get built once.

---

## M2d, as landed

The seam, and validation running through it. **17 of 80**, from 14 — and no
example is blocked on a `where` refinement any more, which was the row worth 14.

Seventeen is the wrong number to read this milestone by, and the reason is the
one the brief opened with: `Even` and `Int64` are the same bytes. A lowering that
emits the type and forgets the check turns all fourteen refinement examples green
while validating nothing, permanently. So the number that matters is one.

### The seam went where `expr_as` already was

`Fn_::coerce`. `expr_as` is now its only caller and does nothing but evaluate and
hand over, which means the seam inherited every flow site rather than having to
find them: a typed `let`, an assignment, a field store, an element store, a call
argument, a return, a join arm, an array element, an enum payload. The two
conversions that were already there — the `ArrayN → Array` heapify and RFC-0002's
record width rebuild — moved inside it unchanged, which is the check that the
seam is where the reconciliations were rather than a new layer above them.

**Validation runs first, before the `ll`-equality shortcut.** That ordering IS the
milestone. Every other conversion in `coerce` is reached because the
representations differ; this one is reached precisely because they do not.

### Three flows were validating nothing for a reason that was not the seam

Found by writing the seam and then asking which boundaries actually reach it. The
backend was resolving declared types away: a parameter, a `let` annotation and a
return type spelled `Age` were all in scope as `Int64`, so `from == to` at every
one of them and the boundary was not a boundary. `ret_ty: cx.resolve(..)` was one
character's worth of code and three missing checks.

They keep the declared spelling now. `binary` does the exact opposite for the same
reason — it coerces its right operand to the RESOLVED type, because `age + 1` must
not check `1` against `Age`'s predicate, and it returns the resolved type so that
the *assignment* re-validates the sum. The LLVM emitter reaches that same split by
returning its `numty` rather than its `lty`, which is worth recording: the
direct backend rediscovered a decision the other one had already made, and
disagreeing would have been a spurious trap on one target only.

### Two sites are not the seam, and correctly so

A record literal of a predicated type, and `Age(n)`. At both, the value already
IS the named type — `from == to`, so `validation_required` says no, and it is
right to. There is no flow to hang the check on because construction is not a
flow. That is why the LLVM emitter has a construction site of its own, and this
backend now has the same two. Both skip the check when every argument is
constant, which is `gen_construction`'s rule.

### What is single-sourced, and what is not

Three things are now free functions in `vyrn-codegen`, and both backends call
them:

- **`validation_required`** — the decision. `Type::Named`, `from != to`,
  `predicate.is_some()`. A second spelling would have been two semantics for one
  fact, exactly what `llt_of` exists to prevent for shapes.
- **`validation_message`** — the wording, byte-identical because parity compares
  stderr.
- **`predicate_binds`** — what a predicate has in scope: a record base binds every
  field by name, everything else binds `value`.

RFC-0020's containment escape needs the expression rather than the two types, so
it stays one layer out — but it was already a single shared function
(`finite::string_flow_proven`), and both backends call it on the same AST. The
consteval precedent.

**What is duplicated, and why.** The predicate's *lowering* — one backend prints
LLVM text, the other writes wasm bytes, so there is no version of this where one
function emits both. `emit_predicate_cond` carries a comment claiming to be the
ONE place a predicate is lowered; that is now true of LLVM specifically, and what
the two backends share is the structure each walks. `predicate_binds` is that
structure, and it is the only thing that decides what a predicate can see.

`predicate_binds` paid for itself before the direct backend used it. The comment
was already wrong about its own file: `emit_validation` held a byte-for-byte copy
of the same binding walk, so the two could have drifted exactly as the comment
feared. It is now that function plus a trap. A third copy — the cross-field check
in `gen_struct_lit` — binds registers that are not in an aggregate yet, so it
walks its own list and stays where it is; it is the one place still able to drift,
and it is named here rather than left to be found.

### How the checks were proved to be emitted

Two ways, because "the examples pass" is the thing being guarded against.

**`validate_fail.vyrn` is in `PASSING`.** Its refinement is violated at runtime,
so the ladder compares its stderr and its exit code against the interpreter — and
a backend that emits the type and forgets the check fails it while passing every
other refinement example. It already existed in the corpus, which is luck; M2c's
bounds message had to have a test written for it.

**`a_validated_type_is_checked_wherever_it_is_reached`** is what became of
`a_validated_type_is_a_gap_not_a_bare_int`. Same two positions — the bare type,
and inside a record "because that is where it would hide" — asserting the check IS
emitted rather than that the type is refused. The evidence is the trap message in
the data segment, which only `emit_validation` interns, so its presence means a
check exists and its absence means one does not. A third case with the refinement
declared but unreached pins that the assertion is about the check and not about
the word `Age` arriving some other way.

### Nothing contradicted M0, M1, M2a, M2b or M2c

One new fact about the encoder, learned by needing it: **a value may sit on the
operand stack underneath an `if` block**. An `if` records the stack height it
opened at and nothing inside can reach below it, but what is below survives to
the `End`. That is what lets a validation be a check ON a flow rather than a step
in it — the destination address a store already pushed is still there afterwards.
It is exercised rather than reasoned about: an `Array<Age, 2>` literal validates
each element with the element's destination address live beneath the check.

The value under check is parked in a local, not left on the stack, because the
predicate's own code would bury it. For a scalar base the parked local IS the
`value` binding, so nothing is copied twice.

### Refused, specifically

Items 2–4 of the LLVM `coerce` list did **not** fall out free, and none is
pretended to have:

- **Re-tagging a function value** between fn-typed spellings (RFC-0037) — no
  example reaches it, because generics stop them all first.
- **Re-materializing an `Option`/`Result` whose payload representation changed** —
  still the M2c refusal it was.
- **Numeric conversion between widths** — `a conversion from Int64 to UInt16`,
  one example. The truncation itself is small; `wrap_intn` parity across the
  sized widths is not, and the sibling blocker `Add on Int32` says the arithmetic
  table is i64-only anyway. Two rows of the table below, named.

Also refused: **a `where` clause over a non-record aggregate**. Its one `value`
binding has nowhere to live, because `Place` is a wasm local or a frame slot and
cannot name "the address in this local". No example has one; binding it to
something adjacent would have been silent.

### 17 of 80, regrouped

| blocked on | n |
|---|---|
| generics — monomorphization runs inside `Gen`, not before it | 9 |
| builtins with no lowering (`toJson` 5, `stringFromBytes` 3, `jsonSchema` 3, `readFile` 2, `readLine` 2, `args`, `cell`/`set`/`get`, `logger`, `parse`, `chars`, `schemaOf`, `fromJson`, `hostNowMillis`, `@charCount`, a map literal) | 25 |
| module state (RFC-0013 top-level `let`) | 5 |
| a `modify` parameter | 4 |
| `=~` on strings, `?`, `if let`, a fallible construction, `spawn` 2, `region` | 7 |
| floats, sized-int arithmetic and conversion, bitwise | 4 |
| a `Map` or `SmallArray` index — refused in M2c | 2 |
| a `T` conversion inside a generic payload | 1 |

The refinement row is gone. What is left is one big row that does not compress
(25 builtins, 15 different things) and one that is a single pass in the wrong
place: **monomorphization runs inside `Gen`**, so the direct backend never sees a
concrete instantiation. That is 9 examples and the same shape of argument M2a made
about the type wall and M2b made about arrays, both of which were right.

---

## M2e, as landed

Generics. **20 of 80**, from 17 — and the generics row is gone, which is the
smaller half of the story, because six of its nine examples were never blocked on
generic *functions* at all.

### The third assumption this corpus falsified

> **No generics.** Monomorphization runs before any instruction is emitted.

It does not, in either backend. `Gen` accumulates `instantiations` as a **side
effect of emitting a body**; the driver takes them, enqueues, and drains a
worklist, and two worklists feed each other because a generic body may take `fn`
parameters and a specialized instance may call generics. There is no pre-pass to
consume.

Which made the design question the interesting part, and the trap in it explicit:
a standalone "collect the instantiations reachable from these roots" pass would
have been a second traversal that has to agree with lowering about what gets
instantiated. That is a new source of truth, free to drift — the failure mode
`llt_of` (M2b) and `predicate_binds` (M2d) exist to prevent, and in M2d's case
one that had *already happened* inside a single file. So there is no discovery
walker. `Mono` is fed from inside `Fn_` and drained by `compile`, the same shape
as the textual driver.

### The one thing that is stricter here, and it is not a taste

A textual call emits a **symbol**; a wasm call emits a function **index**. So an
index has to be handed out where a specialization is *discovered* and its body
added later, which only works if the two orders are the same. `Mono::insts` is
append-only and `done` only moves forward — FIFO by construction.

That is not defensive framing. The textual driver drains with `queue.pop()`, a
stack, and is right to: nothing depends on the order because every reference is a
name. The same code here would renumber every call after the first out-of-turn
discovery, and *the module would still validate* — wrong function, plausible
output, no diagnostic. It has its own running test rather than a comment:
`twice` calls `wrap`, so `wrap<Int64>` is discovered while three other instances
are still queued.

### What is shared, and the one thing that turned out not to be shareable

Shared as free functions both backends call:

- **`solve_param`** — the unification rule, already single-sourced, now
  `pub(crate)`.
- **`solve_type_args`** — the *decision* a site makes: which type arguments it
  fixes, given the declared types and the concrete ones supplied. It **reports**
  an unsolved parameter rather than deciding for it, because the two backends
  genuinely differ there and pretending otherwise would have changed the textual
  output. The textual emitter has always substituted `Unit` and let it lower to
  `void`; the direct backend refuses, because a `void` in a wasm signature is not
  a diagnostic, it is a signature with one fewer parameter.
- **`applied_type`** — the concrete type a *construction* site produces. Four
  sites in the textual emitter route through the two of them (both generic call
  paths, `applied_enum_type`, and `gen_struct_lit`'s result type); `emit-ir` over
  every generic-using example is byte-identical.

**`mangle_name` is not shareable, and this RFC's brief was wrong to list it.** It
is a *symbol*, and a wasm function does not have one — imports and exports do,
internal functions are indices. Worse, it is not injective: every record mangles
as `Rec`, so two distinct instantiations can produce one symbol, and the textual
driver's `emitted.insert(sym)` would silently skip the second. The direct backend
keys specializations on the type arguments themselves, which cannot collide on
the thing being distinguished. (The textual hole is latent rather than observed —
named here rather than left to be found, like M2d's third `predicate_binds`
copy.)

**What is duplicated:** nothing new. M2d's precedent is that a predicate's
*lowering* is necessarily per-backend while the structure it walks is shared;
here the *recording* of instantiations is per-backend, because each walks its own
bodies, while the rule for what to record is not.

### `Type::Param` is now unreachable rather than unhit

`Cx::sub` is the chokepoint: `ll`, `resolve`, `fields` and `ty_gap` all go through
it, so a parameter cannot reach `llt_of` — where M0 left it printing `void`, and
`layout` gives `void` a size of zero — by any route that asks this `Cx` about a
type. `ty_gap`'s arm stays, reworded, as what is left over when an instantiation
failed to fix something. Asserted from both sides, because "it never fires" is a
different claim from "it cannot": outside a monomorphization the parameter is
refused and `ll` gives `void`; inside one, every entry point gives the type the
instantiation fixed, *including inside a constructor* — `Array<T>` is the same
triple for every `T` but its element stride is not.

One substitution subtlety worth recording: `sub` substitutes into the type
*expression*, before any `App` is expanded. `type Box<T>` and `fn f<T>` may both
spell their parameter `T`, and `resolve` builds the declaration's own
substitution from the `App`'s arguments afterwards — so the two cannot be
confused, and nothing had to rename anything.

### Generic records and enums did NOT fall out free

The brief asked whether generic enums fall out, since they monomorphize inline at
concrete use sites. Half of one does, and records do not at all.

- A **generic record** literal needs its type arguments solved from the FIELD
  values, before the slot is allocated, because `Box<Int64>` and `Box<Bool>` are
  not the same size.
- A **generic enum** falls out wherever the position names the type
  (`let b: Opt<Int64> = Empty`), because `resolve` substitutes an `App`'s
  arguments and every payload type arrives concrete. It does not fall out for a
  bare constructor with no expectation (`let a = Wrap(41)`), whose use site is
  its own payload.

Both are `applied_type` from a different set of declared types — fields, or a
variant's payloads — so they are one rule applied twice, not two features. And
both put the backend back in the bind M2b's `peek` was written for: the
destination has to exist before the value does, so `peek` predicts and `expr_as`
re-checks, and a wrong prediction is a compile error. That is now the third
construct standing on that pattern (`if`, `match`, and a call whose callee's
signature is not known until its arguments are typed).

### The bug only running could catch, again

A call reported its return type **resolved**. `Pair<Int64, Int64>` reduced to its
record shape no longer matches `Pair<A, B>`, so `firstOf(twice(41))` could not
fix `A` — and the failure was a refusal only by luck: had the outer generic's
parameter been solvable from somewhere else, the resolved type would have flowed
on and specialized something plausible. It returns the declared type now, which
is what the textual emitter always did. Three milestones' worth of examples never
noticed because no example nested one generic's result inside another's argument.

### Refused, specifically

**RFC-0023 higher-order specialization did not fall out free, and is refused by
name.** A function with a `fn`-typed parameter has no first-order definition in
either backend — it exists only as specializations, from a *second* worklist that
this milestone does not build. It is refused at the call site rather than at the
callee's shell so the ladder groups it as one feature.

That is the number worth being honest about: of the nine examples the generics row
held, **six were RFC-0023** (`map` in five, `defer` in one), not generic
functions. Only one of those six is still blocked on it — the other five moved on
to a `modify` parameter, a map literal or `stringFromBytes` — which is what the
burndown is for, but 17 → 20 rather than 17 → 26 is the honest reading of the
row.

### Three things came along because the shells were in the way

The textual driver skips three kinds of function in its step 1; this one skipped
only externs, and lowered the rest as unspecializable shells. A shell that cannot
lower fails the whole build over a function nothing calls, which is what
`gendemo.vyrn` was blocked on — a `readFile` inside a `gen fn`, i.e. inside code
that only ever runs in the compiler. Generic, `fn`-parameter and `gen fn`
functions are all skipped now, as they always should have been.

**Protocol dispatch** (RFC-0002 §5) landed with generics rather than after them,
because a bounded generic is what protocols are for: `describe<T: Show>` calling
`x.show()` is the whole of `protocol.vyrn`. Static dispatch on the receiver's
concrete type — which inside a bounded generic is concrete only because `subst`
says so — through the same mangled impl name the textual emitter calls.

### Nothing contradicted M0, M1, M2a, M2b, M2c or M2d

Every offset is still `layout::of_ll ∘ llt`. Destination-first, no-`return`-in-a-
body, the widening rule and the memory map all went in as written, and the M2d
seam took the one addition generics needed: `coerce` substitutes before
`validation_required` looks at its two types, because a `T` where `T = Age` is an
`Age` flow and a bare `Param` is neither `Named` nor a boundary. That is the same
class of silent hole M2d found three of — a declared spelling that stops being
one — reached by a different route.

### 20 of 80, regrouped

| blocked on | n |
|---|---|
| builtins with no lowering (`toJson` 5, `stringFromBytes` 4, `cell`/`set`/`get` 5, `jsonSchema` 3, `readLine` 2, a map literal 2, `args`, `chars`, `fromJson`, `hostNowMillis`, `logger`, `parse`, `readFile`, `schemaOf`, `value`, `@charCount`) | **31** |
| a `modify` parameter | 6 |
| module state (RFC-0013 top-level `let`) | 6 |
| `?`, `if let`, `=~` on strings 2, a fallible construction, `spawn` 2, `region` | 8 |
| floats, sized-int arithmetic and conversion, bitwise | 4 |
| a lambda 1, a `fn`-typed parameter (RFC-0023) 1 | 2 |
| a `Map` or `SmallArray` index — refused in M2c | 2 |
| a generic enum variant with no expectation to type it | 1 |

The shape of the list has changed. Every remaining row is small except the one
that is 16 different builtins, and the two 6s are single features: a `modify`
parameter is copy-out at a call (M2b refused it because passing it like a `read`
one throws the callee's writes away silently), and module state is one global per
top-level `let` plus an initializer that runs before `main`. Neither is control
flow, neither is a type-system gap, and after them the biggest thing left in this
corpus is `toJson`.

---

## M2f, as landed

Module state and `modify` parameters. **23 of 80**, from 20 — and both rows of
the M2e table are gone, which was 12 examples' first blocker between them.

### They were not one gap. They were one gap and one non-gap

The milestone was framed on a hypothesis: `Place` is a wasm local or a frame slot,
module state needs a *global* place and a `modify` parameter needs an *indirect*
one, so extending `Place` twice unblocks both. Half of that survived contact.

**Module state needed the new place.** `Place::Static(u32)` — an absolute address
in linear memory, reserved zeroed by the encoder before any body is walked. It is
a separate variant rather than a flag on `Slot` for the reason that makes module
state module state: a frame offset is relative to a base that changes every call
and a global's address does not.

**A `modify` parameter needed no place at all.** It is call-by-value-result: the
incoming wasm local holds the caller's address, the value is copied *in* at the
prologue and back *out* at the epilogue, and in between the parameter is an
ordinary local or frame slot indistinguishable from a `read` one. An
`Indirect(local)` place would have been smaller code — no copies — and *different
semantics*: the caller would see each write as it happened rather than at the
return. The textual backend already chose copy-in/copy-out (`modify_copyout`), so
parity decided this rather than taste, and the smaller design was the wrong one.

So the shared cause under the two rows is real but narrower than the framing:
what both need is **the address of a binding**, and that is `Place::addr`, one
method with an `Option` return. The `None` case is the finding — a scalar in a
wasm local has no address — and it is what the caller side of `modify` had to
handle rather than assume away.

### One mechanism for both kinds of global, and it is not a wasm global

A wasm global holds one value type, so an aggregate could never have lived in
one; the obvious split is scalars in globals and aggregates in a data segment.
That is two mechanisms for one language feature. Everything goes in memory
instead, which costs a scalar global a load where a local would have had a
`local.get` and buys one code path — and it is what the textual backend does
anyway, since an LLVM global is a pointer.

`Module::reserve` is deliberately not `Module::data`: `data` shares identical
contents, which is exactly right for a string pool and exactly wrong for storage.
Two zero-initialized `Int64` globals would have been one address, i.e. one
variable, and the module would still have validated. It has its own test.

### `modify.vyrn`'s zeroes were the missing copy-back, as they looked

M2b's refusal recorded the symptom without diagnosing it. Confirmed: the callee
copies the aggregate into a slot of its own in the prologue — M0's by-value
parameter convention, which is right for every other capability — and without a
copy-out the caller's record never changes. `c.value` stays 0 across three
`bump`s. Nothing about the convention was wrong; one direction of it was missing.

### The copy-out is in one place, because M1 said a body has one exit

`return` is a `br` to the function's outermost block (M1), so the copy-out goes
*after* that block's `End` and runs on every return path including the fall-off.
A backend that emitted a real `return` would need the copy at every exit and
would silently miss one. This is the second time that rule has paid for itself,
and it needed M2d's other fact too: the instructions are stack-neutral, so a
scalar result already sitting on the block's stack survives them untouched.

### The one thing the convention costs that the textual backend does not pay

A `modify` argument is the caller's binding by address. A frame slot has one and
module state has one. **A scalar in a wasm local does not** — so it is spilled to
a frame slot for the callee to write through and reloaded after the call. LLVM
never faced this because its locals are already `alloca` slots.

Nothing in `examples/` or `std/` has a scalar `modify` parameter — every one of
the 36 is a record, an `Array<T>`, a `Parser` or a `Scanner` — so the ladder
cannot see that path
at all, and omitting either half of the spill compiles cleanly and prints 21
where 42 belongs. It has its own running test
(`a_modify_parameter_copies_back_whatever_the_caller_kept_it_in`), together with
the two shapes the corpus also lacks: module state as a `modify` argument, where
the address is a constant, and a `modify` parameter handed on to another one,
where the address the inner call writes through is the outer callee's own slot.

### Initialization order was already decided, and the loader had already done it

RFC-0013 made top-level `let` root-only and host-owns-the-loop, and the textual
backend's `@__vyrn_globals_init` runs the initializers in declaration order from
`vyrn_entry` before `main`. `program.globals` arrives from the loader already in
linker order, dependencies first, so declaration order *is* the answer and
nothing had to be sorted: `statemod`'s diamond initializes its shared store
before either arm reads it, and prints its init markers in the order that proves
it. Once rather than per call falls out of there being one function and one call
to it from `_start`.

The initializer is a body like any other rather than a table of constants, which
is what lets it go through [`Fn_::store_into`] and therefore through the M2d
coercion seam: a top-level `let n: Age = ..` validates exactly as one inside a
function does, and a top-level array literal reaches the heap by the same
`ArrayN → Array` conversion. An unannotated global is typed by `peek`, so the
reservation loop had to move *after* the signature loop — a call is the one
initializer shape whose type only a signature knows.

### The M2d refusal is liftable, and it stopped being a `Place` problem

M2d refused a `where` clause over a **non-record aggregate** because "`Place` is a
wasm local or a frame slot and cannot name 'the address in this local'".
`Static` does not lift that — a global's address is fixed, and the value under
check is on the operand stack — but writing the record arm made the shape
obvious: copy the whole value into a frame slot and bind `value` to it, which is
what the record base already does field by field and needs **no new variant**.
Left refused, because no example has one and an untested lowering is worse than a
named gap. The comment now says that instead of blaming the enum.

### Nothing contradicted M0, M1, M2a, M2b, M2c, M2d or M2e

Every offset is still `layout::of_ll ∘ llt`, including a global's size and
alignment. Destination-first at a store, no-`return`-in-a-body, the widening rule
and the memory map all went in as written — the last of them twice over, since
module state is data placed from `DATA_BASE` up and the encoder's
`STATICS_LIMIT` assertion covers it for free.

One small thing came along because it was in the way: `pop` and `swapRemove` took
their receiver's frame offset and re-derived it three times. They take its address
into a local once instead, so they work on module state as well as a local — and
two of the four `b.slot(base + off)` sequences collapsed into a `MemArg` offset,
which is one instruction rather than three.

### 23 of 80, regrouped

| blocked on | n |
|---|---|
| builtins with no lowering (`toJson` 6, `stringFromBytes` 4, `jsonSchema` 3, `cell`/`set`/`get` 2, `readLine` 2, `args`, `chars`, `fromJson`, `hostNowMillis`, `Int64`, `logger`, `parse`, `readFile`, `schemaOf`, `value`, `@charCount`) | **28** |
| a map literal 3, indexing a `Map` 2, indexing a `SmallArray` 1 | 6 |
| `Match` on strings 4, `if let` 2, `?`, a fallible construction | 8 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda 1 | 4 |
| a branch yielding an unpeekable call (`get` 3, `Held` 1) | 4 |
| `spawn` 2, `region` | 3 |
| floats, sized-int arithmetic and conversion, bitwise | 4 |

Every row is now either one big undifferentiated bucket of builtins or a small
named feature. The `modify` and module-state rows are gone, and the six examples
each held did not turn into six passes — three did, and the rest moved on to
`toJson`, a map literal, `Match` on strings and `if let`, which is what the
burndown is for. The 12 → 3 reading is the honest one; 20 → 23 is the number.

---

## M2g, as landed

Eight builtins and a refusal. **30 of 80**, from 23 — and the builtins row is 17
rather than 28, which is the first time it has moved at all.

This milestone was briefed as the one with no unifying insight: sixteen unrelated
builtins, highest count first, no compression. Two of those three things turned
out to be wrong, and the third is what the milestone is actually about.

### The row was sorted by count, and count was the wrong order

`toJson` is 6 and `stringFromBytes` was 4, so those were the brief's first two
items. Neither was where the examples were.

**`jsonSchema` and `schemaOf` have no runtime lowering in EITHER backend.** They
are RFC-0021-family compile-time reflection: `gen_call` rewrites `jsonSchema(T)`
into `Expr::Str(json_schema_string(decl, types))` and `schemaOf(T)` into
`schema_struct_lit(decl)`, then lowers the ordinary expression that comes out. So
the direct backend does the same rewrite from the same two frontend functions —
about twenty lines, four examples, and *no bytes for the two backends to disagree
about*, which is the same argument `llt_of` and `predicate_binds` rest on. The
brief's instinct that `schemaOf` might have to be refused as compile-time-only
was exactly half right: it IS compile-time-only, and that is what makes it the
cheapest item on the list rather than the most expensive.

`value(x)`, `charCount`, `bytes`, `slice` and the numeric conversions came next,
in cost order rather than count order, and between them they closed four more
rows.

### `stringFromBytes` was not a wall, it was a queue

All four examples filed under it reach it through `std/strings`, and behind it
that module is a chain: `slice`, then `Eq` on `UInt8`, then `Add` on `UInt8`, then
`Shr` on `UInt64`. Four of those five are lowered now, and the four examples have
moved to the fifth. **Zero of them passed.**

That is a property of the ladder worth naming, because three milestones have now
read its rows as features: a blocker table names each example's FIRST stop, so a
row of size *n* is a lower bound on *n* examples' worth of work and says nothing
about how much. Every previous row happened to be one feature deep. This one was
five, and the only way to find that out was to lower the first item and look.

The lowerings stand — `utf8valid` over the shared Höhrmann table, `bytes`,
`slice` with both of its traps — and they have a running test rather than an
example, because an untested lowering is the thing this RFC keeps refusing to
ship. What that test checks is the *failures*: an embedded NUL rejected before the
UTF-8 check and with its own wording, an overlong form, a surrogate, a lone
continuation byte, `> U+10FFFF`, a truncated sequence, and `slice`'s two traps.
Comparing two backends would pass if both were confidently wrong about which
failure happened, so the interpreter's own answer is pinned in the assertion.

### The bug only running could catch, again — and it was not in any of the eight

`box_value` took **one** scratch local for the value and for the box's address.
`Fn_::scratch` is keyed on `(ValType, n)`, so for an i32-shaped payload — a
`String`, a `Bool`, a `UInt8` — `scratch(I32, 2)` twice is one local: the
`LocalTee` of the `malloc` result clobbered the value, the store wrote the box's
address into the box, and `print` of the payload showed the pointer's bytes.

It compiled, it validated, and no passing example had ever boxed an i32 scalar
into a sum's word — every one was an `i64`, which takes a different scratch key
and so never collided. `tagged.vyrn`'s `StrVal(userName)` is the first, and it
arrived only because `value` unblocked the file. The two shapes still absent from
the corpus (`BoolVal`, and `charCount` at all) have their own running test.

### One row of the sized-int table came along, and only the provable half

`Eq` on `UInt8` was in the way, so narrow unsigned ints are now lowered:
comparison, `+`/`-`/`*`, truncation into the width, zero-extension out of it, and
`toString`/`print`.

`UInt8`/`UInt16` **only**, and the boundary is not taste. Those are the widths
whose zero-extended `i32` compares the same signed or unsigned — so `binary`
needs no second comparison table — and for an unsigned type a mask after the
operator IS the wrap `wrap_intn` performs. A signed narrow int needs the opposite
extension (and `load_of` zero-extends), and a `UInt32` needs unsigned compares.
Both are still refused, and `Add` on `Int32` still names them. `Int64(x)` reaching
the same seam is what closed `a conversion from Int64 to UInt16` too, one row it
was not filed under.

### `toJson` is refused, and the reason is not its size

It is not a builtin with no lowering. It is a **serializer**, and the textual
backend's version is roughly 300 lines plus a *generated function per
payload-bearing enum* (`__vyrn_enc_<name>`, a tag switch, so a self-referential
payload becomes a call), plus RFC-0024's wire tagging, plus `Map`, tuples and
validated names. Under it sits the shim's JSON DOM and `vsb_escape`, which own
key order, escaping and number formatting — the bytes parity compares.

A direct backend has no shim, so it needs its own: an output buffer, an escaper
that agrees byte-for-byte with `vsb_escape` including `\u00xx` under 0x20, and a
runtime tag dispatch per enum. That is a milestone, and two of its six examples
(`storage`, `domdemo`) have further blockers behind it anyway.

Also refused, specifically: `readLine` and `parse` (2 examples, and only
`input.vyrn` needs *just* those two — `vlog.vyrn` is a whole CLI behind them),
`args`, `readFile` and `hostNowMillis` (each needs WASI syscalls and, for the
fixed clock, `environ_get`), `chars` (a UTF-8 *decoder*, where `bytes` needed only
a copy), `cell`/`get`/`set` (a generational slot table, which is also what the
`a branch yielding get` row really is), `fromJson` (the parser half of `toJson`),
and `logger`.

### The widening `abi` exists for was NOT exercised, and cannot be here

The brief said `toJson` is where M0's one live ABI mismatch lives — the emitter's
`ptr @__vyrn_vj_bool(i1)` against the shim's `VJ* __vyrn_vj_bool(int)` — and that
M2g would finally exercise `wasm::abi`'s widening.

It cannot. **`direct::compile` imports exactly two functions**, `fd_write` and
`proc_exit`, both `i32` throughout. There is no shim beside a directly-emitted
module (M2b: `vyrn build --target wasm` produces ONE module) and `Module::import`
is called nowhere else, so the entire 68-signature boundary M1 audited is
*unreachable from this backend today*. `abi`'s widening is dead code with a unit
test, and it stays that way until M4 moves the prelude into the shim and the
direct backend starts importing from it. `tests/imports_vs_shim.rs` keeps guarding
the textual emitter's side, which is the side that has the boundary.

That is worth recording as a fourth falsified assumption, in the same direction as
the other three: the milestone that finally makes `__vyrn_vj_bool` a live call is
M4, not this one — and when it comes, it arrives with every other shim import at
once rather than one builtin at a time.

### Nothing contradicted M0, M1, M2a–M2f

Every offset is still `layout::of_ll ∘ llt`, including the `{ i1, i64, i64 }` the
`stringFromBytes` runtime writes through — it reads its own field offsets from
`of_ll` rather than spelling 0/8/16, so the runtime and the lowering cannot
disagree about where the tag is. Destination-first, no-`return`-in-a-body and the
memory map went in as written; the aggregate-through-a-hidden-pointer convention
covered a *runtime* function returning an aggregate with no change at all.

### 30 of 80, regrouped

| blocked on | n |
|---|---|
| builtins with no lowering (`toJson` 6, `readLine` 2, `args`, `cell`/`set`, `chars`, `fromJson`, `hostNowMillis`, `logger`, `parse`, `readFile`) | **17** |
| `Match` on strings 4, a map literal 3, indexing a `Map` 2, indexing a `SmallArray` | 10 |
| sized-int and float arithmetic (`Shr` on `UInt64` 4, a float literal 2, `Add` on `Int32`, `BitAnd`) | 8 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda | 4 |
| a branch yielding an unpeekable call (`get` 3, `Held` 1) | 4 |
| `if let` 2, `?`, a fallible construction | 4 |
| `spawn` 2, `region` | 3 |

The builtins row finally has a shape: `toJson` is a third of it and is one
subsystem, and everything else in it is a WASI syscall or a runtime data
structure. The row that grew is sized ints and floats — 4 → 8, because the
`stringFromBytes` chain deposited four examples there — and it is now the second
biggest, which makes it the argument M2c made about arrays and M2e made about
generics: one table, in one place, worth eight.

---

## M2h, as landed

The sized-int and float row. **36 of 80**, from 30, and the row is gone.

It was billed at eight and paid **six** — `bits`, `strings`, `modules`,
`floats`, `sizedints`, `templates`. The two that did not pass moved on
(`htmltree` to a branch yielding `El`, `vyxdemo` to `if let`), which is the
first time a row has been read as a lower bound rather than as a feature and
turned out to be worth most of it. M2g's warning still applies to the reading:
the six are what the ladder says, not what the table promised.

Unlike M2g's builtins it really was one table in one place, and the two halves
turned out to have nothing in common except the row they shared. Integers are
bookkeeping wasm forces on you; floats are four opcodes and one algorithm.

### The invariant that makes the integer table a table

wasm has `i32` and `i64` arithmetic and nothing narrower, so an `Int8` rides an
`i32` and every operator that can overflow leaves it out of range. Written down
as `Num`, the rule is the interpreter's own: a value is **correctly represented**
in its carrier — sign-extended when signed, zero-extended when not — which is
exactly where `wrap_intn` leaves it in an `i64`. `renorm` after every wrapping
operator is what makes that true, and having made it true, signedness picks an
*opcode* rather than a fixup: `div_s`/`div_u`, `shr_s`/`shr_u`, four orderings,
and `+`/`-`/`*`/`&`/`|`/`^` blind to it because two's complement is.

That is what M2e's "the arithmetic table is i64-only" note was pointing at, and
M2g widened it by exactly the one row that was in the way — `UInt8`/`UInt16`,
where a mask after the operator IS the wrap and a zero-extended `i32` compares
the same either way. Those were the widths that needed no invariant. Every other
one does.

### Two places would have been silently wrong

Both are the class this RFC keeps naming, and neither is arithmetic.

**A load.** `llt` prints `i8` for both `Int8` and `UInt8`, so the bytes in memory
do not say how to extend them — the same ambiguity the textual backend resolves
with a `sext`/`zext` at each use, except that there the use site is looking at
the type. Here it rides `load_of`, taken off the same type the shape comes off,
so a caller cannot forget it. A negative `Int8` in a record field or an array
element read back zero-extended is **197**, and 197 is a number rather than a
crash.

**The op width.** It comes from EITHER operand, which is the textual backend's
`numty` rule. Reading it off the left alone computes `0 - eight` (an `Int32`) in
64 bits — the *same answer* for `+`, `-` and `*`, because truncating a 64-bit
sum gives the 32-bit sum, and a *different* one for `/`, `>>` and every
comparison. So it is exactly the kind of hole that passes the examples that
would have caught it.

The integer resize also had to move **ahead of** `coerce`'s `ll`-equality
shortcut. `Int8` and `UInt8` are one shape and two representations, and the
shortcut would have swallowed that pair — M2d put validation before the same
shortcut for the same reason, which is now twice that the shortcut has been the
thing in the way.

### `%f` is an exact decimal conversion, and there is nothing to borrow

Float arithmetic needed no design: `f32` and `f64` are wasm value types, so
nothing renormalizes and a `Float32` operation rounds to single precision because
the opcode does. Printing is the milestone.

`%f` is not "six decimals" — it is the **exact** decimal value of the double,
rounded **half-to-even** at the sixth place. `{:.6}` and `printf("%f")` agree on
that, nothing computed in floating point does, and M2g established there is no
shim beside a directly-emitted module to take a `snprintf` from. So `f64_str`
does it properly, and the arithmetic falls out of one identity: a double is
`M × 2^E`, so `x × 10^6` has numerator `M × 10^6 × 2^E`. In base-10^6 limbs that
is a single multiply loop with two parameters — by 2 `E` times when `E ≥ 0`, and
no digit is dropped; by 5 `k = -E` times when `E < 0`, and the last `k` digits
are the fraction to round away.

Base 10^6 rather than the obvious 10^9 is the whole reason it is one loop: the
`× 10^6` the six places need is then a shift by one *whole limb*, i.e. a zero
limb at the bottom, so there is no separate scaling pass. Every limb operation
then stays inside an `i32` (`999999 × 5 + 5` is small), so nothing after
unpacking the mantissa needs 64-bit arithmetic.

`k` reaches **1074** for a subnormal, which is why the digits are left-padded to
`k + 1`. That is what deletes the edge cases rather than handling them: after the
pad there is always a kept digit to round, always a digit before the one being
examined, and always a spare byte in front for a carry that escapes.

Two conversions are also not the opcode they look like:

- **`trunc_sat`, not `trunc`.** wasm's plain `i64.trunc_f64_s` **traps** out of
  range, where LLVM's `fptosi` is undefined and Rust's `as` saturates — and the
  interpreter *is* Rust's `as`, which is the answer the ladder compares against.
  `Int64(10^300)` is `Int64.max`.
- **Float → sized int goes through 64 bits first** and narrows after, because the
  interpreter does `f as i64` then `wrap_intn`, and the two genuinely disagree:
  `Int8(1e10)` is 0 through an `i64` and −1 through an `i32` whose saturation
  clamped at `i32::MAX`.

### What was checked by running, and against what

`sizedints.vyrn` is the example that exercises wrapping at each width and it was
blocked on a float conversion, so until this milestone finished the ladder could
not see the signed narrow widths at all. Both halves therefore have running
tests pinning the **interpreter's** answers, because two backends can be
confidently wrong together:

- 33 numbers over every width, each one through a record field and an array
  element as well as a local, plus the two comparison mistakes that are plausible
  in both directions (a signed opcode reads `4000000000` as negative, an unsigned
  one reads `-59` as enormous).
- The five numeric traps at widths other than 64. The divide-overflow guard
  compares against **the width's** minimum, so `Int8` `-128 / -1` has to trap
  where a guard written for `Int64` silently returns −128. The shift trap is one
  unsigned `>=` covering both an over-wide amount and a negative one, asserted
  rather than argued.
- The two exact ties: `0.0078125` keeps its even `2` and `0.0234375` rounds its
  odd `7` up, so a half-**up** formatter passes one and fails the other.
- `10^300`'s 301 digits pinned whole, because a carry bug in the doubling loop is
  a wrong digit in the *middle* rather than at either end; a subnormal at the
  loop's full depth; `NaN`, `inf`, `-inf` and `-0.0`, which are spelled rather
  than computed.
- 400 randomly generated doubles off-tree, byte-identical — worth doing once and
  not worth committing, since the curated cases are the ones that fail.

Vyrn has no exponent literals, so the extreme values are built by
multiplication. That is better than a literal: both engines reach the same double
by the same IEEE steps, and the mantissa that comes out is messy rather than
round.

### Refused, specifically

Nothing new, and that is worth saying because every milestone since M2b has
refused something. `%` and the bitwise family on a float are type errors in the
checker, so there is no lowering to refuse; every integer width and every
operator RFC-0045 defines is now emitted. The only thing this milestone leaves
behind is a note rather than a gap: `f64_str` is ~300 lines of emitted wasm that
M3's varargs shim and M4's prelude will make redundant, and it is here rather
than deferred because a float that does not print is a float nothing can test.

One divergence found rather than introduced, recorded because it is not this
backend's: `NaN != NaN` is **true** in the interpreter (Rust's `!=`) and in wasm
(`f64.ne` is the negation of `f64.eq`), and **false** natively (LLVM's `fcmp one`
is "ordered and not equal"). No example compares a NaN, so parity has never seen
it. Matching the interpreter is the rule the ladder is written to, and it happens
to be free here.

### Nothing contradicted M0, M1, M2a–M2g

Every offset is still `layout::of_ll ∘ llt`. Destination-first, no-`return`-in-a-
body — `f64_str` is one `block (result i32)` with a `br` for the non-finite
cases, which is the M1 rule applied to a runtime function rather than a user one
— and the memory map went in as written. The M2d seam took both numeric
conversions without a second path, which is the third milestone in a row that
`Fn_::coerce` has absorbed a new kind of reconciliation rather than growing a
sibling.

The one thing that grew is the shadow stack's appetite: `f64_str` claims 1,744
bytes of frame for its limbs and digits, the largest single frame this backend
emits by two orders of magnitude. It does not recurse and it is a leaf, so the
64 KB below `STACK_TOP` still covers a deep call chain — but it is the first
frame big enough that the number is worth knowing.

### 36 of 80, regrouped

| blocked on | n |
|---|---|
| builtins with no lowering (`toJson` 6, `readLine` 2, `args`, `cell`/`set`, `chars`, `fromJson`, `hostNowMillis`, `logger`, `parse`, `readFile`) | **17** |
| `Match` on strings 4, a map literal 3, indexing a `Map` 2, indexing a `SmallArray` | 10 |
| a branch yielding an unpeekable call (`get` 3, `El`, `Held`) | 5 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda | 4 |
| `if let` 3, `?`, a fallible construction | 5 |
| `spawn` 2, `region` | 3 |

Six rows, and for the first time none of them is arithmetic or a type. `toJson`
is a third of the biggest one and is a subsystem (M2g said so and it is still
true); everything else in that row is a WASI syscall or a runtime data structure.
The two rows that are neither — `Match` on strings with a map literal, and the
`peek` failures — are both the same shape of thing as the rows M2b through M2f
closed, which is to say small and named.

---

## M2i, as landed

The link. A directly-emitted module can import RFC-0076's shared shim instead of
standing alone, memory included — `direct::compile_linked(program, Link::Shim)`,
selected by `VYRN_WASM_BACKEND=direct-shim`, run as
`wasmtime --preload env=<out>.shim.wasm <out>`.

**36 → 37 of 80**, and the number is not the point. What this milestone found is
that M3 and M4 were written for a backend that no longer exists, and the
reordering below is the deliverable.

### The mechanism was RFC-0076's, and reusing it was the whole design

`toolchain::shim_wasm` is the clang half of what `vyrn-genwasm::build_shim` used
to do alone, moved down so `vyrn-codegen` can reach it, with the cranelift half
left where it was. `SHIM_BASE` moved to `wasm.rs` beside the `STATICS_LIMIT`
derived from it. Neither is tidiness: `--global-base` and `-z stack-size` at that
address ARE the memory map, and two copies of a memory map are a memory map that
can disagree with itself — the failure RFC-0076 M6 spent a milestone finding once.

Nothing about the map needed negotiating, which was M0's prediction and is now
true of a second consumer. The `STATICS_LIMIT` assertion covers the split for
free — it is the same 8 MB whether a shim is beside the module or not — and it
covers one thing it was not written for: the memory's declared minimum is
`data_end` rounded up, and the shim's actual memory is 256+ pages, so an import
that fits under the ceiling fits the shim's memory by construction.

Two spike findings, both of which would have cost a confusing afternoon:

- **An imported memory must still be EXPORTED.** wasmtime's WASI reads an iovec
  out of the *main* module's memory, so a module that only imports one dies on
  its first `print` with `missing required memory export`. An imported memory is
  index 0 too, so re-exporting it is the entire fix — but nothing about the
  encoder said so, and `Module::import_memory` had been dropping the export since
  M1 with a unit test asserting that it did.
- **The shim is a MODULE, not a library.** `--preload env=shim.wasm` registers it
  under the namespace the imports name, and wasmtime links the two at
  instantiation. That works with a reactor shim that itself imports WASI, which
  was the one thing worth checking before building anything.

### The widening is live, and it fails in two different ways

`wasm::abi` has been dead code with a unit test since M1 because
`direct::compile` imported exactly `fd_write` and `proc_exit`. It is now on every
shim import, and the signatures come from `wasm::boundary` — the textual
emitter's own `declare` lines, parsed once and shared with the audit test, whose
private copy of the same parser is deleted. `SHIM_IMPORTS` is therefore a list of
NAMES: writing a signature down beside the one `imports_vs_shim.rs` proves
against the C would have been a second chance to get it wrong.

`tests/shim_link.rs` is that audit with wasmtime doing the checking: **68
signatures** declared as imports and resolved against the module that defines
them. And the two ways one can be wrong are not the same failure, which is worth
recording because only one of them is what M0 warned about:

- **M0's `i1`.** Mis-mapped to an `i64` parameter it never reaches instantiation
  — the caller pushes what `abi` said the value was, and wasm's own type checker
  rejects the module. The widening is load-bearing at *emission* time, not at
  link time, and `__vyrn_vj_bool` could not have been got wrong quietly.
- **A signature on an import nothing calls.** `__vyrn_now_millis` returning `i32`
  instead of `i64` — the `size_t` shape M1 named — validates perfectly and fails
  when the two modules meet, by name. That is the class the whole boundary was
  unchecked-by-running for, and it is unreachable for a module with no shim.

### Shared memory, proved by three parties

The guest takes eight bytes out of the shim's `dlmalloc` heap, asserts the
pointer is above `SHIM_BASE` (a private heap in a private memory would also
"work", right up until something on the other side read it), writes `"hi"` into
them, and C reads the length back. `__vyrn_vj_bool(true)` then encodes to `true`
through the shim's own JSON writer. One address space, or the numbers do not come
out.

In the emitter the split is **five instructions**: `malloc` is a wrapper whose
signature is the one the emitted runtime already calls, so it became
`i64.extend_i32_u; call __vyrn_malloc` and ~20 call sites did not move.

### What reaching the shim actually bought: three symbols

RFC-0043's host boundary. `hostNowMillis`, `hostMonotonicNanos` and
`hostRandomSeed` are real shim symbols on every target rather than `vyrn` host
imports — which is what makes a clock example a three-way parity citizen — so a
linked module simply calls them, and `clock.vyrn` passes with the fixed clock and
seed honoured through WASI's `environ_get`. A standalone module still refuses
them by name: it has nowhere at all to get a clock.

That is one example, and every other row of the blocker table is exactly where
M2h left it. **All 36 standalone passes hold under the link**, which is the result
worth having: the memory map carries the entire existing backend unchanged.

Gated by a second ladder tier rather than a flag on the first, for the reason this
RFC keeps giving — `vyrn-codegen-llvm` rotted to unbuildable in twelve days — and
written as the *delta*, so a standalone pass that stops passing under the link is
reported as a regression in the link. One loop, two lists. The shim tier needs
clang and a wasi sysroot, because the shim is C; the standalone tier still needs
only a `wasmtime` binary, and it is the one carrying this RFC's acceptance
criterion.

### `f64_str` was not retired, and could not have been

M2h left a note saying its ~300 lines and 1,744-byte frame would become redundant
once the shim was reachable. They did not. `%f` needs `__vyrn_snprintf`, which is
one of the **three variadic** boundary functions — the only three a linked module
cannot import at all, because wasm has no varargs. So the shim being reachable
does not make `snprintf` reachable, and it will not until M3.

Even with M3 done, retiring `f64_str` would mean deleting exact half-to-even
decimal conversion that parity proves — the two exact ties, `10^300`'s 301
digits, a subnormal at `k = 1074` — in favour of a 353 KB C dependency. It stays.

### The measurement that reorders the RFC

`examples/fib.vyrn` to wasm:

| | module | beside it |
|---|---|---|
| `direct` | **2,836 bytes** | — |
| `direct-shim` | 2,914 bytes | 352,870 bytes of shim |

The split makes a directly-emitted module *larger*, and the reason is not the
shim's size — it is that M2b through M2h already emitted the runtime the shim
would have supplied. `strlen`, `strcmp`, `int_str`, `concat`, `print`,
`utf8valid`, `slice`, `f64_str` and the trap messages are all in that 2,836
bytes, all parity-proven, and the split replaced exactly one of them.

Which falsifies M4 as written:

> **M4 — the prelude.** The 1,080-line hand-written IR prelude moves into the C
> shim, which RFC-0076 M6 already compiles once.

That was the right plan for a backend with no runtime of its own. This one has
one, and moving it back would mean deleting working code in favour of a C
toolchain dependency, a second file, and a `--preload` flag at every run — for a
module that gets bigger. It is the fifth assumption this corpus has falsified,
and like the other four it is in the direction of less work.

It also puts M4 and M5 in direct conflict, which nothing said before: M5's
acceptance is that `vyrn build --target wasm` needs no clang and no sysroot, and
a module that imports the shim needs both. **The shim link can therefore never be
the default for `vyrn build`.** That is why `direct-shim` is a third value of a
temporary switch rather than a replacement for `direct`, and why the standalone
ladder is the tier that gates the acceptance criterion.

### What is left of M3 and M4

- **M3 — STRUCK.** See the M3 line above and "M2j, as landed": the direct
  backend emits zero `printf`-family calls, cannot import a variadic function,
  and therefore never had call shapes to generate wrappers from. This paragraph
  contradicted that and is retained only as a marker of where it was.
- **M4 — not the prelude any more.** What survives is the narrow version: reach
  the shim for the subsystems that are not worth re-emitting, which the M2i
  mechanism now makes a per-builtin decision instead of a milestone-sized move.
  The three that qualify on the current blocker table are the JSON DOM and
  `vsb_escape` (`toJson` 6, `fromJson` 1 — M2g refused `toJson` as "a serializer,
  and the textual backend's is ~300 lines plus a generated function per
  payload-bearing enum"; with the DOM importable what is left is the per-type
  encode walk, which is emitter work rather than runtime work), the WASI I/O
  helpers (`readLine`, `readFile`, `args` — 4), and `cell`/`get`/`set`'s
  generational slot table. All of them land in the shim tier only, and the
  standalone tier will keep refusing them by name until someone decides the
  emitted runtime should grow instead.
- **M5 — unchanged, with one constraint made explicit.** Deleting the LLVM wasm
  path requires the standalone shape to be complete, not the linked one.
  `VYRN_WASM_BACKEND` still does not survive it, and now neither does
  `direct-shim`.

### Nothing contradicted M0, M1, M2a–M2h

Every offset is still `layout::of_ll ∘ llt`. Destination-first,
no-`return`-in-a-body, the widening rule and the memory map all went in as
written — the map twice over, since it is now shared with a second module and
needed no adjustment for it. `Fn_::coerce` took nothing new this milestone, which
is the first time since M2c that it has not.

### 37 of 80, regrouped

The standalone tier is unchanged from M2h. The shim tier is that list plus
`clock.vyrn`:

| blocked on | direct | direct-shim |
|---|---|---|
| builtins with no lowering (`toJson` 6, `readLine` 2, `args`, `cell`/`set`, `chars`, `fromJson`, `logger`, `parse`, `readFile`) | 17 | **16** |
| `Match` on strings 4, a map literal 3, indexing a `Map` 2, indexing a `SmallArray` | 10 | 10 |
| a branch yielding an unpeekable call (`get` 3, `El`, `Held`) | 5 | 5 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda | 4 | 4 |
| `if let` 3, `?`, a fallible construction | 5 | 5 |
| `spawn` 2, `region` | 3 | 3 |

`hostNowMillis` is the row that left. Of the sixteen that remain, seven are the
JSON subsystem and four are WASI syscalls — i.e. eleven of them are now a
*decision* about which tier should own them rather than a lowering nobody has
written, which is what M2i changed and the reason it is worth its 37.

---

## M2j, as landed

RFC-0014's input I/O and RFC-0043's host boundary, over raw
`wasi_snapshot_preview1`. **39 of 80 standalone**, from 36 — and the shim tier is
39 as well, because its entire delta went away.

### The milestone is that `clock.vyrn` moved, not that three examples did

M2i got `clock.vyrn` passing by reaching the shim, and then found two things that
between them make that not count: the split makes a directly-emitted module
*larger*, and M5's own acceptance criterion (`vyrn build --target wasm` needs no
clang) forbids the link from ever being the default. A shape that only works
linked does not advance this RFC.

So the question was whether the three symbols the shim was supplying —
`__vyrn_now_millis`, `__vyrn_monotonic_nanos`, `__vyrn_random_seed` — could be
served standalone. They can, and the reason is that they never needed C in the
first place: the shim's implementations are `getenv` + `timespec_get` +
`getentropy`, and on `wasm32-wasip1` those **are** `environ_get`,
`clock_time_get` and `random_get`. wasi-libc is a wrapper this backend was paying
a whole C toolchain to call. The emitted runtime calls them directly, in **both**
link shapes rather than one, and `SHIM_IMPORTS` is down to `__vyrn_malloc` — the
one import whose whole purpose is to prove the two modules share an address
space.

`PASSING_SHIM` is therefore empty, which is the number worth reading this
milestone by. `direct-shim` still runs, still audits 68 signatures against the C
in `tests/shim_link.rs`, and no longer passes anything `direct` does not.

### Twelve imports, unconditionally, and the alternative was the mistake

An import is declared before the first body (M1: one index space, imports at the
bottom), and nothing knows which builtins a program reaches until the bodies are
walked. So either every module imports the whole WASI set it *might* use, or
there is a pre-scan over the AST — a second traversal that has to agree with
lowering about what it needs. That is the failure mode `llt_of` (M2b) and
`predicate_binds` (M2d) exist to prevent, and the one M2e refused a standalone
instantiation walker over. Twelve unconditional imports it is.

It costs less than it looks like, because the set is already implemented twice:
wasmtime provides all of preview1, and `web/wasi-min.js` implements exactly these
for the browser with RFC-0014's graceful degradation — no argv, EOF on stdin, no
preopens, every `path_open` NOENT. Two names had to be added there
(`environ_sizes_get`, `environ_get`) and an empty environment is precisely the
right answer: it sends a page's `now()` to `clock_time_get`, where
`hooks.fixedTime` already is.

### The wording is now one list, and it was already two

The canonical RFC-0014 messages are `IO_MESSAGES` in `lib.rs`. The textual backend
interns them as `@.io.<name>` globals and renders them with `__vyrn_snprintf`;
this backend has no `snprintf` and splits each format on its `%s`, so
`` cannot read `%s` `` becomes two interned halves and one `concat`. A backend
that spelled `cannot read` itself would have been a second wording of a fact
parity compares byte-for-byte.

Extracting the list also closed a hole that was already open: `direct.rs` held its
own copies of `bytes contain a NUL byte` and `bytes are not valid UTF-8` from M2g.
Two spellings, free to drift, exactly as the comment on `emit_predicate_cond`
feared about predicates in M2d — and found the same way, by needing the thing next
door.

### `readFile` and `writeFile` came along, and `path_open` is why

Neither was on this milestone's list. Both fall out of the one piece of real work
`readFile` needs, which is not reading — it is that **WASI has no `open` relative
to a working directory**. Every path resolves under a preopened directory the host
chose, so `open_at` walks the preopens from fd 3 until `fd_prestat_get` says there
are no more and takes the first that resolves the path. `--dir .` gives exactly
one; a browser gives none, and every path is then RFC-0014's canonical `Err`
rather than a crash. With that in hand, `readFileBytes` is the same slurp with no
NUL and no UTF-8 rule, and `writeFile` is the same open with two more flag bits.

`files.vyrn` needs all three, so it is the example those two bought.

The `Ok` payload of `readFileBytes` is the one shape that needed care: an
`Array<UInt8>` triple is three words and a sum's payload is two, so the runtime
boxes it — the same `Word::Boxed` encoding `Fn_::box_value` produces, at the same
`layout::of_ll ∘ llt` offsets, because a runtime function that spelled 0/8/16
could disagree with the lowering about where the tag is.

### What has no example, and therefore has a test

Three examples moved and between them they exercise the happy path only:
`args.vyrn` runs with an **empty** argv (it has no `.args` fixture, deliberately —
the harness gives every example zero arguments), `files.vyrn` reaches one of
`readFile`'s three failures, and **nothing at all reaches `readLine`**, because
`input.vyrn` needs `parse` and `vlog.vyrn` needs `fromJson`. Both moved *to* those
from `readLine`, which is the burndown working, and neither passes.

So the edges are a running test pinning the interpreter's own answers:

- `\r\n` and `\n` read identically, or Windows and POSIX pipes disagree; an empty
  line is `Some("")` and not `None`; a final line with no terminator is still a
  line. And `None` is three different things — EOF, a NUL byte, and invalid UTF-8.
- `readFile`'s NUL rule fires **before** the UTF-8 check and with its own wording,
  while `readFileBytes` of the same file **succeeds** — which is what makes them
  rules about `String` rather than about reading. A reader that validated first
  would report the wrong one of two plausible messages.
- An argv token with a space in it, which is what says the pointers are read out
  of WASI's own array rather than re-split.

### The bug only running could catch — and this time it was the test's

`nul.bin` is a Windows reserved device name **with any extension**, and wasmtime's
capability-based path resolution refuses one. The fixture read as five bytes under
the interpreter and as `cannot read` under wasm, which looked exactly like a
`readFile` that had lost its NUL rule. Recorded because the next person to write a
filesystem fixture on this platform will pick the same obvious name.

### The size, honestly

`examples/fib.vyrn`: **5,167 bytes**, from M2i's 2,836. It uses none of this — the
whole I/O runtime is in every module whether it is reachable or not, which has
been true since M2b and is now about 2.3 KB rather than 500 bytes. Still fiftyfold
under the 277,438 the clang path produces, and the fix when it matters is a
reachability sweep over the *finished* call graph, which is what a linker does and
is not a second source of truth about what a program needs.

### Is M3 needed at all? No, and it never was for this backend

M3 was written for a backend that would call the shim's `printf`, `fprintf` and
`__vyrn_snprintf`, on the premise of 377 `printf`-family sites. Checked rather
than assumed:

| | `printf`-family calls |
|---|---|
| textual emitter (`lib.rs`) | 39 sites, the 377 uses among them |
| direct emitter (`direct.rs`) | **0** — six mentions, all of them comments |

A directly-emitted module's entire import list is the twelve WASI functions plus,
under the link, `__vyrn_malloc`. **None is variadic, and none could be**: wasm has
no way to express a variadic call, so this backend was never able to plan on one
and has emitted its own formatter at every point the textual backend reaches for
`printf`. `print_i64` (M2a), `trap_idx` (M2c), `f64_str` (M2h) and now `err3` are
each the place a `%lld`, a `%d`, a `%f` and a `%s` would have gone.

So M3 has no call shapes to generate wrappers from. Its 377 sites belong to the
path M5 deletes, and native — which keeps that path — has real varargs. **M3 is
struck**, and the milestone list now says why.

That also settles the loose end M2i left. It said `f64_str` could not be retired
because `%f` needs `__vyrn_snprintf`, "the only three a linked module cannot
import at all", and that M3 would change that. It would not have. `f64_str` is
permanent, and it should be — it is exact half-to-even decimal conversion that
parity proves against `10^300`'s 301 digits and a subnormal at `k = 1074`, and the
alternative is a 353 KB C dependency.

### Refused, specifically

Unchanged, and named rather than lowered hopefully: `toJson` (a serializer — M2g's
reasons all still hold), `fromJson`, `chars`, `parse`, `logger`,
`cell`/`get`/`set`. None of them is a WASI syscall; every one is a subsystem or a
data structure, which is what M2i predicted the residue would be.

Three deliberate ceilings, marked in the code rather than left to be discovered:

- **One `fd_read` per byte** in `getbyte`, where C's `getchar` is buffered.
  `readLine` is the only caller and the corpus feeds it a fixture.
- **No prefix matching against the preopens' own names**, so an *absolute* guest
  path only opens under a preopen mounted at `/`. wasi-libc does that matching for
  the textual backend; nothing in the corpus has an absolute path, and the fix is a
  string walk over `fd_prestat_dir_name` for a case no example has.
- **`str_i64` is not `strtoll`**: no leading-whitespace skip, no clamp to
  `LLONG_MAX`. Its only callers are `VYRN_FIXED_TIME` and `VYRN_FIXED_SEED`, which
  the harness writes as bare decimals.

`write_all` also does not report a partial write, where the C `writeFile` returns
status 1 on one. It is shared with `print` and the trap path, no example can
observe the difference, and giving it a return value would make every one of its
callers responsible for checking it — which is the shape of the M2b bug it was
written to prevent.

### Nothing contradicted M0, M1, M2a–M2i

Every offset is still `layout::of_ll ∘ llt` — the array triple `args` builds, the
sum `read_line` writes, the box `read_file_bytes` allocates. Destination-first at
the slot every one of these runtime functions writes through,
no-`return`-in-a-body (each is one `block` with `br` to it, which is why
`read_line` has a `none` block and a `fin` block rather than an early exit), the
widening rule and the memory map all went in as written. `Fn_::coerce` took
nothing new, for the second milestone running: every one of these builtins
produces a value of a type the seam already knew how to flow.

### 39 of 80, regrouped

Both tiers, and they are now the same list:

| blocked on | n |
|---|---|
| builtins with no lowering (`toJson` 6, `parse` 2, `fromJson` 2, `cell`/`set` 2, `chars`, `logger`) | **14** |
| `Match` on strings 4, a map literal 3, indexing a `Map` 2, indexing a `SmallArray` | 10 |
| a branch yielding an unpeekable call (`get` 3, `El`, `Held`) | 5 |
| `if let` 3, `?`, a fallible construction | 5 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda | 4 |
| `spawn` 2, `region` | 3 |

The builtins row is 14 from 17, and for the first time it is not the whole story:
`toJson` and `fromJson` are one subsystem worth 8, and what is left beside them is
four small things. Every other row is exactly where M2h left it. The two worth 15
between them — the `Map` family, and the five `peek` failures — are the same shape
as everything M2b through M2f closed, which is to say small, named, and not
control flow.

## M2k, as landed

`if let`, `?`, and `Age?(n)`. **45 of 81 standalone**, from 43 — and the
milestone is not the two examples.

### The result is `std/jsonread`, and it is why this row was next

RFC-0078 M3 is `fromJson`, which needs `std/jsonread`, which was unbuildable here
for exactly two constructs: `?` at six sites and `if let` at one. It now compiles
under `VYRN_WASM_BACKEND=direct` and parses byte-identically to the interpreter —
a nested document, a surrogate pair (`readHex4`'s `?` twice in one expression, the
only nested one in the module), and four rejections whose `line N, col M:` wording
comes out through six propagating frames. So M3 can land on all three engines at
once rather than on the interpreter and the native build while wasm waits, which
is a stronger claim than any number of examples.

The examples, honestly, went the other way. The M2j table read `if let` 3, `?`, a
fallible construction — 5. `option.vyrn` and `validate.vyrn` arrived; all four
`if let` examples had a *second* blocker and moved to it (`argsdemo` to `parse` in
a branch, `vyxdemo` to `lineAt`, `controlflow` to `region`, `vlog` to `fromJson`).
A row of 5 was worth 2. That is the first time a row has been read as an upper
bound and been wrong in this direction; M2h's was read as a lower bound and was
right.

### `if let` needed a lowering, and is still sugar

The parser keeps it as `Stmt::IfLet` — `while let` is the construct that
desugars, onto *it* — so there was no desugar to inherit. But it is sugar in
shape: one tag test, the payload bound on the taken side, and no join at all,
because the statement form carries no value. So it is `match_expr` with the arm
chain replaced by a single `if`, over the same `pattern_binds` and `bind_payload`,
and `peek` is not involved.

The tag test itself is now one function the three probes share (`match`, `if let`,
`?`); a second spelling of it would be a second chance to read the tag at the
wrong width — one byte for `Option`/`Result`, eight for a user enum — which is the
silent class, not a compile error.

The scrutinee's address goes in a local of its own rather than in shared scratch,
because it has to survive the test AND the binds, and an `if let` nests.

### `?` routes to the epilogue, and that was the whole design

M1's rule is that a body must not emit `return`: the shadow-stack release, and
since M2f the `modify` copy-back, sit AFTER the block every exit branches to. `?`
is an early return in everything but the instruction, so it writes the sum through
`dest` exactly as `Stmt::Return` does and takes the same `br` to the same block.
Nothing else was needed — `drop` is a no-op in this backend (the allocator never
reuses), so the textual emitter's `emit_all_drops` before its `ret` has nothing to
answer here.

Three things fell out of that shape rather than being arranged:

- **The success path is the fall-through, not an arm.** The failing side branches
  away, so there is no join, no destination to allocate, and no `peek` to predict
  wrong. `?` is the first value-producing construct in this backend that is not a
  join.
- **A value may sit beneath it**, which M2d already required of `if`: the test
  consumes only the tag, so a destination address parked under the operand stack
  by `let r: Rec = f(g()?)` is untouched.
- **The propagated value is the whole sum, byte for byte.** That is only sound
  because both sides are `{ i1, i64, i64 }`, differing at most in a payload half
  the failing tag says is not there. The textual backend gets this free
  (`ret { i1, i64, i64 } %agg`); a `memory.copy` has a width, so the width is
  checked rather than assumed, and a `?` whose enclosing return is a different
  shape is refused.

**How the no-leak claim was checked**: 20,000 calls that each propagate, and the
point is that a wrong lowering is loud. Emitting a real `Instruction::Return`
there once traps `out of bounds memory access` before the first `print` — 20,000
unreleased frames having walked the stack pointer past 0, which is the trap the
memory map was laid out to get (M0). With the `br` it prints 20,000, and that
number is the count of `modify` writes made *before* each propagation, so the same
test says the copy-back happens on the failing path too. Both are in
`tests/directwasm.rs`; neither is visible in a small program.

### `Age?(n)` is the one flow that steps AROUND the M2d seam

Every milestone since M2d has added a flow *ahead of* `Fn_::coerce`'s `ll`-equality
shortcut — validation, substitution, integer resize. This is the opposite, and the
reason is the whole point of the form: `expr_as(n, Age)` would emit the validation
that aborts, and `Age?(n)` exists so that it does not. So the argument is
evaluated at the refinement's **base** type and the predicate's own answer becomes
the tag, which is exactly what `gen_try_construct` does — a value the two backends
disagreed about would be a diverging `None`.

`predicate_holds` is split out of `emit_validation` rather than written twice.
Both need "run the `where` clause over the value"; only one of them traps. Two
spellings could disagree about what the predicate *binds*, and an `Age?(n)` that
read a different `value` than `Age(n)` does would be a `None` on one engine only.

### The bug only running could catch, and it is M2b's

`validate.vyrn` built, and then printed `error: validation failed for` Age where
the interpreter prints `-1`. The `?` and the construction were both right; the
`match` around them was not:

```vyrn
return match Age?(n) {
    Some(a) => a,      // an Age
    None => 0 - 1,     // an Int64
}
```

`peek` typed the join by its first arm, correctly, as `Age` — and then every other
arm was coerced *into* `Age`, through M2d's seam, and validated against a
refinement the language never asked it to satisfy. The checker unifies those two
arms at the base and asks nothing of the second one. **A join is not a value
boundary.**

So `join_ty` decays a refined type at the two sites that use `peek`'s answer as a
coercion target (`join` and `match_expr`), and nothing else changes: the boundary
the value really crosses — the `let`, the `return`, the field, the argument — still
validates, because that coercion is a separate one outside the join.

The hole is M2b's, not M2k's. A plain `match` on an `Option<Age>` has had it since
n-way arms landed, and no example held one; `validate.vyrn` becoming compilable is
what produced the first. This is the third time a milestone's new reach has exposed
an older milestone's silent assumption (M2b's `peek` gap, M2f's missing copy-back,
this), and all three were found by running.

### Refused, specifically

- **`?` whose enclosing return is not the same sum shape.** The propagation is a
  fixed-width copy of the whole aggregate; a function returning something else is a
  refusal naming both types, not a truncated copy.
- **A fallible construction over an aggregate base.** Only `emit_validation`'s
  record arm binds one, and it binds by *field* — there is no single local to
  become the payload word. `word2` would have to box it and `bind_payload` unbox
  it, which is mechanical, and a guess is a silent `None` rather than a trap.
- **`?` on a user enum.** It has no meaning; `Sum::Enum` is simply not a case.
- **The `peek` entries for `Try` and `TryConstruct`.** A branch *yielding* `x?` or
  `Age?(n)` is two lines of `peek` away, and no example or std module has one. The
  cost of being wrong is a named gap, so the two lines can wait for a caller.

### Nothing contradicted M0, M1, M2a–M2j

Every offset is still `layout::of_ll ∘ llt` — the sum `?` copies, the tag `if let`
tests, the two words `Age?(n)` writes. Destination-first still holds where there is
a destination, and `?` is the construct that showed the rule has an exception which
costs nothing rather than a case that breaks it. `Type::Param` stayed unreachable,
`Num`'s carrier invariant was not touched, and the LLVM wasm path is byte-for-byte
unaffected (parity: 6 passed, unchanged).

### 45 of 81, regrouped

| blocked on | n |
|---|---|
| builtins with no lowering (`fromJson` 5, `parse` 2, `cell`/`chars`/`lineAt`/`logger`/`set`) | **12** |
| `Match` on strings 4, a map literal 3, indexing a `Map` 2, indexing a `SmallArray` | 10 |
| a branch yielding an unpeekable call (`get` 3, `fromJson`, `parse`) | 5 |
| `spawn` 2, `region` 2 | 4 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda | 4 |
| a conversion from `Cargo` to `T` | 1 |

Control flow is off the list entirely. What is left is three groups: the `Map`
family at 10, a builtins row at 12 that `fromJson` alone is 6 of, and eight things
that are one gap each. `fromJson` is the next milestone by the same argument this
one used — it is RFC-0078 M3's other half, and `std/jsonread` compiling is now the
only reason it can be one.

## M2l, as landed

**The three containers, and one `peek` gap that was a whole class.** 54/87 → 63/87.
Nine examples: `autorelease`, `fieldmut`, `freelist`, `genref`, `linkedlist`,
`mapdemo`, `server`, `smallarray`, `tree`. `Map` and `SmallArray` both landed;
nothing in the cluster is refused.

The milestone was framed as "a container the direct backend cannot lower", and two
of its three rows were M2c refusals with specific reasons. Both reasons survived
contact; what changed is where they are paid.

### The `peek` row was not three examples, it was a class

The brief asked whether the "branch yielding `get`" rows might be one general fix.
They were, and the fix is worth more than the three examples: `peek`'s `Call` arm
fell through to `Cx::sigs`, which holds **user functions only**, so *any* builtin
in a branch position read as "a branch yielding `X`". Two changes closed the group:

- Route RFC-0078's Vyrn-implemented builtins through the same
  `loader::routed_builtin` the emitting path uses, so a builtin that IS a Vyrn
  function is typed by that function's signature rather than by a name written
  here twice.
- For the builtins `call` lowers by REWRITING, peek the rewrite. `fromJson`'s
  rewrite bottoms out in a generated decoder whose signature `Cx::sigs` already
  holds, so it needed no new case at all — which is the property worth keeping,
  because a type named here would be a second answer free to drift from the one
  the emitting path produces.

`storage` and `argsdemo`, both outside this cluster, moved past their `peek` gap
and now report what they actually want (`a conversion from Config to T`, `parse`).
The remaining hand-written `peek` rows are the ones with no callee to ask: a
literal, a constructor, an operator.

There is still no general mechanism, and there cannot be one while `call` computes
a builtin's result type as it emits. `peek` is a second source of truth for
exactly the builtins that are neither routed nor rewritten, and every milestone
that adds one to `call` owes it a row here. What M2l did was shrink that set to
the ones that genuinely have no other answer.

### The slot table is emitted, and it is the one thing the shim could never supply

RFC-0004 §4's generational references are **not** in the C shim. The LLVM build
gets them from a hand-written IR prelude (`CELL_RUNTIME`), so there was nothing to
import in either link shape — M2i's split is irrelevant here, and this is the
first runtime piece for which that is true.

Three functions, not the prelude's five, and the merge is not tidying: every
`get`, `set`, `release` and `drop` of a reference checks the generation and then
wants the payload address, so `cell_addr(slot, gen) -> ptr` **is** the check and no
caller can reach a payload without paying for one. `cell_new(dest, payload)` writes
the `{ i64 slot, i64 generation }` pair through a destination pointer, which is the
aggregate ABI (M2b rule 3) rather than a special case — a `Ref` is an aggregate.
`cell_release(slot)` bumps the generation and pushes the slot.

Two numbers are load-bearing and are worth writing down because both look
arbitrary:

- **65536 cells.** `autorelease` and `freelist` both run PAST it on purpose — a
  million allocations and a hundred thousand respectively. A slab of a different
  size would either exhaust where the other two engines do not, or hide a release
  that never fired. It is the prelude's number and it has to be.
- **Lazily allocated.** Statically reserving the three arrays would put 1 MiB of
  zeroes in every module this backend emits, `fib` included, because `runtime()`
  runs before any body is walked and cannot know whether the program uses a cell.
  So the arrays are one `malloc` behind twelve bytes of reserved state. Bump-
  allocated pages are fresh and therefore zero, which is what the prelude's
  `zeroinitializer` globals give it for free.

The payload is not freed on release. A bump allocator has no free, and a free is
not a thing a program can print — but the **slot** very much is.

### `get` of an aggregate copies, and that is the M2c hazard in a new place

Handing back the slab's payload address would make `get(r)` an alias into the cell
rather than a load of it, where the LLVM backend emits `load {ll}`. `freelist`
holds `Ref<Node>` and `genref` holds `Ref<Ref<Int64>>`, so this is reached; the
copy is three instructions and the alternative is a class of bug that prints the
right answer until something writes.

### The ownership analysis, which this backend had never read

`autorelease.vyrn` built and DIVERGED, which is the shape M2 keeps producing: it
allocates a cell per iteration and relies on the inferred release, and `Stmt::Drop`
here was a no-op on the grounds that reclamation is unobservable. That is true of
every kind `own::analyze` reports EXCEPT `ReleaseRef`. A missed `free` prints
nothing different; a missed release loses one of 65536 slots, and a million
iterations say so — loudly, with the interpreter's own wording, which is why it was
a divergence rather than a silent wrong answer.

So `own::analyze`'s `droppable` map is read here now, with the same key the textual
backend uses (the `let` statement's node address), and only its `ReleaseRef` rows
are acted on. Frames are per block, released innermost-first and newest-first on
fall-through; `return`, `break` and `continue` release every frame they unwind past
**before** branching, without popping it — so the enclosing block's own copy lands
after the branch, where wasm has already marked it unreachable. That is the textual
backend's rule arrived at from the other end: it keeps its frames for the same
reason.

### `Map` is the shape whose length is in the wrong place

`{ ptr keys, ptr vals, i64 len, i64 cap }` — the length is field **2** where a
growable array's is field 1. M2c's refusal was that reaching for it as a triple
compiles, validates, and indexes off the value pointer. It is now a branch rather
than a refusal: `at` and `IndexSet` test for a `Map` *ahead of* building a `Walk`,
and the map path never snapshots at all.

That last part is the other half of one fact. An `Array` is snapshotted so a `for`
keeps walking the buffer it started on, matching an interpreter that iterates a
copy. A Map has no iteration form to protect — `m.keys()` hands out a snapshot of
its own — while an insert moves *both* buffers, so every read has to go through the
header's address. The two containers want opposite things and the reason is the
same reason.

One runtime function, `map_find`. RFC-0028 chose insertion order over hashing, so
the linear `strcmp` scan IS the lookup and matching the shim's is not a
simplification. `reserve`, the order-preserving `remove` shift and the `keys`
snapshot are each reached from one site and are a `malloc` plus a copy, so they are
emitted there rather than becoming three more indices in a table whose numbering is
load-bearing.

**The value type comes from the position, not the first entry**, which is what
`fieldmut` is: `["k": [[5], [6, 7]]]` in a `Map<String, Array<Array<Int64>>>` has to
store growable arrays, and a nested literal on its own lowers as a fixed `[N x T]`.
Storing one at the literal's width and reading it back as a triple is M2c's hazard
one level down, and the textual backend has the same guard for the same reason.

`mapdemo` passing was not planned for. It is the whole RFC-0028 surface AND
`toJson`/`fromJson`/`jsonSchema` of a `Map`, and the codec half needed nothing:
RFC-0078 made both directions rewrites over a Vyrn library, so a `Map` on the wire
is just a `Map` in Vyrn. This is the second milestone in a row where RFC-0078's
work arrived as an absence of work here.

### `SmallArray`'s two states cost one function

`{ i64 len, i64 cap, ptr data, [N x T] inline }`, `cap == N` inline and `cap > N`
spilled. M2c's reason — first field a length where a growable array keeps a pointer
— is why it was the last row standing, and the thing that made it affordable is
that the hazard fits in **one** function. Every element access goes through
[`Walk`], and a `Walk` is a base pointer and a count, so the state branch happens
in `walk` once and nothing downstream knows there are two states. Only the four
header-mutating operations (`push`, `pop`, `swapRemove`, `toArray`) needed arms of
their own, and only `push` needed the spill.

A contextual `[a, b, c]` in a `SmallArray` position does not reach the heap: it
copies into the inline buffer and sets `cap` to `N`, which is the state
discriminant. That is a second `ArrayN` conversion at the M2d seam rather than a
second literal path — the fifth thing to move through that seam, after validation,
substitution, integer resize, and `Age?(n)`'s detour around it.

Refused, specifically: a `SmallArray` of a **two-word** value (a `Ref`, a stored
`fn`) in `pop`. The payload word there IS an address, so encoding it needs the
destination's second word rather than a `box_value`; nothing in the corpus holds
one, and guessing is the silent class.

### The bug only running could catch, again

An empty `[]` into a `SmallArray` shares the builder with the non-empty case, and
the builder starts by popping the source address off the stack — which the empty
case never pushed, because `[0 x T]` is not a shape `llt` prints and there is no
fixed literal to have produced one. wasmtime refused the module ("expected i32 but
nothing on stack") rather than running it wrong, so this one was loud. It is still
the eighth consecutive milestone whose real bug was found by executing something.

Two failures no example reaches are now pinned by a test that executes:
`a_stale_reference_and_a_full_slab_say_what_the_interpreter_says`. `genref` drops a
cell and never touches it again, and `autorelease` only proves the slab does *not*
fill — so a `cell_addr` that skipped the compare would pass every listed example,
and so would a slab of the wrong size, whose exhaustion message is
indistinguishable from a release that never fired. Both stderr strings are compared
against the interpreter rather than against a spelling written in the test.

### Nothing contradicted M0, M1, M2a–M2k

Every offset is still `layout::of_ll ∘ llt` — the Map's four fields, the
SmallArray's inline buffer at 24, the `Ref` pair at 0 and 8. A size is still a
stride. Aggregates still travel as the address of a shadow-stack slot, which is why
`cell_new` needed no new convention. `Type::Param` stayed unreachable, `Num`'s
carrier invariant was not touched, no body emits a `return` (`map_find` carries its
hit out of a block in a local rather than returning early), and the LLVM wasm path
is byte-for-byte unaffected (parity: 6 passed, unchanged).

One thing is *slightly* stricter than the textual backend and it is worth naming:
`map_set` evaluates the key and then the value **before** the scan, matching the
textual backend's order, because a side-effecting value expression must not run at
a different point on the two backends. Nothing in the corpus has one; the cheaper
order was the tempting one.

### 63 of 87, regrouped

| blocked on | n |
|---|---|
| `Match` on strings | 4 |
| the call `parse` | 4 |
| the call `lineAt` 3, the call `logger` 2 | 5 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda 2 | 5 |
| `spawn` 2, `region` 2 | 4 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

Every container is off the list. What remains is four groups and no long tail:
three builtins with no lowering (`parse`, `lineAt`, `logger` — 9 examples), the
RFC-0023/RFC-0037 function-value worklist (5), two control-flow features this RFC
scoped out from the start (`spawn`, `region` — 4), `Match` on strings (4), and one
generic-payload conversion twice (2).

The nine-example builtins row is the next milestone, and — unlike the last three
— **none of it is a routing question.** RFC-0078 checked and refused all three
deliberately: `parse` wraps on overflow where `std/num`'s `parseInt64` declines, so
folding them would be a language change; `lineAt`/`colAt` are a memoized
line-start table that a Vyrn library cannot hold, and M4b asserts they stay absent
from the routing table so a later edit cannot quietly add them. `logger` is
RFC-0008's leveled sink. All three want real lowerings here, and all three are
small and self-contained — the largest is `parse`, which is a digit loop with
wrapping arithmetic this backend already has in `int_op`.

What is genuinely large is what is left after that: `Match` on strings is
RFC-0046's DFA, and the function-value worklist is RFC-0023 plus RFC-0037's
defunctionalization, which M2a already identified as the one place this module has
a function table.

### The runtime table registers itself (not a milestone)

RFC-0078's census flagged the shape of `Rt`, the emitted runtime's function table,
as a hazard: the indices were hand-written as `base + n` with a `count` that had to
agree, so emission order was load-bearing, adding an entry renumbered every entry
after it, and a misnumber only failed loudly where the two signatures differed.
M2l's three cell functions had to get that right by care. So the numbering is now
handed out — `Rt::slots` appends and returns what it appended, `count` is the
length, and a struct literal that names every field means a field added and not
registered is a compile error rather than an index of zero pointing at `write_all`.

Order is still load-bearing, because a `call` carries an index; it is now
*honestly* load-bearing. Every one of the 35 emit sites is preceded by
`rt.next_is(m, rt.<helper>)`, which asserts that the function about to be written
is the one the table reserved, and `runtime` asserts the total on the way out. A
boundary check per group would have been cheaper and would have missed the case
that matters: `read_file` and `read_file_bytes` are both `(i32, i32) -> ()`, and
`malloc`, `strlen` and `charcount` are all `(i32) -> i32`, so exchanging two
members of either set keeps the count right and produces a module that *validates*
and then reads the wrong thing. Exchanging the two `read_file` declarations was
tried: it now panics while compiling `fib`, which calls neither. A test pins the
other half — one index per helper, dense from `base`, `count` equal to the number
handed out.

Pure refactor, and it is checked as one: `fib`, `mapdemo` and `freelist` emit
byte-identical modules before and after, and the ladder is unchanged at 63/87 on
both link shapes.

## M2m, as landed — function values

**Function values, both halves.** RFC-0023's higher-order specialization — the
second worklist M2e refused by name — and RFC-0037's stored `fn` values by
defunctionalization. **63/87 → 68/87**: `lambdas`, `rpc`, `rpcsplit`, `closures2`,
`fnvalstore`.

The five are the whole row, and unlike the last three milestones the row was worth
exactly what it said. Nothing here is refused.

### The function table is still unnecessary, and now it is measured for both halves

M2a's pre-flight found zero indirect calls and read that as RFC-0037's
defunctionalization holding. It does, and this milestone is where it was cashed:

- A `fn`-typed parameter's target is a compile-time function **index** — a lifted
  lambda, a named function, or a forwarded parameter — resolved at the call site
  that specializes the callee.
- A stored value is `{ i64 tag, i64 payload }`, and every call through one is ONE
  direct call to the signature's dispatcher, which switches on the tag and
  direct-calls the target.

So the emitted module has no table, no `ref.func` and no `call_indirect` for
either feature. The nine function-addresses-as-values M2a found are still exactly
`spawn`'s, and still the only ones.

### One queue, and then the thing that could not be queued

The textual driver runs **two** worklists that feed each other plus a dedup set
for lifted lambdas, and drains each with `pop()`. It is right to: nothing there
depends on order, because every reference is a name.

Here the order was the numbering, so the three kinds of discovered body — a
generic instantiation, an RFC-0023 specialization, a lifted lambda — went into
**one** append-only queue. "They feed each other" then costs nothing to arrange:
it is appending to the list you are reading, and M2e's FIFO invariant covers all
three instead of one.

Then RFC-0037's dispatcher broke the invariant outright, and that is the finding.
A dispatcher switches over every construction of its signature **anywhere in the
module**, so its body is complete only after the last body is walked — while its
index has to be callable from the middle of that walk. No ordering discipline can
arrange that. The alternatives were both worse: a pre-scan for constructions is
the second-traversal-that-must-agree M2e refused an instantiation walker over, and
an inline switch per call site would silently miss a variant registered later.

So `wasm::Module::reserve_func`/`fill` landed, and **every** index in the module
became a reservation — user functions, the globals initializer, specializations,
lambdas, dispatchers. M2e's note that "`insts` is append-only and `done` only
moves forward — FIFO by construction" is therefore superseded rather than
extended: the discipline is replaced by the mechanism it was standing in for, and
an out-of-turn body is now impossible instead of asserted against. That is the one
place this milestone contradicts an earlier one.

### Both new bodies are synthesized `Function`s, which is why there is no new lowering

A lifted lambda is a `Function` whose captures are ordinary **read** parameters. An
RFC-0023 specialization is the callee's `Function` with each `fn` parameter
replaced by its target's capture parameters. Both then go through `lower_body`
unchanged, and that is not tidiness — three things fall out of it:

- M0's by-value parameter copy IS the capture snapshot. An aggregate capture
  arrives as an address and the prologue copies it into a slot; RFC-0023 specifies
  exactly that, so it cost nothing.
- A target's signature is captures-then-parameters, so **calling** a target is
  `emit_call` with the captures prepended to the argument list. The aggregate
  convention, `modify`, and the M2d coercion seam have no second implementation to
  disagree with.
- A dispatcher is therefore a target with **one** capture, which is why a stored
  value flowing into a `fn`-typed parameter needed no third mechanism: the capture
  the specialization receives is the enum itself.

An instance's capture parameters are named `@cap..`, which no Vyrn identifier can
be. That matters once and silently: `on(p, |q| .. p.x ..)` where the callee's own
first parameter is also `p` binds the lambda's `p` to the capture, and a spelling
that could collide would bind it to the callee's and print a plausible number.

### What is shared, and one shared rule that was missing an arm

Two more free functions both backends call:

- **`lambda_captures`** — the capture WALK. A capture list is part of a lifted
  function's signature, so two backends disagreeing about its length or its order
  emit calls with the wrong number of arguments. Only "is this name an enclosing
  local?" is per-backend, and it is one closure.
- **`normalize_fn_sig`** — which spellings are one signature. It decides which
  constructions a dispatcher covers, so two backends grouping differently would
  give one of them a dispatcher missing a variant: a defensive trap where a call
  belongs, reached only by the spelling nobody wrote a test for.

And `solve_param` — the unification rule M2e made shared — **had no `Type::Fn`
arm**. So a generic record holding a `fn` whose parameter is the record's own type
parameter (`Deferred<P, T> = { run: fn(P) -> T }`, the `std/ui` `ParamQuery` shape)
solved nothing from its field and `applied_type` filled both in with `Unit`. The
direct backend then registered a variant under `fn(Unit) -> Unit` and dispatched
under `fn(User) -> String`, so `fnvalstore` built and hit the defensive arm. The
textual backend survived it by accident: it pushes the *unsubstituted* field type
as the expected one and its own `normalize_sig` re-applies the ambient
substitution, so the `Param`s were fixed on the way past. Adding the arm is
`emit-ir` byte-identical across all 89 examples, which is what says it was a gap
rather than a decision.

### The bug only running could catch

`each(xs, |x| print(x))` — a **Unit-returning** `fn` type over an expression body.
The lifted lambda synthesized `return print(x)`, and the return path correctly
refused a value the signature does not carry. It is a statement rather than a
return, which is the split the textual emitter reaches by testing
`llt(ret) == "void"`. Ninth consecutive milestone whose real bug was found by
executing something.

### Refused, specifically

- A **generic or itself-higher-order function as a target**: the first has no index
  until something fixes its type arguments, the second has no first-order
  definition at all.
- A **target taking a `modify` parameter**. A `fn` type cannot carry a capability,
  so passing one would change the ABI silently — the callee would be handed a value
  where it expects an address.
- A `fn`-typed argument that is neither a lambda nor a name, and a call through a
  stored value whose receiver is not a name. That is RFC-0037's own surface
  (calls-by-name-only), so it is a refusal with no source to refuse.

### What the ladder cannot see, and therefore has tests

Two running tests, both pinning the **interpreter's** answers rather than comparing
two backends:

`a_fn_typed_parameter_specializes_to_whatever_the_call_site_resolved` — an
aggregate capture whose name collides with the callee's own parameter, the same
capture forwarded through two boundaries, an aggregate parameter and return on the
`fn` type, two distinct lambdas of one shape at two sites (two instances, not
one), one literal inside a generic body under two instantiations (two lifted
copies, or the `Int64` copy gets a `String`), a block-bodied lambda, two `fn`
parameters in one specialization, and the Unit-returning signature above.

`a_stored_function_value_dispatches_by_signature_not_by_spelling` — a
Unit-signature slot holding a value-returning function (the dispatcher has to drop
the result), an aggregate return from a lifted lambda through a dispatcher's own
hidden destination, and `Make` against the bare `fn(Int64) -> Pt`, which must be
one enum or a tag built under one spelling falls through the other's switch.

### Nothing else contradicted M0, M1, M2a–M2l

Every offset is still `layout::of_ll ∘ llt` — the `{ i64, i64 }` a stored value is,
the capture block's fields, the sum payload a `fn` rides inline in (`Word::Inline2`
already said "a `Ref` or a stored `fn`", so `Option<Transform>` needed nothing).
Destination-first holds at the dispatcher too, where the aggregate result travels
through the dispatcher's own destination parameter with the call sitting on top of
it — M2d's "a value may sit beneath an `if`" applied to a call rather than to a
check. No body emits a `return`: a dispatcher is a chain of nested `if`/`else`
whose innermost `else` is the defensive arm, and its `unreachable` is what
satisfies a result-typed chain without any arm branching out. `Type::Param` stayed
unreachable, and the LLVM wasm path is byte-for-byte unaffected.

`Fn_::coerce` took nothing new, which is the third milestone in a row. A `fn` value
flowing between fn-typed spellings really is a re-tag only in the textual emitter,
and here it is not even that: two spellings normalize to one signature, so the two
`ll`s are equal and the seam's shortcut is correct rather than dangerous.

### The size

`fib.vyrn` is unchanged at 5,167 bytes — nothing here is in the emitted runtime.
`lambdas` is 8,162, `closures2` 10,812 and `fnvalstore` 11,183, which is the
per-program cost of one function per lambda plus one dispatcher per signature.

### 68 of 87, regrouped


---

## M2m, as landed

`spawn` and `join` (RFC-0025). **65/87**, from 63: `concurrency` and `parallel`,
on both link shapes.

The milestone is not the two examples. It is that the one wasm feature this RFC
had convinced itself it needed — a function table — is not needed, and the
measurement that "corrected" the original claim is where the mistake was.

### The nine addresses are real, and none of them is a function pointer

M2a's pre-flight counted 9 function-addresses-as-values across all 81 examples,
all of them `spawn`:

```
%t2 = call ptr @__vyrn_spawn(ptr @__vyrn_task_vyrn_fib, ptr %t0)
```

and concluded a table element, `ref.func` and `call_indirect` per site. The count
is right. The conclusion never checked what the shim does with the pointer *on
this target*, and `toolchain::RUNTIME_SHIM` answers in four lines:

```c
#if defined(__wasi__)
typedef struct VTask { void* frame; } VTask;
void* __vyrn_spawn(void (*thunk)(void*), void* frame) {
    VTask* t = (VTask*)__vyrn_malloc(sizeof(VTask));
    t->frame = frame;
    thunk(frame); /* eager: single-threaded target */
    return t;
}
```

wasm has no threads, so the pointer is formed and consumed in one statement. It
exists only because the LLVM path routes an eager call through a C function that
cannot know the callee — and a spawn site does know it, syntactically, which is
the very reason the pre-flight could enumerate nine of them.

So the lowering is that eager path emitted directly: `spawn f(a)` is `f(a)`, at
the spawn point, in argument order. Which is also literally what the interpreter
does (`interp.rs`, `Expr::Spawn`: evaluate the arguments, then `self.call`), so
there is one schedule and all three engines run it. Measured on the finished
module — its sections are type, import, func, memory, global, export, code, data:
**no table section and no element section.**

The sixth assumption this corpus has falsified, and like the other five it is in
the direction of less work. The encoder needed nothing: `wasm.rs` is untouched.

### What `spawn` has to do here, versus natively

Natively `spawn` is a thread (Win32 or pthreads), a task registry, a per-task
completion event, and `__vyrn_join_all` at exit so a leaked task's work — and its
trap — still happens. Not one of those crosses to wasm:

- **No thread.** The callee runs at the spawn point.
- **No wait.** `join` cannot block on something that already ran; it reads.
- **No registry and no `join_all`.** Eager means every task has run by the time
  `main` returns, which is exactly why the shim's wasm arm defines
  `__vyrn_join_all` empty.
- **No thunk.** It existed to give C a callee it could not name.

What survives is the **frame**, and it is load-bearing: a `Task<T>` outlives the
shadow-stack frame that made it, and `join` is idempotent. So the result is boxed
on the bump heap and the `Task` is that address — the shim's `VTask { frame }`
minus the thunk field it no longer needs, under the same ownership rule (never
freed; the count is bounded by the number of spawns).

Isolation is **not** enforced here, and must not be. The checker proves it
transitively for every engine (`checker.rs`: `spawn_safe`, the RFC-0037 rejection
of `fn`-typed parameters, and the post-check re-verification against the extended
fixpoint), so a second opinion in one backend would be a rule free to disagree
with itself. The RFC-0013/RFC-0025 `spawn`-`drop` hole that was fixed once was
fixed there, and this milestone adds nothing beside it.

### The bug only running could catch — and the first version of the test missed it

`Fn_::spawn` routes through `Fn_::call`, so argument coercion, the aggregate
return convention and generic instantiation are the ones an ordinary call gets and
cannot diverge from. The one thing it adds is the box, and the box is exactly
where the plausible shortcut is wrong: a frame slot compiles, validates, and
passes both examples.

`Frame::alloc` hands out an offset once per function and never reuses it — a slot
inside a loop is one slot — so four tasks spawned in a loop would all be the same
address. The first version of the escaping test did not catch that. It returned a
`Task` out of a callee and recursed 20 deep before joining, and it **passed** with
a frame slot: `fib` keeps its parameters in wasm locals, so its frame is zero
bytes and it never writes to the stack at all, and `print`'s 32-byte digit buffer
landed one slot below the box. A test that cannot fail is worse than no test, so
it was rewritten to hold four live tasks — the stack-slot build prints `233` four
times where the interpreter prints `55 89 144 233`. Both versions were built and
run to find out which one discriminates.

`a_task_that_escapes_its_frame_says_what_the_interpreter_says` pins that plus the
two other shapes no example makes: a `Task` of an **aggregate**, where `join`
copies rather than handing out the box's own address (the `load {ll}` the LLVM
backend emits, and M2l's `get` hazard one container along), and a `Task<Unit>`,
which has no result to read and still has to be a value `join` can consume.

### One guard, because `spawn f` and `f` must resolve the same way

`Fn_::call` matches builtin spellings before user functions, and the textual
backend's `prep_spawn_target` looks only at `funcs`. A user function sharing a
builtin's name would therefore have spawned the builtin here and the function
there. So the route is guarded by requiring the name to be a function this backend
knows — which is the checker's own rule, since `Expr::Spawn` resolves in `sigs`
and admits nothing else.

Worth recording while checking that: `prep_spawn_target`'s generic branch in the
textual backend is **unreachable**. The checker rejects a generic spawn callee
before either backend sees it — `spawn twice(41)` is
`` `spawn twice` argument expects T, found Int64 `` — because it checks the
arguments against unsubstituted parameter types. Not touched; not this RFC's call
to make.

### M2i's split refusal is unaffected, and stays honest

`vyrn-genwasm`'s `NOT_SPLITTABLE = ["__vyrn_spawn"]` is about the *textual*
backend's clang output: a function pointer is an index into a table the two-module
split does not share, so a generator that spawns gets the single fat module. That
reasoning is unchanged and still correct for that path.

It is simply not reachable from here. `SHIM_IMPORTS` is `["__vyrn_malloc"]`, so
the direct backend never imports `__vyrn_spawn` in either link shape — the
shim-linked module still defines it, and still nothing calls it. Both examples
were run under `--preload env=….shim.wasm` as well as standalone, so
`PASSING_SHIM` stays empty by construction rather than by omission.

### Nothing contradicted M0, M1, M2a–M2l

The box's size is the result type's layout through `layout::of_ll ∘ llt`, which
for a scalar is a size that is also a stride (M2c). Aggregates still travel as the
address of a slot, which is why an aggregate-returning spawned call needed no
convention of its own — `call` allocated the destination and this copies out of
it. No body emits a `return`: `spawn` is an expression and adds no exit. `Place`
gained nothing, `wasm::Frame` gained nothing, and the runtime table gained no
entry, because `malloc` was the only helper needed and it was already index 1.

One thing the eager path makes observable, and it is RFC-0025's rather than this
RFC's: a **trapping** spawned task traps at the spawn point here and in the
interpreter (`1`, then `error: division by zero`, exit 1 — checked), where a
native thread may print past it first. No example spawns a task that traps, so
nothing in parity says which it should be. Recorded rather than legislated.

### 65 of 87, regrouped

`region { .. }` (RFC-0004 §4). **64 of 87 standalone**, from 63, and the number is
the smaller half of the milestone: what a region *is* in this backend turned out to
be a different thing from what the brief expected, and the reason is a soundness
argument rather than a shortcut.

### A region is a counter here, and the arena is somebody else's ceiling

RFC-0004 §4's arena frees a group of allocations at once when the block exits. The
brief asked whether a bump pointer with a save/restore mark is the right shape.
**It is not, and neither is the obvious repair.** Both were checked against what
else allocates:

- **Marking the shared heap** and restoring at exit reclaims everything allocated
  since the mark — including the `Array` buffer a `push` inside a region grew for a
  binding *outside* it. `region.vyrn`'s own comment says growable arrays "compose
  with the arena freely" precisely because the textual backend's `push` does not go
  through `heap_alloc`; a mark does not know that. Same for a cell payload and both
  of a `Map`'s buffers.
- **A separate arena, routed on the RUNTIME depth**, is sound for everything the
  region *lexically* contains and wrong one call out. The textual backend routes
  lexically: `heap_alloc` reads the emitter's `region_depth`, so a function *called
  from inside* a region concatenates out of `malloc`. Route on a runtime counter
  instead and that callee's String comes from the arena — and the escape guard
  never examined it, because the guard checks stores into *named bindings in the
  region's own frames* (`region_store_guard`), which a callee storing into module
  state is not.

A lexically-routed separate arena is the sound version, and it is ~80 lines of
emitted runtime plus a flag threaded through six string helpers. It is not here,
and the reason is the standard this RFC has applied since M2f: **an untested
lowering is worse than a named gap**, and there is nothing to test. This backend's
`malloc` never frees for `push`, for a cell payload or for `Stmt::Drop` (M2b's
`ponytail:` note, M2l's "the payload is not freed on release"), so a region that
does not reclaim is that decision's cost rather than a new one, and a free is still
not a thing a program can print. The ceiling is marked in `Fn_::region_exit` with
both counterexamples, so the next person to reach for a mark has the argument.

### The counter is the finite resource, and it is the M2l shape exactly

The region stack is 64 deep — the LLVM prelude's number, and the interpreter's own
`region_depth >= 64` with the same wording, so all three engines refuse the same
nesting. That makes an unbalanced counter loud in both directions, and loud in the
way M2l described: a missed pop prints nothing different for 64 turns and then
traps, an extra pop reads as an enormous *unsigned* depth on the very next
`region`.

No example runs anywhere near either edge. `region.vyrn` has two regions, neither
nested, neither left by a branch; `controlflow.vyrn` has one `continue` under a
region and six turns. So the balance has its own running test —
`every_exit_out_of_a_region_balances_and_the_65th_traps` — and it was measured by
sabotage rather than asserted: with the unwind removed from `break`, from
`continue`, and from `return` in turn, a 200-turn loop prints nothing and traps
where the interpreter prints four numbers. An extra pop is caught by `region.vyrn`
itself.

Both halves pin the interpreter's answers rather than a spelling written in the
test, for M2g's reason: two backends can be confidently wrong together about which
depth is the bound.

### Four edges, and `return` is where the engines disagree

A region is one more frame the exit edges close, which is the shape M2l gave the
inferred release — so `Stmt::Break` and `Stmt::Continue` unwind to the depth the
loop recorded, and `Stmt::Return` unwinds to zero, in each case *after* the
releases and without popping the frames. The fall-through exit then lands in code
wasm has already marked unreachable, which is the same argument `Fn_::block` makes
about its own releases and the reason no `if !terminated` guard was needed.

`return` is not free, though, and finding out why is the part worth recording.
**The textual backend leaks a region-stack slot on a `return` out of a region**:
`Stmt::Region` emits `@__vyrn_region_exit` only on the fall-through path, and
`Stmt::Return` does not emit one at all. Measured — 70 calls to a function whose
region contains a `return` print `2485` under the interpreter and
`error: region nesting exceeds 64`, exit 1, natively. No example returns out of a
region, so parity has never seen it.

It is not a two-line fix, which is why it is reported rather than patched:
`__vyrn_region_exit` frees the arena as well as popping, and a `return a + b` out
of a region hands back a pointer *into* that arena. Checked directly — the escape
guard covers stores into bindings, not returns, so the program compiles and prints
`Hello, world!` today. Fixing it needs either a pop that does not free or a guard
on `return`, and both are RFC-0004 decisions.

This backend frees nothing, so it cannot dangle, so it simply matches the
interpreter — which is the rule the ladder is written to, and the same call M2h
made about `NaN != NaN` diverging from native for free.

**Both divergences this section reports are closed now**, and the resolution was
the first of the two options: a pop that does not free. The textual backend pops
the region counter on `return`, `break`, `continue` and `?` without freeing the
arena, which is exactly what the other two engines do, so the escape guard did not
have to grow. `NaN != NaN` was native being wrong rather than a divergence to
tolerate — `fcmp one` never answers true for a NaN operand, and it is `une` now.
The witness is
`a_return_out_of_a_region_balances_the_region_stack_on_every_engine`, whose
`viaTry` arm caught one more instance of the same defect *in this backend*: `?`
lowers its own early exit here and was skipping both unwinds, so the 65th `?` out
of a region aborted where the interpreter kept going.

### Both link shapes, trivially

The counter touches no allocator, so M2i's split is irrelevant: `region.vyrn`
prints `5`, `13` and exits 13 under `direct` and under `direct-shim`, and the
nesting probe traps identically in both. First milestone since M2i where "which
shapes does it work in" has a one-line answer, and it is because the thing that
would have made it interesting is the thing that was refused.

### `Stmt` is exhaustive now

`region` was the last unlowered statement kind, so the catch-all gap reporter and
the `stmt_name`/`stmt_line` pair feeding it were dead code claiming coverage.
Deleted: a statement kind added to the AST is now a compile error naming the `stmt`
match, the same trade `Rt::slots`'s all-fields-named struct literal makes.
Expressions keep theirs — `expr_name` still has work.

### Nothing contradicted M0, M1, M2a–M2l

No offset was computed at all, which is a first: a region has no layout. No new
runtime function, so `Rt`'s numbering did not move — the counter and the trap
message are two more of the derived address fields beside `msg_div0`.
Destination-first still holds where there is a destination, `region_exit` is
stack-neutral so it composes with a return value already on the operand stack
(M2d's note, M2f's copy-out), no body emits a `return`, and the memory map took
four reserved bytes without negotiation.

Cost, honestly: **+40 bytes in every module** — `fib.vyrn` 5,628 → 5,668 — for a
trap message and a counter most programs never reach. `runtime` runs before any
body is walked and so cannot know, which is M2j's twelve-unconditional-imports
argument and M2l's lazily-allocated slab argument arriving a third time. The fix is
still a reachability sweep over the finished call graph.

### 64 of 87, regrouped

| blocked on | n |
|---|---|
| `Match` on strings | 4 |
| the call `parse` | 4 |
| the call `lineAt` 3, the call `logger` 2 | 5 |
| `spawn` 2, `region` 2 | 4 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

Five rows, no long tail, and the function-value row is gone. What remains is the
three builtins RFC-0078 checked and deliberately refused to route (9 examples), the
two control-flow features this RFC scoped out from the start (4), RFC-0046's DFA
(4), and one generic-payload conversion twice.

---

## M2m, as landed — `=~` is a DFA

**`=~`, and the row that was not a dispatch problem.** 63/87 → 67/87, on both
link shapes. Four examples: `finitekeys`, `i18ndemo`, `regex`, `twdemo`. 128 lines
of `direct.rs`, nothing refused, nothing deleted.

### The row was read as a `match`, and it is an operator

"`Match` on strings" is the gap message `` `Match` on strings ``, and it came from
`cmp_i32(BinOp::Match)` returning `None` at the tail of the string-operator arm —
`BinOp::Match` is RFC-0046's `=~`. So the question of whether a `match` on strings
with many arms wants something denser than an O(n) chain has no site to be asked
at: `Pattern` has no string case, nothing in the corpus matches on a string value,
and the dense form is what `=~` already *is*. RFC-0046 answered that question with
a DFA, and this milestone's job was to run one.

What was left to choose is small and worth stating anyway:

- **One runner over a per-pattern table, not a walk specialized per pattern.** The
  pattern is entirely in the table it is handed, so every `=~` site in a module
  shares 33 instructions. That is the same split `@__vyrn_regex_run` /
  `@.rx.N.table` makes in the textual backend, and matching it means a divergence
  between the two can only be in the walk, never in the language.
- **The table is interned at the USE site**, which the textual backend cannot do.
  An LLVM global has a name that must exist before the reference to it, so `emit`
  walks every function, type predicate and global to collect the patterns first
  (`collect_regex_expr`). A data address has no name, and `Module::data` already
  shares identical contents — so the two sites of `value =~ "[a-z]+"` in
  `regex.vyrn` get one table because their bytes are equal, not because a pass
  went looking for them. There is no collection pass here at all, and generated
  code (RFC-0021 generators, RFC-0078's rewrites) is reached for free rather than
  by teaching a walker about it.

That second choice paid a number nobody was aiming at. `twdemo`'s generated module
declares two finite-string types, `TwClass` (391 states) and `Tw` (781); every
`TwClass` boundary in the file is proven at compile time by RFC-0020's containment,
so **no check for it is emitted and its 400,384-byte table is not in the module at
all.** The textual backend emits both. Measured on the data section: 831,524 bytes,
of which 799,744 is `Tw`.

### The footprint is RFC-0046's, and it is named rather than discovered

A complete 256-wide table of `u32` is 1 KB per state, so `Tw` is the largest static
this backend emits anywhere — an 880 KB module whose code section is 69 KB. Not a
regression, since the textual path emits the same shape (twice over, here), and
comfortably under `STATICS_LIMIT`'s 8 MB, which is about 8,000 states. The upgrade
path if it ever matters is byte equivalence classes, and it is written down beside
the interning rather than left to be rediscovered from a module size.

### The bug only running could catch, and this time it had to be planted

Ninth consecutive milestone, by a different route. There was no accidental bug —
all four examples built and agreed on the first run, which has not happened before
— so the walk was broken on purpose instead, and that found something better than
a bug: **the whole corpus is blind to a non-ASCII input byte.**

The load of the input byte has to be unsigned. With `i32.load8_s` a UTF-8
continuation byte becomes a negative table index, the walk reads memory *below* the
transition table, and the answer is wrong with no trap — the table sits in the
middle of a live address space, so there is nothing to fault on. All four examples,
rebuilt against that, still pass: every `=~` in `examples/` and `std/` runs over
ASCII keys and ASCII class names.

So `the_dfa_walk_agrees_with_the_interpreter_on_what_no_example_reaches` pins nine
answers against the interpreter's. The two that matter are `"é" =~ "."` false and
`"é" =~ ".."` true — `.` is one BYTE, which is the fact RFC-0046's byte DFA rests
on and the fact a signed load destroys. It also covers the zero-length walk, where
the answer is whether the START state accepts (a do-while shape gets that wrong and
no example asks), and a non-match that keeps walking after it is already lost,
which is what a dead state absorbing the rest of the input means.

### Nothing contradicted M0, M1, M2a–M2l

No offset is computed here at all: a DFA row is `state << 10` into a table this
milestone interned, not a field of a Vyrn type, so `layout::of_ll ∘ llt` has
nothing to say and is not consulted. `=~` yields a scalar `Bool`, so
destination-first has no destination; the runner reaches its epilogue without a
`return`, as `map_find` does; and the widening rule is untouched because the table
is data rather than a boundary. M2l's self-registering `Rt` did exactly what it was
built for — the new helper is one `slot("regex_run")` beside its `next_is`, and the
count test needed no edit. The LLVM wasm path is byte-for-byte unaffected: the diff
does not touch `lib.rs`.

### 67 of 87, regrouped

| blocked on | n |
|---|---|
| the call `parse` | 4 |
| the call `lineAt` 3, the call `logger` 2 | 5 |
| a `fn`-typed parameter (RFC-0023) 3, a lambda 2 | 5 |
| `spawn` 2, `region` 2 | 4 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

M2l's list minus one row, and nothing else moved: no example blocked on `=~` had a
second blocker behind it, which is the first time a row has paid its full face
value. What remains is the nine-example builtins row (`parse`, `lineAt`, `logger`),
the RFC-0023/RFC-0037 function-value worklist, the two control-flow features this
RFC scoped out from the start, and one generic-payload conversion twice.

| a `fn`-typed parameter (RFC-0023) 3, a lambda 2 | 5 |
| `region` | 2 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

`controlflow.vyrn` spawns *and* opens a `region`, so its blocker moved from
`spawn` to `region`: that row is `controlflow` and `region.vyrn`, the two examples
it always was, now reported by the feature that actually stops them.

| a `fn`-typed parameter (RFC-0023) 3, a lambda 2 | 5 |
| `spawn` | 3 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

`controlflow.vyrn` moved from `region` to `spawn`, which is the third example in
that row and the reason the region row is gone while the spawn row grew. Nothing
else moved: a region was nobody else's first blocker.

---

## M2n, as landed — `parse` and line/column — `parse`, `lineAt` and `colAt`

The two builtins RFC-0078 refused to route. **76 of 87 → 83 of 87**, seven
examples: `argsdemo`, `input`, `numbytes`, `stringops` for `parse`; `pagesdemo`,
`textbytes`, `vyxdemo` for the line/column pair. Nothing in the row is refused.

### The row was two refusals of two different kinds, and each is the lowering's spec

M2l read this as "three builtins with no lowering, and none of it is a routing
question." That is right, but the two halves are refused for reasons that are not
the same reason, and each reason is what its lowering had to be measured against.

**`parse` is a SEMANTICS refusal.** `std/num`'s `parseInt64` is the same digit
loop and *declines* where `parse` wraps, so RFC-0078 M4a refused to fold them: the
overflow behaviour of every existing caller is the language. What the interpreter
actually does — `parse_int` in `interp.rs`, twelve lines — is an optional `-`
(never a `+`), then bytes that must ALL be ASCII digits, accumulated with
`wrapping_mul(10)` and `wrapping_add`, negated with `wrapping_neg`. So there are
exactly three declines and overflow is not one of them:

- nothing after the sign, which is `""` and `"-"`;
- any byte in the rest that is not a digit, which is `"+1"`, `" 1"`, `"1 "`,
  `"1.5"`, `"1e3"`, `"abc"`, `"12a"` and `"--1"`;
- and that is all.

`parse("9223372036854775808")` is `Int64.min`, `parse("18446744073709551615")` is
`-1`, `parse("99999999999999999999999999")` is `-2537764290115403777`. Every one of
those rows is already a literal in `examples/numbytes.vyrn`, which RFC-0078 M4a
wrote as a pin BEFORE moving anything — so this backend has an oracle rather than a
record of what came out, and `parse` needed no test of its own.

Deliberately not `str_i64` next door, which was the tempting reuse. That one reads
`+` and stops at the first byte that is not a digit, because it is `strtoll` for an
injected `VYRN_FIXED_TIME`; those are the opposite answers on exactly the inputs
`numbytes` prints. M2j named it "not full `strtoll`" and the ceiling is still
accurate — it just points away from here.

**`lineAt`/`colAt` are a CACHE refusal**, and that is a different obligation: the
loop is four lines and the reason it is a builtin is that it is O(offset) while a
scanner asks once per node.

### No cache, and the number that says so

The interpreter memoizes a line-start table keyed on the array's `Rc` pointer. The
native shim does not memoize at all — `__vyrn_line_at` counts LFs from byte 0 and
`__vyrn_col_at` walks bytes backwards, on every call. This backend takes the
shim's route, so the three engines are two implementations rather than three.

Measured rather than argued, because RFC-0078 M4b(2)'s 122 ms of a 291 ms page
compile is the number that makes this a real question. The page-compile shape, in
Vyrn: a 23 KB buffer (eight times a big `.vyx`), a newline every 40 bytes, one
`lineAt` and one `colAt` per node, 585 asks — the quadratic sweep a cache removes.

| | one sweep |
|---|---|
| interpreter, memoized (binary search per ask) | ~0.4 ms |
| direct wasm, uncached | **~3 ms** |

Two facts settle it. The 122 ms was **generation**-time and is paid by whichever
engine runs the generator; `vyrn build --target wasm` runs generators under the
interpreter (`wasm-gen` is off by default in the CLI), so it is the memoized
number. And a module emitted by this backend reaches these functions at run time
only where compiled code calls them — `vyxdemo` and `pagesdemo` contain
`std/vyx`'s scanner because codegen emits every function of a linked module whether
it is called or not, and never call it. Both run at wasmtime's ~25 ms startup
floor. The one example that does call the pair at run time is `textbytes`, over
twelve short buffers.

So the cache is worth ~2.6 ms on a buffer eight times the size of the file that
motivated it, against per-buffer state in linear memory keyed on an address a bump
allocator can recycle — which is the hazard the interpreter avoids by holding the
`Rc` and this backend has no way to. Not a small change, and nothing measured wants
it. M5's question stays M5's.

### A column counts BYTES, and this backend now says so at both ends

Confirmed against the interpreter (`off - lineStart + 1` over a byte table) and the
shim (a backward byte walk): the `x` in `éx` is column 3. `std/vyx.vyrn:165`'s
"chars since the last LF" is still wrong and still harmless, for RFC-0033's
C-`#line` reason.

`textbytes` sweeps the interesting middle — a CRLF, an empty line, past the end,
and the `éx` row. The two cases it never reaches are the two whose lowering is a
compare's SIGNEDNESS, and they have a test:
`line_and_column_agree_with_the_interpreter_off_both_ends_of_the_buffer`.

- **A negative offset.** The interpreter clamps with `.max(0)`; the shim does not
  clamp below zero at all and agrees anyway because `i < off` and `i > 0` are
  signed. Flipping `i64.le_s` to `i64.le_u` in `col_at` is one character and turns
  `1:1` into `1:12` — a wrong answer with **no trap**, because the wrapped address
  stayed in bounds. That is M2m's `=~` shape exactly, and it is why the test exists
  rather than a comment claiming the path is covered.
- **An empty buffer**, where the `off > len` clamp is the only thing between
  `colAt` and a read below the allocation. Deleting it turns `1:1` into `1:6`.

Both mutations were planted and both fail the test. Every row is compared against
the interpreter AND spelled out, because two backends can be confidently wrong
together.

### Refused, specifically: a buffer whose elements are not bytes

The checker accepts an `Array`, an `ArrayN` or a `SmallArray` of anything as
`lineAt`'s first argument. `walk` hands all three over as one base-and-count, so
the lowering was indifferent — and that is precisely where it must not be, because
a wider element means the three engines read three different things. This backend
refuses a stride other than 1, naming the element type.

Refusing found a **latent divergence in the other two engines**, which is the
fourth milestone running where looking at an untaken edge found a defect elsewhere:

```
let mut a: Array<Int64> = []
a.push(1)
a.push(10)
print(lineAt(a, 2))     // interpreter: 2      native: 1
```

The interpreter takes `v as u8` of each *element* (so element 1 is an LF); native
reads `unsigned char*` off the data pointer, where the same array is
`01 00 00 00 00 00 00 00  0A ...` and byte 1 is zero. Not fixed here — deciding
which answer is right is a language decision, and the right shape is probably the
checker requiring `Array<UInt8>` rather than any array. Recorded because no example
reaches it and nothing else would have looked.

### Nothing contradicted M0, M1, M2a–M2m

Every offset is still `layout::of_ll ∘ llt`: `parse_i64` writes the
`{ i1, i64, i64 }` sum through `sum2`'s own field list, and the one place it spells
its stores out rather than calling `sum2_write_to` is because the payload is
already an `i64` rather than a word to zero-extend — the offsets still come from
the same `Layout`. The `Option<Int64>` travels as the address of a shadow-stack
slot the call site allocates, which is the hidden destination `readLine` gets and
needed no new rule. All three helpers are one `block` with `br`s to it, so no body
emits a `return`. Both branch positions were checked by running: `if c { parse(a) }
else { parse(b) }` types through `peek`'s existing `parse` row, and the pair got
one of its own — M2l's rule is that a builtin `call` types as it emits owes `peek`
a row, and these are `call`'s newest two. `Fn_::coerce` took nothing new, for the
fourth milestone running.

Three indices came out of `Rt::slots` rather than being hand-numbered, and the
`next_is` assertion at each of the three emit sites is the mechanism working as
advertised: the additions are three appended lines in the declaration and one
appended top-level `text_runtime`, with no existing index touched.

### 83 of 87, regrouped

| blocked on | n |
|---|---|
| the call `logger` | 2 |
| a conversion from `Cargo`/`Config` to `T` | 2 |

Four examples, two features, no long tail — and both are in flight beside this
milestone. RFC-0008's leveled sink is the last builtin with no lowering; the
generic-payload conversion is one row of the M2d seam's list.

One bookkeeping note that is not this milestone's: **`controlflow.vyrn` passes and
is not in `PASSING`**, and has been since `spawn` landed. The ladder prints
`NEW controlflow.vyrn` on every run and nothing has claimed it.

## M2o, as landed — `logger` and a generic payload — the logger, and a payload that forgot its type

RFC-0008's leveled logger, and the generic-payload conversion. **76/87 → 78/87**
on both link shapes: `logging` and `genericpayload`. Two unrelated features, and
the honest half of the milestone is that the row each one sat in was worth less
than it said — `logger` was 2 examples and paid 1, the payload conversion was 2
and paid 1, and in both cases the second example has a blocker behind it that the
ladder could not see.

(Numbered M2n because three parallel milestones already landed as "M2m"; the
merge could not have known, and renaming theirs afterwards would break the
references in their own commits.)

### The census refused to route `logger`, and every reason it gave is why this was small

RFC-0078's M5 census kept `logger` in the compiler because it is "a syscall three
times over". Read from this side that is not a cost, it is the whole lowering:
`write_all(fd, ptr, len)` has been the ONE place bytes leave this module since
M2a, and a log line is five calls to it — three interned constants of known
length, and two `ptr`s, because a `String` IS a NUL-terminated pointer. There is
no format string to parse, no varargs to lack, and nothing to assemble. It is the
textual backend's `fprintf(stream, "[%s] %s: %s\n", lvl, name, msg)` in the only
shape a wasm module can have one.

The alternative was considered and is worse: `concat` three times into one string
and write it once costs three `malloc`s out of an allocator that never frees, to
save four calls that are the same syscall either way. Nothing can interleave with
a half-written line — RFC-0008 bars logging from a spawned task — so there is no
atomicity to buy.

The two `String`s are parked in scratch locals rather than left on the stack,
because each `write_all` takes three operands and the second value cannot wait
underneath the first one's call. Two locals, and `logger(name)` itself is the
identity on a `ptr`: a handle has no content but its name, so it emits nothing.

### The fold is the milestone, and the ladder cannot see it

With `logging { level: warn }` a `.debug(..)` call emits **no write**. That is
RFC-0008's Q3, and the thing RFC-0078's census names as the reason routing would
be a language change — a deleted call becoming a runtime comparison is
`byteLength`'s consteval argument in different clothing.

A passing ladder says nothing about it. A backend that emitted the comparison
prints exactly the same lines and passes every example. So the evidence is the
module's bytes: `[LEVEL] ` is interned at the *emitting* site and nowhere else —
M2m's DFA rule, and M2d's argument that a trap message only `emit_validation`
interns is proof a check exists — so a prefix in the data section means a write
exists and its absence means one does not. `logging.vyrn` contains `[DEBUG] `,
`[INFO] ` and `[WARN] ` exactly once each and does not contain `[TRACE] ` or
`[ERROR] ` at all.

The test pins the other half in the same breath, and that half is what makes it a
test rather than a tautology: the suppressed calls' own MESSAGES are asserted
**present**. Arguments are still evaluated under suppression (Q4, so a message's
side effects do not depend on a level), and without that assertion the test would
also pass on a backend that deleted the statement whole.

### The `file(..)` sink is a held descriptor, and M2j is why it could be

A file sink is the one shape `writeFile` cannot express: `writeFile` opens,
truncates and closes per call, and a log file is opened once and written many
times. Natively that is a `FILE*` in `@__vyrn_log_file`, `fopen`ed in `@main` and
`fclose`d after `vyrn_main`.

M2j put `path_open` in this module directly, so the same shape is expressible
here and is what landed: `_start` opens the path with `CREAT|TRUNC` —
`fopen(path, "w")`, truncating — BEFORE the globals initializer, because a
top-level `let` may log, which is the order `vyrn_entry` already used; the
descriptor goes in four reserved bytes; `fd_close` runs after `main`. The
reservation is made **only for a file sink**, so every console-sink module in the
corpus is byte-for-byte what it was — this milestone changes no module that does
not log to a file.

What it does on a failure is decided rather than inherited. `open_at` gives -1,
that is what the slot holds, and `write_all`'s errno test swallows every write. So
a bad path is silence, which is the interpreter's `if let Some(f) = ..` and
RFC-0008's Q6 leaning — and, the part worth recording, it is also the browser:
`wasi-min.js` has no preopens, so `open_at` returns -1 for every path and a page's
file sink degrades to silence instead of trapping. RFC-0014's graceful-degradation
posture, reached for free.

Verified by running: the log file is byte-identical to the interpreter's
`std::fs::File`, and TWICE, because a `path_open` without `TRUNC` appends and one
run cannot tell the two apart.

### Neither sink nor threshold that matters has an example

`logging.vyrn` and `vlog.vyrn` are the only logging examples, both write to
`stderr`, and only the first builds here — so `stdout`, `file(..)` and every
threshold but `debug` have no example at all. All three are running tests against
the interpreter.

The `stdout` case is the one worth naming. A log line and `print` go to the SAME
descriptor, so their INTERLEAVING is observable — and a sink that quietly stayed
on 2 would still look right to a test that read only stdout. The assertion is the
full stream, in order, `print` and log lines together.

### `peek` named the enum where the emitting path named the instantiation

The payload gap was one line and it was in `peek`. A variant construction in a
branch reported `Type::Named("Crate")`; `resolve` of a bare generic name gives the
declaration's own `Held(T)`, so the payload's coercion was handed a `Type::Param`
and refused "a conversion from `Cargo` to `T`". The emitting path — `sum_ctor` —
had used the shared `applied_type` since M2e and was right all along; the two
disagreed, and `peek`'s answer is the one a `match` on the result binds from.

That is M2e's bug from the other end. There, a call reported its return type
**resolved**, so `Pair<Int64, Int64>` no longer matched `Pair<A, B>`; here a
construction reported it **unapplied**, so nothing had a `Cargo` to solve `T`
from. Both are a type that stopped carrying what the next site needs, and both
surfaced as a refusal only because a `Param` has no layout.

`applied_variant` is that rule in one place, and `peek` had no BARE-NAME form to
call it from either: `expr` distinguishes a nullary constructor from a local by
failing to be one, and `peek` did not, so a `match` whose first arm is `Empty`
could not be typed at all. Both forms route through the one function now.

### The arm scan is order-independence, not this example

`match_ty` is the first-arm peek — previously two copies, in `peek` and in
`match_expr` — plus one upgrade: a non-applied answer is replaced by the first arm
that has an applied one. `ty_is_concrete_app` moved out of `Gen` and both backends
call it, for `solve_type_args`'s reason: an enum's layout is arity-wide
(`enum_ll`), so two backends preferring different arms would not fail to link,
they would encode a payload one way and read it the other.

Measured rather than assumed, because the temptation was to claim the scan is what
unblocked the example. It is not. With the scan removed `genericpayload.vyrn`
still builds and emits a byte-identical module — it puts its concrete arm FIRST,
so first-arm-wins is right about it. What the scan buys is that the answer stops
depending on arm ORDER, and the flipped order refuses with "a conversion from
`Cargo` to `Unit`" without it.

The flipped order has no example because the **checker** refuses it: `Empty` first
is uninferable without an annotation. So it is a test, and it pins VALUES rather
than agreement, for a reason the two payload shapes make concrete. A `Cargo`
payload is `Word::Boxed`, so a forgotten `T` has a conversion to refuse and is
loud. An `Int64` payload is `Word::Direct` — the word is an `i64` either way — so
the same mistake has nothing to refuse and would read a pointer as a number.

### Two rows were mis-attributed, and the ladder cannot help it

The emitter stops at the first gap, so an example is reported by whichever blocker
its first-emitted function happens to hit. Both of this milestone's rows had a
second one behind them:

- **`storage.vyrn` was never blocked on the payload conversion.** It is blocked on
  `renameFile`, and has been since M2j: `std/storage`'s `writeAtomic` is
  `writeFile` then `renameFile`, and only the first has a lowering. Confirmed
  against the base commit with a program that calls nothing but `writeAtomic`. The
  `Config`-to-`T` message came from a generated decoder emitted earlier in the
  module.
- **`vlog.vyrn` was blocked on `logger` and is now blocked on `parse`**, which is
  the row beside it in the same table.

So "the call `logger` 2" and "a conversion from `Cargo`/`Config` to `T` 2" were
each one example. Nothing about either fix is smaller than it looked; the rows
were.

### Refused, specifically

**`renameFile`.** It is what `storage.vyrn` needs and it is not this milestone,
for a reason of shape rather than size: it is a new syscall at the BOUNDARY.
`path_rename` would join the twelve unconditional WASI imports, which renumbers
the whole function index space and changes the bytes of every module in the corpus
— including `fib.vyrn`'s pinned size. It also needs the preopen walk `open_at`
does, and a second error class the corpus never reaches (`EXDEV`, "cannot rename
`%s` across devices", distinct from "cannot write `%s`"). It belongs with
`fsyncFile` and `listDir` as one RFC-0014 milestone rather than smuggled into a
logging one.

**A `Logger` anywhere but a local.** `llt_of` prints `ptr` for one so it would
lower, but nothing in the corpus stores a logger in a record, an array or a
module-state global, and RFC-0008's per-logger overrides — the thing that would
give a handle content — are explicitly not implemented. Not claimed.

### Nothing contradicted M0, M1, M2a–M2m

The logger computes no offset at all: five `write_all`s over interned addresses
and two `ptr` locals, so `layout::of_ll ∘ llt` has nothing to say and is not
consulted. The payload half consults it only through `enum_ll`, whose shape is
arity-wide and therefore identical across instantiations — which is exactly why
the wrong instantiation was silent about layout and loud only at a coercion.
Destination-first is untouched (a log call yields `Unit`; `match_ty` is the same
answer `peek` already gave, from one function instead of two), no body emits a
`return`, and `Type::Param` stayed unreachable — the payload fix is the one that
stops one arriving. `Rt` gained nothing: a log write reuses `write_all` and
`strlen`, so there is no new index and the count test needed no edit.

The M2d seam took nothing new, which is the fourth milestone in a row. A `Logger`
flows only to a `Logger`, so `from == to` and the `ll`-equality shortcut is
correct rather than dangerous.

The LLVM wasm path: the logger diff does not touch `lib.rs` at all, and the
payload diff touches it only to make `ty_is_concrete_app` a free function with the
same body behind the same call. emit-ir is byte-identical over `logging`, `vlog`,
`storage`, `genericpayload`, `jsoncodec`, `enum` and `option`.

### 78 of 87, regrouped

| blocked on | n |
|---|---|
| the call `parse` | 5 |
| the call `lineAt` | 3 |
| the call `renameFile` | 1 |

Three rows, each one builtin, and the two big ones are the pair RFC-0078 checked
and deliberately refused to route (`parse` wraps where `parseInt64` declines;
`lineAt`/`colAt` are a memoized table a Vyrn library cannot hold). `renameFile` is
the third, refused above. There is no structural gap left in this corpus: every
remaining example is one call away.

---

## M2p, as landed — the reachability sweep, and `renameFile`

**87 of 87 standalone, and 87 of 87 linked.** Every example in the corpus compiles
through the direct backend, runs under wasmtime, and agrees with the interpreter
byte for byte, traps included. The ladder is finished.

Two commits, in that order, and the order is the point: `renameFile` was refused
at M2o for a reason the sweep dissolves.

### The sweep is in the encoder, and it changed nothing about lowering

M2j measured the cost of twelve unconditional WASI imports and about 2.3 KB of I/O
runtime in every module, and named the fix as "a reachability sweep over the
*finished* call graph, which is what a linker does and is not a second source of
truth about what a program needs". That sentence is the whole design. An **AST
pre-scan** was refused three times (M2j for imports, M2e for a standalone
instantiation walker, and again by the region milestone) because it would be a
second traversal obliged to agree with lowering about what lowering emits — the
drift hazard `llt_of`, `layout ∘ llt` and `predicate_binds` exist to prevent. A
sweep over the finished graph cannot drift: the graph **is** what was emitted.

So the sweep is not a pass over the program. `wasm::Module` now holds imports,
bodies and exports as plain data until `finish`, and `Module::sweep` walks the
`Instruction::Call`s that are actually in the bodies, from the exports, and drops
everything else. Nothing in `direct.rs` learned anything: the emitter still emits
forty runtime helpers and thirteen imports whether a program reaches one or not,
because it still cannot know until the bodies are walked. One line at the end of
`compile_linked` is the entire integration.

**Not two passes.** The task's framing offered emit-twice — cheap now that clang is
gone — and M2m's `reserve_func`/`fill` as the alternative. It is neither: the only
thing standing in the way of a single pass was that `Module::add` encoded each
`Frame` to `wasm_encoder::Function` bytes on arrival, and an encoded body cannot
have its call indices renumbered. Keeping the `Frame` until `finish` is a smaller
change than emitting anything twice, and it deleted the eager type-section
interning as a side effect — a signature only a pruned function used now costs
nothing either. Emission stays exactly as many traversals as it was: one.

**`Rt::slots` + `next_is` are untouched and still assert.** They are about the
declared order agreeing with the emission order, and both still happen before
anything is pruned. The sweep runs when nothing is being emitted any more, which
is why the two mechanisms never see each other — and why adding `rename_file` to
the middle of the runtime table needed no renumbering by hand.

### Roots are the exports, which made `export extern fn` one

Enumerated deliberately rather than assumed, because a missing root is a function
gone from a module that needed it:

- **`_start`.** WASI's entry point, and the only export this backend had.
- **The globals initializer** (RFC-0013) is *not* a root and does not need to be:
  `_start` calls it. When a program has no module state it is emitted, unreached,
  and swept — two bytes back.
- **`main`** is likewise not a root in its own right; `_start` calls it.
- **`vyrn_entry`** does not exist here. It is the native/LLVM entry, and the
  direct backend has never emitted one.
- **RFC-0012 `export extern fn`** was the real gap, and it was not a sweep gap.
  The direct backend exported `_start` and nothing else, so a JS caller had
  nothing to call — `wasm-export-name` plus `--export-all` was doing this on the
  LLVM path and no one had noticed the direct shape did not. One `m.export` per
  `is_export_extern` fixes the feature and makes the root list and the callable
  surface one list. Nine of them in `domdemo.vyrn`, two in `externdemo2.vyrn`.

A function index appears in exactly two places — a `call` and an export — and
`reachable` panics on `call_indirect`, `ref.func`, `return_call` and
`return_call_indirect` rather than silently sweeping past one. There is no table
today (M2m measured that: a defunctionalized `fn` value is a tag and a direct
call), and if one ever arrives the sweep must be told before it prunes a target.

### The numbers, and the one that is honest about itself

Standalone shape, before and after the sweep:

| | before | after |
|---|---|---|
| `fib.wasm` | 6,048 | **1,406** |
| `mapdemo.wasm` | 49,948 | 31,814 |
| `vyxdemo.wasm` | 100,816 | 16,231 |
| `twdemo.wasm` | 902,171 | 836,639 |

`fib.wasm` is the yardstick because it uses none of the runtime: 4,420 bytes of
code became **290**, and the import section is 442 bytes to 70 in every module in
the corpus. M2j's 5,167 had grown to 6,048 by M2o (the region counter's trap
message, `parse`, `lineAt`/`colAt`, the logger), which is the shape of an
unconditional runtime — it grows with every milestone whether the milestone is
reachable or not. It does not any more.

`vyxdemo` is the extreme because a page compile links `std/vyx`, `std/html` and
`std/ui` and calls a small fraction of them: **84% of that module was unreachable
code**. `twdemo` is the floor for the same reason in reverse — 836 KB of it is one
DFA transition table, which is data.

**The data pool is NOT swept, and that is the remaining ceiling.** `fib.wasm`'s 941
bytes of data are 67% of what is left, and every byte is a trap or I/O message the
program cannot reach. It is left because pruning it means deciding which
`i32.const` in a body is a pool address and which is an integer that happens to
look like one — a heuristic, where the call graph is a fact. The honest cost shows
up in this milestone's second half: `renameFile`'s cross-device wording is interned
whether reached or not, so every module is 32 bytes bigger than the table above
(`fib.wasm` 1,406 to 1,438). Interning lazily at the use site would fix the class
(the pool already deduplicates, so repeats are free), and it is a milestone of its
own rather than smuggled into this one.

### `renameFile` cost nothing structural, which was the whole bet

M2o's refusal was three sentences and only one of them was about work:
`path_rename` "would join the twelve unconditional WASI imports, which renumbers
the whole function index space and changes the bytes of every module in the corpus
— including `fib.vyrn`'s pinned size". After the sweep there are thirteen
declarations and `fib.wasm` imports **two**. The renumbering objection is gone
because the numbering is no longer a property of the corpus; the pinned-size
objection is gone because the pin is now smaller than before the syscall was added.

What was left was mechanical. `rename_file` is `open_at`'s preopen walk without the
open — WASI has no rename relative to a working directory either — with both paths
going through the SAME directory fd, which is also why the cross-device arm is
nearly unreachable: a preopen is one mount. The two error classes come out of
`IO_MESSAGES` like every other message here (`@.io.xdeverr` for `errno::xdev`,
`@.io.writeerr` for anything else, both naming the TARGET), which is the same pair
the interpreter picks between on `is_cross_device`.

One number was worth reading rather than assuming: **preview1's `xdev` is 75**, and
POSIX's `EXDEV` of 18 is `errno::dom` in WASI's own alphabetical list. A backend
that carried the POSIX number over would have reported the wrong one of two
plausible messages, on the arm nothing can reach — i.e. silently, forever.

### `storage.vyrn` had a second blocker behind the first, again

M2o found that `storage.vyrn`'s reported blocker was a decoder's and its real one
was `renameFile`. There was a third: with the syscall lowered, it still failed with
"a branch yielding `renameFile`", because **`peek` had no row for any of
RFC-0014's I/O**. `writeAtomic` is

    return match writeFile(tmp, content) {
        Ok(done) => renameFile(tmp, path),
        Err(why) => Err(why),
    }

and an arm's value is typed by `peek`. Nothing in the corpus had ever put an I/O
call in a branch, so `writeFile` in an arm failed identically before this milestone
— confirmed by writing one. M2l's rule is that a builtin `call` lowers owes `peek`
a row; the fix is that both now read one `io_builtin_ty`, because two spellings of
`Result<Bool, String>` are two chances to size a destination slot differently from
the value written into it.

That is the third milestone running where the ladder's blocker list under-counted
by hiding one gap behind another. The emitter stops at the first gap, and there is
no fix for that beyond reading it as a lower bound.

### What ran, and what a passing run could not have caught

The renumbering is checked by running, because a prune that forgets to rewrite a
call still **validates** whenever the two signatures match — the silent case
`Rt::next_is` exists for, arranged deliberately: four helpers of one signature, two
of them unreachable and sitting between the two that are, so the surviving pair
cannot keep its old indices. Mutating the rewrite out turns it red; 90, 91, 93, 94
and 181 are each a specific mistake it would have printed instead of 7.

`renameFile`'s edges are a running test pinning the interpreter's own answers,
since `storage.vyrn` reaches the happy path only:

- An **existing target is overwritten**, which POSIX `rename` and `path_rename`
  both do and Windows C `rename` refuses — the semantic RFC-0044 exists for, and
  the one a backend could plausibly get wrong in the safe-looking direction.
- The **source is gone** afterwards, which is what says it moved rather than
  copied.
- Both reachable failures — a missing source, an unresolvable target — are
  `cannot write` **about the target**, byte for byte what the interpreter says.

Self-setting-up, because a rename is destructive and the interpreter and the wasm
module run in the same directory one after the other. Three engines agree on all
seven lines (interpreter, native, direct wasm).

### Nothing contradicted M0, M1, M2a-M2o

The sweep computes no offsets, allocates no slots and emits no instructions, so
`layout::of_ll ∘ llt` has nothing to say about it. `rename_file` is the same
`sum2_write_to` at the same offsets `writeFile` uses — the runtime writing a
`Result` through a destination the caller allocated, i.e. M2b's aggregate rule with
no case of its own. No body emits a `return`; the helper's early exits are `br` out
of a labelled block, and its `Ok`/`Err` split is one `if`/`else` with the message
selection an `if` **with a result type**, because a block that leaves a value on
the stack must say so.

The M2d seam took nothing new, which is the fifth milestone in a row. The M2i link
shape needed no attention at all: its import list is `__vyrn_malloc` and it goes
through the same `import`/`sweep` path, so `direct-shim` is 87/87 too — and if a
program never allocates, the shim import is now pruned as well.

The LLVM wasm path: **neither commit touches `lib.rs`**. The one shared thing this
milestone reads is `IO_MESSAGES`, which RFC-0078 M4c had already made the single
source, so there was no wording to write down twice.

### 87 of 87

| blocked on | n |
|---|---|
| — | 0 |

The table is empty. M5 is now unblocked, and what it needs is a decision rather
than a lowering:

- **Delete the textual emitter's wasm path and `VYRN_WASM_BACKEND` with it**, so
  `vyrn build --target wasm` is this backend unconditionally. The acceptance
  criterion "needs no clang, no wasi sysroot, no builtins archive" is already true
  of the direct shape and cannot be true of the other one.
- **Fold the `directwasm` tier into `parity`**, since after the deletion the wasm
  column IS this backend. `PASSING` and `PASSING_SHIM` stop being lists and become
  the corpus; the tier that runs twice stops needing to.
- **Decide what `direct-shim` is for.** It has passed nothing `direct` does not
  since M2j, its import list is one function, and M2i already established that the
  split makes a module larger. It is a live audit of the boundary signatures
  (`tests/shim_link.rs`) and of shared-memory linking, which RFC-0076's generator
  artifacts still use — so the question is whether that audit belongs to RFC-0076
  rather than whether the shape survives.
- **M4 is now optional, and probably struck.** "The 1,080-line prelude moves into
  the C shim" was a plan for a backend that reached the shim. This one does not,
  emits its own forty helpers, and now ships only the ones a program calls: the
  prelude's remaining cost is 290 bytes in `fib.wasm`. Moving it into C would
  reintroduce the clang dependency M5's acceptance criterion forbids.
- **What M5 must not lose:** the browser story. `web/wasi-min.js` implements
  exactly the preview1 set this backend imports, and RFC-0012's exports are named
  by this backend only as of this milestone — `--export-all` was covering for that.
  A page that calls anything but `_start` and an `export extern fn` is the one
  thing to check before the LLVM path goes.

---

## M5, as landed — the deletion

`vyrn build --target wasm` is this backend, unconditionally. `VYRN_WASM_BACKEND`
is gone, `direct-shim` with it, the `directwasm` tier is folded into `parity`, and
M4 is struck. Every acceptance criterion is met.

The interesting part of the milestone is not the deletion. It is that M2p's
closing note — "the browser story ... is the one thing to check before the LLVM
path goes" — was right, and checking it found **two** gaps rather than none.

### The ladder was blind to RFC-0012 in both directions

87 of 87, and `extern fn` imports had **no lowering at all**. `externdemo.vyrn`
failed to build with "no lowering for the call `jsNow`" — `Cx::externs` held a
return type and nothing else, for RFC-0043's three host-boundary names, and an
ordinary extern call fell through to `Cx::sigs` and missed.

Nothing in the repo could see it, and the reason is structural rather than an
oversight. `externdemo.vyrn` is in `WASM_ONLY` because wasmtime supplies WASI and
not `vyrn`, so there is no run to compare — and the harness excluded it from the
*build* as well as from the comparison. So a lowering that did not exist cost
nothing on any backend for the length of M2. `common/mod.rs` now says that
alongside the exclusion, because the exclusion is what made it possible.

The other half is the one M2p half-found. It named `export extern fn` as a sweep
root and exported each one — and a `String` still could not cross **into** one,
because `__vyrn_malloc` was not exported either. On the LLVM path that is
`-Wl,--export=__vyrn_malloc` under exactly the condition it is emitted under here;
without it every handler in `web/domdemo.html` throws. `+1` clicked and the count
stayed 0. Not a missing feature — a demo where no button works.

### Both were found by loading the page, and only a page can find them

The pages were loaded before anything was deleted, which is the order the task
required and the right one:

- **`domdemo.html`** — the counter counts, the text input round-trips a typed
  `String` through `onType`, the keyed list reorders, and the `Every(1s)`
  subscription ticks, with nothing on the console. Nine `export extern fn`s driven
  by name through `vyrn-dom.js`'s delegated listeners.
- **`externdemo.html`** — `jsLog` receives a decoded `String`
  (`t=1721000000.500000`, six decimals from `f64_str`), `jsNow` returns a
  `Float64`, `jsAdd` round-trips two `Int64`s as BigInts. Then `externdemo2`
  answers `vyrnAdd(40, 2) = 42` and `greet("world")` through the allocator.
- **`eventloop.html`** — module state survives between host calls: the timer drives
  `onTick()` and the count is Vyrn's own (RFC-0013).
- **`index.html`** — `fib` exits 55; `files` degrades to RFC-0014's canonical `Err`
  wording with no preopens; a division by zero reaches the page as
  `error: division by zero`, exit 1.

### The import ABI needed almost nothing, and that is M2h's doing

Every conversion the textual backend's `to_extern_abi` performs is already done by
the carrier invariant: a `Bool` and every sub-64-bit int ride an `i32`, correctly
extended, which is exactly what the RFC-0012 ABI widens them to. So the whole loop
is `String`, which crosses as a `(ptr, len)` **pair** — the asymmetry against an
*export*, where a `String` parameter is a single pointer because the JS caller can
allocate inside the module. The ABI table itself is `extern_abi_ll` mapped through
`wasm::abi`: shared with the textual emitter rather than respelled, for the reason
`SHIM_IMPORTS` was a list of names, and a wrong import signature is a misread
argument rather than a link error.

Two details are the classes this RFC keeps naming. Each String argument takes its
**own** scratch number, because one local for two live values is M2g's bug and here
it would hand the host a length off the wrong string. And a narrow-int result is
renormalized, because the host returns an `i32` and a JS number out of range would
otherwise be a carrier every other site reads as in-range.

Declaring the imports is **not** the AST pre-scan M2e and M2j refused three times
over. An `extern fn` *is* the import, one for one, with nothing for lowering to
disagree with — and M2p's sweep drops the ones a program never calls, which is why
`externdemo.wasm` imports its three `vyrn.*` and `proc_exit` and not even
`fd_write`: its only output crosses the boundary.

What is pinned in the repo is the half that was **absent**, on the module's bytes,
for M2o's reason: a length-prefixed `vyrn`/`jsLog` pair (so the namespace and the
name cannot be satisfied separately), and the allocator export present with a
String-taking export and ABSENT without one — the negative case being the one that
matters, since always exporting it would pass a one-sided test. The ABI *shapes*
stay browser-verified, which is where RFC-0012 has always been checked; a
`(ptr, len)` pair only means anything to a host that decodes it.

### `direct-shim` is deleted, and its own milestones are the argument

M2p left this as a question — "the question is whether that audit belongs to
RFC-0076 rather than whether the shape survives" — and the answer is that the audit
was never wired to the shape. `vyrn-codegen/tests/shim_link.rs` builds its own
guest module out of `wasm::Module` and `import_memory`; it has never called
`compile_linked`. So `Link`, `compile_linked` and `SHIM_IMPORTS` go and the
68-signature audit is byte-for-byte what it was, as is RFC-0076's generator split.
`wasm::Module::import_memory` and `SHIM_BASE` stay, for those two.

Everything else about it had already argued itself out: M2i measured the split
making a module *larger*, because M2b through M2h had emitted the runtime the shim
would have supplied; M2j emptied `PASSING_SHIM` by serving RFC-0043's clock out of
WASI directly; and M2i concluded the link "can never be the default" because it
needs clang — which is this milestone's acceptance criterion. A shape that passes
nothing, costs bytes, needs a C toolchain and gates no test is the thing this RFC
opens by refusing.

### The fold, and the gate that was not one

`directwasm` measured the wasm column over the corpus, and after the deletion so
does `parity`. Two gates over one corpus is how a number stops being about the
thing it names.

The part worth recording is what the fold exposed: **`directwasm` was never in
CI.** The parity job runs `--test parity` and nothing else, so nineteen tests
pinning the cases no example reaches — the bounds message, the DFA walk over a
non-ASCII byte, a column off both ends of a buffer, a stale cell and a full slab,
`readLine`'s three flavours of `None`, `f64_str`'s two exact ties and the 301
digits of ten-to-the-300, a suppressed log call, the post-sweep renumbering — have
been passing locally and gating nothing. They move across unchanged but for the
flag, and the fold is the first time they are checked on a machine that is not this
one.

`PASSING`, `PASSING_SHIM`, `ladder` and `blocker` are deleted. A burndown list that
has reached the whole corpus is a list of the whole corpus, and the tier that ran
twice stops needing to.

Parity: **87 checked, 0 failed, 25 tests, 51 s** — faster than the 65 s the double
compile took, and it no longer sets `WASI_SYSROOT` or `WASI_BUILTINS` at all. CI
keeps the same wasm-tools cache key as the gen-engine job (which still needs the
sysroot for RFC-0076's C shim) and takes only the wasmtime out of it. The
clang-needing native column is untouched.

### M4 is struck

Not deferred. Moving the prelude into the C shim would reintroduce the clang
dependency the acceptance criterion above forbids, in order to make modules
*bigger* — M2i measured that — and after the sweep the prelude's whole remaining
cost is 290 bytes of code in `fib.wasm`. The narrow version M2i left of it ("reach
the shim for the subsystems not worth re-emitting") named three candidates, and
M2j, M2l and M2n emitted all three (WASI I/O, the generational slot table,
`parse`/`lineAt`) without a shim. There is nothing left of the milestone.

### The criterion, demonstrated

"`vyrn build --target wasm` needs no clang, no wasi sysroot, no builtins archive"
is the RFC's headline claim, so it was run rather than reasoned about, with a
control:

- `PATH` reduced to `C:\Windows\System32` — nothing named clang on it;
- `CLANG` pointed at a stub that prints `POISONED CLANG WAS INVOKED` and exits 99.
  An *existing* file, so `find_clang` returns it — pointing it at a **missing**
  path is not a control, because `find_clang` falls back to
  `C:\Program Files\LLVM\bin\clang.exe`, which is how the first attempt at this
  proof quietly succeeded at the native build and proved nothing;
- `tools/wasi-sysroot-25.0` and `tools/libclang_rt.builtins-wasm32-wasi-25.0`
  renamed off disk, with `WASI_SYSROOT` and `WASI_BUILTINS` pointing at nothing.

**Control:** the NATIVE build in that same shell invokes the stub and exits 1.
**Criterion:** `--target wasm` builds **88 of the 89 examples** — the 89th is
`validate_compile.vyrn`, the intentional compile error — leaves no `.ll` and no
`.shim.c` anywhere, and `fib.wasm` is 1,438 bytes that wasmtime runs to exit 55.

### Acceptance

| criterion | status |
|---|---|
| parity green, interp == native == wasm, traps included, wasm column direct | 87 checked, 0 failed |
| RFC-0076's cross-engine gate green | 1 passed (every generator byte-identical under both engines) |
| `--target wasm` needs no clang, no sysroot, no builtins | demonstrated above, with a control |
| `VYRN_WASM_BACKEND` does not survive | gone, and so is `direct-shim` |

`cargo test --release`: 1,243 passed, 0 failed — the number this branch started at.
`vyrn-codegen`, `vyrn-lsp` and `vyrn-genwasm` all build.

### What the whole RFC cost, and what it removed

The estimate was 3,500–5,500 lines against the 9,465 the emitter was. `direct.rs`
is 10,507 lines including its own runtime, and the thing it replaced was not the
emitter — `lib.rs` keeps every one of those lines for native. What went is 205
lines of driver and backend plumbing, one environment variable, one link shape, one
test tier, and the double compile: two compilers run back to back with the first
one's output thrown away, on a target where clang was given no `-O` flag at all and
was therefore never optimizing. A very expensive translator, as the problem
statement put it.

The measurement that motivated this, at the end of it: `fib.vyrn` to wasm is
**1,438 bytes** where clang produced **277,438**.

The last clause of that sentence used to read "and the generation engine no longer
declines on a machine without a C toolchain", which was **false when it was
written**: this milestone changed `vyrn build`, and the generation engine had its own
call to clang. It became true one RFC over, in RFC-0076 M7, which pointed that engine
at this backend — see the note under "The problem" above, and RFC-0076's own M7
section. Recorded rather than quietly corrected, because the failure is worth naming:
an acceptance criterion about `--target wasm` was read as a claim about every consumer
of the wasm target, and the one consumer it missed was the one the opening paragraph
was about.
