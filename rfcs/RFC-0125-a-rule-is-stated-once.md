# RFC-0125 — a rule is stated once

- **Status:** Draft (2026-09-02). M0 is done: `main` requires the CI
  checks. M1's write half, header hoist and trap site all landed the same
  day and are measured in §3 M1: nbody 11.8 s to 1.98 s under Cranelift
  against 0.88 s native, output byte-identical throughout. M1's gate as
  written — Cranelift within twice native on nbody, spectral-norm and
  fannkuch — is NOT met (2.25x, 2.9x, 1.8x); the release path, wasm2c and
  clang, IS within it on all three (1.5x, 1.9x, 1.8x). The route decision of
  §2.5 is recorded with those numbers and left to be taken on purpose. M2's
  first slice landed the same day: the named core, the linear judgment, and
  a corpus test — 5,292 instances accepted, 53 refused, and two of the
  refusal classes are verified defects in the current release placement
  (§3 M2, `rfcs/probes-0125/`). The first was closed in `movecheck` the same
  day; M3's first slice, the placer, closed the second from the core's side:
  5,344 accepted, 1 refused, every probe flat, every gate green. Nothing
  after M3's first slice has landed.
- **Depends on:** RFC-0101 (the lowered form — this RFC is what its own ledger
  says it could not become), RFC-0077 (the direct wasm backend — the emitter
  this RFC keeps), RFC-0087..0091 (ownership is defined, not inferred — the
  fact that makes a linear kernel possible), RFC-0114 (release placement — the
  fifty-seven rounds this RFC replaces with one pass), RFC-0121 (a pattern is
  a place — the half of "places" that already exists), RFC-0094 (a builtin
  is a declaration — its thesis needed the one value model §2.4 supplies).
- **Evidence:** the line and arm counts in §1.1, measured at `c5ed4796`; the
  probe in §1.4, run on the machine `rfcs/bench-0104/results/2026-08-28.json`
  names, with `wasmtime 46.0.1` and node `v24.11.1`; RFC-0101's own ledger,
  quoted in §1.2; the emitted IR of `examples/binarytrees.vyrn` and
  `examples/nbody.vyrn`, read in §1.4 and §1.5.

---

## The question

Every rule this language has is written once in an RFC and then again in each
place that applies it. Release placement is written in the interpreter, the
native backend and the wasm backend, at six exit kinds each. Validation is
written at every boundary in all three. Trap wording is twenty sentences at
fifty-five sites. The coercion ladder is 505 lines of one decision, and the two
compiled backends order its rungs differently. When the copies disagree, a
parity test finds the disagreement, and a patch makes them agree again.
RFC-0114 took fifty-seven such rounds to reach zero leaks, and it reached zero
by adding a free audit, a poison fill and a ratchet on top of the rule.

RFC-0101 saw this and proposed a shared lowering. Its ledger records what
happened: "M3's −1,200 is permanently unmet", "the two compiled backends share
twenty function names and no code", and "parity would hold by construction,
and Miri refuses it". The lowering was built. The backends kept their own walk
over it, because each still had its own picture of what a value is in memory.

The question this RFC answers is: **where should a rule live so that it is
written once, and what checks it?** The answer has three parts. A rule lives in
a desugar into a small core, or in a judgment a kernel makes about that core,
or in a table the runtime reads. The kernel is a few hundred lines and runs on
every compile. An emitter reads the core and decides nothing. There is one
emitter.

---

## 1. The evidence

### 1.1 The size is a product

The workspace is 139,332 lines of Rust. The surface is small: `Expr` has 20
variants, `Stmt` has 14, `Type` has 35. The passes are not small:

| file | code | comments | `Expr::` arms | `Stmt::` arms |
|---|---|---|---|---|
| `vyrn-codegen/src/direct.rs` | 14,066 | 4,871 | 89 | 24 |
| `vyrn-codegen/src/lib.rs` | 13,262 | 4,437 | 140 | 75 |
| `vyrn-frontend/src/checker.rs` | 12,272 | 3,159 | 149 | 97 |
| `vyrn-frontend/src/interp.rs` | 8,143 | 2,739 | 97 | 39 |
| `vyrn-frontend/src/movecheck.rs` | 5,098 | 2,428 | 133 | 63 |
| `vyrn-frontend/src/own.rs` | — | — | 53 | 55 |
| `vyrn-lower/src/lib.rs` | — | — | 46 | 40 |

Thirty-one files switch on `Expr`. Twenty variants become 149 arms in the
checker because a `Call` is not one case: it is ninety builtins, each with its
own typing, and each of the three engines implements each of them against its
own value model. The interpreter matches 44 builtin names, the native backend
81, the wasm backend 50. A Map with a String key, an Int64 key and a packed key
is three layouts, in Rust, in C and in emitted wasm.

So the size is not (surface × passes). It is (surface × types × builtins ×
engines), and the factor RFC-0101 did not remove is the value model. Three
engines have three pictures of memory, and every builtin is written against
each picture.

### 1.2 The lowering could not finish, and said why

RFC-0101 M1 built a lowered form. Its header in `vyrn-lower/src/lib.rs` reads:
"M1 builds the value and nothing consumes it in anger." Its migration ledger
reads: "after every sharing move `peek` still answers 109 questions about the
backend's own locals". The backends could not stop walking because the lowered
form told them what each expression *is* and not where each value *lives*. A
decorated tree handed to three walkers is three walkers.

The same RFC counts five derivations of expression types across the tree, and
notes that the lowering had to avoid becoming a sixth.

### 1.3 Safety is patched where it is applied

The ownership model is sound. Its implementation is a walk over the surface
tree, and every construct that can hold a value without a name is a case for
that walk: argument temporaries, the temporary a `match` owns, the ones
`if let` and `for in` own, `break`, `continue`, `return`, a propagating `?`,
and a join where two edges leave different values alive. RFC-0114 lists them.
Each backend carries 1,800 lines of placement over 1,421 lines of shared
analysis (RFC-0101 §1.4). What the walk missed was caught by a free audit
(`toolchain.rs`, a spinlock and a hash table on every allocation when enabled),
a 0xDD poison fill, and a leak ratchet in CI.

`region { .. }` is the clearest case. The language has the syntax of an arena.
The runtime (`__vyrn_region_alloc`) calls `__vyrn_malloc` for every block,
records the pointer in a side list, and frees the list one pointer at a time
at the closing brace. Only string concatenation goes through it. A user who
writes `region { check(make(depth)) }` gets the syntax and none of the effect.

### 1.4 The probe: the wasm is slow because of the lowering, not the optimizer

The plan this RFC replaces would have made wasm the one intermediate form and
let an external optimizer (Cranelift, V8, or clang through wasm2c) make it
fast. Before writing that down, the claim was run. `examples/nbody.vyrn` and
`examples/binarytrees.vyrn` were built at the record's N by the native path
and by the direct wasm path, and the wasm was run under two optimizing
engines. Outputs are byte-identical after CRLF normalisation.

| program | native (LLVM `-O2`) | wasmtime 46, Cranelift | wasmtime, precompiled | node 24, V8 |
|---|---|---|---|---|
| nbody, 25 M steps | 0.88 s | 11.8 s | 11.8 s | 18.5 s |
| binary-trees, depth 18 | 1.87 s | 0.88 s | 0.88 s | 1.9 s |

Two optimizers, thirteen and twenty-one times slower than LLVM on the same
program. That is not an optimizer gap. The emitted `advance` (the hot function,
1,428 lines of wat) contains, per iteration of the inner loop:

- **21 `memory.copy` instructions.** Every `b[i].x` is lowered as: bounds
  check, element address (`i * 56`), copy the whole 56-byte record into a
  scratch slot on the shadow stack, then load one field from the slot. A
  field read is a record copy.
- **38 bounds checks**, one per element access, each a compare, a branch and
  a cold call.
- **one call to `sqrtF`**, which is a user function, so it pays the
  call-depth accounting: a load, compare, add and store of a global memory
  cell on entry, and again on exit, around a single `f64x2.sqrt`.

The native IR does the same thing: `vyrn_advance` holds 47 loads of the whole
seven-double record, one per field read. LLVM's scalar replacement turns an
aggregate load followed by `extractvalue` into one scalar load, so the native
binary never pays. Cranelift and V8 see a `memory.copy` into linear memory and
keep it. **The native path is fast because LLVM repairs the lowering.** Any
plan that removes LLVM from the default path has to fix the lowering first, and
the wasm column of the benchmarks page, thirteen times C on nbody, is the same
defect measured a different way.

binary-trees goes the other way, and the reason is the runtime, not the
codegen: the wasm runtime carries a segregated free list, the native shim calls
the platform allocator through a wrapper with an audit branch. The same source
runs twice as fast under wasmtime as natively.

### 1.5 What a recursive enum costs

`Tree = | Leaf | Node(Tree, Tree)` lowers to a 24-byte value `{tag, ptr,
ptr}`. Building a `Node` allocates a 24-byte box for the left child and another
for the right, and a `Leaf` child is boxed too. The C harness allocates one
16-byte node and represents a leaf as a null pointer. Two allocations of 24
bytes against one of 16, and half of ours hold a tag and nothing else. Rust's
harness program is written the same way, `Node(Box<Tree>, Box<Tree>)`, which is
why the record shows Vyrn at 1.11x Rust and 2.31x C on this program. The
representation is a compiler decision, invisible to the language, and it is
made in two backends.

### 1.5b Two of the output-path trio, measured

The runtime's byte sink flushed stdout twice per call and, on Windows,
switched the stream to binary mode and back around each write; the UTF-8
validator walked its DFA one byte at a time over lines known to be ASCII.
Both are runtime-only. stdout is binary from `main` now on every platform,
`writeStdout` is one buffered `fwrite`, and the validator skips an ASCII
prefix eight bytes at a time in both backends before the DFA starts, in
state 0, where the prefix would have left it. Output byte-identical to the
previous compiler's after CRLF normalisation — and on Windows the native
binary now writes the bare `0x0A` the interpreter and every other platform
always wrote, which removes the one byte-level difference between engines
the parity harness had to normalise.

| program, record's N | native before | native after | wasm before | wasm after |
|---|---|---|---|---|
| fasta | 0.96 s | 0.86 s | 1.08 s | 0.89 s |
| reverse-complement | 0.56 s | 0.41 s | 1.09 s | 0.98 s |

Neither program uses the byte sink yet; the whole gain is the validator, one
call per output line.

**The third, `clear`, landed as RFC-0115's addendum** and touched the ten
sites the builtin checklist names: parser, prelude, checker, interpreter,
both backends, the editor's rows, the primitive census (97 to 98), two
examples and the refusal registry — the product this RFC is about, priced
once more at ten places for one word. With the two line builders rewritten
to keep one buffer per sequence (`examples/fasta.vyrn`,
`examples/revcomp.vyrn`), outputs byte-identical:

| program, record's N | native, validator only | native, with `clear` | wasm, with `clear` |
|---|---|---|---|
| fasta | 0.86 s | 0.80 s | 0.93 s |
| reverse-complement | 0.41 s | 0.46 s (noisy run; 0.44 best) | 1.06 s |

The reverse-complement native number did not move outside its noise on this
run; its remaining cost is the three per-byte passes and the bounds checks,
which are the fixed-array and views items, not the allocator.

### 1.6 The proportion

Of 139,332 lines: about 35,500 are one semantics written three times, about
8,700 derive ownership from the surface tree, about 1,600 are a C runtime that
the wasm backend re-emits by hand, and about 30% of every large file is prose
that retells an RFC. The rest is a parser, a loader, a CLI, an LSP and tests,
and is ordinary.

---

## 2. The design

### 2.1 The core: every value has a name, every access is a place

The lowering produces a program, not a decorated tree. The program is in a
named form: every intermediate value is bound. `check(make(depth))` becomes
`let t = make(depth); let r = check(t)`. There are no temporaries, because
there is nothing without a name.

Every access to memory is a **place**. `b[i].x` is the address
`elem(b, i) + offset(x)`, and a read of it is one scalar load. A write
`b[i].vx = e` is one scalar store. A place is never copied to read a field
of it. RFC-0121 made match payloads places; this makes every field, element
and projection one. The `memory.copy` in §1.4 does not exist in this form.

The core has these node kinds and no others: `let`, `call`, `prim` (an
arithmetic, compare or conversion primitive with one specification row),
`load`, `store`, `addr` (a place), `if`, `loop` with `break` and `continue`,
`return`, `switch` on a tag, `drop`, `validate`, `trap`. Control flow stays
structured, which is what RFC-0101 §2.2 decided and what wasm requires.

Every surface form is a desugar into this. `?`, `if let`, refutable `let`,
interpolation, tagged templates, projections, `for in`, `a[i].f = v`, closures
after defunctionalization, protocol dispatch after monomorphization. A desugar
is a source-to-source transform that `vyrn emit-lowered` prints, and a person
can read the output.

Evaluation order is left to right, fixed by the naming. The interpreter
already agrees; the parity corpus checks it the day the naming pass exists.

### 2.2 The kernel: three judgments

A kernel of a few hundred lines re-checks every core program on every
compile. It knows nothing about the surface language. It makes three
judgments:

1. **Linear.** Every owned name is consumed exactly once on every path from
   its binding. Consumed means passed to a `consume` parameter, returned,
   stored into a place, or dropped. A name consumed twice is a double free.
   A name never consumed is a leak. Both are refused at compile time.
2. **Effect.** Every body's effects are within what its signature declares and
   what its target provides. There are four effects: allocates, does I/O, may
   trap, may spawn. A host import is an I/O effect. A target (RFC-0103's
   floor) is the set of effects it provides. An audience (RFC-0072's fence) is
   the set of declarations it may see. Both become inclusion checks.
3. **Typed by construction.** A name has the type its producer gave it, and a
   validated type has exactly one producer: its `validate` function. A value
   of validated type `T` therefore exists only if it passed `T`'s predicate.
   No boundary needs a check, because the type is the proof. The coercion
   ladder is gone; a coercion is a call.

The drops the linear judgment checks are inserted by one liveness pass, at
the last use of each name, with drops on the edges of a join where the live
sets differ. That pass replaces the six exit kinds, the placement walks in
every engine, the free audit, the poison fill, and the ratchet as a language
gate. This is Rust's drop elaboration on MIR and Koka's Perceus, and both
exist because placement by tree-walk is what produced fifty-seven rounds here.

Borrowing stays second-class. A `read` or `modify` parameter lives for one
call and cannot be stored, which is why there are no lifetimes and why the
kernel stays small. A view of an array range is the same thing: creatable
from a `read` parameter, passable to `read` parameters, never stored. RFC-0109
waited for a payer; §1.4's reverse-complement at 231 MB against C's 45 is the
payer, and the check is the one the kernel already makes.

### 2.3 One emitter, and it never optimizes

The emitter reads the core and writes wasm. It maps `prim` rows to wasm
instructions, `load` and `store` to typed loads and stores at computed
addresses, `drop` to a call, `trap` to a call with a table index, and control
flow to wasm's blocks. It decides nothing: it does not place releases, does
not check bounds it was not told to, does not know what a validated type is.
It is the direct backend of RFC-0077 with its runtime removed and its walk
replaced.

The emitter carries no optimizer, ever. Optimization is the job of whatever
runs the wasm, and every runtime that runs it is better at that than a
hand-emitter will be. §1.4 shows the condition: the core must already be
place-based. Then the optimizer has scalar loads to work with, not copies.

The recursive-enum representation of §1.5 is decided here, once: an enum whose
payload-less variant can be a null pointer is one pointer, boxed at the enum,
payloads inline in one box. Half the allocations, a third the bytes.

### 2.4 The runtime is Vyrn

Allocator, Map, String, Array operations, validation functions, the trap
table and their wording live in one Vyrn module. The raw-memory primitives it
needs (load, store, allocate pages) are the only unsafe surface in the
language, fenced in that module, and reviewed there. Both the C shim's logic
and the runtime the wasm backend hand-emits are replaced by it. The allocator
is the segregated free list the wasm runtime already has, because §1.4
measured it winning.

The C side of a native binary becomes the WASI host: read, write, clock, exit,
arguments. About two hundred lines, supplied by the route in §2.5.

### 2.5 Execution routes

The compiler produces one thing. It runs four ways:

| use | how | needs |
|---|---|---|
| `vyrn run`, `test`, `bench`, site export | wasmtime, already embedded for generators (RFC-0076) | nothing new |
| `vyrn build` | Cranelift ahead-of-time, an ordinary executable | wasmtime's compiler, already a dependency |
| `vyrn build --release` | wasm2c to C, clang at `-O2` | clang, recorded not pinned, as today |
| browser | V8 | nothing |

This route is a claim until M1 re-runs §1.4's probe on place-based output. If
the Cranelift number stays outside twice the LLVM number on the numeric
kernels after that, the fallback is one core, one runtime in Vyrn, and two
emitters, wasm and the existing text-IR path. The interpreter, the placement
walks and the C shim's logic still go; the LLVM emitter stays a second place
that must agree with the first, and the kernel checks both.

### 2.6 What replaces parity

Parity compared three engines that nobody else runs. After this, the same
bytes run in wasmtime, V8 and, through wasm2c, clang: three engines maintained
and fuzzed by other people. The oracle becomes the expected-output fixtures,
which exist, plus an invariant parity could not state: **the emitted wasm for
every fixture is byte-identical across the CI matrix.** One hash per fixture,
seconds against the parity job's five minutes, and it catches a compiler that
depends on its host.

The interpreter is deleted, not shrunk. Its value model is the third picture
of memory in §1.1, and `vyrn run` executing the wasm is faster than the
tree-walker by the factor the site export already measured for generators.

### 2.7 What is deleted

The interpreter's evaluator and `Val` model. `movecheck.rs` and `own.rs`. The
placement walks in both backends. The free audit, the poison fill and the
audit lock. The coercion ladder in both backends. The runtime hand-emitted by
`direct.rs`. The C shim's allocator, maps and strings. The three trap tables.
The estimate is 139,000 lines to between 40,000 and 45,000, before the
narrative comments are moved to the records they retell.

### 2.8 What is not in this RFC

- **Self-hosting.** Once the compiler is a core, a kernel and an emitter, it
  is the size of a serious Vyrn program and should be one; output is
  deterministic, so building it with the old compiler and with itself and
  comparing bytes is the "trusting trust" check. That is a later RFC, and it
  needs the recursion limit to become a stack-pointer compare first.
- **The surface census.** Three array kinds, three Map key kinds, and several
  result-like types can become library over fixed arrays and canonical packs.
  Each is a checker case that becomes a desugar. A later RFC, done while the
  compiler is still in Rust and easy to change.
- **wasm32's 4 GB limit, 128-bit SIMD lanes, and threads.** Known costs of the
  route in §2.5, stated so nobody discovers them later. Wider SIMD types lower
  to pairs. Threads need the wasm threads proposal in whichever native route
  is chosen. Memory64 lifts the limit when engines have it.

---

## 3. Migration

Two rules from RFC-0101 §3.0 stand: every milestone writes its prediction as a
program before it lands, and reports the result either way; and no milestone
deletes what it replaces until the corpus is green on the replacement.

### M0 — the gates are required

Not code. `main` gets branch protection requiring the workspace tests and the
parity job. On 2026-09-01 a PR merged with four jobs still running because
nothing required them. Every gate below is optional until this is on.

**Done (2026-09-02).** `main` requires eleven checks before a merge: the
workspace tests on all four platforms, three-way parity, cross-engine
generation, the four `checks` jobs, and the site `build`. The branch need
not be up to date with `main`, and administrators are not bound — on
purpose, and for one reason: `ci.yml` ignores `rfcs/**` and `**.md`, so a
PR that touches only a design record runs no CI at all, reports none of the
eleven, and would be unmergeable by anyone. Until CI runs a job on every PR,
the owner merging a docs-only PR by hand is the escape, and it is the only
one.

### M1 — places, and the probe re-run

The current lowering and the direct emitter learn places: a field read is a
scalar load at a computed address, a field write is a scalar store, `a[i]` is
an address. No named form yet, no kernel yet. The prediction: the 21
`memory.copy` in `advance` go to zero, and nbody under wasmtime moves from
11.8 s toward the native 0.88 s.

**Gate:** the parity corpus green, the record's wasm column re-measured, and
the §1.4 table re-run. M1's number decides §2.5. If Cranelift lands within
twice LLVM on nbody, spectral-norm and fannkuch, the one-emitter route is
taken. If not, the two-emitter fallback is written into §2.5 and the rest of
the migration proceeds unchanged.

M1 is useful on its own: it is the wasm column's thirteen-times defect, fixed.

**The write half landed (2026-09-02), and here is its number.** The copies
in §1.4 come from the write side only: the direct emitter already read
`b[i].x` through the element's address, and the parser's `a[i].f = v` idiom
(copy the element out into an unspellable temp, store the field, copy it back)
made every field write two 56-byte copies. `Fn_::elem_field_store` in
`direct.rs` recognises the idiom on a heapless element and emits one bounds
check, one address and one store — the same prefix `Stmt::IndexSet` emits, plus
a field offset. The plan's store decisions on the three statements are
acknowledged so RFC-0114 §26's finish check sees them considered; a heapless
element owes no release. `vyrn-cli/tests/fieldstore.rs` pins both halves of the
rule: zero element-sized copies for a heapless element, the idiom kept for an
element that holds heap. Parity: 41 passed, three engines byte-identical.

| nbody, 25 M steps | before | after |
|---|---|---|
| `memory.copy` per inner iteration of `advance` | 21 | 3, all 24-byte header copies once per call |
| wasmtime 46, Cranelift | 11.8 s | 3.74 s |
| node 24, V8 | 18.5 s | 2.97 s |
| native, LLVM `-O2` | 0.88 s | 0.88 s |

Three times faster, and still four times off native. What the loop still
pays, per inner iteration, read from the new wat: 29 array-header reloads
(`walk` re-reads `data` and `len` from the binding's slot at every access,
because a `f64.store` into linear memory may alias the slot and no engine can
prove it does not), 29 bounds checks with their address arithmetic, and the
`sqrtF` call's depth accounting around one instruction. `-C inlining=y` made
wasmtime slower (4.5 s), so the call is not the cost; the reloads and checks
are. That is the read half of M1: a `walk` cached per binding for the extent
of a loop the binding is not written in, and one check per binding-and-index
pair. The gate is not decided until that half is measured. The prediction
stands as written: within twice native, or the fallback.

**The header half of the read side landed the same day.** `Fn_::hoist_walks`
takes the header of every array, fixed array, small array or String a `while`
indexes apart once, before the loop, when `header_invariant` can show on the
syntax that nothing in the loop moves it: no assignment to the binding, no
shadowing `let` or pattern, no `drop`, no `consume`, the binding never handed
whole to a call other than `@at` (a `push` is an assignment, a `pop` is such
a call), and no lambda that mentions it. Module state is never hoisted. An
element store and the field-store idiom keep the hoist, because neither moves
a header. `at`, `Stmt::IndexSet` and `elem_field_store` read the hoisted
locals. Two tests in `fieldstore.rs` pin it: one header read for a loop that
only reads, a reload per access for a loop that pushes. Parity: 41 passed.

| nbody, 25 M steps | write half | + header hoist |
|---|---|---|
| header reloads per inner iteration of `advance` | 32 | 5, none in the loop |
| wasmtime 46, Cranelift | 3.74 s | 3.56 s |
| node 24, V8 | 2.97 s | 2.16 s |
| native, LLVM `-O2` | 0.88 s | 0.88 s |

V8 took the reloads' cost and moved; Cranelift barely did, so the reloads were
not what Cranelift was paying for. What is left per iteration is 29 bounds
checks with their address arithmetic and one `sqrtF` call. Which of those
Cranelift pays for is the next measurement, and it is cheap to take: a
throwaway build with the check emitted as nothing puts a ceiling on what
check elimination can be worth before any of it is designed.

**The ceilings, measured, and what they said.** Three throwaway builds, each
a one-line refusal behind an environment variable, none of them kept:

| nbody under Cranelift | time |
|---|---|
| as landed above | 3.56 s |
| depth accounting emitted as nothing | 3.31 s |
| bounds checks emitted as nothing | 1.71 s |
| checks kept, the trap CALL replaced by `unreachable` | 1.71 s |
| both accounting and checks gone | 1.34 s |

The last two rows are the finding. Twenty-nine compare-and-branch pairs cost
nothing measurable; twenty-nine call sites inside them cost 1.85 s. The engine
was paying for the call, not the check, so no check elimination was designed.
Instead every check now parks its message and its index in two locals and
branches to one trap site per function, where the one call stands. Every
message, every exit code and every trap is byte-identical: parity 41 passed.

**The defect this found.** The first cut of the trap site put the call after
the function block and returned from inside it. The frame's own note on
`Frame::ins` says a body must never emit `return`, because the shadow-stack
pop is appended at finalization and a `return` jumps past it. It did: 48
bytes leaked per call, nbody and fannkuch died with a wasm memory fault
after enough calls, spectral-norm survived by making fewer. The trap block
now nests inside the function block, so a `return` still branches out over
the trap call into the epilogue, and only a failed check can reach the call.
Reported here because the rule was written down and was still broken once.

**M1's number, on the three programs the gate names.** Native is the
text-IR path through clang at `-O2`; the wasm is the same program, output
byte-identical to native in every cell.

| program | native | wasmtime 46, Cranelift | V8 | Cranelift ÷ native |
|---|---|---|---|---|
| nbody, 25 M steps | 0.88 s | 1.98 s | 1.60 s | 2.25x |
| spectral-norm, n = 5500 | 0.98 s | 2.83 s | 1.29 s | 2.9x |
| fannkuch, n = 11 | 2.0 s | 3.58 s | 7.1 s | 1.8x |

Where the day started, nbody's cell read 11.8 s. Two more throwaway
measurements bound what is left. With depth accounting emitted as nothing,
nbody is 1.70 s and inside the gate; spectral-norm moves from 2.83 to 2.75
s and fannkuch not at all. With Cranelift's inliner on (`-C inlining=y`),
spectral-norm does not move and nbody gets slower. So spectral-norm's
remaining 2.8x is not the checks, not the depth counter, and not the calls
the engine can inline, and it is not explained by anything measured here.

**The gate is not met.** Cranelift is outside twice native on two of three.
One of our own items would bring nbody inside: emit the depth check only in
functions that are part of a recursive cycle, which is a call-graph question
the frontend can answer. spectral-norm needs a profile before anything is
designed for it. Per §2.5 that means the two-emitter fallback is the route
unless the read half continues; that is a decision about how much of M1 to
spend before choosing, and it is recorded here as open rather than taken.

What the record's wasm column will show when the harness is re-run is a
separate step of this milestone and has not been done: these numbers are the
probe's, on the probe's machine, at the record's N.

**spectral-norm, read rather than profiled.** wasmtime's guest profiler
writes nothing for a program that leaves through `proc_exit`, which every
Vyrn program does, so the wat was read instead. The inner loop of
`multiplyAv` is already tight: one call to `cell`, one bounds check that
branches, a load, a multiply, an add. `cell` itself is a fifty-instruction
function: the depth accounting, the two guards an `i64.div_s` by a runtime
value carries (zero, and MIN by -1), the division, a convert and an `f64.div`.
It is called 121 million times. LLVM inlines it to nothing and folds the
divide by two into a shift. Cranelift calls it, and under wasmtime's calling
convention every value live across a call — the sum, both indices, the
bound, the vector's data and length — is spilled and reloaded around each
one. That is the 1.85 s, and `-C inlining=y` did not take it because the
engine's inliner declined a function of that size. So spectral-norm's gap is
the cost an external optimizer needs inlining to remove, Cranelift's inliner
does not remove it, and the route that would (wasm2c and clang) is the
release path §2.5 already names and has not been run on this machine, because
wasm2c is not installed. That measurement is what the route decision waits
on now, not more emitter work.

**The release path, measured.** wabt 1.0.41 (`wasm2c`) and simde v0.8.2
(the SIMD header wasm2c's output includes, for the one `f64x2.sqrt`) were
installed into the gitignored `tools/`, recorded here the way clang is
recorded and not pinned. Each program's wasm — the same bytes the Cranelift
and V8 columns ran — was translated to C and compiled with the native
path's own flags, `clang -O2 -ffp-contract=off`, against a two-hundred-line
WASI host (`fd_write`, `proc_exit`). Output byte-identical to native in
every cell. Guard-page memory checking is wasm-rt's default on 64-bit and was
confirmed to be in effect, so no row below pays an explicit bounds check per
memory access.

| program | native | Cranelift | V8 | wasm2c + clang | the same, without wasm-rt's stack counter |
|---|---|---|---|---|---|
| nbody | 0.88 s | 1.98 s | 1.60 s | 1.30 s (1.5x) | 1.06 s (1.2x) |
| spectral-norm | 0.98 s | 2.83 s | 1.29 s | 1.90 s (1.9x) | 1.25 s (1.3x) |
| fannkuch | 2.0 s | 3.58 s | 7.1 s | 3.64 s (1.8x) | 3.55 s (1.8x) |

The last column is a bound, not a route: on Windows wasm-rt has no
signal-based stack exhaustion, so it counts call depth in every function
prologue, which is a second counter beside Vyrn's own. The build with
`WASM_RT_NONCONFORMING_UNCHECKED_STACK_EXHAUSTION` removes it and is named
non-conforming by wabt for a reason; it says what a host with signal-based
exhaustion (Linux, macOS) would see, and it is 0.25 s on nbody and 0.65 s on
spectral-norm — spectral-norm's 121 million calls again. LLVM inlined `cell`
as predicted: the conforming column already halves spectral-norm's gap.

fannkuch does not move through any of it: 1.8x on the release path, 1.8x
under Cranelift, with or without either counter. Its remaining cost is in
the emitted shape itself, not in what runs it, and it is not explained by
anything measured in this milestone. It is the next thing to read.

**What the gate says now.** As written, the gate named Cranelift, and
Cranelift does not meet it. The release path meets it on all three programs
in its conforming form, 1.5x, 1.9x and 1.8x, and the Cranelift build is the
development build in that route, the way the interpreter is today. So the
measured answer is: one emitter is viable if the release build is wasm2c and
clang, and the default build is accepted at two to three times native. That
is a trade to make on purpose, and it is the route decision §2.5 left open.

### M2 — the named core and the linear judgment, beside the pipeline

The lowering emits the core of §2.1. The kernel makes the linear judgment. In
debug builds, every compile runs the kernel and refuses on failure; nothing
else changes. The prediction: the kernel accepts all 216 corpus programs, and
refuses each of the leak witnesses RFC-0114 recorded when their fixes are
reverted on a branch.

**Gate:** the corpus accepted, the reverted witnesses refused, the leak ratchet
still at zero.

**The first slice landed (2026-09-02), and the prediction was wrong in the
way M2 exists to find.** `vyrn-lower/src/core.rs` builds the named core for a
function instance from the checker's types, the plan's decisions and the
declarations' answer to "does this type own heap", and derives nothing
about ownership itself; `vyrn-lower/src/kernel.rs` makes the linear
judgment over it; `vyrn-cli/tests/kernel.rs` runs both over every example.
Over 164 programs the tally is:

| instances | count |
|---|---|
| accepted by the kernel | 5,292 |
| refused | 53 |
| unlowered, by construct | 1,229 |

The unlowered constructs are a list, not a feeling: a read of module state
that owns heap in a take position (719), a `consume` hole (177), a move out
of a field or an element (155), a lambda (28), a `consume` of a place that
is not a name (31), a map lookup as a place (7), a `region` (7), and a few
dozen expressions the checker never typed or calls this slice cannot
attribute. Each is a rule to state, and none is a guess.

The 53 refusals are four classes, and two of them are verified defects:

1. **`push` in expression position whose result escapes.** `return match
   parseJson(src) { Ok(j) => out.push(j), .. }` in `std/jsondec`'s
   `readDoc`: the plan releases `out` at the return, `@push` hands `out`'s
   buffer back as the result, and the caller receives a freed buffer. It has
   worked because `out` was empty there. `rfcs/probes-0125/push-in-expression-position.vyrn`
   does it with three elements: the interpreter prints `4 1 2 3 4`; the
   native binary dies with `free audit: double or foreign free` under the
   audit and crashes without it. Nine instances in the corpus.
2. **A String returned on one path and left on the other.** `let stray =
   gqlNoArgs(..)` then `if .. { return .. stray .. }` and nothing after: the
   plan places no release for `stray` on the fall-through, and neither does
   Rule N, because the taking edge returned rather than joined.
   `rfcs/probes-0125/returned-on-one-path.vyrn` calls such a function on the
   untaken path 200,000 and 400,000 times: peak working set 20.1 MB and
   27.8 MB, about 38 bytes per turn, which is the String. Thirteen instances
   in `std/graphql`, `std/rpc` and `std/tw`.
3. **A payload binder an arm never reads** — `parseErr`'s `Ok(v) => ""` —
   the same shape as 2 one construct over. Twelve instances, predicted from
   the kernel's refusal and not yet probed.
4. **An arm the program cannot reach**, the `None` arm of a lookup by a key
   the loop just listed, in four generated map encoders. The kernel judges
   every path; the plan judged the reachable ones. Not a defect; a rule the
   kernel is stricter about, recorded.

One more instance, `smallarray.vyrn`'s `main`, the lowering misreads and
the plan does not. The corpus test's gate is a ratchet on the count, 53,
which may fall and not rise; each class closes by fixing the plan.

**Class 1 closed the same day.** `movecheck::sinks` now answers that a
seeded row whose result is its receiver's own type — `push`, `reserve`,
`append`, `copyFrom`, a map's `tally` — takes the receiver, because the
buffer comes back through the result. The write-back statement,
`xs = xs.push(v)` and `s.keys = s.keys.push(k)`, is the one exception: it
takes and revives in one line, so its take is not recorded and the untake
fold sees what it always saw. `return xs.push(v)` now moves `xs`, the plan
places no release for it, and the probe prints its five lines natively with
the free audit on. One example changed: `examples/map.vyrn`'s `put` returned
`a.push(..)` on a `read` parameter, which handed the caller's buffer back
while the caller still held it; its parameter is `consume` now, which is what
returning the buffer meant. Parity 41 passed, the residue ratchet held, the
frontend's 1,176 unit tests passed, and the kernel's count fell to 42:
5,303 accepted, 42 refused, 1,229 unlowered. The unreachable-arm class went
with it, because the encoders' `fs.push(..)` is a take now and Rule N
places the other arm's release.

**Class 2 has a second half, and the plan's own fold names it.** Beside the
conditional `return` with a fall-through, the kernel refused every reader
in `std/von` whose accumulator is taken by the final `return Ok(..)` after
a loop that returns `Err` from inside. `rfcs/probes-0125/early-return-before-the-take.vyrn`
runs that shape 200,000 and 400,000 times: peak working set 98 MB and 212 MB,
about 570 bytes per turn, which is the array — and the compiler as it stood
before this branch measures 92 MB and 212 MB on the same probe. Pre-existing,
not a regression of the take rule. The fold that places early-exit releases
(`own.rs`, round forty-two) says why in its own words: "the in-loop exits
keep their leak until the fold can order across a back edge". Ordering
across a back edge is what the named core has and the event log does not,
so this class is M3's to close, and the kernel now lists every instance of
it rather than the one round forty-two remembered.

**What the slice learned about the plan, recorded because M3 replaces it.**
The plan is not one table but nine, and a body is right only when all nine
are read together: the placed release rows, the argument-temporary drops,
the store releases with their `mentions_place` stand-down and the
`store_fresh` override of that, the edge releases at joins, the receiver
frees, the arm payload frees, the consuming matches, the per-binding fate
notes, and the per-loop drop kind whose `FreeArr` value means "the body took
the elements". Two rules live in the emitters and not in the plan at all: a
String temporary is freed by the site that reads it (RFC-0096 M3), and a
builtin whose result is its receiver's type hands the buffer back, so the
receiver is taken by the call. The kernel reproduces every one of these to
judge the plan, which is the measure of how much M3's one liveness pass
deletes.

### M3 — the emitter reads the core

The direct emitter walks the core instead of the AST. Release placement in
that emitter is deleted; drops come from the core. Then the same for the
native emitter, or its deletion, per M1's decision.

**Gate:** parity green with placement code deleted from every emitter that
reads the core. `movecheck.rs` and `own.rs` are deleted when the last consumer
of their answers is gone.

**The first slice landed (2026-09-02), the other way round: the core places
what the plan owed, into the plan.** Rather than teach an emitter to read
the core, the kernel walks every body in placement mode inside
`own::analyze` and fills the plan's own tables with what it found missing —
a release row at the exit where a name is still held, keyed by the exit's
node and the binding's node exactly as the plan keys its rows, and the
binding entered in the droppable table so every engine registers a slot; an
edge row where one edge of a join holds what another took (Rule N's table);
an arm row where a payload binder was never moved (round forty's table). The
three engines then consume the rows through the one path RFC-0101 M4 gave
them, and no emitter changed. The placer is installed by the CLI at start-up
the way the generator engine is (`vyrn_lower::install`); `VYRN_NO_PLACER=1`
compiles against the analysis alone.

| kernel over the corpus | analysis alone | with the placer |
|---|---|---|
| accepted | 5,303 | 5,344 |
| refused | 42 | 1 |

The one refusal left is the lowering's own misread of a SmallArray literal.
The three probes under `rfcs/probes-0125/` are flat: 4.2 MB peak at 200,000
and at 400,000 turns for both leak shapes, against 20 to 28 MB and 98 to
212 MB before, and the push probe prints its five lines with the audit on.
Parity 41 passed, the residue ratchet held, the lowered-form gate, the
frontend's unit tests and the wasm tests passed.

What this says about M3's shape: the plan's nine tables are a *protocol*
between the analysis and the emitters, and the core can speak it. So the
emitters need not read the core until the plan is deleted; until then the
core corrects the plan, and each correction is an exit row, an edge row or an
arm row the emitters already know how to run. The event-log fold that could
not order across a back edge is not repaired; it is overruled, per body, by
a walk that can. One caveat the emitters carried: the arm table freed a
single binder only, so a multi-binder arm's row was placed and not run, which
is a leak the ratchet does not see.

**The caveat closed the same day.** The arm row is `(match, arm) ->
[(binder, kind)]` now: the analysis writes its one screened binder, the
placer writes every binder the kernel found held at the arm's end, and each
engine frees the binders its row names and no other (`direct.rs` `match_expr`,
`lib.rs` `gen_arm_body` and `gen_match_enum`, the core's `St::Drop` per named
binder). The interpreter gained its first consumer of the table, because a
declared `release` on a payload binder is observable there and the placer
can place one; a buffer it frees by `Rc` as before.
`rfcs/probes-0125/two-binders-neither-read.vyrn` is the witness: a
two-`String` variant whose arm reads neither payload beside an arm that hands
its payload out. Peak working set natively, before and after:

| turns | before | after |
|---|---|---|
| 200,000 | 28.8 MB | 4.1 MB |
| 400,000 | 50.7 MB | 4.1 MB |

About 112 bytes a turn, the two Strings. Parity 41 passed, the residue
ratchet held.

### M4 — the runtime in Vyrn

The runtime module of §2.4, compiled by the emitter into every program. The
hand-emitted runtime in `direct.rs` and the logic in the C shim are deleted;
the shim becomes the WASI host.

**Gate:** parity green, the free audit and poison deleted, binary-trees under
the native route at or below its wasmtime time from §1.4.

### M5 — `vyrn run` is compiled

`run`, `test` and `bench --check` execute the wasm in the embedded wasmtime.
The interpreter is deleted. The parity job is replaced by the cross-platform
hash of §2.6 plus the fixture comparison.

**Gate:** every fixture's output identical to the recorded expected output;
every fixture's wasm byte-identical across the matrix; site export time
recorded against its interpreter baseline.

### M6 — the other two judgments

Validation by construction replaces the boundary checks. The trap primitive
and its table replace the sites. The effect judgment replaces the audience
and floor passes. Each is a prediction-as-program: one boundary check deleted,
one program that must still refuse.

**Gate:** the audience, floor and contract test suites green with their passes
deleted.

### What each milestone is worth on its own

M1 fixes the wasm column. M2 makes leaks a compile error. M3 halves the
emitters. M4 makes the runtime one file. M5 makes `run` compiled and CI
minutes into seconds. M6 makes every remaining rule one rule. Any of them can
be the last one landed and the language is better than before it.

---

## Open questions

1. **The kernel's own trust.** It is a few hundred lines and it is the trusted
   base together with the emitter. The intent is that it stays small enough to
   read in one sitting; the check is a line budget in the test, the way M3 of
   RFC-0101 carried one.
2. **Regions.** With drops at last use, a region is a bump arena that the
   linear judgment already understands: values allocated in it are consumed by
   its close. Whether `region { .. }` keeps its syntax or becomes a library
   type over the runtime's arena is M4's question, and the regions census's
   verdict on *inferred* regions stands either way.
3. **Reference counting.** The linear judgment is the same for unique
   ownership and for precise counting. This RFC keeps unique ownership, because
   the language chose explicit copies. Counting stays available as a runtime
   change under the same kernel if `.copy()` cost becomes the complaint users
   actually have.
