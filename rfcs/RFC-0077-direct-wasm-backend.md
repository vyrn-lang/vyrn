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
