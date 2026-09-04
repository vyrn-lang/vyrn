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
  lowering), and the numbers are in §3 M5. On 2026-09-03 the site export
  ran under `--engine wasm` byte-identical to the interpreter's, once RFC-0089
  rule 2's alias half was stated in the core, the kernel and the checker
  (§3 M5, "the take out of a `read` parameter").
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
needs (load, store, allocate pages) and the host imports it calls (the WASI
table, each a declaration the emitter lowers to one `call`) are the only
unsafe surface in the language, fenced in that module, and reviewed there
(`PLAN-0125-runtime.md` §2.1 and §2.2). Both the C shim's logic
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

**The take out of a `read` parameter (2026-09-03): the rule, stated once,
and the export byte-identical.** The defect above and `placeorder.vyrn`'s
alias write (M5's second slice) are one rule seen from two sides. Both
probes are under `rfcs/probes-0125/`, and what each engine did before the
fix is pinned here:

| probe | interpreter | native | `--engine wasm` |
|---|---|---|---|
| `take-out-of-a-read-parameter.vyrn` (`let mut mt = h.meta` on a `read` parameter, then `mt.push(..)`; the caller reads `titled.meta` after) | `2`, `1`, `a` | exit `0xC0000374`, nothing printed | trap in `free`, nothing printed |
| `alias-then-write-through-the-root.vyrn` (`let before = t.xs`, then `t.xs[0] = 99`, then `before[0]`) | `99`, `1` | `99`, `99` | `99`, `99` |

The interpreter copies on write (`Rc::make_mut`), so it prints the answer a
copy would; the compiled routes share the buffer, so the first probe
releases one buffer twice and the second reads the write through it.

The rule is RFC-0089's rule 2 and RFC-0090's one line: a `read` or
`modify` value may be observed and passed on, it may not be retained,
stored, returned or consumed, and all mutation is exclusive. So `let mut mt
= h.meta` is neither a copy nor a move: it is a borrow of `h.meta`, and the
program is refused where the borrow is taken or where its place is written,
with `.copy()` named at the binding. Not a copy, because RFC-0088 put the
cost of a copy at the call site where a reader can see it (`copy` is "the
escape hatch"), and a hidden allocation on every `let x = p.f` would undo
that. Not a refusal of the `let` itself, because a read-only projection is
the corpus's commonest idiom and costs nothing.

Where it lives:

- **The core** (`core.rs`): a name carries `borrow` — its type owns heap,
  the body does not own it, and it is not static data or the source of a
  whole-value alias. A `read` parameter, a `let` of a place read
  (`is_place_read`), a second name for a borrow, a lending call's result, a
  payload binder and a `for` variable over a container the loop does not own
  are borrows. An `if` or `match` expression with an arm that yields a
  borrow yields one (`names_a_place`'s reading, one arm is enough); it used
  to be lowered as an owned temporary the arms stored a borrow into. A
  nullary constructor (`None`, a fieldless variant) is a literal, not a read
  of module state. Module state as the receiver of a rebuilding call
  (`books.push(b)`) is a take of the place and a store back, as a field is.
- **The kernel** (`kernel.rs`, "Borrows"): a borrow bound by a read of a
  place is an alias of that place, resolved through the aliases on its root
  and remembering the name it was read through. A take of an alias — a
  `consume` argument, a literal part, a store into an owned binding, a
  `return` — is refused. A take of a sub-place, a store into a place, a
  store into a binding, a drop and a placed row end every alias of the
  storage they write, except the alias the write goes through and its
  chain (RFC-0082's desugar reads `t.xs` into `t.xs[]` and writes through
  it); a later read of an ended alias is refused at the write. The write-back
  of that desugar, an alias stored into the very place it reads, changes no
  owner and is not a take. A `let` rebinds: the alias and its end are
  cleared. Joins keep an alias either edge bound and an end either edge
  made; a loop whose body ends an alias is walked once more from that
  state. The wordings are the checker's, below.
- **The checker** (`movecheck.rs`): the write-back statement's exemption from
  `sinks` (a rebuilding builtin takes its receiver) held for any receiver.
  It now holds for a place this frame owns and for the `modify` parameter
  itself; a borrowed local — a projection, a second name for a `read` or
  `modify` parameter, a loop variable — is refused at its binding:
  ``` `mt` is read out of `h.meta` here — a place that owns it / line 19: ...
  and `push(..)` takes `mt`, so `mt` must be a value of its own / fix:
  `h.meta.copy()` if `mt` should own what `push(..)` rebuilds ```, and
  `vyrn fix` applies the copy on the binding's line. Each borrow records
  what it reads (`reads`, in lockstep with `borrows`), and a write to a
  place — an assignment, a field or element store, a prefix take, a drop —
  records a consumption for every borrow reading storage that touches it;
  the later use is refused as a move is: ``` `t.xs[..]` is written here while
  `before` still reads out of it / line 15: ... and `before` is used again
  here / fix: `t.xs.copy()` on line 12, so `before` is a value of its
  own ```. The kernel prints the same two lines without the menu.
- **The engines**: nothing. A program the rule refuses never reaches them,
  and a program it accepts they already release once.

The corpus had eleven sites, every one edited to the spelling the rule
wants and none with a changed output: `std/ui.vyrn`'s four `with*` head
builders (the site's own copy of the probe: `h.meta.copy()` and `meta: mt`),
`std/rpc.vyrn`'s `rpcApplyConfig` (`cfg.pinKeys.copy()`, and its returned
literal read `cfg.prefix` through an `if` expression the checker's `store`
does not look into), `std/vyx.vyrn`'s selector merge, `std/i18n.vyrn`'s two
insertion sorts (`let tmp = names[j - 1]` then `names[j - 1] = ..`: the
store released the element `tmp` still read, and `tmp.copy()` copied out of
freed memory), `examples/show.vyrn`'s `tag` (`let mut out = parts[0]` then
`return out`, a borrow returned on the empty path; the copy is annotated
`String`, because the analysis cannot type a copied element and left it
unreleased), `examples/assoctype.vyrn`'s generic `valueOr` (a `read`
parameter returned, invisible to the checker because `Output` has no type
until it is instantiated; its copied result is now a `leak 1` row in the
residue baseline, because the analysis reads the generic body, says the
return type `T` owns no heap, and the caller releases nothing), and the three
generator-only token loops in `std/cli.vyrn`, `std/http.vyrn` and
`std/von.vyrn` (`tk.text` into a literal, invisible to the checker because a
`lex()` token has no type outside generation). `placeorder.vyrn`'s alias
test is now "a field write does not disturb a copy taken before it", with
the copy written. The kernel corpus test's ratchet is still 0: 11,306
instances accepted, 0 refused, 0 unlowered.

Not modelled, recorded: what a `modify` argument does to the aliases of what
it is handed (`examples/tree.vyrn`'s `freeNode` reads a child handle out of
the node it then removes, and a handle is safe to hold, so the kernel does
not end an alias at a call); a `read` parameter taken as a whole (the
checker's `check_handover`); a lending call's result (`a[i]` through a
projection) has no place the kernel can name; and the checker's two blind
spots above (a place read through an `if` expression at a store, and a
`lex()` token's type) stay the kernel's finds until the checker types them.

**The gate.** From the repository root, `site/data/history.json` and
`demo.json` generated, `out/` and its five subdirectories present, release
binary, generator cache warm, on the machine of §1.4:

| | `run site/export.vyrn` | files written |
|---|---|---|
| interpreter | 160.1 s | 241 |
| `--engine wasm` (compile and load inside) | 7.5 s | 241 |

`diff -r` between the two trees: empty, and the two runs' standard output is the same 82 routes and 14 assets line for line. That is §2.6's fixture gate for
the largest program in the repository, and the first time the compiled
route wrote it.

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
  take of a name it does not own. *Closed for the place-read half on
  2026-09-03 (§3 M5, "the take out of a `read` parameter"): the core marks
  a borrow, the kernel keeps what a borrow bound to a place reads, refuses a
  take of it and ends it at a write to the place. A parameter taken as a
  whole, a lending call's result and a `modify` argument's effect on the
  aliases of what it is handed are still the checker's alone.*
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

**The census (2026-09-03): every refusal the move checker can give, and
what the kernel says about the same program.** The slice after this one
takes `movecheck.rs`'s placement code away, and it may take away only what
something else states. So each refusal site — a `menu(..)` or a
`Diagnostic::error(..)` in `movecheck.rs`, and the two `region` guards the
close-out above attributed to it — has one minimal refused program under
`compiler/vyrn-cli/tests/refusals/`, and `tests/refusals.rs` runs each twice:
`vyrn check` for the checker's sentence, and `VYRN_NO_MOVECHECK=1
VYRN_KERNEL_STRICT=1 vyrn check` for the kernel's. The last column is
asserted per row, in four values. *The same* means the kernel prints the
checker's whole sentence at the same file and line, minus the `fix:` menu.
*Its own words* means it refuses the program in a sentence of its own, so
the row is not closed: what a reader is told would change the day the
checker goes. *Nothing* means the kernel accepts the program. *Not the move
check's* means the refusal survives `VYRN_NO_MOVECHECK=1` because another
pass gives it.

The table below is the state AFTER this slice closed what it could. It is
printed from the test's own rows (`cargo test -p vyrn-cli --test refusals --
--ignored --nocapture the_census_as_a_table`), so the prose and the
assertion cannot drift apart by a transcription. Before the slice the column
read: 6 the same, 10 its own words, 16 nothing, 2 not the move check's. It
now reads 11, 15, 6 and 2.

| # | rule | RFC | the checker's sentence | the kernel |
|---|---|---|---|---|
| 01 | rule 2: an element read may not be stored | RFC-0092 | `b.xs[0]` may not be stored into `push(..)` — it is read out of a place that owns it | its own words |
| 02 | rule 2: a field of a `read` parameter may not be stored | RFC-0089 | `h.meta[0]` may not be stored into `push(..)` — it is a `read` parameter | its own words |
| 03 | rule 2: a projection is a borrow of its root, whatever the root is | RFC-0092 | `d.title` may not be stored into `push(..)` — it is read out of a place that owns it | its own words |
| 04 | a name with a hole may not be used whole | RFC-0093 | `p.name` was taken out of `p` here / line 10: ... and `p` is used as a whole here, with the hole still in it | the same |
| 05 | a write to a place ends every alias that reads out of it | RFC-0090 | `t.xs[..]` is written here while `before` still reads out of it / line 9: ... and `before` is used again here | the same |
| 06 | rule 1: a `consume` parameter takes ownership | RFC-0089 | `x` is used here but was already consumed by `take(..)` on line 8 /   (a `consume` parameter takes ownership; the value can't be used afterward) | the same |
| 07 | rule 1: a move into a binding, and a use of the source after it | RFC-0089 | `s` was moved here into the binding `t` / line 5: ... and `s` is used again here | the same |
| 08 | `consume` reaches a field, never an element | RFC-0093 | `xs[0]` may not be taken — an element is not a place a take reaches | nothing |
| 09 | `consume` with nothing to take | RFC-0093 | `consume` here has nothing to take — the value is already owned, so there is no place to leave a hole in | nothing |
| 10 | module state may not be taken: a prefix `consume` | RFC-0013 | module state `names` may not be consumed by a take — nothing may take ownership of module state (it lives for the whole module and is never dropped) | its own words |
| 11 | rule 2: a prefix `consume` of a `read` parameter | RFC-0089 | `ys` may not be consumed — it is a `read` parameter | its own words |
| 12 | module state may not be taken: a `consume` parameter | RFC-0013 | module state `names` may not be passed to a `consume` parameter via `take(..)` — nothing may take ownership of module state (it lives for the whole module and is never dropped) | the same |
| 13 | rule 2: a whole `read` parameter to a `consume` parameter | RFC-0089 | `ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` parameter | the same |
| 14 | rule 2: a projection to a `consume` parameter | RFC-0092 | `d.title` may not be passed to a `consume` parameter via `take(..)` — it is read out of a place that owns it | its own words |
| 15 | module state may not be taken: a `return` | RFC-0013 | `names` may not be returned — it is module state, which nothing may take, and a return is owned | the same |
| 16 | rule 2 at the return: a field of a `read` parameter | RFC-0089 | `d.title` may not be returned — it is a `read` parameter, and a return is owned | its own words |
| 17 | an exported function owns its result | RFC-0012 | `s` may not be returned from an exported function — it is a `read` parameter, and the JS caller releases what it is handed | its own words |
| 18 | rule 2 at the return: a whole `read` parameter | RFC-0089 | `ys` may not be returned — it is a `read` parameter, and a return is owned | the same |
| 19 | rule 2 through a wrapper: a `read` parameter put into the result | RFC-0089 | `s` may not be put into `Some(..)` — it is a `read` parameter | its own words |
| 20 | rule 1 at the drop: what a `consume` parameter took is gone | RFC-0089 | `a` is dropped here but was already consumed by `take(..)` on line 6 | its own words |
| 21 | rule 4 at the drop: the place that owns a value releases it | RFC-0089 | `owned` may not be dropped — it is read out of a place that owns it | its own words |
| 22 | `drop` releases the whole binding, and a take left a hole | RFC-0093 | `p` may not be dropped — `p.name` was taken out of it on line 17, and `drop` releases the whole binding | nothing |
| 23 | a `modify` borrow is exclusive | RFC-0090 | `a` is passed to `bump` as `modify` and read again in the same call — a `modify` borrow is exclusive | nothing |
| 24 | a closure that outlives the call may not capture a borrow | RFC-0037 | `s` may not be captured by a closure that outlives this call — it is a `read` parameter | nothing |
| 25 | rule 1 across a back edge | RFC-0089 | `x` is consumed by `take(..)` inside a loop, so it would be used again on the next iteration | the same |
| 26 | a rebuilding builtin takes its receiver | RFC-0125 | `mt` is read out of `h.meta` here — a place that owns it / line 7: ... and `push(..)` takes `mt`, so `mt` must be a value of its own | the same |
| 27 | rule 2: a `read` parameter to a builtin that declares `consume` | RFC-0089 | `xs` may not be stored into `fromArray(..)` — it is a `read` parameter | its own words |
| 28 | a closure's result is its caller's, and a capture is not its to give | RFC-0037 | `s` may not be returned from a closure — it is a captured binding, and the closure's result is its caller's | the same |
| 29 | module state may not be taken: `for .. in consume` | RFC-0013 | module state `names` may not be consumed by a `for` loop — nothing may take ownership of module state (it lives for the whole module and is never dropped) | its own words |
| 30 | a must-use obligation is discharged on every path | RFC-0075 | `s` is a `Stream` and is never disposed | nothing |
| 31 | a must-use obligation is discharged exactly once | RFC-0075 | `s` is a `Stream` and is disposed more than once | its own words |
| 32 | a value the region allocated may not be stored where it outlives the region | RFC-0004 §4 | cannot store a heap value into `kept`, which outlives the enclosing `region` (it would dangle when the region frees). Move `kept` inside the region, or compute a non-heap result to carry out. | not the move check's |
| 33 | a `consume` parameter may not take a value the region frees | RFC-0004 §4 | cannot hand a heap value to argument 1 of `take`, which is `consume`, inside a `region`. The region frees the value at its closing brace, so the callee cannot own it. Move the call out of the region, or pass a value that holds no heap. | not the move check's |
| 34 | rule 2: a `read` parameter into a builtin's `consume` argument | RFC-0089 | `s` may not be stored into `push(..)` — it is a `read` parameter | its own words |

Three things the census found before a line of kernel code changed.

- **The `region` escape rule is not the move check's.** Rows 32 and 33 are
  refused with `VYRN_NO_MOVECHECK=1`, because `checker.rs`'s
  `region_store_guard` and `region_consume_guard` give them, not
  `movecheck.rs`. The close-out above listed region escape under "from
  `movecheck.rs`"; that attribution is wrong, and the rule owes the deletion
  track nothing.
- **Seven of the fifteen open rows are one rule.** Rows 11, 13, 17, 18, 19,
  27 and 34 are all a `read` parameter taken whole — consumed, returned,
  stored, put into a literal — and the kernel is silent on every one because
  a parameter has no alias in its table, only a place-read borrow does.
- **A refusal in words of its own is not a closed row.** Rows 12, 15 and 29
  are module state, and the kernel refuses each as a place read: "read out
  of `names`, a place that owns it". True, and not RFC-0013's sentence,
  which names the reason the rule exists.

**What the slice closed, rule family by rule family.**

- **A `read` parameter taken whole** (rows 11, 13, 17, 18, 19, 27, 34, and
  28 with it). A parameter is a borrow with no place to be an alias of, so
  the alias table never saw one. The core carries its kind instead
  (`core::BorrowKind`), which is the capability and the parameter's own
  spelling, and the kernel refuses a take of it — a `consume` argument, a
  `return`, a store, a literal part — in the checker's words. `let t = s`
  carries the kind on, so a second name says so. Two exceptions the rule
  needs: a `consume` parameter is not a borrow, and neither is a parameter
  whose type carries a must-use obligation, because RFC-0075 M1 states that
  "a stream PARAMETER carries the obligation into the callee" whatever the
  capability says — without that, eleven corpus instances of
  `boxStream(s)` were refused.
- **Module state** (rows 10, 12, 15, 29). `consume <global>` was a gap, so
  the whole body went unjudged; it lowers as a read of the global now, like
  every other read of one. The kernel words a take of a whole global with
  RFC-0013's own sentence, which names the reason: the global lives for the
  whole module and nothing drops it, so there is no owner to take from.
- **A capture** (row 28). A lambda frame's captures carry
  `BorrowKind::Capture`, and a `return` of one gets RFC-0037's sentence.
- **The `drop` of a borrow** (rows 21 and 29). A release is a take, so a
  `drop` of an alias is refused as one; the sentence is the checker's and
  the line is the binding's, because a `Drop` in the core carries none.

**What the slice did not close, and why.**

- **Region escape** (rows 32, 33). Nothing is owed. The rule is
  `checker.rs`'s (`region_store_guard`, `region_consume_guard`), it survives
  `VYRN_NO_MOVECHECK=1`, and the close-out's attribution of it to
  `movecheck.rs` was wrong.
- **A closure that outlives the call** (row 24). Whether a closure escapes
  is a lifetime question, and §2.2 has no lifetimes on purpose — borrowing
  is second-class so that the kernel stays small. It stays the checker's
  until something states the rule without them.
- **`consume` with nothing to take, and `consume` of an element** (rows 9,
  8). Both are about the KEYWORD, not about ownership: `consume make()` and
  `make()` denote the same value, and the kernel has no keywords. The rule
  belongs where the desugar is written, which is the core.
- **A `modify` argument's effect on aliases.** Tried and measured. Ending
  every alias that overlaps a `modify` argument refuses three corpus
  programs, all the same shape: `freeNode` in `tree.vyrn`,
  `linkedlist.vyrn` and `freelist.vyrn` reads `t[h].left` — an
  `Option<Handle<T>>`, which `owns_heap` calls heap because a wide payload
  travels boxed (round twenty-nine) — and then calls `remove(t, h)`, which
  shuffles two index arrays and never touches the payload the read points
  into. So the refusal is a false positive, and the rule needs to know WHICH
  place a callee writes. That is the per-argument retention over the call
  graph (`ArgVerdict`, `note_handover`) the deletion track already owes, and
  it is where this rule belongs.
- **Rows 30 and 31**, the must-use obligation, and **row 22**, a `drop`
  after a hole. Not attempted this slice.
- The `fix:` menus, the `sinks` write-back exception, and `own.rs`'s
  per-node tables are the next slice's, as the close-out above says. The
  write-back exception is now marked rather than modelled: a rebuilding
  builtin's receiver passed by name carries `Rhs::Call::write_back`, and the
  kernel exempts that one take, so a `modify` parameter may still be one.

**Three corpus programs were refused by the kernel and accepted by the
checker, and all three were defects.** Each is the checker's recorded blind
spot — a generic body has no type for `T` until the instance, and the
checker does not re-run per instance — so each is a rule 2 violation nobody
had seen.

| program | what it wrote | what it did |
|---|---|---|
| `examples/generics.vyrn` | `fn id<T>(x: T) -> T { return x }` | `id("polymorphic")` returned the caller's buffer; the reduced form exits `0xC0000374`, STATUS_HEAP_CORRUPTION |
| `examples/regex.vyrn` | `return Username(name)` on a `read` parameter | a validated constructor hands its argument on as the result, so the caller and the result released one buffer; same exit code |
| `examples/polyrecursion.vyrn` | `return P { a: x, b: x }` | two fields out of one `read` parameter — two owners for one value |

Each took the `.copy()` the rule names, and no output changed. The kernel
tally: 18,343 instances accepted, 0 refused, 0 unlowered, so the ratchet is
still 0.

**The census (2026-09-03): every read of the plan in the direct wasm
emitter, and whether the core says the same thing.** The slice after this
one deletes `own.rs`'s per-node tables, and it may delete only what
something else states. So every `self.cx.plan.*` in
`compiler/vyrn-codegen/src/direct.rs` is one row below, with the function
that reads it, what the read decides, and whether the core carries the same
fact at the same key. `compiler/vyrn-cli/tests/coretables.rs` pins the last
column: it runs the analysis over the corpus with the placer installed and
diffs the core's own answer (`vyrn_lower::core::facts`, folded out of every
body and every lambda frame AFTER the placer has added its rows) against
the plan's at the same site.

| # | reader | table | what it decides | the core |
|---|---|---|---|---|
| 01 | `Fn_::stmt`, the `String` append spine | `store_owned` | whether the buffer the append copied out of is this path's to free | a key, not stated |
| 02 | `Fn_::stmt`, a store to a name | `store_owned` | whether the store releases the value it displaces | a key, not stated |
| 03 | `Fn_::stmt`, a store to a name | `store_fresh` | whether a mention of the place in the value can hand the old buffer back | a key, not stated |
| 04 | `Fn_::stmt`, `SetField` | `store_owned` | the same, through a field | a key, not stated |
| 05 | `Fn_::stmt`, `IndexSet` | `store_owned` | the same, through an element | a key, not stated |
| 06 | `Fn_::stmt`, `IndexSet` on a map, and a projection's store | `store_owned` | acknowledged only, so §26's finish check counts the row | a key, not stated |
| 07 | `Fn_::elem_field_store` | `store_owned` | the same, for the `a[i].f = v` idiom's three statements | a key, not stated |
| 08 | `Fn_::stmt`, `Stmt::Expr` | `discarded_results` | free an owned result nothing binds, rather than dropping it | a key, not stated |
| 09 | `Fn_::expr` | `arg_drops` | tee this node's value and free it after the call or operator above | a key, not stated |
| 10 | `Fn_::stmt`, `Stmt::If`; `Fn_::join`; `Fn_::match_expr` | `edge_releases` | the release one edge of a join owes because another edge took the name | a position, not a key |
| 11 | `Fn_::expr_inner`, `Expr::Field` | `receiver_frees` | free the unnamed receiver right after the read | **carried** |
| 11b | `Fn_::expr_inner`, `Expr::Field` | `receiver_malloc` | whether that free stands inside a `region` — a callee allocation is malloc-side | the region stand-down, not modelled |
| 12 | `Fn_::expr_inner`, `Expr::Field` | `receiver_holes` | the field the read took, which the free walks around | **carried** |
| 13 | `Fn_::match_expr` | `arm_frees` | the payload binders this arm releases, and the holes each has | **carried** for a `match` |
| 14 | `Fn_::frees_boxes` | `consuming_matches` | whether the arms free the boxes their binders came out of | a different rule |
| 15 | `Fn_` construction | `releases` (through `own::placed`) | the order and the exits this body releases at | `St::Row`, keyed the same |
| 16 | `Fn_` construction | `droppable`, `early`, `holes` | which bindings get a release slot, with what kind and what holes | a name's `owned` and `holes` |
| 17 | `compile_inner` | `unconsumed` | §26's finish check: a planned row no query hit | the audit's, not a placement read |
| 18 | `Fn_::expr_inner`, `Fn_::join`, `Fn_::match_expr` | `alias_clones`, `alias_scope`, `alias_unwind` | the node a projection's expansion stands for | not a placement table |

Three readings of the table.

- **Three tables are carried and flipped in this slice.** Rows 11, 12 and 13.
  The core states each as a `St::Drop`: of a name whose `NameInfo::receiver`
  is the `Expr::Field` node, and of a payload binder named by `Arm::frees`,
  with `NameInfo::holes` as the row's hole set. The test diffs 4,350 arm
  sites and 21 receiver sites, in both directions, and finds no difference —
  which is what lets the emitter read `core::facts` for all three;
  `VYRN_PLAN_ROWS=1` puts it back on the plan for a bisect, and
  `VYRN_NO_PLACER=1` does too, because the placer is what folds the core's
  answers. The emitted bytes did not change: `VYRN_WASM_MANIFEST=check` is
  green after each commit.

  Two things the flip needed, and both were the core's gap rather than the
  emitter's. **The arm's releases had to be NAMED.** A reader cannot find
  them by position: `stmt` pushes them at the end of the arm's own
  statements and then the edge drops of a join follow, so the trailing run
  of `St::Drop` is sometimes the arm's rows and sometimes Rule N's. `Arm`
  carries `frees: Option<Vec<Name>>` now — the binders this arm releases,
  or `None` where this pass states no answer. The `if let` and `?` desugars
  build arms of their own and consult no table, so they state `None` and
  the emitter reads the plan at those sites; 117 plan rows in the corpus
  are there, and no emitter this slice flips reads the table for them.
  **And a `_` payload binder had to exist.** `bind_pattern` skipped it — it
  names nothing a body can read — so a consumed scrutinee's `Err(_) => ""`
  had a real payload the core never mentioned, while the plan's arm table
  named `_` and both compiled backends freed it. The binder is bound and
  left out of the scope now. Three corpus programs have the shape
  (`fasta.vyrn`, `knucleotide.vyrn`, `revcomp.vyrn`), and each was a leak
  the kernel could not see; the tally did not move, because the plan's row
  was already placed at every one.
- **Seven have no key in the core, and that is the whole obstacle.** Rows 01
  to 09 are the store, discard and argument-temporary decisions. The core
  states every one of them — `St::Store` carries `Old::Released`, a
  discarded result is a temporary with a `St::Drop`, an argument temporary
  is a name in the drop queue — but `St::Store` and `St::Drop` carry a line
  and no node, so nothing can be looked up by the address the emitter walks.
  Row 10 is the same shape one level worse: an edge release is a `St::Drop`
  at a POSITION in a branch, and a position is not a key at all. Each is a
  "make the core carry it" step: a `site` on `St::Store`, on the discarded
  temporary's `Let`, and on the edge drops, before the flip.
- **One table states a different rule, and the difference is the emitter's.**
  `St::Switch::consuming` is not `consuming_matches`. It is the whole
  disjunction `frees_boxes` computes — a `consume`, a scrutinee that names
  no place, or the table — narrowed to an owned scrutinee with no placed
  release after the construct. The core says "consuming" at 1,089 sites in
  the corpus where the table names far fewer, and one site
  (`scan.vyrn`'s `match q { Some(s) => s, None => "<none>" }`) goes the other
  way, because the plan placed the scrutinee's release after the construct
  and the core then treats the binders as borrows. So the row is not a flip:
  it is `frees_boxes`'s own rule, and the core would have to state that rule
  before the table can go.

**What the direct emitter no longer reads.** `receiver_frees`,
`receiver_holes`, and `arm_frees` at every `match` — three of the nine
tables. It still reads `store_owned`, `store_fresh`, `discarded_results`,
`arg_drops`, `edge_releases`, `consuming_matches`, `receiver_malloc`, and
`arm_frees` at an `if let` or a `?`; it still reads `releases`, `droppable`,
`early` and `holes` to build a frame, and it still ACKNOWLEDGES every row it
took off the core, so §26's finish check keeps counting the plan's decisions.
The acknowledgement (`ReleasePlan::acknowledge`) goes when the tables go.

**What the fold costs.** The placer builds every body once to place, and
the rows it adds are not in the body it built — so the facts come from a
SECOND build, after the placer is done, reusing the same lowering. The site
export measures the difference: 2 m 14 s with the fold, 2 m 10 s with
`VYRN_PLAN_ROWS=1`, which skips it because nothing would read it. About 3
per cent, and it goes when the plan goes and the placer's own build is the
only one.

**The lesson the flip taught, and it is the one this RFC keeps finding.** A
fact stated at a POSITION is not stated. The arm table looked carried — the
core pushes a `St::Drop` per released binder — and the reconstruction was
wrong in both directions until `Arm` NAMED the binders: too many when it
swept the arm's own `drop l`, too few when Rule N's edge drop followed the
row and ended the trailing run. A table is carried when a reader can ask
for it by key, not when a careful reader can see it.

**The emitter-reads-the-core slice (2026-09-03): the seven keyless tables are
keyed, and five more rows are flipped.** The census above named one obstacle
for rows 01 to 10 — the core states the decision and nothing can ask for it,
because `St::Store` and a temporary's `St::Drop` carry a line and no node.
`core::Site` is that key, and the core's doc states what one is: **a site is
the address of the AST node the ownership plan keys its row by, and nothing
else.** Not a source position, not an ordinal. The statements that carry one:
`St::Store` (its `Stmt::Assign`, `Stmt::SetField` or `Stmt::IndexSet` node), a
`St::Drop` of a discarded result (its `Stmt::Expr`), a `St::Drop` one edge of
a join owes (`Site::Edge`, the join and the edge), `St::Row`, `St::If`,
`St::Block`, `Break`, `Continue`, `Return` and `Arm`. An argument temporary's
key rides on the name rather than the drop (`NameInfo::arg_drop`), because the
drop that runs it belongs to the binding after the call. Everything else
states `Site::None`, and a reader falls back to the plan there.

The census, with the column this slice moved. Rows 15 to 18 are unchanged.

| # | table | the core, before | the core, now |
|---|---|---|---|
| 01 | `store_owned`, the append spine | a key, not stated | **carried** |
| 02 | `store_owned`, a store to a name | a key, not stated | **carried** |
| 03 | `store_fresh` | a key, not stated | **carried**, inside 02's one answer |
| 04 | `store_owned`, `SetField` | a key, not stated | **carried** |
| 05 | `store_owned`, `IndexSet` | a key, not stated | **carried**, except a user container's |
| 06 | `store_owned`, a map entry and a projection's store | a key, not stated | acknowledged only, as before |
| 07 | `store_owned`, the `a[i].f = v` idiom | a key, not stated | acknowledged only, as before |
| 08 | `discarded_results` | a key, not stated | **carried** |
| 09 | `arg_drops` | a key, not stated | **carried** |
| 10 | `edge_releases` | a position, not a key | **carried**, by join and edge |
| 11 | `receiver_frees` | carried | carried |
| 11b | `receiver_malloc` | the region stand-down, not modelled | unchanged |
| 12 | `receiver_holes` | carried | carried |
| 13 | `arm_frees` | carried for a `match` | unchanged |
| 14 | `consuming_matches` | a different rule | measured, and not flipped |

`compiler/vyrn-cli/tests/coretables.rs` reconstructs each new row from the
core and diffs it against the plan over the corpus, in both directions, with
the plan's rows restricted to functions the lowering instantiated — a row in a
function nothing built is nobody's to state, exactly as §26's finish check
skips a row in a function nothing emitted. What the diff found, and where each
was fixed:

- **`arg_drops`: the core keyed a quarter of the rows.** 2,184 of the plan's
  reached rows had no key. Three causes, all the core's. 1,646 were
  temporaries `read_val` had already queued for release, and the key was
  written only where the queue was empty — the drop was the same drop either
  way. 531 were an OPERATOR's operand: `a + b` is `@concat(a, b)` to the plan
  and a `Prim` to this pass, so `call` never saw them. Seven were a `lazy`
  field read, which binds a borrow here, so no drop carried a key at all. The
  key is taken once now, in `read_val`, for every read in an argument
  position, and it stands whether or not this pass releases the temporary
  itself: what the row answers is "does an argument-temporary drop stand at
  this node", and that is what an emitter asks. 2,188 rows, none missing.
- **`store_owned`: `Old` was not the answer, and the difference was 123
  modules.** Two findings. First, both compiled backends spell an exception
  the core did not — a String concatenation builds a fresh buffer whatever it
  reads, so a mention of the place cannot be a hand-back (`s = s + x`), which
  they call `fresh_str`. It is stated in the core now, so the rule is in one
  place. Second, and this is the finding: `old` and the emitter's question are
  not the same question. `old` is what the KERNEL sees at the place, and a
  place holding nothing that owns heap displaces nothing whatever the plan
  decided about the statement; the emitter asks what the plan decided. Reading
  `Old::Released` as the emitter's answer changed 123 of the corpus's 172
  modules. `St::Store` carries both, named apart, and the fold reads the
  second.
- **`store_owned`, the residue left to the plan: twelve rows.** RFC-0091 M2
  rewrites a user container's `c[i] = v` into a block of its own before the
  checker walks it, so the plan's row stands on a statement this pass never
  sees while the source statement it does see has an answer of its own. The
  core states no site for such a store, so the emitter falls back; the count
  is pinned in the test, and a thirteenth is a site nobody has looked at.
- **`edge_releases`: a sub-place row has a name, and a temporary does not.**
  Rule N's row may be `d.line`, one level down, which the core lowers as a
  take into a temporary dropped at once. A temporary spelled `@t7` gives a
  reader nothing back, so the temporary is spelled for the place it took.
- **`discarded_results`: zero rows in the corpus.** Round twenty-eight's table
  is empty across every function the lowering reaches, so the flip is a no-op
  that stands for the day a row appears. Recorded because a green diff over an
  empty table proves nothing about the table.
- **Every reader falls back per node, and the fallback is the site's OWN old
  answer.** Two gates outside the corpus made the same point twice. The first
  cut had `arg_drops` and `discarded_results` read the core alone, absent
  meaning no: `valuecount.vyrn`, a parity fixture and no corpus program, reads
  a field off a String literal (`"héllo".byteLength`), which this pass refuses
  as "a place that is a literal", so `main` is not lowered, the core states
  nothing about any of it, and six argument drops went unemitted. §26's finish
  check caught it, which is the loudness it exists for. Then the cross-engine
  generator gate caught the sharper form: a store to a NAME fell back to the
  plan's ROW, and the row alone is not what that site used to read — the
  `mentions_place` guard and the `fresh_str` exception stood beside it.
  Seventeen generated programs trapped, and `VYRN_PLAN_ROWS=1` did not restore
  them, because the fault was in the fallback rather than in the flip. Stated
  once: where the core answers, the answer stands; where it does not, the site
  reads exactly what it read before the flip.

| the corpus, this slice | sites |
|---|---|
| `store_owned`: core states / plan rows / left to the plan | 50,569 / 39,338 / 12 |
| `arg_drops`: core / plan | 2,188 / 2,188 |
| `edge_releases`: core / plan | 52 / 52 |
| `arm_frees`, `receiver_frees` (the previous slice) | 4,350 / 21 |
| `discarded_results`: core / plan | 0 / 0 |
| kernel: accepted / refused / unlowered | 18,841 / 0 / 0 |

**Row 14 was measured and left alone, and the measurement is the reason.**
`frees_boxes`'s rule stated in the core would be: the construct took its
scrutinee, so the boxes its binders came out of are its own to give back. The
core states exactly that (`St::Switch`'s `consuming`) at 1,808 switch sites.
Against the plan's `consuming_matches` it says yes at 1,089 sites the table
does not name — those are temporary scrutinees, which the emitter's own
disjunction already covers — and no at six the table does name. The six are
the finding. Each is `let s = match o { Some(v) => v, .. }` on an owned local,
and round twenty-seven's table is what MAKES that local's row `Aliased`: the
core then reads the name as a borrow and the rule answers "not taken". The
rule rests on an ownership the decision itself changes. Flipping stopped
freeing six payload boxes over `jsonplace.vyrn`, `matchown.vyrn` and
`refutablelet.vyrn`, each one a leak, so the emitter keeps the table and the
core owes a binding's ownership stated apart from the decision it feeds.

**What the direct emitter no longer reads.** `store_owned` and `store_fresh`
at every store but a user container's element store and the two acknowledged
idioms; `discarded_results`; `arg_drops`; `edge_releases`; and the three the
previous slice took (`receiver_frees`, `receiver_holes`, `arm_frees` at a
`match`). What it still reads: `consuming_matches`, `receiver_malloc`,
`arm_frees` at an `if let` or a `?`, `store_owned` at the twelve rewritten
statements, and `releases`, `droppable`, `early` and `holes` to build a frame.
It still ACKNOWLEDGES every row it took off the core, so §26's finish check
keeps counting the plan's decisions; the acknowledgement goes when the tables
go. `VYRN_PLAN_ROWS=1` puts every one of them back on the plan for a bisect.
The wasm manifest is byte-identical after each of the five commits.

**What `own.rs` still owes, after this slice.** The list in the close-out
above stands, less the five tables flipped here. `own.rs` can go only after:
`frees_boxes`'s rule is stated on an ownership the table does not decide (the
six sites above); the native emitter reads `core::facts` for the same ten
rows, which `compiler/vyrn-codegen/src/lib.rs` reads through `gen_stmt`,
`gen_expr`, `gen_call`, `gen_arm_body` and `gen_match_enum`; the interpreter
reads it for the arm table; RFC-0091 M2's rewritten store statements are
keyed, or the rewrite runs where the core can see it; and the answers that
have no kernel equivalent at all — `DropKind` and a declared release's
ordering, `Leak::Hole` and `Leak::Region`, the binding notes behind `vyrn why
--memory`, the `FreeArr` handover, `receiver_malloc`'s region stand-down — are
stated somewhere else.

**What the native emitter and the interpreter still need.** Neither moved in
this slice. `compiler/vyrn-codegen/src/lib.rs` reads the same tables through
`gen_stmt`, `gen_expr`, `gen_call`, `gen_arm_body` and `gen_match_enum`, and
the interpreter reads `arm_frees` and the placed rows. So `own.rs` can go
only after: the seven keyless tables above are keyed in the core and flipped
in both compiled backends; `frees_boxes`'s rule is stated in the core; the
interpreter reads `core::facts` for the arm table; and the answers the
close-out above lists as having no kernel equivalent at all — `DropKind` and
a declared release's ordering, `Leak::Hole` and `Leak::Region`, the binding
notes behind `vyrn why --memory`, the `FreeArr` handover — are stated
somewhere else.

**The census (2026-09-04): every read of the plan in the NATIVE emitter, and
whether the core says the same thing.** The slice after this one deletes
`own.rs`'s per-node tables, and it may delete only what something else states.
The direct emitter's census above did that job for `direct.rs`; this one does
it for `compiler/vyrn-codegen/src/lib.rs`, which reads the same tables through
`gen_stmt`, `gen_expr`, `gen_call`, `gen_match`, `gen_arm_body` and
`gen_match_enum`. Every `self.plan.*` in that file is one row below, with the
function that reads it, what the read decides, and whether the core carries the
same fact at the same key. The last column is `coretables.rs`'s, not an eye's.

| # | reader | table | what it decides | the core |
|---|---|---|---|---|
| 01 | `Gen::gen_stmt`, `Stmt::Assign`, the `String` append spine | `store_owned` | whether the buffer the append copied out of is this path's to free | **carried** |
| 02 | `Gen::gen_stmt`, `Stmt::Assign`, a store to a name | `store_owned` | whether the store releases the value it displaces | **carried** |
| 03 | `Gen::gen_stmt`, `Stmt::Assign`, a store to a name | `store_fresh` | whether a mention of the place in the value can hand the old buffer back | **carried**, inside 02's one answer |
| 04 | `Gen::gen_stmt`, `Stmt::SetField` | `store_owned` | the same, through a field | **carried** |
| 05 | `Gen::gen_stmt`, `Stmt::IndexSet`, the `Array` and `SmallArray` arms | `store_owned` | the same, through an element | **carried** |
| 06 | `Gen::gen_stmt`, `Stmt::IndexSet`, the `ArrayN` and `Map` arms, and a projection's store | `store_owned` | acknowledged only, so §26's finish check counts the row | acknowledged only |
| 07 | `Gen::gen_stmt`, `Stmt::Expr` | `discarded_results` | free an owned result nothing binds, rather than dropping it | **carried** |
| 08 | `Gen::gen_expr` | `arg_drops` | tee this node's value and free it after the call or operator above | **carried** |
| 09 | `Gen::gen_expr_inner`, `Expr::Field` — `byteLength`, `length`, a record field | `receiver_frees` | free the unnamed receiver right after the read | **carried** |
| 09b | `Gen::gen_expr_inner`, `Expr::Field` | `receiver_malloc` | whether that free stands inside a `region` — a callee allocation is malloc-side | the region stand-down, not modelled |
| 10 | `Gen::gen_expr_inner`, `Expr::Field` | `receiver_holes` | the field the read took, which the free walks around | **carried** |
| 11 | `Gen::gen_match`; `Gen::gen_if_expr` | `edge_releases` | the release one edge of a join owes because another edge took the name | **carried**, by join and edge |
| 12 | `Gen::gen_match_body_boxed`, `Gen::gen_arm_body`, `Gen::gen_match_enum` | `arm_frees` | the payload binders this arm releases, the holes each has, and the KIND each is released with | the binders and the holes **carried**; the kind is the TYPE's |
| 13 | `Gen::gen_match` | `consuming_matches` | whether the arms free the boxes their binders came out of | a different rule |
| 14 | `Gen::register_drop` | `malloc_scrutinees` | whether a binding's release stands inside a `region` | the region stand-down, not modelled |
| 15 | `Gen::lower_body`, `Gen::emit_releases` (through `own::placed`) | `releases`, `droppable`, `early`, `holes` | which bindings get a release slot, with what kind and what holes, and the order and exits they run at | `St::Row`, keyed the same; a name's `owned` and `holes` |
| 16 | `compile_inner` | `unconsumed` | §26's finish check: a planned row no query hit | the audit's, not a placement read |
| 17 | `Gen::gen_call_inner`, the `fromJson` rewrite | `alias_clones`, `alias_scope`, `alias_unwind` | the node a projection's expansion stands for | not a placement table |

Three readings of the table, and the first is the point of the slice.

- **The native emitter's needs are the direct emitter's, one column apart.**
  Rows 01 to 12 are the same ten decisions, at the same keys, that the two
  slices above flipped in `direct.rs` — so nothing new has to be stated in the
  core, and the flip is a reader change alone. `core::Site` already keys the
  store statements, the discarded result, the argument temporary and the edge
  release; `NameInfo::receiver`, `NameInfo::holes` and `Arm::frees` already
  name the receiver and the arm.
- **One column IS the native emitter's alone: the release kind.** `direct.rs`
  derived an arm payload's release shape from the binder's TYPE all along
  (`rel_for`), so the plan's stored `DropKind` was never its reader; `lib.rs`
  emitted the row's own `kind`. The core states no kind, and it should not: a
  kind is a property of the type, not of the site, and the placer itself
  derives the plan's kinds from `Owned::release_kind`. So the flip asks that
  table. `coretables.rs`'s `arm_kinds` walks every arm the core frees a binder
  in — 594 over the corpus — and asserts the type's answer equals the plan's
  row, so the column is asserted rather than argued.
- **Two rows are a region stand-down, and one is not a placement table.** Rows
  09b and 14 (`receiver_malloc`, `malloc_scrutinees`) ask whether a value
  inside a `region` came from the arena or from a callee's `malloc`, which the
  core does not model — a `region` lowers as an ordinary block. Row 17's alias
  scope is the `fromJson` rewrite's own bookkeeping. Row 13 is
  `consuming_matches`, measured in the slice above and left with the plan for
  the reason recorded there: its rule rests on an ownership the decision itself
  changes.

**The deletion slice (2026-09-04): the native emitter reads the core, and four
of `own.rs`'s tables lose their last reader.** The census above, with the
column this slice moved. Rows 13 to 17 are unchanged.

| # | table | the core, before | the native emitter, now |
|---|---|---|---|
| 01 | `store_owned`, the append spine | carried | **flipped** (`Gen::store_row`) |
| 02 | `store_owned`, a store to a name | carried | **flipped** (`Gen::store_fact`, the guards in the fallback) |
| 03 | `store_fresh` | carried, inside 02's one answer | **flipped**, with 02 |
| 04 | `store_owned`, `SetField` | carried | **flipped** |
| 05 | `store_owned`, the `Array` and `SmallArray` element stores | carried | **flipped** |
| 06 | `store_owned`, the `ArrayN` and `Map` arms and a projection's store | acknowledged only | acknowledged only, as before |
| 07 | `discarded_results` | carried | **flipped** (`Gen::discarded_row`) |
| 08 | `arg_drops` | carried | **flipped** (`Gen::arg_drop_row`) |
| 09 | `receiver_frees` | carried | **flipped** (`Gen::receiver_row`) |
| 09b | `receiver_malloc` | the region stand-down, not modelled | unchanged |
| 10 | `receiver_holes` | carried | **flipped**, inside 09's one row |
| 11 | `edge_releases` | carried, by join and edge | **flipped** (`Gen::edge_rows`) |
| 12 | `arm_frees` | the binders and the holes carried | **flipped** (`Gen::arm_row`); the kind is `Gen::rel_kind`'s |
| 13 | `consuming_matches` | a different rule | unchanged |
| 14 | `malloc_scrutinees` | the region stand-down, not modelled | unchanged |

Three things the slice needed, and all three were already there.

- **The core needed nothing new.** The native emitter's decisions are the
  direct emitter's, at the same keys: `core::Site` on `St::Store`, on a
  discarded result's `St::Drop`, on an edge release's `Site::Edge`, and
  `NameInfo::arg_drop`, `NameInfo::receiver`, `NameInfo::holes` and
  `Arm::frees` for the rest. So this slice is a reader change alone, and its
  six helpers on `Gen` are `Cx`'s six spelled for the textual backend.
- **The release kind comes off the type, and the test says so.** `lib.rs`
  emitted the arm row's stored `DropKind`; the core states none, because a
  kind is a property of the type and not of the site. It asks
  `Gen::rel_kind` — `Owned::release_kind` under this instantiation's
  substitution, which is where the placer itself took the plan's kinds from.
  `coretables.rs`'s `arm_kinds` walks the 594 arm binders the core frees over
  the corpus and asserts the type's answer equals the plan's row at every one.
- **The fallback rule held, unchanged.** Where the core answers, its answer
  stands; where it does not, the site reads exactly what it read BEFORE the
  flip. That is why a store to a NAME asks `store_fact` and falls back to the
  row AND its two guards, while `SetField` and an element store ask
  `store_row` and fall back to the row alone — those two never had the
  guards. The region gate stays the emitter's at every one of them: whether
  an arena's memory is this path's to free is a property of where the code
  stands, not of the store.

**What the native emitter no longer reads.** `receiver_frees`,
`receiver_holes`, `arm_frees` at every `match`, `store_owned` and
`store_fresh` at every store but the three acknowledged idioms and a user
container's element store, `discarded_results`, `arg_drops` and
`edge_releases`. What it still reads: `consuming_matches` (row 13),
`receiver_malloc` and `malloc_scrutinees` (the two region stand-downs),
`arm_frees` at an `if let` or a `?`, `store_owned` at the rewritten
statements, the `fromJson` rewrite's alias scope, and `releases`,
`droppable`, `early` and `holes` to build a frame. It still ACKNOWLEDGES
every row it took off the core, so §26's finish check keeps counting the
plan's decisions. `VYRN_PLAN_ROWS=1` puts every one of them back on the plan
for a bisect. The wasm manifest is byte-identical after each of the six
commits, which is the gate this slice needed most: a change to the textual
backend that moves a wasm byte touched the wrong thing.

**Which `own.rs` tables now have NO reader, and what deleting them takes
away.** A reader is an emission site or the interpreter. The
`VYRN_PLAN_ROWS=1` fallback inside a reader helper is the bisect knob, and it
goes with the table; `coretables.rs` diffs the table against the core and goes
with it too. Line counts are of `compiler/vyrn-frontend/src/own.rs` at this
commit.

| table | where its lines are | lines |
|---|---|---|
| `edge_releases` | the field (5), `edge_releases_at` (9), `fold_edge_releases` (63), its call (1), the owners loop (5), its `unconsumed` class (1) | **84** |
| `arg_drops` | the field (4), `arg_drop` (10), the build in `analyze` (6), its `unconsumed` class (1), and `Ownership::arg_drops`, which nothing has called for some time (26) | **47** |
| `receiver_frees` and `receiver_holes` | the two fields (10), `receiver_free` (9), `receiver_holes_at` (8), its `unconsumed` class (1) | **28** |
| `discarded_results` | the field (4), `discarded_result` (5), the build in `analyze` (2) | **11** |

170 lines, and one caveat: R1′'s 60-line fold in `analyze` cannot go with the
28 above, because `receiver_malloc` is derived from `receiver_frees` and the
region stand-down still has a reader in both backends. The fold and the
11-line owners loop go the day that stand-down is stated somewhere else, which
takes the four tables to 241 lines.

**What still has a reader, and who it is.** `arm_frees` is read by the
INTERPRETER (`interp.rs`, one lookup per arm), which this slice did not move,
so the table stays although both compiled backends are off it.
`store_owned` and `store_fresh` are read at the three acknowledged idioms and
at the twelve store statements RFC-0091 M2's `place at` rewrite builds.
`consuming_matches`, `malloc_scrutinees` and `receiver_malloc` are the three
the census above measured and left. `releases`, `droppable`, `early` and
`holes` build a frame in all three engines. So `own.rs` can go after: the
interpreter reads `core::facts` for the arm table; `frees_boxes`'s rule is
stated on an ownership the table does not decide; the `place at` rewrite runs
where the core can see it; the two region stand-downs are stated; and the
answers the close-out above lists as having no kernel equivalent at all —
`DropKind` and a declared release's ordering, `Leak::Hole` and `Leak::Region`,
the binding notes behind `vyrn why --memory`, the `FreeArr` handover — are
stated somewhere else.

**The deletion slice for the direct emitter and the interpreter (2026-09-04).**
The rows the two slices above left with the plan are closed here, and the two
that stay say what a reader of `own.rs` still needs. Every row's final verdict
for these two engines:

| # | table | the verdict |
|---|---|---|
| 01–05 | `store_owned`, `store_fresh` | carried, flipped in the slice above |
| 06 | `store_owned` at a map entry | **no reader**: acknowledged only |
| 07 | `store_owned` at `a[i].f = v` | **no reader**: acknowledged only |
| 08 | `discarded_results` | carried, flipped in the slice above |
| 09 | `arg_drops` | carried, flipped in the slice above |
| 10 | `edge_releases` | carried, flipped in the slice above |
| 11 | `receiver_frees` | carried, flipped two slices above |
| 11b | `receiver_malloc` | **carried and flipped here** |
| 12 | `receiver_holes` | carried, flipped two slices above |
| 13 | `arm_frees` at a `match` | carried two slices above; the interpreter reads it here |
| 14 | `consuming_matches` | **carried and flipped here** |
| — | `store_owned` at RFC-0091 M2's rewrite | **stays with the plan**, twelve rows |

**Rows 06 and 07 were never read.** The census listed both as "acknowledged
only", and that is the whole of them: `Fn_::stmt`'s map arm decides the entry's
release from `map_set`'s own two questions, and `Fn_::elem_field_store` emits
nothing for a heapless element. Each calls `store_owned_at` and throws the
answer away, so §26's finish check does not count the row as a decision nobody
looked at. Nothing to flip; the calls go with the acknowledgement when the
tables go.

**Row 14, and the ownership the rule needed.** The previous slice measured six
sites where the plan called a `match` consuming and the core did not, each one
`let s = match o { .. }` on an owned local, and each a payload box the flipped
emitter would have stopped freeing. The reason was named there: round
twenty-seven's table is what makes `o`'s note `Leak::Aliased`, this pass read
that note as "never owned", and the rule then answered "not taken" — resting on
the decision it feeds. **An alias is a handover, not a loss of ownership.** The
binding owns its value where it is bound; the alias takes it, and this core
already models both takes — `let t = s` is a take of `s`, and a consuming
construct takes its scrutinee into a temporary. So where a construct the plan
calls consuming names such a binding, the core states the ownership and the
take (`core::Builder::own_the_scrutinee`), and `frees_boxes` reads
`St::Switch`'s `consuming` (`direct.rs` `Cx::match_consumes`). The pin moved
from a count to a diff: `coretables.rs` fails on a site the plan calls
consuming and the core does not, and there is none.

The statement is made AT THE TAKE and nowhere else, which is what keeps the
placement still. Read as a blanket rule — every `Leak::Aliased` binding owned —
the kernel finds fourteen more Rule N edges owed across ten corpus programs
(`attrKey`'s `found` in five, `rpcApplyConfig`'s `p` and `t`, `gqlScalar`'s
`folded`, `gqlSplitDecl`'s `head`, `gqlVariantsOf`'s `body`, `storage`'s
`fallback`), because an owned name nothing takes is a release the placer adds
where the plan places none. Those fourteen are a class of their own and are not
this slice's; recorded so the next reader does not re-find them.

The six boxes, proved: the residue ratchet is green with `jsonplace`,
`matchown` and `refutablelet` all `clean` (the native build under
`VYRN_LEAK_CHECK=1`, where a double free exits 134 and a leak 135), and each of
the three prints the same bytes under `vyrn run --engine wasm` as under the
interpreter. One program's wasm moved, and it is the one place the flip changed
a decision rather than restating it: `refutablelet.vyrn` gains one `FreeStr` of
`tag` at the end of `main`. `let Tagged(tag, n) = local` is RFC-0121's
refutable let over a consumed local, so the payload is the binder's; the plan
releases nothing for `local`, which is `Aliased`, and nothing released the
payload either. Here it is a string literal, so the free is a no-op the audit
does not see; over a heap payload it is the leak the rule closes. The manifest
records that one line and nothing else.

**Row 11b, the region stand-down, is the core's now.** The rule is round
fifty-seven's: a CALLEE allocated the block, so it is malloc-side whatever
`region` is open at the call site, while the `@`-spelled producers route
through the arena lexically. The core states it where the producer is known —
the receiver expression is a call whose name does not start with `@` — and
carries it beside the receiver (`NameInfo::receiver_malloc`,
`Facts::receiver_malloc`). The region DEPTH stays the emitter's, as it does at
a store, because this pass lowers a `region` as an ordinary block. The diff is
pinned one way and counted the other: a plan row the core loses would stop a
free inside a `region` and fails; the core answering yes where the plan does
not is counted, and the count is one — `gqlParseQuery(query).sels` in
`gqlTestProject`, whose producer the analysis spells `@fieldof:gqlParseQuery`
and screens out with the arena's own `@` names, though a callee allocated it.
The emitter is at region depth zero there, so no byte moves; the core's answer
is the rule the table means.

**The interpreter reads round forty off the core.** It could not call
`vyrn_lower::core::facts()`: `vyrn-frontend` is the crate BELOW the lowering,
which is why the placer is installed rather than called. So the core hands this
one answer down the same way — `own::install_arm_rows`, filled by
`vyrn_lower::install`, answering from the core's own fold. The slot is not a
placement table: it holds no rows and goes when `ReleasePlan` does. One thing
the flip needed: an arm row names a release KIND, and the interpreter has no
type of its own to ask, so `Facts::arms` carries `(binder, holes, kind)` with
the kind read off the binder's type through the same `Owned` table the analysis
decided with. `coretables.rs` diffs the kind too. The fallback rule is the
slice's own: an `if let` or a `?` states no answer, and the site reads the plan
exactly as before.

**RFC-0091 M2's rewritten stores stay with the plan, and here is precisely what
it would take.** The rewrite CAN be keyed: `project::store_index` memoizes its
expansion on the index node (`project::stored`), so the checker, both emitters
and this pass all get the same block at the same statement addresses. What it
cannot do is JUDGE it. Lowering the block here takes the residue to zero —
55,486 store sites stated, none left to the plan — and refuses one function:
`main` in `std/slots`, "`people[]` may not be stored into a store — it is read
out of `people[..]`, a place that owns it". The rewrite's last statement is a
write-back — `people.vals[][idx] = people[]`, the element read back into the
container it came out of — and the kernel's write-back exception matches an
alias's root and path against the store's place, which `people.vals[]` (a fresh
read of `people!.vals`) does not equal. Refusing `main` loses the two release
rows the placer had put at its return, so the emitted bytes would move for a
leak and not for a fix. The rule the row waits on is the one the census already
owes: `sinks`'s write-back exception, stated for a projection's expansion
rather than for a rebuilding builtin's receiver alone. Until then the twelve
rows are the plan's, and `coretables.rs` pins the count.

**What `own.rs` has no reader for, from these two engines.** After this slice
the direct wasm emitter and the interpreter read no per-node placement table
except `store_owned` and `store_fresh` at RFC-0091 M2's twelve rewritten
statements, and `arm_frees` at an `if let` or a `?`. Named:

- `consuming_matches` — no reader from these two engines. The core reads it
  (`Builder::scrutinee`, `own_the_scrutinee`), as the core reads the whole plan
  until M5 and M6 take that away; no emitter does.
- `receiver_malloc` — no reader from these two engines.
- `receiver_frees`, `receiver_holes` — no reader from these two engines.
- `arm_frees` at a `match` — no reader from these two engines; at an `if let`
  or a `?` the plan is still read by both.
- `store_owned`, `store_fresh` at a map entry and at `a[i].f = v` — no reader,
  acknowledgement only.
- `discarded_results`, `arg_drops`, `edge_releases`, and `store_owned` and
  `store_fresh` everywhere but the twelve — no reader, from the slice above.

The union with the native emitter's list is what the deletion slice removes.
Everything else the close-out named is unchanged: `DropKind` and a declared
release's ordering, `Leak::Hole` and `Leak::Region`, the binding notes behind
`vyrn why --memory`, the `FreeArr` handover, and the argument verdicts over the
call graph.

**The checker's deletion path (2026-09-04): the structural census of
`movecheck.rs`.** The census above is rule by rule. This one is line by line,
and it answers the question the deletion needs answered: how many lines is the
deletion worth, and what stands in its way. Every section of the file — an
item and everything under it up to the next section — carries one kind:

- **a rule the kernel now gives**: the rule-by-rule census says `the same`
  for it, so the sentence a reader gets does not change the day the checker
  goes;
- **a rule only the checker gives**: the census says `nothing` or `its own
  words`, so nothing may take it yet;
- **placement rows for the engines**: what `own.rs` reads. It is not a rule,
  and the kernel does not replace it — the own-side track above does;
- **a fix menu**: surface knowledge the kernel has no source for;
- **shared machinery**: the walk itself, the scope stacks, the path algebra,
  the entry points, the instruments.

`compiler/vyrn-cli/tests/refusals.rs` holds the classification and computes
the spans, so the numbers below are asserted rather than transcribed: the
sections must tile the file with no gap and no overlap, each anchor must name
exactly one line, and the totals per kind must be these
(`the_structural_census_covers_the_file`,
`the_structural_census_is_what_the_rfc_records`). The table is printed from
the same rows (`cargo test -p vyrn-cli --test refusals -- --ignored
--nocapture the_structural_census_as_a_table`).

| kind | lines | share |
|---|---|---|
| a rule the kernel now gives | 1,045 | 10 per cent |
| a rule only the checker gives | 723 | 7 per cent |
| placement rows for the engines | 2,349 | 23 per cent |
| a fix menu | 81 | 1 per cent |
| shared machinery | 3,656 | 37 per cent |
| tests | 2,169 | 22 per cent |
| **the file** | **10,023** | |

| section | lines | kind | what it is |
|---|---|---|---|
| `pub struct OwningSite` | 129 | shared machinery | the module's own statement of the rules, and the two recorded measurements (RFC-0089 rule 1's sites, RFC-0092's projections) |
| `pub enum Gone` | 134 | placement rows for the engines | why a binding does not hold its value at its block's end, and the row `own.rs` reads it from |
| `pub enum ArgVerdict` | 24 | placement rows for the engines | what a callee does with the temporary at a call-argument position |
| `pub struct ExitEv` | 218 | placement rows for the engines | the event records: exits, reads, consuming matches, arm payloads, stores, Rule N edges, place stores |
| `pub fn facts(program: &Program) -> Facts` | 139 | placement rows for the engines | the two facts out of one walk, and the lender and retention post-passes over them |
| `enum Want` | 35 | shared machinery | what a run is for, and one run's outputs |
| `fn arg_verdict` | 94 | placement rows for the engines | the verdict for one argument temporary, read at a position instead of at a binding |
| `fn let_id(s: &Stmt) -> usize` | 52 | placement rows for the engines | the key of a `let`, the lending builtins, and the projection names |
| `pub fn check_accum(program: &Program) -> Vec<Diagnostic>` | 27 | shared machinery | the entry points a caller uses |
| `fn run(program: &Program, want: Want) -> Run` | 348 | shared machinery | the one walk: the capability tables, every body, the drains and the stamps |
| `pub fn check(program: &Program) -> Result<(), String>` | 10 | shared machinery | the historical string shim |
| `struct MoveCheck<'a>` | 153 | shared machinery | the pass's state: the scope stacks, the sinks, the recorded rows |
| `enum Borrow` | 48 | a rule the kernel now gives | what a borrow is, in words — `core::BorrowKind::what` is this sentence |
| `fn fixes(&self, root: &str, path: &str) -> Vec<String>` | 24 | a fix menu | the named ways out of a borrow error |
| `enum TakeForm` | 18 | a rule the kernel now gives | which form wrote the `consume`, and how a refusal names it |
| `fn nothing_to_take(self) -> String` | 13 | a rule the kernel now gives | `consume` with nothing to take (row 09) |
| `fn drop_it(self) -> String` | 8 | a fix menu | the `drop` a take's menu offers |
| `fn root_of(path: &str) -> &str` | 211 | shared machinery | the path algebra and the consumed table: overlap, reach, revival |
| `impl MoveCheck<'_>` | 89 | shared machinery | one body, with its parameters and its return type |
| `fn enter(&self)` | 35 | shared machinery | the three scope stacks, read as one environment |
| `fn wrote_place(&self, path: &str, line: usize, consumed: &mut Consumed)` | 37 | a rule the kernel now gives | a write to a place ends every alias that reads out of it (row 05) |
| `fn place_key(&self, e: &Expr) -> usize` | 20 | placement rows for the engines | the key a row is written under |
| `fn note_temporary(&self, s: &Stmt, value: &Expr) -> usize` | 479 | placement rows for the engines | the recording: temporaries, store events, branches, reads, exits, takes, holes, place stores, hand-overs at a `return` |
| `fn is_bound_name(&self, e: &Expr) -> bool` | 18 | placement rows for the engines | whether a `let` names storage somebody else owns, for reclamation |
| `fn names_a_place(&self, value: &Expr) -> Option<&'static str>` | 76 | a rule the kernel now gives | whether a value reads a place that owns it — the kernel's alias table |
| `fn fixes_here(&self, b: &Borrow, root: &str, path: &str) -> Vec<String>` | 36 | a fix menu | the ways out that exist in THIS function |
| `fn is_module_state(&self, name: &str) -> bool` | 72 | shared machinery | module state, the borrow table, and the type reading |
| `fn sinks(&self, name: &str, i: usize) -> bool` | 47 | a rule the kernel now gives | a rebuilding builtin takes its receiver, and the write-back statement excepted (row 26) |
| `fn store` | 113 | a rule the kernel now gives | rule 1's move and rule 2's refusal at a store (rows 01, 02, 03, 27, 34) |
| `fn borrow_from(&self, value: &Expr) -> Option<Borrow>` | 68 | a rule the kernel now gives | the borrow status a `let` of a value gives its binding |
| `fn payload_binding` | 60 | shared machinery | what a pattern's binders name, and whether an iterable is a place |
| `fn check_use(&self, path: &str, line: usize, consumed: &Consumed) -> Result<(), Diagnostic>` | 79 | a rule the kernel now gives | rule 1 asked of a path: is the storage still all there (rows 04, 06, 07) |
| `fn check_take` | 57 | a rule the kernel now gives | a take's refusals: an element, and nothing to take — `core::take_prefix` states both (rows 08, 09) |
| `fn check_handover(&self, arg: &Expr, callee: &str, line: usize) -> Result<(), Diagnostic>` | 123 | a rule the kernel now gives | rule 2 at the third exit: a borrow may not be consumed (rows 11, 12, 13, 14) |
| `fn refuse_projected_arg` | 34 | a rule the kernel now gives | the refusal a projected argument to a `consume` parameter gets |
| `fn arm_binder(&self, name: &str) -> bool` | 24 | shared machinery | an arm's binders, and whether a callee keeps a `fn` value |
| `fn check_return(&self, e: &Expr, line: usize) -> Result<(), Diagnostic>` | 132 | a rule the kernel now gives | rule 3: a return is owned (rows 15, 16, 18, 19, 28) |
| `fn refuse_return(&self, b: &Borrow, root: &str, path: &str, line: usize) -> Diagnostic` | 47 | a rule the kernel now gives | the one exit every returned borrow leaves by, the exported function's own sentence with it (row 17) |
| `fn note_handover(&self, arg: &Expr, callee: &str, i: usize, line: usize)` | 27 | placement rows for the engines | the retention and hand-over records the call graph is closed over |
| `fn note_arg_temp(&self, arg: &Expr, callee: &str, ix: usize, line: usize)` | 527 | placement rows for the engines | the argument-temporary row: its producer, its type and its release kind |
| `fn ctor_valued(&self, e: &Expr) -> bool` | 57 | placement rows for the engines | what an expression builds: a variant, a String, a concatenation |
| `fn note_arm_aliases(&self, e: &Expr, line: usize, binders: &[String])` | 84 | placement rows for the engines | an arm that yields a place, and what naming one costs |
| `fn value_cannot_alias(&self, e: &Expr, root: &str) -> bool` | 119 | placement rows for the engines | Rule N's edge guard, the mention guard, and what a call may forward |
| `fn carries_param_storage(&self, e: &Expr) -> bool` | 171 | placement rows for the engines | the escape screen: storage flow rather than mention |
| `fn lends(&self)` | 34 | placement rows for the engines | the lending record, and the lend a wrapper hides |
| `fn returned_borrow(&self, e: &Expr) -> Option<(Borrow, String, String)>` | 54 | a rule the kernel now gives | the first borrow a returned expression yields |
| `fn note_returned_projection(&self, e: &Expr, line: usize)` | 59 | shared machinery | RFC-0092's instrument |
| `fn lends_through_a_wrapper(&self, e: &Expr) -> Option<(Borrow, String, String)>` | 77 | placement rows for the engines | the same question through a constructor, to record a lend and never to refuse one |
| `fn site(&self, kind: &'static str, line: usize, e: &Expr, declared: Option<&Type>)` | 40 | shared machinery | RFC-0089 rule 1's instrument |
| `fn block(&self, b: &Block, consumed: &mut Consumed, scope: &mut Vec<HashSet<String>>) -> bool` | 32 | shared machinery | a block, and whether it diverges |
| `fn stmt` | 890 | shared machinery | the walk over statements: it calls the refusal helpers and writes the plan's rows in the same arm |
| `fn capture_site(&self, name: &str, line: usize)` | 75 | placement rows for the engines | a lambda's captures, recorded for the enclosing block |
| `fn check_exclusive(&self, callee: &str, args: &[Expr], line: usize) -> Result<(), Diagnostic>` | 35 | a rule only the checker gives | a `modify` borrow is exclusive (row 23) |
| `fn check_capture(&self, name: &str, line: usize) -> Result<(), Diagnostic>` | 43 | a rule only the checker gives | a closure that outlives the call may not capture a borrow (row 24) |
| `fn check_loop_reuse` | 39 | a rule the kernel now gives | rule 1 across a back edge (row 25) |
| `fn expr` | 1,039 | shared machinery | the walk over expressions: the same traversal does both jobs |
| `fn reject_consume_global` | 36 | a rule the kernel now gives | module state may not be taken (rows 10, 12, 15, 29) |
| `pub fn mentions_place(e: &Expr, base: &str) -> bool` | 95 | shared machinery | whether a stored value mentions the place it is stored into |
| `mod linear` | 645 | a rule only the checker gives | the must-use obligation: acquired once, disposed exactly once (rows 30, 31) |
| `fn store_path(e: &Expr) -> Option<String>` | 27 | shared machinery | the place an expression names, as the store arms spell it |
| `fn sinks(decl: &Declared, name: &str, i: usize) -> bool` | 24 | a rule the kernel now gives | whether a builtin's parameter takes its argument for good |
| `fn reads(e: &Expr) -> Vec<String>` | 141 | shared machinery | the names an expression reads, and the calls in it |
| `pub fn element_path(e: &Expr) -> Option<(String, String)>` | 84 | shared machinery | the place spellings every rule above compares |
| `fn menu(line: usize, message: String, fixes: Vec<String>) -> Diagnostic` | 13 | a fix menu | one diagnostic with its menu of fixes |
| `fn declared_in(block: &crate::ast::Block, out: &mut std::collections::HashSet<String>)` | 56 | shared machinery | the names a block declares, and a pattern's binders |
| `mod tests` | 2,169 | tests | the pass's own unit tests |

Four things the structural census says, and the third is the finding.

- **The rules are a sixth of the file.** After this slice's three rule
  commits, 1,045 lines state a rule the kernel gives and 723 state one only
  the checker gives. Together they are 1,768 lines out of 10,023. Every
  argument about which rule closes next is an argument about a sixth of
  `movecheck.rs`, and about two lines in five of that sixth.
- **The placement rows are more than twice the rules.** 2,349 lines write
  what `own.rs` reads. They are not a second opinion about a rule, so no
  kernel refusal takes them; the own-side track above does, table by table.
  `note_arg_temp` alone is 527 lines and `note_temporary` and the recorders
  under it are 479, which together are more than every rule the checker
  states on its own.
- **The obstacle is the walk, not the open rules.** `stmt` (890 lines) and
  `expr` (1,039) are one traversal doing both jobs: an arm calls
  `check_handover` and writes an `ArgTemp` row in the same breath. So a
  closed rule frees its own helper and not the arm that calls it, and the
  file cannot lose a fifth of itself until the recording has a walk of its
  own or the plan has gone. That is 1,929 lines, and it is the largest single
  entry in the census.
- **The fix menus are 81 lines.** They are named in the close-out as a thing
  the kernel does not have, and they are the smallest of everything the
  deletion track owes.

**The rules this slice closed.** Each is one commit, and each moves a row of
the rule-by-rule census from `nothing` or `its own words` to `the same`: the
kernel prints the checker's whole sentence at the same file and line, minus
the menu. `tests/refusals.rs` runs both passes over every row, so a wording
that drifts fails the build rather than the reading.

- **`consume` with nothing to take, and `consume` of an element** (rows 09
  and 08). The census left these open with the reason written down: both are
  about the KEYWORD and not about ownership, because `consume make()` and
  `make()` denote the same value and the kernel has no keywords. So the rule
  belongs where the desugar is written, which is the core
  (`core::take_prefix`). A `consume` whose operand names no place is refused
  there, and an element is named as the element it is. The core states it as
  a REFUSAL rather than a gap: `core::Gap` carries a `rule` now — a sentence
  the program broke, not a construct the slice cannot lower — and the placer
  reports one the way it reports the kernel's own. No corpus program reaches
  either refusal, because the checker refuses every such program before the
  core is built; the corpus tally counts a rule-gap as refused, so one that
  got through would move the ratchet rather than hide in the gap column.


- **An exported function owns its result** (row 17). The kernel refused the
  program already, in the general sentence: "it is a `read` parameter, and a
  return is owned". True, and it names the wrong reason — the reason is that
  the caller is JS and `wasi-min.js` releases every String an export hands
  back (RFC-0012 M2, RFC-0089 M3b), so `.copy()` is the only way out and
  `consume` is not one. The core carries `Body::export` now, a lambda frame
  carries the flag of the body that holds it, and the kernel words the
  return refusal as `movecheck::refuse_return` words it. One field and one
  branch.


- **The `sinks` write-back exception** (row 26). The close-out recorded this
  as "the core restates the first half and reads the plan for the second".
  Neither half is the plan's now, and neither is restated. The rule under it
  — which builtin hands its receiver's buffer back through its result — is
  `prelude::rebuilds`, one function that `movecheck::sinks` and `core::call`
  both read, so the predicate is written once instead of twice. The exception
  itself is the core's (`Rhs::Call::write_back`), and the kernel exempts that
  one take. The row was already `the same`; what moved is that a reader can
  now find the rule in one place, which is what the deletion needs.

- **Region escape** (rows 32, 33). Nothing is owed, and the census already
  proved it: both programs are refused with `VYRN_NO_MOVECHECK=1`, because
  `checker.rs`'s `region_store_guard` and `region_consume_guard` give them.
  The rule is not the move check's, so it does not stand in the deletion's
  way. The `Kernel::Elsewhere` rows assert this on every run.


**The rule-by-rule census, after this slice.** Printed from the test's own
rows (`cargo test -p vyrn-cli --test refusals -- --ignored --nocapture
the_census_as_a_table`), so the prose and the assertion cannot drift apart by
a transcription.

| # | rule | RFC | the checker's sentence | the kernel |
|---|---|---|---|---|
| 01 | rule 2: an element read may not be stored | RFC-0092 | `b.xs[0]` may not be stored into `push(..)` — it is read out of a place that owns it | its own words |
| 02 | rule 2: a field of a `read` parameter may not be stored | RFC-0089 | `h.meta[0]` may not be stored into `push(..)` — it is a `read` parameter | its own words |
| 03 | rule 2: a projection is a borrow of its root, whatever the root is | RFC-0092 | `d.title` may not be stored into `push(..)` — it is read out of a place that owns it | its own words |
| 04 | a name with a hole may not be used whole | RFC-0093 | `p.name` was taken out of `p` here / line 10: ... and `p` is used as a whole here, with the hole still in it | the same |
| 05 | a write to a place ends every alias that reads out of it | RFC-0090 | `t.xs[..]` is written here while `before` still reads out of it / line 9: ... and `before` is used again here | the same |
| 06 | rule 1: a `consume` parameter takes ownership | RFC-0089 | `x` is used here but was already consumed by `take(..)` on line 8 /   (a `consume` parameter takes ownership; the value can't be used afterward) | the same |
| 07 | rule 1: a move into a binding, and a use of the source after it | RFC-0089 | `s` was moved here into the binding `t` / line 5: ... and `s` is used again here | the same |
| 08 | `consume` reaches a field, never an element | RFC-0093 | `xs[0]` may not be taken — an element is not a place a take reaches | the same |
| 09 | `consume` with nothing to take | RFC-0093 | `consume` here has nothing to take — the value is already owned, so there is no place to leave a hole in | the same |
| 10 | module state may not be taken: a prefix `consume` | RFC-0013 | module state `names` may not be consumed by a take — nothing may take ownership of module state (it lives for the whole module and is never dropped) | its own words |
| 11 | rule 2: a prefix `consume` of a `read` parameter | RFC-0089 | `ys` may not be consumed — it is a `read` parameter | its own words |
| 12 | module state may not be taken: a `consume` parameter | RFC-0013 | module state `names` may not be passed to a `consume` parameter via `take(..)` — nothing may take ownership of module state (it lives for the whole module and is never dropped) | the same |
| 13 | rule 2: a whole `read` parameter to a `consume` parameter | RFC-0089 | `ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` parameter | the same |
| 14 | rule 2: a projection to a `consume` parameter | RFC-0092 | `d.title` may not be passed to a `consume` parameter via `take(..)` — it is read out of a place that owns it | its own words |
| 15 | module state may not be taken: a `return` | RFC-0013 | `names` may not be returned — it is module state, which nothing may take, and a return is owned | the same |
| 16 | rule 2 at the return: a field of a `read` parameter | RFC-0089 | `d.title` may not be returned — it is a `read` parameter, and a return is owned | its own words |
| 17 | an exported function owns its result | RFC-0012 | `s` may not be returned from an exported function — it is a `read` parameter, and the JS caller releases what it is handed | the same |
| 18 | rule 2 at the return: a whole `read` parameter | RFC-0089 | `ys` may not be returned — it is a `read` parameter, and a return is owned | the same |
| 19 | rule 2 through a wrapper: a `read` parameter put into the result | RFC-0089 | `s` may not be put into `Some(..)` — it is a `read` parameter | its own words |
| 20 | rule 1 at the drop: what a `consume` parameter took is gone | RFC-0089 | `a` is dropped here but was already consumed by `take(..)` on line 6 | its own words |
| 21 | rule 4 at the drop: the place that owns a value releases it | RFC-0089 | `owned` may not be dropped — it is read out of a place that owns it | its own words |
| 22 | `drop` releases the whole binding, and a take left a hole | RFC-0093 | `p` may not be dropped — `p.name` was taken out of it on line 17, and `drop` releases the whole binding | nothing |
| 23 | a `modify` borrow is exclusive | RFC-0090 | `a` is passed to `bump` as `modify` and read again in the same call — a `modify` borrow is exclusive | nothing |
| 24 | a closure that outlives the call may not capture a borrow | RFC-0037 | `s` may not be captured by a closure that outlives this call — it is a `read` parameter | nothing |
| 25 | rule 1 across a back edge | RFC-0089 | `x` is consumed by `take(..)` inside a loop, so it would be used again on the next iteration | the same |
| 26 | a rebuilding builtin takes its receiver | RFC-0125 | `mt` is read out of `h.meta` here — a place that owns it / line 7: ... and `push(..)` takes `mt`, so `mt` must be a value of its own | the same |
| 27 | rule 2: a `read` parameter to a builtin that declares `consume` | RFC-0089 | `xs` may not be stored into `fromArray(..)` — it is a `read` parameter | its own words |
| 28 | a closure's result is its caller's, and a capture is not its to give | RFC-0037 | `s` may not be returned from a closure — it is a captured binding, and the closure's result is its caller's | the same |
| 29 | module state may not be taken: `for .. in consume` | RFC-0013 | module state `names` may not be consumed by a `for` loop — nothing may take ownership of module state (it lives for the whole module and is never dropped) | its own words |
| 30 | a must-use obligation is discharged on every path | RFC-0075 | `s` is a `Stream` and is never disposed | nothing |
| 31 | a must-use obligation is discharged exactly once | RFC-0075 | `s` is a `Stream` and is disposed more than once | its own words |
| 32 | a value the region allocated may not be stored where it outlives the region | RFC-0004 §4 | cannot store a heap value into `kept`, which outlives the enclosing `region` (it would dangle when the region frees). Move `kept` inside the region, or compute a non-heap result to carry out. | not the move check's |
| 33 | a `consume` parameter may not take a value the region frees | RFC-0004 §4 | cannot hand a heap value to argument 1 of `take`, which is `consume`, inside a `region`. The region frees the value at its closing brace, so the callee cannot own it. Move the call out of the region, or pass a value that holds no heap. | not the move check's |
| 34 | rule 2: a `read` parameter into a builtin's `consume` argument | RFC-0089 | `s` may not be stored into `push(..)` — it is a `read` parameter | its own words |


**The retention over the call graph: measured, and there is nothing in it.**
The close-out owed the deletion track `ArgVerdict` and `note_handover` — "whether
a callee keeps what it is handed", which `arg_drops` is built from — and named
the kernel's fixpoint over the core as its natural home. The measurement says
no fixpoint is needed, because the sets are empty. `movecheck` keeps two
closures over the call graph: `lending`, the functions whose result the caller
must not release, and `retains`, the `(callee, index)` positions that KEEP a
borrowed parameter. `VYRN_LEND_DUMP=1 vyrn check <file>` prints both after the
fixpoint. Over 276 programs — every file under `examples/`, `std/`, `bench/`,
`site/` and `site/app/` — both are empty, and no run seeds either one.

The reason is a rule, and it is one the kernel now gives. A function retains a
borrowed parameter only by storing it, returning it or handing it to a
`consume` parameter, and rule 2 refuses all three (rows 11, 13, 17, 18, 19, 27,
34). A function lends its result only by returning a borrow, and rule 3 refuses
that (rows 15, 16, 18). So the two closures are the shadow of a rule that is
enforced: they were built when the rule was recorded rather than applied, and
nothing has filled them since. `arg_verdict`'s `Lent` and `retains` clauses,
`facts`'s lender post-pass and `fresh_stores`'s retention screen all read a set
that is always empty.

`movecheck::Facts` carries both sets now, and the kernel corpus tally asserts
they are empty over every corpus program — pinned rather than described,
because a program that fills one is the day the argument changes. What the
deletion track owes here is therefore a DELETION and not a kernel rule: the
next slice may take the two closures and the clauses that read them, on that
assertion. Nothing was added to the kernel, so `vyrn check site/export.vyrn`
did not move: 7.8 and 7.9 seconds warm before and after, against 32.9 seconds
cold.

**The `fix:` menus stay in the checker, and this is the decision.** They are 81
lines: `Borrow::fixes`, `TakeForm::drop_it`, `MoveCheck::fixes_here` and the
`menu` constructor. A menu is a function of two things — the rule that was
broken, and the surface spelling of the place it was broken at — and the kernel
has the second and not the first: it prints a sentence, not a rule name. Two
options were weighed.

- Give the kernel the menus. It would have to carry a rule identity beside
  every refusal, and then carry `.copy()`, `consume`, `swapRemove` and the
  write-back form, which are four surface spellings the kernel exists not to
  know. It would also put RFC-0087 U2's wording inside a pass whose whole
  claim is that it knows nothing about the surface.
- Keep them where the surface is. A refusal names a rule; a menu is a table
  from a rule to a suggestion; the table belongs beside the parser's own
  vocabulary. When the checker's rules go, the menus become a small pass over
  a kernel refusal, keyed by the rule the refusal names — one `match`, and no
  walk.

The second, and nothing is built for it this slice: `vyrn fix` reads the
diagnostics the checker already prints, and the checker still runs. The
`refusals` census asserts what this costs today — the kernel's sentence is the
checker's minus the menu, on every row that says `the same` — so the day the
checker goes, the menu is the only thing a reader loses and the pass that
restores it is written against a refusal that names its rule.

**What the census column says now.** Before this slice: 11 `the same`, 15 `its
own words`, 6 `nothing`, 2 `not the move check's`. After: **14, 14, 4 and 2**.
Rows 08, 09 and 17 moved to `the same`; the rest is unchanged, and the table
above is printed from the test.

**The rules still open, and why each is.**

- **Rows 01, 02, 03, 14, 16, 21 and 34** — a projection, an element read, a
  field of a `read` parameter. The kernel refuses every one, and in its own
  words: "read out of `d.title`, a place that owns it". True, and it names the
  PLACE where the checker names the CAPABILITY. Closing them is a wording
  change and not a rule: the alias would have to carry the kind of the
  parameter its root came from, so `h.meta[0]` can be called what the reader
  wrote rather than what the kernel sees.
- **Rows 10, 29** — module state, taken by a prefix `consume` or a `for` loop.
  The kernel refuses both with RFC-0013's sentence at the `consume` parameter
  form (rows 12, 15) and reaches a different form of the take here. Also a
  wording gap.
- **Rows 11, 19, 20, 27, 31** — the same shape once more: the kernel's sentence
  names the taker (`via take(..)`) where the checker names the form.
- **Row 22**, a `drop` after a hole, and **rows 30, 23, 24** — a must-use
  obligation on every path, a `modify` borrow's exclusivity, and a closure that
  outlives the call. These four are the rules the kernel gives NOTHING for.
  Row 24 is a lifetime question and §2.2 has no lifetimes on purpose. Row 23's
  own extension — what a `modify` argument does to the aliases of what it is
  handed — was measured when the alias rule was tried and refuses three corpus
  programs falsely, and the measurement above says the call-graph retention
  that would fix it is empty, so the rule needs a per-argument WRITE set rather
  than a retention set.
- **Rows 32, 33** — region escape. Nothing is owed: `checker.rs` gives them.

**How many lines of `movecheck.rs` have no caller after this slice: none, and
that is the right answer.** The slice deleted no rule, because the refusals
suite must stay byte-identical and the checker is still the pass that runs
first. What the slice produced is the number the deletion slice acts on: **1,045
lines now state a rule the kernel gives in the same sentence**, and the census
test names every one of them by section. Against them stand three things, in
this order of size:

1. **the walk**, 1,929 lines of `stmt` and `expr`, which calls the refusal
   helpers and writes the plan's rows in the same arm. Until the recording has
   a walk of its own, deleting a rule frees its helper and not its call site;
2. **723 lines of rules the kernel does not give in the same words**, of which
   the wording gaps above are most and the four real gaps (rows 22, 23, 24, 30)
   are `check_exclusive`, `check_capture` and `mod linear` — 723 lines, and
   `mod linear` alone is 645 of them;
3. **81 lines of menu**, which stay by the decision above and become a table.

The next slice's first move is therefore not another rule. It is to split the
walk: one traversal that refuses, one that records, so that a closed rule takes
its call site with it.

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

*Step 7 landed 2026-09-03 (`track-r`): the I/O family — `write_all` and
its stdout buffer, `getbyte` and its stdin buffer, `read_line`, `open_at`,
`read_all`, the two readers, the two writers, `rename_file`, `fsync_file`,
`args`, `env_get`, `err3`, `readdir_blob`, `list_dir` and the clock and seed
readers — is Vyrn in `std/runtime` (782 lines with its comment) over the
fifteen `wasi_snapshot_preview1` imports, which `std/mem` now declares beside
the raw-memory primitives with their witx signatures and the emitter lowers to
one `call` each (the plan's §2.2); the wasm emitter's twenty-one hand-emitted
bodies are deleted, 2,250 lines out of `direct.rs`. The generator's mediated
read is two more `std/mem` rows an ordinary build lowers as `unreachable`, and
the three readers have a generation twin the emitter calls at the builtin's
site. The wording stays `trap.rs`'s, interned by the emitter and passed as
arguments. Parity 41 of 41, residue green, the kernel and effect ratchets held;
`files`, `storage` and `clock` byte-identical to the base wasm under the fixed
clock and seed; the RFC-0103 census re-run with no row change. fasta and
reverse-complement under wasmtime, base and head interleaved, medians of five:
1.043 s against 1.012 s and 1.171 s against 1.164 s. Details under the plan's
§6 table.*

*Step 8 landed 2026-09-03 (`track-s`): `region { .. }` is a bump arena.
`regionEnter` and `regionExit` are Vyrn in `std/runtime`, `strNew` allocates
from the region's chunks while the emitter routes a `String` there, and the
wasm emitter's `region_keep`, `region_free` and `region_pop` with their three
64-word side tables are deleted, 261 lines out of `direct.rs`. A chunk is one
ordinary block holding the older chunk, its end and the region's allocations;
the closing brace frees the chunks, not the blocks, and keeps the oldest, so a
region in a loop asks the allocator nothing after its first turn. The ownership
test needed no new mechanism: a block the free list hands out carries its class
in the header, a block the arena hands out carries 0, and `free`'s class-range
refusal already declines it. The routing stays LEXICAL — the emitter marks
which allocation is the arena's — because the checker states that an `Array`
buffer is never the arena's, so the plan's shared-bump reading would reclaim a
global array grown inside a region under a live binding; that finding is on the
record in the plan's §6. Parity 41 of 41, residue green, the kernel ratchet at
0 refused. `census-regions.md` §5a's three shapes at forty million turns under
wasmtime 46, base and head interleaved, medians of five: one region per
iteration 0.908 s to 0.513 s with peak working set flat at 13 MB either way,
which puts it at the no-region line (0.463 s) for the first time; one region
around the loop 0.968 s to 0.692 s and 1,681 MB to 1,579 MB; no region
unchanged. The plan's §8 question 1 is answered there too: keep the syntax,
because a library `Arena` cannot route the allocations a program never names.
Details under the plan's §6 table.*

*Step 9 landed 2026-09-03 (`track-t`): the traps and the two renderers —
`trap`, `trap_idx`, `print_i64` and `bool_str` — are Vyrn in `std/runtime`
(`trapV`, `trapIdx`, `printI64`, `boolStr`, 93 lines) over the plan's §2.3
`trap` primitive and the `writeAll` step 7 put there, and the wasm emitter's
four bodies are deleted, 171 lines out of `direct.rs`. Neither renderer
allocates: one `digitsAt` writes an integer backwards into a 32-byte cell, so a
trap needs no heap to say why it trapped. The call-depth counter STAYS in the
prologue, which is this milestone's open question closed on a number: as a
`std/runtime` `enter`/`leave` pair, nbody at 25 M steps went from 2.155 s to
2.306 s and fannkuch at n = 11 from 3.599 s to 4.014 s under wasmtime 46,
medians of five — two calls per user call cost more than the counter M1 priced
at 0.25 s. Parity 41 of 41, residue green; every `error:` line in the corpus
byte-identical to the record, and `concurrency.vyrn` byte-identical, tasks
still eager in the module and the native pool still in the host (§2.8). What is
left hand-emitted in `direct.rs` is instruction sequences at their one site
each — the prologue's counter, M1's trap site, the `a[i]` check, the
`SmallArray` push, `_start`, the three trap-writing builtins, the type-aware
tests around a runtime call, a `String` header load and `std/mem`'s own
lowering — plus the three region functions, which are the plan's step 8. The
emitter's runtime section was 4,205 lines before step 0 and is 387. Details
under the plan's §6 table.*

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
  on record here and closed by the rule in "the take out of a `read`
  parameter" below: the alias is refused, and the test takes a copy.
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

#### The third slice (2026-09-03): the census of what only the interpreter does

M5 deletes the interpreter. This slice counts what goes with it. One row per
capability the interpreter alone provides today; the third column was proved by
running the compiled route over the same input, never by reading it; the fourth
column is a measurement or a named piece of work, never an estimate. Nothing is
deleted here, and `--engine interp` is still the default.

| capability | who needs it | the compiled route today | what moving it costs |
|---|---|---|---|
| `run-default` | every user; 34 of `vyrn-cli`'s test suites, at 150 `vyrn run` call sites; the fixture recorder | yes — 204 examples run under both engines, all byte-identical since the fourth slice below (203 of 204 when this row was written), the same 58 exit non-zero | flipping one default. the corpus costs 89.1 s under the interpreter and 39.4 s under the compiled route, compile included |
| `test-bodies` | 25 corpus files; `tests/testing.rs` | yes — all 25 byte-identical, `placeorder.vyrn` among them | 4.6 s against 5.3 s over the 25. The compiled route is SLOWER here: the bodies are small and the compile is the whole cost |
| `test-state` | nothing in the corpus | no — one fresh instance per body, where the interpreter runs the module's state once and lets the bodies see each other's writes | `rfcs/probes-0125/module-state-across-test-bodies.vyrn`: 2 passed under the interpreter, `1 != 2` under wasm. The cost is the semantics, and M5 retires them rather than reproducing them |
| `bench-check` | CI's "Bench --check" step; 17 corpus files | yes — all 17 byte-identical, after this slice's fix below | 52.3 s against 2.4 s over the 17, a factor of 22 |
| `serve` | `vyrn serve`, `vyrn dev`; `tests/serve.rs`, `rpc.rs`, `universal_pages.rs` | no — `serve_cmd` takes no engine, and `vyrn serve --engine wasm` silently serves from the interpreter | `interp::serve` holds ONE live instance across every request: `main` runs once and the module's state persists. A resident wasm instance the host calls into per request is not designed anywhere in this RFC or its plan |
| `mounted-routes` | `vyrn routes`, for the hand-written channel | no | `interp::mounted_routes` evaluates the arguments of `mount(..)` before any program runs. The derived rows survive without it, and the command already prints a note when the channel fails |
| `from-json` | `vyrn fmt --from-json` (RFC-0097 M1) | no — `fmt` has no engine flag | one constant 40-line Vyrn converter, run in process. Compile it instead, or move it to Rust |
| `run-profile` | `vyrn run --profile`, `vyrn check --profile` | no, and there is nothing to profile: under the compiled route the time is wasmtime's | `vyrn_frontend::prof` counts interpreter steps. `check --profile` measures generation and survives while generation is interpreted |
| `gen-fn` | every `gen fn` (RFC-0021), on every command that loads a module: `run`, `check`, `test`, `bench`, `build`, `doc`, `why`, `routes`, `fmt`, `emit-*`, and the LSP | partial — RFC-0076's `vyrn-genwasm` runs a generator as compiled wasm, but the feature `wasm-gen` is OFF in `vyrn-cli`'s default build and ON in `vyrn-lsp`'s, it needs clang and a wasi sysroot, and it declines to the interpreter for a module reaching `writeFile`, `writeAtomic`, `renameFile` or `fsyncFile` | the largest row. `generate_interpreted` is both the reference and the fallback; deleting it makes clang a hard requirement of `vyrn check` |
| `fixture-oracle` | `examples/expected/*.stdout`, `.stderr`, `.exit` | no — the interpreter IS the oracle the compiled route is compared against | after the deletion `VYRN_FIXTURES=write` records from the route under test, and the fixture gate is a self-comparison. The oracle becomes a reviewed diff plus `wasmhash`'s cross-platform bytes |
| `parity-column` | CI's parity job, 41 programs three engines | yes, by replacement — `fixtures` plus `wasmhash` state the invariant §2.6 names | parity is 971 s on one platform; `fixtures` is 123 to 213 s and `wasmhash` 97 to 146 s, each on four |
| `boundary-carrier` | `tests/boundaries.rs`, 18 of its 19 rules | yes — every row keeps a native, wasm or Vyrn carrier | 18 rows lose a carrier and the census's copy total falls by 18 |
| `library-run` | `vyrn_frontend::run`, `interp::run`; `jsondec.rs` and `loader.rs` self-tests | no — the compiled route lives in `vyrn-cli`, not in the frontend | those tests move to the CLI's harness, or the frontend takes a dependency on a backend |
| `extern-unavailable` | `examples/externdemo.vyrn`, the corpus's one host-only program | yes, since the fourth slice below — both engines print ``error: extern `jsNow` is not available on this target`` on standard error and exit 1. The third slice read `worse` here: the compiled route trapped with `error: error while executing at wasm backtrace:`, the one output difference in 204 programs | the embedded host answers the `vyrn` namespace with `interp::extern_unavailable`'s sentence, as native's C stub already did. No emitted byte changes |
| `site-export` | CI's Site job | yes, and this is new — the frame-limit refusal M5's second slice recorded is gone, and the compiled route writes the same 241 files | 187.30 s against 13.89 s, medians of three interleaved runs, and the 241 files are byte-identical |

#### How the third column was proved

Every `yes` above is a run, not a reading.

- **The corpus, both engines.** All 204 `examples/*.vyrn` under `vyrn run` and
  `vyrn run --engine wasm`, with the harness's own conventions — cwd
  `examples/`, the `.stdin` and `.args` fixtures, the fixed clock and seed.
  203 are byte-identical on stdout, stderr and the exit code. The refusal sets
  are the same set, not the same size: 58 programs exit non-zero under each,
  and no program is refused by one engine and run by the other. The single
  difference is `externdemo.vyrn`, the `extern-unavailable` row — closed by the
  fourth slice, which makes the count 204 of 204.
- **The bodies.** `vyrn test` over the 25 examples with `test` blocks and
  `vyrn bench --check` over the 17 with `bench` blocks, under both engines.
  All byte-identical. `placeorder.vyrn`, the placement defect M5's second
  slice recorded, passes under both here: the rule that closed it landed with
  M1.
- **The one disagreement that is by design** is `test-state`, and the probe
  reproduces it in twelve lines.

#### The measurements

Every pair is interleaved — interpreter, compiled route, interpreter,
compiled route — because other worktrees run their gates on this machine and
an interleaved pair moves together under someone else's load. Release binary,
generator cache warm.

| what | interpreter | `--engine wasm` | ratio |
|---|---|---|---|
| the corpus, 204 programs, `run` | 89.1 s | 39.4 s | 2.3x |
| `test` over 25 files | 4.6 s | 5.3 s | 0.9x |
| `bench --check` over 17 files | 52.3 s | 2.4 s | 22x |
| the site export, 241 files | 187.30 s | 13.89 s | 13.5x |

The corpus row is the weakest of the four, and says so: at the game's small
inputs `vyrn run` is process start-up under either engine, so 204 programs
measure the two start-ups and not the two engines. The interpreter's time is
in the bodies and in the export.

The site export is the row that changed. M5's second slice measured 136.9 s
under the interpreter and a refusal after 5.2 s under the compiled route, at
`chapters` in `site/app/guide.vyrn`, 11,360 bytes of frame against a limit of
8,192. On this branch the export compiles and runs, and the two engines write
the same 241 files, byte for byte. §2.5's first row is no longer a claim.

CI's two jobs, read off the last green run of each branch with `gh run view`:

| job | what it runs | platforms | wall |
|---|---|---|---|
| `parity` | 41 programs, interp == native == wasm | 1 | 971 s (`rfc-0125-core`), 544 s (`main`) |
| `fixtures` | 203 examples, compiled route against the recorded output | 4 | 123, 152, 168, 213 s |
| `wasmhash` | every example's wasm, one SHA-256 per example | 4 | 97, 103, 112, 146 s |

So the pair that replaces parity costs less on its worst leg than parity does
on its only one, and it runs on four platforms rather than one.

#### The binary, and why the gate is a count

The census should price the interpreter in the release binary and in the build.
**A clean feature gate is not possible, and that is itself a census
finding.** `mod interp` cannot be `#[cfg]`-ed out: six CLI
commands call into it (`run`, `test`, `bench --check`, `serve`, `dev`,
`fmt --from-json`), `vyrn routes` calls `mounted_routes`, the loader's
generator path calls `generate`, `vyrn-frontend`'s own public `run` calls it,
and both backends plus the checker read five constants that live in the file
(`CALL_DEPTH_LIMIT`, `FRAME_LIMIT`, `REGION_MAX`, `ARRAY_LIT_LIMIT`,
`INTERP_STACK_BYTES`) along with `extern_unavailable`. Gating it is the
deletion, not a measurement of it. So the price is a count:

- `interp.rs` is 11,631 lines — 8,401 of code and 2,805 of comment — of
  `vyrn-frontend`'s 87,237 and the workspace's 149,870 lines of `src`. 13.3
  percent of the crate, 7.8 percent of the compiler.
- It pulls in **no crates**. `vyrn-frontend` has an empty `[dependencies]`
  table, so the interpreter costs the binary nothing but its own machine code.
  The 21,110,272-byte `vyrn.exe` measured here is the embedding of wasmtime
  and Cranelift that M5's first slice already priced (9,973,248 to 20,988,928
  bytes), and deleting the interpreter does not touch it.
- What it does buy back is the `Val` model of §1.1's third picture, the copies
  the boundary census counts at `Carrier::Interp` (18 rows), and the 44
  builtin names the interpreter matches against its own value model.

#### The deletion plan

The order is fixed by what has no replacement, not by what is easiest.

1. **The suites move first.** `fixtures.rs` already compares the compiled
   route against a recorded file. The other 33 suites call `vyrn run` and read
   its output; they move by adding `--engine wasm` to the command they build,
   which is a one-line change per call site and 150 call sites. Nothing else
   can be deleted while they are the interpreter's largest consumer.
2. **The parity job is replaced by the pair it already runs beside.**
   `fixtures` plus `wasmhash` state §2.6's invariant, on four platforms, for
   less wall time than parity's one. `tests/parity.rs` and its
   `KNOWN_DIVERGENT` / `NATIVE_UNSUPPORTED` lists go with it; `tests/route.rs`
   keeps the native route honest against the same wasm.
3. **`VYRN_FIXTURES=write` records from the compiled route.** This is the
   change that costs the most and shows the least: the fixture gate stops
   being a comparison of two engines and becomes a record of one. The
   replacement oracle is the reviewed diff — a recording run's output is read
   before it is committed — plus `wasmhash`, which catches the compiler that
   depends on its host, which is the failure a single-engine oracle cannot
   see.
4. **`serve`, `dev`, `routes` and `fmt --from-json` are ported or dropped.**
   These are the four with no route at all. `routes` and `from-json` are
   small. `serve` is not: see below.
5. **The generator path is last**, because it is the only consumer that gets
   SLOWER and less available when it moves: `wasm-gen` needs clang and a wasi
   sysroot, and `vyrn check` on a machine without a toolchain is a thing that
   works today.
6. **Then `interp.rs`, `Val`, and the 18 `Carrier::Interp` rows.**

#### What breaks, with no replacement yet

- **`vyrn serve` and `vyrn dev`.** One live instance, `main` run once, module
  state persisting across requests, and a Rust accept loop calling back into
  it per request. The compiled route runs a module to completion and exits.
  Nothing in this RFC or `PLAN-0125-runtime.md` designs a resident instance
  the host calls into, and three test suites spawn it.
- **The generator path without clang.** `vyrn check` runs every `gen fn`
  today with no toolchain at all. After the deletion it cannot.
- **The oracle.** Deleting the reference semantics leaves the fixture files
  as the only statement of what a program prints, and they were recorded from
  the thing being deleted.
- **`vyrn run --profile`.** The flag reports the interpreter's own steps.

#### The one gate that would prove the deletion safe

Record every `examples/expected/*` file a second time, from the compiled
route, and require the two recordings to be byte-identical — the interpreter's
committed files against the compiled route's fresh ones, over all 203
comparable examples, on all four platforms of the matrix. That is the
`fixtures` job with `VYRN_FIXTURES=write` run into a scratch tree and diffed
against the committed one. It passes today, which is the point: it is the same
statement the fixture gate makes, made once more at the moment the oracle
changes hands, so the recording that replaces the interpreter is provably the
recording the interpreter would have made.

#### The defect the census found, and its fix

One program the interpreter ran and the compiled route refused:
`examples/langbench.vyrn`, under `vyrn bench --check --engine wasm`, with

    error: direct backend: no lowering for a branch yielding `blackBox` at line 212

The direct backend lowers `blackBox(v)` as `v` (RFC-0055), but its type peek
held no row for the name, so an `if` arm that yielded one had no type and the
whole program was refused. The reduction is four lines and is recorded as
`rfcs/probes-0125/branch-yielding-blackbox.vyrn`. The fix is one arm in
`Fn_::peek` that peeks the argument, which is what the emitting path does with
it, and `tests/benching.rs` pins both engines on the reduction. It changes no
emitted bytes: `blackBox` exists only inside a `bench` or `test` body, and
`wasmhash` builds each example's `main`.

Two things were found and NOT fixed here, both recorded rather than repaired.
The fourth slice below fixed both:

- `examples/externdemo.vyrn` under `--engine wasm` traps with wasmtime's
  backtrace where the interpreter names the unavailable `extern`. The
  `extern-unavailable` row above.
- `site/export.vyrn` reads its output directory from `args()[1]`, but
  `args()` excludes the program name (RFC-0014), so `vyrn run
  site/export.vyrn out` passes one argument, `argv.length > 1` is false, and
  the export writes to its default `out` whatever it is given. The site
  workflow creates `out/` and so never noticed. It is the site's bug, not the
  compiler's, and it is written down here because the census tripped over it.

#### Gates

In order, one at a time, on the machine of §1.4 with other worktrees running
their own gates on it: `cargo fmt --all --check`, clean; `cargo build --release
-p vyrn-cli`; `cargo test --workspace -- --skip _natively`, 1,965 tests over 87
suites; `cargo test -p vyrn-cli --test memory -- --test-threads=1` serial, green
on the first attempt; the `fixtures`, `boundaries`, `traps`, `audience`, `floor`
and `contracts` suites (16, 2, 21, 3, 12 and 2); the `kernel`, `effects` and
`typed` suites (2, 1 and 1); parity in release with `--ignored`, 41 of 41
byte-identical in 238 s; the cross-engine generator test with a fresh
`VYRN_GEN_CACHE_DIR`, every generator example the same source under both
engines — the five programs M5's second slice reported red are green here;
`vyrn doc --std --verify`, 41 files up to date; and `vyrn test` over
`site/export.vyrn` and the site's own modules, 28 files and every block green.

One gate is red, and it was red before this slice.
`VYRN_WASM_MANIFEST=check` on `wasmhash` reports that all 172 examples differ
from `rfcs/census/wasm-sha256.tsv`. The same command at the base commit
(29aff225) reports the same 172, so the committed manifest is stale on this
branch, as M5's second slice said it would be until the integrator regenerates
it after the merge. This slice's change to the direct backend emits no
different bytes, and that was checked rather than argued: the manifest was
regenerated twice in release, once with the base's `direct.rs` and once with
this slice's, and the two files are identical. The committed file is left
alone.

#### The fourth slice (2026-09-04): the two defects the census recorded

The third slice found two things and repaired neither. This slice repairs
both. Nothing is deleted, and no emitted byte changes.

**The reached `extern`.** A program that calls an `extern fn` no host answers
must fail the same way on every engine. It did not. Read off
`examples/externdemo.vyrn`, before:

| engine | standard error | exit |
|---|---|---|
| interpreter | ``error: extern `jsNow` is not available on this target`` | 1 |
| native | ``error: extern `jsNow` is not available on this target`` | 1 |
| `--engine wasm` | `error: error while executing at wasm backtrace:` | 1 |

After, all three print ``error: extern `jsNow` is not available on this
target`` and exit 1.

**Where the rule lives, and why there.** In the HOST, not in the emitter and
not in a fourth copy of the wording. `vyrn-cli`'s embedded engine
(`src/wasmrun.rs`) now defines every import in the `vyrn` namespace as a
function that writes `vyrn_frontend::interp::extern_unavailable`'s sentence to
fd 2 and exits 1, and it does that before `define_unknown_imports_as_traps`
sees the module. Two other places were possible and both are wrong:

- **The emitter.** A trap stub in the module would delete the import, and the
  import is the whole point of RFC-0012 — a browser page fills it, and
  `std/rpc`'s `vyrnRpcCall` is that import in three client artifacts. An
  emitter that refuses on the author's behalf refuses in the browser too.
- **A new wording.** There is one table since RFC-0101 M5 and one sentence for
  this refusal since RFC-0012. The host reads it; it does not respell it. This
  is the same shape native has: `toolchain::extern_trap_stubs` writes a C stub
  per declaration from the same function.

The sixth M6 slice decided that the `extern` row is a CALL, and the emitter
agrees: `Module::sweep` drops an import nothing reaches, so a `vyrn` import in
a loaded module is an `extern` the program REACHES. Naming it is therefore
never a refusal of a program that would have run.

**What the registries say now.** `tests/fixtures.rs` no longer skips
`WASM_ONLY`, and `examples/expected/externdemo.{stdout,stderr,exit}` are
recorded: the corpus is 204 of 204 compared, not 203 with one skipped. The
list itself stays, because it is true of the harnesses that drive an OUTSIDE
tool — parity's wasm column is the `wasmtime` CLI and `route.rs`'s is wasm2c,
and neither knows the namespace. Its doc comment says which harness it binds
and which it does not. The census row above reads `yes`, and `fixtures.rs`'s
`CENSUS` reads `yes` with it.

**The export's out-directory.** `site/export.vyrn` read `args()[1]`, and
`args()` excludes the program's name (RFC-0014), so `vyrn run
site/export.vyrn dist` fell to the default and wrote `out/` while printing
`exported .. to out`. The read is one function now, `outDirFrom`, with the
convention in its doc comment and a `test` block on it — the export into
`dist` writes 241 files there and creates no `out/`, measured. The workflow's
argument is unchanged and now takes effect: it passes `out`, which is also the
default, which is why CI never saw the bug. The export produces the same 241
files it did.

**Gates.** In the order §1.4 requires, one at a time: `cargo fmt --all
--check`; the release build; the `fixtures`, `boundaries` and `traps` suites;
`kernel`, `coretables`, `typed` and `effects`; `audience`, `floor`,
`contracts`, `fieldstore`, `places`, `simd`, `wasmabi`, `wasmio` and
`bytesink`; `vyrn-frontend`; the workspace less the peak-RSS tests; those
serially; parity in release with `--ignored`, 41 of 41; the residue ratchet;
the cross-engine generator test with a fresh cache; `vyrn doc --std --verify`;
and `vyrn test` over the site's files, 35 blocks in `export.vyrn` where there
were 34. `VYRN_WASM_MANIFEST=check` is GREEN, which is the third slice's red
gate turned over: the manifest was regenerated on this branch after that slice,
and this slice adds no trap stub to any module, so every example's wasm still
hashes to the committed row. The manifest is not regenerated here and needs no
regenerating.

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
| `write-output` | `print`, `writeStdout`, `trace`, `debug`, `info`, `warn`, `error` | `fd_write` on 1, 2 | yes | yes | yes | no (finding 4) |
| `fs-read` | `readFile`, `readFileBytes` | `path_open`, `fd_read`, `fd_close`, `fd_prestat_get` | yes | yes | `NOENT` | `readFile` yes, `readFileBytes` no (finding 5) |
| `fs-write` | `writeFile`, `writeFileBytes`, `renameFile`, `fsyncFile` | `path_open`, `fd_write`, `path_rename`, `fd_sync` | yes | yes | `NOENT` | no |
| `fs-list` | `listDir`, `listDirKinds` | `fd_readdir` | no (`NATIVE_UNSUPPORTED`) | yes | `BADF` | yes, mediated |
| `args` | `args` | `args_sizes_get`, `args_get` | yes | yes | empty | no |
| `clock` | `hostNowMillis`, `hostMonotonicNanos` | `clock_time_get`; `environ_get` for `VYRN_FIXED_TIME` | yes | yes | yes | no (finding 13) |
| `random` | `hostRandomSeed` | `random_get`; `environ_get` for `VYRN_FIXED_SEED` | yes | yes | yes | no (finding 13) |
| `extern` | every other extern declaration, resolved by name | the `vyrn` namespace | trap | no instantiation | yes | no |
| `serve` | `serveStream` | — | trap | trap | trap | no |
| `spawn` | no name: the marker the core keeps on a spawned call (§2.1; the spawn flag of a core call, second slice) | — | yes | yes, eager | yes, eager | no |
| `trap` | `panic`, `@panicAt`, `assert`, `assertEq`, `runtime$trap`, `mem$trap`; and the core's trap statement | `proc_exit` | yes | yes | yes | yes |
| `gen-only` | `moduleInterface`, `contractOf`, `lex`, `render`, `raw`, `rawAt`, `@codeText`, `@codeSplice` | — | no | no | no | yes |

Every one of the fifteen preview1 imports `direct.rs` declares is in the
third column; `environ_sizes_get` and `environ_get` serve the clock and the
seed and nothing else. The runtime module's own primitives (`std/mem`,
`std/runtime`) are pure but for the four rows that name them. `spawn` has
no atom: the core keeps a marker on the call (`Rhs::Call::spawn`, second
slice), and the spawning body carries the effect. The spawn-isolation rule
of RFC-0004 §Q4 is the one inclusion check the judgment makes today: the
spawned callee's set within `effects::Effects::SPAWN_ALLOWS`, which is
`alloc, trap`. The harness counts every spawn site and puts one outside the
rule in the ratchet.

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

#### The second slice (2026-09-03)

The second slice closes findings 1, 2, 3 and 6, records a decision for 4,
5 and 13, and records why no floor check moves out of the pass yet. The
tally at its last commit:

    effects over the corpus: 180 programs (31 not loadable here, 2 refused as
    recorded), 18775 functions judged, 6267 pure, 8 unlowered, 63 calls
    through a function value judged over their sources, 70 unattributed
      open sets: 43
      effect 12447  alloc
      effect    11  read-input
      effect   288  write-output
      effect   189  fs-read
      effect    19  fs-write
      effect   106  fs-list
      effect     8  args
      effect    29  clock
      effect    20  random
      effect    31  extern
      effect    22  serve
      effect     4  spawn
      effect  3285  trap
      effect   698  gen-only
      spawn:      12 sites judged, 0 outside `alloc, trap`
      floor:      18535 agree, 24 callee-carried, 216 gen-body, 0 floor-blind,
                  0 core-blind
      audience:   18334 no fence, 27 agree, 414 declared-only, 0 unfenced,
                  0 server-extern

The corpus grew between the slices: M3 to M5 merged the runtime in Vyrn and
the frames, so the function count is not the first slice's. **The ratchet
is 0.**

- **1, closed.** `Rhs::Call` carries `spawn`; the core prints a spawned call
  as `spawn f(..)`. The lattice has the `spawn` row and the spawning body
  carries the effect. The spawn-isolation rule of RFC-0004 §Q4 is the one
  inclusion check `effects.rs` makes: the spawned callee's set within
  `Effects::SPAWN_ALLOWS`, which is `alloc, trap`. The harness judges every
  spawn site (12 in the corpus) and puts one outside the rule in the
  ratchet; none is. The checker's rule is wider than the effect: it also
  refuses a `modify` parameter, `drop`, module state, and the pure builtins
  `close`, `stringFromBytes`, `lineAt` and `colAt`. None of those is an
  effect. They stay in the checker, and the judgment does not restate them.
- **2, closed.** M3's close-out made a lambda body a frame of its own
  (`Body::lambdas`). The harness hands every frame to `judge`, and a body
  joins the frames of the lambdas it holds: the value it builds can run
  them, which is presence, as the floor counts it. Scratch `lambda.vyrn`'s
  `main` (`let readIt = p -> readFile(p)`) is judged `alloc, write-output,
  fs-read`, floor *agree*.
- **3, closed.** A call whose callee is a name of the body with a function
  type is judged over RFC-0037's stored sources for that type
  (`checker::stored_fn_effects`, matched by `checker::fn_sigs_match`, which
  is public now). A named source is its instances; a lambda source is its
  frame, keyed by the function it was written in and its line — the two
  facts `StoredLambda` records. 63 calls in the corpus are judged this way and 70
  are not. The 70 are two kinds, and the harness prints both under `open
  sets` (43 types) and `VYRN_EFFECTS_UNKNOWN=1`. A lambda written in a
  module-state initializer or a `bench` body has no frame, because neither
  is an instance the lowering builds: `examples/bin/server.vyrn`:33,
  `examples/shelf/server.vyrn`:31 and 35 (the route tables),
  `examples/genericpayload.vyrn`:58–60, `examples/langbench.vyrn`:216. And a
  local whose type matches no collected source at all: the `cb` of every
  `std/rpc` deliverer (`fn(RpcReply<T>)` in `bin`, `fullstack`, `shelf`,
  `rpc.vyrn`, `rpcsplit.vyrn`), the `run` thunk of a page's `Lazy<T>` and
  `ParamQuery<P, T>` (`std/ui`'s `runLazy`, `runParamQuery`), the
  `Cursor`-stepping closures of `std/stream` (`streamops.vyrn`,
  `streamunfold.vyrn`, `streamlazy.vyrn`, `membench.vyrn`, `lambdas.vyrn`,
  `knucleotide.vyrn`), and the resolvers of `std/graphql` (`graphql.vyrn`,
  `shelf/server.vyrn`). For these the checker's collection and the core's
  name types do not meet: the frame's parameter is typed `fn(T) -> U` with a
  bare parameter the sources were collected without, or the source flowed
  through a record field or a generated module. This is **finding 14**, a
  hole in the join, and the deletion slice inherits it before finding 7.
  Calls to a projection by name (`field`, `tag`, `tryAt`, `tryField`,
  `wrapped`) are among the 70 too; they are not function values, and the
  harness's resolver is what does not name them.
- **6, closed.** `listDir` and `listDirKinds` are in `floor::CALLS`, row
  `fs`; RFC-0103's tables carry the row with the date. The prediction
  program is `examples/listing`: a browser artifact whose entry lists a
  directory. `vyrn check` refuses it with the floor's wording (`` `listDir`
  needs `fs`; target `browser` has no filesystem ``), `tests/common`'s
  `EXPECTED_PROJECT_CHECK_FAILURE` records the refusal, `tests/floor.rs`
  asserts it, and the effects harness counts the entry as refused rather
  than failing to load. `listdir.vyrn`'s `main` is *agree* now.
- **No floor check moved.** The verdicts agree on the whole corpus for the
  `stdin` row (11 functions with `read-input`) and the `args` row (8), so
  either could be the prediction-as-program by verdict alone. The
  placement stops it. The floor decides inside the loader (`loader.rs`,
  after the link and before the checker runs), for every command that
  loads with an `artifacts` map, and `vyrn why --capability` reads the same
  graph. The judgment needs a checked program: the core is built from the
  checker's types and the ownership analysis, and 8 instances still have
  no core (finding 12). Routing a row through `effects.rs` today would
  move its refusal from the load to the commands that build a core, after
  the type errors instead of before them, and would leave the report
  reading a graph the check no longer reads — a wording and an ordering
  change, which the brief for this slice does not allow. A row moves when
  the load can build the core, which is M2's gap list closed. Every floor
  check still lives in `floor.rs`.
- **4, decided.** `write-output` is one effect and the row's `gen` cell is
  one cell: `no`. RFC-0021's sandbox is deterministic and cache-keyed; a
  `print` in a `gen fn` writes to the compiler's stdout, is no cache input,
  and is silent on a cache hit, so the same build prints or does not print
  by the state of the cache. No change in this track: `print` in `COMPTIME_FORBIDDEN` is one
  line, but the fence's hint names what it refuses and would have to say
  `print`, and a test that pins the refusal does not exist yet. The
  deletion slice makes the cell, with the hint and the pin.
- **5, decided.** The row stays split, and the reason is the route, not
  the effect. `readFile` in a `gen fn` goes through the loader's resolver
  and is a cache input; `readFileBytes` does not, so a generation that
  read bytes would be cached on a key that does not name them. The cell
  becomes one (`yes` for both) when `readFileBytes` takes the resolver
  route, and not before. No change.
- **13, decided.** The verdict stands (`clock` is `no` at generation time);
  the wording is the fence's, and it names `hostNowMillis` an extern
  because `extern_fns` does not know RFC-0103 M2's host-boundary rule.
  When the fence becomes the inclusion check, the reason is the row:
  "reads the clock". Until then the wording stays, because changing it is
  a new branch in `check_comptime_purity` and not one line. No change.

Findings 7 and 8 are unchanged: the `extern` declaration-or-call question
and the presence-or-reachability rule change wait for the deletion slice,
which now inherits them first.

#### The third slice (2026-09-03)

The third slice closes finding 12, closes half of finding 14 and lists the
rest, and finds why no floor row moves — a different reason from the second
slice's, and one the second slice could not have found. The tally at its
last commit:

    effects over the corpus: 180 programs (31 not loadable here, 2 refused as
    recorded), 25443 functions judged, 7345 pure, 0 unlowered, 64 calls
    through a function value judged over their sources, 69 unattributed
      open sets: 40
      effect 18037  alloc
      effect    11  read-input
      effect   292  write-output
      effect   189  fs-read
      effect    19  fs-write
      effect   106  fs-list
      effect     8  args
      effect    29  clock
      effect    20  random
      effect    31  extern
      effect    22  serve
      effect     4  spawn
      effect  3285  trap
      effect   826  gen-only
      spawn:      12 sites judged, 0 outside `alloc, trap`
      floor:      25203 agree, 24 callee-carried, 216 gen-body, 0 floor-blind,
                  0 core-blind
      audience:   25002 no fence, 27 agree, 414 declared-only, 0 unfenced,
                  0 server-extern

The corpus grew again between the slices: the I/O family moved into Vyrn, so
the function count is not the second slice's. **The ratchet is 0**, and the
kernel's tally is 17,513 instances accepted, 0 refused, 0 unlowered.

- **12, closed.** The eight instances were one function: `std/vyx`'s
  `vyxRegion`, whose `rawAt` call `core::call` could not attribute.
  `ast::SURFACE_BUILTINS` is one list of four names and the arm spelled two
  of them (`render`, `lex`), so `raw` and `rawAt` fell to the gap. The arm
  asks `ast::is_surface_builtin` now. Unlowered is 0 in both harnesses, and
  the effects harness judges eight more functions.
  `VYRN_EFFECTS_GAPS=<substring>` shows where a remaining gap is, as
  `VYRN_KERNEL_GAPS` does.
- **14, half closed and the rest listed.** Two defects, and one body that
  stays out.
  - A parameter's type in the core was the DECLARATION's, not the
    instance's, so `map<Int64, Int64>`'s `f` read `fn(T) -> U` and matched
    no signature RFC-0037 collected. `core::build` substitutes the
    instance's arguments into a parameter's type now; every other type in
    the core comes from the instance's rows and was substituted already.
    Every open set the harness prints names a concrete signature after it.
  - The module-state initializer (RFC-0013) has a core:
    `core::build_module_state` states each `let` at module scope as the
    store into the global it names, run once at `_start`, and both
    harnesses judge it and the lambda frames it holds. The three route
    tables — `examples/bin`'s and `examples/shelf`'s `middleware`,
    `examples/genericpayload`'s `ops` — are judged now. The kernel accepts
    62 more instances at 0 refused: a store into a global consumes the
    value it is handed, so an initializer is linear by construction.
  - A `test` (RFC-0015) or `bench` (RFC-0055) body is a body too and does
    NOT get a core here. The checker checks a CLONE of it
    (`check_tests`, `check_benches`), so nothing typed the nodes `own` and
    the lowering walk, and a core built over the real nodes is one gap per
    expression: 3,663 of them, measured on a branch. The plan is keyed by
    the real nodes and the checker's types by the clone's, and the two meet
    only when the checker checks the real body — a frontend change this
    slice does not make. `examples/langbench.vyrn`'s `bench@7` is the one
    open set left of this kind.
  - Open sets fall from 43 to 40 and unattributed calls from 70 to 69. The
    harness prints every one of the 69 with its program, its line and its
    reason, and they are two kinds. Thirteen are a projection dispatched by
    name (`field`, `tag`, `tryAt`, `tryField`, `wrapped` in
    `namedplace.vyrn`, `tryplace.vyrn`, `protoplace.vyrn`, `jchain.vyrn`,
    `jsonplace.vyrn`): a projection is no function value, it is expanded at
    its site (RFC-0123), and the harness's resolver has no body to name.
    Fifty-six name a function type no collected source matches — the `cb`
    of every `std/rpc` deliverer, the `run` thunk of `std/ui`'s `Lazy<T>`
    and `ParamQuery<P, T>`, the `Cursor` steppers of `std/stream`, the
    resolvers of `std/graphql`, and the lambda an argument position
    monomorphized away (`lambdas.vyrn`, `streamops.vyrn`,
    `streamunfold.vyrn`, `streamlazy.vyrn`, `membench.vyrn`,
    `knucleotide.vyrn`). The second kind is RFC-0037's collection and the
    core's names not meeting, and it stays finding 14.
- **No floor row moved, and the reason is not the second slice's.** The
  second slice named the ordering and M2's gap list. The gap list is closed
  now, and the cost objection does not stand either: `vyrn check` on
  `site/export.vyrn` is 3.32–3.43 s warm, the same command with
  `own::analyze` and the placer run is 3.23–3.40 s (inside the noise), and
  `vyrn emit-lowered` on the same file — the load, the analysis and the
  whole lowering — is 3.51–3.56 s, which is 4 to 6 per cent. What stops the
  row is placement, and this slice can name it exactly:
  - **The crate boundary.** `effects.rs` is in `vyrn-lower` and reads the
    named core, which is built from `Instance`. `floor::objection` is in
    `vyrn-frontend`, which `vyrn-lower` depends on. The judgment cannot be
    called from where the refusal is made.
  - **The order.** The floor runs inside the load (`loader.rs`, after the
    link and before `check_and_synthesize`), so its refusal comes BEFORE
    every type error: a browser entry that calls `readLine` and also binds
    `let bad: Int64 = "x"` reports the floor and not the type error today.
    A judgment over the core runs after the checker, so the row's refusal
    would swap places with the type errors while the other three rows kept
    their place — one rule with two orders.
  - **The LSP.** `vyrn-lsp` depends on `vyrn-frontend` alone, and it shows
    the floor's refusal because the loader makes it. A row that moves to
    `vyrn-lower` leaves the editor, unless `vyrn-lsp` takes the dependency
    too. `vyrn why --capability` reads the same graph and would stop
    answering for the row.

  The row moves when the floor's decision moves out of the load as a whole,
  to one call after the check that the CLI and the LSP both make. That is a
  change of where a rule is stated, which is this RFC's subject, and it is
  the deletion slice's first act rather than a row's. Every floor check
  still lives in `floor.rs`.

#### The fourth slice (2026-09-03)

The fourth slice installs the effect judgment into the floor's decision,
moves the first two rows into it, and takes the decision for those two rows
out of the load. `floor::CALLS` holds the `fs` row alone now.

**The hook.** `floor::install_judge(Judge)` is the placer's shape
(`own::install_placer`): a function pointer in `vyrn-frontend`, set once,
first installation wins. `vyrn_lower::install()` registers
`effects::reaches` beside `core::augment`, so the CLI arms both at
start-up and the LSP arms neither. The judgment is handed a checked
program and answers one question: which module reaches which of
[`floor::JUDGED`]'s capabilities. It builds the named core for every
instance, joins each body's set to a fixpoint, and reports
`(module key, capability)`.

**Where the loader calls it.** The load cannot answer a judged row: the
core is built from the checker's types and nothing in the load is checked.
So the loader asks first and refuses second. At the point it called
`floor::objection` it now calls `floor::objected`, which is the same walk
without the diagnostic. An objection on a row no judgment answers is
refused there, in the words and the order it always was. An objection on a
judged row is HELD — `floor::defer` keeps the graph, the root key, the
artifact map and the origin maps — and `floor::decide` makes it at the end
of `check_and_synthesize`, which is the one call every CLI command makes
with a checked program. `decide` drops the judged rows the judgment does
not confirm and re-runs `objection` over what is left, so the refusal is
still the scan's own words: the carrier it found and the line it found it
on. Nothing else moved: a load whose first objection is on `fs` or
`extern` never reaches `decide`.

The floor's whole call did NOT move after the check, and it did not have
to. Moving it would have changed the order of every row against every type
error; deferring only the row a judgment answers changes the order of that
row alone, which is the smallest change that lets a row move at all.

**What the LSP does.** Nothing, and that is the point. `vyrn-lsp` depends
on `vyrn-frontend` alone and installs no placer, so it installs no judge
either — the brief's "make them the same". With no judgment installed
`floor::objected` reports no judged row, the loader refuses inside the load
as before, and the editor's diagnostics are the same diagnostics. The same
is true of `vyrn why --capability`: `floor::carried` reads `CALLS` and
`JUDGED` as one scan, so the report still names every carrier of a moved
row, with its module and its line.

**What moved.** `readLine` (`stdin`) and `args` (`args`). Both are out of
`floor::CALLS` and in `floor::JUDGED`. The verdicts agree over the whole
corpus; the harness says so per function, and refuses if they ever do not:

    judged:     25436 agree, 7 callee-carried, 0 gen-body, 0 differ

The seven are the whole-floor comparison's *callee-carried* kind at the
grain of these two rows: the judgment has the effect and a callee's body
spells it, so the floor's union over the closure agrees. `0 differ` is the
new ratchet, beside the old one. The rest of the tally is the third
slice's, unchanged: 25443 functions judged, 0 unlowered, 40 open sets, 69
unattributed, floor 25203 agree / 24 callee-carried / 216 gen-body / 0
blind either way, and the ratchet 0.

Two declarations hold no instance and are read from the AST by the same
`JUDGED` names: a `let` at module scope (RFC-0013) and a `where` predicate.
Nothing else in a program can carry one of these rows.

**The rule these two rows now follow.** Presence gave way to reachability
(finding 8), and less than it sounds: `lower` instantiates every
non-generic function, so a dead ordinary function is still judged and still
refused. What the judgment no longer sees is a GENERIC function nobody
instantiates, and a body the core cannot build (0 in the corpus). What it
sees and the scan does not is nothing, because a moved row is still found
by the scan first — the judgment can only clear a row, never add one.

**The order.** A judged row's refusal now comes AFTER a type error, where
it came before. A browser entry that calls `readLine` and also binds `let
bad: Int64 = "x"` reports the type error today and the floor once the
types are right. That is the price of the row needing a checked program,
it is paid by two rows and not by the floor, and `VYRN_NO_JUDGE=1` puts
both rows back in the pass and the order back with them. `tests/floor.rs`
holds the two refusals byte-identical under both settings.

**One knob, not two.** `VYRN_NO_JUDGE=1` is separate from
`VYRN_NO_PLACER=1` although one call installs both. They bisect different
things — a wrong release row and a wrong refusal — and a bisect that has
to disable the placer to read a floor diagnostic is a worse bisect. The CLI
installs both; each knob stands its own judgment aside.

**The cost.** `vyrn check site/export.vyrn`, warm, four runs each:
2.78–3.15 s with the judgment armed, 2.87–3.20 s under `VYRN_NO_JUDGE=1`.
Inside the noise, and it should be: the judgment runs only when the load's
objection is on a judged row, which for this artifact and for almost every
artifact is never. A browser entry that does spell `readLine` pays one
lowering and one ownership analysis, which `emit-lowered` measures at 4 to
6 per cent of a check.

**The rows still in the floor, and why each has not moved.**

- `fs` — `readFile`, `readFileBytes`, `writeFile`, `writeFileBytes`,
  `renameFile`, `fsyncFile`, `listDir`, `listDirKinds`, and the `logging {
  sink: file(..) }` declaration. Two reasons, and the second is the hard
  one. The declaration carries `fs` and no effect set holds it (finding
  10): the sink is a `logging` block, not a call, and the judgment would
  have to read it as a declaration the way it reads a module-scope `let`.
  That part is small. The other part is not: 216 functions in the corpus
  are `gen fn` bodies carrying `fs-read` or `fs-list`, which the floor
  skips by design and the judgment sees (finding 9). Moving the row means
  the judgment must know the generation context from the artifact context,
  which is the table's `gen` column becoming a check — the deletion
  slice's work, and finding 4's cell with it. Both judged rows had 0
  gen-body functions, which is why they could move first.
- `extern` — carried by an `extern fn` DECLARATION and not by a call
  (finding 7). The lattice can state only the call, and it disagrees: the
  judgment gives `main` no `extern` for an import nothing reaches, while
  the floor refuses it because an unanswered import stops instantiation.
  The row moves when that question is decided, and deciding it changes
  what the floor refuses. Unchanged here.

Findings 5, 9, 10, 11 and 13 still change nothing. Findings 7 and 8 are
now half spent: 8 is on record and paid by two rows; 7 is what keeps
`extern` where it is.

#### The fifth slice (2026-09-03)

The fifth slice makes the lattice's `gen` column a check, closes findings 4
and 13 with it, and moves the `fs` row into the judgment. `floor::CALLS`
holds nothing now.

**Where the column had to go.** The table's DATA — the effects, the sets,
`ATOMS` and the new `Effect::gen` — is `vyrn-frontend/src/effects.rs`. The
JUDGMENT that joins a body's atoms with its callees' sets stays in
`vyrn-lower/src/effects.rs` and re-exports every name, so it still spells the
lattice `effects::`. The fourth slice's hook could not carry this rule.
RFC-0021's fence (`checker::check_comptime_purity`) runs at step 7 of the
check, over the AST, and it must answer for EVERY `gen fn`: a generic one no
lowering instantiates, and a body no core is built for. A judgment over the
core cannot be handed to it, because the core is built from the checker's
types and the check is what is running. A hook with no answer would drop the
fence out of the LSP, which installs no judge; a hook with a fallback would
keep the second list this slice deletes. So the column is data both readers
can see rather than a function pointer one of them installs, and the fence
keeps its place in the order of diagnostics.

`COMPTIME_FORBIDDEN` is deleted. It was thirteen names — the column written a
second time — and the two had drifted in exactly the two places the findings
named, plus one the findings had not.

- **4, closed.** `write-output` is one effect and its `gen` cell is one cell:
  `no`. `print` in a `gen fn` is refused with the other six atoms of its row.
  The hint names it, `checker::gen_fn_using_print_is_rejected` pins the
  refusal, and the table's cell is `no (finding 4)`.
- **13, closed.** The fence no longer calls the clock an extern. `extern_fns`
  skips what `trap::host_boundary_extern` knows — RFC-0103 M2's rule that the
  three host-boundary names are not host imports, because the runtime shim
  implements them on every target — and the row answers instead: "reads the
  clock", "reads entropy". The pin
  `rfc0043_host_clock_extern_is_rejected_in_a_generator` holds the new sentence
  and holds the old one out.
- **5, unchanged, and now written where a reader will find it.**
  `effects::GEN_ATOM_OVERRIDES` is one row: `readFileBytes`, `no`, against its
  `fs-read` row's `yes`. The reason is the route and not the effect, exactly as
  the second slice decided. The one-line version is this line, and it changes
  no verdict.
- **One cell the list did not have.** `serveStream` is an atom of the `serve`
  row, whose `gen` cell is `no`, and it was on no list. It is refused in a
  `gen fn` now. Nothing in the corpus spells it in one.

**The knob.** `VYRN_NO_JUDGE=1` is still one knob. It puts every judged row
back in the pass, and it puts the fence's two changed cells back: `print` is
allowed again and the three host-boundary names are externs again. A refusal
that is new can be told from one that is not, without a second env var.

**What moved out of the floor.** `readFile`, `readFileBytes`, `writeFile`,
`writeFileBytes`, `renameFile`, `fsyncFile`, `listDir`, `listDirKinds` — the
whole `fs` row of calls. `floor::CALLS` is empty and kept: the module scan
reads it and `JUDGED` as one list, so a row that has to come back comes back
by moving one line.

What held the row back was the generation context, which is what the `gen`
column becoming a check gives. 216 corpus bodies are `gen fn`s carrying
`fs-read` or `fs-list`; the floor skips them because a generator runs against
the compiler's filesystem and is never compiled into the artifact; a judgment
that did not know the context would have refused every one of them.
`effects::reaches` skips a `gen fn` instance now, which is the same line
`floor::carried` draws. The fence decides what a generator may do, the floor
decides what a target may do, and this is the line between them.

`floor::is_judged` asks about the CARRIER and not the capability, because `fs`
has both kinds: eight calls a judgment decides and one declaration it does
not. `floor::objected` hands the loader the carrier, so a declaration's
refusal is still made inside the load, before every type error, where it
always was.

**The tally.** `cargo test -p vyrn-cli --test effects`, at this slice's last
commit:

    effects over the corpus: 180 programs (31 not loadable here, 2 refused as
    recorded), 26883 functions judged, 8244 pure, 0 unlowered, 64 calls
    through a function value judged over their sources, 69 unattributed
      open sets: 40
      effect 18218  alloc
      effect    11  read-input
      effect   292  write-output
      effect   189  fs-read
      effect    19  fs-write
      effect   106  fs-list
      effect     8  args
      effect    29  clock
      effect    20  random
      effect    31  extern
      effect    22  serve
      effect     4  spawn
      effect  7785  trap
      effect   826  gen-only
      spawn:      12 sites judged, 0 outside `alloc, trap`
      floor:      26643 agree, 24 callee-carried, 216 gen-body, 0 floor-blind,
                  0 core-blind
      audience:   26442 no fence, 27 agree, 414 declared-only, 0 unfenced,
                  0 server-extern
      judged:     26643 agree, 24 callee-carried, 216 gen-body, 0 differ

The corpus grew again between the slices — the count is not the fourth
slice's — and **both ratchets are 0**. The `judged:` line is the whole-floor
comparison now, because every call row the scan finds is a judged row: 24
*callee-carried* and 216 *gen-body*, the two kinds that are not
disagreements, and nothing else.

**The rows the floor still holds, and why.**

- `extern` — carried by an `extern fn` DECLARATION and not by a call (finding
  7). Unchanged, and the reason is unchanged: the lattice can state only the
  call, and the judgment gives `main` no `extern` for an import nothing
  reaches, while the floor refuses it because an unanswered import stops
  instantiation. The row moves when that question is decided, and deciding it
  changes what the floor refuses.
- The `logging { sink: file(..) }` DECLARATION, which carries `fs` (finding
  10). The sink is a block, not a call; no effect set holds it, and RFC-0103
  §4's reason stands: a declaration is not a capability a body reaches. It
  is the one `fs` reach that degrades SILENTLY in a page, so the pass keeps
  it and keeps deciding it inside the load.
  `floor::the_log_sink_is_a_declaration_the_judgment_does_not_clear` pins that,
  byte-identical under the knob.

What the floor decides on its own is two declarations now, and no call at all.

**The cost.** `vyrn check site/export.vyrn`, warm, on a machine running other
worktrees' gates at the same time, so the spread is the machine's. Four
interleaved pairs, base binary then this slice's: 6.36 / 3.32, 3.64 / 2.82,
3.31 / 2.85, 3.49 / 2.78 s. Five more of this slice's alone: 3.74, 2.85, 2.92,
3.45, 3.19 s, and five under `VYRN_NO_JUDGE=1`: 4.84, 5.08, 3.09, 3.00, 3.09 s.
No cost, and there should be none: this artifact is native, its target has
`fs`, so the load raises no objection and the judgment never runs. A BROWSER
entry that does spell `readFile` pays one lowering and one ownership analysis,
which `emit-lowered` measured at 4 to 6 per cent of a check in the third
slice.

**What the deletion slice still inherits.** Finding 7, which is the `extern`
row. Finding 14, the calls through a function value the join cannot attribute:
40 open sets and 69 calls. And the audience pass, which this milestone has not
touched. 414 *declared-only* functions say why (finding 10): a fence
protects a declaration, and no effect set holds one.
#### The fifth slice (2026-09-03): the third judgment's census

The four slices above are the effect judgment. This one starts the third:
typed by construction. `tests/boundaries.rs` counts every value-boundary check
the three engines carry, runs a program per rule under all three, and asserts
that the three answers are the same bytes. Then one rule moves: the user's own
`where` predicate, which the two compiled backends decided in a crate the
interpreter cannot read and the interpreter therefore decided three more times.
The deletion slices read this table before they delete anything else.

**What a copy is.** The WORDING has been one table since RFC-0101 M5
(`vyrn_frontend::trap`, and `tests/traps.rs` is what keeps it one). What is
still written out per engine is the CONDITION — the half that decides whether
the wording is reached at all. A *carrier* is one engine's own statement of
that condition: `interp` is `vyrn-frontend/src/interp.rs`, `native` is
`vyrn-codegen/src/lib.rs`'s IR together with the C shim in `toolchain.rs`,
`wasm` is `vyrn-codegen/src/direct.rs`, and `vyrn` is a Vyrn module — either
`std/runtime.vyrn` or a library that states the rule in ordinary Vyrn. An
engine that CALLS another carrier's statement is not a carrier: the wasm
emitter does not carry `string-utf8`, because it calls `std/runtime`'s
`strFromBytes`; the interpreter does, because it calls Rust's `from_utf8`.

The carriers were read, not grepped. A grep for a wording finds the comments
that explain it and the tests that pin it, and the censuses of 2026-08-24
recorded what that costs.

#### The census

| rule | what it refuses | RFC | copies | carriers |
|---|---|---|---|---|
| `array-index` | an index outside `0..len` of an array | RFC-0011 | 3 | `interp` `native` `wasm` |
| `string-index` | an index outside `0..byteLength` of a String | RFC-0022 | 3 | `interp` `native` `wasm` |
| `int-div-zero` | an integer divided by zero | RFC-0002 | 3 | `interp` `native` `wasm` |
| `int-rem-zero` | an integer remainder by zero | RFC-0002 | 3 | `interp` `native` `wasm` |
| `int-div-overflow` | `Int64.MIN / -1`, whose quotient is not an `Int64` | RFC-0002 | 3 | `interp` `native` `wasm` |
| `shift-range` | a shift count outside `0..bits` | RFC-0045 | 3 | `interp` `native` `wasm` |
| `int-narrowing` | nothing — it answers, with the low bits and the sign re-read | RFC-0002 | 3 | `interp` `native` `wasm` |
| `float-to-int` | nothing — it answers, truncated toward zero | RFC-0002 | 3 | `interp` `native` `wasm` |
| `where-scalar` | a scalar failing its named type's `where` predicate | RFC-0003 | 1 | `vyrn` |
| `where-record` | a record failing its cross-field `where` predicate | RFC-0003 | 1 | `vyrn` |
| `string-nul` | bytes holding a NUL, made into a String | RFC-0014 | 1 | `vyrn` |
| `string-utf8` | bytes that are not UTF-8, made into a String | RFC-0014 | 1 | `vyrn` |
| `file-nul` | a file holding a NUL, read as a String | RFC-0014 | 3 | `interp` `native` `vyrn` |
| `file-utf8` | a file that is not UTF-8, read as a String | RFC-0014 | 3 | `interp` `native` `vyrn` |
| `io-status` | nothing — it turns a host status into canonical Vyrn wording | RFC-0014 | 3 | `interp` `native` `vyrn` |
| `call-depth` | recursion past `CALL_DEPTH_LIMIT` frames | RFC-0004 | 3 | `interp` `native` `wasm` |
| `region-depth` | arena nesting past `REGION_MAX` frames | RFC-0004 | 3 | `interp` `native` `wasm` |
| `json-decode` | nothing — it accumulates `Issue`s, shape and `where` alike | RFC-0018 | 1 | `vyrn` |
| `char-boundary` | a byte offset inside a multi-byte character | RFC-0046 | 1 | `vyrn` |

**19 rows and 45 copies**, and it was 53 when this census was written. Thirteen
rows are stated three times. Six are stated once, and they are the rows that
went where §2.3 sends the rest: `json-decode` is `std/jsondec` plus the decoders
`jsondec.rs` synthesizes per target type (RFC-0078 M3), `char-boundary` is nine
lines of `std/strings`, `where-scalar` and `where-record` are the generated
constructor of the fourth slice below, and `string-nul` and `string-utf8` are
`std/text`'s `stringFault`, the fifth slice's. All of them are ordinary Vyrn, so
all three engines run the one body and the wording carries the module and the
line it was stated on —
`substring: byte offset 4 is inside a multi-byte UTF-8 character
(std/strings.vyrn:94)`. That is what a row looks like after it moves.

Every row's program is `compiler/vyrn-cli/tests/boundaries/<rule>.vyrn`, and
every one of them hides its value behind a parameter so no engine folds the
check away. The harness compares stdout, stderr and the exit code, and the
last column of the census is therefore a measurement: on 2026-09-03 all 19
rows answer the same bytes under `vyrn run`, `vyrn run --engine wasm` and a
native binary. The native column needs clang and is skipped by name without
it, as every tool-dependent tier here is.

#### What the census found

1. **The trap table did half the job, and the half it did is the visible
   half.** No row's WORDING differs, because no engine spells one: RFC-0101
   M5's gate makes that impossible. Every row's CONDITION is still written per
   engine, and a condition that drifts is a program that runs on two targets
   and dies on the third — which is what parity exists to catch and what §2.3
   exists to make impossible.
2. **Three of the five rows about what a `String` may hold are already one
   statement on the wasm route.** `string-nul`, `string-utf8`, `file-nul`,
   `file-utf8` and `io-status` all name `std/runtime.vyrn` as a carrier,
   because PLAN-0125-runtime §6 steps 1, 4 and 7 moved them there and the wasm
   emitter calls them. Their remaining two copies are the interpreter's Rust
   and the native route's C shim, and step 3 — the native binary through
   wasm2c — deletes the second of those without this milestone touching it.
   That is the largest single deletion available and it is M4's, not M6's.
3. **The interpreter is the carrier no step removes.** Every row's `interp`
   copy is Rust over `Val`, and PLAN-0125-runtime §5.2 records why it cannot
   call the Vyrn statement: the module manages addresses in a linear memory
   and the interpreter has no linear memory. §5.1 lists the six byte-in,
   byte-out families that COULD go through the embedded engine, and M5 deletes
   the rest by deleting the interpreter. So for seventeen rows the honest
   count today is three, and the path to one is M4 step 3 plus M5 — not a
   rewrite of the rule.
4. **Two rows answer rather than refusing, and they are the rows §2.3 is
   about.** `int-narrowing` and `float-to-int` are the only value boundaries
   in the language where a value that does not fit is not refused: `UInt8(300)`
   is 44, in all three engines, by three separately written statements of the
   same wrap. They have three copies and no trap, so nothing about them is
   pinned except this census's program. A validated type would refuse them at
   its constructor, and the language would have to decide what a program that
   narrows means. That is §2.3's question and the next slice's.
5. **`where-scalar` and `where-record` already share half of one statement.**
   The two compiled backends both ask `vyrn_codegen::validation_required` WHERE
   to check, and both call `emit_validation` to do it; the interpreter asks its
   own question in `coerce_walk`, over a value with no `from` type, so it
   re-runs a predicate on a same-type crossing that the emitters exempt. The
   verdicts agree — re-running a predicate that cannot fail changes nothing —
   but the rule is stated twice, and it is stated in `vyrn-codegen`, which the
   interpreter cannot import. `trap.rs` is in `vyrn-frontend` for exactly that
   reason (RFC-0101 §6.4).
6. **`region-depth` is a rule the runtime module deliberately did not take.**
   `std/runtime.vyrn`'s comment says it: "The nesting depth and its
   `region nesting exceeds 64` trap stay with the emitter, because a program
   that traps at the limit must trap where it did." The counter is in the
   prologue, and a prologue is the one place a call cannot be. The row is not
   a candidate for a constructor.
7. **`io-status` is not a value check and should not become a type.** It maps
   a host status onto canonical Vyrn wording. Its three copies are three
   translations of a host's answer, not three statements of a predicate. It
   goes with the I/O family under M4 step 3 and is in the census so that a
   later reader does not look for it under §2.3.

#### The design: which check becomes what

Five lines, and the census's rows sorted into them.

- **A constructor of a validated type.** `where-scalar`, `where-record`,
  `string-nul`, `string-utf8`, `int-narrowing`, `float-to-int`. Each is a
  predicate on one value with no other input. §2.3's shape: the type has one
  producer, the producer runs the predicate, and every slot of the type holds
  a value that passed it. `UInt8` from an `Int64` is that producer for the two
  coercion rows, and it either answers `Option`/`Result` or traps at one site
  named by the type.
- **The trap table and its primitive.** `array-index`, `string-index`,
  `int-div-zero`, `int-rem-zero`, `int-div-overflow`, `shift-range`,
  `call-depth`, `region-depth`. Each is a check the EMITTER inserts because
  the core told it to, not one a producer runs: the value is fine, the
  operation is not. §2.3's emitter maps `trap` to a call with a table index,
  so the wording is the table's row and the condition is the core's.
- **An I/O error, and it stays one.** `file-nul`, `file-utf8`, `io-status`.
  A host answered; the answer is a value. These follow M4 step 7's family and
  M4 step 3 deletes their C copy.
- **Already one.** `json-decode`, `char-boundary`. Nothing to do, and they are
  the shape the first two lines are aiming at.
- **What the kernel's third judgment checks, and where.** A name of a
  validated type is produced only by that type's constructor, and no raw value
  reaches a slot of it. It is a use-def walk over the named core, beside the
  linear and effect judgments, in `vyrn-lower`: for every store into a place
  whose type is validated, the value's producer is that type's `validate`, or
  a name already of that type, or a literal the checker proved. It refuses
  anything else. That is one judgment over one form, and it replaces the
  `coerce`-time check every engine makes at every boundary — which is where
  the 97.6 per cent of coercions that answer "nothing to do" come from
  (`interp.rs`, `coerce`).

#### The row that moved: `where-scalar` and `where-record`

The prediction was: take the row with the most copies whose rule is a pure
function of the value, give it one statement every engine calls, delete the
copies, and keep the wording byte-identical. The census changed which row that
is, and the reason is the next section. The row taken is the user's own
predicate, and it moved as far as it can move before §2.3's constructor exists.

**The decision left `vyrn-codegen`.** `validation_required(from, to, types)` —
which declaration's `where` a value crossing from one type into another must
satisfy — was the one copy for the two compiled backends and it was in the
wrong crate. The interpreter lives in `vyrn-frontend`, which `vyrn-codegen`
depends on, so it could not read that decision and asked the same question for
itself. It is `vyrn_frontend::validate::required` now, beside `trap` and for
the reason RFC-0101 §6.4 put `trap` there; `vyrn-codegen` keeps the old name as
a re-export, so both backends ask the same words they always did.

`validate::of` takes the declaration the caller already looked up rather than
the map, because the engines hold their declarations differently — the emitters
key by `String` and own the values, the interpreter keys by `&str` and borrows
them. Taking one engine's map would have made the rule one engine's map shape,
which is how a rule ends up with two spellings again.

**The interpreter went from three statements to one.** It refused a value for
its type's `where` at `Age(n)` (`construct`), at a record literal
(`Expr::StructLit`) and at every typed boundary (`coerce_walk`). Each ran a
predicate; each spelled its own wording. Only the third built the wording from
the declaration, so the constructor path would have handed a record base the
scalar sentence and the literal path always spelled the record one. `enforce`
is the one statement now, and `validates` learned the second binding form:
`validate::is_cross_field` decides whether the predicate sees the fields or
`value`, and `trap::validation_of` decides the sentence by the same fact, so
the two cannot disagree.

**What the copy count does and does not show.** The census's column counts
CARRIERS — engines that state the rule themselves — and for these two rows it
is 3 before and 3 after, because the interpreter still runs the predicate in
Rust over a `Val`. What fell is the number of statements inside the carriers:
five sites checked and two decided, and now three check and one decides. The
census table is unchanged at 19 rows and 53 copies, and this paragraph is here
so that a later reader does not take an unchanged table for an unchanged tree.

The programs that must still refuse are the census's own:
`tests/boundaries/where-scalar.vyrn` prints ``validation failed for `Age` `` and
`tests/boundaries/where-record.vyrn` prints
``validation failed: `Range` violates its `where` clause``, both on stderr at
exit 1, and both byte-identical under the interpreter, the compiled wasm and
the native binary. Parity, the fixtures and the manifest hold: no example's
output changed, so no recorded byte changed either.

#### What did not move, and why

No row's carrier count fell, and the census is what makes the reason exact
rather than a suspicion.

- **There is one mechanism by which all three engines run one body**, and it is
  `loader::RtModule` — a builtin that IS an exported function of a std module,
  so the interpreter interprets it and both emitters compile it. RFC-0078 M4c
  used it for the six codecs, and `char-boundary` and `json-decode` are the
  census's two one-copy rows because they took it. `RtModule`'s own doc states
  the price: "Adding a builtin to the runtime is now an ENTRY here plus a
  deletion in each engine."
- **A row can take that mechanism only if its rule is expressible over ordinary
  Vyrn values.** `string-nul` and `string-utf8` are pure functions of an
  `Array<UInt8>` and would qualify — except that the function they belong to,
  `stringFromBytes`, must BUILD the String it validates, and that needs the
  raw-memory primitives `std/mem` fences. Splitting the check from the build
  means an unchecked builder primitive, which is exactly §2.3's constructor and
  exactly not a slice that lands beside a census.
- **Seven rows are one or two instructions per site.** `array-index`,
  `string-index`, the four arithmetic rows and `int-narrowing` are lowered
  inline by both emitters. Routing them through a call would trade the thing
  §1.4 measured — a place-based core an optimizer can work with — for a rule
  stated once, and RFC-0101 §3.0's second rule says a copy goes when its
  replacement's gate is green, not before.
- **`region-depth` cannot be a call** (finding 6) and `io-status` should not be
  a type (finding 7).
- **The interpreter's copy is not this milestone's to delete** (finding 3).
  PLAN-0125-runtime §5.2 records why it cannot call the Vyrn statement, §5.1
  lists the six byte-in, byte-out families that could go through the embedded
  engine, and M5 removes the rest by removing the interpreter. For the five
  `String` and I/O rows the native copy is M4 step 3's, not M6's. So seventeen
  rows read three today, and the path to one runs through M4 and M5 before it
  runs through a rewritten rule.

What the next slice inherits, in order: the unchecked builder primitive that
lets `string-nul` and `string-utf8` become a constructor over ordinary Vyrn
values; then the same shape for `int-narrowing`, whose rule is the smallest
pure function of its value in the census and whose constructor is the one §2.3
names — `UInt8` from an `Int64`, answering `Option` or trapping at one site
named by the type; then the kernel's third judgment over the core, which is
what makes a boundary check unnecessary rather than shared.

#### The sixth slice (2026-09-03)

The sixth slice decides finding 7, deletes the floor's call machinery, and
records the audience decision. What the floor decides on its own is one
declaration now, and it keeps no list of capability-carrying names at all.

**7, decided: the `extern` row is a CALL.** RFC-0103 M2's finding 3 made the
declaration the carrier and gave one reason — an unanswered import stops
instantiation, so a program that never calls the import still cannot start.
The reason stopped being true. The direct backend (RFC-0077) emits an
`(import "vyrn" ..)` for a host function the call graph reaches and for no
other, so a declaration nothing calls produces no import at all. Read off the
emitted module, three programs:

| program | `vyrn` import emitted | what the artifact does |
|---|---|---|
| declares `jsAdd`, calls nothing | none | `wasi`: instantiates, prints, exits 0. `native`: links, prints, exits 0 |
| declares `jsAdd`, calls it from `main` | `(import "vyrn" "jsAdd" ..)` | `wasi`: a host without the namespace cannot instantiate. `native`: the call traps, `` extern `jsAdd` is not available on this target `` |
| declares `jsAdd`, calls it from a function `main` never reaches | none | as the first row |

Both prediction programs are in `vyrn-cli`
`floor::an_unreached_host_import_is_no_capability`. The first row is what the
old rule refused and the artifact runs; the second is what both rules refuse,
in one wording, byte-identical under `VYRN_NO_JUDGE=1`. The third row is the
backend sweeping HARDER than the floor: it drops a call `_start` cannot reach
where the judgment keeps every non-generic instance, so the floor stays the
wider of the two and never accepts a program whose artifact holds the import.
RFC-0103 carries the decision as a dated addendum and its vocabulary table
carries the row; the refusal, its wording, its chain and the target sets are
unchanged.

Nothing in the corpus changes verdict. The three client artifacts whose
`std/rpc` and `std/connect` stubs declare `vyrnRpcCall` are `browser`, and a
page HAS `extern`; a stub calls its own import in the same module, so the one
refusal the tree relies on — the same client retargeted to `native` — is the
refusal it was. `examples/externdemo.vyrn` is no artifact's entry and the floor
never touched it.

**What was deleted.** `floor::CALLS` and `floor::JUDGED`, 41 lines with their
reasons, and the `extern fn` declaration scan, 21 more. The floor's vocabulary
is the LATTICE's rows now, read through one match (`floor::Capability::of`):
`fs` is three effect rows, `stdin` is one, `args` is one, `extern` is one, and
every other effect is `None` for one of RFC-0103 M2's two opposite reasons —
every target has it, or no compiled target does. `floor::call_carrier` is the
one reading of a call site, and the scan, the judgment
(`vyrn_lower::effects::reaches`) and the effects harness all make it instead
of each holding a copy. A `Carried` records whether the scan found a CALL or a
DECLARATION, so `floor::is_judged` asks the scan rather than matching names.

The count is a wash and the rule is not: 62 lines of two constant tables and a
second scan out, 58 lines of derivation in, `floor.rs` 829 lines to 851. What
was two lists to keep equal is one table read twice.

**What the floor still is.** A scan and a declaration check, and neither is a
verdict on a call.

- The SCAN finds every carrier and writes every refusal — the carrier it
  quotes, the line it found it on, the import chain, and the `connect(..)`
  remedy. The breadth-first walk in `floor::locate` is the IMPORT graph and
  stays: the shortest chain to the offending module is the diagnostic.
- The DECLARATION check is `logging { sink: file(..) }` (finding 10). A
  declaration is no call, no effect set holds one, and RFC-0103 §4's reason
  stands. It is also the one `fs` reach that degrades SILENTLY in a page, so
  the pass keeps deciding it inside the load, before every type error.
  `floor::the_log_sink_is_a_declaration_the_judgment_does_not_clear` pins it,
  byte-identical under the knob.

The VERDICT on every call is the judgment's. The floor holds it back for the
check (`floor::defer`, `floor::decide`) and drops the rows no instance
reaches.

**The knob, and what it means now.** `VYRN_NO_JUDGE=1` stays, and it restores
two things rather than a second list. The floor goes back to PRESENCE: the
scan's carriers are refused whether or not an instance reaches them, inside
the load and before every type error, which is the rule and the order of
RFC-0103 M2. The generation fence goes back to its own two cells (fifth
slice): `print` is allowed in a `gen fn` again and the three host-boundary
names are externs again. One knob, because the two are one milestone.

**The audience pass: it stays, and this is the record.** RFC-0103 §4 says what
a fence is for, and the tally says the judgment agrees: 414 *declared-only*
functions — a route handler, a page, a store's shape — are server-only or
client-only with no target-restricted effect at all, and **0 unfenced, 0
server-extern**. A fence protects a DECLARATION. A secret in a constant uses
no capability, so no effect set can hold the rule and no judgment can replace
it. No audience code path is dead by the tally either: `audience.rs` is path
classification, the widening rule, the remedy and the display, and every one
of them is the declared boundary rather than a reach. Nothing was deleted, and
the reason it was not is the finding.

**The tally.** `cargo test -p vyrn-cli --test effects`, at this slice's last
commit:

    effects over the corpus: 180 programs (31 not loadable here, 2 refused as
    recorded), 26883 functions judged, 8244 pure, 0 unlowered, 64 calls
    through a function value judged over their sources, 69 unattributed
      open sets: 40
      effect 18218  alloc
      effect    11  read-input
      effect   292  write-output
      effect   189  fs-read
      effect    19  fs-write
      effect   106  fs-list
      effect     8  args
      effect    29  clock
      effect    20  random
      effect    31  extern
      effect    22  serve
      effect     4  spawn
      effect  7785  trap
      effect   826  gen-only
      spawn:      12 sites judged, 0 outside `alloc, trap`
      floor:      26625 agree, 42 callee-carried, 216 gen-body, 0 floor-blind,
                  0 core-blind
      audience:   26442 no fence, 27 agree, 414 declared-only, 0 unfenced,
                  0 server-extern
      judged:     26625 agree, 42 callee-carried, 216 gen-body, 0 differ

The function count is the fifth slice's: the judgment did not change, only who
reads it. *callee-carried* rises from 24 to 42 because `extern` is compared
now — eighteen functions whose extern comes from a callee, which is the floor
agreeing over the closure. **Both ratchets are 0.** The `floor:` and `judged:`
lines are one line twice: every call the scan finds is judged.

**The M6 gate.** "The audience, floor and contract test suites green with
their passes deleted." Where each stands:

- **floor** — `cargo test -p vyrn-cli --test floor`, 12 passed. The pass's
  call machinery is deleted and its verdicts are the judgment's; the scan and
  one declaration check remain, and the RFC says above why a scan is not a
  second statement of the rule and why a declaration cannot be an effect.
- **audience** — `cargo test -p vyrn-cli --test audience`, 21 passed. The pass
  is NOT deleted, by the decision recorded above; the fence is a declared
  boundary and the judgment replaces nothing of it.
- **contracts** — `cargo test -p vyrn-cli --test contracts`, 12 passed.
  Untouched by this milestone.

What still stands between here and the gate is M6's other two sentences, which
no slice has started: validation by construction in place of the boundary
checks, and the trap primitive and its table in place of the sites. Finding 14
was open here — 69 calls through a function value the join could not
attribute, 40 open sets — and it bounded neither: the ratchet counted it as
unattributed rather than as a disagreement. The seventh slice below closes it,
and the gate has no hole from it.

#### The seventh slice (2026-09-04)

The seventh slice closes finding 14. The judgment answers for every call in
the corpus: **0 unattributed, 0 open sets**, and the test asserts both
counts exactly rather than as a bound.

Three things were missing, and none of them was a value holding a function
from outside a closed set. Every one was a body or a source the collection
did not reach, so the answer to the question the finding poses — a gap in
what is collected, or a genuinely open set — is the first, in all three.

**1. A `test` or a `bench` body had no core, because the checker checked a
CLONE of it.** `check_tests` and `check_benches` built a synthetic
`test@<i>` / `bench@<i>` function whose body was `t.body.clone()`. The
checker keys its recorded answers by node ADDRESS (RFC-0101 M1), so the
types landed on the clone and the real nodes — the ones `own`, the lowering
and the interpreter walk — stayed untyped. The third slice measured what a
core over them would cost at 3,663 gaps and left the body out. The head is
still synthetic and the body is the real node now (`Checker::function_body`,
one line of `Checker::function` split out), which is the whole of the
frontend change: same statements, same diagnostics, one address instead of
two. `Lowered::bodies` carries each body's rows, walked as `predicates` are
and NOT followed into the worklist — a test body is no part of an artifact,
so a generic it alone calls is an instantiation no backend emits — and
`core::build_outside` builds it under the name `own`'s release plan is keyed
by. A module-state initializer already had its core since the third slice;
this is the other half.

One core refused eleven of those bodies: `"abc".byteLength` is a place whose
base is a literal, which `core::place` had no name for. It binds a temporary
now, which is what the arm above it does for every other unnamed receiver.
No instance in the corpus writes one, so the arm fires only inside a test
body; unlowered is 0 in the effects harness, and the kernel's tally does not
move — it builds no test body.

**2. Whether a function value was collected turned on how the parameter's
type was SPELLED.** RFC-0037 collects a defunctionalization source at a
STORED position — a `let` annotation, a record field, an element, a return.
A call ARGUMENT is checked by `Checker::check_fn_arg` instead, which
monomorphizes it (RFC-0023) and recorded nothing. And the two paths are
chosen by the parameter's type node: `f: Bump`, a named alias for
`fn(Int64) -> Int64`, is not a `Type::Fn`, so it falls through to the stored
path and IS collected; `f: fn(Int64) -> Int64` goes to `check_fn_arg` and is
not. One value, one closed set, two answers by the spelling.

`check_fn_arg` records the argument now, in the two arms that introduce a
function — a lambda literal and a bare function name. The third arm, an
expression of `fn` type, forwards a value some other position already
collected, so the set stays closed by construction. The signature recorded
is the parameter's type under everything the call has solved, so a generic
`fn(T) -> U` is the concrete signature the instance calls through.

The rows are a list of their own, `StoredFnEffects::arg_sources`, and NOT
part of `sources`. The two positions are different things: a stored value
carries a defunctionalization tag and one enum variant, an argument carries
neither. The spawn-safety fixpoint and the `--workers` gate read `sources`
alone and their verdicts do not move; the effect judgment reads both
(`every_source`), because a parameter's call reaches whatever a caller
handed it, whichever route the value took.

**3. A projection is never flattened into `Program::functions` (RFC-0091
M2), so the name the core calls it by named no body.** The core lowers
`doc.field("items")` as a call to `field`, and neither the instance table
nor the flattened-method table holds that name — twenty calls in
`jchain.vyrn`, `jsonplace.vyrn`, `namedplace.vyrn`, `protoplace.vyrn` and
`tryplace.vyrn` were unattributed for it. `Lowered::places` carries each
`impl` projection's rows, walked as `predicates` are and not followed for
the same reason (a projection is inlined at its site), and both readers —
the harness and the floor's judge — build its core under the empty
substitution a declaration has and resolve the surface name to it. A
projection body can trap and allocate, so this is not a bookkeeping change:
the join now covers what an access site runs.

**An empty set is an answer, not a hole.** Six function types are left where
the closed set is EMPTY — the program declares the type and holds no value
of it, so the call cannot run. The judgment says so with `Callee::Empty` and
the tally counts them apart from an unattributed call:

| program | type | why no value exists |
|---|---|---|
| `examples/bin/client/boot.vyrn` | `fn(RpcReply<PasteList__from0>)` | `pastes/recent` is server-rendered and needs no client callback — the file says so on line 58; `std/rpc` still generates the deliverer |
| `examples/shelf/client/boot.vyrn` | `fn(RpcReply<TagCounts>)` | the same, for `books/tags` |
| `examples/fullstack/server.vyrn` | `Feed` | nothing in the program calls `sse` or `ws`, and `std/http`'s `httpFeed` is a non-generic function, so the lowering roots it whether or not it is reached |
| `examples/pagesdemo.vyrn` | `Feed` | the same |
| `examples/rest.vyrn` | `Feed` | the same |
| `examples/shelf/server.vyrn` | `Feed` | the same |

Every one is a generated or library body the worklist roots and the program
never calls. Judging such a call pure is not an approximation: no value can
reach it. `tests/effects.rs` asserts the count is 6 and lists them on
failure.

**The tally.** `cargo test -p vyrn-cli --test effects`, before and after, on
the same 180 programs. The BEFORE column is this slice's own re-measure at
its parent commit, not the sixth slice's printed numbers: the third
judgment's fourth slice added the `where` constructors, so the corpus's
function count moved between.

| line | before | after |
|---|---|---|
| functions judged | 27,131 | 27,131 |
| pure | 8,368 | 8,358 |
| unlowered | 0 | 0 |
| calls through a function value | 64 | 109 |
| through one whose set is empty | — | 6 |
| **unattributed** | **69** | **0** |
| **open sets** | **40** | **0** |
| empty sets | — | 6 |
| floor agree | 26,873 | 26,865 |
| floor callee-carried | 42 | 50 |
| floor gen-body | 216 | 216 |
| judged differ | 0 | 0 |

Ten functions stop being pure and eight move from *agree* to
*callee-carried*: their `extern` comes from a callee the join reaches only
through a function value or a projection. `alloc` rises 18,218 to 18,228,
`extern` 31 to 39 and `trap` 7,909 to 7,911, and no other row moves. **Both
ratchets are 0**, and `judged: 0 differ` — the moved floor rows and the
judgment still give one program one answer.

**Finding 14 is closed, and the M6 gate has no hole from it.** Every call in
the corpus is one of: an atom, a body the join names, a projection whose
body it names, or a call through a function type with no value in the
program. None of them is a call whose effects the judgment cannot bound.
What the count does NOT prove is a property of the language: the argument
position is closed by construction because `check_fn_arg` accepts three
shapes and the third forwards, and a projection is closed because it is
inlined. A route that let a function value into a program without passing
either would be a new source list, not a raised number, and the exact
assertion is there so it is found that way.

#### The third judgment's second slice (2026-09-03)

The census sorted every value boundary into five lines. This slice takes two
of them: the trap table becomes a table on the wasm route, and the judgment
itself runs beside the linear and the effect judgment.

**The trap table.** §2.3 says the emitter "maps `trap` to a call with a table
index". It did not. Eight rows of the census — `array-index`, `string-index`,
`int-div-zero`, `int-rem-zero`, `int-div-overflow`, `shift-range`,
`call-depth`, `region-depth` — each had their WORDING interned as a private
field of the wasm backend's runtime record (`msg_div0`, `msg_aoob`,
`msg_oob_end`, six more), and each site pushed the pointer it needed. Two rows
carry a number in the middle of the sentence, so they went through a second
runtime function with a three-piece protocol (`trapIdx(pre, i, post)`) that
existed only because those two rows are shaped differently from the other six.

Now `trap::Rule` is the table: eight rows in one order, each stating its two
halves — what stands before the value and what stands after it, or nothing for
a row with no value. The emitter lays those addresses out as one data segment
and every site pushes A NUMBER. `std/runtime`'s `trapAt(rule, v, table)` reads
the row and writes it. `trapIdx` is deleted, the nine interned wordings are
deleted, and the emitter spells no sentence: the two shapes the old code told
apart at eight sites are told apart once, by a zero in the row's second half.

Seven of the eight rows reach it through the function's ONE trap site (M1):
the check parks the row's number and the value in the site's two locals and
branches out, so a division check now costs a compare and a branch where it
used to cost a compare and a call — the same shape M1 measured for the bounds
check (3.56 s against 1.71 s on nbody's inner loop). The eighth is
`call-depth`, whose check is the prologue: it stands before the block a branch
would target, so it calls `trapAt` itself. Finding 6's `region-depth` is not
an exception here — its counter stays in the prologue's neighbourhood, but its
check is inside the body and takes the site like the rest.

`direct.rs` falls from 16,547 lines to 16,532 at this commit (16,529 after the
third one below) and `std/runtime.vyrn` rises from 1,942 to 1,952: the
deletion is fifteen lines of emitter and the addition is ten lines of Vyrn,
which is the trade §2.3 asks for and not a line count worth celebrating. The
module BYTES rise, and the reason is worth recording rather than hiding: a
site that parks two locals and branches is three instructions where a site
that pushed a pointer and called was two.
`nbody.wasm` goes from 10,847 to 10,913 bytes, `fannkuch.wasm` from 7,637 to
7,789, `jsoncodec.wasm` from 49,610 to 49,997 — 0.6, 2.0 and 0.8 per cent.
The call sites are what the engine paid for, and they are gone.

Every wording is byte-identical, which is what the census's own programs
prove: all eight rows answer the same bytes under `vyrn run`, `vyrn run
--engine wasm` and a native binary, and the boundaries suite, the fixtures and
parity are the gate. `rfcs/census/wasm-sha256.tsv` is NOT regenerated in this
slice: the trap sites changed, so the recorded module hashes changed, and a
hash regenerated in the same commit that changed the bytes records nothing.

**The judgment.** `vyrn-lower/src/typed.rs`, beside `kernel.rs` and
`effects.rs`, over the same form. It is a use-def walk and nothing else: every
name of a body is bound once, so the producer of a name is a lookup. For every
store into a place whose type is validated — a `let`, an assignment, a field
or an element — it asks what produced the value, and the three answers that
are the rule are the type's own constructor, a name already of the type, and a
literal the checker proved.

WHICH crossings are validated is not the judgment's to decide. It asks
`vyrn_frontend::validate`, which is where the fifth slice put the rule: the
`where` rows through `validate::of`, and the two narrowing rows through
`validate::narrows`, which is new here and states in one place what the three
engines each write instructions for — a crossing that changes the width or the
signedness re-reads the low bits and the sign, and the same pair does not.
`tests/typed.rs` is the corpus tally, `VYRN_TYPED_DUMP=<file>:<fn>` prints one
body's judged stores, and the ratchet is on the findings.

The tally over 180 programs, on 2026-09-03:

| answer | stores |
|---|---|
| by-constructor | 46,473 |
| by-literal | 9,103 |
| by-name | 349 |
| findings | 6 |
| **judged** | **55,931** |
| unjudged | 94,691 |

**RATCHET 6.** The six findings, each with its program and line:

1. `examples/bin/server/store.vyrn:107`, `createPaste` — a PRIMITIVE into
   `Created`: `let bumped: Created = store.counter + 1`. The sum is an
   `Int64` and the slot is validated.
2. and 3. `examples/shelf/server/store.vyrn:84`, `rateBook` — a READ OF A
   PLACE into `Stars`: `let s: Stars = req.rating`, where the request's field
   is a plain `Int64`. Two, because two entry points of the project reach the
   module.
4. `examples/autovalidate.vyrn:46` — a RECORD LITERAL into `Range`.
5. and 6. `examples/inlinewhere.vyrn:15` and `:19` — a record literal into
   `User`.

**None of the six is a defect**, and the probe is what says so rather than an
argument: `rfcs/probes-0125/raw-value-into-a-validated-slot.vyrn` runs all
three shapes with a value that breaks the predicate, and all three refuse
under `vyrn run`, `vyrn run --engine wasm` and a native binary, in the
census's own words — `validation failed for `Small`` and
``validation failed: `Pair` violates its `where` clause``. They are the sites
the boundary check exists FOR. That is the judgment's real answer: the check
is not missing anywhere, it is present everywhere, and §2.3's constructor is
what makes it unnecessary rather than what makes it correct.

Two of the six shapes are one shape. A record literal of a validated record
type is a SECOND producer for that type, beside the constructor, and it is the
`where-record` row's whole reason for existing. The other two — a primitive
and a read of a place — are the value boundary the interpreter's `coerce`
takes 97.6 per cent of the time for nothing.

**What the judgment cannot see, and it is the same fact twice.** 94,691 stores
are unjudged, every one of them into a sized integer whose producer has no
type in the core: `Rhs::Prim` erases the operator, so `a + b` and `UInt8(n)`
read alike, and a builtin's result type is nobody's declaration. A narrowing
IS a store whose producer is of another width, so a judgment that guessed
would call every integer store one. That number is not a gap in the walk; it
is the size of what §2.3's constructor closes, stated as a count. When
`UInt8` is a producer with a name, 94,691 stores become answerable by the
lookup that already answers the other 55,931.

The judgment holds no table of builtins. The caller answers what a callee
returns, and the three the corpus stores through — `copy`, `swapRemove` and
an index — hand their RECEIVER's value back, which the caller reads off the
argument type. Before that rule the corpus showed 120 findings and every one
of them was a copy of a `Title` reported for not being a `Title`.

**The constructor rows' first move.** The judgment finds no store into a
sized integer whose producer is a wider value and not the conversion — every
one it can name goes through `UInt8(..)` or a literal the checker proved. So
the two narrowing rows move as far as they can move before §2.3's constructor
exists, which is the same distance `where-scalar` moved in the fifth slice:
the DECISION leaves the engine that held it.

`validate::narrows` says which crossings re-read the bits, `validate::wrap`
says what an integer reads as at a width, `validate::from_float` says what a
float reads as, and `validate::width` says which types are integers at all.
The interpreter's `wrap_intn` was the third of those, private to `interp.rs`
at ten sites; the wasm emitter's `Num::of` was the fourth, written again in
`direct.rs`. Both ask now. `convert_val`'s four float arms — signed and
unsigned, `Float64` and `Float32` — become one call, and
`coercion_is_noop`'s own "already at this width and signedness" becomes
`narrows` read the other way round.

The census's copy column does not move, and the fifth slice already said why
it would not: the interpreter re-reads bits in Rust over a `Val`, the wasm
emitter emits `i32.wrap_i64` and a mask, the native backend emits `trunc`.
Three carriers, three representations, and no call can join them — finding 3
and the paragraph on the seven inline rows. What falls is the number of
STATEMENTS inside the carriers: seven decisions about width and truncation
across two engines, and four of them are now one function each in
`vyrn_frontend::validate`, which is the crate all three can read.

#### The third judgment's third slice (2026-09-03)

The second slice ended on a number it could not reduce: 94,691 stores unjudged,
every one into a sized integer whose producer had no type in the core. This
slice gives every producer a type and judges them all.

**The producer type.** A right-hand side of the core now names the type it
makes. `Rhs::Prim` carries the operator's own result and `Rhs::Call` what the
callee answers at that site. Both are the CHECKER's answer at that node, read
off the pair the form already carries — `Row::has` where the node's own shape
settles its type, and `Row::ty` where the two are one answer (RFC-0101 §2.1
item 2 [A16]). The lowering guesses nothing and the checker records nothing
new: the half that says what a value HAS was already derived, and this slice is
the first reader that needed it at a `prim`.

One class of node has no row of its own — a projection the checker expanded at
the site (RFC-0122) — and it answers from its declared result under the
receiver's type arguments, which is the answer `core::ty_of` already reads
there. Five calls in the corpus. `tests/coretables.rs` pins the rest at zero:
over the whole corpus, 145,678 calls, 183,079 primitives, 5,968 literals,
25,485 names, 17,998 reads and 442 takes, and **not one right-hand side without
a producer type**. There is no exception list because there is no exception.

**What the judgment does with it.** A store into a sized integer is judged by
the lookup that already answered every other store. The producer at the
destination's width and signedness crossed nothing, which is
`validate::narrows` read the other way round — the same exemption `required`
states at the `where` rows, and the reason `Int` and `Int64` are one width
written two ways rather than a finding. The type's own conversion is its
constructor. A literal is the checker's. A primitive over LITERALS ONLY, into a
sized integer, is a fourth answer and it is named apart from the third: the
checker ranges a constant against a destination of the same sign — `-200` into
an `Int8` is refused, at compile time — and where the signs differ the
`int-narrowing` row answers rather than refuses, so `-1` into a `UInt8` is 255,
which is the row's answer and the same fact as `UInt8(300)` being 44. It is not
a finding, and calling it `by-literal` would have claimed a proof that half of
it does not have.

The tally over the same 180 programs, on 2026-09-03:

| answer | before | after |
|---|---|---|
| by-constructor | 46,473 | 46,473 |
| by-literal | 9,103 | 9,103 |
| by-name | 349 | 156,963 |
| by-constant | — | 1,080 |
| findings | 6 | 6 |
| **judged** | **55,931** | **213,625** |
| unjudged | 94,691 | **0** |

**RATCHET 6, and they are the same six** — one primitive, two reads of a place,
three record literals, each recorded above with its program and line, and each
a legitimate boundary site that all three engines refuse
(`rfcs/probes-0125/raw-value-into-a-validated-slot.vyrn`). No new finding, so
no new probe.

`by-name` rises by 156,614 because the second slice did not COUNT a store it
could not ask about. Two kinds joined. The 94,691 it counted as unjudged: 1,080
of those are the constants above and the other 93,611 are `by-name`. And 63,003
more it dropped silently — a store whose source type resolved and whose width
matched was no crossing, so the rule answered "no row" and the store left no
row in the tally either. A store that owes nothing is still a store the
judgment looked at, and it is counted now. The 366 stores into a named `where`
type are unmoved; the rest are the sized-integer rows: 137,076 `Int32`, 33,129
`UInt32`, 25,097 `UInt64`, 17,938 `UInt8`, and nineteen at the other three
widths.

**Unjudged is 0, and the last twenty-five were the harness's.** They were a
byte read out of a `String` — `for b in s`, `s[i]` — where the corpus harness
resolved a place's element type for an array and not for a String. The
`string-index` row says what a String indexes as, so the harness says it too.
Nothing in the judgment changed for them.

**What the judgment now proves, and what it still does not license.** Every
narrowing store in the corpus — every store into a sized integer whose producer
is of another width — goes through that type's own conversion, or is a literal
or a constant the program wrote out. Not one is a raw wider value. The second
slice predicted that over the stores it could see; this slice has seen them
all.

That proof licenses no deletion, and the reason is the shape of the two rows
rather than the state of the tree. `int-narrowing` and `float-to-int` REFUSE
NOTHING — finding 4 of the census. What each engine writes out for them is the
wrap itself, which is the conversion's meaning and not a check in front of it:
the interpreter re-reads bits in Rust over a `Val`, the wasm emitter emits
`i32.wrap_i64` and a mask, the native backend emits `trunc`. Deleting any of
the three deletes the answer. The DECISIONS around them left their engines in
the second slice and are `vyrn_frontend::validate`'s — `narrows`, `wrap`,
`from_float`, `width` — and a judgment that finds no unchecked narrowing gives
no third thing to remove. The census is therefore unchanged at **19 rows and 53
copies**, and `tests/boundaries.rs` derives that sentence from its own table so
that an unchanged count stays a measurement.

The `where` rows are the ones a deletion waits on, and the judgment says
plainly that it cannot yet clear them: six stores reach a validated slot
without the constructor, and a record literal of a validated record type is a
second producer by design. §2.3's constructor is what closes that, and it is
the next slice's, not this one's.

#### The third judgment's fourth slice (2026-09-03)

The third slice ended on the two rows a deletion waits on. This slice builds
their constructor, points all three engines at it, and deletes what they each
wrote out.

**Where the constructor lives: it is a function of the program.** For every
declaration that carries a `where`, `vyrn_frontend::ctor` generates two
ordinary Vyrn functions and `check_and_synthesize` appends them to the linked
program beside the JSON walks:

```text
fn where$p<Name>(binds..) -> Bool   the `where` clause itself
fn where$c<Name>(value: Base)       calls it, or panics in the census's words
```

The three options were a generated function in the program, an entry in
`std/runtime` taking a predicate index, and an entry in `validate` each engine
calls. The last two cannot be built: a `where` clause is the USER'S expression,
so no fixed body in `std/runtime` and no function in `validate` can hold it —
only the program can. That leaves the mechanism the census named as the one
that works, `loader::RtModule`'s: a function the interpreter interprets and
both emitters compile. `jsonenc` and `jsondec` already inject per-type walks
this way (RFC-0078 M2b, M3), and `jsondec` already synthesized a
`Bool`-returning function whose body is a `where` clause — this slice makes
that shape the only statement of the rule rather than a second one.

The pair rather than one function, because a fallible construction (`Age?(n)`,
RFC-0077 M2k) wants the same answer without the trap. Two spellings of "run the
predicate" could disagree about what `value` means; one function called by both
cannot.

**The constructor answers nothing, and that is not a compromise.** It takes the
raw value and returns Unit. A validated value's runtime representation IS its
base — `Interp::construct`'s own words, "zero overhead" — so the caller
already holds what the constructor would answer. Returning it would cost twice:
the `return value` inside the constructor crosses into the validated type,
which is the boundary that runs the predicate, so the function would call
itself for ever; and a record base would have to be moved out and back for a
check that cannot write to it.

**What each engine lost.** The interpreter's `validates` built a scope and
evaluated the clause; it calls `where$p` now, and `enforce` calls `where$c`, so
the sentence on stderr is written by one `panic` in one Vyrn body. The textual
emitter's `emit_validation` and `emit_predicate_cond` lowered the predicate to
LLVM at three sites — a coercion, a construction and a record literal — and
each is one call now; its per-type `@.trap.verr.*` globals and
`validation_message` are deleted, so that backend spells no validation wording
at all. The direct wasm backend's `emit_validation` bound every field by
`predicate_binds`, walked the clause and interned the sentence; it parks the
value in a local and calls, which is three instructions where the binding walk
was a page. `Cx::predicate` and `Gen::decls` — the program's own predicate
node, read at every validation site — are deleted with them.

`direct.rs` falls from 16,592 lines to 16,532, `lib.rs` from 19,099 to 19,022
and `interp.rs` from 11,631 to 11,625, against 204 lines of `ctor.rs`. That is
the trade §2.3 asks for: 143 lines of three engines for one generator of two
small functions.

**The census.** `where-scalar` and `where-record` each fall from three carriers
to one, `vyrn`, and the table says **19 rows and 49 copies**.
`tests/boundaries.rs` derives that sentence from its own table, so the count is
still a measurement and not a claim; the two rows' programs answer the same
bytes under `vyrn run`, `vyrn run --engine wasm` and a native binary, as they
did before. Four of the nineteen rows are stated once now, and they are the
four that took the same mechanism.

The price is stated rather than hidden. A declaration that carries a `where`
now costs two functions in every module that declares it, reached or not, and
the constructor's sentence is in the data segment whether or not anything
crosses into the type. `VYRN_WASM_MANIFEST=check` therefore fails on 36 of
the 172 examples — every one that declares a `where` type. `direct.rs`'s
`a_validated_type_is_checked_wherever_it_is_reached` had asserted on the
message's ABSENCE for an unreached declaration; the message is the
constructor's own string now, so the test compares a reached module against one
whose value the checker proved instead. Emitted bytes changed, so
`rfcs/census/wasm-sha256.tsv` is NOT regenerated here, for the second slice's
reason: a hash written in the commit that moved it records nothing.

**The judgment.** The tally over the same 180 programs, on 2026-09-03:

| answer | before | after |
|---|---|---|
| by-constructor | 46,473 | 46,482 |
| by-literal | 9,103 | 9,103 |
| by-name | 156,963 | 156,966 |
| by-constant | 1,080 | 1,080 |
| findings | 6 | 0 |
| **judged** | **213,625** | **213,631** |
| unjudged | 0 | 0 |

**RATCHET 0**, and `tests/typed.rs` asserts equality now rather than a bound: a
store into a validated place whose producer is raw is a refusal there, not a
row in a record. The six went two ways.

Three were record literals — `autovalidate.vyrn:46` into `Range`,
`inlinewhere.vyrn:15` and `:19` into `User` — and they were never findings in
the first place. A record literal of a validated record type is that type's
SECOND producer by design: RFC-0003's cross-field `where` has no other
spelling, and since this slice all three engines run the generated constructor
at it. The judgment names `Rhs::Make` into a validated place a constructor for
that reason.

Three were the program's, and the program says so now.
`bin/server/store.vyrn:107` was `let bumped: Created = store.counter + 1` and
is `let bumped = Created(store.counter + 1)`; `shelf/server/store.vyrn:84` was
`let s: Stars = req.rating` and is `let s = Stars(req.rating)`, reached by two
entry points and therefore counted twice. Neither rewrite changes what runs —
the boundary was going to call the constructor either way — and both make the
producer the one the judgment can name.

**Zero is a fact about this corpus, not a new rule of the language.** A raw
value entering a validated slot is RFC-0003's automatic validation and stays
legal; refusing it would delete a documented feature, which is a different
RFC's decision. What the judgment now proves is narrower and worth having: over
180 programs, every value that reaches a `where` type reaches it through that
type's own constructor, and there is exactly one constructor.

**What the remaining constructor rows wait on.** The design sorted six rows
into this line and two of them are done. Of the other four:

- `string-nul` and `string-utf8` wait on what the fifth slice already named:
  their function, `stringFromBytes`, must BUILD the String it validates, and
  that needs the raw-memory primitives `std/mem` fences. This slice does not
  supply it. The `where` rows could take a constructor precisely because they
  build nothing — the value already exists and the constructor only judges it.
  An unchecked builder primitive is still the missing piece.
- `int-narrowing` and `float-to-int` REFUSE NOTHING (finding 4 of the census).
  What each engine writes out for them is the wrap itself, which is the
  conversion's meaning and not a check in front of it, so a constructor has
  nothing to take: deleting the wrap deletes the answer. Their DECISIONS left
  their engines in the second slice and are `vyrn_frontend::validate`'s. They
  move when the language decides what a program that narrows means, which is
  §2.3's open question and not a slice's.

The interpreter's copy is gone for these two rows and no other, and the reason
is worth stating: the `where` rows are the only ones whose rule is written in
Vyrn by the USER. Every other row's rule is the compiler's, so moving it means
writing it in Vyrn, and PLAN-0125-runtime §5.2 is why the interpreter cannot
then call it. Finding 3 stands for the remaining fifteen rows.

#### The third judgment's fifth slice (2026-09-04)

The fourth slice built a constructor for the two rows whose rule the USER writes.
This one takes the two rows whose rule the COMPILER writes and that the previous
four slices kept naming as blocked: `string-nul` and `string-utf8`. They were
blocked for one stated reason — "`stringFromBytes` must BUILD the String, which
needs the `std/mem` primitives" — and the reason is about the build, not about
the check. So this slice separates them.

**Where the check lives: `std/text`, as an ordinary exported function.** The
CHECK over the bytes is a pure predicate: it reads an `Array<UInt8>` and answers
a number, allocating nothing and touching no primitive. So it takes the
mechanism the census named — a Vyrn function the interpreter interprets and both
emitters compile — and it takes it as an entry in `RT_MODULES` rather than as a
generated function, because unlike a `where` clause its body is fixed and
belongs to the compiler:

```text
fn stringFault(b: Array<UInt8>) -> Int64   0 fine, 1 a NUL byte, 2 not UTF-8
```

`std/text` rather than a new module, because the UTF-8 ranges were already
written there. `decodeUtf8` has decided the same question since RFC-0078 M4b by
first-byte dispatch, and that file's own doc called the arrangement "not
duplicated, inverted": one implementation that keeps the codepoints, one per
engine that throws them away. Both now call `utf8Width`, a new nine-branch
function that is the single statement of what UTF-8 admits. Four statements of
those ranges became one: Rust's `String::from_utf8` in the interpreter, Björn
Höhrmann's DFA in `@__vyrn_utf8valid`, the same DFA again in `std/runtime`'s
`utf8Valid`, and `decodeUtf8`'s own dispatch.

**What each engine lost.** The interpreter's arm scanned for a NUL and then
called `String::from_utf8`; it calls `stringFault` and keeps `from_utf8` only to
BUILD, where it cannot fail and says so. The textual emitter decided the NUL rule
by `@__vyrn_bytes_dup` answering null and the encoding rule by a call to
`@__vyrn_utf8valid`; it calls `stringFault`, switches on the number and copies
the bytes with a new `@__vyrn_bytes_copy`, which is `bytes_dup` without the scan
— `bytes_dup` keeps the scan for `@tallyBytes`, whose rule is RFC-0116's and not
this row's. `std/runtime`'s `strFromBytes` walked the bytes for a NUL and then
walked the DFA; it takes the answer as an argument where the DFA table used to
go, and its body is two comparisons, a `strNew` and a `copy`. The direct wasm
backend was never a carrier — it called `strFromBytes` — and now what it calls
is not one either.

**The census.** `string-nul` and `string-utf8` each fall from three carriers to
one, `vyrn`, and the table says **19 rows and 45 copies**. Six of the nineteen
rows are stated once now. `tests/boundaries.rs` derives that sentence from its
own table, so the count is still a measurement; all 19 rows answer the same
bytes under `vyrn run`, `vyrn run --engine wasm` and a native binary, as they
did before.

**`file-nul` and `file-utf8` did not follow, and the reason is not the rule.**
Their predicate is the same predicate. What differs is the value it would be
asked about: in every engine a file read holds the raw buffer it just slurped —
a `Vec<u8>`, a `char*`, an address in linear memory — and never an
`Array<UInt8>`. Calling `stringFault` would mean materializing one, which is a
copy of every file at every read, and in the interpreter a `Val` per byte. The
wasm route is worse than that: its check is inside `std/runtime`'s
`readFileFrom`, where an `Array<UInt8>` cannot be built at all without the
primitives the fence exists to keep out. So the three DFA copies stay for the
file rows, and they go where finding 3 and PLAN-0125-runtime §5.2 already send
them — M4 step 3 for the native copy, M5 for the interpreter's. `io-status` is
unchanged for finding 7's reason.

**No engine disagreed.** The separation was the place a difference would have
shown, so it was looked for rather than assumed. `tests/text.rs`'s malformed
corpus — every lead byte against ten continuation bytes at three widths, the
surrogate range, the overlong forms, every truncation of a valid sequence, the
five-byte forms — is now judged by Rust's `std::str::from_utf8` instead of by
the other Vyrn function, and every buffer agrees. That change was forced rather
than optional: the old test compared `decodeUtf8` with `stringFromBytes`, and
since both read `utf8Width` an agreement between them would prove nothing.

The other half of the question is whether the check that MOVED agrees with the
DFA that stayed, and the file rows make that answerable: `readFile` still walks
the DFA in all three engines. A throwaway probe wrote each of 2,253 buffers —
the same corpus, minus the NUL cases the file rows refuse for another reason —
with `writeFileBytes`, read it back with `readFile`, and compared the verdict
with `stringFromBytes`'s. Zero mismatches under `vyrn run`, `vyrn run --engine
wasm` and a native binary. Nothing is committed for it because there is nothing
to fix; the corpus that would find a difference is `tests/text.rs`'s and it runs
on every build.

**The cost, in module bytes.** A program that makes a `String` from bytes goes
from 4,576 to 5,355 bytes, and a program that only formats an integer goes from
5,072 to 5,851 — the same 779 bytes, +17.0 and +15.4 per cent. The second
program pays because `std/runtime`'s `intStr` makes its digits into a `String`
with `stringFromBytes`, which is the ONLY route from bytes to a `String` a Vyrn
body has. That is also why `std/text` is `always` in `RT_MODULES` beside the
runtime: the mention scan reads the modules loaded BEFORE the injection loop,
and the runtime enters inside it, so no scan could see that mention. What the
779 bytes buy is two Vyrn functions where there was one runtime loop and one DFA
walk; the DFA and its 364-byte table stay, reached now only by the file readers.
`rfcs/census/wasm-sha256.tsv` is NOT regenerated here, for the second slice's
reason: a hash written in the commit that moved the bytes records nothing.
`VYRN_WASM_MANIFEST=check` therefore fails on 169 of the 172 examples, where the
fourth slice failed on 36: the three that are unchanged neither format an
integer nor make a `String` from bytes, so the sweep leaves them exactly the
module they had.

**What the remaining constructor rows wait on.** `int-narrowing` and
`float-to-int` are where the fourth slice left them: they refuse nothing, so a
constructor has nothing to take, and their decisions are already
`vyrn_frontend::validate`'s. The four `String` and I/O rows this slice did not
take are M4's and M5's, not M6's.

#### The coercion ladder's census (2026-09-04)

§2.7 puts "the coercion ladder in both backends" on the deletion list, and §1
measures it: "The coercion ladder is 505 lines of one decision, and the two
compiled backends order its rungs differently." Nothing had counted the sites.
This is the count, and it is the list the deletions come off.

A **coercion** is where a value crosses into a declared type. A site is in
this census when it DECIDES something about that crossing. The ladder proper —
the rung decision, which is what §1's 505 counts — is marked; the two other
rows are here so a later reader does not look for them under §2.3. The metric
is CODE lines, non-blank and not a comment, over the whole span including the
doc comment, which is §1.1's own column.

This is the count as it stood before anything moved. The table the TEST asserts
is the next section's, because the next section is where the tree ends up;
`the_coercion_census_is_what_the_rfc_records` measures every one of these sites
on every run, so the two tables cannot both be wrong.

| site | rung ladder | engine | what it decides | code |
|---|---|---|---|---|
| `vyrn-codegen/src/lib.rs` `coerce_plan` | yes | shared | which rung a pair takes | 49 |
| `vyrn-codegen/src/lib.rs` `Gen::coerce` | yes | native | the rung, and the IR for it | 104 |
| `vyrn-codegen/src/direct.rs` `Fn_::coerce` | yes | wasm | the rung, and the wasm for it | 177 |
| `vyrn-frontend/src/interp.rs` `Interp::coerce` | yes | interp | the scalar targets that need no walk | 19 |
| `vyrn-frontend/src/interp.rs` `coerce_walk` | yes | interp | the rung, by target type and value shape | 112 |
| `vyrn-frontend/src/interp.rs` `coercion_is_noop` | yes | interp | whether the walk would change the value | 86 |
| `vyrn-frontend/src/interp.rs` `coercion_is_identity` | yes | interp | whether a target type can change any value at all | 35 |
| `vyrn-codegen/src/lib.rs` `coerce_flow` | no | native | whether RFC-0020's containment proof skips the check | 15 |
| `vyrn-frontend/src/checker.rs` `prove_coercion` | no | checker | whether a CONSTANT fails its target's predicate at compile time | 44 |

**How many separate statements of one rule there are: three.** The rung rule —
which of eleven things a crossing does — is stated by the native emitter, by
the wasm emitter and by the interpreter, in three vocabularies over three
pictures of memory. The three carriers are 533 code lines, and §1's 505 is
those same six rows, measured before RFC-0101 §1.5's shadow added its
observation hook. A fourth statement exists and is not a carrier:
`coerce_plan`, 49 lines, which names the eleven rungs and places one for every
pair, and which no engine asks — only the corpus gate does.

**Two rows are about a coercion and are not the ladder.** `coerce_flow` decides
whether RFC-0020's containment proof lets a validation be skipped; both compiled
backends run `vyrn_frontend::finite::string_flow_proven`, so the rule is one and
this is its native call site. `prove_coercion` refuses a CONSTANT that fails its
target's predicate before any engine runs; it is the compile-time half of the
boundary census's `where-scalar` and `where-record` rows, and the fourth slice
already moved the runtime half to `vyrn_frontend::validate`.

**What the plan already proves, and what it does not.** RFC-0101 §1.5's shadow
records every crossing either compiled engine makes and compares it with the
rung the plan places. Over the corpus that is 623,244 crossings: 319,595 take
the planned rung and 303,649 take another by one of four named rules, none of
which is about the value —

| rule | crossings | what it is |
|---|---|---|
| `NumericBeforeShape` | 299,773 | the wasm ladder's numeric rung answers for every integer pair, equal or not |
| `SizedTargetRung` | 3,590 | the native ladder's resize rung is guarded by the TARGET being sized, so `Int16 -> Int16` takes it and emits nothing |
| `FnByShape` | 230 | the wasm ladder has no function-value rung; its shape shortcut answers first |
| `ParamSpelling` | 56 | a type parameter still spelled `T` on one side, because the native ladder does not substitute and the wasm one does |

Every one of the four is a rung that does no work, or a spelling. So the two
compiled ladders already AGREE about every value; what they disagree about
is the order they ask in, and that is what a plan keyed on a pair replaces. The
interpreter is the third carrier and cannot be held to the plan at all, for the
reason §2.4 row 4 states: its `coerce` takes a value and a target and has no
`from` type, so its rung is a fact about the value's shape rather than about a
pair.

#### Where the rung rule lives, and what asks it (2026-09-04)

Both compiled emitters ask `coerce_plan` for the rung now, and neither states
one. The guard chains are gone from both; what is left in each is one arm per
rung, and an arm writes instructions.

**Why the frontend was not the answer, and this time the evidence says so.**
The fourth slice moved `validation_required` out of `vyrn-codegen` and into
`vyrn_frontend::validate` for one reason: a THIRD engine needed to ask it, and
the interpreter lives in the crate `vyrn-codegen` depends on. That reason does
not apply here. The interpreter cannot ask a plan keyed on a pair at all —
§2.4 row 4 states why, and the census's four interpreter rows are what it
looks like: its `coerce` takes a value and a target and has no `from` type, so
it decides by the value's SHAPE. So the plan has two askers and both are in
`vyrn-codegen`. It is also made of `llt_of`, which is an LLVM shape, and a
shape has no business in the frontend. The rule stays where its inputs are.

**The census, after.** This is the table
`the_coercion_census_is_what_the_rfc_records` asserts, printed from the test's
own rows (`cargo test -p vyrn-cli --test lowered -- --ignored --nocapture
the_coercion_census_as_a_table`), so the prose and the tree cannot drift apart
by one edit. The two rows that did not move are in the census above.

| site | states the rung | engine | what it decides | code |
|---|---|---|---|---|
| `vyrn-codegen/src/lib.rs` `coerce_plan` | yes | shared | which rung a pair takes | 49 |
| `vyrn-codegen/src/lib.rs` `Gen::coerce` | no | native | the IR for the rung the plan placed | 111 |
| `vyrn-codegen/src/direct.rs` `Fn_::coerce` | no | wasm | the wasm for the rung the plan placed | 169 |
| `vyrn-frontend/src/interp.rs` `Interp::coerce` | yes | interp | the scalar targets that need no walk | 19 |
| `vyrn-frontend/src/interp.rs` `coerce_walk` | yes | interp | the rung, by target type and value shape | 112 |
| `vyrn-frontend/src/interp.rs` `coercion_is_noop` | yes | interp | whether the walk would change the value | 86 |
| `vyrn-frontend/src/interp.rs` `coercion_is_identity` | yes | interp | whether a target type can change any value at all | 35 |

**Four statements of one rule became two**, and
`the_coercion_census_is_what_the_rfc_records` asserts the number: it was the
native ladder, the wasm ladder, the interpreter's walk and a plan nobody
asked; it is the plan and the interpreter's walk. The interpreter is the
remaining one and it is M5's, not this slice's, for the same reason every
other row of §3 M6 gives — deleting it is deleting the third picture of
memory.

**The line counts, and what they do and do not show.** The rung ladder was 533
code lines and is 532. The native emitter grew by 7 and the direct one shrank
by 8. That is the same measurement the `where-scalar` row already carried and
the same warning: the column that counts CARRIERS moves when a rule moves, and
the column that counts LINES moves when a shape moves. What each emitter lost
is eleven guards; what it gained is eleven arm headers, and the two are nearly
the same number of lines. The rule is stated once anyway, which is the whole
claim, and the corpus is what proves it: **623,244 boundary crossings, every
one of them taking the rung the plan places, at both engines, with no rule to
explain a difference.** It was 319,595 planned and 303,649 explained away by
four named rules the day the census was written.

**The four rules are deleted, and so is the machinery that named them.**
`RungRule`, `rung_rule` and `the_ladder_rules_refuse_what_they_are_not_about`
described a drift that a shared statement cannot have. `tests/lowered.rs`
keeps the shadow itself — every crossing is still recorded and still compared
— and the two assertions now read as one sentence: a crossing takes the
planned rung, or the gate fails and prints the pair. The unit test on the
plan's own order stays, because a green corpus cannot see an order, and the
order is now the whole of the rule.

**Two behaviours changed, and both close a difference §1.5 recorded.**

- **The textual emitter substitutes before it asks.** It did not, which is why
  a `String` flowing into a `T` the monomorphization had already fixed matched
  no guard and fell off the end of its ladder — 56 crossings, and the census
  called them `ParamSpelling`. They are `String -> String` now, and the two
  emitters read one pair.
- **The two ladders end the same way.** The textual one reinterpreted an
  unhandled pair — the bits as they are, under a new name — where the direct
  one refuses, and §1.5 called that "a program that compiles on one target
  only". Both refuse now. Nothing in the corpus reached that end after the
  substitution above, which is why the change is safe to make and worth
  making: the end that cannot be reached is the one to state correctly.

**What could not go, and what each needs.**

- **The interpreter's four functions, 252 lines.** They need a `from` type,
  which means they need the core's producer type at each `Rhs` (the third
  judgment's third slice put it there) and an interpreter that walks the core
  rather than the surface tree. That is M5 deleting the interpreter, not a
  rewritten rule.
- **`Rung::FloatCross` in the textual emitter emits one instruction and reads
  as three.** The plan places every crossing of the int/float line there; this
  emitter has `fptrunc` for `Float -> Float32` and nothing else, because nothing
  else arrives — a Vyrn program crosses that line by calling `Int64(..)` or
  `Float(..)`, which is a builtin and not a boundary, and all 197 of the
  corpus's native float crossings are `Float -> Float32`. What it needs is the
  boundary census's `float-to-int` row: that row refuses nothing today, so
  §2.3's constructor has nothing to take, and until it does there is no answer
  for the emitter to write down.
- **`Rung::Elementwise` in the direct emitter is a shape test and a refusal.**
  The textual emitter unrolls a per-element crossing; this one has no lowering
  for it, so it answers for the pairs whose elements share a shape and refuses
  the rest. The corpus reaches the rung zero times at either engine. What it
  needs is a program that wants it.
- **`coerce_flow` and `prove_coercion` are not the ladder** and did not move.
  Both already state their rule once — `finite::string_flow_proven` and
  `consteval::eval` — and both are call sites of it.

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
   verdict on *inferred* regions stands either way. *Answered by M4 step 8:
   the arena is real and the recommendation is to keep the syntax. A library
   type cannot route the allocations a program never names — `a + b`, `@str`,
   `.copy()` — without an allocator parameter on every one of them.*
3. **Reference counting.** The linear judgment is the same for unique
   ownership and for precise counting. This RFC keeps unique ownership, because
   the language chose explicit copies. Counting stays available as a runtime
   change under the same kernel if `.copy()` cost becomes the complaint users
   actually have.
