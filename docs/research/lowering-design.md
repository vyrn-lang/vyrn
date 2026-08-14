# Lowering design for RFC-0101

Two questions about `rfcs/RFC-0101-a-backend-is-an-emitter.md`. **How should the
lowered form be shaped**, judged against compilers that already made the choice.
And **what does it cost an LLM agent to read**, because this repository is built
by agents and reading is the budget.

- **Repository HEAD:** `1eff3d3` (RFC-0101 itself). Every repository claim below
  was measured in this worktree at that commit.
- **Method:** read the code for the repository claims; read primary sources for
  the others. Every outside claim carries a URL. Claims I could not confirm are
  marked **UNVERIFIED**.
- **Status:** research. It decides nothing. §4 lists the amendments it proposes
  to RFC-0101, and RFC-0101 is where a decision belongs.

---

## The three verdicts, first

**1. The form RFC-0101 describes already exists, and it is Zig's AIR.**
Fully typed, one instance per *instantiated* function, produced after comptime
and generics are resolved, structured control flow as instructions, consumed by
six backends including a C backend
([`src/Air.zig`](https://github.com/ziglang/zig/blob/master/src/Air.zig),
[`src/codegen.zig`](https://github.com/ziglang/zig/blob/master/src/codegen.zig)).
That is RFC-0101 §2.1 and §2.2, point for point, shipped. Vyrn should copy the
shape and refuse one thing: AIR's textual dump is debug-only, it is not a test
artifact, and it has broken repeatedly
([#7670](https://github.com/ziglang/zig/issues/7670),
[#10031](https://github.com/ziglang/zig/issues/10031),
[#12599](https://github.com/ziglang/zig/issues/12599)). The dump must be gated
or it will rot, and this repository already paid that bill once: commit `b1eef04`
deleted the Inkwell backend — "890 lines that went from working to unbuildable in
twelve days, unnoticed, because nothing checked it."

**2. Explicit release steps are the confirmed part of the design, and two
compilers reached it independently.** Swift puts `destroy_value` in the IR and
verifies it continuously until ownership is lowered
([Ownership.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/Ownership.md));
Swift decides ARC placement in SIL, not in IRGen
([OptimizerDesign.md](https://github.com/swiftlang/swift/blob/main/docs/OptimizerDesign.md)).
rustc's `ElaborateDrops` turns conditional drops into unconditional ones before
any backend sees the body, and the phase distinction states the point exactly: an
unelaborated drop is a question, an elaborated drop is an instruction
([drop-elaboration.md](https://raw.githubusercontent.com/rust-lang/rustc-dev-guide/master/src/mir/drop-elaboration.md)).
Vyrn's `droppable` map is a question, asked once in the interpreter and five
times in each backend. It writes 1,800 lines of placement over 1,421 lines of
shared analysis, and the three most recent cross-backend defects all lived in the
placement. RFC-0101 M4 has prior art behind it and a bug ledger in front of it.

**3. "Parity becomes structural" is the one claim to weaken.** RFC-0101 §2.4 says
that when the interpreter walks the lowered form, "parity stops testing whether
three copies agree". Miri is the counter-example inside the strongest supporting
precedent. Miri runs the same MIR the backends compile — `Machine::load_mir` and
`codegen_mir` both call `TyCtxt::instance_mir`, which asserts the Runtime phase —
**and** it disables five passes over that MIR, because it wants its own
diagnostics (`MIRI_DEFAULT_ARGS` in `miri/src/lib.rs`). Vyrn's interpreter
already does the same thing: it runs no releases on the `?` path
(`compiler/vyrn-frontend/src/interp.rs:2760`) because the host reclaims. **A
shared form does not make three engines identical. It makes their differences
declared instead of accidental.** That is a smaller promise, it is true, and it
still justifies the arc.

---

## 0. What Vyrn constrains

Verified here, not taken from the brief.

**Three consumers, and the two compiled ones are the same size as each other.**
`compiler/vyrn-frontend/src/interp.rs` is 9,348 lines,
`compiler/vyrn-codegen/src/lib.rs` is 16,295 and
`compiler/vyrn-codegen/src/direct.rs` is 16,180. The checker is 13,526
(`compiler/vyrn-frontend/src/checker.rs`).

**The parity invariant is byte-identical output, traps included, over 161
examples.** `compiler/vyrn-cli/tests/parity.rs:1` states it; the corpus is 161
`.vyrn` files in `examples/`, median 67 lines, 15,463 lines in total.

**The invariant is load-bearing beyond its own test.** RFC-0076 swaps the engine
that runs a generator — interpreter or compiled wasm — and
`compiler/vyrn-genwasm/src/lib.rs:12` names parity as the reason that swap is
safe: "the sacred invariant is that interp == native == wasm". A design that
weakens parity breaks a thing that is not the parity test.

**Ownership is defined, shared, and thrown away.**
`vyrn_frontend::own::analyze` (`compiler/vyrn-frontend/src/own.rs:945`) returns
`Ownership`, whose `droppable: HashMap<String, HashMap<usize, DropKind>>`
(`own.rs:890`) answers one question per node address. `DropKind`
(`own.rs:53`) has seven variants. The answer is a fact lookup; every engine then
places the releases itself.

**Monomorphization is total and there are no function pointers.** RFC-0037
defunctionalized stored function values; `compiler/vyrn-codegen/src/direct.rs:864`
records the consequence — "Every call goes through the signature's dispatcher".
A lowered form therefore never has to represent an indirect call target, which
is one of the two reasons GHC needs STG below Core (§1.3).

**The comptime sandbox is a fourth consumer.** A `gen fn` runs through
`crate::interp::generate` (`compiler/vyrn-frontend/src/loader.rs:1629`) or,
since RFC-0076, through compiled wasm. Both routes are Vyrn programs. Both would
consume the lowered form.

**The AST carries lines, not columns.** `line: usize` appears 40 times in
`compiler/vyrn-frontend/src/ast.rs`; `col: usize` appears twice. `Diagnostic`
carries `col` and `end_col` (`compiler/vyrn-frontend/src/diagnostics.rs:52-56`),
but the tree the lowering would read does not. This contradicts RFC-0101 §2.1
item 6 and §2.5 together, and §4 amendment 6 says what to do about it.

**The AST is small.** `Expr` has 20 variants and `Stmt` has 14
(`compiler/vyrn-frontend/src/ast.rs:1207`, `:1067`). A lowered form over 34
constructs is a thing one agent can hold in one context. This is the single
most encouraging measurement in this document.

**The checker already writes six answers down, for six consumers, none of them a
backend.** `check_accum_full` (`compiler/vyrn-frontend/src/checker.rs:313`)
returns five values; `symbols::Analysis`
(`compiler/vyrn-frontend/src/symbols.rs:117`) has twelve fields, all for the LSP.
RFC-0101 §1.2 counts four side channels. The measured count is higher, and every
one was added for a consumer that asked. The backends never asked.

---

## 1. Prior art, one verdict each

### 1.1 Rust: one MIR, three backends, two interpreters — and it is not monomorphized

This is the precedent RFC-0101 leans on hardest, so it gets the most checking.
Two of its claims hold. One does not.

**The pipeline.** HIR is "a compiler-friendly representation of the abstract
syntax tree" with sugar removed and **no types on its nodes**
([hir.html](https://rustc-dev-guide.rust-lang.org/hir.html)). THIR is "a lowered
version of the HIR where all the types have been filled in", where "method calls
and overloaded operators are converted into plain function calls" and
"Destruction scopes are also made explicit"
([thir.html](https://rustc-dev-guide.rust-lang.org/thir.html)). MIR is a
control-flow graph: "It does not have nested expressions" and "All types in MIR
are fully explicit"
([mir/index.md](https://raw.githubusercontent.com/rust-lang/rustc-dev-guide/master/src/mir/index.md)).

**THIR exists to keep MIR smaller, and that worked.** THIR's three consumers are
MIR construction, exhaustiveness checking and unsafety checking
([thir.html](https://rustc-dev-guide.rust-lang.org/thir.html)) — the checks that
need types *and* source structure, which the CFG destroys. Once THIR unsafeck
stabilized in 1.77, MIR unsafeck was deleted, and the PR says what that bought:
"This PR also removes safety information from MIR"
([#123322](https://github.com/rust-lang/rust/pull/123322)). Put the check where
the structure is, and the IR loses a field.

**MIR is not SSA, on purpose.** RFC 1211 rejected SSA because temporaries must
be borrowable — alloca-like memory locations are what the borrow checker's
path-based reasoning needs ([RFC 1211](https://rust-lang.github.io/rfcs/1211-mir.html)).

**Drops are explicit, and the phase decides what they mean.** `MirPhase` is
`Built`, `Analysis`, `Runtime`
([rustdoc](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_middle/mir/enum.MirPhase.html)).
In analysis MIR, "`Drop` terminators represent _conditional_ drops". In runtime
MIR "the drops are unconditional… if the type has drop glue that drop glue is
always executed". `ElaborateDrops` is the pass that converts one into the other,
and it runs in `run_runtime_lowering_passes`
(`compiler/rustc_mir_transform/src/lib.rs`), after borrow checking. Elaboration
classifies each drop as static, dead, conditional or open, using a
maybe-initialized dataflow pair, deletes the dead ones, and gives flags only to
the conditional ones
([drop-elaboration.md](https://raw.githubusercontent.com/rust-lang/rustc-dev-guide/master/src/mir/drop-elaboration.md),
[RFC 320](https://rust-lang.github.io/rfcs/0320-nonzeroing-dynamic-drop.html)).

**Miri runs the same MIR the backends compile, and the proof is one line.**
`Machine::load_mir` in `compiler/rustc_const_eval/src/interpret/machine.rs` calls
`ecx.tcx.instance_mir(instance)`. `rustc_codegen_ssa::mir::codegen_mir` calls the
same function. `TyCtxt::instance_mir` (`compiler/rustc_middle/src/ty/mod.rs`)
ends with `assert!(matches!(body.phase, MirPhase::Runtime(_)))`. The interpreter
is shared with const evaluation: it is "shared between the compiler (for
compile-time function evaluation, CTFE) and the tool Miri, which uses the same
virtual machine"
([interpret.html](https://rustc-dev-guide.rust-lang.org/const-eval/interpret.html)).
So the interpreter gets drop elaboration for free, and drop glue arrives as an
ordinary callable MIR body from `mir_shims`.

**But the shared body is generic.** `instance_mir` is keyed on `DefId`, not on
generic arguments. Codegen substitutes at the use site with
`instance.instantiate_mir_and_normalize_erasing_regions`; the interpreter
substitutes with
`instantiate_from_current_frame_and_normalize_erasing_regions`. Codegen collects
its instances eagerly ([monomorph.html](https://rustc-dev-guide.rust-lang.org/backend/monomorph.html));
Miri instantiates lazily as it interprets. **One body, two instantiation
strategies.** This directly contradicts RFC-0101 §2.1 item 1, which says
`Lowered` holds "Concrete function bodies, one per instantiation".

**The shared boundary is the IR, not a builder trait.** `rustc_codegen_ssa`
"provides an abstract interface for all backends to implement, namely LLVM,
Cranelift, and GCC", and its `BuilderMethods` trait is the per-instruction seam
([backend-agnostic.html](https://rustc-dev-guide.rust-lang.org/backend/backend-agnostic.html)).
A code search for `BuilderMethods` under `compiler/rustc_codegen_cranelift`
returns **zero hits**; the same search under `rustc_codegen_llvm` and
`rustc_codegen_gcc` returns `builder.rs`, `abi.rs`, `intrinsic.rs` and more.
cg_clif reuses `CodegenBackend`, `ExtraBackendMethods`, `WriteBackendMethods`
and the CGU/linking driver, and then walks MIR itself in `src/base.rs`, with its
own `value_and_place.rs`, `discriminant.rs`, `vtable.rs`, `abi/` and
`intrinsics/`. **Two of three backends use the instruction abstraction. Three of
three use the IR.**

**Miri does not share the whole lowering, and it says so in a flag list.**
`MIRI_DEFAULT_ARGS` (`miri/src/lib.rs`) sets `-Zmir-opt-level=0`,
`-Zmir-preserve-ub`, `-Zalways-encode-mir`, and disables three lowering passes
with the comment "Disable passes that add checks for language UB -- we get
better diagnostics if we let Miri do these checks"
(`-CheckAlignment,-CheckNull,-CheckEnums`), plus `-ReferencePropagation` and
`-GVN` because they disagree with Miri's aliasing model. The form is shared; the
pass selection over it is per consumer.

**Does the shared MIR catch engine divergence?** Mostly it catches undefined
behaviour, which is a different job: Miri is "an Undefined Behavior detection
tool for Rust" ([README](https://github.com/rust-lang/miri)), and the POPL 2026
paper reports bugs found in real crates, not in backends
([paper page](https://popl26.sigplan.org/details/POPL-2026-popl-research-papers/50/Miri-Practical-Undefined-Behavior-Detection-for-Rust);
body text **UNVERIFIED**, the PDF would not extract). It *has* caught
optimizer-versus-interpreter disagreement — `-ReferencePropagation` and `-GVN`
are disabled for exactly that, and a Miri test carries "this miscompiles with
optimizations" against
[rust#132898](https://github.com/rust-lang/rust/issues/132898). But it caught it
by letting the interpreter opt out, not by forcing agreement.

**The MIR dump is greppable, snapshot-tested, and explicitly unstable.**
`compiler/rustc_middle/src/mir/pretty.rs` writes into every dump: "WARNING: This
output format is intended for human consumers only and is subject to change
without notice. Knock yourself out." And yet `tests/mir-opt` blesses `.mir`
files: "The `mir-opt` test format emits MIR to extra files that you can
automatically update by specifying `--bless`"
([tests/mir-opt/README.md](https://github.com/rust-lang/rust/blob/master/tests/mir-opt/README.md)).
Normalization is done with flags, not promises —
`-Zdump-mir-exclude-pass-number` exists so an inserted pass does not renumber
every file ([compiletest.html](https://rustc-dev-guide.rust-lang.org/tests/compiletest.html)).
The workflow guide tells authors to bless and commit the dump *before*
implementing an optimization, "so that you (and your reviewers) can see a
before/after diff of what the optimization changed"
([optimizations.html](https://rustc-dev-guide.rust-lang.org/mir/optimizations.html)).

**Cost.** MIR-based translation shipped in Rust 1.12 after "many months of
effort" ([1.12 announcement](https://blog.rust-lang.org/2016/09/29/Rust-1.12/),
[#34096](https://github.com/rust-lang/rust/pull/34096)). No compile-time or
line-count figure for MIR itself is published (**UNVERIFIED**). The one number
that exists is for the codegen refactor: ~12,000 shared lines, of which
"approximately 10,000 LOC that would otherwise have had to be duplicated"
([backend-agnostic.html](https://rustc-dev-guide.rust-lang.org/backend/backend-agnostic.html)).

**Verdict — three things to copy, one to refuse, one to reword.**

*Copy: explicit, elaborated drops in the lowered form.* rustc and Swift agree,
independently, and rustc's phase distinction says why it matters: an
unelaborated drop is a *question*, an elaborated drop is an *instruction*. Vyrn's
`droppable` map is a question, asked five times per backend. RFC-0101 §2.1 item 3
is correct and has two precedents.

*Copy: the IR is the multi-backend contract; a shared instruction builder is
optional.* Cranelift, the newest and fastest-moving rustc backend, skipped the
builder abstraction entirely and consumes MIR directly. RFC-0101 §2.3 lists what
stays in a backend and proposes no shared emitter interface. That restraint is
correct and should be stated as a rule, not left as an omission.

*Copy: unstable text, blessed snapshots.* RFC-0101's open question 5 worries that
"a rendering people read becomes a rendering people depend on". rustc answered
that in a comment: print the disclaimer, bless the snapshots, and change the
format whenever you like — a format change becomes one wide, reviewable diff.

*Refuse: rustc's non-monomorphized body — but only just, and with a named
threshold.* rustc keeps generic MIR because it must: Rust has trait objects,
separate compilation and a lazily-instantiating interpreter. Vyrn has none of
those. It monomorphizes everything already, has no function pointers, and its
worst monomorphization defect (#165) was a *mangled string used as an identity*.
Concrete bodies delete substitution from all three engines, which is the whole
thesis: a backend that substitutes is a backend that decides. But rustc's shape
is the answer to RFC-0101's own open question 1 if the copies prove expensive —
one generic body plus one instance list, and consumers substitute through a
single shared helper. See §4 amendment 3 for the threshold.

*Reword: "parity becomes structural".* RFC-0101 §2.4 says that if the
interpreter walks the same form, "parity stops testing whether three copies
agree". Miri is the counter-example in the same sentence's shape: it runs the
same MIR *and* disables five passes over it, because it wants different
diagnostics. Vyrn's interpreter already does the same thing — it runs no
releases on the `?` path (`compiler/vyrn-frontend/src/interp.rs:2760`) because
the host reclaims, which RFC-0101 §1.4 records as intentional. **A shared form
does not make three engines identical. It makes their differences declared
instead of accidental.** That is a smaller claim and a true one, and it is still
worth six milestones.

### 1.2 Swift SIL: the raw form is where the checker still speaks

**What it carries.** SIL has three stages, not two. `SILStage` in
[`SILModule.h`](https://github.com/swiftlang/swift/blob/main/include/swift/SIL/SILModule.h)
is `{ Raw, Canonical, Lowered }`. Raw SIL is what SILGen produces before the
mandatory passes; it "may not have a fully-constructed SSA graph" and "may
contain dataflow errors, like not all variables are initialized"
([SIL.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/SIL.md)).
Canonical SIL is the same program after mandatory optimization and diagnosis.
Lowered SIL is prepared for IRGen and no longer sees canonical passes.

**Ownership is in the IR, and verified.** Ownership SSA is "an augmented version
of SSA that enforces ownership invariants"; `copy_value` produces an owned value,
`destroy_value` consumes one, `begin_borrow`/`end_borrow` bracket a scope, and
"the SILVerifier validates the aforementioned relationship on all SIL values,
uses at all points of the pipeline until OSSA is lowered"
([Ownership.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/Ownership.md)).
The pass that erases it is `OwnershipModelEliminator`
([source](https://github.com/swiftlang/swift/blob/main/lib/SILOptimizer/Mandatory/OwnershipModelEliminator.cpp)),
and that pass has been moved progressively later as more passes learned to keep
ownership. Ownership stays explicit exactly as long as something checks it.

**What stayed below.** ARC placement is decided in SIL: "ARC optimization is
implemented at SIL-level"
([OptimizerDesign.md](https://github.com/swiftlang/swift/blob/main/docs/OptimizerDesign.md)).
LLVM still runs a second, weaker ARC pass in `lib/LLVMPasses`, which "must be
more conservative"
([ARCOptimization.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/ARCOptimization.md)).
So the lower layer refines the decision; it does not make it again.

**The text is the test artifact.** "Textual SIL files have the file extension
`.sil`" and the compiler parses them back
([SIL.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/SIL.md)).
[`test/SILOptimizer/sil_combine.sil`](https://github.com/swiftlang/swift/blob/main/test/SILOptimizer/sil_combine.sil)
is SIL in and SIL out through `FileCheck`.

**Cost.** [`SILNodes.def`](https://github.com/swiftlang/swift/blob/main/include/swift/SIL/SILNodes.def)
defines roughly 180 to 190 instruction kinds (approximate count, **UNVERIFIED**
as an exact figure). SIL is a second compiler. No published engineering-cost
figure exists (**UNVERIFIED**).

**Verdict — copy the ownership-in-the-IR rule; refuse the stage ladder.** Vyrn's
`own::analyze` is Swift's ownership analysis with the answer left outside the
program. Putting `Release(place, kind)` steps into the body is the same move
Swift made, and Swift's own history says the right direction is to keep them
explicit *longer*, not shorter. Refuse Raw/Canonical/Lowered: Vyrn diagnoses
before lowering, in the checker and in `movecheck`, so a form that legally holds
a wrong program has nobody to serve. One stage. Refuse the instruction count
too — 180 instructions is what a language with classes, existentials, generics
at runtime and exceptions needs. Vyrn has 34 AST constructs and no runtime
generics.

**Open question this raises:** Swift verifies ownership continuously. RFC-0101
proposes no verifier. §4 amendment 3 proposes one, and §1.3 explains why it is
nearly free.

### 1.3 GHC Core: small on purpose, and the reason it can stay small

**The thesis, and the measured size.** `Expr` in
[`compiler/GHC/Core.hs`](https://github.com/ghc/ghc/blob/master/compiler/GHC/Core.hs)
has **10 constructors** — `Var`, `Lit`, `App`, `Lam`, `Let`, `Case`, `Cast`,
`Tick`, `Type`, `Coercion` — plus two for `Bind` and one for `Alt`. Thirteen in
total. Core is System FC.

**Why small pays.** The GHC chapter of *The Architecture of Open Source
Applications* states the payoff without decoration: "When new language features
are added to the source language (and that happens all the time) the changes are
usually restricted to the front end; `Core` stays unchanged, and hence so does
most of the compiler" ([aosabook](https://aosabook.org/en/v2/ghc.html)).

**A typed IR buys a linter.** `-dcore-lint` turns on "heavyweight intra-pass
sanity-checking within GHC, at Core level"
([users guide](https://downloads.haskell.org/ghc/latest/docs/users_guide/debugging.html)),
and it works because Core is explicitly typed: Lint is "a 100% independent check
on the type inference engine" ([aosabook](https://aosabook.org/en/v2/ghc.html)).
An untyped IR cannot buy this.

**One correction to the small-core story.** "Every pass round-trips through
Core" is false. GHC also optimizes STG (`-fstg-lift-lams`, `-fstg-cse`) and Cmm
(`-fcmm-sink`, `-fcmm-elim-common-blocks`, and others)
([users guide](https://ghc.gitlab.haskell.org/ghc/doc/users_guide/using-optimisation.html)).
Core is closed under the *main* optimizer loop; each lower IR gets the passes
that need what it added.

**Why three IRs and not one.** STG is "essentially just `Core` annotated with
more information required by the code generator" and Cmm is "a low-level
imperative language with an explicit stack"
([aosabook](https://aosabook.org/en/v2/ghc.html)). Concretely, STG adds closure
representation — `LambdaFormInfo`, arity, free variables, argument descriptors
([GHC.StgToCmm.Closure](https://hackage.haskell.org/package/ghc-lib-0.20210601/docs/GHC-StgToCmm-Closure.html)).

**Verdict — one IR, and Vyrn is entitled to one.** GHC needs STG because a lazy
functional language must represent closures at run time. Vyrn defunctionalized
its closures (RFC-0037) and has no function pointers
(`compiler/vyrn-codegen/src/direct.rs:864`), so the thing STG exists to carry
does not exist here. GHC needs Cmm because it has several machine backends
sharing a calling convention; Vyrn's two compiled backends share nothing below
the decision layer and RFC-0101 §2.3 is right that they should not. **Take
`-dcore-lint`.** A form with a type on every node admits an independent
type-checker over itself, and it is the cheapest permanent guard RFC-0101 could
buy: M1's corpus gate, generalized from "run once during migration" to "run in
debug builds forever".

### 1.4 QBE: minimalism, priced

**The claim.** QBE "aims to provide 70% of the performance of industrial
optimizing compilers in 10% of the code", and the size limit "prevents embarking
on a never-ending path of diminishing returns"
([c9x.me/compile](https://c9x.me/compile/)). QBE describes itself as "less than
8 kloc" ([llvm.html](https://c9x.me/compile/doc/llvm.html)) — the project's own
figure, **UNVERIFIED** against a current tree.

**The IL.** Text first: "The intermediate language is provided to QBE as text"
([il.html](https://c9x.me/compile/doc/il.html)). Four base types — `w`, `l`,
`s`, `d`. Phi nodes, not block parameters, and **the producer need not build
SSA**: "phi instructions are NOT necessary when writing a frontend to QBE", and
QBE "is able to fixup programs not in SSA form". Types are semantic only: "QBE
is not using types as a means to safety".

**What minimalism costs and buys.** No vector types, no unwinding, no inlining,
three targets. What it *buys* the frontend is the C ABI: QBE argues that an
LLVM-based frontend "needs to reimplement large chunks of the ABI"
([llvm.html](https://c9x.me/compile/doc/llvm.html)).

**Verdict — QBE is not the model, it is the price list.** QBE sits *below* a
language; RFC-0101's `Lowered` sits *above* one. Their type systems point in
opposite directions on purpose, and copying QBE's "types are not for safety"
would delete the very thing RFC-0101 exists to share. What QBE is good for here
is a number: **8 kloc buys a whole optimizing backend for three architectures,
including register allocation and the C ABI.** RFC-0101 §1.7 estimates Vyrn's
irreducible per-target residue at about a third of 16,000 lines, roughly 5,400.
QBE says that estimate is the right order of magnitude. RFC-0101 §5's refusal to
promise a free fourth backend is honest, and QBE is the citation for it.

### 1.5 Zig: AIR is the design RFC-0101 is describing

**The pipeline.** Source → AstGen → ZIR → Sema → AIR → codegen. ZIR is untyped,
one instance per source file, immutable and cacheable
([`Zir.zig`](https://github.com/ziglang/zig/blob/master/lib/std/zig/Zir.zig)).
AIR's own header states the shape: "Analyzed Intermediate Representation. This
data is produced by Sema and consumed by codegen. Unlike ZIR where there is one
instance for an entire source file, each function gets its own `Air` instance"
([`Air.zig`](https://github.com/ziglang/zig/blob/master/src/Air.zig)).

**AIR is monomorphic.** No single sentence says so, so here is the chain. Sema
"transforms untyped ZIR instructions into semantically-analyzed AIR
instructions", performing "type checking, comptime control flow, and safety-check
generation" ([`Sema.zig`](https://github.com/ziglang/zig/blob/master/src/Sema.zig)).
`InternPool` gives each generic instantiation its own function entity, with
`generic_owner` naming the generic it came from and `comptime_args` holding the
values it was instantiated with; the instance's type "will potentially have fewer
parameters than the generic owner's type, because the comptime parameters will be
deleted" ([`InternPool.zig`](https://github.com/ziglang/zig/blob/master/src/InternPool.zig)).
Backends never see a generic. **UNVERIFIED as a quotation; verified as a
structure.**

**AIR is structured, not a CFG.** `block` "runs its body which always ends with a
`noreturn` instruction"; `loop` is "a labeled block of code that loops forever";
`br`, `cond_br`, `switch_br` all have result type `noreturn`. Bodies are flat
lists of instruction indices ([`Air.zig`](https://github.com/ziglang/zig/blob/master/src/Air.zig)).

**Six backends, one AIR.** `codegen.zig` dispatches AIR plus liveness to
`.stage2_aarch64`, `.stage2_riscv64`, `.stage2_sparc64`, `.stage2_x86_64`,
`.stage2_wasm` and `.stage2_c`
([`codegen.zig`](https://github.com/ziglang/zig/blob/master/src/codegen.zig)).
The LLVM backend consumes AIR too, outside that switch. A **C backend** consuming
the same AIR is the closest existing analogue to Vyrn's textual-LLVM emitter.

**The dump is the failure.** `--verbose-air` is debug-only and has been
incomplete or crashing repeatedly:
[#7670](https://github.com/ziglang/zig/issues/7670) lists unfinished dumping for
asm, block, call, condbr, constant, loop and switchbr;
[#10031](https://github.com/ziglang/zig/issues/10031) crashed on an empty file;
[#12599](https://github.com/ziglang/zig/issues/12599) crashed on the behavior
tests. Zig does not use AIR dumps as test fixtures (**UNVERIFIED**; I found no
evidence that it does, and its compiler tests are per-backend behavior tests).

**Verdict — copy AIR, and gate the dump Zig did not gate.** Every structural
choice RFC-0101 argues for in §2.1 and §2.2 is AIR: typed, per-instantiation,
post-comptime, structured control flow, many backends including a text-emitting
one. That is a working existence proof from a project of comparable ambition, and
it is the strongest single answer to "is this shape right". The one part Zig got
wrong is the part §2 of this document is about: an ungated dump decays, and a
decayed dump is worse than none, because an agent trusts it. Vyrn already has the
matching lesson written down at `b1eef04`, where a second native backend was
deleted for going unbuildable in twelve days with nothing checking it.

### 1.6 Go: the shared/per-arch line is drawn at one pass, and the tests are not IR tests

**Where the line is.** Go's generic SSA phase runs "a series of machine-independent
passes and rules" on every `GOARCH`; the `lower` pass "is special; it converts the
SSA representation from being machine-independent to being machine-dependent"
([ssa/README.md](https://github.com/golang/go/blob/master/src/cmd/compile/internal/ssa/README.md)).
Rules live in `_gen/generic.rules` (shared) and `_gen/ARCH.rules` (per-arch), and
the generated rewriters are checked in.

**The dump.** `GOSSAFUNC=Foo go build` writes an `ssa.html` showing the IR after
every pass ([cmd/compile/README.md](https://github.com/golang/go/blob/master/src/cmd/compile/README.md)).
It is a debugging tool.

**The tests are assembly regexes.** `test/codegen/` "compiles Go code ... and
matches the generated assembly ... against a set of regexps", written as per-arch
comments such as `// amd64:"SQRTSD"`
([test/codegen/README](https://github.com/golang/go/blob/master/test/codegen/README)).
Machine-independent passes get hand-built-IR unit tests with an `Equiv` comparison
([func_test.go](https://github.com/golang/go/blob/master/src/cmd/compile/internal/ssa/func_test.go)).
There are no IR snapshot tests.

**Verdict — Vyrn already has Go's tier, for one backend only, and that asymmetry
is a bug.** `compiler/vyrn-cli/tests/places.rs` is `test/codegen/` exactly: it
proves a property no program output can show — that a container mutation moves
the header instead of copying it — by reading `vyrn emit-ir` text and counting
allocating calls (`places.rs:16-60`). Ten test files use `emit-ir` this way.
**The direct wasm backend has no text form at all**: `wasmprinter`, `wat` and
`wasm2wat` appear nowhere in `compiler/`. So half the compiled surface cannot be
tested the way the other half is. That is a defect available to fix today, before
any of RFC-0101 — see §4 amendment 8.

### 1.7 MLIR: take two lines of it

**What it costs.** MLIR ships 48 upstream dialects
([Dialects](https://mlir.llvm.org/docs/Dialects/)). A minimal out-of-tree dialect
means building your own `opt`-like tool as part of hello world
([examples/standalone](https://github.com/llvm/llvm-project/tree/main/mlir/examples/standalone)),
and each operation is a TableGen record with summary, arguments dag, results dag,
traits, verifier, assembly format and folders
([Defining Dialects: Operations](https://mlir.llvm.org/docs/DefiningDialects/Operations/)).
Line count is **UNVERIFIED** — no citable figure exists.

**What people report it costs.** A Cornell CS6120 project building a small
dialect: "The project was challenging right from the beginning, starting with
project setup", naming CMake and TableGen as the learning curve
([writeup](https://www.cs.cornell.edu/courses/cs6120/2025fa/blog/brilir/)).
MLIR's own creator writes that MLIR "now faces ... an identity crisis" and that
dialect proliferation "led to fragmentation in downstream systems"
([Modular](https://www.modular.com/blog/democratizing-ai-compute-part-8-what-about-the-mlir-compiler-infrastructure)).

**What it genuinely buys.** From
[LangRef](https://mlir.llvm.org/docs/LangRef/): "MLIR has a simple and unambiguous
grammar, allowing it to reliably round-trip through a textual form. This is
important for development of the compiler". And the generic form parses even for
unregistered dialects, so "types to be round-tripped without needing to link in
the dialect library that defined them"
([Rationale](https://mlir.llvm.org/docs/Rationale/Rationale/)). The convention
that follows is two lines at the top of a test file
([`ops.mlir`](https://github.com/llvm/llvm-project/blob/main/mlir/test/Dialect/Arith/ops.mlir)):

    // RUN: mlir-opt %s | mlir-opt | FileCheck %s
    // RUN: mlir-opt %s --mlir-print-op-generic | mlir-opt | FileCheck %s

**Verdict — refuse the framework, and refuse the round-trip as well.** For a
project with one language, one team and one lowering pass, the dialect machinery
has nobody to amortize across, and MLIR's own history says what happens when the
generality outruns the coordination. The round-trip needs a parser for the
lowered form. A parser is a second front end, written to test a printer, and it
would be the largest single piece of new code RFC-0101 could accidentally
acquire. **Keep the property, drop the mechanism**: print deterministically,
check the print into the repository, diff it. That is a snapshot test. It catches
every change the round-trip would catch except "the printer and the parser
disagree", and there is no parser to disagree with.

### 1.8 Cranelift: what it wants, and what it will not do for you

**SSA: the library builds it.** Cranelift IR itself demands strict SSA and uses
block parameters, not phi nodes
([ir.md](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/ir.md)).
But `cranelift-frontend`'s `FunctionBuilder` constructs SSA for you: "through
calling the functions declare_var, def_var and use_var, the FunctionBuilder will
create for you all the Cranelift IR values"
([docs.rs](https://docs.rs/cranelift-frontend/latest/cranelift_frontend/)),
implementing Braun et al. over an incomplete CFG
([ssa.rs](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/frontend/src/ssa.rs)).

**Control flow: the library does not build it.** Cranelift is a flat CFG;
"execution can never fall through to the next BB without an explicit branch"
([ir.md](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/ir.md)).
There is no structured `if`/`loop` in the builder API. Wasmtime's own wasm
frontend writes the flattening itself, with a `ControlStackFrame` holding
`block`/`if`/`else`/`loop` frames
([stack.rs](https://github.com/bytecodealliance/wasmtime/blob/main/crates/cranelift/src/translate/stack.rs),
path located by search, contents **PARTIALLY UNVERIFIED**). Chris Fallin names
the translation as the central problem, not a detail
([cfallin.org](https://cfallin.org/blog/2021/01/22/cranelift-isel-2/)).

**CLIF text is a first-class artifact.** Filetests are `.clif` files with a
`test <name>` header; `test compile` runs the whole pipeline, `test run` executes
and checks results with `; run:`, and `test cat` parses a function and prints it
back
([testing.md](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/testing.md)).

**Speed.** The 2020 measurement was a full debug build at 49.93 s with Cranelift
against 54.64 s with LLVM, and incremental *slower* at 7.98 s against 5.48 s
([Inside Rust](https://blog.rust-lang.org/inside-rust/2020/11/15/Using-rustc_codegen_cranelift/)).

**Verdict — Cranelift confirms RFC-0101 §2.2's direction and corrects its
price.** Structured control flow is the right thing to keep, because wasm accepts
nothing else and a CFG in the middle would force a relooper. But RFC-0101 says
Cranelift "flattens on the way in" as if the flattening were free. It is not
free, and it is not Cranelift's: the *producer* writes it, and Wasmtime's version
is a whole control stack. The correction is small and worth making, because
RFC-0101 §4 uses "a new backend is an emitter" as its payoff, and a Cranelift
emitter starts with a control-flow flattener before it emits one instruction.
Also note the speed evidence cuts both ways: RFC-0101 cites clang at 1,974 ms
against cranelift at 250 ms, and the Rust measurement shows Cranelift losing on
incremental builds. Neither number transfers. Keep RFC-0101 §5's "no
compile-speed claim".

### 1.9 What the survey settles

| System | Typed IR | Ownership in the IR | Monomorphic before backends | Structured CF | IR text is a test artifact |
|---|---|---|---|---|---|
| rustc MIR | yes | drops elaborated | **no** — generic body, consumer instantiates | no (CFG) | yes (`mir-opt`, `--bless`) |
| Swift SIL | yes | yes (OSSA), verified | n/a (runtime generics) | no (CFG) | yes (`.sil` filetests) |
| GHC Core | yes | no (GC) | no (polymorphic) | no (lambda/case) | no |
| QBE | minimal | no | n/a | no (CFG) | yes (test suite is IL) |
| Zig AIR | yes | no (manual) | **yes** | **yes** | no (debug dump only) |
| Go SSA | yes | no (GC) | no (until generics shape stenciling) | no (CFG) | no (assembly regex) |
| Cranelift CLIF | yes | no | n/a | no (CFG) | yes (filetests) |
| MLIR | per dialect | per dialect | per dialect | regions, so yes | yes (round-trip) |

Read the two columns RFC-0101 depends on. **Monomorphic before the backends: Zig
alone, and Zig is the only entry in the table without runtime generics or a
garbage collector, which is Vyrn's position exactly.** **Structured control
flow: Zig alone, and Zig is the only entry with a first-class wasm backend of its
own, which is again Vyrn's position.** RFC-0101 §2.1 and §2.2 are not novel and
are not eccentric. They are Zig's answers, reached independently, for the same
two reasons.

The monomorphization column is also where the two strongest precedents
**disagree**, so Vyrn must choose rather than cite. rustc keeps one generic body
and lets each consumer instantiate; Zig gives each instantiation its own AIR.
Both work. The tiebreak is Vyrn's own principle, "a backend encodes what was
decided": a substitution is a decision, and a generic body leaves it in three
places. Choose Zig's, and keep rustc's as the named fallback (§4 amendment 5).

The other column is the warning. **Every system whose IR text is a test artifact
made the text a supported entry point with its own tooling. The one system that
treated the dump as a debug convenience is the one whose dump broke.** rustc adds
the refinement that matters most: the text can be a test artifact *without* being
a stable contract. Its dumps carry the sentence "subject to change without
notice. Knock yourself out." and its `.mir` snapshots are blessed
(`compiler/rustc_middle/src/mir/pretty.rs`,
[tests/mir-opt/README.md](https://github.com/rust-lang/rust/blob/master/tests/mir-opt/README.md)).

---

## 2. Token efficiency as a design force

An LLM agent works on this compiler. Reading is the budget, the same way cache
lines are the budget for a data layout. Four consequences, each measured.

### 2.1 The dump: what `vyrn emit-lowered` should print

**Naming, first.** The brief asks for `vyrn build --emit-lowered`. The repository
spells this kind of thing as a subcommand: `vyrn emit-ir`, `vyrn emit-gen`
(`compiler/vyrn-cli/src/main.rs:6-7`). Use `vyrn emit-lowered <file>`.

**Scope.** Print the root module's functions only, by default. `vyrn why
--memory` already decided this and wrote down why: "Only the file asked about. A
linked program carries every import's functions, and they are another file's
answer" (`compiler/vyrn-cli/src/main.rs:1139-1141`). Median example is 67 lines;
its linked program is not.

**Shape.** One decision per line. Indentation is structure. Every line ends in a
position. Sketch:

    ; vyrn lowered v1 — examples/slottable.vyrn
    fn main() -> Int64                                        @3
      let xs : Array<Int64> = array<Int64>[ 1, 2, 3 ]         @4
      let n  : Int64        = at<Int64>(xs, 0) !aoob          @5
      if lt(n, 3) : Bool                                      @6
        call print(n) -> Unit                                 @7
      end                                                     @6
      release xs : FreeArr                                    @8 exit=fn
      return 0                                                @8

    fn map<Int64,Str>(xs: Array<Int64>, f: fn(Int64)->Str) -> Array<Str>   @std/list.vyrn:12
      ...

Five rules make that dump cheap to read and cheap to diff.

1. **A type on every binding and every call result.** `grep ': Array<'` answers
   a whole class of question in one command. The type is the shared decision, so
   it must be visible; RFC-0101 §2.1 item 2 already says it is a field, not a
   side table, and printing it costs nothing extra.
2. **A trap site is one token.** `!aoob` names a row in the one trap table
   RFC-0101 M5 creates. `grep '!'` lists every trap a program can reach, with its
   source line. Today the same question needs the walk in §2.2 below.
3. **A release is a line, not an inference.** `release xs : FreeArr @8 exit=fn`
   is `own`'s answer placed. `grep '^ *release'` answers "what is freed, where,
   how" for a whole program. The precedent is in the repository:
   `DropKind::words()` (`compiler/vyrn-frontend/src/own.rs:94`) already exists so
   that `vyrn why --memory` and the LSP print the same sentence, and
   `why_memory` is documented as "a **printer**. Every word comes out of
   `own::Ownership`, recorded by the walker that decided — never re-derived here"
   (`compiler/vyrn-cli/src/main.rs:1093-1095`). The dump extends one printer; it
   does not invent a second reading.
4. **Instantiations are spelled, symbols are not.** `fn map<Int64,Str>` in the
   dump; `mangle_ty`'s output stays in the emitters. Defect #165 was a mangled
   string used as an identity. A dump that shows the mangle invites the same
   confusion into every bug report.
5. **Positions in their own column, always last.** A diff that only moves
   positions then looks different from a diff that changes semantics, at a
   glance, without reading either.

**Determinism is a gate, not an intention.** The repository already learned this
once. `compiler/vyrn-cli/tests/reproducible.rs:8-17` records the defect: one
`HashSet`, iterated by the direct backend to reserve ownership words, built "SIX
different modules from this one file — same length, first difference at byte
1016". The test crosses a process boundary on purpose, because "a `HashSet`
iterates identically twice inside one process". `emit-lowered` needs a row in
that file on the day it lands, alongside
`the_same_source_emits_the_same_ir_in_every_process`. Sort functions by module
then name then rendered type arguments. Never print from a `HashMap`.

**Verdict.** The dump is not a debugging convenience. It is the artifact that
makes every other claim in this document checkable, and it is cheap: a printer
over a form that already holds every fact. Build it in M1, gate it in M1, and do
not build a parser for it.

### 2.2 The reading cost of the design itself, measured

**The question:** *where does an array bound get checked in the wasm backend?*

**Today, for the wasm backend alone.** `grep` finds `bounds_check`
(`compiler/vyrn-codegen/src/direct.rs:9094`, 34 lines) and `bounds_check_span`
(`:9128`, 39 lines). Neither is readable without `Walk`, produced by `walk`
(`:9000`, 78 lines). There are five call sites — `:3481` inside `stmt` (the
indexed store), `:6704` (SIMD), `:9523` inside `at`, `:9626` inside
`swap_remove`, `:11948` inside `sa_method` — and each needs its context to say
which index expressions reach it. The *wording* is not there at all: it is three
interned pieces (`:12611-12615`) rendered by a runtime function `trap_idx`
(`:13385`, 32 lines) that writes three times because wasm has no varargs.
**Eight regions, about 430 lines, and the message is assembled in a ninth
place.**

**Today, to answer it in the way a parity failure demands** — that is, for all
three engines, because a red parity run does not tell you which engine is wrong:
add `compiler/vyrn-codegen/src/lib.rs:1187-1195` (the two format strings),
`emit_array_oob_trap` (`lib.rs:7081`, 29 lines), three inline compare-and-branch
sites at `lib.rs:4745`, `:4779`, `:4799` — which sit inside a 587-line function
— and the SIMD site at `:8922`. Then
`compiler/vyrn-frontend/src/interp.rs` at `:940`, `:3596`, `:3608`, `:4044`,
`:4053`, `:4247`, `:4258`, `:5255`, `:5267`. **Twenty-three sites in three
files, well over 700 lines, and no site references any other except through a
comment.**

**The comments are the index.** A grep for the phrases that assert cross-engine
agreement — "mirrors the interpreter", "byte-for-byte", "byte-identical", "all
three backends" and their variants — returns **57 hits** across the three engine
files. `compiler/vyrn-codegen/src/direct.rs` is 26% comment (4,270 of 16,180
lines). That density is not a flaw. It is the only cross-reference the code has.

**After the lowered form.** `vyrn emit-lowered f.vyrn | grep '!aoob'` prints one
line per checked index, with its source line, for the program. To answer "which
expressions get a check", read the lowering's index arm — one place — instead of
five call sites in one engine and thirteen in the others. The trap wording is one
row in one table. **One command and one function replace 23 sites.**

**Verdict.** The reduction is roughly two orders of magnitude in reading, and it
is not the design's side effect — it is the design. The rule to write into
RFC-0101: **a decision lives in one file, and the dump names that decision at
every site it applies.** Where a decision has to be re-stated for a target, it is
not a decision; §2.3 of RFC-0101 already lists what is allowed to be re-stated.

### 2.3 Migration in agent-sized phases

**The measured budget.** Over the last 30 commits on `main`, median insertions
per commit are **611** and median total churn is about **700 lines across 10
files**. That is what this repository has already proved one agent can land, and
it is the budget. Call it **≤ 800 changed lines, ≤ 15 files, per phase.**

**RFC-0101's six milestones do not fit it.** Three of them break the budget
outright.

- **M3** deletes `peek` (510 lines, 49 call sites, all verified) and rewires the
  `(String, Type)` convention. Here RFC-0101 overstates and understates at once.
  It says the convention covers "~300 functions"; measured, **16 functions return
  `Result<(String, Type), String>`** and the spelling appears 40 times, in a file
  with 400 `fn`s. So the signature change is smaller than claimed. But the gate
  — "at least −1,200 lines across the two backends" — plus 49 call-site rewrites
  plus both `expect` stacks is still well over 800 lines of churn in one PR.
- **M4** proposes −900 lines across three engines, replacing `emit_drop` (180),
  `rel_at` (239), `deep_release` (149), three scope-frame stacks and three
  break/continue boundary indices. That is three encoders rewritten together, and
  RFC-0101 itself calls it "the milestone the bug ledger pays for". It is the one
  that most needs to be small, and it is the largest.
- **M6** points `interp::expr` (1,847 lines, `interp.rs:3878-5725`) and
  `interp::stmt` (775 lines, `:3025-3800`) at the lowered form. Both figures in
  RFC-0101 §2.4 are exactly right, and 2,622 lines of walk is not one PR.

**The correction: shadow, then delete.** RFC-0101 already invented the right
pattern and applied it to one milestone. M1 "deletes nothing on purpose" and adds
a corpus test asserting that the recorded type equals what `peek` and the native
backend derive. **Make that the rule for every phase.** Each becomes two PRs:

- **A — shadow.** The lowering computes the answer. A corpus gate asserts it
  equals what each engine derives today. Nothing is deleted, nothing changes
  behaviour, parity is trivially green, and a disagreement is a bug this PR found
  rather than a regression the next PR caused. Additive, small, and its line
  count is stated rather than excused.
- **B — delete.** The engines read the recorded answer; their derivation goes.
  Nearly all deletion. A deleted line is the cheapest line an agent can review.

Twelve PRs instead of six. Each is half the size, each is independently green,
and the risky half of each pair is the small additive one.

**And split by construct, not only by mechanism.** M4B cannot land as one PR
even after shadowing. Split the *placement* by exit kind, in the order the bug
ledger ranks them: block exit first, then `break`/`continue`/`return`, then `?`
and match-arm handover. That works only if a construct can be half-migrated —
which is the real answer to RFC-0101's open question 1.

**Open question 1 (own or borrow) is decided by the migration, not by
performance.** RFC-0101 asks whether `Lowered` owns or borrows, and proposes
measuring the owned version. Measure it, but the migration already constrains the
answer: a form whose nodes keep a reference to the AST node they came from lets
each engine migrate one arm at a time and fall back to the old walk for the rest.
Without that, every phase-B PR is all-or-nothing per engine, and M6 has no legal
intermediate state. Zig's answer points the same way but for a different reason:
`InternPool` keeps one generic owner and one entity per instantiation
([InternPool.zig](https://github.com/ziglang/zig/blob/master/src/InternPool.zig)),
so a body is not copied per substitution. **Recommend: borrow during migration,
and revisit after M6 when the fallback arms are gone.**

**One thing RFC-0101 gets right and should keep loudly.** "A milestone that fails
its gate says so in this file." RFC-0094 did exactly that: its M1 demanded a net
reduction, "measured **+149**", and the RFC records the failure, the move and
M2's number in three places
(`rfcs/RFC-0094-a-builtin-is-a-declaration.md:9-13`, `:400-410`, `:456-471`).
That is the discipline. A twelve-PR arc needs it more than a six-PR one, not
less.

### 2.4 Failure output as tokens

**The bad precedent, and it is the parity harness itself.** On divergence,
`compiler/vyrn-cli/tests/parity.rs:118-123` pushes:

    {name}: DIVERGED
      exit: interp {i_code:?} vs native {n_code:?}
      stdout interp: {i_out:?}
      stdout native: {n_out:?}
      stderr interp: {i_err:?}
      stderr native: {n_err:?}

Two whole program outputs, `{:?}`-escaped onto one line each, with every `\n`
spelled `\\n`. For a corpus whose largest example is 944 lines, that is a wall of
escaped text in which the agent must find the first difference by eye. Then the
same block again for the wasm column (`:167-172`).

**The good precedent, and it is in the same repository.** `limits.rs` asserts
that a compiler that used to die now says something, and reads the number "from
the code that enforces them, so a test cannot pin a limit the compiler no longer
takes" (`compiler/vyrn-cli/tests/limits.rs:11-13`). `reserved.rs` asserts a
count. `reproducible.rs` compares bytes and reports the offset of the first
difference. These fail with a fact, not a transcript.

**Three fixes, in increasing cost.**

1. **Available today, independent of RFC-0101.** Replace the two `{:?}` dumps
   with a unified diff: the first differing line number, that line from each
   engine, and two lines of context. The information an agent needs is *where*
   they diverge and *what* differs there, not the whole of both. This is a small
   change to one function and it pays on every red run from now on.
2. **After M1.** When a parity run goes red, print the lowered function the
   diverging output came from. The dump makes the failure name a decision instead
   of a symptom.
3. **After M6.** The lowered form is the single artifact, so a semantic change
   shows up as a diff of the checked-in dump inside the PR itself. This is
   rustc's `mir-opt` model and Swift's `.sil` model: the reviewable delta is the
   IR, not the behaviour. Snapshot a small corpus — ten examples, not 161 — and
   give it a `--bless` flag. Ten small snapshots are read; 161 large ones are
   skipped.

**Verdict.** Fix 1 now; it needs no RFC. Fixes 2 and 3 are the payoff RFC-0101
should claim in §4 and does not.

### 2.5 One thing the literature has nothing to say about

I found no compiler designed for machine reading. Every textual IR convention
above — MLIR's round-trip, Swift's `.sil` filetests, Cranelift's `.clif`
filetests, rustc's blessed `.mir` snapshots — was designed so a *person* could
read a pass's output and check it into a test. That the same properties serve an
agent is a coincidence, and a lucky one: determinism, one fact per line, a stable
grammar and a small diff are what both readers want.

Where they differ is scale. A person reads one function of a dump. An agent reads
the dump with `grep` and pays for every line it does not need. So one convention
is worth deriving from first principles rather than copying: **default to the
root module, and make the dump greppable by prefix.** `release`, `fn`, `let`,
`!trap` at a known position in the line means one command answers a whole-program
question. None of the systems above optimizes for that, because none of them was
read by a program that pays per token.

---

## 3. Where the code and the literature contradicted the brief

Stated plainly, because both matter more than agreement.

1. **The brief says `vyrn build --emit-lowered`.** The repository spells emitters
   as subcommands (`vyrn emit-ir`, `vyrn emit-gen`,
   `compiler/vyrn-cli/src/main.rs:60`). Use `vyrn emit-lowered`.
2. **The brief calls `peek` the thing an agent hunts through on a parity
   failure.** `peek` is 510 lines and is a *type* derivation
   (`direct.rs:4740-5249`, verified exactly). The parity hunt described in §2.2
   goes through bounds checks, trap wordings and release walks — `peek` is not on
   that path. `peek` is M3's deletion target, not M4's.
3. **Cranelift does not flatten structured control flow for you.** RFC-0101 §2.2
   says Cranelift "flattens on the way in". `FunctionBuilder` builds SSA
   ([docs.rs](https://docs.rs/cranelift-frontend/latest/cranelift_frontend/)); the
   producer builds the CFG. Wasmtime writes its own control stack for it.
4. **RFC-0101 overstates the `(String, Type)` convention.** It says "a convention
   over ~300 functions". Measured: 16 functions return it, 40 textual
   occurrences, in a file with 400 `fn`s. M3 is smaller than advertised on that
   axis.
5. **RFC-0101 promises columns the AST does not have.** §2.1 item 6 says "Every
   node keeps the line and column it came from". `ast.rs` has `line: usize` 40
   times and `col: usize` twice. Adding columns everywhere is a pervasive parser
   change — the cost §2.5 says the design avoids.
6. **The Zig belief that its AIR dump is a test artifact is wrong.** It is
   debug-only, and it has broken three times in the issue tracker. That is the
   cautionary half of the strongest positive precedent.
7. **QBE's slogan is "industrial optimizing compilers", not "advanced
   compilers", and QBE uses phi nodes, not block parameters**
   ([c9x.me/compile](https://c9x.me/compile/),
   [il.html](https://c9x.me/compile/doc/il.html)). It also accepts non-SSA input
   and fixes it up, which is the more useful fact for a producer.
8. **GHC does not run every optimization on Core.** STG and Cmm have their own
   passes ([users guide](https://ghc.gitlab.haskell.org/ghc/doc/users_guide/using-optimisation.html)).
   The small-core claim is about the *main* loop.
9. **rustc's shared MIR is not monomorphized.** The brief and RFC-0101 §2.1 item
   1 both assume the shared form holds one body per instantiation. rustc holds
   one generic body per `DefId` and lets codegen and the interpreter each
   substitute at the use site. Zig does it the other way. Vyrn must choose, and
   §4 amendment 3 says which and why.
10. **The brief's suspicion about cg_clif is correct.** `rustc_codegen_cranelift`
    does not implement `BuilderMethods`; a code search returns zero hits, against
    many in `rustc_codegen_llvm` and `rustc_codegen_gcc`. It consumes MIR
    directly and writes its own place, discriminant, vtable, ABI and intrinsic
    layers.
11. **Miri finds undefined behaviour, not backend divergence.** The brief hopes
    the shared MIR makes parity structural. It has surfaced
    optimizer-versus-interpreter disagreement — `-ReferencePropagation` and
    `-GVN` are disabled in `MIRI_DEFAULT_ARGS` for exactly that — but it
    surfaced it by letting the interpreter opt out, not by forcing agreement.
12. **There is no MIR dump stability policy, and that is the finding.** Every
    dump carries "subject to change without notice. Knock yourself out."
    (`compiler/rustc_middle/src/mir/pretty.rs`). Stability is enforced by a
    blessed snapshot suite, not promised by a contract. That is a better answer
    than the one the brief was looking for.

---

## 4. What RFC-0101 should change

Fifteen amendments. The design holds; none of these reverses it. Seven touch the
form, three the migration, five the tooling.

**The form**

1. **Name Zig's AIR as the precedent, in §2.1.** Every structural choice in §2.1
   and §2.2 is AIR's, reached independently
   ([Air.zig](https://github.com/ziglang/zig/blob/master/src/Air.zig),
   [codegen.zig](https://github.com/ziglang/zig/blob/master/src/codegen.zig)).
   *Reason:* an RFC that proposes a novel shape is argued; an RFC that proposes a
   shipped shape is checked. It also tells a future reader where to look when the
   form turns out wrong.

2. **Reword §2.4.** Replace "parity stops testing whether three copies agree"
   with the smaller true claim: the form makes the three engines' differences
   *declared* rather than accidental. *Reason:* Miri runs the same MIR as the
   backends and still disables five passes over it for better diagnostics
   (`MIRI_DEFAULT_ARGS`, `miri/src/lib.rs`); Vyrn's interpreter already skips
   releases on the `?` path on purpose (`interp.rs:2760`, recorded in RFC-0101
   §1.4). The RFC states both facts and then makes a claim that does not survive
   them.

3. **Decide monomorphization explicitly, and name the fallback.** Keep §2.1 item
   1 — concrete bodies, one per instantiation, identity is the type arguments —
   because a substitution is a decision and a generic body leaves it in three
   engines. But say that rustc chose the other way and why: `TyCtxt::instance_mir`
   is keyed on `DefId`, and both `codegen_mir` and `Machine::load_mir` substitute
   at the use site. *Reason:* this is the one place the two strongest precedents
   disagree, so the RFC must choose rather than cite. State the fallback with a
   threshold M1 can measure: if concrete bodies cost more than *(pick a number)*
   of peak memory on the largest corpus module, switch to one generic body plus
   one instance list and one shared substitution helper — which also answers open
   question 1's second half.

4. **Amend §2.1 item 6 to say "line", not "line and column".** *Reason:* the AST
   has `line: usize` 40 times and `col: usize` twice
   (`compiler/vyrn-frontend/src/ast.rs`). Adding columns everywhere is the
   pervasive parser change §2.5 says this design avoids, and no trap message
   prints a column. If a later consumer needs columns, that is its own RFC.

5. **Add one rule to §2.3: the form is the contract, and there is no shared
   emitter interface.** *Reason:* rustc built one — `BuilderMethods` — and its
   newest backend declined it. A code search for `BuilderMethods` under
   `compiler/rustc_codegen_cranelift` returns zero hits, while
   `rustc_codegen_llvm` and `rustc_codegen_gcc` both implement it. Two of three
   backends share the instruction abstraction; three of three share the IR. §2.3
   currently gets this right by omission, and an omission is not a rule.

6. **Decide open question 1 as "borrow, during migration".** *Reason:* a borrowed
   node can carry the AST node it came from, which is the only thing that lets an
   engine migrate one arm at a time and fall back for the rest. Without it, every
   phase-B PR is all-or-nothing per engine and M6 has no legal intermediate state.
   Measure the owned version too, but the migration constrains the answer before
   performance does.

7. **Add a lint over `Lowered`, in debug builds, permanently.** A form with a
   type on every node admits an independent type-check over itself. That is
   `-dcore-lint`
   ([GHC users guide](https://downloads.haskell.org/ghc/latest/docs/users_guide/debugging.html)),
   described as "a 100% independent check on the type inference engine"
   ([aosabook](https://aosabook.org/en/v2/ghc.html)), and it is what Swift's
   `SILVerifier` does for ownership until ownership is lowered
   ([Ownership.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/Ownership.md)).
   *Reason:* M1's corpus gate is a one-off version of this. Making it permanent
   costs one pass and turns "the lowering is right" from a migration claim into a
   standing invariant. Both systems that verify their IR verify it continuously.

**The migration**

8. **Restructure as shadow-then-delete pairs, and state the line budget.** Twelve
   PRs, each ≤ 800 changed lines and ≤ 15 files. The repository's measured median
   over its last 30 commits is 611 insertions across 10 files. *Reason:* M3, M4
   and M6 as written each exceed what one agent can land in one context. M1
   already invented the pattern — "This deletes nothing on purpose" — and applies
   it once. Apply it six times.

9. **Split M4 by exit kind.** Block exit; then `break`/`continue`/`return`; then
   `?` and match-arm handover. *Reason:* it is the largest phase, it holds all
   three placement defects (#163, #166, #172), and each exit kind already has
   regression tests in `compiler/vyrn-cli/tests/memory.rs`.

10. **State M3's real size.** RFC-0101 calls the `(String, Type)` return
    convention "a convention over ~300 functions". Measured: 16 functions return
    `Result<(String, Type), String>`, the spelling appears 40 times, and
    `compiler/vyrn-codegen/src/lib.rs` has 400 `fn`s. *Reason:* the phase is
    smaller than advertised on that axis and larger on the `peek` call-site axis
    (49 sites, verified). A gate written from a wrong number is a gate nobody can
    meet or miss on purpose.

**The tooling**

11. **Add `vyrn emit-lowered` to M1 as a deliverable, and gate it in M1.** Not an
    open question — a milestone item. Root module by default, following
    `why --memory`'s stated rule (`compiler/vyrn-cli/src/main.rs:1139-1141`), with
    a row in `compiler/vyrn-cli/tests/reproducible.rs` beside
    `the_same_source_emits_the_same_ir_in_every_process`. *Reason:* the dump is
    what makes M1's own gate readable, and Zig's ungated `--verbose-air` is the
    failure mode ([#7670](https://github.com/ziglang/zig/issues/7670),
    [#10031](https://github.com/ziglang/zig/issues/10031),
    [#12599](https://github.com/ziglang/zig/issues/12599)).

12. **Close open question 5 as "print, do not parse; unstable text, blessed
    snapshots".** *Reason:* MLIR's round-trip serves a pass pipeline
    ([LangRef](https://mlir.llvm.org/docs/LangRef/)) and Vyrn has one pass, so a
    parser would be a second front end written to test a printer. And the RFC's
    worry — "a rendering people read becomes a rendering people depend on" —
    already has rustc's answer: every MIR dump carries "subject to change without
    notice. Knock yourself out." (`compiler/rustc_middle/src/mir/pretty.rs`) while
    `tests/mir-opt` blesses `.mir` files
    ([README](https://github.com/rust-lang/rust/blob/master/tests/mir-opt/README.md)).
    Print a version line, promise nothing, bless the snapshots.

13. **Claim the snapshot payoff in §4, after M6.** A semantic change becomes a
    diff of a checked-in lowered dump inside the PR. rustc tells authors to bless
    and commit the dump *before* implementing an optimization, "so that you (and
    your reviewers) can see a before/after diff"
    ([optimizations.html](https://rustc-dev-guide.rust-lang.org/mir/optimizations.html)).
    Snapshot ten examples with a `--bless` flag, not 161. *Reason:* §4 lists what
    the design unlocks and omits the one that pays on every PR.

14. **Fix the parity failure message now, in its own PR, ahead of this RFC.**
    Replace the two `{:?}` output dumps
    (`compiler/vyrn-cli/tests/parity.rs:118-123`, `:167-172`) with the first
    differing line number, that line from each engine, and two lines of context.
    *Reason:* it is the failure output every milestone below will read, it costs
    one function, and it needs nothing from this RFC.

15. **Correct §2.2's Cranelift sentence and drop the 250 ms figure from §4.**
    `FunctionBuilder` builds SSA for you — `declare_var`/`def_var`/`use_var` over
    Braun et al.
    ([docs.rs](https://docs.rs/cranelift-frontend/latest/cranelift_frontend/),
    [ssa.rs](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/frontend/src/ssa.rs))
    — but it does not build the CFG. The producer writes the flattener, as
    Wasmtime does. And the published Rust measurement has Cranelift *slower* on
    incremental builds: 7.98 s against 5.48 s
    ([Inside Rust](https://blog.rust-lang.org/inside-rust/2020/11/15/Using-rustc_codegen_cranelift/)).
    *Reason:* §5 refuses to make a compile-speed claim and §4 makes one.

---

## 5. What this document does not settle

- **Whether the lowering costs compile time.** Nothing here measured it, and
  RFC-0101 is right to refuse the claim.
- **Whether the interpreter stays fast enough on the lowered form.** RFC-0101 §5
  names it and requires M6 to measure it. Miri proves the shape works and says
  nothing useful about speed: it runs at `-Zmir-opt-level=0` on purpose and is
  not a performance tool.
- **What happens to `movecheck`'s diagnostics** (RFC-0101 open question 2). Swift
  answers it by keeping diagnosis *before* canonicalization
  ([SIL.md](https://github.com/swiftlang/swift/blob/main/docs/SIL/SIL.md)), which
  is what Vyrn already does. That is a hint, not an answer.
- **The exact size of `Lowered`.** 34 AST constructs is the floor, not the
  figure. Swift needed 180 instructions for a much larger language; QBE needs far
  fewer for a much smaller job.
