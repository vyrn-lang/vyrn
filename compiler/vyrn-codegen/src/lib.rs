//! The wasm emitter ([`direct`]) and the helpers and constants it shares with
//! the rest of the compiler: `llt_of`, the type-to-shape match that [`layout`]
//! measures; [`coerce_plan`], the boundary coercion ladder; generic type-argument
//! solving; the instantiation limits; the generator-host imports and constants;
//! and the [`observe`] hooks a gate reads.

pub mod direct;
pub mod layout;
pub mod toolchain;
pub mod wasm;

use std::collections::HashMap;

use vyrn_frontend::ast::*;
use vyrn_frontend::types::solve_param;

use layout::{Leaf, Shape};

/// Returns the index of the arm that tests a tag, and the tag, when a switch has
/// the `if let` shape: two arms, one testing a tag and one the default. Such a
/// switch lowers to a two-way branch. `tags` holds one entry per arm, `None` for
/// the default.
pub(crate) fn two_way(tags: &[Option<usize>]) -> Option<(usize, usize)> {
    match tags {
        [Some(t), None] => Some((0, *t)),
        [None, Some(t)] => Some((1, *t)),
        _ => None,
    }
}

/// The I/O error wording. The list lives in
/// [`vyrn_frontend::trap::IO`] so every engine reads one copy.
pub use vyrn_frontend::trap::{io as io_message, IO as IO_MESSAGES};

/// The time and randomness externs, which lower to a runtime call
/// at each use site, not to a host import.
pub use vyrn_frontend::trap::host_boundary_extern;

/// The wasm type one `extern` parameter or result crosses as: its leaf's
/// [`layout::Leaf::val_type`], or `None` for `Unit`. A `String` parameter
/// crosses as a `(ptr, len)` pair and is handled apart.
///
/// # Panics
///
/// On an aggregate: the checker's `extern_abi_type_ok` admits the scalars,
/// `String` and `Unit` only.
pub(crate) fn extern_abi(ty: &Type) -> Option<wasm::ValType> {
    match shape_of(ty, &HashMap::new()) {
        Shape::Void => None,
        Shape::Leaf(l) => Some(l.val_type()),
        s => unreachable!("the checker admits no `extern` of the shape {s:?}"),
    }
}

/// Records the types the emitter derives for expressions, the instances it
/// emits and the coercion rungs it takes, so a gate can compare them with the
/// checker's and `vyrn-lower`'s. Every hook records a decision already made.
/// Off (the default) it costs one thread-local read per expression; on, it grows
/// one row per typed expression per instantiation, so only gates turn it on.
pub mod observe {
    use vyrn_frontend::ast::Type;

    /// Which engine, and which of its derivations, produced a row.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Site {
        /// The wasm emitter: the instances it emits and the rungs it takes.
        Wasm,
        /// `Fn_::peek`, the wasm emitter's second expression typer.
        Peek,
    }

    /// One backend answer: this node, under this instantiation, has this type.
    #[derive(Debug, Clone)]
    pub struct Row {
        pub site: Site,
        pub kind: &'static str,
        /// The cloned tree the answer was given inside, or `""` for a node the
        /// program holds. `"lambda"`: `Fn_::lift_lambda` copies a lambda's body,
        /// so its nodes, and projection expansions built while walking it, are
        /// off-program. `"pred"`: a `where` predicate is cloned out of
        /// `types::decl_map` and again at each validation site.
        pub ctx: &'static str,
        pub node: vyrn_frontend::ast::NodeId,
        /// The instantiation the emitter was inside, sorted by parameter name.
        pub subst: Vec<(String, Type)>,
        pub ty: Type,
    }

    /// One body the emitter emitted: a function and its type arguments, for the
    /// gate that compares them with `vyrn-lower`'s instances. A lifted lambda is
    /// not recorded: it has no name, and its identity is the address of a cloned
    /// node the lowering cannot key against.
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
        static TYPINGS: std::cell::RefCell<Vec<Typing>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Start recording on this thread, discarding anything already collected.
    pub fn start() {
        ROWS.with(|r| r.borrow_mut().clear());
        INSTS.with(|r| r.borrow_mut().clear());
        CROSSINGS.with(|r| r.borrow_mut().clear());
        TYPINGS.with(|r| r.borrow_mut().clear());
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

    /// One boundary crossing an engine made: the type pair, which is its
    /// identity, and the rung it took.
    #[derive(Debug, Clone)]
    pub struct Crossing {
        pub site: Site,
        pub from: Type,
        pub to: Type,
        pub rung: crate::Rung,
    }

    /// Records the rung an engine took. Every `return` path of a `coerce` calls
    /// this once, so the corpus gate's floor sees a rung that stops being
    /// reachable.
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

    /// One right-hand side of the core the emitter typed: the type it derived
    /// from the callee or the operator, and the producer type the checker wrote
    /// on the row.
    #[derive(Debug, Clone)]
    pub struct Typing {
        /// `"call"` or `"prim"`.
        pub kind: &'static str,
        /// The callee, or the operator.
        pub what: String,
        pub got: Type,
        pub checker: Type,
    }

    /// Records one typing. The emitter calls this, behind [`on`], at every `Call`
    /// and `Prim` it emits whose row carries the checker's type.
    pub(crate) fn note_typing(kind: &'static str, what: &str, got: &Type, checker: &Type) {
        TYPINGS.with(|r| {
            r.borrow_mut().push(Typing {
                kind,
                what: what.to_string(),
                got: got.clone(),
                checker: checker.clone(),
            })
        });
    }

    /// The typings recorded since [`start`]. Read after [`take`], like
    /// [`take_insts`].
    pub fn take_typings() -> Vec<Typing> {
        TYPINGS.with(|r| std::mem::take(&mut *r.borrow_mut()))
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

    /// The expression kind a row is reported under; a variable's kind carries
    /// its name. Names are interned because [`Row`] holds a `&'static str`; the
    /// pool is bounded by the distinct variable names in one corpus, and only a
    /// recording gate calls this.
    pub fn kind_of(e: &vyrn_frontend::ast::Expr) -> &'static str {
        use vyrn_frontend::ast::Expr as E;
        match e {
            E::Int(_, _) => "int",
            E::Byte(_, _) => "byte",
            E::Float(_, _) => "float",
            E::Bool(_, _) => "bool",
            E::Str(_, _) => "str",
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
            E::Lambda { .. } => "lambda",
            E::Consume { .. } => "consume",
        }
    }

    /// One leaked `&'static str` per distinct string.
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
        node: vyrn_frontend::ast::NodeId,
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

/// Every `wasi_snapshot_preview1` call a module imports, with its witx signature, in import
/// order. `std/mem` names each in lowerCamelCase: `fd_write` is `fdWrite`.
///
/// Each host implements exactly this set, and a test on each side compares its set with this
/// table: `vyrn-cli/src/wasmrun.rs` for the embedded engine, `wasi_host.c` for the wasm2c
/// route. `std/mem` declares each call as a Vyrn function and `web/wasi-min.js` implements it
/// for the browser, degraded: no argv, EOF on stdin, no preopens, every `path_open` NOENT.
pub const WASI_IMPORTS: &[(&str, &[wasm::ValType], &[wasm::ValType])] = {
    use wasm::ValType::{I32, I64};
    &[
        ("fd_write", &[I32, I32, I32, I32], &[I32]),
        ("fd_read", &[I32, I32, I32, I32], &[I32]),
        ("fd_close", &[I32], &[I32]),
        ("proc_exit", &[I32], &[]),
        (
            "path_open",
            &[I32, I32, I32, I32, I32, I64, I64, I32, I32],
            &[I32],
        ),
        ("path_rename", &[I32, I32, I32, I32, I32, I32], &[I32]),
        ("fd_sync", &[I32], &[I32]),
        ("fd_prestat_get", &[I32, I32], &[I32]),
        ("args_sizes_get", &[I32, I32], &[I32]),
        ("args_get", &[I32, I32], &[I32]),
        ("environ_sizes_get", &[I32, I32], &[I32]),
        ("environ_get", &[I32, I32], &[I32]),
        ("clock_time_get", &[I32, I64, I32], &[I32]),
        ("random_get", &[I32, I32], &[I32]),
        // `listDir`'s entries, in the host's order; `list_dir` sorts them.
        ("fd_readdir", &[I32, I32, I32, I64, I32], &[I32]),
    ]
};

/// The `WASI_IMPORTS` name of a `std/mem` host primitive: `fdWrite` is `fd_write`.
pub(crate) fn wasi_snake(camel: &str) -> String {
    camel
        .chars()
        .flat_map(|c| {
            [
                c.is_ascii_uppercase().then_some('_'),
                Some(c.to_ascii_lowercase()),
            ]
        })
        .flatten()
        .collect()
}

/// Every `vyrn_gen` import a generator module makes: a signature in LLVM's
/// spelling and its import name. [`wasm::declare_sig`] turns each into a wasm
/// signature through [`wasm::abi`], so `i1`, `i8` and `ptr` widen in one place.
///
/// The host side of each is the interpreter's own code (the piece arena,
/// `render_code`, the splice table, the lexer, the linker), so escaping,
/// identifier validation and float formatting match by construction.
pub(crate) const CODE_IMPORTS: &[(&str, &str)] = &[
    ("i64 @__vyrn_code_text(ptr)", "text"),
    ("i64 @__vyrn_code_splice(i32, i64, ptr, i64)", "splice"),
    ("i64 @__vyrn_code_raw_at(ptr, ptr, i64, i64)", "rawAt"),
    ("i64 @__vyrn_code_concat(i64, i64)", "concat"),
    ("i64 @__vyrn_code_render(i64)", "render"),
    // `render` answers with a length and the guest allocates, then `fetch`
    // copies: the host must not allocate inside guest memory.
    ("void @__vyrn_gen_fetch(ptr)", "fetch"),
    // `reflect` asks the host for a value of a named type (`lex`,
    // `moduleInterface`, `contractOf`) as a flat atom stream; `nextInt` and
    // `nextStr` pull the atoms in the order the decoder walks the type.
    // `nextStr` answers with a length, as `render` does.
    ("void @__vyrn_gen_reflect(i64, ptr)", "reflect"),
    ("i64 @__vyrn_gen_next_int()", "nextInt"),
    ("i64 @__vyrn_gen_next_str()", "nextStr"),
    // The mediated read that serves `readFile`, `readFileBytes` and `listDir`.
    ("i64 @__vyrn_gen_read(ptr, i32)", "read"),
];

/// `@__vyrn_gen_reflect`'s kinds: which builtin the host answers. The argument
/// is the module path, the contract name, or the source to lex.
pub const REFLECT_MODULE_INTERFACE: i64 = 0;
pub const REFLECT_CONTRACT_OF: i64 = 1;
pub const REFLECT_LEX: i64 = 2;
pub const REFLECT_TYPE_ARG: i64 = 3;

pub use vyrn_frontend::checker::{GEN_ENTRY_LEX, GEN_ENTRY_MODULE_INTERFACE, GEN_ENTRY_TYPE_ARG};

/// The atom-stream primitives the synthesized decoders call. The checker
/// declares and types them on a generator host; the emitter lowers them and
/// `vyrn-genwasm` writes the decoders.
pub use vyrn_frontend::checker::{GEN_NEXT_INT, GEN_NEXT_STR, GEN_REFLECT};

/// `@__vyrn_code_splice`'s value tags: which interpreter `Val` the host rebuilds
/// from the word. Exactly the set `gen::gen_code_splice` accepts; `pub` so the
/// host reads this numbering.
pub const TAG_STR: i32 = 0;
pub const TAG_CODE: i32 = 1;
pub const TAG_BOOL: i32 = 2;
pub const TAG_INT: i32 = 3;
pub const TAG_UINT: i32 = 4;
pub const TAG_F64: i32 = 5;
pub const TAG_F32: i32 = 6;

/// The instantiation bounds. They live in `vyrn_frontend::types` because
/// `vyrn-lower`, below this crate, runs the same worklist.
pub use vyrn_frontend::types::{MONO_DEPTH_LIMIT, MONO_SIZE_LIMIT};

/// The phrase every instantiation-limit refusal contains. `vyrn check` matches
/// it to promote this one codegen error to a check failure.
pub const MONO_LIMIT_NEEDLE: &str = "past the instantiation limit";

/// The phrase every frame-size refusal contains, so a test pins the limit and
/// not its sentence.
pub const FRAME_LIMIT_NEEDLE: &str = "past the frame limit";

/// The phrase every statics-size refusal contains.
pub const STATICS_LIMIT_NEEDLE: &str = "past the statics limit";

/// Refuses an instantiation whose type arguments pass [`MONO_DEPTH_LIMIT`] or
/// [`MONO_SIZE_LIMIT`]. The message names the type, not the call chain: the type
/// is the chain written down (`P<P<P<..>>>`), and it is what the author changes.
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
            // The lexer accepts Unicode identifiers, and `truncate` panics
            // mid-character, so cut at a char boundary.
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

/// Runs `vyrn-lower`'s monomorphization and reports only its depth refusal, for
/// `vyrn check`; other codegen errors stay outside `check`'s contract.
/// `vyrn-cli/tests/lowered.rs` asserts over the corpus that every instance the
/// emitter builds is one the lowering's worklist has. `world` is `program`'s.
pub fn check_instantiations(program: &Program, world: &vyrn_lower::World) -> Result<(), String> {
    let types = vyrn_frontend::types::decl_map(program);
    for u in vyrn_lower::lower_with(program, &world.ownership).unresolved {
        if u.why == vyrn_lower::Why::PastTheLimit {
            check_inst_depth(&u.callee, u.args.iter(), u.line, &types)?;
        }
    }
    Ok(())
}

/// Deep-normalizes a stored-fn signature so structurally identical
/// spellings (an alias, a validated scalar, transformer sugar) register and
/// dispatch as one synthesized enum. It decides which constructions a dispatcher
/// covers; a second grouping would leave a dispatcher missing a variant.
pub(crate) fn normalize_fn_sig(t: &Type, types: &HashMap<String, TypeDecl>) -> Type {
    let norm = |x: &Type| normalize_fn_sig(x, types);
    match vyrn_frontend::types::resolve(t, types) {
        Type::Fn(ps, r) => Type::Fn(ps.iter().map(norm).collect(), Box::new(norm(&r))),
        Type::Array(i) => Type::Array(Box::new(norm(&i))),
        Type::ArrayN(i, n) => Type::ArrayN(Box::new(norm(&i)), n),
        Type::Map(k, v) => Type::Map(Box::new(norm(&k)), Box::new(norm(&v))),
        // Every sum, `Option` and `Result` included. Payloads normalize too, or
        // a sum registers under a spelling the dispatcher does not look up.
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

/// Bjoern Hoehrmann's UTF-8 validation DFA: 256 byte-class entries, then a
/// 108-entry (9 states x 12 classes) transition table. State 0 accepts, 12
/// rejects. The wasm emitter puts it in a data segment for `@__vyrn_utf8valid`,
/// which rejects what `String::from_utf8` rejects (overlong forms, surrogates,
/// code points above U+10FFFF).
///
/// The table is his, byte for byte, and the one piece of third-party code in
/// this repository. His MIT terms require the notice to travel with every copy,
/// including the binaries this emits it into:
///
/// ```text
/// Copyright (c) 2008-2009 Bjoern Hoehrmann <bjoern@hoehrmann.de>
/// See http://bjoern.hoehrmann.de/utf-8/decoder/dfa/ for details.
/// ```
///
/// The full notice is in `THIRD-PARTY-NOTICES.md`, which the release archive
/// ships.
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

/// The type arguments of a generic call, with any parameter the arguments leave
/// open taken from the type the call site expects. `fn newSlots<T>() -> Slots<T>`
/// has no argument to read `T` from; the checker answers from the expected type,
/// so this must too, or the two disagree about which instance the program calls.
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

/// The type arguments a call or construction site instantiates a generic with:
/// `declared` are the parametric types (parameters, variant payloads, record
/// fields), `actual` the concrete types supplied. Each type parameter comes back
/// `Some` if the match fixed it, `None` if not. The caller decides what `None`
/// means; the wasm emitter refuses, because a `void` in a wasm signature makes a
/// different function.
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
/// parameter becomes `Unit`.
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

/// Whether a field's value can settle a type parameter. An empty `[]` or `[:]`
/// reports a placeholder element type, so it settles nothing: in
/// `Deque { back: ["z"], front: [] }`, settling from `front` would bind
/// `T = Int64` and store a `String` pointer into an `i64` element. The checker
/// agrees by another road: there `[]` is `Array<T>`, and a parameter bound to
/// itself is dropped.
pub(crate) fn settles_type_args(e: &Expr) -> bool {
    !matches!(e, Expr::ArrayLit { elems, .. } if elems.is_empty())
        && !matches!(e, Expr::MapLit { entries, .. } if entries.is_empty())
}

/// The type arguments a construction site's expected type settles.
///
/// A stored `fn` field registers its dispatch variant against the
/// type being built, so its arguments must be known before any field is read;
/// solved from field values alone, the variant lands under a signature no
/// dispatcher covers. A `Unit` placeholder or an open `Param` settles nothing,
/// and the value-side solve keeps those.
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
/// must satisfy, if any. Stated in `vyrn-frontend` so the interpreter
/// reads the same rule.
pub(crate) use vyrn_frontend::validate::required as validation_required;

/// One rung of the boundary ladder: how a value crosses into a declared type.
/// [`coerce_plan`] picks the rung and the emitter's `coerce` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// A `Never` reaching a boundary: the `panic` already left, so
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
    /// One sum at another shape of itself: the same variant names, a different
    /// slot count (`Validation<Unit>` and `Validation<Point>`, or a bare `None`
    /// built at the narrowest `Option`). The tag and the shared slots move; the
    /// rest is zero.
    Reshape,
    /// The bits are already right.
    Identity,
    /// No rung handles this pair.
    Refuse,
}

/// The variant names of a sum in tag order, or `None` for any other type.
/// [`coerce_plan`] compares them to decide whether two sums are one sum.
pub(crate) fn sum_variants(ty: &Type) -> Option<Vec<String>> {
    match ty {
        Type::Enum(vs) => Some(vs.iter().map(|v| v.name.clone()).collect()),
        _ => None,
    }
}

/// The variants of a sum in tag order, or `None` for any other type. `Option<T>`
/// resolves to `| None | Some(T)` and `Result<T, E>` to `| Err(E) | Ok(T)`.
pub(crate) fn sum_variants_of(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
) -> Option<Vec<EnumVariant>> {
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Enum(vs) => Some(vs),
        _ => None,
    }
}

/// The error when a `coerce` arm cannot destructure the pair [`coerce_plan`]
/// sent it: the plan and the arm have come apart. One sentence for every arm.
pub(crate) fn plan_disagrees(from: &Type, to: &Type, rung: Rung) -> String {
    format!("the coercion plan placed {rung:?} for `{from}` into `{to}` and the emitter's arm for it cannot take that pair")
}

/// The rung a value crossing from `from` into `to` takes.
///
/// The interpreter's `coerce` has no `from`, so it is not held to this plan.
/// The middle rungs' guards are disjoint except for an integer pair that shares
/// one LLVM shape (`i8` for `Int8` and `UInt8`), which is why the resize comes
/// before [`Rung::Identity`].
pub fn coerce_plan(from: &Type, to: &Type, types: &HashMap<String, TypeDecl>) -> Rung {
    let (rf, rt) = (
        vyrn_frontend::types::resolve(from, types),
        vyrn_frontend::types::resolve(to, types),
    );
    // Integers compare by resolved spelling: `Int` and `Int64` are one type,
    // and that pair needs no rung.
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
    // Two shapes of one sum. The variant names decide, so a generic enum at two
    // instantiations qualifies and two different enums of one width do not.
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

/// The shape of a Vyrn type, in LLVM's spelling, which the equality sites
/// compare. `ty` is resolved here but not substituted; a caller inside a
/// monomorphized body substitutes first.
pub(crate) fn llt_of(ty: &Type, types: &HashMap<String, TypeDecl>) -> String {
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Int => "i64".into(),
        Type::IntN { bits, .. } => format!("i{bits}"),
        Type::Float => "double".into(),
        Type::Float32 => "float".into(),
        // The wasm emitter reads the vector spellings back to reach `v128`.
        Type::F32x4 => "<4 x float>".into(),
        // `I32x4` and `Mask32x4` share one representation, so an `I32x4`
        // comparison yields a `Mask32x4` with no conversion.
        Type::I32x4 => "<4 x i32>".into(),
        // A mask is all-ones or all-zeros at the lane width, not `<N x i1>`.
        Type::Mask32x4 => "<4 x i32>".into(),
        Type::F64x2 => "<2 x double>".into(),
        Type::Mask64x2 => "<2 x i64>".into(),
        Type::Bool => "i1".into(),
        Type::Str => "ptr".into(),
        // `Never` carries no value, so it lowers like `Unit`.
        Type::Unit | Type::Never => "void".into(),
        // `{ ptr data, i64 len, i64 cap }`.
        Type::Array(_) => "{ ptr, i64, i64 }".into(),
        // `{ ptr data, i64 len, i64 tag, i64 pay, i64 cur, i64 gen }`. A
        // negative `tag` is a buffer: `data`/`len` are the array and `cur` the
        // read position. Otherwise it is a step: `tag`/`pay` are a `fn` value,
        // `cur`/`gen` the `Ref<Int64>` cursor it is called with, `len` is 1 once
        // the step answers `None`, and `data` is null. The pairs sit 8-aligned so
        // `&s + 16` and `&s + 32` are those values. Nothing reads a field whose
        // variant it has not tested.
        Type::Stream(_) => "{ ptr, i64, i64, i64, i64, i64 }".into(),
        // `{ ptr keys, ptr values, i64 len, i64 cap, ptr idx }`: two parallel
        // buffers sharing one length and capacity; `idx` is `cap * 2` `i64` hash
        // buckets.
        Type::Map(..) => "{ ptr, ptr, i64, i64, ptr }".into(),
        Type::ArrayN(inner, n) => format!("[{n} x {}]", llt_of(&inner, types)),
        // `{ i64 len, i64 cap, ptr data, [N x T] inline }`: `cap == N` means
        // inline, `cap > N` spilled onto `data`.
        Type::SmallArray(inner, n) => {
            format!("{{ i64, i64, ptr, [{n} x {}] }}", llt_of(&inner, types))
        }
        // A logger handle is a `ptr` to its name string.
        Type::Logger => "ptr".into(),
        Type::Record(fields) => {
            let inner: Vec<String> = fields.iter().map(|f| llt_of(&f.ty, types)).collect();
            format!("{{ {} }}", inner.join(", "))
        }
        // `{ i64 tag, i64 slot0, ... }`: one slot per payload word of the widest
        // variant, so a two-word payload rides inline, not in a heap box.
        Type::Enum(ref vs) => enum_ll(enum_slots_of(vs, types)),
        // On a generator host, `Code` is an opaque `i64` handle into the host's
        // piece arena: the one `Named` that survives `resolve` undeclared.
        Type::Named(ref n) if n == "Code" => "i64".into(),
        // Unreachable after `resolve` (Named/App/transformers/params reduced away).
        Type::Named(_)
        | Type::App(..)
        | Type::Omit(..)
        | Type::Pick(..)
        | Type::Merge(..)
        | Type::Partial(..)
        | Type::Param(_) => "void".into(),
        // A bare integer type argument never stands alone; `SmallArray` consumes
        // it before lowering.
        Type::ConstInt(_) => "void".into(),
        // A stored function value: `{ i64 tag, i64 payload }`. The tag
        // selects the named function or lifted lambda; the payload is 0 or a
        // pointer to the malloc'd capture block.
        Type::Fn(..) => "{ i64, i64 }".into(),
        // Unreachable: `resolve` answers `Fn([], T)` for a `lazy T` field.
        Type::Lazy(_) => "{ i64, i64 }".into(),
        // The checker's recovery sentinel; a program with an `Err` has
        // diagnostics and never reaches codegen.
        Type::Err => "void".into(),
    }
}

/// The shape of a sum with `slots` payload words: `{ i64 }` for 0,
/// `{ i64, i64 }` for 1, and so on.
fn enum_ll(slots: usize) -> String {
    let mut s = String::from("{ i64");
    for _ in 0..slots {
        s.push_str(", i64");
    }
    s.push_str(" }");
    s
}

/// A payload's word count, stated in [`vyrn_frontend::types`] because `own`
/// asks the same question of the same types.
pub(crate) use vyrn_frontend::types::payload_words as payload_words_of;

/// The shape of a Vyrn type: the one match from a type to a memory layout.
/// `ty` is resolved here but not substituted; a caller inside a monomorphized
/// body substitutes first.
pub(crate) fn shape_of(ty: &Type, types: &HashMap<String, TypeDecl>) -> Shape {
    let leaf = Shape::Leaf;
    let words = |n: usize| vec![leaf(Leaf::I64); n];
    let st = Shape::Struct;
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Int => leaf(Leaf::I64),
        Type::IntN { bits, .. } => leaf(match bits {
            8 => Leaf::I8,
            16 => Leaf::I16,
            32 => Leaf::I32,
            64 => Leaf::I64,
            _ => unreachable!("the frontend builds `IntN` at 8, 16, 32 and 64 bits only"),
        }),
        Type::Float => leaf(Leaf::F64),
        Type::Float32 => leaf(Leaf::F32),
        // `I32x4` and `Mask32x4` share one representation, so an `I32x4`
        // comparison yields a `Mask32x4` with no conversion.
        Type::F32x4 | Type::I32x4 | Type::Mask32x4 | Type::F64x2 | Type::Mask64x2 => {
            leaf(Leaf::V128)
        }
        Type::Bool => leaf(Leaf::I1),
        // A logger handle is a pointer to its name string.
        Type::Str | Type::Logger => leaf(Leaf::Ptr),
        // `Never` carries no value, so it lowers like `Unit`.
        Type::Unit | Type::Never => Shape::Void,
        // `{ ptr data, i64 len, i64 cap }`.
        Type::Array(_) => st(vec![leaf(Leaf::Ptr), leaf(Leaf::I64), leaf(Leaf::I64)]),
        // `{ ptr data, i64 len, i64 tag, i64 pay, i64 cur, i64 gen }`. A
        // negative `tag` is a buffer: `data`/`len` are the array and `cur` the
        // read position. Otherwise it is a step: `tag`/`pay` are a `fn` value,
        // `cur`/`gen` the `Ref<Int64>` cursor it is called with, `len` is 1 once
        // the step answers `None`, and `data` is null. The pairs sit 8-aligned so
        // `&s + 16` and `&s + 32` are those values. Nothing reads a field whose
        // variant it has not tested.
        Type::Stream(_) => st([vec![leaf(Leaf::Ptr)], words(5)].concat()),
        // `{ ptr keys, ptr values, i64 len, i64 cap, ptr idx }`: two parallel
        // buffers sharing one length and capacity; `idx` is `cap * 2` `i64` hash
        // buckets.
        Type::Map(..) => st(vec![
            leaf(Leaf::Ptr),
            leaf(Leaf::Ptr),
            leaf(Leaf::I64),
            leaf(Leaf::I64),
            leaf(Leaf::Ptr),
        ]),
        Type::ArrayN(inner, n) => Shape::Array(n, Box::new(shape_of(&inner, types))),
        // `{ i64 len, i64 cap, ptr data, [N x T] inline }`: `cap == N` means
        // inline, `cap > N` spilled onto `data`.
        Type::SmallArray(inner, n) => st(vec![
            leaf(Leaf::I64),
            leaf(Leaf::I64),
            leaf(Leaf::Ptr),
            Shape::Array(n, Box::new(shape_of(&inner, types))),
        ]),
        Type::Record(fields) => st(fields.iter().map(|f| shape_of(&f.ty, types)).collect()),
        // `{ i64 tag, i64 slot0, ... }`: one slot per payload word of the widest
        // variant, so a two-word payload rides inline, not in a heap box.
        Type::Enum(ref vs) => st(words(1 + enum_slots_of(vs, types))),
        // On a generator host, `Code` is an opaque `i64` handle into the host's
        // piece arena: the one `Named` that survives `resolve` undeclared.
        Type::Named(ref n) if n == "Code" => leaf(Leaf::I64),
        // Unreachable after `resolve` (Named/App/transformers/params reduced
        // away). A bare integer type argument never stands alone: `SmallArray`
        // consumes it. `Err` is the checker's recovery sentinel, and a program
        // with an `Err` has diagnostics and never reaches codegen.
        Type::Named(_)
        | Type::App(..)
        | Type::Omit(..)
        | Type::Pick(..)
        | Type::Merge(..)
        | Type::Partial(..)
        | Type::Param(_)
        | Type::ConstInt(_)
        | Type::Err => Shape::Void,
        // A stored function value: `{ i64 tag, i64 payload }`. The tag selects
        // the named function or lifted lambda; the payload is 0 or a pointer to
        // the malloc'd capture block. `resolve` answers `Fn([], T)` for a
        // `lazy T` field, so the `Lazy` arm is unreachable.
        Type::Fn(..) | Type::Lazy(_) => st(words(2)),
    }
}

/// The slots one variant's payloads occupy, laid out consecutively.
fn variant_slots_of(payload: &[Type], types: &HashMap<String, TypeDecl>) -> usize {
    payload.iter().map(|p| payload_words_of(p, types)).sum()
}

/// A sum's slot count: the widest variant's.
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

    /// Written out rather than derived, so a change to `shape_of` or to the
    /// layout arithmetic has to disagree with numbers a person wrote down.
    #[test]
    fn each_runtime_shape_has_the_layout_written_down() {
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
        let int = |bits, signed| Type::IntN { bits, signed };
        let u8t = int(8, false);
        let fn0 = || Type::Fn(Vec::new(), Box::new(Type::Int));
        let cases: Vec<(Type, u32, u32, &[u32])> = vec![
            (Type::Int, 8, 8, &[]),
            (int(32, true), 4, 4, &[]),
            (int(16, true), 2, 2, &[]),
            (u8t.clone(), 1, 1, &[]),
            (Type::Bool, 1, 1, &[]),
            (Type::Float, 8, 8, &[]),
            (Type::Float32, 4, 4, &[]),
            (Type::Str, 4, 4, &[]),
            (Type::F32x4, 16, 16, &[]),
            (Type::Unit, 0, 1, &[]),
            // The built-in sums: an `i64` tag plus one slot per payload word.
            (Type::option(Type::Int), 16, 8, &[0, 8]),
            (Type::result(Type::Int, Type::Str), 16, 8, &[0, 8]),
            (Type::option(fn0()), 24, 8, &[0, 8, 16]),
            // A 4-byte hole after the data pointer.
            (Type::Array(Box::new(Type::Str)), 24, 8, &[0, 8, 16]),
            (
                Type::Stream(Box::new(Type::Int)),
                48,
                8,
                &[0, 8, 16, 24, 32, 40],
            ),
            // The trailing index pointer pads the size to 32, not 28.
            (
                Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
                32,
                8,
                &[0, 4, 8, 16, 24],
            ),
            (fn0(), 16, 8, &[0, 8]),
            (rec(&[]), 0, 1, &[]),
            (
                rec(&[Type::Bool, Type::Str, Type::Int, u8t.clone(), Type::Float]),
                32,
                8,
                &[0, 4, 8, 16, 24],
            ),
            // A vector's 16-alignment moves it to offset 16 and the size to 32.
            (rec(&[u8t.clone(), Type::F64x2]), 32, 16, &[0, 16]),
            (Type::ArrayN(Box::new(Type::I32x4), 3), 48, 16, &[]),
            (Type::ArrayN(Box::new(Type::Int), 4), 32, 8, &[]),
            (Type::ArrayN(Box::new(u8t.clone()), 3), 3, 1, &[]),
            (
                Type::SmallArray(Box::new(Type::Int), 4),
                56,
                8,
                &[0, 8, 16, 24],
            ),
            // The inline buffer ends at 23, and the tail pads to 24.
            (Type::SmallArray(Box::new(u8t), 3), 24, 8, &[0, 8, 16, 20]),
            (
                Type::SmallArray(Box::new(Type::Str), 2),
                32,
                8,
                &[0, 8, 16, 20],
            ),
        ];
        for (ty, size, align, fields) in cases {
            let l = shape_of(&ty, &types).layout().unwrap();
            assert_eq!(
                (l.size, l.align, &l.fields[..]),
                (size, align, fields),
                "the layout of `{ty}`"
            );
        }
    }

    /// `536870912 * 8` is 2^32, which wraps to zero: a 4 GiB array that would
    /// pass the frame limit as needing no bytes.
    #[test]
    fn a_shape_past_four_gigabytes_is_refused_rather_than_wrapped() {
        let types = HashMap::new();
        let i64s = |n| Type::ArrayN(Box::new(Type::Int), n);
        let byte = Type::IntN {
            bits: 8,
            signed: true,
        };
        let field = |name: &str, ty| Field {
            name: name.into(),
            ty,
        };
        for (ty, wrapped) in [
            (i64s(600_000_000), 505_032_704u64),
            (i64s(536_870_912), 0),
            (
                Type::ArrayN(Box::new(i64s(100_000)), 100_000),
                2_690_588_672,
            ),
            (
                Type::Record(vec![field("b", byte), field("a", i64s(600_000_000))]),
                505_032_712,
            ),
        ] {
            let e = shape_of(&ty, &types)
                .layout()
                .expect_err(&format!("`{ty}` wrapped to {wrapped} instead"));
            assert!(
                e.contains("bytes, past the 4294967295 one shape may occupy")
                    && e.contains("belongs on the heap as `Array<T>`"),
                "`{ty}`: {e}"
            );
        }
        // The largest shape that fits keeps its exact size.
        let l = shape_of(&i64s(536_870_911), &types).layout().unwrap();
        assert_eq!(l.size, 4_294_967_288);
    }

    /// One `Type` per variant of the type enum. [`Type::VARIANTS`] and the
    /// exhaustive match in [`Type::variant_name`] hold it complete.
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
            Type::Logger,
            Type::Fn(vec![Type::Int], b(Type::Unit)),
            Type::Lazy(b(Type::Int)),
            Type::Never,
            Type::Err,
        ]
    }

    /// Three wrappers per type that [`grow`] lacks: a one-field record (the
    /// member's alignment), an `i8`-then-`t` record (the hole in front of it),
    /// and a one-payload enum.
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

    /// Over a few thousand type trees built from [`layout_seeds`] by [`grow`]
    /// and [`in_records`]: every tree has a padded layout, and every [`Leaf`]
    /// is some tree's, so no leaf is dead.
    #[test]
    fn every_type_has_a_padded_layout_and_every_leaf_is_reached() {
        let types = HashMap::new();
        let seeds = layout_seeds();
        // `variant_name` is exhaustive, so a new variant stops this file
        // compiling; `VARIANTS` makes a named but unseeded variant fail the test.
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

        // Exhaustive, so a new leaf has to be given a slot here.
        let slot = |l: Leaf| match l {
            Leaf::I1 => 0,
            Leaf::I8 => 1,
            Leaf::I16 => 2,
            Leaf::I32 => 3,
            Leaf::I64 => 4,
            Leaf::Ptr => 5,
            Leaf::F32 => 6,
            Leaf::F64 => 7,
            Leaf::V128 => 8,
        };
        fn leaves(s: &Shape, out: &mut dyn FnMut(Leaf)) {
            match s {
                Shape::Void => {}
                Shape::Leaf(l) => out(*l),
                Shape::Struct(ms) => ms.iter().for_each(|m| leaves(m, out)),
                Shape::Array(_, e) => leaves(e, out),
            }
        }
        let mut reached = [false; 9];
        for ty in &all {
            let s = shape_of(ty, &types);
            let l = s
                .layout()
                .unwrap_or_else(|e| panic!("`{ty}` has no layout: {e}"));
            assert!(l.align.is_power_of_two(), "`{ty}`: align {}", l.align);
            assert_eq!(l.size % l.align, 0, "`{ty}`: size {} is not padded", l.size);
            leaves(&s, &mut |l| reached[slot(l)] = true);
        }
        assert_eq!(reached, [true; 9], "a leaf no type reaches");
        assert!(all.len() > 4_000, "the corpus shrank to {}", all.len());
    }

    /// Exercises the `Record`, `Enum` and `Lazy` arms of [`solve_param`]
    /// directly. The checker refuses the first two shapes (next test); a
    /// generic `lazy` field reaches the third.
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
        // `lazy T` in either spelling; the two are one type.
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
    }

    /// The checker refuses the record and enum shapes those arms handle
    /// (`Checker::unify`'s fall-through is a diagnostic), so `solve_param`
    /// never faces one. If one starts checking, this fails.
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
        ];
        for (what, src) in cases {
            assert!(
                check(src).is_err(),
                "{what}: the checker accepted this, so `solve_param` now faces it — \
                 see the arms above"
            );
        }
    }

    /// `std/mem` declares each `WASI_IMPORTS` call, with the table's signature, and no other:
    /// the checker reads these declarations and the emitter reads the table. A declaration is a
    /// wasi call when its doc line opens with the call's name and a parenthesis.
    #[test]
    fn std_mem_declares_exactly_the_wasi_calls() {
        use crate::wasm::ValType::{self, I32, I64};
        use std::collections::BTreeMap;
        let ty = |t: &str| match t.trim() {
            "Int32" => I32,
            "Int64" => I64,
            t => panic!("`{t}` is not a wasi type"),
        };
        let mut declared: BTreeMap<String, (Vec<ValType>, Vec<ValType>)> = BTreeMap::new();
        let mut doc = None;
        for line in include_str!("../../../std/mem.vyrn").lines() {
            if let Some(d) = line.strip_prefix("/// `") {
                let name = d.split_once('(').map(|(n, _)| n);
                doc = name.filter(|n| {
                    n.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
                });
            }
            let Some(sig) = line.strip_prefix("export fn ") else {
                continue;
            };
            let Some(wasi) = doc.take() else { continue };
            let (name, sig) = sig.split_once('(').expect("a signature");
            let (params, ret) = sig.split_once(')').expect("a signature");
            assert_eq!(wasi_snake(name), wasi, "`{name}` is documented as `{wasi}`");
            let params = params.split(',').filter(|p| !p.is_empty());
            let params = params.map(|p| ty(p.split_once(':').expect("a parameter").1));
            let ret = ret.trim_end_matches(" {").strip_prefix(" -> ").map(ty);
            declared.insert(
                wasi.to_string(),
                (params.collect(), ret.into_iter().collect()),
            );
        }
        let want: BTreeMap<_, _> = WASI_IMPORTS
            .iter()
            .map(|(n, p, r)| (n.to_string(), (p.to_vec(), r.to_vec())))
            .collect();
        assert_eq!(declared, want);
    }

    /// `web/wasi-min.js` implements every `WASI_IMPORTS` call. It also implements calls the
    /// table omits (`fd_seek`, `fd_fdstat_get`): wasi-libc's C route imports them and no module
    /// the direct backend emits does.
    #[test]
    fn the_browser_host_implements_every_wasi_call() {
        let js = include_str!("../../../web/wasi-min.js");
        let (_, object) = js
            .split_once(
                "  const wasi = {
",
            )
            .expect("the wasi object");
        let (object, _) = object
            .split_once(
                "
  };",
            )
            .expect("the wasi object");
        let keys: Vec<&str> = object
            .lines()
            .filter_map(|l| l.strip_prefix("    "))
            .map(|l| l.split([',', ':']).next().unwrap_or_default())
            .collect();
        for (name, ..) in WASI_IMPORTS {
            assert!(
                keys.contains(name),
                "web/wasi-min.js does not implement `{name}`"
            );
        }
    }
}
