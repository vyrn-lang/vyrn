# RFC-0077 — A Direct Wasm Backend: Stop Going Through LLVM

- **Status:** Draft
- **Depends on:** RFC-0076 (generators as wasm; the shared runtime shim and the
  memory map it established), RFC-0012 (the `extern` ABI), RFC-0037
  (defunctionalized closures — the reason *closures* need no function table),
  RFC-0025 (`spawn`, which is the reason a small one is needed anyway — see the
  M2a pre-flight)
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
- **No function table** — *wrong, and the M2a pre-flight caught it.* Indirect
  calls really are zero, and RFC-0037's defunctionalization really does route
  stored `fn` values through a synthesized `switch` into direct calls. But
  function-addresses-as-values is **9**, not 0: RFC-0025's `spawn` hands the shim
  a thunk symbol (`call @__vyrn_spawn(ptr @__vyrn_task_*, ptr)`) and the shim
  calls it, which in wasm needs a table element, `ref.func` and `call_indirect`.
  Bounded and enumerable — 9 sites across 3 examples, all syntactic — but a
  milestone that had assumed this away would have discovered it at the end.
- **No generics** — *wrong, and M2e is where it was corrected.* Monomorphization
  does not run before any instruction is emitted, in either backend: a
  specialization is **discovered** at a call site as a side effect of emitting
  the body containing it (`Gen::instantiations`, drained by the driver). So a
  direct backend cannot consume monomorphized code; it needs the same
  interleaved shape, and it needs one more thing the textual one does not,
  because a wasm call names a function *index* rather than a symbol. See
  "M2e, as landed".
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

### The correction: there IS a function table, and it is `spawn`

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

### Verdict

The design survives. No relooper, no reconstruction of joins, one small table
whose contents are a compile-time list.

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
