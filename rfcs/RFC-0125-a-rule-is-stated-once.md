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
  5,344 accepted, 1 refused, every probe flat, every gate green. M2's
  second slice lowered every construct: 6,581 accepted, 0 unlowered, 9
  refused in five classes, each a leak the plan cannot express and each
  measured by a probe (§3 M2, "the second slice"). M5's first slice landed
  the same day: `run`, `test` and `bench --check` take `--engine wasm` and
  run the direct backend's module in a wasmtime the CLI embeds, and the
  fixture gate of §2.6 exists — 201 of 203 examples byte-identical to the
  recorded output, 2 skipped by name. The interpreter stays the default and
  is not deleted; the site export does not compile yet (`listDir` has no
  lowering), and the numbers are in §3 M5. Nothing after that has landed.
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
measured it winning. The design for this — the inventory of all three
runtimes with counts, the primitive set, the fence, the allocator, the
migration order and the cost — is `PLAN-0125-runtime.md`.

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

That invariant is a test today, before M5: `compiler/vyrn-cli/tests/wasmhash.rs`
builds every example `vyrn check` accepts with `--target wasm` and writes one
SHA-256 per example to `rfcs/census/wasm-sha256.tsv`. The committed file is the
reference. CI's `wasmhash` job runs the test with `VYRN_WASM_MANIFEST=check` on
each platform of the matrix, and a leg whose bytes differ fails and names the
example. A change to the direct backend regenerates the file with
`VYRN_WASM_MANIFEST=write` and commits it beside the change. The test builds
each example from `examples/` by its bare name: a generated module's symbol map
(RFC-0073) keys origins by the path the loader was given, so an absolute path
puts the checkout's location into the bytes. That is the first host dependence
the manifest found. It is recorded here rather than fixed, because the key is
the LSP's contract.

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

**One check runs on every PR now.** `.github/workflows/docs.yml` has no
path filter and one job, `rfc-index`, reported as "the RFC index agrees with
the directory". It runs `cargo test -p vyrn-cli --test rfc_index`, the gate
that says `rfcs/README.md` is derived from `rfcs/` — the one check a docs-only
PR most needs, and the one `paths-ignore` had been skipping on exactly those
PRs. It is a twelfth check, not a replacement: the eleven still do not
report on a docs-only PR, so the owner's hand merge stays the escape for
those until the required list on `main` is changed to say what a docs-only
PR must pass. That list is a repository setting, and this RFC records the
choice rather than making it.

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

**The second slice landed (2026-09-02): aggregates are built in place, and a
statement's temporaries are its own.** M5's second slice priced the site
export at the frame limit: `chapters` needed 11,360 bytes of frame and the
generated `uiPageBody__from0` 25,984, against `FRAME_LIMIT`'s 8,192, and
raising the constant only moved the wall. Both numbers were §1.4's per-node
copy in a different coat: every nested aggregate of a literal was built in a
slot of its own and copied into its consumer's, and every slot a body ever
took was added up. Two changes in `direct.rs` and one in `wasm.rs`, on
`track-i`:

- **Destination passing.** A record literal, an array literal, a sum
  constructor and a call that returns an aggregate write straight into the
  consumer's storage when the consumer owns storage that holds that very
  type (`Dest`, `Fn_::agg_into`). The consumers: a `let`'s slot (annotated or
  not; the typer names the type first), a field of a record literal, an
  element of an array literal, a `return`'s hidden destination, and a field
  or element store. A hint is taken at the top of `expr_inner` so only the
  immediate expression sees it; a call carries it into `call_inner` before
  any argument is lowered, so a nested call cannot claim it. An array literal
  in an `Array<T>` position takes its heap buffer first and builds the
  elements in it, hinted or not, so the fixed `[N x T]` frame extent and
  `heapify`'s second copy never exist for it. **The rule, stated once: a
  value is built in place only into storage nothing can name while it is
  being built.** A fresh `let`, a literal under construction and a call's
  result are such storage. An assignment, module state, and a field or
  element store whose value mentions the binding or contains a call keep the
  copy, because the interpreter builds the whole value before it stores and
  a field written early would be readable by a later field's initializer. A
  variable, a field read, a coercion and a builtin's result still copy.
- **A statement's temporaries are its own.** `Frame::alloc` handed out
  offsets for the whole function and never reused one. `Fn_::block` now takes
  `Frame::mark` before each statement and `Frame::reset`s to it after one that
  left the scope's length as it found it; a `let` keeps everything it took.
  The prologue claims the high-water mark, which `Frame::bytes` now reports.
  The first cut claimed the final `frame` instead, and every program printed
  its lines without newlines: `print_str`'s buffer sat in the part of the
  frame the prologue no longer claimed. Reported here because the encoder
  had two spellings of one number and the wrong one was reached first.
- Field reads and element reads were already addresses in this emitter
  (the first slice above): a scalar field is one load, an aggregate field is
  `i32.add`. Nothing changed there.

`VYRN_FRAME_TRACE=1` prints every body's frame to stderr, refused or not.
The ten largest in the site export, at the base commit and here:

| body | base | this slice |
|---|---|---|
| `chapters` | 11,360 | 2,240 |
| `docLines` | 4,288 | — |
| `guideLines` | 3,328 | — |
| `docKinds`, `docNames`, `docProse`, `docSigs` | 2,768 each | — |
| `docTests` | 2,480 | — |
| `vyxGroupNodes` | 2,416 | 1,568 |
| `vyxParseElem` | 1,424 | — |
| `routes` | — | 3,072 |
| `uiPageBody__from13` | — | 2,432 |
| `uiPageBody__from5` | — | 2,384 |
| `uiPageBody__from16`, `__from7` | — | 2,032 each |
| `uiPageBody__from3` | — | 1,872 |
| `uiPageBody__from19` | — | 1,792 |
| `uiPageBody__from4` | — | 1,744 |

The base column stops where the drain stopped: 672 bodies were sized before
the refusal, and `uiPageBody__from0` was never reached. With the first
change alone it is 23,424; with the second alone `chapters` is still 11,360,
because a literal's temporaries all live in one statement. Together: 1,822
bodies, the largest 3,072, `FRAME_LIMIT` unchanged. `chapters`'s remaining
2,240 is the argument temporaries of its `section(..)` calls: an aggregate
argument still travels as the caller's address and the callee copies it in,
so an argument keeps a slot of its own by design.

The three gate programs under wasmtime 46 (Cranelift), RFC-0104's recipe,
medians of five, the base binary and this one built from the same tree on
the same afternoon:

| program | base | this slice |
|---|---|---|
| nbody, 25 M steps | 1.99 s | 2.26 s |
| spectral-norm, n = 5500 | 2.90 s | 2.99 s |
| fannkuch, n = 11 | 3.76 s | 3.62 s |

`advance` is byte for byte the same function in both modules (795 lines of
wat, three copies), so nbody's spread is the machine and not the lowering:
re-timed with the two binaries interleaved, five each, the medians are 2.11 s
and 2.13 s. The three modules are 76, 87 and 132 bytes smaller. The release route (wasm2c and
clang) was not re-run: the WASI host the earlier measurement used is not in
the repository.

**The site export: compiled, and stopped by a defect this slice did not
make.** `vyrn run --engine wasm site/export.vyrn out` (from the repository
root, with `out/` and its six subdirectories present, `site/data/history.json`
and `demo.json` generated) compiles every body under the limit and starts
running. On the machine of §1.4, generator cache warm:

| | `run site/export.vyrn out` | files written |
|---|---|---|
| interpreter | 137.5 s | 247 |
| `--engine wasm` | trapped after 5.6 s (compile and load inside) | 0 |

The trap is `out of bounds memory access` inside `malloc`, reading a free
block whose first word had become string bytes. A `name` section
(`VYRN_WASM_NAMES=1`, new in this slice, written by `Module::finish` and
renumbered with the calls in `prune`) named the path — `route`, `uiTry13`,
`uiRender13`, `headTitle__from14`, `uiPgHead__from13`, `pageHeadOf`,
`pageHeadWith` — and a check added to `free` for the measurement found the
double free in `pageHeadWith`. The program below reproduces it with nothing
from the site. It prints `1` under the interpreter and natively, and traps
in `free` under `--engine wasm` at the base commit as at this one, with every
in-place path switched off:

    type M = { name: String }
    type H = { title: Option<String>, meta: Array<M> }
    fn empty() -> H { return H { title: None, meta: [] } }
    fn withT(h: H, t: String) -> H {
        return H { title: Some(t.copy()), meta: h.meta.copy() }
    }
    fn withM(h: H, name: String) -> H {
        let mut mt = h.meta
        mt.push(M { name: name.copy() })
        return H { title: h.title.copy(), meta: mt.copy() }
    }
    fn main() -> Int64 {
        let titled = withT(empty(), "T")
        let sized = withM(titled, "v")
        print(sized.meta.length)
        return 0
    }

`let mut mt = h.meta` takes the array out of a `read` parameter's copy, and
the caller's `titled` still owns the same buffer: two releases of one
buffer. That is the placement, not this slice's lowering, and it is the
class of defect M5 recorded as being fixed on its own branch
(`placeorder.vyrn`'s alias write). So the export's byte comparison against
the interpreter is not made here: the compiled route reaches the first route
and stops. The interpreter's 247 files are the target, on record above.

Gates run before the commit: `cargo fmt --all --check`; `cargo build
--release -p vyrn-cli`; `cargo test --workspace` (one test moved with the
lowering: `limits.rs`'s frame refusal needs a third 4 KB binding now that a
`let` of a call costs one record, not two); the kernel, lowered, fixtures,
fieldstore (two new tests: a nested literal costs the frame of its outermost
value, 48 bytes not 144; three call results in three statements cost one
slot, not three), places, simd, wasmabi, wasmio, traps and bytesink suites;
parity in release with `--ignored`; the residue ratchet; the cross-engine
generator test, red for the same five programs as at the base; and `vyrn doc
--std --verify`. The wasm manifest is not regenerated in this slice.

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

**The second slice lowered every construct (2026-09-02).** The 1,229
unlowered instances were thirteen constructs, and each is now a rule the
core states. What each became, and why:

- **A read of module state that owns heap** (719): a borrow of
  `Place::Global`. Nothing may take module state (RFC-0013): `movecheck`
  refuses returning it or passing it to a `consume` parameter, and `own`
  notes `let x = g` as a borrow. A store into a field or an element of a
  global is a store into that place, with the plan's row saying what it
  displaces.
- **A `consume` hole** (177) and **a `consume` of a place that is not a
  name** (31): `Rhs::Take(place)`. The value moves out into an owned name and
  the base keeps a hole at that path. The kernel tracks holes per name: a
  read or take that overlaps a hole is refused, a store at the hole fills
  it, a drop of the name releases the rest (the plan's walk minus its hole
  set), and two edges of a join must agree on the holes as on the names.
  An element hole is `[]`, any index. A binding whose hole the walk cannot
  skip (`Leak::Hole`) is not modelled: the plan leaks it on purpose, and
  none is in the corpus.
- **A move out of a field or an element** (155): three things. The receiver
  of a rebuilding builtin on a field (`s.dense.push(i)`, which the parser
  writes back as `s.dense = @push(s.dense, i)`) is a take whose store fills
  the hole. A `for` over a field borrows the container. Every other place
  read in a take position — `best = m.name`, an `if` arm that yields
  `parts[0]` — is an alias, because `movecheck::names_a_place` says so at
  the `let` and at the store and refuses every take that would own it.
- **A map lookup as a place** (7): `Place::Key`. A store into it takes the
  key — the map keeps the key it is handed, or releases the surplus one
  (`examples/mapkeyowned.vyrn`) — and a read borrows it. The seventeen
  refusals that appeared the moment map lookups lowered were all this rule.
- **A lambda** (28): reads of its captures. A capture is by read and a
  stored closure snapshots what it captured (RFC-0037), so the enclosing
  frame keeps its value. In an argument position the literal is
  monomorphized away and owns nothing. The lambda's own body is a separate
  frame and is not judged yet.
- **A `region`** (7): an ordinary block. The arena owns what is allocated
  inside; the plan notes each such binding `Leak::Region`, and a `drop` of
  one is what both compiling backends emit for it: nothing.
- **A call this slice cannot attribute** (35): reserved names with no
  prelude row (`fromJson`, `value`, `lex`, `render`, a log level) take the
  prelude's capability where it has one and `read` elsewhere; a projection
  an `impl` declares takes its own parameters' capabilities and yields a
  borrow; a name that is a function, a type or a contract used as a value is
  static.
- **An expression the checker did not type** (33): the same names as
  values, and a call to a projection the checker expands at the site
  (`people.tryAt(h)`), typed as the impl declares it under the receiver's
  type arguments. **An element of a non-container** (26) is a user
  container's `nth` or `at` projection, typed the same way. **An `Err`
  pattern on a non-result** (1) is `?` on a declared `Fallible` type: the
  failing path returns the whole value, the succeeding one hands it to the
  impl's `success`.
- **A `for` over a stream** closes the stream at the loop's end and at every
  `return` or `?` inside it — the direct backend's cursor stack, stated once.
  **A `let mut s = ""`** is `Static` in the kernel until a store gives it a
  value: a store over static data releases nothing, a drop of it frees
  nothing, and a loop whose body replaces it is judged again from the state
  the first turn leaves. The same holds for a literal built from literals
  (`[]`, `Body { nodes: [] }`).
- **The one misread.** `smallarray.vyrn`'s `let out = xs.toArray()` was
  refused because the plan's note says "the type unknown owns no heap" — it
  could not type the call — and the lowering read the note as "not owned".
  A note that could not type its binding now says nothing, and the checker's
  type decides.

| kernel over the corpus | first slice | with the placer | every construct |
|---|---|---|---|
| accepted | 5,292 | 5,344 | 6,581 |
| refused | 53 | 1 | 9 |
| unlowered | 1,229 | 1,229 | 0 |

**The nine refusals are five classes, all leaks the plan cannot express,
each measured by a probe under `rfcs/probes-0125/`** (peak working set at
200,000 and 400,000 turns, native build; a flat program measures 4.2 MB at
both):

5. **A payload binder with a hole.** `std/graphql`'s `reply`: `match consume
   r { Some(res) => consume res.body, .. }`. The arm table frees a binder
   whole or not at all, so the rest of `res` — its headers map — is never
   released. `payload-binder-with-a-hole.vyrn`: 53 MB and 102 MB. The
   placer must not place an arm row for a holed binder, because the
   emitters' arm free walks the whole binder (`direct.rs`, round forty), and
   the row would free what `consume res.body` handed to the caller. The
   kernel's placement mode leaves such a binder held, and the judgment
   refuses it.
6. **A field taken out of a temporary.** `gqlTestProject`'s `let sels =
   gqlParseQuery(query).sels`: the binding takes the field (`movecheck`:
   "the binding takes ownership of the extracted buffer") and nothing
   releases the rest of the temporary. `field-out-of-a-temporary.vyrn`:
   35 MB and 66 MB.
7. **A `consume` of a sub-place on one edge of a join.** `vlog`'s
   `recordsFrom` (`if d.ok { .. consume d.line .. } else { .. }`) and
   `std/rpc`'s `rpcApplyConfig`: the plan's hole set is per binding, not
   per path, so the release walk skips `line` on the path that never took
   it. `consume-on-one-edge.vyrn`: 12 MB and 20 MB. The placer saw the same
   shape in four `std/vyx` bodies (`vyxEmitAttrs`, `vyxEmitComp`,
   `vyxMergeImports`, `vyxProcessElemInner`) while loading a generator
   module; those programs do not load in the corpus test and are not
   counted.
8. **A `for` variable one of whose fields the body takes.** `std/tw`'s
   `twSafelist`: `for p in pairs { out.push(consume p.value) }`. The plan's
   handover row (`FreeArr`) frees the buffer alone because every take came
   through the variable, and `p.token` leaks per element — the direction
   the analysis says it is allowed to be wrong in. `for-variable-with-a-hole.vyrn`:
   66 MB and 127 MB.
9. **A binding returned whole on one path and drained on the other.**
   `gqlListValue`'s `let v = gqlValue(..); if v.err != "" { return v };
   items.push(consume v.value)`, and the same shape in `gqlObjectValue` and
   `gqlSelSet`. The plan's fate is "moved into the return", so it places no
   release anywhere, and on the other path `v.err` is held at the block's
   end. In these three the untaken field is an empty String and costs no
   bytes; `moved-on-one-path-holed-on-the-other.vyrn` gives it a value:
   20 MB and 35 MB.

**What the placer may not place, found by the gates.** The first cut placed
an exit row for every held name, holed or not, and `graphql` died natively
of a use after free while `jsonplace` double-freed: parity and the residue
ratchet both caught it. The emitters' release walk skips only the holes the
plan's own table lists for a binding, and a binding the plan never meant to
release (class 9's "moved" fate) has no entry there, so a placed row walked
`inner.sels` after `subs` had taken it. The placer now places no row of any
kind — exit, edge or arm — for a name that has a hole where the row would
run; placement goes on past it as if it were released, so the body's other
rows still land, and the judgment refuses the name. The second find was the
same fallback in the other direction: a note that could not type its
binding now defers to the checker's type (the `smallarray` fix), and a
projection's result is typed but borrowed, so `let items = doc.field("items")`
must stay a borrow whatever the note says; a lending call binds a borrow
before the note is consulted.

Each closes by fixing the plan: a hole set per path (7), a hole set on a
binder, on a temporary and on a moved binding (5, 6, 9), and an element
release minus the holes (8) — or, per M3, by the core placing what the plan
cannot say, once the emitters' walk can be told the kernel's holes. The
corpus test's ratchet is 9, for these and no others.

**Strict mode.** With `VYRN_KERNEL_STRICT=1`, a hard refusal by the kernel
— a double free, a use after release, a join whose edges disagree; not a
missing release, which the placer repairs — fails `vyrn check` and `vyrn
build` with the kernel's message printed as a diagnostic. The placer
collects the refusals it meets (`core::augment`), and the CLI runs the
analysis once more on the program it was given and drains them, so a
generator's refusal is not charged to the program that ran it. Off by
default while the five classes above stand in the corpus.

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

**The second slice placed the holed names (2026-09-02).** M2's nine
refusals were names the placer could not place because a placed row walked
the whole value minus the binding's own hole set, and the kernel's hole set
at an exit is per path. The plan's tables now carry the set per row, and the
placer fills them:

- A placed release row (`own::Release`) has `holes: Option<Vec<String>>`,
  the set THIS row walks around. `placed` hands each engine `(binding,
  Option<holes>)`; round fifty-two's `full` is the empty set. Both compiled
  backends park the binding's own set around the one emit (`direct.rs`
  `emit_releases`, `lib.rs`'s release step). The core reads such a row as
  `St::Row`, and the kernel checks its set against the state: a row that
  walks a place a take left is a double free; a row that skips a place
  still held is a leak, and in placement mode the row takes the state's set.
  That check is what found class 7's probe, where the analysis had placed
  `drop d` at both returns with the binding's set and the kernel had trusted
  it.
- An arm row is `(binder, kind, holes)`; the placer writes the binder's
  holes and each engine skips them (class 5).
- An edge row's name may be a sub-place, `d.line`: where two held edges of
  a join disagree on a hole, the edge that did not take releases the
  sub-place and holds the hole afterwards — Rule N one level down. The core
  lowers it as a take into a temporary that is dropped at once, so the
  judgment sees the hole; each compiled backend resolves the path to the
  field's address inside the binding (class 7 at a live join).
- A `for` variable is keyed by the address of its spelling in the
  statement, since it has no `let`; the core binds it inside the body's
  block, and each compiled backend registers a slot for it when the plan's
  droppable table names the key. Its rows release the rest of the element
  at every exit of the body; the handover row still frees the buffer alone
  (class 8).
- The receiver a read TOOK a heap field out of (`let sels = parse(q).sels`)
  goes into R1′'s table with `receiver_holes`; each compiled backend frees
  it around the field right after the read (class 6). Both had run R1′'s row
  after a scalar field read only, so the analysis's own row for a heap
  field stood for nothing, and the core no longer reads it as a `drop`.
- Class 9 needed nothing beyond the first item: a "moved" binding held with
  a hole at a block's end gets an exit row with that hole.
- The direct backend's `bind_payload` now frees a consumed scrutinee's
  payload box under the textual backend's `free_boxes` rule (`frees_boxes`),
  less its map-lookup clause: telling a map lookup from an element read
  needs the receiver's type, and `peek` on the receiver before the arms is
  not free of effect in this backend. A lookup's box stays the leak it was
  here. The rest of the rule is what the payload-binder probe measured under
  wasmtime once everything else was flat: that box was "the safe leak every
  boxed enum payload already is" in this backend.

**What the cross-engine generator gate caught in this slice, and neither
gate in the brief could.** `genwasm`'s
`every_generator_example_emits_the_same_source_under_both_engines` runs the
`std/vyx`, `std/html` and `std/ui` generators as compiled wasm and compares
their output with the interpreter's; parity and the residue ratchet never
execute those bodies. The gate is part of this slice's list from here on.

The root cause was in the kernel's model, not in an emitter: the core
lowered a `for` as a loop with no exit — only a `while` got the `if .. else
break` at its top — and a loop nothing leaves ends the path. Everything
after a `for` in its block was never judged, and at every join above it the
edge holding the `for` was dead. That was silent while the placer only
added rows. The rewrite of a row's hole set reads the kernel's state at the
exit, and in `vyxMergeImports` that state came from the one live edge of the
`if imp.ns` join (holes `alias`, `spec`) while the dead edge had taken
`imp.names`; the rewritten row walked `names`, and the generator freed it
twice. A `for` has the same exit a `while` has now, and the corpus tally
moved: the kernel judges the rest of every such block, `gqlAnswer`'s
if-expression join (which the core had given no site, though the plan keys
edge rows by the expression) is placed, and three temporaries read off a
call result after a loop came into view.

Two smaller defects went with it. The `for` variable was keyed by the
address of its `var` string, which is the first field of `Stmt::ForIn` — at
offset 0 under a niche-encoded discriminant, so it equals the statement's
own address, the container row's key; each backend then registered the
variable's slot over the container's. The key is the spelling's heap buffer
now (`own::for_var_key`), stated once. And `frees_boxes` lost its map-lookup
clause: it was removed on a bisect result that the generator artifact cache
had contaminated (artifacts are keyed by the binary's identity, not by the
environment, so a knob run after a clean run loads the clean run's module),
and it stays out because `peek` on a receiver before the arms is a path this
backend has not run before, while the cost is the lookup box leaking as it
did before this slice.

| kernel over the corpus | every construct | holed names placed |
|---|---|---|
| accepted | 6,581 | 6,587 |
| refused | 9 | 3 |
| unlowered | 0 | 0 |

The probes, peak working set at 200,000 and 400,000 turns. The native
"before" column is M2's record; the wasm one is this build with
`VYRN_NO_PLACER=1`. The "after" cells poll every millisecond and take the
larger of three runs. Under wasmtime every cell reads 12.7 or 14.2 MB
whatever the turn count — the engine's own footprint, which steps between
those two levels from run to run — so the native column is the tight
measurement and the wasm column shows the absence of growth.

| probe | native before | native after | wasm, placer off | wasm after |
|---|---|---|---|---|
| payload-binder-with-a-hole | 53 / 102 MB | 3.8 / 3.8 MB | 55.5 / 115.9 MB | 14.2 / 12.7 MB |
| field-out-of-a-temporary | 35 / 66 MB | 3.8 / 3.9 MB | 30.6 / 67.2 MB | 12.7 / 14.2 MB |
| consume-on-one-edge | 12 / 20 MB | 3.8 / 3.8 MB | 21.0 / 20.4 MB | 14.2 / 14.2 MB |
| for-variable-with-a-hole | 66 / 127 MB | 3.8 / 3.8 MB | 63.3 / 119.1 MB | 12.7 / 14.1 MB |
| moved-on-one-path-holed-on-the-other | 20 / 35 MB | 3.8 / 3.8 MB | 32.3 / 40.9 MB | 12.8 / 14.2 MB |

`consume-on-one-edge-live-join.vyrn` is the same shape with the binding read
after the join, which is the corpus's shape (`recordsFrom`, `rpcApplyConfig`)
and the one the sub-place edge row exists for: 4.2 / 4.1 MB natively and
12.6 / 12.7 MB under wasmtime.

**The refusals left are one class, a leak the corrected lowering
uncovered.** `std/graphql`'s `gqlIsRecord` returns
`gqlSplitDecl(src).rhs.startsWith("{")`: a heap field READ off a temporary
nobody names, in a read position. The analysis puts the receiver in R1′'s
table, both compiled backends run that row after a scalar read only, and
the borrowed field outlives the read, so no table keys the free that is
owed after the read's consumer. `arrays.vyrn`'s `weekdayLetters()[1]` and
`slots.vyrn`'s `(people.get(bob) ?? Person { .. }).name` are the same shape
one construct over, an element read and a field read off a call result.
`rfcs/probes-0125/field-read-off-a-temporary.vyrn`: 25.9 MB and 58.7 MB
natively, 32.6 MB and 76 MB under wasmtime, the whole temporary per turn.
The corpus test's ratchet is 3, for these and no other. The class closes
with an argument-temporary drop of the receiver after the read's consumer,
which is the shape `arg_drops` has for the argument itself and not for what
the argument was read out of.

**The third slice closed it (2026-09-02).** The row is the argument-temporary
drop the analysis already has, keyed one node down: not the read
(`gqlSplitDecl(src).rhs`), whose value is the borrowed field, but the node
that PRODUCED the receiver — `gqlSplitDecl(src)`, `weekdayLetters()`, the
`match` a `??` spells. Both compiled backends already tee the value of any
node in `arg_drops` where they evaluate it and free it after the call or
operator that encloses it (`direct.rs` `expr` and `call`, `lib.rs` `gen_expr`
and `gen_call`), which is after the read's consumer; the core drops the
receiver at the same point, by queueing it with the String temporaries the
consumer's binding releases. The placer writes the row where the kernel finds
such a receiver held and a call or an operator encloses the read
(`NameInfo::producer`); a read nothing drains encloses — `for p in
f().items`, `let x = f().a.b` — gets no row and stays refused, and none is in
the corpus. One emitter rule came with it: a lending call (`a[i]`, a
projection) whose result owns heap must not drain its arguments' temporaries,
because its result points into one of them; the call or operator above
drains them. Without the rule `print(weekdayLetters()[1])` would free the
array after `[1]` and print out of it.

| kernel over the corpus | holed names placed | the receiver placed |
|---|---|---|
| accepted | 8,595 | 8,598 |
| refused | 3 | 0 |

(The corpus is 166 programs at this branch's base, so the accepted count is
not the 6,587 of the second slice's record; the refused count is.) The
probe, peak working set at 200,000 and 400,000 turns, measured as the table
above; the "placer off" columns are this build with `VYRN_NO_PLACER=1`, and
a flat probe reads 18.2 / 14.9 MB under the pinned wasmtime on this host:

| probe | native, placer off | native after | wasm, placer off | wasm after |
|---|---|---|---|---|
| field-read-off-a-temporary | 36.7 / 68.8 MB | 4.4 / 4.4 MB | 43.7 / 72.6 MB | 14.9 / 14.8 MB |

The corpus test's ratchet is 0.

**Lambda frames are judged (2026-09-02, the third slice).** The reason
they were not was in the emitters, and it was one change in each.
`direct.rs` names a lifted lambda's shell `@lambda <owner>` and `lower_body`
reads `Cx::droppable` and `Cx::releases` under the owner; `lib.rs` keeps
`placed` and `droppable` across the lift and takes only the slots away. The
analysis had always recorded a lambda's rows under the enclosing function's
name, keyed by the lambda's own nodes, so both backends now run what it
wrote and what the placer adds. The core builds each lambda literal as a
frame of its own (`Body::lambdas`, `Builder::lambda_frame`): its captures are
the enclosing names spelled again as borrowed inputs, its parameters are
`read` (RFC-0023), its bindings are ordinary, and an expression body is a
`return` of its value at no site. The kernel judges every frame and the
placer places in every frame; the corpus test counts each as an instance.
`rfcs/probes-0125/lambda-holds-on-one-path.vyrn` is the witness: a lambda
that binds a String, hands it to a `consume` parameter on every third turn
and returns without it on the others — the analysis's fate is "moved", so it
places no release on the untaken path, and before this slice no row inside a
lambda ran at all. Peak working set at 200,000 and 400,000 turns, the
"placer off" column being this build with `VYRN_NO_PLACER=1` (the emitter
change alone):

| probe | native before | native, placer off | native after | wasm before | wasm after |
|---|---|---|---|---|---|
| lambda-holds-on-one-path | 15.2 / 25.9 MB | 15.6 / 26.2 MB | 4.8 / 4.8 MB | 32.1 / 41.7 MB | 22.2 / 22.0 MB |

| kernel over the corpus | the receiver placed | lambda frames judged |
|---|---|---|
| accepted | 8,598 | 8,653 |
| refused | 0 | 0 |
| unlowered | 0 | 0 |

Two things the frames taught. A lambda's type is often an alias
(`Transform`, `Middleware`) or a `lazy` field's, so the frame resolves it
before it reads the parameters; and a literal in the argument position of a
generic has no type of its own — the instance monomorphized it away — while
its body is typed, so each parameter takes the type of its first use there.
And `mentions_place` does not see a callee: `n -> f(n) + 1` captures the
function value `f`, which the frame now counts (`mentions_in_lambda`), or
the call could not be attributed. Every frame in the corpus is accepted
with the placer on, so the rows the placer adds inside lambdas are the
whole difference between the two probe columns above.

**Wordings (2026-09-02, the third slice).** A hard refusal by the kernel
under `VYRN_KERNEL_STRICT=1` prints as the checker's move diagnostics print:
`file:line:0: message`, the message in `movecheck.rs`'s voice. The kernel
now keeps, per consumed name, the line and the taker in the checker's words
("the binding `t`", "`take(..)`", "`consume`", "a `return`"), and the core's
taking statements carry their source line, which is what the wording needs
and the judgment did not. The comparison was made on five small programs
under `VYRN_NO_MOVECHECK=1` — a knob added for exactly this, since the
checker refuses each before the kernel is reached:

| program | the checker (`vyrn check`) | the kernel (`VYRN_NO_MOVECHECK=1 VYRN_KERNEL_STRICT=1`) |
|---|---|---|
| `let t = s` then `print(s)` | w1.vyrn:3:0: `s` was moved here into the binding `t` / line 4: ... and `s` is used again here / fix: `s.copy()` if both sides need a value | the same two lines, no fix |
| `take(s)` twice, `take(v: consume String)` | w2.vyrn:8:0: `s` is used here but was already consumed by `take(..)` on line 7 / (a `consume` parameter takes ownership; the value can't be used afterward) | the same two lines |
| `take(s)` inside a `while` | w3.vyrn:10:0: `s` is consumed by `take(..)` inside a loop, so it would be used again on the next iteration | the same line |
| `take(consume p.name)` then `print(p.name)` | w4.vyrn:9:0: `p.name` was moved here into `consume` / line 10: ... and `p.name` is used again here / fix: `p.name.copy()` if both sides need a value | the same two lines, no fix |
| `take(consume p.name)` then `keep(p)` | w5.vyrn:13:0: `p.name` was taken out of `p` here / line 14: ... and `p` is used as a whole here, with the hole still in it / two fix lines | the same two lines, no fix |

Where the two differ the kernel's is the same sentence with the `fix:` menu
missing: the menu names `.copy()` and write-back as ways out, which are the
checker's knowledge of the surface and not the kernel's. The join refusal
has no checker equivalent (the checker accepts a conditional move and the
plan's Rule N releases the other edge; the kernel says so only when the
placer is off), so its sentence is the kernel's own.

**What the analysis answers that the kernel does not, recorded for the
deletion track.** Neither `movecheck.rs` nor `own.rs` is deleted here. The
answers below have no kernel equivalent today; each is a rule the kernel or
the core must state before its source can go.

From `movecheck.rs`:

- Rule 2, borrows: a `read` or `modify` parameter, a place read
  (`names_a_place`), a loop variable or a projection's result may not be
  returned, stored, or handed to a `consume` parameter without `.copy()`.
  The core binds such a name as not owned, and the kernel does not refuse a
  take of a name it does not own.
- Module state may not be taken (RFC-0013): the core reads a global as a
  borrow and says nothing about a `consume` of it.
- A `region`'s escape rule (RFC-0004 §4): a value the arena allocated may
  not leave the region. The core lowers a `region` as an ordinary block.
- A capture's rules: a moved name may not be captured, and a closure that
  captures a borrowed parameter may not escape (`a2_capture_escape`). The
  frame reads its captures as borrowed inputs and judges nothing about
  their lifetime.
- `consume` with nothing to take, and the `for .. in consume` forms.
- The `fix:` menus and `vyrn fix`.
- An exported function returning a borrow.
- The argument verdicts over the call graph (`ArgVerdict`, `note_handover`):
  whether a callee keeps what it is handed, which is what `arg_drops` is
  built from. The kernel judges the row; it does not derive it.
- `sinks`: a rebuilding builtin takes its receiver, and the write-back
  statement excepted. The core restates the first half (`call`, `rebuilds`)
  and reads the plan for the second.

From `own.rs`:

- Every per-node table the core reads and does not derive: `arg_drops`,
  `store_owned` and `store_fresh` with `mentions_place`, `discarded_results`,
  `consuming_matches`, `malloc_scrutinees` and `receiver_malloc` (the region
  stand-downs), `receiver_frees` for a scalar read, the `for` handover
  (`DropKind::FreeArr`), the binding notes (`Fate`, and `vyrn why --memory`).
- The release kind of a type (`DropKind`) and a declared `release`'s
  ordering: the emitters read kinds from the plan; the kernel asks only
  whether a type owns heap.
- `Leak::Hole` (a hole the walk cannot skip) and `Leak::Region`: not
  modelled; a binding under the first is a gap.
- An edge row's hole set, and an element hole (`.[]`): no row.
- A lambda expression body's exit: a name still held at it has no site an
  engine runs, so it is refused, not placed.



**Lambda bodies are not judged, and the reason is in the emitters.** Both
compiled backends lift a lambda under a shell that owns no rows: `direct.rs`
`f_shell` names it `@lambda`, so `Cx::droppable` and the placed steps answer
nothing for it, and `lib.rs` takes `drop_slots` away for the lifted body.
The analysis records rows inside a lambda under the enclosing function's
name, keyed by the lambda's blocks, and no engine looks them up there
(`vyrn_lower::Lowered::lambda_bodies` records the same finding: zero steps
placed inside a lambda across the corpus). A row the placer put there would
be placed and not run, which is the caveat the arm table had. So the kernel
does not judge lambda bodies until the emitters' lambda lowering reads the
plan's rows under a name the plan writes them under; that is one change in
each backend and the placer needs none.

**Not done, recorded.** An edge row still carries no hole set, so a name
holed on a held edge while another edge took it whole gets no row and is
left to the judgment; none is in the corpus. An element hole (`.[]`) has no
row either, because no walk skips inside an element. The kernel trusts the
hole set the plan's own table gives a binding; only a placed row's set is
checked against the state.

### M4 — the runtime in Vyrn

The runtime module of §2.4, compiled by the emitter into every program. The
hand-emitted runtime in `direct.rs` and the logic in the C shim are deleted;
the shim becomes the WASI host.

**Gate:** parity green, the free audit and poison deleted, binary-trees under
the native route at or below its wasmtime time from §1.4.

*Steps 0 and 1 of `PLAN-0125-runtime.md` §6 landed 2026-09-02 (`track-e`):
`std/mem` fenced by an audience the compiler declares, `std/runtime` linked into
every program, and the ten pure string functions (`strLen`, `strCmp`, `starts`,
`intStr`, `parseI64`, `strI64`, `utf8Valid`, `lineAt`, `colAt`, `regexRun`)
written in Vyrn with their 692 lines of hand-emitted wasm deleted from
`direct.rs`. Parity 41 of 41, residue green. fasta and reverse-complement under
wasmtime: 0.89 s and 1.07 s against §1.5b's 0.93 s and 1.06 s, on a machine
shared with another parity job; the native route is unchanged until step 3. The
numbers and the gates are recorded under the plan's §6 table.*

*Step 2 landed 2026-09-02 (`track-g`): `malloc` and `free` are Vyrn in
`std/runtime` — the 113-class segregated free list of the plan's §4, its heads
and bump offset in the heap's first 480 bytes — and the wasm emitter's 286
lines of allocator and its `HEAP` global are deleted. binary-trees at depth 18
under wasmtime: 0.94 s against the base's 0.95 s, medians of five, base and
head interleaved on a shared machine. The
audit and poison are the C shim's and go with it at step 3; the wasm copy never
had them. Details under the plan's §6 table.*

*Step 3, first slice, landed 2026-09-02 (`track-j`): the release route of §2.5
is a build flag, `vyrn build --route wasm2c`, beside the text-IR route and not
in its place. The same wasm `--target wasm` writes goes through wasm2c (wabt
1.0.41) to C and through clang at the native route's own flags, with a
574-line C WASI host (`vyrn-codegen/src/wasi_host.c`) that does what the
embedded engine's host does, import for import. wasm2c and simde are
discovered and recorded, never pinned, the way clang is (`$VYRN_WASM2C`,
`$VYRN_SIMDE`, else `tools/`; `vyrn deps` prints both). `tests/route.rs` holds
the route to the engine: 171 corpus programs byte-identical on stdout, stderr
and exit code against the `wasmtime` CLI, 33 skipped for the parity loop's
reasons. Nothing is deleted: the shim and the text-IR route stay until the
numbers below decide the route. Seven programs at RFC-0104's timing sizes,
medians of five, one machine, the three routes interleaved by the harness
(`run.py --contestants vyrn-native,vyrn-wasm,vyrn-wasm2c`):*

| program | native (text-IR, LLVM) | wasmtime 46 | wasm2c + clang | wasm2c ÷ native |
|---|---|---|---|---|
| nbody, 25 M steps | 0.95 s | 2.23 s | 1.54 s | 1.6x |
| spectral-norm, n = 5500 | 1.06 s | 4.13 s | 2.31 s | 2.2x |
| fannkuch, n = 11 | 2.09 s | 3.98 s | 3.93 s | 1.9x |
| binary-trees, depth 18 | 2.14 s | 1.05 s | 1.07 s | 0.5x |
| fasta, n = 5 M | 0.80 s | 0.91 s | 0.76 s | 0.9x |
| reverse-complement, 40 M bases | 0.38 s | 1.05 s | 0.97 s | 2.6x |
| k-nucleotide, 4 M bases | 0.13 s | 0.29 s | 0.20 s | 1.5x |

*Build time of nbody: native 0.63 s, wasm 0.03 s, wasm2c 1.47 s (wasm2c
itself is under 0.1 s; the rest is clang over a 150 KB C file). The three
numeric kernels sit where M1 measured them, 1.6x, 2.2x and 1.9x against 1.5x,
1.9x and 1.8x, on a run whose wasmtime column was slower than M1's too.
binary-trees under the route is at its wasmtime time, which is M4's gate for
the native route once the route is this one, and fasta is under native.
reverse-complement is the widest gap and the one not yet explained: the route
runs the wasm's runtime, so the answer is in step 4's string family, not in
the route. The full record is under the plan's §6 table.*

*Step 4 landed 2026-09-02 (`track-l`): the allocating strings — `str_new`,
`concat`, `str_append`, `str_from_bytes` — are Vyrn in `std/runtime`
(`strNew`, `strConcat`, `strAppend`, `strFromBytes`) over the step-2 allocator
and `std/mem`'s `copy`, with the same growth policy and the same two failure
wordings, and the wasm emitter's 342 lines for them are deleted. The three
`STRING_RUNTIME` accessors are the native IR's and have no wasm twin; they
leave at step 3. Parity 41 of 41, residue green. Under wasmtime, base and head
interleaved, medians of five: the census's append builder 0.106 s against
0.107 s, fasta 0.845 s against 0.847 s, reverse-complement 0.989 s against
0.990 s. Every string function the wasm route runs is now Vyrn, so
reverse-complement's 2.6x against native (step 3's table) is not in the
runtime's transcription; it is in the program's own loops as the emitter
lowers them. Details under the plan's §6 table.*

*Step 5 landed 2026-09-02 (`track-m`): the maps — the String, Int64 and
packed-key chains of RFC-0028 and RFC-0117, `tallyBytes`'s byte-window probe
of RFC-0116, and the reserve, remove and keys operations that were inline —
are one body in `std/runtime` (`mapFind`, `mapPut`, `mapReindex`,
`mapReserve`, `mapRemoveAt`, `mapKeysCopy`) over the step-2 allocator and
`std/mem`, the layout a pair of constants the emitter passes at each call, and
the wasm emitter's fourteen hand-emitted map functions and three inline copies
are deleted: 1,194 lines out of `direct.rs`, 272 in the module. Parity 41 of
41, residue green. k-nucleotide under wasmtime, base and head interleaved,
medians of five: 0.297 s against 0.284 s, 5 percent, which is the layout's
two compares per probe, and 0.297 s against the 0.29 s step 3's table holds for
the row. On the way there the plan's §7.3 assumption failed on the record: a
function level between the caller and the probe loop cost one call per probe,
20 percent on this row, and wasmtime 46 took none of it back, so the module
keeps the hand-emitted copy's two levels. `std/hash`'s `fnv1a` stays a
separate function of the same arithmetic (the plan's §8 question 5, closed).
Details under the plan's §6 table.*

*Step 6 landed 2026-09-02 (`track-p`): the arrays — `push`, `reserve`,
`append`, `copyFrom` and `clear` — are runtime functions for the first time in
any engine, Vyrn in `std/runtime` (`arrPush`, `arrReserve`, `arrAppend`,
`arrCopyFrom`, `arrClear`) over the step-2 allocator and `std/mem`'s `copy`,
told the element stride by the emitter the way the maps are told a layout;
the wasm emitter's five inline copies are deleted, 388 lines out of
`direct.rs`, 136 in the module. `at` stays inline, and the number that keeps
it there is on the record: with the check and the address behind one module
call, nbody went from 1.95 s to 7.28 s under wasmtime 46, which is the per-call
cost step 5 found, on the per-element path. Parity 41 of 41, residue green.
The five programs under wasmtime, base and head interleaved, medians of five:
nbody 1.974 s against 1.960 s, spectral-norm 2.845 s against 2.849 s,
fannkuch 3.413 s against 3.403 s (M1's 1.98 s, 2.83 s, 3.58 s), binary-trees
0.845 s against 0.833 s, k-nucleotide 0.298 s against 0.289 s. Details under
the plan's §6 table.*

### M5 — `vyrn run` is compiled

`run`, `test` and `bench --check` execute the wasm in the embedded wasmtime.
The interpreter is deleted. The parity job is replaced by the cross-platform
hash of §2.6 plus the fixture comparison.

**Gate:** every fixture's output identical to the recorded expected output;
every fixture's wasm byte-identical across the matrix; site export time
recorded against its interpreter baseline.

**The first slice landed (2026-09-02): the compiled route exists beside the
interpreter, and the fixture gate exists.** `vyrn run`, `vyrn test` and
`vyrn bench --check` take `--engine interp|wasm`, before the file the way
`--profile` is. `wasm` compiles the program with the direct backend and runs
the module in a wasmtime the CLI now embeds; the default stays `interp`, the
interpreter is not deleted, and CI's required jobs are unchanged. The
`wasmtime` crate enters `vyrn-cli`'s default build (the same version and
features `vyrn-genwasm` uses), which is a Rust crate and leaves the
workspace's property — no LLVM, no clang, no sysroot — where it was. What
the embedding costs, on record: `vyrn.exe` (release) 9,973,248 to
20,988,928 bytes; a cold `cargo build --release -p vyrn-cli` 31.9 s to
122.6 s on the machine of §1.4, all of it the wasmtime and Cranelift tree.

The WASI host is hand-written in `compiler/vyrn-cli/src/wasmrun.rs`: the
fourteen `wasi_snapshot_preview1` imports `direct.rs` declares and no other
(an unknown import traps, as the generator engine's does). It gives the guest
what the parity harness's `wasmtime run --dir . --env ..` line gives it:
argv, this process's environment, the three standard streams passed through,
and the working directory as the one preopened directory, with the CLI's
capability rule (no absolute path, no `..` past the root). `random_get` is
answered from the operating system's source, `/dev/urandom` or
`BCryptGenRandom`, as the CLI answers it. A trap the program did not spell —
a wasm `unreachable`, an out-of-bounds access — prints `error: <wasmtime's
trap text>` and exits 1, where the CLI prints `Error: failed to run main
module` around the same text; no fixture reaches one, since every trap a
Vyrn program can take is written by the program itself.

`test` and `bench --check` under `wasm` lift each body into a function and
synthesize a `main` that reads ONE line from standard input and calls the
body that line names; the host serves that line before the process's own
input, and runs one instance per body. A body's trap ends its instance with
`error: <message>` on fd 2 and exit 1, and the host turns the message into
the `FAILED: <message>` line the interpreter prints. Two test-only builtins
have no lowering in the direct backend, so the CLI rewrites them before it
compiles: `assert(c)` and `assertEq(a, b)` become `if` around `panic` with
the interpreter's wording (a call operand is bound to a fresh local first;
any other operand is written twice), and `blackBox(v)` becomes `v`, which is
what the interpreter runs and `--check` measures nothing. The lines that come
out are byte-identical to the interpreter's on the `testing.rs` corpus and on
all seventeen bench programs in `examples/`.

**The fixture gate.** `compiler/vyrn-cli/tests/fixtures.rs` runs every
top-level example with `vyrn run --engine wasm` under the corpus's
conventions (cwd `examples/`, the `.stdin` and `.args` fixtures, the fixed
clock and seed) and compares stdout, stderr and the exit code byte for byte
against `examples/expected/<name>.stdout|.stderr|.exit`, recorded once from
the interpreter with `VYRN_FIXTURES=write`. A refusal is compared like any
program — its output is the diagnostic, and both engines share the load that
prints it. 203 examples: 201 compared and identical on the first run, 2
skipped with a reason — `externdemo.vyrn` (host-only, no terminal supplies
its `extern` namespace) and `polyrecursion.vyrn`, the one program the
interpreter runs (it prints `0`) and the compiled route refuses. The
interpreter's 203 runs take 155 s in a debug build; the wasm engine's 201
take 32 s, compile included. A `fixtures` CI job runs the gate on the four
platforms of the matrix; it is not required by branch protection in this
slice. With `wasmhash` it is the shape §2.6 describes.

**What a user pays, both engines, medians of three, wall clock with the
compile in the wasm column.** `vyrn run` at the game's small inputs is
process start-up under either engine; the bench bodies are where the
interpreter's time goes.

| program | `run`, interp | `run --engine wasm` | of which compile | `bench --check`, interp | `bench --check --engine wasm` |
|---|---|---|---|---|---|
| nbody | 0.16 s | 0.03 s | 0.03 s | 7.99 s | 0.04 s |
| spectral-norm | 0.41 s | 0.03 s | 0.02 s | 10.53 s | 0.05 s |
| fannkuch | 0.11 s | 0.03 s | 0.02 s | 9.62 s | 0.05 s |
| binary-trees | 0.58 s | 0.03 s | 0.02 s | 1.17 s | 0.03 s |
| fasta | 0.05 s | 0.03 s | 0.02 s | 0.56 s | 0.03 s |
| reverse-complement | 0.03 s | 0.03 s | 0.02 s | 1.00 s | 0.06 s |
| k-nucleotide | 0.07 s | 0.04 s | 0.03 s | 0.73 s | 0.05 s |

The `bench --check` column is CI's "Bench --check" step, 45 to 59 s across
the fleet under the release interpreter (ci.yml's table); under the compiled
route the seven above sum to 0.31 s.

**Site export: not measured under the compiled route, and here is why.**
`vyrn run site/export.vyrn` under the interpreter, three runs on the machine
of §1.4 with the generator cache warm: 130 s, 151 s, 136 s (82 routes and 14
assets; `main`'s binary of 2026-08-29 gives 136 s on the same input, and
`VYRN_NO_PLACER=1` 138 s, so neither this slice nor M3 moved it). The 13.8 s
this milestone's gate cites from RFC-0124 does not reproduce here and is
left as a discrepancy to resolve, not a regression to claim. Under
`--engine wasm` the export is refused before it runs:

    error: `listDir` runs in the interpreter / at generation time (RFC-0021);
    it has no native or wasm lowering in v1 — use it in a `gen fn` or under
    `vyrn run`

That is the gap between this slice and §2.5's first row: the export walks
directories, and the direct backend has no `fd_readdir`. It is one lowering
and one host import, and it is the next thing M5 needs.

**The other gaps, each on record so nobody discovers it later:**

- `polyrecursion.vyrn`: `vyrn check` refuses it with `past the instantiation
  limit`; `vyrn build --target wasm` and `run --engine wasm` refuse it with
  `f needs 12288 bytes of stack for one call, past the frame limit` — the
  frame check trips before the instantiation check on this route. Two
  refusals for one program; `check` was meant to predict `build` (audit
  A5.2) and here predicts a different sentence.
- `test --engine wasm` initializes module state once per BODY, where the
  interpreter initializes it once per run and lets bodies see each other's
  writes. Input a body read ahead of its lines is not seen by the next body.
  A body that calls `main()` calls the harness's empty `main`. These are the
  semantics a fresh instance per test has; the interpreter's are the ones to
  retire when it goes.
- `assertEq`'s non-call operands are evaluated twice (once for the compare,
  once for the message); the interpreter evaluates every operand once. A
  side-effecting non-call operand does not exist in the language's surface,
  so nothing observes it, and the rewrite records it anyway.
- Standard input under `--engine wasm` is read by the host in one `read`
  per `fd_read`, as a syscall would; the CLI's host does the same.

Gates run before the commit: `cargo fmt --check`; `cargo test -p
vyrn-frontend`; the fixtures gate; the kernel, lowered, fieldstore, places,
simd, wasmabi, wasmio, traps and bytesink suites; parity in release with
`--ignored` (41 programs, three engines byte-identical); the residue ratchet;
and `VYRN_WASM_MANIFEST=check` on the wasm manifest, which passed unchanged
because no emitter changed. All green: 1,176 frontend tests, the fixture
gate (24 s), the nine suites, parity 41 passed in 209 s, the residue
ratchet, and the manifest.

**The second slice landed (2026-09-02): the five gaps above, closed or
priced.** Four commits on `track-h`, each green on the fixture gate before
the next.

- `listDir` and `listDirKinds` are lowered by the direct backend over WASI
  `fd_readdir`, the fifteenth preview1 import. The runtime function
  `readdir_blob` opens the directory through `open_at` with
  `oflags::directory` and `right::fd_readdir`, walks `fd_readdir`'s buffer
  (a cut last entry is re-read from its predecessor's cookie), drops `.` and
  `..`, and joins the names with `\n` — the encoding the generator host
  already answers `GEN_MODE_LIST` in, so `list_dir` splits one shape
  whichever host filled it, and sorts by `strCmp` on the WASI path, since the
  interpreter sorts. The `Err` is RFC-0014's `listerr` from the one table.
  The embedded host (`wasmrun.rs`) answers `fd_readdir` and opens a
  directory on `path_open`; the `wasmtime` CLI already did; `web/wasi-min.js`
  gains the stub so a page still links. The generator engine is unchanged: a
  `gen fn` lists through `vyrn_gen.read`, as before. `examples/listdir.vyrn`
  pins the three answers (a listing, a `listDirKinds` listing with its `/`,
  the `Err`) and 228 entries of `examples/` itself came out byte-identical
  from the interpreter, the embedded host and the `wasmtime` CLI. The
  text-IR backend still has no lowering and says so in its own sentence
  (`LIST_DIR_NO_LOWERING`, reworded); the example is on the parity
  harness's `NATIVE_UNSUPPORTED` list and the residue baseline's `skip` row.
- `assert`, `assertEq` and `blackBox` are lowered in `direct.rs` beside
  `panic`, and the CLI's pre-compile rewrite (`desugar_asserts` and the
  `blackBox` walk, 156 lines) is deleted. `assertEq` evaluates each operand
  once into a local, compares by the operand's type, and on a mismatch
  writes `error: assertion failed at line L: `, the two renderings `@str`
  uses, and ` != ` in pieces, the way `panic` writes; an operand that
  allocated is released by `Fn_::call` after the arm returns, as every call
  argument is. The double-evaluation gap above is closed with the rewrite.
  `std/num` is injected for a program that mentions `assertEq`, so a float
  mismatch renders. Over every example with `test` blocks (25 files) and
  every one with `bench` blocks (17), `vyrn test --engine wasm` and `vyrn
  bench --check --engine wasm` print the interpreter's lines byte for byte,
  with one exception that is not this slice's: `placeorder.vyrn`'s "a field
  write does not disturb an alias taken before it" fails under wasm with
  `99 != 1`, and the same body as a `main` prints `99` natively too, where
  the interpreter prints `1` — a placement defect of the compiled routes,
  on record here and being fixed on its own branch.
- `polyrecursion.vyrn` has one refusal. The direct backend's drain now
  defers a frame refusal until the worklist is empty, so the instantiation
  refusal — `check_inst_depth`'s sentence, the one `vyrn check` and the
  text-IR backend give — wins when it comes; a plain frame overflow is still
  refused after the drain, in the same words as before. And `vyrn run` calls
  `check_instantiations` before it runs, under either engine, so the
  interpreter no longer prints `0` for a program `check` refuses: one
  program, one sentence, from `check`, `run` and `build`. The cost is one
  `vyrn_lower::lower` per run — under 3 s on the site export, which is the
  whole of a warm `vyrn check` of it. The fixture is recorded from the
  interpreter like every other refusal, and `INTERP_ONLY` is gone from
  `fixtures.rs`: 203 compared, 1 skipped (host-only).
- **Site export under `--engine wasm`: refused at the frame limit, and the
  gap is priced.** With the `listDir` gap closed the export compiles until
  `chapters` in `site/app/guide.vyrn` — one literal of every chapter's
  record, with its sections' records nested in it — needs 11,360 bytes of
  frame against the 8,192 of `FRAME_LIMIT`. Raised to 12 KiB for one build
  (not committed), the next refusal is the generated `uiPageBody__from0` at
  25,984 bytes; raised further the shadow stack (`FRAME_LIMIT` × the 1,000
  call depth) no longer fits under the statics limit, so the constant is not
  the knob. The frame is §1.4's finding again: every nested aggregate
  temporary of a literal lands in the frame under the per-node copy
  lowering, and M1's place-based lowering is what shrinks it. Measured on
  the machine of §1.4, release binary, generator cache warm, medians of
  three (the compile is inside the wasm column):

  | | `run site/export.vyrn out` | of which before the first route |
  |---|---|---|
  | interpreter | 136.9 s (136.4, 143.8, 136.9) | — |
  | `--engine wasm` | refused after 5.2 s (5.20, 5.18, 5.16) | the load and the compile |

  The interpreter wrote 84 routes and 14 assets, 247 files; the wasm column
  wrote nothing. So §2.5's first row still waits on M1 for this
  program, and nothing else: no further import, builtin or host behaviour
  stood between the export and the compiled route in this slice.
- `test --engine wasm` still runs one instance per body. Not done in this
  slice: the interpreter's per-run state and shared input are the semantics
  to retire, not to reproduce, and the time went to the export.

Gates run before the report, in order: `cargo fmt --all`; `cargo build
--release -p vyrn-cli`; `cargo test --workspace`; the fixture gate; the
kernel, lowered, fieldstore, places, simd, wasmabi, wasmio, traps, bytesink
and audience suites; parity in release with `--ignored`; the residue
ratchet; and the cross-engine generator test, which was red at the base
commit for five programs (a placement defect on another branch) and is red
for the same five here. The wasm manifest is not regenerated in this slice:
the lowering renumbers every module's runtime table, and the integrator
regenerates it after the merge.

### M6 — the other two judgments

Validation by construction replaces the boundary checks. The trap primitive
and its table replace the sites. The effect judgment replaces the audience
and floor passes. Each is a prediction-as-program: one boundary check deleted,
one program that must still refuse.

**Gate:** the audience, floor and contract test suites green with their passes
deleted.

**The first slice landed (2026-09-02): the effect judgment, beside the two
passes, over the corpus.** Nothing is deleted. `vyrn-lower/src/effects.rs`
computes a per-function effect set over the named core; `tests/effects.rs`
runs it over every example and every entry point of the four example
projects, and stands the audience pass (RFC-0072) and the floor (RFC-0103)
beside it, function by function. Every disagreement is a numbered finding
below. The deletion slice reads this list before it deletes anything.

#### The lattice

One table. `effects::ATOMS` is its second column and nothing else;
`tests/effects.rs` reads the table out of this file and refuses to run when
the two differ. A row names an effect, the builtins and host imports that
are its atoms, the WASI import the direct backend (RFC-0077) reaches it
through, and which target provides it — `yes` is RFC-0103 §2's cell, `gen`
is RFC-0021's generation-time sandbox. A function's set is the join of its
own atoms and its callees' sets, to a fixpoint. An owned name born of a
primitive, a literal or a builtin is an allocation; a user callee's set says
whether it allocated. `pure` is the bottom.

| effect | atoms | WASI import | native | wasi | browser | gen |
|---|---|---|---|---|---|---|
| `alloc` | `runtime$malloc`, `mem$grow`; and an owned name born of a primitive, a literal or a builtin | `memory.grow` | yes | yes | yes | yes |
| `read-input` | `readLine` | `fd_read` on 0 | yes | yes | EOF | no |
| `write-output` | `print`, `writeStdout`, `trace`, `debug`, `info`, `warn`, `error` | `fd_write` on 1, 2 | yes | yes | yes | `print` yes, the rest no (finding 4) |
| `fs-read` | `readFile`, `readFileBytes` | `path_open`, `fd_read`, `fd_close`, `fd_prestat_get` | yes | yes | `NOENT` | `readFile` yes, `readFileBytes` no (finding 5) |
| `fs-write` | `writeFile`, `writeFileBytes`, `renameFile`, `fsyncFile` | `path_open`, `fd_write`, `path_rename`, `fd_sync` | yes | yes | `NOENT` | no |
| `fs-list` | `listDir`, `listDirKinds` | `fd_readdir` | no (`NATIVE_UNSUPPORTED`) | yes | `BADF` | yes, mediated |
| `args` | `args` | `args_sizes_get`, `args_get` | yes | yes | empty | no |
| `clock` | `hostNowMillis`, `hostMonotonicNanos` | `clock_time_get`; `environ_get` for `VYRN_FIXED_TIME` | yes | yes | yes | no (an extern) |
| `random` | `hostRandomSeed` | `random_get`; `environ_get` for `VYRN_FIXED_SEED` | yes | yes | yes | no (an extern) |
| `extern` | every other extern declaration, resolved by name | the `vyrn` namespace | trap | no instantiation | yes | no |
| `serve` | `serveStream` | — | trap | trap | trap | no |
| `trap` | `panic`, `@panicAt`, `assert`, `assertEq`, `runtime$trap`, `mem$trap`; and the core's trap statement | `proc_exit` | yes | yes | yes | yes |
| `gen-only` | `moduleInterface`, `contractOf`, `lex`, `render`, `raw`, `rawAt`, `@codeText`, `@codeSplice` | — | no | no | no | yes |

Every one of the fifteen preview1 imports `direct.rs` declares is in the
third column; `environ_sizes_get` and `environ_get` serve the clock and the
seed and nothing else. The runtime module's own primitives (`std/mem`,
`std/runtime`) are pure but for the four rows that name them. `spawn` is not
a row: the core lowers `spawn f(..)` as a call to `f`, so the judgment
cannot see it (finding 1).

#### The comparison

For every function the harness records three things. The judgment's set.
The floor's answer at function grain — which of `fs`, `stdin`, `args` the
body spells, by `floor::CALLS`, a `gen fn` body skipped, as `floor::carried`
does per module. The audience's answer — `audience::audience_of` for the
module the function was declared in, under the project's `vyrn.json`. Each
function lands in one floor kind and one audience kind:

- floor: *agree*; *callee-carried* (the judgment has more, and a callee's
  body spells it, so the floor's union over the closure agrees); *gen-body*
  (a `gen fn` reads at generation time; the floor skips it by design, and
  the verdict agrees because the context differs); *floor-blind* (the
  judgment has an effect no body in the program spells — a disagreement);
  *core-blind* (the body spells a call the core does not lower — a
  disagreement).
- audience: *no fence* (no `audience` key, or a module outside the project:
  std, a remote); *agree*; *declared-only* (server-only or client-only with
  no target-restricted effect — the fence protects a declaration, RFC-0103
  §4; not an effect, stays); *unfenced* (universal or client-only with an
  effect a browser lacks — a disagreement); *server-extern* (server-only
  with an extern a native target lacks — a disagreement).

The ratchet is the sum of the disagreement kinds.
`VYRN_EFFECTS_DUMP=<file>:<fn>` prints one function's set and where each
callee's came from.

#### The tally

`cargo test -p vyrn-cli --test effects`, 2026-09-02, at the commit that
landed this slice:

    effects over the corpus: 181 programs (31 not loadable here), 16650 functions
    judged, 4479 pure, 8 unlowered, 117 calls through a function value
      effect 12110  alloc
      effect    11  read-input
      effect   280  write-output
      effect   192  fs-read
      effect    12  fs-write
      effect   106  fs-list
      effect     8  args
      effect    22  clock
      effect    13  random
      effect    31  extern
      effect    22  serve
      effect  3282  trap
      effect   698  gen-only
      floor:      16414 agree, 19 callee-carried, 216 gen-body, 1 floor-blind,
                  0 core-blind
      audience:   16209 no fence, 23 agree, 418 declared-only, 0 unfenced,
                  0 server-extern

The 181 programs are the 204 examples less the 31 that need the root
manifest's remote dependencies (the count `kernel.rs` reports), plus the
eight entry points of `examples/bin`, `examples/fullstack`, `examples/leak`
and `examples/shelf`. The 8 unlowered are instances of the project entries
whose core the M2 lowering cannot attribute a call in; they are judged
nowhere. **The ratchet is 1**: `listdir.vyrn`'s `main`, finding 6.

#### The findings

Each finding names the kind the harness puts it in, the program that shows
it, and what it is: a pass that is wrong, an atom the lattice lacks, a hole
in the judgment, or a rule that is not an effect and stays. The programs
named `scratch/..` were written for this slice and run against the binary
of this commit; the ones under `examples/` are in the corpus.

1. **The core erases `spawn`** (lattice: a missing atom). §2.2 names four
   effects and the fourth is *may spawn*; the core lowers `spawn f(..)` as a
   call to `f` (`core.rs`, `Expr::Spawn`), so the judgment sees `f`'s set
   and no spawn. The checker's spawn-isolation rule — `spawn work()` is
   refused with "`work` (or something it calls) does I/O", scratch
   `spawned.vyrn`:6 — is exactly an inclusion check, the spawned callee's
   set within `{alloc, trap}`, and it cannot be stated on the core until a
   call carries a spawn marker. `examples/concurrency.vyrn` is the corpus
   case. The deletion slice adds the marker first.
2. **The core does not lower a lambda body** (judgment: a hole). A lambda is
   a read of its captures (`core.rs`, `lambda`) and its body is no
   instance, so a call inside one is judged nowhere. Scratch `lambda.vyrn`:2
   binds `p -> readFile(p)`; the dump gives `main — alloc, write-output` and
   the floor kind *core-blind*, because the body spells `readFile`. The
   corpus has no such function (core-blind is 0), which is why the ratchet
   does not carry it; the deletion slice cannot land while it is possible.
3. **A call through a function value is judged pure** (judgment: a hole).
   117 call sites name a parameter or a binding (`f` in `map<Int64, Int64>`,
   `resolve` in `std/graphql`'s `gqlAnswer`, `cb` in `std/rpc`'s deliverers)
   and resolve to no body. RFC-0037's defunctionalization already knows
   every source a `fn`-typed slot can hold (`checker::StoredSource`); the
   judgment should join over them. `VYRN_EFFECTS_UNKNOWN=1` lists the sites.
4. **The generation fence splits `write-output`** (pass: inconsistent with
   the lattice). `COMPTIME_FORBIDDEN` refuses `writeStdout` and the five log
   levels in a `gen fn` and says nothing of `print`: scratch
   `genprint.vyrn` — a `gen fn` that prints — is `ok`, and the same body
   with `writeStdout` is refused. One effect, two verdicts. The table's
   `gen` column records the split; the deletion slice picks one cell.
5. **The generation fence splits `fs-read`** (not an effect; stays until
   the route is one). `readFile` in a `gen fn` is permitted because it
   routes through the loader's resolver and is a cache input (RFC-0021);
   `readFileBytes` is refused because it does not. Scratch `genread.vyrn`
   is `ok`, `genbytes.vyrn` is refused. The difference is which reads the
   generation cache is keyed by, not what the program does. It stays a
   rule of the resolver until `readFileBytes` takes the same route, and
   then the row is one cell.
6. **The floor has no row for `fs-list`** (pass: wrong — a program that
   should be refused is not). `examples/listdir.vyrn`:18 calls `listDir`
   at run time and is the corpus's one *floor-blind* function. `floor.rs`
   leaves `listDir` out because "no compiled target has them"; M5 lowered
   it over `fd_readdir`, `wasi` runs it, and a page answers `BADF`, so a
   browser artifact calling it now degrades to the canonical `Err` the
   floor exists to refuse. Scratch `p3/client/boot.vyrn` under
   `{ "app": { "target": "browser" } }` passes `vyrn check`. The row
   belongs in `fs`.
7. **An `extern` declaration is a capability the direct backend does not
   need** (pass: refuses a program that runs). The floor carries `extern`
   on the declaration (M0 finding 3: an unanswered import stops
   instantiation). Scratch `p5/unused.vyrn` declares `jsAdd` and never
   calls it: under a native artifact `vyrn check` refuses it ("imports a
   host function"), and `vyrn run --engine wasm` on the same file prints
   and exits 0, because the direct backend sweeps an import nothing
   reaches (RFC-0077 M2p). The judgment gives `main` no `extern`. The
   deletion slice decides whether the rule is a declaration or a call; the
   lattice can state only the call.
8. **Presence per module against reachability per function** (rule
   difference). The floor carries what a module SPELLS, whichever function
   spells it; the judgment carries what a function REACHES. On branches
   the two agree — the judgment joins both arms of every `if`. On dead
   functions they do not: scratch `p6/client/boot.vyrn` under a browser
   artifact has a `dump()` nothing calls that writes a file, the floor
   refuses it, and `main` is judged `alloc, write-output`. The 19
   *callee-carried* functions are the same difference at function grain
   with the closure agreeing. The deletion slice's inclusion check for an
   artifact is over the entry's set, which is reachability, and this RFC
   says so here so the change of rule is on record.
9. **A `gen fn` body is judged; the floor skips it** (context differs,
   verdict agrees). 216 generation-time bodies carry `fs-read`, `fs-list`
   or `gen-only` — `std/ui`, `std/vyx`, `std/i18n`, every generator that
   reads its inputs. The floor is right to skip them for the artifact; the
   judgment is right to see them for the generation context. The table's
   `gen` column is the target row the deletion slice's check reads for
   them. Not a disagreement.
10. **A fence protects a declaration** (not an effect; stays). 418
    server-only and client-only functions in `bin`, `fullstack` and `shelf`
    have no target-restricted effect at all — a route handler, a page, a
    store's shape. RFC-0103 §4 says what the fence is for: a secret in a
    constant uses no capability. The audience pass stays as the declared
    boundary it is; the judgment replaces nothing of it. The `logging {
    sink: file(..) }` carrier is the same kind: the floor's `fs` by
    declaration, the judgment's `write-output` on every log call.
11. **A universal module with a file read, imported by a client** (not a
    disagreement; recorded so nobody looks for it). Scratch `p4` declares
    an `audience` and no `artifacts`; `shared/format.vyrn` calls
    `readFile`, `client/boot.vyrn` imports it. The fence allows the import
    (universal is importable from anywhere) and the floor refuses it, from
    the browser artifact the manifest's `client` key implies. The corpus
    has no *unfenced* function. The floor and the fence divide the work as
    RFC-0103 designed, and the judgment sides with the floor.
12. **Eight instances have no core** (judgment: judged nowhere). The project
    entries reach eight instances whose lowering stops at "a call this
    slice cannot attribute" (§3 M2's gap list); `examples/*.vyrn` has none.
    They are counted as unlowered and are outside the ratchet.
13. **The generation fence names the clock an extern** (verdict agrees;
    wording does not). Scratch `genclock.vyrn` calls `std/time`'s `now` in
    a `gen fn` and is refused as "calls the extern `hostNowMillis`". The
    table's `gen` column says no for `clock`, so the verdict stands; but
    RFC-0103 M2 established that the three host-boundary externs are not
    imports, and the fence's `extern_fns` set does not know it. When the
    fence becomes the inclusion check, the reason is the row: `clock`.

What the deletion slice inherits, in order: the spawn marker (1), lambda
bodies in the core (2), the function-value join (3), then the three table
cells the passes disagree with (4, 6, 7), then the rule change on record
(8). Findings 5, 9, 10, 11 and 13 change nothing.

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
