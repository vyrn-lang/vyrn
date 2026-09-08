//! The structural census of `direct.rs` — RFC-0125 §3 M3, the size strand.
//!
//! `compiler/vyrn-codegen/src/direct.rs` is the ONE emitter. RFC-0125 §2.3 says
//! what it should be: it maps `prim` rows to wasm instructions, `load` and
//! `store` to typed loads and stores, `drop` to a call, `trap` to a call with a
//! table index, and control flow to wasm's blocks. "It decides nothing", it
//! carries no optimizer, and §2.7 says "the runtime hand-emitted by `direct.rs`"
//! is deleted. All nine steps of `PLAN-0125-runtime.md` §6 landed, track-cg
//! deleted the text-IR emitter and the C shim, and the file still stands at
//! sixteen and a half thousand lines. Nobody had counted it. `own.rs` was
//! censused by a reader against every part, `movecheck.rs` by the kind table in
//! `tests/refusals.rs` and `checker.rs` by `tests/checker_census.rs`; this is
//! the same measurement for the emitter.
//!
//! # The method, which is `checker_census.rs`'s
//!
//! A section is one item — a `fn`, a `struct`, an `enum`, an `impl`, a `mod`, a
//! `const` — together with every item after it up to the next section's anchor.
//! The span runs from the anchor's own doc comment to the line before the next
//! anchor's, so every line of the file belongs to exactly one section and the
//! counts add up to the file. The test computes the spans; the table below
//! records the anchor, the kind and a reader, so an edit to the file moves the
//! numbers and the classification stays where a reader put it.
//!
//! # The second column, and why
//!
//! The checker's census counts refusals beside lines, because a refusal is what
//! the checker produces. This emitter produces wasm, so the column beside lines
//! is the number of `Instruction::` sites a section holds: the wasm it writes by
//! hand. The two numbers say different things. A family that moves out of the
//! emitter — into `std/runtime.vyrn`, into the core, into a row-driven table —
//! moves both. A family that only gets shorter moves lines and leaves the
//! instruction count where it is, which is the reading that catches a deletion
//! that deleted prose rather than emission.

use std::path::{Path, PathBuf};

/// What a section of `direct.rs` is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The mapping RFC-0125 §2.3 names, and the whole of what an emitter is
    /// allowed to be: a `prim` row to its wasm instruction, a `load` or a
    /// `store` to a typed load or store at a computed address, a `drop` to a
    /// call, a `trap` to a call with a table index, and a control-flow form to
    /// wasm's blocks. Nothing replaces this.
    Mapping,
    /// A decision the emitter still makes that §2.3 says it must not: it places
    /// something, checks a bound it was not told to check, decides what a
    /// validated type is, optimizes, re-types an expression the record already
    /// types, or performs a rewrite that should be stated once before it. These
    /// are the deletion candidates; each names the pass that should state it.
    Decision,
    /// The runtime this emitter writes by hand — §2.7's "the runtime
    /// hand-emitted by `direct.rs`". `PLAN-0125-runtime.md` §6 moved the
    /// FUNCTIONS to `std/runtime.vyrn` over nine steps, so what is filed here is
    /// what those steps left: the declaration table, the index of it, and the
    /// few inline sequences step 9 lists.
    Runtime,
    /// One hand-written block per builtin name — the `builtins` factor of
    /// RFC-0125 §1.1 as this emitter pays it, the shape `Checker::call` had
    /// before RFC-0125 §3 M6 emptied it. A row-driven mapping over the builtin's
    /// seeded signature and the core's rows is what replaces one of these.
    Builtin,
    /// The wasm format rather than the language: the import tables, the ABI
    /// rules, the custom sections, the memory arguments. `wasm.rs` holds most of
    /// it; this is the part that stayed here.
    Encoding,
    /// Shared machinery: the driver, the monomorphisation queue, the contexts,
    /// the frames, the scratch locals, the name lookup.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Mapping => "the mapping §2.3 names",
            Kind::Decision => "a decision §2.3 says it must not make",
            Kind::Runtime => "the runtime it emits by hand",
            Kind::Builtin => "one block per builtin name",
            Kind::Encoding => "the wasm format",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// What a section READS to decide what it emits — RFC-0125 §2.3's own
/// sentence, counted: "the emitter reads the core and writes wasm".
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Reads {
    /// Neither: the section decides nothing about a source form. The wasm
    /// format, a layout, the driver, the file's own tests.
    Neither,
    /// The core's rows alone. This is what §2.3 asks every section to be.
    Core,
    /// The SOURCE form, and the core has a row that says the same thing. The
    /// AST read is a second statement of a rule, which is what this RFC is
    /// named after, and these are the switches to take.
    Twice,
    /// The source form, and the core states NO row for it. The core is
    /// incomplete for that form, and the ranked list in §3 M3 names the row
    /// and the builder each one needs before it can switch.
    Source,
    /// Both, for two different questions — the core for a release, a take or
    /// a type, and the form for what to emit. A section leaves this class by
    /// the core stating the second question too.
    Both,
}

impl Reads {
    fn label(self) -> &'static str {
        match self {
            Reads::Neither => "neither",
            Reads::Core => "the core's rows",
            Reads::Twice => "the source, and the core says it too",
            Reads::Source => "the source, and the core has no row",
            Reads::Both => "both, for two questions",
        }
    }
}

/// One section: the exact source line that starts it, its kind, what it reads,
/// and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    reads: Reads,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, reads: Reads, what: &'static str) -> Section {
    Section {
        at,
        kind,
        reads,
        what,
    }
}

/// The sections, in file order. The first one starts at line 1.
fn sections() -> Vec<Section> {
    use Kind::*;
    #[allow(clippy::enum_glob_use)]
    use Reads::{Both, Core, Neither, Source, Twice};
    vec![
        sec(
            "fn unsupported<T>(what: &str, line: usize) -> Result<T, String> {",
            Shared, Neither,
            "the module head, and the three wordings this backend states when it \
             cannot lower a construct — `unsupported`, `gap`, `too_big`",
        ),
        sec(
            "struct Wasi {",
            Encoding, Neither,
            "the two host-import tables: `wasi_snapshot_preview1` for a program, \
             and the generator host's `vyrn` namespace. §2.4's \"each a \
             declaration the emitter lowers to one `call`\"",
        ),
        sec(
            "struct Ext {",
            Encoding, Neither,
            "RFC-0012's `extern fn` as one wasm import, and the ABI its \
             declaration implies",
        ),
        sec(
            "pub fn compile(program: &Program) -> Result<Vec<u8>, String> {",
            Shared, Neither,
            "the four entry points: a module, its text, the generator host's \
             reachable set, and the generator host module",
        ),
        sec(
            "fn compile_inner(program: &Program) -> Result<Vec<u8>, String> {",
            Shared, Core,
            "the driver: reserve the index space, declare module state, walk the \
             declarations, drain the monomorphisation queue, sweep what no \
             export reaches",
        ),
        sec(
            "fn abi_kind(ty: &Type) -> &'static str {",
            Encoding, Neither,
            "the `vyrn.abi` custom section, written byte by byte — a wasm \
             section, not a rule of the language",
        ),
        sec(
            "enum MapKey {",
            Shared, Neither,
            "which key family a `Map` runs on, and whether a type travels in a \
             wasm value or as the address of a slot",
        ),
        sec(
            "struct Sig {",
            Shared, Neither,
            "the monomorphisation state: a signature, an instance key, a pending \
             body, the function-value tables",
        ),
        sec(
            "struct Cx<'a> {",
            Shared, Neither,
            "the whole-module context every lowering reads",
        ),
        sec(
            "fn receiver_row(&self, node: usize) -> Option<Vec<String>> {",
            Mapping, Core,
            "the nine readers of the core's rows — the receiver, the store, the \
             discard, the argument drop, the edges, the `match` consume, the arm \
             bindings. This IS \"the emitter reads the core\"",
        ),
        sec(
            "fn sub(&self, ty: &Type) -> Type {",
            Shared, Neither,
            "the type machinery on `Cx`: substitution, resolution, a layout's \
             words, a sum's variants, instantiation, and the gap wording for a \
             type this backend cannot represent",
        ),
        sec(
            "fn wasm_sig(&self, sig: &Sig, line: usize) -> Result<(Vec<ValType>, Vec<ValType>), String> {",
            Encoding, Neither,
            "M2b's four ABI rules: which parameters ride a wasm value and which \
             ride a hidden destination",
        ),
        sec(
            "enum Place {",
            Shared, Neither,
            "a place, a release and its slot, and the signature of a stream's \
             step function",
        ),
        sec(
            "impl Place {",
            Mapping, Neither,
            "a place's address, and the destination a store writes through — \
             §2.3's \"typed loads and stores at computed addresses\"",
        ),
        sec(
            "const LAMBDA: &str = \"@lambda\";",
            Shared, Neither,
            "the shell a lambda is lowered as",
        ),
        sec(
            "struct Fn_<'a, 'p> {",
            Shared, Neither,
            "the per-function state: the frame, the scopes, the placed releases, \
             the open regions, the stream cursors",
        ),
        sec(
            "fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx<'_>) -> Result<Frame, String> {",
            Shared, Twice,
            "RFC-0013's module state: the initializer and the teardown, and the \
             one-line entry to a function body",
        ),
        sec(
            "fn lower_body(",
            Mapping, Neither,
            "a function body: the parameters into locals, the prologue, the \
             epilogue, the return",
        ),
        sec(
            "fn frame_fits(b: &Frame, name: &str, line: usize) -> Result<(), String> {",
            Decision, Neither,
            "the frame-size refusal and the call-depth counter — a check the \
             emitter inserts and a limit it enforces. The language states the \
             depth (`vyrn_frontend::trap`); the counter and its comparison are \
             the emitter's",
        ),
        sec(
            "fn lower_fnval_copy(cx: &Cx<'_>) -> Result<Frame, String> {",
            Shared, Source,
            "RFC-0037's defunctionalisation: the copy helper and the dispatcher \
             a stored closure is called through",
        ),
        sec(
            "fn scratch(&mut self, b: &mut Frame, t: ValType, n: u8) -> u32 {",
            Shared, Neither,
            "scratch locals, the string temporaries a call's arguments tee, and \
             the value a `return` leaves",
        ),
        sec(
            "fn block(&mut self, m: &mut Module, b: &mut Frame, blk: &Block) -> Result<(), String> {",
            Mapping, Neither,
            "a block: its statements, then the releases the core placed at its \
             brace",
        ),
        sec(
            "fn elem_field_store(",
            Mapping, Source,
            "`a[i].f = v` — the address, then the typed store",
        ),
        sec(
            "fn cached_walk(&self, e: &Expr) -> Option<Walk> {",
            Decision, Source,
            "THE OPTIMIZER. A loop whose header proves an array's base and count \
             invariant has its walk hoisted out of the body and cached. §2.3: \
             \"The emitter carries no optimizer, ever\" — and M1 measured this \
             one and kept it (nbody under V8, 2.97 s to 2.16 s). It is filed \
             here because the sentence is the sentence; what moves it is the \
             core making a loop-invariant header a named place, not a deletion",
        ),
        sec(
            "fn emit_releases(",
            Mapping, Source,
            "the core's drop rows at an exit become calls, in the order the core \
             placed them — §2.3's \"`drop` to a call\"",
        ),
        sec(
            "fn rel_for(&mut self, ty: &Type, line: usize) -> Result<Option<Rel>, String> {",
            Decision, Neither,
            "what releasing a type MEANS, derived here from the type's shape: a \
             recursive walk over records, elements, payloads and buffers. §2.3 \
             says a drop is a call, and since the two-shape-walks slice it IS \
             one — `rel_at` emits a call and this walk is that function's body, \
             written once per type. What is still filed here is the SHAPE, \
             because every step of it is a byte offset and a byte offset is \
             `layout.rs`'s: no pass above this crate can state it",
        ),
        sec(
            "fn addr_local(&mut self, b: &mut Frame, p: Place, off: u32) -> u32 {",
            Decision, Neither,
            "the snapshot family: which buffers a store overwrites and must free \
             first, read off the type here rather than named by the core's store \
             row",
        ),
        sec(
            "fn region_enter(&mut self, b: &mut Frame) {",
            Mapping, Neither,
            "RFC-0004's `region`: the enter and exit calls, the arena routing \
             flag `std/runtime` reads, and `stringFromBytes`'s check",
        ),
        sec(
            "fn lookup(&self, name: &str, line: usize) -> Result<(Place, Type), String> {",
            Shared, Neither,
            "a name's place: the scope stack, then module state",
        ),
        sec(
            "fn stmt(&mut self, m: &mut Module, b: &mut Frame, s: &Stmt) -> Result<(), String> {",
            Mapping, Both,
            "one arm per statement form — `let`, assignment, `if`, `while`, \
             `for`, `break`, `continue`, `return`, `defer`. The control flow of \
             §2.3, plus the surface forms that reach the emitter unrewritten",
        ),
        sec(
            "fn cond(&mut self, m: &mut Module, b: &mut Frame, e: &Expr, line: usize) -> Result<(), String> {",
            Mapping, Neither,
            "a condition, a place for a representation, the string append, and \
             the two stores an aggregate takes",
        ),
        sec(
            "fn expr_as(",
            Mapping, Neither,
            "an expression lowered at a wanted type",
        ),
        sec(
            "fn coerce(",
            Decision, Neither,
            "THE COERCION LADDER — §2.7 deletes it in both backends. Which \
             conversions are implicit is a typing rule, and the emitter \
             re-decides it here",
        ),
        sec(
            "fn proven(&self, e: &Expr, to: &Type) -> bool {",
            Decision, Source,
            "what a validated type is, and the check inserted at every value \
             boundary. §2.3: it \"does not know what a validated type is\"",
        ),
        sec(
            "fn expr(&mut self, m: &mut Module, b: &mut Frame, e: &Expr) -> Result<Type, String> {",
            Mapping, Both,
            "one arm per expression form",
        ),
        sec(
            "fn applied_record(",
            Mapping, Core,
            "a record and a variant built into their slots, and the join of a \
             branch that yields",
        ),
        sec(
            "fn peek(&mut self, e: &Expr, line: usize) -> Result<Type, String> {",
            Shared, Both,
            "the type of an expression the emitter has not emitted yet. This WAS \
             the class: twenty arms deriving a type the checker had already \
             stated. RFC-0125 §3 M5 emptied them — `peek` reads the record by \
             node, and `peek_inner` answers only for the trees this backend \
             builds itself, which RFC-0101 §2.3 assigns to the backend",
        ),
        sec(
            "fn regex_dfa(",
            Builtin, Neither,
            "the regex builtin's table",
        ),
        sec(
            "fn binary(",
            Mapping, Neither,
            "an operator, and the releases the core placed on its edges",
        ),
        sec(
            "fn binary_inner(",
            Mapping, Both,
            "one arm per operator — the `prim` rows of §2.3, at their widths",
        ),
        sec(
            "fn user_claims(&self, name: &str) -> bool {",
            Builtin, Both,
            "RFC-0076 M7's generation-time builtins: the `Code` handle \
             operations and the atom stream, one block each, plus the two string \
             renderings they share with `print`",
        ),
        sec(
            "fn call(",
            Mapping, Neither,
            "a call: the argument temporaries it drains afterwards, and which \
             names lend their result",
        ),
        sec(
            "fn call_inner(",
            Builtin, Both,
            "ONE HAND-WRITTEN BLOCK PER BUILTIN NAME — 84 names in one `match`. \
             The shape `Checker::call` had before RFC-0125 §3 M6 emptied it, and \
             the largest single item in the file",
        ),
        sec(
            "fn mem_prim(",
            Mapping, Neither,
            "`std/mem`'s primitives, one wasm instruction each \
             (`PLAN-0125-runtime.md` §2.1) — the clearest `prim` row in the file",
        ),
        sec(
            "fn log_write(",
            Builtin, Neither,
            "RFC-0008's log write",
        ),
        sec(
            "fn reflected(&self, which: &str, arg: &Expr, line: usize) -> Result<Expr, String> {",
            Builtin, Both,
            "RFC-0094 M3's reflection: which `show` a type renders itself with, \
             and the variant a `value` box carries",
        ),
        sec(
            "fn emit_call(",
            Mapping, Both,
            "the call itself: a direct call, a higher-order call, and RFC-0037's \
             stored function values with their capture blocks and dispatchers",
        ),
        sec(
            "fn length_of(",
            Builtin, Neither,
            "`len` over every receiver that has one, and RFC-0025's `spawn`",
        ),
        sec(
            "struct Walk {",
            Builtin, Source,
            "RFC-0075's `Stream<T>`: one hand-written block per stream operation \
             — from an array, from a step, box, unbox, pull, next, release, and \
             the `for` over one",
        ),
        sec(
            "fn walk(&mut self, b: &mut Frame, ty: &Type, line: usize) -> Result<Walk, String> {",
            Mapping, Neither,
            "a receiver's base and count, and an element's address — the \
             computed address of §2.3",
        ),
        sec(
            "fn trap_row(&mut self, b: &mut Frame, rule: vyrn_frontend::trap::Rule, val: Option<u32>) {",
            Mapping, Neither,
            "a trap is a call with a table index. §2.3, exactly",
        ),
        sec(
            "fn bounds_check(&mut self, b: &mut Frame, w: &Walk, idx: u32, string: bool) {",
            Decision, Neither,
            "the bounds check, and the span check a SIMD access takes. §2.3: it \
             \"does not check bounds it was not told to\" — the core has no \
             `check` row to tell it",
        ),
        sec(
            "fn load_elem(&mut self, b: &mut Frame, w: &Walk, line: usize) -> Result<(), String> {",
            Mapping, Neither,
            "the typed load of an element",
        ),
        sec(
            "fn array_lit(",
            Builtin, Both,
            "the `Array` family, one hand-written block per operation: the \
             literal, the heap literal, `push`, `pop`, `at`, `swapRemove`, \
             `@reserve`, `@append`, `@copyFrom`, `@clear`",
        ),
        sec(
            "type Sum = Vec<EnumVariant>;",
            Shared, Source,
            "a sum's tag, and the two-word encoding a payload rides in",
        ),
        sec(
            "fn owns_heap(&self, ty: &Type) -> bool {",
            Decision, Neither,
            "the deep-copy family: what copying a type MEANS, derived here from \
             its shape, in the same recursive-walk shape as the release above — \
             and a call at the site for the same reason, one body per type",
        ),
        sec(
            "fn copy_word(",
            Mapping, Both,
            "sums: build a variant, box a payload, name a constructor's types",
        ),
        sec(
            "fn match_expr(",
            Mapping, Both,
            "`match`: the scrutinee, the tag test, the arms, the join — control \
             flow to wasm's blocks",
        ),
        sec(
            "fn try_(",
            Decision, Both,
            "the emitter states the `?` rewrite a second time — the tag test, \
             the whole sum copied out through the function's own destination, \
             and the payload read on the fall-through — for the built-in sums \
             and for `Fallible` (RFC-0080 M3) beside it. `core.rs`'s `Expr::Try` \
             arm states the same rewrite, as a two-arm switch and a `return` \
             row; RFC-0127 §4 refuses the parser for it, because an \
             expression-position `?` has no statement to put a `return` in. It \
             leaves when the emitter reads the core",
        ),
        sec(
            "fn try_construct(",
            Decision, Neither,
            "`Age?(n)` (RFC-0003): the argument is evaluated at the refinement's \
             BASE type and the predicate's own answer becomes the tag, which is \
             §2.3's \"does not know what a validated type is\" — the same row as \
             `emit_validation`, waiting on the same `check` row. RFC-0127 §5.3 \
             refuses folding the node into `Expr::Call`",
        ),
        sec(
            "fn optional_if_let(",
            Decision, Both,
            "an OPTIONAL projection tested by `if let` (RFC-0122). The expansion \
             is `project::optional_site`'s and is shared with the checker and \
             the lowering, but the binding is this file's: a synthetic `let` \
             with no analysis row, and the guard that a declared function of the \
             name wins over a projection. `core.rs`'s `Stmt::IfLet` arm lowers \
             the ordinary switch and knows nothing of this form, so there is no \
             row to read yet",
        ),
        sec(
            "fn tag_test(",
            Mapping, Both,
            "a tag test and the binders a pattern's payload opens",
        ),
        sec(
            "fn map_lit(",
            Builtin, Core,
            "RFC-0028 and RFC-0117's `Map`, one hand-written block per operation: \
             the literal, `@tally`, `@tallyBytes`, set, scan, put, reserve, the \
             key pack, `@at`, and the method table",
        ),
        sec(
            "fn sa_parts(&mut self, b: &mut Frame, hdr: u32, l: &Layout, n: usize) -> (u32, u32, u32) {",
            Builtin, Neither,
            "RFC-0080's `SmallArray`: its parts, its literal, its push, and its \
             method table",
        ),
        sec(
            "fn at(off: u32) -> MemArg {",
            Encoding, Neither,
            "the memory arguments a load and a store carry",
        ),
        sec(
            "struct Num {",
            Mapping, Neither,
            "a number's width and signedness, the renormalisation a narrow width \
             needs, and the integer opcode per operator",
        ),
        sec(
            "fn load_of(ll: &str, off: u32, signed: bool) -> Instruction<'static> {",
            Mapping, Neither,
            "the typed load, by the low-level type name",
        ),
        sec(
            "vyrn_frontend::body_scope_descent!(HoistVisit, hoist_block, hoist_stmt, hoist_expr);",
            Decision, Source,
            "the AST walks the hoist above needs, and the header-invariance \
             proof it runs: does the loop body write the name, rebind it, or \
             call anything that could. An optimizer's analysis, in the emitter. \
             The walks READ `ast::body_scope_descent!` since RFC-0125 §3 M6's \
             second body slice; what is left is the one line this reader writes \
             at a node — hand it over, and stop at a lambda",
        ),
        sec(
            "fn store_of(ll: &str) -> Instruction<'static> {",
            Mapping, Neither,
            "the typed store, by the low-level type name",
        ),
        sec(
            "const VYRN_RUNTIME: &[(&str, &[ValType], &[ValType])] = &[",
            Runtime, Neither,
            "the runtime's declaration table: 40 rows `std/runtime.vyrn` defines \
             and this emitter calls, each with the wasm signature the two have \
             to agree about",
        ),
        sec(
            "struct Rt {",
            Runtime, Neither,
            "the index of every runtime function and every interned string the \
             emitter names. It carried the NUMBERING too — a slot allocator, a \
             dense-index table, a count and a per-helper order assertion — until \
             §6 step 9 moved the last four functions to Vyrn and left the \
             machinery with nothing to number",
        ),
        sec(
            "const SHDR: u32 = 8;",
            Runtime, Neither,
            "what nine steps of `PLAN-0125-runtime.md` §6 left: the `String` \
             header offsets, the arena flag's address, and the tag test — the \
             inline sequences step 9 lists, at their one site each",
        ),
        sec(
            "fn runtime(m: &mut Module, wasi: &Wasi, v: &VyrnRt) -> Rt {",
            Runtime, Neither,
            "wiring: every reserved index into its field, the interned messages, \
             the UTF-8 table both backends share, and the trap table `trapAt` \
             reads",
        ),
        sec(
            "const RIGHT_FD_WRITE: i64 = 1 << 6;",
            Shared, Source,
            "the two WASI constants `_start` opens a log sink with, and the \
             small type helpers the I/O builtins ask for their result type",
        ),
        sec("#[cfg(test)]", Tests, Neither, "the file's own unit tests"),
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn emitter() -> Vec<String> {
    let p = repo_root().join("compiler/vyrn-codegen/src/direct.rs");
    std::fs::read_to_string(&p)
        .expect("read direct.rs")
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Where a section's doc comment starts: the run of comment and attribute lines
/// straight above the anchor.
fn doc_start(lines: &[String], anchor: usize) -> usize {
    let mut i = anchor;
    while i > 0 {
        let t = lines[i - 1].trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

/// The sections, with the span each holds: `(index, first line, last line)`,
/// one-based and inclusive. Every line of the file is in exactly one span.
fn spans(lines: &[String]) -> Vec<(usize, usize, usize)> {
    let secs = sections();
    let mut anchors = Vec::new();
    for s in &secs {
        let want: String = s.at.split_whitespace().collect::<Vec<_>>().join(" ");
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.split_whitespace().collect::<Vec<_>>().join(" ") == want)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the anchor `{}` names {} lines of direct.rs; a section's anchor must name one",
            s.at,
            hits.len()
        );
        anchors.push(doc_start(lines, hits[0]));
    }
    let mut out = Vec::new();
    for i in 0..secs.len() {
        let first = if i == 0 { 0 } else { anchors[i] };
        let last = if i + 1 == secs.len() {
            lines.len()
        } else {
            anchors[i + 1]
        };
        assert!(
            first < last,
            "section `{}` of direct.rs is empty or out of order",
            secs[i].at
        );
        out.push((i, first + 1, last));
    }
    out
}

/// How many wasm instructions a span writes by hand.
fn instructions(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .map(|l| l.matches("Instruction::").count())
        .sum()
}

/// How many SOURCE forms a span names — `Expr::`, `Stmt::`, `Pattern::` and
/// `ArmBody::` in code, comments excluded, and `Expr::line` excluded with
/// them because an accessor is not a form.
fn forms(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| {
            ["Expr::", "Stmt::", "Pattern::", "ArmBody::"]
                .iter()
                .map(|p| l.matches(p).count())
                .sum::<usize>()
                - l.matches("Expr::line").count()
        })
        .sum()
}

/// How many CORE rows a span reads: the queries on [`Cx`] that reach
/// `vyrn_lower::core`, the two type answers the record carries, and
/// [`Fn_::peek`], which is the node type read by another name. One list, so a
/// query added to the emitter has to be added here before a section can be
/// classified as reading it.
fn rows(lines: &[String], a: usize, b: usize) -> usize {
    const Q: [&str; 13] = [
        "peek",
        "receiver_row",
        "store_row",
        "store_fact",
        "discarded_row",
        "loop_gives_back",
        "arg_drop_row",
        "edge_rows",
        "arm_row",
        "owns_scrutinee",
        "frees_boxes",
        "node_ty",
        "join_ty",
    ];
    lines[a - 1..b]
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| Q.iter().map(|q| l.matches(q).count()).sum::<usize>())
        .sum()
}

/// The sections tile `direct.rs`: every line is in one, in file order.
#[test]
fn the_structural_census_covers_the_file() {
    let lines = emitter();
    let spans = spans(&lines);
    let mut next = 1;
    for (_, a, b) in &spans {
        assert_eq!(*a, next, "a gap or an overlap at line {a} of direct.rs");
        next = b + 1;
    }
    assert_eq!(
        next - 1,
        lines.len(),
        "the last section does not reach the end of direct.rs"
    );
}

/// The line count and the hand-emitted instruction count per kind, as RFC-0125
/// §3 M3 records them. The prose quotes these numbers, so they are asserted
/// rather than described: a change to `direct.rs` moves one, and the RFC's table
/// moves with it.
#[test]
fn the_emitter_census_is_what_the_rfc_records() {
    let lines = emitter();
    let secs = sections();
    let mut by_kind = std::collections::BTreeMap::new();
    let mut ins_by_kind = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        *by_kind.entry(secs[i].kind as usize).or_insert(0usize) += b - a + 1;
        *ins_by_kind.entry(secs[i].kind as usize).or_insert(0usize) += instructions(&lines, a, b);
    }
    let got: Vec<(&'static str, usize, usize)> = [
        Kind::Mapping,
        Kind::Decision,
        Kind::Runtime,
        Kind::Builtin,
        Kind::Encoding,
        Kind::Shared,
        Kind::Tests,
    ]
    .iter()
    .map(|k| {
        (
            k.label(),
            by_kind.get(&(*k as usize)).copied().unwrap_or(0),
            ins_by_kind.get(&(*k as usize)).copied().unwrap_or(0),
        )
    })
    .collect();
    let want = vec![
        ("the mapping §2.3 names", 5720, 573),
        ("a decision §2.3 says it must not make", 2212, 356),
        ("the runtime it emits by hand", 625, 7),
        ("one block per builtin name", 4807, 978),
        ("the wasm format", 334, 0),
        ("shared machinery", 2388, 77),
        ("tests", 326, 0),
    ];
    assert_eq!(got, want, "the emitter census has moved");
    assert_eq!(
        got.iter().map(|(_, n, _)| n).sum::<usize>(),
        lines.len(),
        "the kinds do not add up to the file"
    );
    assert_eq!(
        got.iter().map(|(_, _, n)| n).sum::<usize>(),
        instructions(&lines, 1, lines.len()),
        "the instruction counts do not add up to the file"
    );
}

/// What each section READS, per class, as RFC-0125 §3 M3 records it — and the
/// mechanical half of the classification, which is what stops it rotting.
///
/// A reader puts a section in a class; the file says whether it can be there.
/// A section that names a source form cannot be `Core` or `Neither`, and one
/// that reads a core row cannot be `Source`, `Twice` or `Neither`. What no
/// count can decide is `Twice` against `Source` — whether the core HAS a row
/// that says the same thing — and that is the reader's, exactly as `Kind` is.
#[test]
fn what_the_emitter_reads_is_what_the_rfc_records() {
    let lines = emitter();
    let secs = sections();
    let mut by_class = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        let (f, r) = (forms(&lines, a, b), rows(&lines, a, b));
        let at = secs[i].at;
        match secs[i].reads {
            Reads::Neither => assert!(
                f == 0 && r == 0,
                "`{at}` is filed as reading neither and names {f} source forms and {r} core rows"
            ),
            Reads::Core => assert!(
                f == 0 && r > 0,
                "`{at}` is filed as reading the core and names {f} source forms and {r} core rows"
            ),
            Reads::Twice | Reads::Source => assert!(
                f > 0 && r == 0,
                "`{at}` is filed as reading the source and names {f} source forms and {r} core rows"
            ),
            Reads::Both => assert!(
                f > 0 && r > 0,
                "`{at}` is filed as reading both and names {f} source forms and {r} core rows"
            ),
        }
        let e = by_class
            .entry(secs[i].reads as usize)
            .or_insert((0usize, 0usize, 0usize, 0usize));
        e.0 += 1;
        e.1 += b - a + 1;
        e.2 += f;
        e.3 += r;
    }
    let got: Vec<(&'static str, usize, usize, usize, usize)> = [
        Reads::Neither,
        Reads::Core,
        Reads::Twice,
        Reads::Source,
        Reads::Both,
    ]
    .iter()
    .map(|c| {
        let (n, l, f, r) = by_class
            .get(&(*c as usize))
            .copied()
            .unwrap_or((0, 0, 0, 0));
        (c.label(), n, l, f, r)
    })
    .collect();
    let want = vec![
        ("neither", 47, 6251, 0, 0),
        ("the core's rows", 4, 1777, 0, 20),
        ("the source, and the core says it too", 1, 81, 1, 0),
        ("the source, and the core has no row", 9, 1847, 50, 0),
        ("both, for two questions", 14, 6456, 93, 49),
    ];
    assert_eq!(got, want, "what the emitter reads has moved");
    assert_eq!(
        got.iter().map(|(_, n, ..)| n).sum::<usize>(),
        secs.len(),
        "the classes do not add up to the sections"
    );
    assert_eq!(
        got.iter().map(|(_, _, l, ..)| l).sum::<usize>(),
        lines.len(),
        "the classes do not add up to the file"
    );
}

/// The table for RFC-0125 §3 M3, printed from the sections above:
/// `cargo test -p vyrn-cli --test emitter_census -- --ignored --nocapture
/// the_emitter_census_as_a_table`.
#[test]
#[ignore]
fn the_emitter_census_as_a_table() {
    let lines = emitter();
    let secs = sections();
    println!("| section | lines | wasm | forms | rows | kind | reads | what it is |");
    println!("|---|---|---|---|---|---|---|---|");
    for (i, a, b) in spans(&lines) {
        let name = secs[i]
            .at
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(" {")
            .trim_end_matches('(')
            .to_string();
        println!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
            name,
            b - a + 1,
            instructions(&lines, a, b),
            forms(&lines, a, b),
            rows(&lines, a, b),
            secs[i].kind.label(),
            secs[i].reads.label(),
            secs[i].what
        );
    }
}
