//! Textual LLVM IR backend for the Vyrn v0 subset.
//!
//! This emits LLVM IR as a string — no LLVM libraries required to *produce* it.
//! Feed the output to a `clang`/`llc` (LLVM 15+, opaque pointers) to get a
//! native object/executable:
//!
//! ```text
//! vyrn emit-ir prog.vyrn > prog.ll
//! clang prog.ll -o prog
//! ```
//!
//! Local variables use `alloca`/`load`/`store` (LLVM's `mem2reg` promotes them
//! to SSA registers), which keeps the emitter simple. `&&`/`||` short-circuit
//! via branches + `phi`, matching the interpreter in [`vyrn_frontend::interp`].

pub mod direct;
pub mod layout;
pub mod toolchain;
pub mod wasm;

use std::collections::HashMap;

use vyrn_frontend::ast::*;
/// RFC-0101 M4's exit vocabulary, shared with `vyrn-lower` and the other two
/// engines so the placement and the walks are compared without a translation.
use vyrn_frontend::types::solve_param;

/// One arm of a switch, borrowed (RFC-0125 §3 M5, the one-emission slice).
///
/// Both emitters lower ONE switch, and an `if let` reaches it as the two-arm
/// switch the core says it is ([`vyrn_lower::core::St::Switch`]) — its pattern
/// arm, and its `else` under [`Pattern::Other`], the arm a user cannot spell.
/// The halves are borrowed rather than cloned because an expression's ADDRESS
/// is the key every side table this backend reads is written under, and a
/// clone has a different one.
pub(crate) struct ArmRef<'a> {
    pub pattern: &'a Pattern,
    pub body: BodyRef<'a>,
}

/// [`ArmBody`], borrowed.
pub(crate) enum BodyRef<'a> {
    Expr(&'a Expr),
    Block(&'a Block),
}

impl<'a> ArmRef<'a> {
    /// The arms a `match` wrote, as the switch reads them.
    pub(crate) fn of(arms: &'a [MatchArm]) -> Vec<ArmRef<'a>> {
        arms.iter()
            .map(|a| ArmRef {
                pattern: &a.pattern,
                body: match &a.body {
                    ArmBody::Expr(e) => BodyRef::Expr(e),
                    ArmBody::Block(b) => BodyRef::Block(b),
                },
            })
            .collect()
    }

    /// A block arm yields nothing, which is what makes a switch a statement.
    pub(crate) fn is_block(&self) -> bool {
        matches!(self.body, BodyRef::Block(_))
    }
}

/// The `if let` shape, keyed on the switch and not on the source form: two
/// arms, one of which names a tag and the other of which is the default.
///
/// A switch of that shape is a two-way branch on a compare — the `br i1` on an
/// `icmp` this backend used to write a second time for `Stmt::IfLet`, and the
/// `if`/`else` the direct backend used to. Every other shape takes the switch.
/// `tags` is one entry per arm: the tag it tests, or `None` for the default.
pub(crate) fn two_way(tags: &[Option<usize>]) -> Option<(usize, usize)> {
    match tags {
        [Some(t), None] => Some((0, *t)),
        [None, Some(t)] => Some((1, *t)),
        _ => None,
    }
}

/// The RFC-0014 I/O wording, and the two readers a backend needs — the list
/// itself is [`vyrn_frontend::trap::IO`], below all three engines (RFC-0101 M5).
/// It was here, which meant the interpreter could not read it and re-spelled all
/// eight at thirteen sites.
pub use vyrn_frontend::trap::{io as io_message, io_parts as io_message_parts, IO as IO_MESSAGES};

/// The host-boundary externs of RFC-0043 (time / randomness), which lower to a
/// real shim symbol at each use site rather than to a host import. The table
/// moved to [`vyrn_frontend::trap::HOST_EXTERNS`] when RFC-0103's floor needed
/// to read it: the frontend must be able to tell a host IMPORT from a shim call,
/// and a second copy of the three names is the drift that file exists to end.
pub use vyrn_frontend::trap::host_boundary_extern;

/// The extern (JS-boundary) ABI value type for one primitive, per the RFC-0012
/// table: `Int64`/`i64`, sized ints ≤32-bit widen to `i32`, `Bool` is `i32`,
/// floats stay `double`/`float`, `String` returns as a bare `ptr`, `Unit` is a
/// missing result. `String` *parameters* are handled separately (they cross as
/// a `(ptr, len)` pair). The checker guarantees no other type reaches here.
///
/// Shared with the direct wasm backend, which maps the answer through
/// [`wasm::abi`] rather than keeping a second table: an ABI written down twice is
/// a misread argument on one backend, not a link error.
pub(crate) fn extern_abi_ll(ty: &Type) -> &'static str {
    match ty {
        Type::Int => "i64",
        Type::IntN { bits: 64, .. } => "i64",
        Type::IntN { .. } => "i32",
        Type::Float => "double",
        Type::Float32 => "float",
        Type::Bool => "i32",
        Type::Str => "ptr",
        Type::Unit => "void",
        // Unreachable: the checker restricts the extern signature domain.
        _ => "i64",
    }
}

thread_local! {
    /// RFC-0076 M2: whether this module is being emitted to run as a GENERATOR
    /// under the wasm engine, where `listDir` is a host import backed by the
    /// loader's resolver. An ordinary build must keep rejecting it (the language
    /// gives `listDir` no runtime lowering), so the flag gates exactly that one
    /// branch.
    ///
    /// A thread-local rather than a `Gen` field because `Gen` is constructed at
    /// nine sites inside [`emit_with`] and not one of them has an opinion.
    static GEN_HOST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is emitting a generator-host module.
///
/// RFC-0076 M7 gave the flag a second reader: the DIRECT wasm backend emits the
/// same surface without clang, and `llt_of`'s `Code` arm is shared between them,
/// so the flag has to be the one both ask rather than a parameter one of them
/// threads.
pub(crate) fn gen_host() -> bool {
    GEN_HOST.with(|g| g.get())
}

pub(crate) fn set_gen_host(on: bool) {
    GEN_HOST.with(|g| g.set(on));
    // The checker asks the same question about the same program: a generator
    // host's bodies are generation code even though `is_gen` was cleared to make
    // them compilable. One flag, set in one place (RFC-0125 §3 M5, the ninth
    // slice).
    vyrn_frontend::checker::set_gen_host(on);
}

/// RFC-0101 M1: what the compiled backend decides an expression's type is.
///
/// The emitter derives the type of every expression itself — `peek` and its
/// satellites (RFC-0101 §1.2). Nothing outside it can see those answers, so
/// nothing can check them against the checker's. This makes them visible, off by
/// default, and adds no decision: every hook records what the emitter was about
/// to return anyway.
///
/// It had a second column until RFC-0125 §3 M4's fourth slice: the text-IR
/// backend's threaded `(String, Type)`, recorded as `Site::Native`. The route
/// went and the column went with it.
///
/// Off, the cost is one thread-local `Cell` read per expression. On, the sink
/// grows one row per typed expression per instantiation, which is why it is a
/// gate's tool and not a compiler's.
pub mod observe {
    use vyrn_frontend::ast::Type;

    /// Which engine, and which of its derivations, produced a row.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Site {
        /// `Fn_::expr` — the wasm emitter's emitting walk.
        Wasm,
        /// `Fn_::peek` — the direct wasm backend's second expression typer.
        Peek,
    }

    /// One backend answer: this node, under this instantiation, has this type.
    #[derive(Debug, Clone)]
    pub struct Row {
        pub site: Site,
        pub kind: &'static str,
        /// The tree the answer was given inside, when it is one an engine
        /// CLONED rather than one the program holds (RFC-0101 M6's second
        /// phase). `""` for an ordinary node.
        ///
        /// Two clones are left in the compiler and this is what sizes each:
        ///
        /// - `"lambda"` — `Fn_::lift_lambda` copies a lambda's body, so every
        ///   node in it, and in every projection expansion built while walking
        ///   it, is off-program by construction. The textual backend never sets
        ///   this: it lifts by walking the literal's OWN nodes.
        /// - `"pred"` — a `where` predicate lives on a `TypeDecl`, both
        ///   backends read theirs out of a cloned `types::decl_map`, and each
        ///   validation site then clones the predicate again to get past the
        ///   borrow checker. Two levels of copy, at every value boundary a
        ///   refined type crosses.
        ///
        /// M6's first phase named the first class and could not size it, and
        /// did not know the second existed. A bucket priced by size is the
        /// mistake its own ledger catches, so both are counted rather than
        /// argued.
        pub ctx: &'static str,
        /// The AST node's address — the identity `own` and `movecheck` use.
        pub node: usize,
        /// The instantiation the emitter was inside, sorted by parameter name.
        pub subst: Vec<(String, Type)>,
        pub ty: Type,
    }

    /// One body this backend decided to emit: a function and the type arguments
    /// it was emitted at.
    ///
    /// RFC-0101 M2's shadow: the backend runs its own worklist, and nothing
    /// outside it has ever been able to see the list, so "`vyrn-lower` builds the
    /// instances the backend builds" has been a claim with no gate under it. This
    /// makes the list readable and adds no decision — every hook records a body
    /// the driver was about to lower anyway.
    ///
    /// A lifted lambda is deliberately NOT here. It is not a function of the
    /// program: it has no name, its body is a clone the backend synthesized, and
    /// its identity is a node address the lowering cannot key against. What that
    /// costs is written into RFC-0101 §3 M2.
    #[derive(Debug, Clone)]
    pub struct Inst {
        pub site: Site,
        pub name: String,
        pub args: Vec<Type>,
    }

    thread_local! {
        static ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static CTX: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
        static ROWS: std::cell::RefCell<Vec<Row>> = const { std::cell::RefCell::new(Vec::new()) };
        static INSTS: std::cell::RefCell<Vec<Inst>> = const { std::cell::RefCell::new(Vec::new()) };
        static CROSSINGS: std::cell::RefCell<Vec<Crossing>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Start recording on this thread, discarding anything already collected.
    pub fn start() {
        ROWS.with(|r| r.borrow_mut().clear());
        INSTS.with(|r| r.borrow_mut().clear());
        CROSSINGS.with(|r| r.borrow_mut().clear());
        ON.with(|o| o.set(true));
    }

    /// Stop recording and take what was collected.
    pub fn take() -> Vec<Row> {
        ON.with(|o| o.set(false));
        ROWS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// The instantiations recorded since [`start`]. Read after [`take`], which is
    /// what stops the recording.
    pub fn take_insts() -> Vec<Inst> {
        INSTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// One boundary crossing an engine actually made: the pair, and the rung it
    /// took (RFC-0101 §1.5's shadow).
    ///
    /// The PAIR is the identity, not a node: the two ladders reach `coerce` from
    /// different call sites, so there is no node they both stand at, and §1.5's
    /// whole claim is about the pair.
    #[derive(Debug, Clone)]
    pub struct Crossing {
        pub site: Site,
        pub from: Type,
        pub to: Type,
        pub rung: crate::Rung,
    }

    /// Record the rung an engine took. Every `return` path of a `coerce` calls
    /// this exactly once, so a rung that stops being reachable stops being
    /// recorded — which is what the corpus gate's floor is for.
    pub(crate) fn note_rung(site: Site, from: &Type, to: &Type, rung: crate::Rung) {
        if !on() {
            return;
        }
        CROSSINGS.with(|r| {
            r.borrow_mut().push(Crossing {
                site,
                from: from.clone(),
                to: to.clone(),
                rung,
            })
        });
    }

    /// The crossings recorded since [`start`]. Read after [`take`], like
    /// [`take_insts`].
    pub fn take_crossings() -> Vec<Crossing> {
        CROSSINGS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    pub(crate) fn note_inst(site: Site, name: &str, args: &[Type]) {
        if !on() {
            return;
        }
        INSTS.with(|r| {
            r.borrow_mut().push(Inst {
                site,
                name: name.to_string(),
                args: args.to_vec(),
            })
        });
    }

    pub(crate) fn on() -> bool {
        ON.with(|o| o.get())
    }

    /// Mark the rows recorded from here on as being inside a cloned tree, and
    /// give back what the mark was so the caller can put it back.
    pub(crate) fn set_ctx(v: &'static str) -> &'static str {
        CTX.with(|f| f.replace(v))
    }

    /// The expression kind a row is reported under — and for a variable, the
    /// NAME too.
    ///
    /// RFC-0101 M5 measured the residue by hand-editing this function, because
    /// `var` as one bucket said "the release receiver is the bulk" and the name
    /// said it is a tenth. A measurement that needs an edit to repeat is a
    /// measurement the next milestone will not repeat, so the name is here
    /// permanently. It is interned rather than owned: [`Row`] holds a
    /// `&'static str`, the pool is bounded by the distinct variable names in one
    /// corpus, and nothing calls this unless [`on`] — the gate — is recording.
    pub fn kind_of(e: &vyrn_frontend::ast::Expr) -> &'static str {
        use vyrn_frontend::ast::Expr as E;
        match e {
            E::Int(_) => "int",
            E::Byte(_) => "byte",
            E::Float(_) => "float",
            E::Bool(_) => "bool",
            E::Str(_) => "str",
            E::Var { name, .. } => intern(format!("var[{name}]")),
            E::Unary { .. } => "unary",
            E::Binary { .. } => "binary",
            E::Call { name, .. } => {
                if name.starts_with('@') {
                    "call@"
                } else {
                    "call"
                }
            }
            E::Match { .. } => "match",
            E::IfExpr { .. } => "ifexpr",
            E::Try { .. } => "try",
            E::StructLit { .. } => "record",
            E::Field { .. } => "field",
            E::TryConstruct { .. } => "tryconstruct",
            E::ArrayLit { .. } => "array",
            E::MapLit { .. } => "map",
            E::Spawn { .. } => "spawn",
            E::Lambda { .. } => "lambda",
            E::Consume { .. } => "consume",
        }
    }

    /// One `&'static str` per distinct string, so a `kind` can carry a name and
    /// a [`Row`] can still be `Copy`-cheap.
    fn intern(s: String) -> &'static str {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        let mut pool = POOL
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap();
        if let Some(hit) = pool.get(s.as_str()) {
            return hit;
        }
        let leaked: &'static str = Box::leak(s.into_boxed_str());
        pool.insert(leaked);
        leaked
    }

    pub(crate) fn record(
        site: Site,
        kind: &'static str,
        node: usize,
        subst: &std::collections::HashMap<String, Type>,
        ty: &Type,
    ) {
        let mut subst: Vec<(String, Type)> =
            subst.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        subst.sort_by(|a, b| a.0.cmp(&b.0));
        ROWS.with(|r| {
            r.borrow_mut().push(Row {
                site,
                kind,
                ctx: CTX.with(|f| f.get()),
                node,
                subst,
                ty: ty.clone(),
            })
        });
    }
}

/// Every `vyrn_gen` import a generator module makes: a signature in LLVM's spelling
/// and the name it imports under, which [`wasm::declare_sig`] turns into the wasm
/// one through [`wasm::abi`] — so `i1`, `i8` and `ptr` are widened in exactly one
/// place and no signature on this boundary is written twice.
///
/// LLVM's spelling and not wasm's because these WERE `declare` lines: RFC-0076 M3a
/// emitted them into the IR with `wasm-import-module` attributes, the way an
/// RFC-0012 `extern` is emitted, and M7's direct backend imports the same set
/// without a textual emitter in the way. Kept in this form because `wasm::boundary`
/// reads the emitter's own `declare` lines the same way, and one parser over one
/// spelling is what stops the two lists drifting.
///
/// The host side of every one of them is the interpreter's own code — the RFC-0054
/// piece arena, `render_code`, the splice table, the real lexer, the real linker —
/// which is what keeps the escaping, the identifier validation and the
/// shortest-roundtrip float formatting byte-identical by construction rather than
/// by testing.
pub(crate) const CODE_IMPORTS: &[(&str, &str)] = &[
    ("i64 @__vyrn_code_text(ptr)", "text"),
    ("i64 @__vyrn_code_splice(i32, i64, ptr, i64)", "splice"),
    ("i64 @__vyrn_code_raw_at(ptr, ptr, i64, i64)", "rawAt"),
    ("i64 @__vyrn_code_concat(i64, i64)", "concat"),
    ("i64 @__vyrn_code_render(i64)", "render"),
    // The M2 stash reader: `render` answers with a length and the guest
    // allocates, because the host must not allocate inside guest memory.
    ("void @__vyrn_gen_fetch(ptr)", "fetch"),
    // RFC-0076 M3b — structured host results. `reflect` asks the host for a
    // value of a known named type (`lex`, `moduleInterface`, `contractOf`) and
    // leaves it as a flat atom stream; `nextInt`/`nextStr` pull the atoms back in
    // the order the decoder walks the type. `nextStr` answers with a length and
    // the guest allocates, exactly as `render` does.
    ("void @__vyrn_gen_reflect(i64, ptr)", "reflect"),
    ("i64 @__vyrn_gen_next_int()", "nextInt"),
    ("i64 @__vyrn_gen_next_str()", "nextStr"),
    // RFC-0076 M2's mediated read, which `readFile`, `readFileBytes` and `listDir`
    // are all served out of. It used to be declared by the C shim rather than the
    // IR, because the shim was what called it; M7 has no shim, so it joins the
    // list it always belonged to.
    ("i64 @__vyrn_gen_read(ptr, i32)", "read"),
];

pub const GEN_MODE_READ: i32 = 0;
pub const GEN_MODE_READ_BYTES: i32 = 1;
pub const GEN_MODE_LIST: i32 = 2;
// 3 is the genwasm host's `moduleInterface`, which never reaches this emitter.
/// `listDirKinds` (RFC-0119): the same `\n`-joined listing as `GEN_MODE_LIST`,
/// with a `/` appended to each directory entry's name. An entry name cannot
/// contain either byte, so the encoding stays invertible.
pub const GEN_MODE_LIST_KINDS: i32 = 4;

/// `@__vyrn_gen_reflect`'s kinds — which builtin the host is answering
/// (RFC-0076 M3b). The argument is the module path, the contract NAME, or the
/// source to lex.
pub const REFLECT_MODULE_INTERFACE: i64 = 0;
pub const REFLECT_CONTRACT_OF: i64 = 1;
pub const REFLECT_LEX: i64 = 2;

/// The generator-host entry points the ENGINE synthesizes and this emitter calls
/// (RFC-0076 M3b).
///
/// Each is an ordinary Vyrn function the engine appends to the wrapper program:
/// it asks the host to compute the value, then decodes it by walking the static
/// type. Codegen only redirects the builtin's call site to it, so the decode is
/// compiled by the ordinary emitter rather than hand-written as IR — the arrays,
/// the records and the Options are the ones every other Vyrn program gets.
pub const GEN_ENTRY_MODULE_INTERFACE: &str = "__vyrnGenModuleInterface";
pub const GEN_ENTRY_LEX: &str = "__vyrnGenLex";
/// Suffixed with the contract's name: the argument is a declaration, not a value.
pub const GEN_ENTRY_CONTRACT_OF: &str = "__vyrnGenContractOf_";

/// The atom-stream primitives the synthesized decoders are written against.
///
/// Declared by the checker, which types a call to one when this thread is a
/// generator host, and re-exported here because the emitters lower them and
/// `vyrn-genwasm` writes the decoders that call them. One name, one signature,
/// one place (RFC-0125 §3 M5, the ninth slice).
pub use vyrn_frontend::checker::{GEN_NEXT_INT, GEN_NEXT_STR, GEN_REFLECT};

/// `@__vyrn_code_splice`'s value tags — which interpreter `Val` the host is to
/// rebuild from the word it was handed. Exactly the set the splice rule accepts
/// (`gen::gen_code_splice`), no more: the checker has already rejected
/// anything else by the time codegen sees the call. `pub` so the host reads the
/// same numbering it is emitted against, rather than a second copy of it.
pub const TAG_STR: i32 = 0;
pub const TAG_CODE: i32 = 1;
pub const TAG_BOOL: i32 = 2;
pub const TAG_INT: i32 = 3;
pub const TAG_UINT: i32 = 4;
pub const TAG_F64: i32 = 5;
pub const TAG_F32: i32 = 6;

/// Emit a complete LLVM IR module for `program`.
///
/// Native only, since RFC-0077 M5 deleted the wasm path — and since RFC-0076 M7
/// there is no `emit_gen_host` beside it either: the generation engine reaches wasm
/// through the direct backend, which needs no C toolchain, so the generator-host
/// variant of THIS emitter had no caller left. The `Code` handle imports, the
/// reflection redirects and `listDir`'s lowering went with it. A code quote outside
/// generation is still the checker's error and this emitter still has no lowering
/// for one, which is what it was before RFC-0076 M3a.
// The two instantiation bounds moved to `vyrn_frontend::types` in RFC-0101 M1,
// and are re-exported here so every existing reader spells them the same way.
// They moved because a bound on monomorphization is not a property of a backend:
// `vyrn-lower` runs the same worklist and sits BELOW this crate, so a copy here
// would be a second number, which is the shape of defect this RFC is about.
pub use vyrn_frontend::types::{MONO_DEPTH_LIMIT, MONO_SIZE_LIMIT};

/// The phrase every instantiation-limit refusal contains. `vyrn check` promotes
/// exactly this one codegen error to a check failure, so it has to recognise it,
/// and one needle both sides read cannot drift.
pub const MONO_LIMIT_NEEDLE: &str = "past the instantiation limit";

/// The phrase every frame-size refusal contains, for the same reason
/// [`MONO_LIMIT_NEEDLE`] exists: a test that pins a limit by quoting its
/// sentence pins the sentence, not the limit.
pub const FRAME_LIMIT_NEEDLE: &str = "past the frame limit";

/// The phrase every statics-size refusal contains, for the reason the two above
/// exist. This one was an `assert!` in [`wasm::Module::finish`] — a limit stated
/// as a Rust panic, in a function whose caller already returns `Result`, so a
/// program with more literals than the module can hold killed
/// `vyrn build --target wasm` with a backtrace and no source at all.
pub const STATICS_LIMIT_NEEDLE: &str = "past the statics limit";

/// Refuse an instantiation whose type arguments pass [`MONO_DEPTH_LIMIT`] or
/// [`MONO_SIZE_LIMIT`].
///
/// The message names the TYPE rather than the chain of calls that built it. The
/// type IS the chain, written down — `P<P<P<..>>>` is one `P` per instantiation —
/// and it is also the thing the author has to change.
pub fn check_inst_depth<'a>(
    name: &str,
    args: impl Iterator<Item = &'a Type>,
    line: usize,
    types: &HashMap<String, vyrn_frontend::ast::TypeDecl>,
) -> Result<(), String> {
    for a in args {
        let d = vyrn_frontend::types::type_depth(a);
        let too_deep = d > MONO_DEPTH_LIMIT;
        let size = vyrn_frontend::types::expanded_size(a, types, MONO_SIZE_LIMIT);
        if !too_deep && size.is_some() {
            continue;
        }
        let what = if too_deep {
            format!("nests {d} levels deep, past the limit of {MONO_DEPTH_LIMIT}")
        } else {
            format!("has more than {MONO_SIZE_LIMIT} parts once its records are written out")
        };
        let mut shown = a.to_string();
        if shown.len() > 80 {
            // The display string may hold multi-byte characters (the lexer
            // accepts Unicode identifiers): back up to a char boundary before
            // cutting, or `truncate` panics mid-character.
            let mut cut = 80;
            while !shown.is_char_boundary(cut) {
                cut -= 1;
            }
            shown.truncate(cut);
            shown.push_str("...");
        }
        return Err(format!(
            "instantiating `{name}` needs a type {MONO_LIMIT_NEEDLE}: it {what}\n  \
             note: `{name}` is declared on line {line}, and the type is `{shown}`\n  \
             note: a generic function that calls itself with a BIGGER type has no \
             finite set of instances — the recursion has to shrink the type, not \
             only the count"
        ));
    }
    Ok(())
}

/// Run the monomorphization the backends run, and report ONLY its depth refusal.
///
/// `vyrn check` reads this. Every other codegen error stays where it is —
/// `check` has never claimed to predict them, and promoting them all here would
/// change its contract by more than the one defect this closes (audit A5.2:
/// `check` said `ok` about a program no backend could finish).
///
/// **RFC-0101 M2b: this used to call [`emit`].** It ran the entire native
/// lowering, built a complete LLVM module as a `String`, matched its error
/// against one needle and threw the module away — because monomorphization only
/// existed inside a backend and the front end had no other way to ask how deep
/// it goes (§1.2). It does now: `vyrn-lower` runs the worklist, from the same
/// two constants, and the refusal is worded by the same [`check_inst_depth`] the
/// emitter calls. `vyrn-cli/tests/lowered.rs` is what makes that prediction sound
/// — it asserts over the corpus that every instantiation the emitter builds is
/// one the lowering's worklist has.
pub fn check_instantiations(program: &Program) -> Result<(), String> {
    let types = vyrn_frontend::types::decl_map(program);
    for u in vyrn_lower::lower(program).unresolved {
        if u.why == vyrn_lower::Why::PastTheLimit {
            check_inst_depth(&u.callee, u.args.iter(), u.line, &types)?;
        }
    }
    Ok(())
}

/// Deep-normalize a stored-fn signature (RFC-0037) so structurally identical
/// spellings — a `type Transform = fn(Int64) -> Int64` alias, a validated scalar,
/// transformer sugar — register and dispatch as ONE synthesized enum.
/// `Task` interiors are left resolved: they cannot hold fn values, and
/// recursing would cycle.
///
/// Shared with the direct wasm backend, because it decides which constructions a
/// dispatcher covers. Two backends grouping differently would give one of them a
/// dispatcher missing a variant — a defensive trap where a call belongs, reached
/// only by the spelling nobody wrote a test for.
pub(crate) fn normalize_fn_sig(t: &Type, types: &HashMap<String, TypeDecl>) -> Type {
    let norm = |x: &Type| normalize_fn_sig(x, types);
    match vyrn_frontend::types::resolve(t, types) {
        Type::Fn(ps, r) => Type::Fn(ps.iter().map(norm).collect(), Box::new(norm(&r))),
        Type::Array(i) => Type::Array(Box::new(norm(&i))),
        Type::ArrayN(i, n) => Type::ArrayN(Box::new(norm(&i)), n),
        Type::Map(k, v) => Type::Map(Box::new(norm(&k)), Box::new(norm(&v))),
        // Every sum, one arm — since RFC-0126 §8.11's M4b the two built-in
        // spellings resolve to variant lists, and a normalization that stopped
        // at the list registered `Option<T>` under an UNnormalized payload while
        // the dispatcher looked one up under a normalized one.
        Type::Enum(vs) => Type::Enum(
            vs.iter()
                .map(|v| EnumVariant {
                    name: v.name.clone(),
                    payload: v.payload.iter().map(norm).collect(),
                })
                .collect(),
        ),
        Type::Record(fs) => Type::Record(
            fs.iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: norm(&f.ty),
                })
                .collect(),
        ),
        other => other,
    }
}

/// The captured (free) local variables of a lambda body (RFC-0023), in
/// first-seen order: names read in the body that are neither the lambda's own
/// parameters/locals nor module state nor functions — i.e. bindings that live in
/// the enclosing local scope, which is what `is_local` answers.
///
/// The descent and the scope stack are `ast::body_scope_descent!`'s since
/// RFC-0125 §3 M6. The scope this pass kept was the walk's, arm for arm; what
/// is its own is the entry — the lambda's own parameters are in `locals` before
/// the body is walked — and one line at a site: a nested lambda literal is not
/// descended, because RFC-0023's nesting lock means there is never one.
pub(crate) fn lambda_captures(
    body: &LambdaBody,
    locals: std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    /// The collector's line at each site: a name read that no local shadows and
    /// `is_local` answers for is a capture, recorded once, in first-seen order.
    struct CapturesOf<'a> {
        out: Vec<String>,
        seen: std::collections::HashSet<String>,
        is_local: &'a dyn Fn(&str) -> bool,
    }

    impl CapturesOf<'_> {
        fn take(&mut self, n: &str, locals: &std::collections::HashSet<String>) {
            if locals.contains(n) || self.seen.contains(n) {
                return;
            }
            // Only an enclosing LOCAL slot is a capture — module state and
            // functions/variants are reached directly by the lifted function.
            if (self.is_local)(n) {
                self.seen.insert(n.to_string());
                self.out.push(n.to_string());
            }
        }
    }

    impl BodyVisit<'_> for CapturesOf<'_> {
        fn expr(&mut self, e: &Expr, locals: &std::collections::HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } => self.take(name, locals),
                // A CALL captures its callee when the callee names an enclosing
                // local: `|req, ps| run(req)` over a `fn`-typed `run` calls a
                // value, not a symbol, and leaving it out of the capture list
                // lowered it as a direct call to `@vyrn_run` — a name no module
                // defines (the interpreter, which resolves through the
                // environment, ran the same program fine). Nothing else changes:
                // `is_local` is false for a top-level function, so an ordinary
                // call still reaches its symbol with no capture at all.
                Expr::Call { name, .. } => self.take(name, locals),
                // RFC-0023's nesting lock: a lambda body may not hold another
                // lambda literal, so there is no inner body to walk.
                Expr::Lambda { .. } => return false,
                _ => {}
            }
            true
        }
    }

    let mut v = CapturesOf {
        out: Vec::new(),
        seen: std::collections::HashSet::new(),
        is_local,
    };
    let mut locals = locals;
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
    v.out
}

/// LLVM byte-string escaping: printable ASCII as-is, everything else `\NN`,
/// plus a trailing NUL. Returns (escaped, total byte length).
/// The wording both compiling backends print for `serveStream` (RFC-0074 M3a).
/// One constant so the two engines cannot drift, which is the rule every trap
/// message in this project follows.
pub(crate) fn serve_stream_trap() -> String {
    vyrn_frontend::trap::line(vyrn_frontend::trap::SERVE_STREAM)
}

/// Björn Höhrmann's UTF-8 validation DFA table: 256 byte-class entries followed
/// by a 108-entry (9 states × 12 classes) transition table. State 0 is ACCEPT,
/// 12 is REJECT. Used by `@__vyrn_utf8valid` so the native decoders reject exactly
/// what Rust's `String::from_utf8` rejects (overlong forms, surrogates, > U+10FFFF).
///
/// Shared with the direct wasm backend (RFC-0077 M2g), which puts the same bytes
/// in a data segment and walks them with the same two loads. A second table would
/// have been a second answer to "is this valid UTF-8", free to drift by a byte.
///
/// The table below is his, byte for byte, and it is the one piece of third-party
/// code in this repository. His terms are MIT and they require the notice to
/// travel with every copy, including the binaries this emits it into:
///
/// ```text
/// Copyright (c) 2008-2009 Bjoern Hoehrmann <bjoern@hoehrmann.de>
/// See http://bjoern.hoehrmann.de/utf-8/decoder/dfa/ for details.
/// ```
///
/// The full permission notice is in `THIRD-PARTY-NOTICES.md` at the repository
/// root, which the release archive ships.
pub(crate) fn utf8d_table() -> Vec<u8> {
    let mut t = vec![0u8; 256];
    for b in 0x80..=0x8F {
        t[b] = 1;
    }
    for b in 0x90..=0x9F {
        t[b] = 9;
    }
    for b in 0xA0..=0xBF {
        t[b] = 7;
    }
    t[0xC0] = 8;
    t[0xC1] = 8;
    for b in 0xC2..=0xDF {
        t[b] = 2;
    }
    t[0xE0] = 10;
    for b in 0xE1..=0xEC {
        t[b] = 3;
    }
    t[0xED] = 4;
    t[0xEE] = 3;
    t[0xEF] = 3;
    t[0xF0] = 11;
    for b in 0xF1..=0xF3 {
        t[b] = 6;
    }
    t[0xF4] = 5;
    for b in 0xF5..=0xFF {
        t[b] = 8;
    }
    #[rustfmt::skip]
    let trans: [u8; 108] = [
        0,12,24,36,60,96,84,12,12,12,48,72,
        12,12,12,12,12,12,12,12,12,12,12,12,
        12, 0,12,12,12,12,12, 0,12, 0,12,12,
        12,24,12,12,12,12,12,24,12,24,12,12,
        12,12,12,12,12,12,12,24,12,12,12,12,
        12,24,12,12,12,12,12,12,12,24,12,12,
        12,12,12,12,12,12,12,36,12,36,12,12,
        12,36,12,12,12,12,12,36,12,36,12,12,
        12,36,12,12,12,12,12,12,12,12,12,12,
    ];
    t.extend_from_slice(&trans);
    t
}

/// If `value` is `name + e1 + e2 + …` — a `+` chain whose left spine bottoms
/// out in a bare `name` — the appended operands in written order. The chain
/// matters: `out + a + ", "` parses as `Add(Add(Var(out), a), ", ")`, so the
/// accumulator sits at the far end of the spine, not under the top `+`.
fn self_append_spine<'e>(name: &str, value: &'e Expr) -> Option<Vec<&'e Expr>> {
    let mut parts: Vec<&Expr> = Vec::new();
    let mut cur = value;
    while let Expr::Binary {
        op: BinOp::Add,
        lhs,
        rhs,
        ..
    } = cur
    {
        parts.push(rhs);
        cur = lhs;
    }
    match cur {
        Expr::Var { name: n, .. } if n == name && !parts.is_empty() => {
            parts.reverse();
            Some(parts)
        }
        _ => None,
    }
}

/// The local `String` accumulators of one function body that `s = s + …` may
/// grow IN PLACE (see `Gen::emit_str_append`).
///
/// In-place growth reallocates, so every other holder of the old pointer is
/// invalidated — `let copy = out` before an append must keep reading "a". The
/// interpreter is safe here because `Rc::make_mut` clones a shared buffer;
/// native code has no refcount, so eligibility is decided statically and the
/// rule is a WHITELIST: a name qualifies only if every occurrence of it in the
/// function is a use that provably cannot retain the pointer — the root of a
/// self-append, a `.field` read (a String's fields are byte/char counts),
/// an operand of the interpolation desugar (which copies), or a tail
/// `return`. Anything else — another `let`, any user call, a record
/// field, an array element, a lambda body, an unrecognized builtin — bans the
/// name. An unknown callee is therefore ineligible by construction, so a new
/// retaining builtin cannot silently make this unsound.
fn append_candidates(body: &Block) -> std::collections::HashSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    scan_append_block(body, &mut targets, &mut banned, false);
    targets.retain(|n| !banned.contains(n));
    targets
}

/// The **module-state** `String` accumulators of a whole program (census P1).
///
/// The same whitelist, read over every body instead of one, because a global is
/// reachable from all of them: a name qualifies when some body grows it with
/// `g = g + …` and NO body puts a pointer to it anywhere that could outlive the
/// grow. `let t = g` is one of the things that bans a name, which is exactly the
/// aliasing guard a global needs and a local already had.
///
/// P1 measured what not having this costs: 4.92 s and 12.2 GB to build a 160 KB
/// string, against 0.095 s for the identical local. The global did not qualify
/// for one reason — the whitelist read one body — and every server that
/// accumulates a response body is a module-state accumulator.
///
/// A body that binds the name LOCALLY votes on neither side, because inside it
/// the name is not the global. Without that filter one `let out` among the
/// hundreds of linked `std/` functions disqualifies a module-state `out`, and the
/// first measurement of this pass hit exactly that.
/// The result is a `BTreeSet` and not a `HashSet` because one caller ITERATES
/// it: the direct backend reserves an ownership word per accumulator, and a
/// reservation is an address baked into every `i32.const` that reads or writes
/// it — and it shifts every later reservation, so the whole static map moves.
/// `RandomState` is seeded per process, so two accumulators were a coin flip and
/// three built six different modules from one source. Sorted here rather than at
/// that loop, because the next caller to iterate it would have to know.
pub(crate) fn global_append_candidates(program: &Program) -> std::collections::BTreeSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    let mut one = |body: &Block, params: &[Param]| {
        let (mut t, mut ban) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        scan_append_block(body, &mut t, &mut ban, false);
        let mut shadowed: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        bound_names(body, &mut shadowed);
        targets.extend(t.into_iter().filter(|n| !shadowed.contains(n)));
        banned.extend(ban.into_iter().filter(|n| !shadowed.contains(n)));
    };
    for f in &program.functions {
        one(&f.body, &f.params);
    }
    for t in &program.tests {
        one(&t.body, &[]);
    }
    for bn in &program.benches {
        one(&bn.body, &[]);
    }
    // A global's own initializer runs once and cannot append, but a name it reads
    // is a name held somewhere this walk should see.
    for g in &program.globals {
        ban_append_expr(&g.init, &mut banned, false);
    }
    targets.retain(|n| !banned.contains(n));
    targets.retain(|n| program.globals.iter().any(|g| &g.name == n));
    targets.into_iter().collect()
}

// The descent over a body is `ast::body_scope_descent!`'s, where the AST is
// declared (RFC-0125 §3 M6). This module's collectors read it.
vyrn_frontend::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

/// The collector's line at each site: a `let`, a loop variable, an `if let` or
/// arm binder, a lambda parameter.
struct BoundNames<'a>(&'a mut std::collections::HashSet<String>);

impl BodyVisit<'_> for BoundNames<'_> {
    // The union of every name bound anywhere, not what is in scope where.
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &Stmt, _: &std::collections::HashSet<String>) {
        match s {
            Stmt::Let { name, .. } => {
                self.0.insert(name.clone());
            }
            Stmt::ForIn { var, .. } => {
                self.0.insert(var.clone());
            }
            Stmt::IfLet { pattern, .. } => self.0.extend(pattern_names(pattern)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        if let Expr::Lambda { params, .. } = e {
            self.0.extend(params.iter().cloned());
        }
        true
    }

    fn arm_pattern(&mut self, p: &Pattern, _: usize, _: &std::collections::HashSet<String>) {
        self.0.extend(pattern_names(p));
    }
}

/// Every name a block binds anywhere inside it — `let`s, loop variables, pattern
/// binders and lambda parameters. Over-collecting is safe here: the only use is
/// to decide that a body is talking about its own name rather than about module
/// state, and an extra name only costs a global the in-place append path.
fn bound_names(b: &Block, out: &mut std::collections::HashSet<String>) {
    let mut locals = std::collections::HashSet::new();
    body_block(b, &mut locals, &mut BoundNames(out));
}

/// The names a refutable pattern binds.
fn pattern_names(p: &Pattern) -> Vec<String> {
    match p {
        Pattern::Success(n) | Pattern::Failure(n) => vec![n.clone()],
        Pattern::Variant(_, ns) => ns.clone(),
        Pattern::Other => Vec::new(),
    }
}

/// Walk a block collecting append targets and banned names. `strict` marks a
/// lambda body: everything inside one is banned outright, because a capture
/// copies the pointer into a value that outlives the append.
fn scan_append_block(
    b: &Block,
    targets: &mut std::collections::HashSet<String>,
    banned: &mut std::collections::HashSet<String>,
    strict: bool,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } => ban_append_expr(value, banned, strict),
            Stmt::Assign { name, value, .. } => match self_append_spine(name, value) {
                Some(parts) if !strict => {
                    targets.insert(name.clone());
                    // The accumulator may not appear on the right as well:
                    // `out = out + out` would read a buffer the realloc moved.
                    for p in parts {
                        ban_append_expr(p, banned, strict);
                    }
                }
                _ => ban_append_expr(value, banned, strict),
            },
            Stmt::SetField { value, .. } | Stmt::Expr(value) => {
                ban_append_expr(value, banned, strict)
            }
            Stmt::IndexSet { index, value, .. } => {
                ban_append_expr(index, banned, strict);
                ban_append_expr(value, banned, strict);
            }
            // Returning the accumulator hands off the buffer at the point the
            // frame dies — nothing can append after it.
            Stmt::Return { value: Some(e), .. } => ban_append_read(e, banned, strict),
            // `drop s` frees the buffer; leave that path on the general lowering.
            Stmt::Drop { name, .. } => {
                banned.insert(name.clone());
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(scrutinee, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::While { cond, body, .. } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::ForIn { iter, body, .. } => {
                ban_append_expr(iter, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::Region { body, .. } => scan_append_block(body, targets, banned, strict),
            Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

/// A position that does not retain its operand: a bare variable there is fine.
fn ban_append_read(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    if strict || !matches!(e, Expr::Var { .. }) {
        ban_append_expr(e, banned, strict);
    }
}

/// Does `op` leave one of its operands holding a String pointer it did not
/// copy? If so, an accumulator there could be left pointing at a buffer a later
/// in-place append `realloc`'d away, and the name must be banned.
///
/// Today the answer is no for all nineteen, so the guard in `ban_append_expr`
/// looks vacuous — it is not, and this function is why it may not be deleted:
/// the match is exhaustive so a new operator cannot be added without deciding,
/// and a String-borrowing operator (a `slice`-like `..`, say) would flip its
/// arm and re-ban its operands with no other change. Getting this wrong is a
/// use-after-free, so the decision is recorded per operator with its lowering:
///
/// - `+` on two Strings — `emit_str_concat`: `malloc(la+lb+1)` then
///   `strcpy`/`strcat`. A fresh buffer, which is exactly why `@concat` (what
///   `"\{out}]"` desugars to) is already whitelisted above. `Code + Code` takes
///   two arena handles, not pointers, and a `Code` name never owns a shadow.
/// - `== != < <= > >=` on two Strings — one `strcmp` and an `icmp`, result `i1`.
/// - `=~` — `@__vyrn_regex_run(ptr s, …)`, result `i1`; the right operand must
///   be a literal pattern, so only the left can even be an accumulator.
/// - `- * / % && || & | ^ << >>` — `binop_type` refuses a `String` operand
///   outright (arithmetic and bitwise need matching numerics, `&&`/`||` need
///   `Bool`), so no String reaches these lowerings at all.
///
/// A `<T: Ord>` operand monomorphized to `String` reaches the same lowerings
/// through the same operators, so the list covers generics too.
fn binop_retains_str(op: BinOp) -> bool {
    match op {
        BinOp::Add
        | BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::LtEq
        | BinOp::Gt
        | BinOp::GtEq
        | BinOp::Match => false,
        BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Rem
        | BinOp::And
        | BinOp::Or
        | BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::Shl
        | BinOp::Shr => false,
    }
}

/// Ban every variable `e` mentions in a position that might retain it. The
/// match is exhaustive on purpose: a new `Expr` variant must be classified
/// rather than silently fall into a permissive default.
fn ban_append_expr(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    match e {
        Expr::Var { name, .. } => {
            banned.insert(name.clone());
        }
        // A String's fields are `byteLength`/`charCount` — an Int, not a borrow.
        Expr::Field { expr, .. } => ban_append_read(expr, banned, strict),
        // The two copying builtins the interpolation desugar emits: `@str`
        // strdups its argument, `@concat` builds a fresh buffer from both
        // halves. Only these two, because the lexer cannot produce a leading
        // `@` — no local binding can shadow the name and turn the call into a
        // dispatch through a stored function value that keeps what it is given.
        // (`print` is spellable, and `let print = f` does exactly that.)
        Expr::Call { name, args, .. } if matches!(name.as_str(), "@str" | "@concat") => {
            for a in args {
                ban_append_read(a, banned, strict);
            }
        }
        Expr::Call { args, .. }
        | Expr::Spawn { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                ban_append_expr(a, banned, strict);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } => ban_append_expr(expr, banned, strict),
        // A take hands the stored place the buffer itself, so the root is
        // banned exactly as a bare mention is.
        Expr::Consume { place, .. } => ban_append_expr(place, banned, strict),
        // An operator's operands are a retaining position only if the LOWERING
        // keeps the pointer. `binop_retains_str` is the decision, exhaustive on
        // `BinOp` so a new operator cannot be added without making one.
        //
        // Banning every operand was the whole of `toJson`'s O(N²): `return out +
        // "]"` at the end of `std/json`'s `emitArr` disqualified `out`, so every
        // element re-`malloc`'d and re-copied the entire result so far (and
        // leaked the previous buffer, which is why 50k records OOM'd on 2.5 MB
        // of output). 80k `Int64` natively: 23.5 s before, 12 ms after. Forty
        // more `return acc + "…"` sites across `std/` were in the same trap.
        Expr::Binary { op, lhs, rhs, .. } => {
            if binop_retains_str(*op) {
                ban_append_expr(lhs, banned, strict);
                ban_append_expr(rhs, banned, strict);
            } else {
                ban_append_read(lhs, banned, strict);
                ban_append_read(rhs, banned, strict);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            ban_append_expr(scrutinee, banned, strict);
            for arm in arms {
                match &arm.body {
                    ArmBody::Expr(e) => ban_append_expr(e, banned, strict),
                    // A block arm (RFC-0118): the throwaway target set means a
                    // self-append inside it keeps the copying path — correct,
                    // just not upgraded; the in-place path can follow demand.
                    ArmBody::Block(b) => {
                        scan_append_block(b, &mut std::collections::HashSet::new(), banned, strict)
                    }
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            ban_append_expr(cond, banned, strict);
            ban_append_expr(then_branch, banned, strict);
            if let Some(eb) = else_branch {
                ban_append_expr(eb, banned, strict);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                ban_append_expr(v, banned, strict);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                ban_append_expr(k, banned, strict);
                ban_append_expr(v, banned, strict);
            }
        }
        // A capture outlives the append, so nothing a lambda touches is eligible.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => ban_append_expr(e, banned, true),
            LambdaBody::Block(b) => {
                scan_append_block(b, &mut std::collections::HashSet::new(), banned, true)
            }
        },
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

/// The type arguments a construction or call site instantiates a generic with:
/// `declared` are the parametric types (a function's parameters, an enum
/// variant's payloads, a record's fields), `actual` the concrete types supplied
/// for them. Each declared type parameter comes back `Some` if the match fixed
/// it and `None` if nothing did.
///
/// This is **the rule**, shared rather than reimplemented (RFC-0077 M2e). It was
/// shared because two backends solving one site differently would specialize
/// *different* functions for the same source, and nothing in this repo compared
/// their instantiation sets. One route is left (RFC-0125 §2.5) and the rule stays
/// stated here, where the checker's depth refusal and `vyrn-lower`'s worklist can
/// both read it.
///
/// What the backend does with a `None` is its own business, which is why this
/// reports rather than decides. The retired LLVM emitter substituted `Unit` and
/// let it lower to `void`; the wasm emitter refuses, because a `void` in a wasm
/// signature is not a diagnostic, it is a different function.
/// The type arguments of a generic call, with any parameter the arguments left
/// open taken from the type the call site expects.
///
/// `fn newSlots<T>() -> Slots<T>` has nothing to read its `T` off: the empty
/// container carries no element. The checker answers from the expected type, so
/// this must answer the same way or the two disagree about which instance the
/// program calls.
pub(crate) fn solve_with_expected(
    type_params: &[String],
    params: &[Type],
    arg_tys: &[Type],
    ret: &Type,
    expected: Option<&Type>,
) -> (HashMap<String, Type>, Vec<Option<Type>>) {
    let (mut subst, solved) = solve_type_args(type_params, params, arg_tys);
    if !solved.iter().any(|t| t.is_none()) {
        return (subst, solved);
    }
    let Some(want) = expected else {
        return (subst, solved);
    };
    let (from_ret, ret_solved) = solve_type_args(
        type_params,
        std::slice::from_ref(ret),
        std::slice::from_ref(want),
    );
    for (tp, t) in from_ret {
        subst.entry(tp).or_insert(t);
    }
    let solved = solved
        .into_iter()
        .zip(ret_solved)
        .map(|(a, b)| a.or(b))
        .collect();
    (subst, solved)
}

pub(crate) fn solve_type_args(
    type_params: &[String],
    declared: &[Type],
    actual: &[Type],
) -> (HashMap<String, Type>, Vec<Option<Type>>) {
    let mut subst: HashMap<String, Type> = HashMap::new();
    for (d, a) in declared.iter().zip(actual) {
        solve_param(d, a, &mut subst);
    }
    let args = type_params.iter().map(|p| subst.get(p).cloned()).collect();
    (subst, args)
}

/// The concrete type a construction site of `name` produces: the bare
/// [`Type::Named`] when the declaration takes no parameters, and otherwise a
/// [`Type::App`] with each parameter solved from what was supplied. An unsolved
/// parameter becomes `Unit`, which is what this has always done for enums.
pub(crate) fn applied_type(
    decl: Option<&TypeDecl>,
    name: &str,
    declared: &[Type],
    actual: &[Type],
) -> Type {
    let named = || Type::Named(name.to_string());
    let Some(decl) = decl.filter(|d| !d.type_params.is_empty()) else {
        return named();
    };
    let (_, args) = solve_type_args(&decl.type_params, declared, actual);
    Type::App(
        name.to_string(),
        args.into_iter().map(|a| a.unwrap_or(Type::Unit)).collect(),
    )
}

/// The type arguments a construction site's EXPECTED type already names.
///
/// [`applied_type`] reads a generic's arguments off what is supplied — a record's
/// field values, a variant's payload. That is the only source when there is no
/// other, and it is enough for a field whose value carries its own type. It is
/// NOT enough for a field that carries a `fn` (RFC-0037): a stored `fn` value
/// registers its dispatch variant against the type it is being built FOR, so if
/// that type is still `fn(P) -> T` when the value is built, the variant lands
/// under a signature no dispatcher covers.
///
/// The site's own expectation knows the answer before any field is read. This
/// takes the arguments it names, and only the ones it settles: a `Unit`
/// placeholder or an open `Param` says nothing, so the value-side solve keeps
/// those.
///
/// Shared with the direct wasm backend, for [`applied_type`]'s reason — two
/// backends seeding one construction differently would build two different
/// types for one literal.
/// Whether a field's value can settle a type parameter at all.
///
/// An empty `[]`/`[:]` reports a PLACEHOLDER element type — the representation
/// is type-independent, so the element type is picked rather than known. Letting
/// it settle the record's parameter binds the placeholder: `Deque { back: ["z"],
/// front: [] }` solved `T = Int64` from the empty `front` (emitted first, in
/// DECLARED order) and then stored a String pointer into an `i64` element. It
/// settles nothing; a later field, or the site's expectation, answers. The
/// checker reaches the same conclusion by a different road — there `[]` reports
/// `Array<T>`, and a parameter bound to itself is dropped.
///
/// Shared with the direct wasm backend for [`expected_type_args`]'s reason.
pub(crate) fn settles_type_args(e: &Expr) -> bool {
    !matches!(e, Expr::ArrayLit { elems, .. } if elems.is_empty())
        && !matches!(e, Expr::MapLit { entries, .. } if entries.is_empty())
}

pub(crate) fn expected_type_args(
    expected: Option<&Type>,
    name: &str,
    decl: Option<&TypeDecl>,
) -> HashMap<String, Type> {
    let Some(Type::App(en, args)) = expected else {
        return HashMap::new();
    };
    let Some(decl) = decl.filter(|d| en == name && d.type_params.len() == args.len()) else {
        return HashMap::new();
    };
    decl.type_params
        .iter()
        .zip(args)
        .filter(|(_, a)| !matches!(a, Type::Unit | Type::Param(_)))
        .map(|(p, a)| (p.clone(), a.clone()))
        .collect()
}

/// The declaration whose `where` predicate a value flowing from `from` into `to`
/// must satisfy, if any (RFC-0003's automatic validation).
///
/// It was the one copy of that decision for the compiled backends and it was in
/// the wrong crate: the interpreter lives in `vyrn-frontend`, which this crate
/// depends on, so it could not read this and asked the same question itself —
/// the census of RFC-0125 §3 M6 counted three more askings in `interp.rs`. The
/// statement is [`vyrn_frontend::validate::required`] now, beside `trap` and for
/// the same reason (RFC-0101 §6.4), and this name is the emitter's spelling of it
/// rather than a second copy.
pub(crate) use vyrn_frontend::validate::required as validation_required;

/// One rung of the boundary ladder — RFC-0101 §1.5's shadow.
///
/// A `coerce` is where a value crosses into a declared type, and each engine
/// wrote the decision as a ladder of guarded rungs: 146 lines in the textual
/// emitter, 198 in the direct one, and §1.5 measured them as ONE decision until
/// M6's second phase read them against each other and found two — the same first
/// two rungs, differently ordered middles, one rung each the other lacks, and
/// opposite ends. M6 made both ask this vocabulary instead, and RFC-0125 §3 M4's
/// fourth slice left one ladder asking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// A `Never` (RFC-0079) reaching a boundary: the `panic` already left, so
    /// there is no value to reconcile.
    Never,
    /// A refined named type: coerce into the base, then run its `where`
    /// predicate. The base crossing is a rung of its own, at its own pair.
    Validate,
    /// A function value between `fn`-typed spellings: a re-tag, no instruction.
    FnRetag,
    /// A fixed array whose ELEMENT type changes: element by element, so each
    /// element crosses its own boundary (and validates, if it has one).
    Elementwise,
    /// A fixed array into the growable `{ptr,len,cap}` triple.
    Heapify,
    /// A fixed array into a `SmallArray`'s inline buffer.
    Inline,
    /// An integer resize: truncate, extend, or renormalise.
    Resize,
    /// Across the int/float line, or between the two float widths.
    FloatCross,
    /// A record used as a differently shaped record: rebuilt field by field.
    Rebuild,
    /// One sum used at another shape of ITSELF — the same variant names, a
    /// different slot count. Since RFC-0126 §8.4 a sum's slot count follows its
    /// widest payload, so `Validation<Unit>` and `Validation<Point>` are two
    /// shapes of one enum, and a bare `None` is built at the narrowest `Option`
    /// there is. The tag and the slots both shapes have move; the rest is zero.
    Reshape,
    /// The bits are already right.
    Identity,
    /// No rung handles this pair.
    Refuse,
}

/// The variant names of a sum, in tag order — `None` for anything else. The
/// key [`coerce_plan`] compares when it asks whether two sums are one sum.
pub(crate) fn sum_variants(ty: &Type) -> Option<Vec<String>> {
    match ty {
        Type::Enum(vs) => Some(vs.iter().map(|v| v.name.clone()).collect()),
        _ => None,
    }
}

/// The VARIANTS of a sum, in tag order — RFC-0126 §8.1, and the one reading of
/// a sum's shape that both compiled engines have.
///
/// `Option<T>` IS `| None | Some(T)` and `Result<T, E>` IS `| Err(E) | Ok(T)`
/// since §8.15's M5 deleted the two constructors, so this is `resolve` and a
/// shape test. It stays a named reading because everything that walks a sum — a
/// release, a copy, a match — asks it, and the question "is this a sum" is worth
/// one name.
pub(crate) fn sum_variants_of(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
) -> Option<Vec<EnumVariant>> {
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Enum(vs) => Some(vs),
        _ => None,
    }
}

/// An arm of an engine's `coerce` could not destructure the pair the plan sent
/// it. Unreachable unless [`coerce_plan`] and that arm have come apart, which is
/// the one class of disagreement a single statement of a rule can still have —
/// so it is one sentence, here, rather than one per arm (RFC-0125 §2.3).
pub(crate) fn plan_disagrees(from: &Type, to: &Type, rung: Rung) -> String {
    format!("the coercion plan placed {rung:?} for `{from}` into `{to}` and the emitter's arm for it cannot take that pair")
}

/// The rung a value crossing from `from` into `to` takes — the PLAN both
/// compiled backends are held to (RFC-0101 §1.5).
///
/// **Two engines, not three** (§2.4 row 4): the interpreter's `coerce` takes a
/// value and a target and has no `from` at all, so it cannot be held to a plan
/// keyed on a pair until it runs the form.
///
/// **The order is this function's, and it is where the rules come from.** The
/// two ladders agree on the first two rungs and order their middles differently;
/// a middle rung's guard is disjoint from the others', so the order is
/// observable at exactly one place — an integer pair whose two spellings share
/// one LLVM shape (`i8` for `Int8` and `UInt8` alike), which is why the resize
/// comes before [`Rung::Identity`] here and in the direct backend. Every
/// remaining difference is one engine taking a rung the other does not have; the
/// corpus gate names each one as a rule rather than hiding it in a plan that
/// splits the difference.
///
/// It is beside [`validation_required`] and [`llt_of`] rather than in
/// `vyrn-lower` because it is made OF them: this is where the two shared
/// codegen decisions already live, and a plan keyed on a pair needs no site.
pub fn coerce_plan(from: &Type, to: &Type, types: &HashMap<String, TypeDecl>) -> Rung {
    let (rf, rt) = (
        vyrn_frontend::types::resolve(from, types),
        vyrn_frontend::types::resolve(to, types),
    );
    // `Int` and `Int64` are one type, and `IntN` carries its own width and
    // signedness — so "the same integer" is a comparison of resolved spellings,
    // and a pair that IS the same integer needs no rung at all.
    let num = |t: &Type| matches!(t, Type::Int | Type::IntN { .. });
    let flt = |t: &Type| matches!(t, Type::Float | Type::Float32);
    if matches!(from, Type::Never) {
        return Rung::Never;
    }
    if validation_required(from, to, types).is_some() {
        return Rung::Validate;
    }
    if num(&rf) && num(&rt) && rf != rt {
        return Rung::Resize;
    }
    if (flt(&rf) || flt(&rt)) && (num(&rf) || num(&rt) || flt(&rf) && flt(&rt)) && rf != rt {
        return Rung::FloatCross;
    }
    if matches!(rf, Type::Fn(..)) && matches!(rt, Type::Fn(..)) {
        return Rung::FnRetag;
    }
    match (&rf, &rt) {
        (Type::ArrayN(fi, fnn), Type::ArrayN(ti, tn)) if fi != ti && fnn == tn => {
            return Rung::Elementwise
        }
        (Type::ArrayN(fi, _), Type::Array(ti))
            if fi == ti || llt_of(fi, types) == llt_of(ti, types) =>
        {
            return Rung::Heapify
        }
        (Type::ArrayN(fi, len), Type::SmallArray(ti, n))
            if llt_of(fi, types) == llt_of(ti, types) && len <= n =>
        {
            return Rung::Inline
        }
        _ => {}
    }
    if llt_of(from, types) == llt_of(to, types) {
        return Rung::Identity;
    }
    // Two shapes of one sum (RFC-0126 §8.4). The variant NAMES decide it, so a
    // generic enum at two instantiations qualifies and two different enums that
    // happen to be the same width do not.
    if let (Some(a), Some(b)) = (sum_variants(&rf), sum_variants(&rt)) {
        if a == b {
            return Rung::Reshape;
        }
    }
    match (
        vyrn_frontend::types::record_fields(&rf, types),
        vyrn_frontend::types::record_fields(&rt, types),
    ) {
        (Some(_), Some(_)) => Rung::Rebuild,
        _ => Rung::Refuse,
    }
}

/// The LLVM shape of a Vyrn type: the ONE match that turns a type into a memory
/// layout, so `Gen::llt` and RFC-0077's direct wasm backend cannot come to
/// different conclusions about the same value. `layout::of_ll` parses what this
/// prints, which is what keeps size and offset arithmetic downstream of lowering
/// rather than beside it.
///
/// `ty` is resolved here but NOT substituted — a caller inside a monomorphized
/// body substitutes first (`Gen::llt` does), because only it knows which
/// instantiation it is in.
pub(crate) fn llt_of(ty: &Type, types: &HashMap<String, TypeDecl>) -> String {
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Int => "i64".into(),
        Type::IntN { bits, .. } => format!("i{bits}"),
        Type::Float => "double".into(),
        Type::Float32 => "float".into(),
        // RFC-0083: LLVM's own vector type, so `fadd <4 x float>` is one
        // instruction and the register allocator puts it in an xmm. The direct
        // wasm backend reads this same spelling back out to reach `v128`, which is
        // why the vector lives here rather than in a table of its own.
        Type::F32x4 => "<4 x float>".into(),
        // M3's integer width, sharing the mask's spelling: `<4 x i32>` is what a
        // v128 of four 32-bit lanes IS on both backends, and the two are different
        // Vyrn TYPES rather than different representations — which is exactly why
        // an `I32x4` comparison can produce a `Mask32x4` without a conversion.
        Type::I32x4 => "<4 x i32>".into(),
        // A mask is `<4 x i32>` of all-ones/all-zeros, not the `<4 x i1>` an
        // `fcmp` actually produces. `<4 x i1>` is a legal IR type but a strange
        // ABI one — it is passed as a packed `i4` in places — and a mask crosses
        // function boundaries here like any other value. The `sext`/`trunc` pair
        // that costs is folded away by `-O2` at every use, which was checked.
        Type::Mask32x4 => "<4 x i32>".into(),
        // M4's wide float width and its own mask, on the same two rules: the
        // vector is LLVM's own so the arithmetic is one instruction, and the mask
        // is all-ones/all-zeros at the LANE width — `<2 x i64>` and not `<2 x i1>`
        // — so the two backends carry the same bit pattern here as they do at 32.
        Type::F64x2 => "<2 x double>".into(),
        Type::Mask64x2 => "<2 x i64>".into(),
        Type::Bool => "i1".into(),
        Type::Str => "ptr".into(),
        // `Never` (RFC-0079) carries no value, so it lowers like `Unit`: a
        // statement-position `panic` has nothing to drop, and a `void` join is
        // already the "no value to merge" case both merges test for.
        Type::Unit | Type::Never => "void".into(),
        // RFC-0126 §8.4: one shape rule for every sum. `{ i64 tag, i64 slot.. }`,
        // where the slot count is the widest variant's payload WIDTH — so
        // `Option<Int64>` is two words where it was three, and `Option<fn>` is
        // three because a stored `fn` needs two and rides inline. The tag is the
        // enum's `i64` rather than the sums' old `i1`: it costs no bytes (the
        // first member padded to 8 either way) and it makes one shape serve both,
        // which is what M4 needs when `resolve` starts answering `Enum` here.
        // A growable array is { ptr data, i64 len, i64 cap }.
        Type::Array(_) => "{ ptr, i64, i64 }".into(),
        // A `Stream<T>` (RFC-0075 M2b) is a tagged header over two producers,
        // and this is the line M2 said would change:
        //
        //   { ptr data, i64 len, i64 tag, i64 pay, i64 cur, i64 gen }
        //
        // `tag` is the discriminant. Negative means a BUFFER — `data`/`len` are
        // the array `fromArray` was handed and `cur` is how far the consumer has
        // read. Non-negative means a STEP: `tag`/`pay` ARE an RFC-0037 function
        // value, `cur`/`gen` ARE the `Ref<Int64>` cursor cell it is called with,
        // `len` is 0 until the step answers `None` and 1 after, and `data` is
        // null.
        //
        // The two-word pairs are adjacent and 8-aligned on purpose: `{ i64, i64 }`
        // is exactly what a fn value and a `Ref` each lower to, so `&s + 16` and
        // `&s + 32` ARE those values and neither backend has to reassemble one to
        // make the call.
        //
        // Overlaid rather than a union of the widest variant because the two
        // shapes are 3 and 5 words and a discriminated 6-word header costs less
        // than a heap box plus an indirection on every `next`. Nothing reads a
        // field whose variant it has not tested.
        Type::Stream(_) => "{ ptr, i64, i64, i64, i64, i64 }".into(),
        // A `Map<String, V>` (RFC-0028) is two parallel growable buffers
        // sharing one length/capacity, plus the hash index over them:
        // { ptr keys, ptr values, i64 len, i64 cap, ptr idx }. Keys are `ptr`
        // (String); values are `llt(V)`-stride; `idx` is `cap * 2` buckets of
        // i64, and the shim's `map_hash`/`map_slot` are what read it.
        Type::Map(..) => "{ ptr, ptr, i64, i64, ptr }".into(),
        // A fixed-size array lowers to the LLVM value aggregate [N x T].
        Type::ArrayN(inner, n) => format!("[{n} x {}]", llt_of(&inner, types)),
        // A small-buffer array (RFC-0056) lowers to
        // `{ i64 len, i64 cap, ptr data, [N x T] inline }` — `cap` is the
        // state discriminant (`cap == N` inline; `cap > N` spilled onto
        // `data`). Every element access branches on it to pick the base.
        Type::SmallArray(inner, n) => {
            format!("{{ i64, i64, ptr, [{n} x {}] }}", llt_of(&inner, types))
        }
        // A task handle (RFC-0025) is an opaque `ptr` to the shim's task
        // record (thread handle + heap frame); `t.join()` blocks on it and
        // loads the result from the frame's leading slot.
        Type::Task(_) => "ptr".into(),
        // A logger handle is a `ptr` to its name string.
        Type::Logger => "ptr".into(),
        Type::Record(fields) => {
            let inner: Vec<String> = fields.iter().map(|f| llt_of(&f.ty, types)).collect();
            format!("{{ {} }}", inner.join(", "))
        }
        // A user enum is { i64 tag, i64 slot0, ... } — RFC-0126 §8.4's slot
        // count, which is the widest variant's payload WIDTH and not its arity:
        // a payload two words wide rides in two slots instead of a heap box.
        Type::Enum(ref vs) => enum_ll(enum_slots_of(vs, types)),
        // RFC-0076 M3a: on the generator-host path `Code` (RFC-0054) is an
        // opaque i64 HANDLE into the host's piece arena — the one `Named`
        // that survives `resolve` undeclared. i64 is also what makes it
        // travel for free: `box_payload` passes an i64 through, so a `Code`
        // in an Option/Array needs no case of its own.
        Type::Named(ref n) if n == "Code" && gen_host() => "i64".into(),
        // Unreachable after `resolve` (Named/App/transformers/params reduced away).
        Type::Named(_)
        | Type::App(..)
        | Type::Omit(..)
        | Type::Pick(..)
        | Type::Merge(..)
        | Type::Partial(..)
        | Type::Param(_) => "void".into(),
        // A bare integer type argument (RFC-0056) never stands alone as a
        // runtime type — `SmallArray` consumes it before lowering.
        Type::ConstInt(_) => "void".into(),
        // A stored function value (RFC-0037) is a synthesized closed enum:
        // `{ i64 tag, i64 payload }` — tag selects the source (one variant
        // per named function / lifted lambda), payload is 0 or a pointer to
        // the malloc'd capture block. v1 `fn`-typed PARAMETERS never reach
        // `llt` (they monomorphize away before lowering).
        Type::Fn(..) => "{ i64, i64 }".into(),
        // Unreachable: `resolve` (above) answers `Fn([], T)` for a `lazy T`
        // field, which is exactly the point — the deferral has no layout of its
        // own (RFC-0085 M4a).
        Type::Lazy(_) => "{ i64, i64 }".into(),
        // `Err` is the checker's recovery sentinel; a program with any `Err`
        // already has diagnostics and never reaches codegen. Lower to void
        // as a defensive fallback (never observed in practice).
        Type::Err => "void".into(),
    }
}

/// The LLVM aggregate type for a sum with `slots` payload words:
/// `{ i64 }` (tag only) for 0, `{ i64, i64 }` for 1, and so on.
fn enum_ll(slots: usize) -> String {
    let mut s = String::from("{ i64");
    for _ in 0..slots {
        s.push_str(", i64");
    }
    s.push_str(" }");
    s
}

/// RFC-0126 §8.4's `words(t)` and §8.11's boxing rule, both stated in
/// [`vyrn_frontend::types`] because `own` asks the same two questions of the
/// same types and used to answer them from a hand-written word list of its own.
pub(crate) use vyrn_frontend::types::payload_words as payload_words_of;

/// The slots one variant's payloads occupy, laid out consecutively.
fn variant_slots_of(payload: &[Type], types: &HashMap<String, TypeDecl>) -> usize {
    payload.iter().map(|p| payload_words_of(p, types)).sum()
}

/// A user enum's slot count: the widest variant's, by §8.4.
fn enum_slots_of(vs: &[EnumVariant], types: &HashMap<String, TypeDecl>) -> usize {
    vs.iter()
        .map(|v| variant_slots_of(&v.payload, types))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::check;

    // ---- the layout engine's link to lowering (RFC-0077 M0) -------------

    /// [`layout::SHAPES`] is what the clang comparison is run against, so it is
    /// only worth anything if it is what `llt` prints. Assert that here, so a new
    /// case in `llt` cannot quietly escape the layout check that stands between
    /// it and a silent miscompile.
    #[test]
    fn llt_prints_the_shapes_the_layout_engine_was_verified_on() {
        let types: HashMap<String, TypeDecl> = HashMap::new();
        let rec = |fs: &[Type]| {
            Type::Record(
                fs.iter()
                    .enumerate()
                    .map(|(i, t)| Field {
                        name: format!("f{i}"),
                        ty: t.clone(),
                    })
                    .collect(),
            )
        };
        let i8t = Type::IntN {
            bits: 8,
            signed: false,
        };
        let cases: &[(&str, Type)] = &[
            ("Int64", Type::Int),
            (
                "Int32",
                Type::IntN {
                    bits: 32,
                    signed: true,
                },
            ),
            (
                "Int16",
                Type::IntN {
                    bits: 16,
                    signed: true,
                },
            ),
            ("Int8", i8t.clone()),
            ("Bool", Type::Bool),
            ("Float64", Type::Float),
            ("Float32", Type::Float32),
            ("String", Type::Str),
            // Since RFC-0126 §8.4 the built-in sums print the enum rows: one
            // slot for a one-word payload, two when the widest is two words.
            ("Enum1", Type::option(Type::Int)),
            ("Enum1", Type::result(Type::Int, Type::Str)),
            (
                "Enum2",
                Type::option(Type::Fn(Vec::new(), Box::new(Type::Int))),
            ),
            ("Array", Type::Array(Box::new(Type::Str))),
            ("Map", Type::Map(Box::new(Type::Str), Box::new(Type::Int))),
            ("Fn", Type::Fn(Vec::new(), Box::new(Type::Int))),
            ("RecordEmpty", rec(&[])),
            (
                "RecordMixed",
                rec(&[Type::Bool, Type::Str, Type::Int, i8t.clone(), Type::Float]),
            ),
            ("ArrayN_i64", Type::ArrayN(Box::new(Type::Int), 4)),
            ("ArrayN_i8", Type::ArrayN(Box::new(i8t.clone()), 3)),
            ("SmallArray_i64", Type::SmallArray(Box::new(Type::Int), 4)),
            ("SmallArray_i8", Type::SmallArray(Box::new(i8t), 3)),
            ("SmallArray_str", Type::SmallArray(Box::new(Type::Str), 2)),
        ];
        for (name, ty) in cases {
            let want = layout::SHAPES
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} missing from layout::SHAPES"))
                .1;
            assert_eq!(
                &llt_of(ty, &types),
                want,
                "llt({name}) drifted from layout::SHAPES"
            );
        }
        // The enum arities, which come from the same helper `llt` calls.
        for (name, arity) in [("Enum0", 0), ("Enum1", 1), ("Enum2", 2), ("Enum3", 3)] {
            let want = layout::SHAPES.iter().find(|(n, _)| *n == name).unwrap().1;
            assert_eq!(enum_ll(arity), want, "enum_ll({arity}) drifted");
        }
    }

    /// One `Type` per variant of the type enum, held complete by a match the
    /// compiler will not let go stale.
    ///
    /// The match computes nothing. Its only job is to fail to compile when a
    /// variant is added, which is what makes the list below a list rather than a
    /// guess — the hand-written `cases` above is exactly what happens without
    /// one, and it had been missing `Stream` and every vector for as long as
    /// they had existed. Both halves live on the type itself
    /// ([`Type::VARIANTS`] / [`Type::variant_name`]), because `vyrn-frontend`'s
    /// wire-form coverage test asks the same question of the same list.
    fn layout_seeds() -> Vec<Type> {
        let b = |t: Type| Box::new(t);
        vec![
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: false,
            },
            Type::IntN {
                bits: 16,
                signed: true,
            },
            Type::IntN {
                bits: 32,
                signed: true,
            },
            Type::Float,
            Type::Float32,
            Type::F32x4,
            Type::I32x4,
            Type::F64x2,
            Type::Mask32x4,
            Type::Mask64x2,
            Type::Bool,
            Type::Str,
            Type::Unit,
            Type::Named("Nowhere".into()),
            Type::Record(Vec::new()),
            Type::Omit(b(Type::Record(Vec::new())), vec!["f".into()]),
            Type::Pick(b(Type::Record(Vec::new())), vec!["f".into()]),
            Type::Merge(b(Type::Record(Vec::new())), b(Type::Record(Vec::new()))),
            Type::Partial(b(Type::Record(Vec::new()))),
            Type::Enum(Vec::new()),
            Type::Param("T".into()),
            Type::App("Box".into(), vec![Type::Int]),
            Type::Array(b(Type::Int)),
            Type::ArrayN(b(Type::Int), 4),
            Type::SmallArray(b(Type::Int), 3),
            Type::ConstInt(8),
            Type::Map(b(Type::Str), b(Type::Int)),
            Type::Stream(b(Type::Int)),
            Type::Task(b(Type::Int)),
            Type::Logger,
            Type::Fn(vec![Type::Int], b(Type::Unit)),
            Type::Lazy(b(Type::Int)),
            Type::Never,
            Type::Err,
        ]
    }

    /// The wrappers [`grow`] has none of, because the mangle it was written for
    /// collapses a record whole and layout does not.
    ///
    /// Three per type: a one-field record, which shows the member's own
    /// alignment; an `i8`-then-`t` record, which shows the HOLE in front of it,
    /// where a wrong alignment becomes a wrong offset; and a one-payload enum.
    fn in_records(ts: &[Type]) -> Vec<Type> {
        let field = |n: &str, x: &Type| Field {
            name: n.into(),
            ty: x.clone(),
        };
        let byte = Type::IntN {
            bits: 8,
            signed: true,
        };
        ts.iter()
            .flat_map(|t| {
                [
                    Type::Record(vec![field("a", t)]),
                    Type::Record(vec![field("n", &byte), field("a", t)]),
                    Type::Enum(vec![EnumVariant {
                        name: "V".into(),
                        payload: vec![t.clone()],
                    }]),
                ]
            })
            .collect()
    }

    /// The leaf spellings in an `llt` string: each scalar word, and each whole
    /// `<N x T>` vector. The `{ }`, `[N x` and `,` scaffolding is structure, not
    /// a leaf — and a vector is a leaf rather than structure because it is the
    /// unit `of_ll` has to know a size for.
    fn atoms(ll: &str, out: &mut std::collections::BTreeSet<String>) {
        fn words(s: &str, out: &mut std::collections::BTreeSet<String>) {
            for w in s.split(|c: char| !c.is_ascii_alphanumeric()) {
                // Skip the counts and the `x` that separates them from the
                // element: both are grammar, and neither is a shape.
                if !w.is_empty() && w != "x" && !w.bytes().all(|c| c.is_ascii_digit()) {
                    out.insert(w.to_string());
                }
            }
        }
        let mut rest = ll;
        while let Some(a) = rest.find('<') {
            let b = a + rest[a..].find('>').expect("a vector spelling closes");
            words(&rest[..a], out);
            out.insert(rest[a..=b].to_string());
            rest = &rest[b + 1..];
        }
        words(rest, out);
    }

    /// Every composite shape, over the types it is given: containers, both
    /// generic applications, function types of three arities, and the sized
    /// containers at two capacities.
    fn grow(base: &[Type], pairs: &[Type]) -> Vec<Type> {
        let mut out = Vec::new();
        for t in base {
            let b = || Box::new(t.clone());
            out.extend([
                Type::option(t.clone()),
                Type::Array(b()),
                Type::Stream(b()),
                Type::Task(b()),
                Type::Lazy(b()),
                Type::ArrayN(b(), 4),
                Type::ArrayN(b(), 8),
                Type::SmallArray(b(), 4),
                Type::SmallArray(b(), 8),
                Type::App("P".into(), vec![t.clone()]),
                Type::App("Q".into(), vec![t.clone()]),
                Type::Fn(vec![], b()),
                Type::Fn(vec![t.clone()], Box::new(Type::Unit)),
            ]);
        }
        for a in pairs {
            for c in pairs {
                out.extend([
                    Type::result(a.clone(), c.clone()),
                    Type::Map(Box::new(a.clone()), Box::new(c.clone())),
                    Type::App("P".into(), vec![a.clone(), c.clone()]),
                    Type::Fn(vec![a.clone(), c.clone()], Box::new(Type::Unit)),
                    Type::Fn(vec![a.clone()], Box::new(c.clone())),
                ]);
            }
        }
        out
    }

    /// The guard the hand-written list above cannot be.
    ///
    /// [`layout::SHAPES`] claims to be the emitter's whole type universe, and
    /// the check next to it walks a list a human typed — so `Stream` and `Ref`
    /// sat in `SHAPES` with no case in the test, and RFC-0083's four vector
    /// spellings were printed by `llt`, refused by `of_ll`, and never once
    /// compared against clang. A list cannot guard a list.
    ///
    /// So derive the cases, on PR #165's generator itself. [`layout_seeds`] is
    /// one `Type` per variant of the type enum, held complete by
    /// [`Type::variant_name`] — an exhaustive match, so a new variant is a COMPILE
    /// error here before it is a missing layout in front of a user. [`grow`],
    /// which the mangle-injectivity test already owns, composes those through
    /// every container constructor twice, and [`in_records`] adds the two
    /// wrappers a mangle does not care about and a layout does. That is a few
    /// thousand type trees, and two things hold over all of them:
    ///
    /// 1. every one has a layout — a shape `llt` can print and `of_ll` cannot
    ///    parse is this test failing, not a build dying at the user;
    /// 2. every leaf spelling that appears also appears in `SHAPES`, so a new
    ///    case in `llt` cannot escape the clang comparison.
    ///
    /// # What it cannot derive
    ///
    /// The reverse direction is leaf-wise — no DEAD spelling in `SHAPES` —
    /// rather than "every row is generated", and that is a real limit rather
    /// than an oversight. Most rows are hand-built PADDING probes:
    /// `RecordNested`, `SmallArray_i8`, `RecordOfVector` are chosen because
    /// clang and the engine could plausibly disagree about them, and no
    /// enumeration of the type enum produces those exact trees. Nor are the
    /// NAMES derivable: `Ref` is `{ i64, i64 }`, the same string a stored `fn`
    /// prints, and nothing in the type enum spells a `Ref` at all.
    #[test]
    fn llt_prints_every_shape_the_layout_engine_was_verified_on() {
        let types = HashMap::new();
        let seeds = layout_seeds();
        // The lock, in two halves. `variant_name`'s match is exhaustive, so a
        // new variant of the type enum stops this file compiling; `VARIANTS` is
        // the same list as a value, so a variant that is named but never seeded
        // stops the test passing. Neither half alone is a guard.
        let seeded: std::collections::BTreeSet<&str> =
            seeds.iter().map(|t| t.variant_name()).collect();
        for v in Type::VARIANTS {
            assert!(seeded.contains(v), "no seed for Type::{v}");
        }
        assert!(
            seeded.iter().all(|s| Type::VARIANTS.contains(s)),
            "a seed names a variant Type::VARIANTS does not list"
        );
        let d1 = grow(&seeds, &seeds[..8]);
        let d2 = grow(&d1[..200], &d1[..20]);
        let all: Vec<Type> = seeds
            .iter()
            .chain(d1.iter())
            .chain(d2.iter())
            .chain(in_records(&seeds).iter())
            .chain(in_records(&d1[..60]).iter())
            .cloned()
            .collect();

        let mut printed = std::collections::BTreeSet::new();
        for ty in &all {
            let ll = llt_of(ty, &types);
            let l = layout::of_ll(&ll)
                .unwrap_or_else(|e| panic!("llt({ty}) = {ll}, which has no layout: {e}"));
            assert!(l.align.is_power_of_two(), "{ll}: align {}", l.align);
            assert_eq!(l.size % l.align, 0, "{ll}: size {} is not padded", l.size);
            atoms(&ll, &mut printed);
        }

        let mut covered = std::collections::BTreeSet::new();
        for (_, ll) in layout::SHAPES {
            atoms(ll, &mut covered);
        }
        // `void` is the one leaf that cannot join them, and the reason is C's,
        // not this crate's: `sizeof(void)` is a GNU extension answering 1 where
        // the engine answers 0, so a `void` row would make the clang comparison
        // disagree about a shape that has no bytes and never occupies any. It is
        // what `llt` prints for `Unit`, `Never`, and every type that resolved
        // away — none of which can be a member of anything.
        printed.remove("void");
        covered.remove("void");
        let missing: Vec<_> = printed.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "{} type trees print {missing:?}, which layout::SHAPES does not cover — \
             so clang is never asked about it",
            all.len()
        );
        let dead: Vec<_> = covered.difference(&printed).collect();
        assert!(
            dead.is_empty(),
            "layout::SHAPES spells {dead:?}, which `llt` no longer prints"
        );
        assert!(all.len() > 4_000, "the corpus shrank to {}", all.len());
    }

    // ---- RFC-0086: the shapes `solve_param` cannot descend into ---------

    /// Every `.vyrn` file under `rel`, relative to the repository root.
    fn corpus(rel: &str, out: &mut Vec<std::path::PathBuf>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "vyrn") {
                    out.push(p);
                }
            }
        }
    }

    /// The constructors [`solve_param`] descends through. A [`Type::Param`]
    /// reached by any other route is a parameter the solver cannot bind.
    fn unreachable_params(t: &Type, under: Option<&'static str>, out: &mut Vec<&'static str>) {
        let go = unreachable_params;
        match t {
            // The root arm: a bare `Param` binds whatever it faces.
            Type::Param(_) => {
                if let Some(k) = under {
                    out.push(k);
                }
            }
            Type::Array(a) | Type::ArrayN(a, _) | Type::SmallArray(a, _) | Type::Stream(a) => {
                go(a, under, out)
            }
            Type::Map(a, b) => {
                go(a, under, out);
                go(b, under, out);
            }
            Type::App(_, args) => {
                for a in args {
                    go(a, under, out);
                }
            }
            Type::Fn(ps, r) => {
                for p in ps {
                    go(p, under, out);
                }
                go(r, under, out);
            }
            // Everything below carries a type and has no arm.
            Type::Record(fs) => {
                for f in fs {
                    go(&f.ty, under.or(Some("Record")), out);
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for p in &v.payload {
                        go(p, under.or(Some("Enum")), out);
                    }
                }
            }
            Type::Lazy(a) => go(a, under.or(Some("Lazy")), out),
            Type::Task(a) => go(a, under.or(Some("Task")), out),
            Type::Partial(a) | Type::Omit(a, _) | Type::Pick(a, _) => {
                go(a, under.or(Some("Omit/Pick/Partial")), out)
            }
            Type::Merge(a, b) => {
                go(a, under.or(Some("Merge")), out);
                go(b, under.or(Some("Merge")), out);
            }
            _ => {}
        }
    }

    /// RFC-0086's last open list: how many places in the corpus hand
    /// [`solve_param`] a declared type whose type parameter sits under a
    /// constructor the match has no arm for.
    ///
    /// It counts the four positions the solver is actually called from — a
    /// generic function's parameters and return, a generic record declaration's
    /// fields, a generic enum's variant payloads, and a generic impl head. Each
    /// of those types is a *root* the solver receives, so a `Param` directly at
    /// the root is fine; only one buried under an unhandled constructor is a
    /// site the solver walks past.
    ///
    /// It parses each file ALONE — no loader, no linking — like the RFC-0089
    /// corpus measurements. A declared type is written where it is declared, so
    /// linking would add no site this misses.
    ///
    /// Ignored by default: it reads the repository, so it is a measurement, not
    /// a unit test. Run it with
    /// `cargo test -p vyrn-codegen --lib rfc0086 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rfc0086_unsolvable_parameter_positions_over_the_corpus() {
        let mut files = Vec::new();
        corpus("examples", &mut files);
        corpus("std", &mut files);
        files.sort();

        let mut by_kind: std::collections::BTreeMap<&'static str, usize> = Default::default();
        let mut rows: Vec<String> = Vec::new();
        let (mut parsed, mut roots) = (0, 0);
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(tokens) = vyrn_frontend::lexer::lex(&src) else {
                continue;
            };
            let (program, errs) = vyrn_frontend::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // (what it is, the generic's name, the line, the root types)
            let mut sites: Vec<(&str, String, usize, Vec<Type>)> = Vec::new();
            for f in &program.functions {
                if f.type_params.is_empty() {
                    continue;
                }
                let mut ts: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
                ts.push(f.ret.clone());
                sites.push(("fn", f.name.clone(), f.line, ts));
            }
            for d in &program.type_decls {
                if d.type_params.is_empty() {
                    continue;
                }
                let ts = match &d.base {
                    Type::Record(fs) => fs.iter().map(|f| f.ty.clone()).collect(),
                    Type::Enum(vs) => vs.iter().flat_map(|v| v.payload.clone()).collect(),
                    other => vec![other.clone()],
                };
                sites.push(("type", d.name.clone(), d.line, ts));
            }
            for i in &program.impls {
                if i.type_params.is_empty() {
                    continue;
                }
                sites.push(("impl", i.protocol.clone(), i.line, vec![i.ty.clone()]));
            }
            for (kind, who, line, ts) in sites {
                for t in &ts {
                    roots += 1;
                    let mut hits = Vec::new();
                    unreachable_params(t, None, &mut hits);
                    for h in hits {
                        *by_kind.entry(h).or_default() += 1;
                        rows.push(format!("{name}:{line} {kind} `{who}` — {t} under {h}"));
                    }
                }
            }
        }

        println!(
            "corpus: {} files ({parsed} parsed), {roots} declared root types",
            files.len()
        );
        println!("parameters `solve_param` cannot reach: {}", rows.len());
        for (k, c) in &by_kind {
            println!("  {k:>18}: {c}");
        }
        for r in &rows {
            println!("    {r}");
        }
    }

    /// The four arms the census found no victim for, exercised directly.
    ///
    /// They are dead behind the checker today (see the test below), so nothing
    /// else would notice them being wrong. This is what says they are right.
    #[test]
    fn the_filled_arms_bind_a_parameter_the_fall_through_walked_past() {
        let t = || Type::Param("T".into());
        let fld = |n: &str, ty: Type| Field { name: n.into(), ty };
        let var = |n: &str, payload: Vec<Type>| EnumVariant {
            name: n.into(),
            payload,
        };
        let solved = |p: Type, a: Type| {
            let mut s = HashMap::new();
            solve_param(&p, &a, &mut s);
            s.get("T").cloned()
        };

        // A record matches by field NAME, and a wider argument is still a match.
        assert_eq!(
            solved(
                Type::Record(vec![fld("v", t())]),
                Type::Record(vec![fld("other", Type::Bool), fld("v", Type::Str)]),
            ),
            Some(Type::Str),
        );
        // An enum matches by variant NAME, then payload-wise.
        assert_eq!(
            solved(
                Type::Enum(vec![var("Empty", vec![]), var("W", vec![t()])]),
                Type::Enum(vec![var("W", vec![Type::Int]), var("Empty", vec![])]),
            ),
            Some(Type::Int),
        );
        // `lazy T` in either spelling — RFC-0085 M4a says they are one type.
        assert_eq!(
            solved(Type::Lazy(Box::new(t())), Type::Lazy(Box::new(Type::Float))),
            Some(Type::Float),
        );
        assert_eq!(
            solved(
                Type::Lazy(Box::new(t())),
                Type::Fn(vec![], Box::new(Type::Float))
            ),
            Some(Type::Float),
        );
        assert_eq!(
            solved(Type::Task(Box::new(t())), Type::Task(Box::new(Type::Bool))),
            Some(Type::Bool),
        );
        // A different constructor still binds nothing — the rule's second half.
        assert_eq!(solved(Type::Task(Box::new(t())), Type::Int), None);
    }

    /// **Why the arms above were dead, and why filling them changed no
    /// program.**
    ///
    /// RFC-0086 deferred this list because "filling in `Lazy`/`Record`/`Enum`/
    /// `Task` turns some silent `Unit` into a real type and some refusal into a
    /// compile". Neither happened, and the reason was that the CHECKER refused
    /// all four shapes before codegen was asked: `Checker::unify` had the same
    /// list, and its fall-through is a diagnostic rather than a substitution.
    /// So `solve_param` never faced one, the corpus census counted zero, and no
    /// program's meaning moved.
    ///
    /// If any of these ever starts checking, this test fails and says so, and
    /// the arms it unblocks are already written and already tested. **`Task`
    /// has now done exactly that**, and it is off the list below.
    /// `Checker::unify` grew a `Task` arm when `@join`'s seeded row became the
    /// first signature that names one (RFC-0125 §3 M6, the `consume` slice), so
    /// `fn awaitOne<T>(t: Task<T>) -> T { return t.join() }` checks, and
    /// `solve_param`'s `Task` arm — written and asserted in the test above —
    /// binds `T`. The program answers 42 on the native route and on the wasm
    /// route. That is the promise this test was written to keep, kept.
    #[test]
    fn the_checker_refuses_every_shape_the_fall_through_used_to_swallow() {
        let cases: &[(&str, &str)] = &[
            // A structural record parameter naming the type parameter.
            (
                "Record",
                "type Box<T> = { value: T }\n\
                 fn unwrap<T>(b: { value: T }) -> T { return b.value }\n\
                 fn main() -> Int64 { let n = Box { value: 7 }\n return unwrap(n) }",
            ),
            // An enum variant whose payload is a record naming the parameter.
            (
                "Enum",
                "type Cell = { v: Int64 }\n\
                 type Wrap<T> = | Empty | W({ v: T })\n\
                 fn main() -> Int64 { let c = Cell { v: 7 }\n let w = W(c)\n return 0 }",
            ),
            // A generic record with a `lazy` field.
            (
                "Lazy",
                "type Holder<T> = { body: lazy T }\n\
                 fn seven() -> Int64 { return 7 }\n\
                 fn main() -> Int64 { let h: Holder<Int64> = Holder { body: () -> seven() }\n\
                 return h.body }",
            ),
            // (A `Task<T>` parameter stood here. It checks now — see the doc
            // comment — so it belongs to `a_generic_task_parameter_solves`
            // below rather than to this list.)
        ];
        for (what, src) in cases {
            assert!(
                check(src).is_err(),
                "{what}: the checker accepted this, so `solve_param` now faces it — \
                 see the arms above and RFC-0086's last open list"
            );
        }
    }

    /// The first shape to leave the list above. A generic function taking a
    /// `Task<T>` checks since RFC-0125 §3 M6 gave `Checker::unify` the `Task`
    /// arm `@join`'s seeded row needed, and `solve_param` binds `T` from the
    /// argument the way it binds one from an `Array`.
    #[test]
    fn a_generic_task_parameter_solves() {
        let src = "fn slow(n: Int64) -> Int64 { return n * 2 }\n\
                   fn awaitOne<T>(t: Task<T>) -> T { return t.join() }\n\
                   fn main() -> Int64 { let t = spawn slow(21)\n return awaitOne(t) }";
        assert!(check(src).is_ok(), "{:?}", check(src));
    }
}
