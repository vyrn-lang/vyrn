//! Type checker. Checks signatures, names, operand types, `mut` assignment,
//! calls and all-paths return. For a validated type it also checks
//! each predicate, refuses a constant construction that fails it, and refuses a
//! raw base value where the validated type is expected.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::ast::*;
use crate::consteval;
use crate::diagnostics::Diagnostic;
use crate::types::mentions_param as type_mentions_param;
use crate::types::walk_type;
use crate::types::FALLIBLE;

/// A checker error on a whole line (column 0): most AST nodes carry no column.
/// The message omits the `"line {N}: "` prefix; [`Diagnostic::render`] adds it.
macro_rules! cerr {
    ($line:expr, $($arg:tt)*) => {
        $crate::diagnostics::Diagnostic::error(
            $crate::checker::line_of($line),
            0,
            "check",
            format!($($arg)*),
        )
    };
}

/// A checker error at a column span. `$span` is `(col, end_col)`, 1-based;
/// `(0, 0)`, from a synthesized declaration, means the whole line.
macro_rules! cerr_at {
    ($line:expr, $span:expr, $($arg:tt)*) => {{
        let (col, end_col) = $span;
        let mut d = cerr!($line, $($arg)*);
        d.col = col;
        d.end_col = end_col;
        d
    }};
}

/// The line a `cerr!` was given, by value: a node matched by reference hands
/// out `&usize`, and this accepts both.
pub(crate) fn line_of(l: impl std::borrow::Borrow<usize>) -> usize {
    *l.borrow()
}

thread_local! {
    /// Whether this thread checks a generator host: the program the engine
    /// compiles to run a `gen fn` as wasm. Its functions have
    /// `is_gen` cleared, so this flag marks the whole program as generation
    /// code. It only enables generation-only names; nothing reads it to refuse.
    static GEN_HOST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks this thread as checking a generator host, or stops. Called by
/// `vyrn_codegen::set_gen_host`, so the checker and the emitter read one flag.
pub fn set_gen_host(on: bool) {
    GEN_HOST.with(|g| g.set(on));
}

/// Whether this thread is checking a generator host. Part of [`recorded`]'s
/// key and of `vyrn_lower::core::decide`'s, because it changes what a check
/// decides.
pub fn gen_host() -> bool {
    GEN_HOST.with(|g| g.get())
}

thread_local! {
    /// Whether this thread checks a test host: the program `vyrn test` and
    /// `vyrn bench` compile, whose functions are lifted `test` and `bench`
    /// bodies. It only enables test-only names such as `assert`.
    static TEST_HOST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks this thread as checking a test host, or stops.
pub fn set_test_host(on: bool) {
    TEST_HOST.with(|t| t.set(on));
}

/// Whether this thread is checking a test host. Part of [`recorded`]'s key.
pub fn test_host() -> bool {
    TEST_HOST.with(|t| t.get())
}

/// Whether a body is checked as generation code: its own `gen fn` marker, or a
/// whole-program generator host.
fn in_gen_of(f: &Function) -> bool {
    f.is_gen || GEN_HOST.with(|g| g.get())
}

/// The atom-stream primitives a generator host's decoders call: one starts a
/// reflected answer, two take the next atom. `vyrn-codegen` lowers them. They
/// exist only under [`set_gen_host`], so no program can name them.
pub const GEN_REFLECT: &str = "__vyrnGenReflect";
pub const GEN_NEXT_INT: &str = "__vyrnGenNextInt";
pub const GEN_NEXT_STR: &str = "__vyrnGenNextStr";

/// The generator-host entry points the engine appends as ordinary functions:
/// each asks the host for a value and decodes it by its static type. A
/// builtin's call site is redirected to one.
pub const GEN_ENTRY_MODULE_INTERFACE: &str = "__vyrnGenModuleInterface";
pub const GEN_ENTRY_LEX: &str = "__vyrnGenLex";

/// The nullary entry `contractOf(contract)` calls, one per contract.
pub fn gen_entry_contract_of(contract: &str) -> String {
    format!("__vyrnGenContractOf_{contract}")
}

/// The type a gen-host primitive returns at that arity.
fn gen_host_primitive(name: &str, argc: usize) -> Option<Type> {
    if !GEN_HOST.with(|g| g.get()) {
        return None;
    }
    match (name, argc) {
        (GEN_REFLECT, 2) => Some(Type::Unit),
        (GEN_NEXT_INT, 0) => Some(Type::Int),
        (GEN_NEXT_STR, 0) => Some(Type::Str),
        _ => None,
    }
}

/// A local binding in one function body, with the type the check decided.
/// The editor indexes these for hover, definition, completion and highlight.
#[derive(Debug, Clone)]
pub struct LocalBinding {
    pub name: String,
    pub kind: LocalKind,
    /// What the check decided, else what the source declares. `None` for an
    /// unannotated `let` in a body the check did not reach.
    pub ty: Option<Type>,
    /// 1-based line of the name.
    pub line: usize,
    /// 1-based name column.
    pub col: usize,
    /// 1-based name end column.
    pub end_col: usize,
    /// The enclosing function's declaration line.
    pub fn_line: usize,
}

pub use crate::ast::LocalKind;

crate::body_scope_descent!(BinderIndex, index_block, index_stmt, index_expr);

/// The editor's local index for one function body. A binder's position is
/// the whole answer, so the scope stack is off.
struct LocalIndex<'a> {
    types: &'a HashMap<(usize, usize), Type>,
    fn_line: usize,
    out: &'a mut Vec<LocalBinding>,
}

impl LocalIndex<'_> {
    fn row(
        &mut self,
        name: &str,
        kind: LocalKind,
        line: usize,
        col: usize,
        declared: Option<&Type>,
    ) {
        // A desugar's binder has no column; a phantom local is worse than none.
        if col == 0 {
            return;
        }
        self.out.push(LocalBinding {
            name: name.to_string(),
            kind,
            // A body the check never reached decided nothing; fall back to
            // the declared type.
            ty: self
                .types
                .get(&(line, col))
                .cloned()
                .or_else(|| declared.cloned()),
            line,
            col,
            end_col: col + name.chars().count(),
            fn_line: self.fn_line,
        });
    }
}

impl BinderIndex<'_> for LocalIndex<'_> {
    const SCOPED: bool = false;

    fn bind(
        &mut self,
        name: &str,
        line: usize,
        col: usize,
        kind: LocalKind,
        declared: Option<&Type>,
    ) {
        self.row(name, kind, line, col, declared);
    }
}

/// Every binding the root module's functions and impl methods make, in source
/// order. `types` is what the check decided, keyed by binder position.
///
/// Projection bodies and `test` blocks are skipped: `symbols::fn_lines` does
/// not scope a cursor to them.
pub(crate) fn local_index(
    program: &Program,
    types: &HashMap<(usize, usize), Type>,
) -> Vec<LocalBinding> {
    let mut out = Vec::new();
    let methods = program.impls.iter().flat_map(|i| i.methods.iter());
    for f in program.functions.iter().chain(methods) {
        if f.module.is_some() {
            continue;
        }
        let mut v = LocalIndex {
            types,
            fn_line: f.line,
            out: &mut out,
        };
        for p in &f.params {
            v.row(&p.name, LocalKind::Param, p.line, p.col, Some(&p.ty));
        }
        index_block(&f.body, &mut HashSet::new(), &mut v);
    }
    // One row per position. An impl method `parse_accum` flattened into
    // `functions` is walked twice, and a refutable `let` binds the
    // same token as its `match` arm. The stable sort keeps the arm's row,
    // pushed first, which is right: a pattern binder is never `mut`.
    out.sort_by_key(|b| (b.fn_line, b.line, b.col));
    out.dedup_by_key(|b| (b.line, b.col));
    out
}

/// Type-check the program, returning **all** problems found across functions
/// and types as structured [`Diagnostic`]s, plus a table of the (inferred or
/// declared) type of each `let` binding and `for`-in loop variable that
/// checked cleanly — keyed by `(line, name)`. The symbol-query layer uses that
/// table to show `let x: Int` on hover for an unannotated `let x = 5` (the
/// checker computes the type either way; this just retains it).
///
/// Accumulation is bounded: the top-level loops over `program.functions` and
/// `program.type_decls` push-and-continue, so an error in one function or type
/// does not suppress errors in the others. Inside a single function body the
/// check is still first-error (recovery there is the same class of work as
/// parser recovery, and is deferred).
pub fn check_accum_with_binders(program: &Program) -> (Vec<Diagnostic>, Vec<LocalBinding>) {
    let (out, binders, _, _, _, _) = check_accum_full(program);
    (out, binders)
}

/// Returns the stored-function-value collection the `--workers`
/// gate needs. Diagnostics are discarded: callers have already checked.
pub fn stored_fn_effects(program: &Program) -> StoredFnEffects {
    check_accum_full(program).2
}

/// Names the compiler owns: builtin functions, builtin type names and the sum
/// constructors. A top-level declaration may not take one. The loader reads it
/// too, so a user `fn at` does not claim the builtin `at` in every `std/` module.
pub const RESERVED: &[&str] = &[
    "print",
    "len",
    "concat",
    "Some",
    "None",
    "Ok",
    "Err",
    "match",
    "array",
    "push",
    "at",
    "alen",
    "str",
    "parse",
    "logger",
    "bytes",
    "floatBits",
    "floatFromBits",
    "args",
    "readLine",
    "readFile",
    "writeFile",
    "writeFileBytes",
    "writeStdout",
    "renameFile",
    "fsyncFile",
    "readFileBytes",
    "stringFromBytes",
    "listDir",
    "listDirKinds",
    "lineAt",
    "colAt",
    "moduleInterface",
    // The log levels (`info`, ...) are not reserved: the sugar carries `@info`
    // (see `crate::parser::METHOD_BUILTINS`).
    "value",
    "list",
    "schemaOf",
    "contractOf",
    "jsonSchema",
    "toJson",
    "fromJson",
    "toString",
    "pop",
    "swapRemove",
    "assert",
    "assertEq",
    "blackBox",
    "panic",
    "fromArray",
    "fromStep",
    "close",
    "boxStream",
    "unboxStream",
    "pullAt",
    "serveStream",
    "Int",
    "Int64",
    "Int32",
    "Int16",
    "Int8",
    "Float",
    "Float64",
    "Float32",
    // `splat` and `lane` are absent: they reach the builtin only through
    // method sugar's internal names, so a free `fn lane` still resolves.
    "F32x4",
    "I32x4",
    "F64x2",
    "UInt8",
    "UInt16",
    "UInt32",
    "UInt64",
    // `r[i] = v` looks up `atSet`.
    "atSet",
];

/// Where a name a program may write, but cannot resolve, has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// An exported function of this `std/` module; the hint is the import.
    Module(&'static str),
    /// A removed spelling; the hint is what to write instead.
    Removed(&'static str),
    /// A name a desugar writes; the hint names the sugar, then the import.
    Desugared {
        module: &'static str,
        sugar: &'static str,
    },
}

impl Gone {
    /// Returns the hint for a program that wrote `name`.
    pub fn hint(&self, name: &str) -> String {
        match self {
            Gone::Module(m) => {
                format!("`{name}` is `{m}`'s — add `import {{ {name} }} from \"{m}\"`")
            }
            Gone::Removed(s) => (*s).to_string(),
            Gone::Desugared { module, sugar } => format!(
                "`{name}` is `{module}`'s, and `{sugar}` writes through it — add \
                 `import {{ {name} }} from \"{module}\"`"
            ),
        }
    }
}

/// Names a program may write that do not resolve, and what to write instead.
/// [`Checker::call`] reads it only for a name that does not resolve.
///
/// A [`Gone::Module`] name must not be in [`RESERVED`]
/// (`every_moved_name_is_gone_from_reserved`). A [`Gone::Removed`] name must
/// stay reserved, or a user `fn push` would shadow the hint.
pub const MOVED_TO_STD: &[(&str, Gone)] = &[
    ("contains", Gone::Module("std/strpred")),
    ("startsWith", Gone::Module("std/strpred")),
    ("endsWith", Gone::Module("std/strpred")),
    ("slice", Gone::Module("std/strpred")),
    ("chars", Gone::Module("std/text")),
    ("hexEncode", Gone::Module("std/codecs")),
    ("hexDecode", Gone::Module("std/codecs")),
    ("base64Encode", Gone::Module("std/codecs")),
    ("base64Decode", Gone::Module("std/codecs")),
    ("urlEncode", Gone::Module("std/codecs")),
    ("urlDecode", Gone::Module("std/codecs")),
    // `parser::storage_desugar` writes `save(path, value)` as `writeAtomic`.
    (
        "writeAtomic",
        Gone::Desugared {
            module: "std/storage",
            sugar: "save(path, value)",
        },
    ),
    // Each fires for the bare name only: the sugar and method forms carry
    // `@`-prefixed names (`@str`, `@push`), which no source can lex.
    (
        "str",
        Gone::Removed("`str(x)` was removed; render a value with `x.toString()`"),
    ),
    (
        "concat",
        Gone::Removed("`concat(a, b)` was removed; concatenate Strings with `a + b`"),
    ),
    (
        "len",
        Gone::Removed("`len(s)` was removed; a String's byte length is `s.byteLength`"),
    ),
    (
        "list",
        Gone::Removed(
            "`list([..])` was removed; write the array literal `[..]` \
             directly where an `Array<T>` is expected",
        ),
    ),
    (
        "toString",
        Gone::Removed("`toString` is a method; write `x.toString()`"),
    ),
    (
        "push",
        Gone::Removed("`push(xs, v)` was removed; push with `xs.push(v)`"),
    ),
    (
        "at",
        Gone::Removed("`at(xs, i)` was removed; index with `xs[i]`"),
    ),
    (
        "alen",
        Gone::Removed("`alen(xs)` was removed; a collection's length is `xs.length`"),
    ),
    (
        "array",
        Gone::Removed("`array()` was removed; write the array literal `[]`"),
    ),
];

/// Returns where a name went, or `None` for one that was never a builtin.
pub fn moved_to_std(name: &str) -> Option<&'static Gone> {
    MOVED_TO_STD
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, g)| g)
}

use crate::types::INT32;

/// Returns the diagnostics, every `toJson` argument type and `fromJson` target,
/// and the refused set: the functions and module state the
/// diagnostics all belong to, so every other body is typed. The set is `None`
/// when a refusal stands anywhere else.
pub fn check_accum_with_json_types(program: &Program) -> CheckedJson {
    let (out, _, _, json, jdec, refused) = check_accum_full(program);
    (out, json, jdec, refused)
}

pub type CheckedJson = (
    Vec<Diagnostic>,
    Vec<Type>,
    Vec<Type>,
    Option<HashSet<String>>,
);

fn check_accum_full(
    program: &Program,
) -> (
    Vec<Diagnostic>,
    Vec<LocalBinding>,
    StoredFnEffects,
    Vec<Type>,
    Vec<Type>,
    Option<HashSet<String>>,
) {
    check_accum_inner(program)
}

/// An impl head as written (`impl<T> Show for Option<T>`), for the overlap
/// diagnostic.
fn render_impl_head(imp: &crate::ast::ImplBlock) -> String {
    let binder = if imp.type_params.is_empty() {
        String::new()
    } else {
        let ps: Vec<String> = imp
            .type_params
            .iter()
            .map(|p| match imp.type_bounds.get(p) {
                Some(bs) => format!("{p}: {}", bs.join(" + ")),
                None => p.clone(),
            })
            .collect();
        format!("<{}>", ps.join(", "))
    };
    format!("impl{binder} {} for {}", imp.protocol, imp.ty)
}

/// A method signature as the conformance diagnostic quotes it:
/// `fn area(modify self, Int64) -> Bool`. It prints capabilities, which must
/// agree, and no parameter names, which a `MethodSig` does not have.
fn render_method_sig(
    name: &str,
    recv: Capability,
    params: &[Type],
    caps: &[Capability],
    ret: &Type,
) -> String {
    let word = |c: Capability, t: String| match c {
        Capability::Read => t,
        Capability::Modify => format!("modify {t}"),
        Capability::Consume => format!("consume {t}"),
    };
    let mut ps = vec![word(recv, "self".to_string())];
    ps.extend(params.iter().enumerate().map(|(i, t)| {
        word(
            caps.get(i).copied().unwrap_or(Capability::Read),
            t.to_string(),
        )
    }));
    format!("fn {name}({}) -> {ret}", ps.join(", "))
}

fn check_accum_inner(
    program: &Program,
) -> (
    Vec<Diagnostic>,
    Vec<LocalBinding>,
    StoredFnEffects,
    Vec<Type>,
    Vec<Type>,
    Option<HashSet<String>>,
) {
    let mut out = Vec::new();
    // The functions and module state with a refusal, and how many diagnostics
    // they gave: when that is every diagnostic, every other body is typed.
    let mut refused: HashSet<String> = HashSet::new();
    let mut in_bodies = 0usize;

    // 1. Collect and validate type declarations.
    let mut types: HashMap<String, TypeDecl> = HashMap::new();
    for t in &program.type_decls {
        if matches!(t.name.as_str(), "Int64" | "Bool" | "Unit") {
            let mut d = cerr!(t.line, "cannot redefine built-in type `{}`", t.name);
            d.file = t.module.clone();
            out.push(d);
            continue;
        }
        if types.contains_key(&t.name) {
            let mut d = cerr!(t.line, "type `{}` defined twice", t.name);
            d.file = t.module.clone();
            out.push(d);
            continue;
        }
        types.insert(t.name.clone(), t.clone());
    }

    // 1b. Collect enum variants into a global constructor table.
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    for t in &program.type_decls {
        if let Some(vs) = crate::types::declared_variants(&t.base) {
            for v in vs {
                if RESERVED.contains(&v.name.as_str()) {
                    out.push(cerr!(t.line, "`{}` is a reserved name", v.name));
                    continue;
                }
                if variants.contains_key(&v.name) {
                    out.push(cerr!(t.line, "enum variant `{}` is defined twice", v.name));
                    continue;
                }
                if let Some(ty) = types.get(&v.name) {
                    // Type and variant names share one namespace across the
                    // linked program, so the other declaration may be in std.
                    let from = ty
                        .module
                        .as_deref()
                        .map(|m| {
                            let std_root = crate::manifest::std_root();
                            let spec = crate::loader::import_specifier("", m, std_root.as_deref());
                            let shown = match spec.starts_with("std/") {
                                true => spec,
                                false => m.rsplit('/').next().unwrap_or(m).to_string(),
                            };
                            format!(" declared in `{shown}`")
                        })
                        .unwrap_or_default();
                    out.push(cerr!(
                        t.line,
                        "enum variant `{}` clashes with the type `{}`{from}; rename the variant",
                        v.name,
                        v.name
                    ));
                    continue;
                }
                variants.insert(
                    v.name.clone(),
                    VariantInfo {
                        enum_name: t.name.clone(),
                        payload: v.payload.clone(),
                    },
                );
            }
        }
    }

    // 2. Collect function signatures (forward references allowed).
    let mut sigs: HashMap<String, (Vec<Type>, Type)> = HashMap::new();
    let mut generics: HashMap<String, Vec<String>> = HashMap::new();
    for f in &program.functions {
        if RESERVED.contains(&f.name.as_str()) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                "`{}` is a reserved name",
                f.name
            ));
            continue;
        }
        if variants.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                "`{}` is both a function and an enum variant",
                f.name
            ));
            continue;
        }
        if sigs.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                "function `{}` defined twice",
                f.name
            ));
            continue;
        }
        if types.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                "`{}` is both a type and a function name",
                f.name
            ));
            continue;
        }
        let params = f.params.iter().map(|p| p.ty.clone()).collect();
        sigs.insert(f.name.clone(), (params, f.ret.clone()));
        if !f.type_params.is_empty() {
            generics.insert(f.name.clone(), f.type_params.clone());
        }
    }
    let all_bounds: HashMap<String, HashMap<String, Vec<String>>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.type_bounds.clone()))
        .collect();
    // Each function's parameter capabilities, for checking `modify` call sites.
    let mut caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    // A protocol method under its surface name, receiver first, for the checks
    // that see only the written name (the lambda-capture rule).
    for p in &program.protocols {
        for m in &p.methods {
            let mut cs = vec![m.recv];
            cs.extend(m.param_caps.iter().copied());
            caps.insert(m.name.clone(), cs);
        }
    }
    // Parameter capabilities keyed by the Debug text of a `Type::Fn`, which
    // carries none, for a call through a stored function value.
    // When two declarations share a signature, `consume` wins: refusing is
    // the sound side.
    let mut caps_by_sig: HashMap<String, Vec<Capability>> = HashMap::new();
    for f in &program.functions {
        let key = format!(
            "{:?}",
            Type::Fn(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                Box::new(f.ret.clone()),
            )
        );
        let cs: Vec<Capability> = f.params.iter().map(|p| p.capability).collect();
        match caps_by_sig.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                for (c, n) in o.get_mut().iter_mut().zip(&cs) {
                    if *n == Capability::Consume {
                        *c = *n;
                    }
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(cs);
            }
        }
    }

    // Protocol registries: each method name to its protocol and
    // signature, and which (protocol, type key) pairs are implemented.
    let mut protocol_methods: HashMap<String, Vec<(String, MethodSig)>> = HashMap::new();
    for p in &program.protocols {
        for m in &p.methods {
            // A projection requirement never dispatches as a method.
            if m.result_cap.is_some() {
                continue;
            }
            protocol_methods
                .entry(m.name.clone())
                .or_default()
                .push((p.name.clone(), m.clone()));
        }
    }
    // Projection names protocols declare, for refusing one on a bounded
    // receiver: a projection inlines, and a type variable has no body.
    let protocol_places: std::collections::HashSet<String> = program
        .protocols
        .iter()
        .flat_map(|p| p.methods.iter())
        .filter(|m| m.result_cap.is_some())
        .map(|m| m.name.clone())
        .collect();
    // A free function named like a protocol method could never run: `call`
    // dispatches that name to an impl first.
    for f in &program.functions {
        if protocol_methods.contains_key(f.name.as_str()) {
            let owners: Vec<&str> = protocol_methods[f.name.as_str()]
                .iter()
                .map(|(p, _)| p.as_str())
                .collect();
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                "`{}` collides with protocol {}'s method of the same name — \
                 method names dispatch to impls before free functions, so this \
                 declaration could never run",
                f.name,
                owners.join(", ")
            ));
        }
    }
    let protocol_decls: HashMap<&str, &crate::ast::ProtocolDecl> = program
        .protocols
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let mut impls: std::collections::HashSet<(String, String)> = Default::default();
    // The impl declared for each (protocol, type constructor) key, so a second
    // one is refused at its declaration, naming both.
    let mut impl_heads: HashMap<(String, String), (usize, String)> = HashMap::new();
    for imp in &program.impls {
        let mark = out.len();
        // The impl binds exactly the associated types the protocol declares.
        // The parser has substituted them into the methods already.
        if let Some(declared) = protocol_decls.get(imp.protocol.as_str()).map(|p| &p.assoc) {
            for name in declared.iter() {
                if !imp.assoc.contains(name) {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        "`impl {} for {}` does not bind the associated type `{name}` \
                             — add `type {name} = ..` (protocol `{}` declares it)",
                        imp.protocol,
                        imp.ty,
                        imp.protocol
                    ));
                }
            }
            for name in &imp.assoc {
                if !declared.contains(name) {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        "`impl {} for {}` binds `type {name}`, which protocol `{}` \
                             does not declare",
                        imp.protocol,
                        imp.ty,
                        imp.protocol
                    ));
                }
            }
        }
        // Every method the protocol declares, against what the impl provides.
        // A bounded generic types `x.area()` from the protocol's signature, so
        // the impl must agree with it.
        if let Some(p) = protocol_decls.get(imp.protocol.as_str()) {
            // A position naming an associated type is not compared: the impl's
            // methods hold the substituted binding, and `ImplBlock::assoc`
            // keeps only the names, so nothing here can prove them equal.
            let probe: HashMap<String, Type> =
                p.assoc.iter().map(|a| (a.clone(), Type::Unit)).collect();
            // Nor is a position naming `Self`: the protocol is already refused,
            // and every impl would repeat it.
            let opaque =
                |t: &Type| crate::types::substitute(t, &probe) != *t || type_mentions_self(t);
            for sig in &p.methods {
                let want = || {
                    render_method_sig(&sig.name, sig.recv, &sig.params, &sig.param_caps, &sig.ret)
                };
                // Whether a provided method or projection agrees with the
                // declaration, and how it reads. The receiver's type is the
                // impl's head and is skipped; its capability is compared, or a
                // `modify self` impl would mutate through a `self` borrow.
                let provided = |f: &crate::ast::Function| {
                    let got: Vec<Type> = f.params.iter().skip(1).map(|p| p.ty.clone()).collect();
                    let got_caps: Vec<Capability> =
                        f.params.iter().skip(1).map(|p| p.capability).collect();
                    let recv = f
                        .params
                        .first()
                        .map(|p| p.capability)
                        .unwrap_or(Capability::Read);
                    let agrees = got.len() == sig.params.len()
                        && (sig.ret == f.ret || opaque(&sig.ret))
                        && recv == sig.recv
                        && got_caps == sig.param_caps
                        && std::iter::zip(&sig.params, &got).all(|(w, g)| w == g || opaque(w));
                    (
                        agrees,
                        render_method_sig(&f.name, recv, &got, &got_caps, &f.ret),
                    )
                };
                if sig.result_cap.is_some() {
                    let Some(f) = imp.places.iter().find(|m| m.name == sig.name) else {
                        out.push(cerr_at!(
                            imp.line,
                            imp.head_span(),
                            "`impl {} for {}` does not provide the projection `{}`, \
                             which protocol `{}` declares — a protocol's members are \
                             all required",
                            imp.protocol,
                            imp.ty,
                            want(),
                            imp.protocol
                        ));
                        continue;
                    };
                    let (agrees, got) = provided(f);
                    if !agrees {
                        out.push(cerr_at!(
                            f.line,
                            f.name_span(),
                            "projection `{}` does not match protocol `{}` — it declares \
                             `{}`, this provides `{}`",
                            f.name,
                            imp.protocol,
                            want(),
                            got
                        ));
                    }
                    continue;
                }
                let Some(f) = imp.methods.iter().find(|m| m.name == sig.name) else {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        "`impl {} for {}` does not provide `{}`, which protocol `{}` \
                             declares — a protocol's methods are all required, so anything \
                             holding a `T: {}` may call it",
                        imp.protocol,
                        imp.ty,
                        want(),
                        imp.protocol,
                        imp.protocol
                    ));
                    continue;
                };
                let (agrees, got) = provided(f);
                if !agrees {
                    out.push(cerr_at!(
                        f.line,
                        f.name_span(),
                        "`{}` does not match protocol `{}` — it declares `{}`, this \
                             provides `{}`",
                        render_impl_head(imp),
                        imp.protocol,
                        want(),
                        got
                    ));
                }
            }
            // A method the protocol does not declare is unreachable: dispatch
            // keys on the protocol's names (`Checker::call`). Often a typo.
            for m in &imp.methods {
                if p.methods.iter().any(|sig| sig.name == m.name) {
                    continue;
                }
                // Ties go to declaration order.
                let near = p
                    .methods
                    .iter()
                    .map(|sig| {
                        (
                            crate::contracts::edit_distance(&m.name, &sig.name),
                            &sig.name,
                        )
                    })
                    .filter(|(d, _)| *d <= crate::contracts::NEAR_THRESHOLD)
                    .min_by_key(|(d, _)| *d)
                    .map(|(_, n)| n);
                let fix = match near {
                    Some(n) => format!("did you mean `{n}`?"),
                    // `x.m(..)` falls through to a free function named `m`.
                    None => format!(
                        "Vyrn has no inherent methods, so a helper is a plain \
                         `fn {}(x: {}, ..)` at the top level — `x.{}(..)` calls that",
                        m.name, imp.ty, m.name
                    ),
                };
                let got: Vec<Type> = m.params.iter().skip(1).map(|p| p.ty.clone()).collect();
                let got_caps: Vec<Capability> =
                    m.params.iter().skip(1).map(|p| p.capability).collect();
                let recv = m
                    .params
                    .first()
                    .map(|p| p.capability)
                    .unwrap_or(Capability::Read);
                out.push(cerr_at!(
                    m.line,
                    m.name_span(),
                    "`{}` provides `{}`, which protocol `{}` does not declare — \
                         dispatch knows only a protocol's own method names, so this one is \
                         reachable from nowhere; {fix}",
                    render_impl_head(imp),
                    render_method_sig(&m.name, recv, &got, &got_caps, &m.ret),
                    imp.protocol
                ));
            }
        } else if let Some((_, sigs)) = crate::types::KNOWN_PROTOCOLS
            .iter()
            .find(|(p, _)| *p == imp.protocol)
        {
            for f in imp.methods.iter().chain(imp.places.iter()) {
                let Some((_, recv, caps)) = sigs.iter().find(|(m, ..)| *m == f.name) else {
                    continue;
                };
                let got: Vec<Type> = f.params.iter().skip(1).map(|p| p.ty.clone()).collect();
                let got_caps: Vec<Capability> =
                    f.params.iter().skip(1).map(|p| p.capability).collect();
                let got_recv = f.params.first().map_or(Capability::Read, |p| p.capability);
                if got_recv != *recv || got_caps != *caps {
                    out.push(cerr_at!(
                        f.line,
                        f.name_span(),
                        "`{}` does not match protocol `{}` — it declares `{}`, this \
                             provides `{}`",
                        render_impl_head(imp),
                        imp.protocol,
                        render_method_sig(&f.name, *recv, &got, caps, &f.ret),
                        render_method_sig(&f.name, got_recv, &got, &got_caps, &f.ret)
                    ));
                }
            }
        } else {
            out.push(cerr_at!(
                imp.line,
                imp.head_span(),
                "`impl {} for {}`: there is no protocol named `{}` — declare it with \
                 `protocol {} {{ .. }}` or import it",
                imp.protocol,
                imp.ty,
                imp.protocol,
                imp.protocol
            ));
        }

        // A named target must be an enum or a record. A validated scalar erases
        // to its base, so its value carries no name to dispatch on.

        // A `Show` impl returns a String. `Show` is known by name, not
        // declared, so the protocol comparison above does not reach it.
        if imp.protocol == crate::types::SHOW {
            for m in &imp.methods {
                if m.name == crate::types::SHOW_SHOW && m.ret != Type::Str {
                    out.push(cerr_at!(
                        m.line,
                        m.name_span(),
                        "`{}`'s `{}` must hand back a String to render through, found {}",
                        crate::types::SHOW,
                        crate::types::SHOW_SHOW,
                        m.ret
                    ));
                }
            }
        }
        let ok_target = match &imp.ty {
            Type::Int | Type::Bool | Type::Str => true,
            // The two built-in sums, under either spelling.
            _ if crate::types::is_sum_alias(&imp.ty) => true,
            Type::Named(n) | Type::App(n, _) => matches!(
                types.get(n).map(|d| &d.base),
                Some(Type::Enum(_) | Type::Record(_))
            ),
            _ => false,
        };
        match crate::types::type_key(&imp.ty) {
            Some(key) if ok_target => {
                let head = render_impl_head(imp);
                match impl_heads.get(&(imp.protocol.clone(), key.clone())) {
                    // Not "overlaps": `Option<Int64>` and `Option<String>` are
                    // disjoint, but dispatch keys on the constructor
                    // (`types::type_key`).
                    Some((prev_line, prev)) => out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        "`{head}` collides with `{prev}` (line {prev_line}) — Vyrn \
                             dispatches on the type constructor, so `{key}` may have only one \
                             impl of `{}`; write one generic impl (`impl<T> {} for {key}<T>`) to \
                             cover every instantiation",
                        imp.protocol,
                        imp.protocol
                    )),
                    None => {
                        impl_heads.insert((imp.protocol.clone(), key.clone()), (imp.line, head));
                        impls.insert((imp.protocol.clone(), key));
                    }
                }
            }
            _ => {
                let named_scalar = match &imp.ty {
                    Type::Named(n) | Type::App(n, _) => types
                        .get(n)
                        .map(|d| &d.base)
                        .filter(|b| !matches!(b, Type::Enum(_) | Type::Record(_))),
                    _ => None,
                };
                let why = if let Some(base) = named_scalar {
                    format!(
                        "`{}` erases to `{base}` at run time, so a value of it carries no name \
                         to dispatch on; implement `{}` for `{base}`, or give `{}` a record type",
                        imp.ty, imp.protocol, imp.ty
                    )
                } else {
                    "implement protocols for Int64/Bool/String, a record, an enum, `Option` \
                     or `Result`"
                        .to_string()
                };
                out.push(cerr_at!(
                    imp.line,
                    imp.head_span(),
                    "`impl {} for {}` is not supported — {why}",
                    imp.protocol,
                    imp.ty
                ))
            }
        }
        // A refused impl refuses every method it provides or its protocol
        // declares, under the impl's key.
        if out.len() > mark {
            in_bodies += out.len() - mark;
            if let Some(key) = crate::types::type_key(&imp.ty) {
                let declared = protocol_decls
                    .get(imp.protocol.as_str())
                    .into_iter()
                    .flat_map(|p| p.methods.iter().map(|m| &m.name));
                for m in imp.methods.iter().map(|m| &m.name).chain(declared) {
                    refused.insert(crate::types::impl_method_name(&imp.protocol, &key, m));
                }
            }
        }
    }

    // Neither an `extern` nor a `gen fn` may become a stored function value.
    let extern_fns: std::collections::HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_extern)
        .map(|f| f.name.clone())
        .collect();
    let gen_fns: std::collections::HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_gen)
        .map(|f| f.name.clone())
        .collect();

    // Contracts are comptime-only: the checker validates their
    // member types and types `contractOf(Name)`.
    let contracts: HashMap<String, ContractDecl> = program
        .contracts
        .iter()
        .map(|c| (c.name.clone(), c.clone()))
        .collect();

    let checker = Checker {
        sigs: &sigs,
        caps: &caps,
        caps_by_sig: &caps_by_sig,
        types: &types,
        contracts: &contracts,
        variants: &variants,
        generics: &generics,
        all_bounds: &all_bounds,
        protocol_methods: &protocol_methods,
        protocol_places: &protocol_places,
        impls: &impls,
        impl_blocks: &program.impls,
        cur_bounds: RefCell::new(HashMap::new()),
        region_floor: RefCell::new(Vec::new()),
        binder_types: RefCell::new(HashMap::new()),
        in_root: std::cell::Cell::new(false),
        errors: RefCell::new(Vec::new()),
        globals: RefCell::new(HashMap::new()),
        in_test: RefCell::new(false),
        in_bench: RefCell::new(false),
        in_gen: RefCell::new(false),
        unknown: std::cell::Cell::new(false),
        here: RefCell::new(None),
        shadows: program.surface_shadows.clone(),
        stmt_line: RefCell::new(0),
        extern_fns: &extern_fns,
        gen_fns: &gen_fns,
        cur_fn: RefCell::new(String::new()),
        stored_sources: RefCell::new(Vec::new()),
        arg_sources: RefCell::new(Vec::new()),
        stored_calls: RefCell::new(Vec::new()),
        json_types: RefCell::new(Vec::new()),
        json_dec_types: RefCell::new(Vec::new()),
    };

    // 2b. Module state, in declaration order. A failed global still binds, as
    //     `Err`, so bodies that read it do not cascade "unknown variable".
    in_bodies += checker.check_globals(program, &mut out, &mut refused);

    // 3. Validate each type decl (base kind, referenced-type existence, predicate).
    for t in &program.type_decls {
        if let Err(s) = checker.unit(|| checker.check_type_decl(t)) {
            out.extend(s);
        }
    }

    // 3b. Validate each contract decl: member types exist, defaults match.
    for c in &program.contracts {
        for s in checker.check_contract_decl(c) {
            let mut d = s;
            d.file = c.module.clone();
            out.push(d);
        }
    }

    // 3c. Validate each protocol decl at its own line: its method signatures
    //     name types that exist.
    for p in &program.protocols {
        for s in checker.check_protocol_decl(p) {
            let mut d = s;
            d.file = p.module.clone();
            out.push(d);
        }
    }

    // 4. `main`, a whole-program error at line 0. A library (any export), a
    // file with tests or benches, and a served module (exactly
    // `fn handle(req: Request) -> Response`) need none.
    let has_served_handle = sigs.get("handle").is_some_and(|(params, ret)| {
        params.as_slice() == [Type::Named("Request".to_string())]
            && *ret == Type::Named("Response".to_string())
    });
    let is_library = program.functions.iter().map(|f| f.exported).any(|e| e)
        || program.type_decls.iter().any(|t| t.exported)
        || program.protocols.iter().any(|p| p.exported)
        || program.contracts.iter().any(|c| c.exported)
        || !program.tests.is_empty()
        || !program.benches.is_empty()
        || has_served_handle;
    match sigs.get("main") {
        None if !is_library => out.push(cerr!(0, "no `main` function found")),
        None => {}
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => {
            out.push(cerr!(0, "`main` must have signature `fn main() -> Int64`"))
        }
        _ => {}
    }

    // 5. Check functions, each independently. In a body, errors accumulate
    //    per statement in `errors`; `function` returns the first and this
    //    drains the rest. Within one expression the check is first-error.
    for f in &program.functions {
        let produced_from = out.len();

        // Signature validation runs outside `function()` and must accept a
        // `Code` type in a `gen fn` signature.
        *checker.in_gen.borrow_mut() = in_gen_of(f);
        *checker.here.borrow_mut() = f.module.clone();
        let r = (|| -> Result<(), Diagnostic> {
            for p in &f.params {
                // A function value cannot cross the host boundary, nor a
                // generation-time signature.
                if checker.contains_fn(&p.ty) && (f.is_extern || f.is_export_extern) {
                    return Err(cerr_at!(
                        f.line,
                        f.name_span(),
                        "an `extern` function may not take a `fn`-typed \
                         parameter"
                    ));
                }
                if checker.contains_fn(&p.ty) && f.is_gen {
                    return Err(cerr_at!(
                        f.line,
                        f.name_span(),
                        "a `gen fn` may not take a `fn`-typed parameter in v1"
                    ));
                }
                checker.ensure_type_exists(&p.ty, f.line)?;
            }
            if checker.contains_fn(&f.ret) && (f.is_extern || f.is_export_extern) {
                return Err(cerr_at!(
                    f.line,
                    f.name_span(),
                    "an `extern` function may not return a function value \
                     (closures do not cross the host boundary)"
                ));
            }
            if checker.contains_fn(&f.ret) && f.is_gen {
                return Err(cerr_at!(
                    f.line,
                    f.name_span(),
                    "a `gen fn` may not return a function value"
                ));
            }
            checker.ensure_type_exists(&f.ret, f.line)?;
            // An `extern` import has no body; an `export extern` has one. Both
            // signatures must fit the host ABI.
            if f.is_extern {
                checker.check_extern_sig(f)?;
            } else {
                if f.is_export_extern {
                    checker.check_extern_sig(f)?;
                }
                checker.function(f)?;
            }
            Ok(())
        })();
        if let Err(s) = r {
            let mut d = s;
            d.file = f.module.clone();
            out.push(d);
        }
        for s in checker.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = f.module.clone();
            out.push(d);
        }
        if out.len() > produced_from {
            refused.insert(f.name.clone());
            in_bodies += out.len() - produced_from;
        }
    }

    // 6. Projection, test and bench bodies. A test or bench is a Unit body
    //    under an unspellable name (`test@<index>`), absent from `sigs`, so no
    //    code can call it.
    check_places(&checker, program, &mut out);
    check_named_blocks(&checker, &program.tests, "test", &checker.in_test, &mut out);
    check_named_blocks(
        &checker,
        &program.benches,
        "bench",
        &checker.in_bench,
        &mut out,
    );

    // 7. Comptime purity of every `gen fn` and its callees, after
    //    the body checks so a generator's type errors come first.
    check_comptime_purity(program, &mut out);

    let effects = StoredFnEffects {
        sources: checker.stored_sources.borrow().clone(),
        arg_sources: checker.arg_sources.borrow().clone(),
        calls: checker.stored_calls.borrow().clone(),
    };
    let binders = local_index(program, &checker.binder_types.borrow());
    let mut json_types = checker.json_types.borrow().clone();
    json_types.dedup_by_key(|t| format!("{t:?}"));
    let mut json_dec_types = checker.json_dec_types.borrow().clone();
    json_dec_types.dedup_by_key(|t| format!("{t:?}"));
    let typed = (in_bodies == out.len()).then_some(refused);
    (out, binders, effects, json_types, json_dec_types, typed)
}

/// Checks every projection body as a function body, plus three rules of its
/// own. It is inlined, so it returns once, as its last statement. It returns
/// a place, because a value would be a hidden copy. The place is
/// rooted in `self` or a parameter, which the access site owns.
fn check_places(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {
    for (imp, f) in crate::project::all(program) {
        let mut push = |msg: Diagnostic| {
            let mut d = msg;
            d.file = f.module.clone();
            out.push(d);
        };
        if crate::project::is_optional(f) {
            check_optional_place(checker, f, &mut push);
            continue;
        }
        let yields = count_yields(&f.body);
        if yields != 1
            || !matches!(
                f.body.stmts.last(),
                Some(Stmt::Return { value: Some(_), .. })
            )
        {
            push(cerr_at!(
                f.line,
                f.name_span(),
                "projection `{}` must end with exactly one `return <place>` — \
                 a projection is inlined at the access site, so it has one exit",
                f.name
            ));
            continue;
        }
        let Some(Stmt::Return { value: Some(y), .. }) = f.body.stmts.last() else {
            unreachable!("checked just above")
        };
        if crate::project::has_try(&f.body) {
            push(cerr_at!(
                f.line,
                f.name_span(),
                "projection `{}` uses `?`, which returns — a projection is \
                  inlined at the access site, so there is no frame to return \
                  from. Check the condition and `panic` instead.",
                f.name
            ));
            continue;
        }
        // A `modify` result rooted at a parameter that is not `modify` is the
        // argument's value at the site, with nowhere to write.
        let modifies =
            f.params.first().map(|p| p.capability) == Some(crate::ast::Capability::Modify);
        let read_root = modifies
            && crate::project::place_root(y).is_some_and(|r| {
                f.params
                    .iter()
                    .any(|p| p.name == r && p.capability != crate::ast::Capability::Modify)
            });
        if !crate::project::is_place(y) || read_root {
            push(cerr_at!(
                f.line,
                f.name_span(),
                "projection `{}` returns a value, not a place — a `read`/`modify` \
                 result must be a field or element of `self`; a projection that \
                 computes a new value is an ordinary `fn` returning `-> T`",
                f.name
            ));
            continue;
        }
        // A read projection's roots are transitive: a prologue `let` that
        // borrows from an accepted root is one. A modify projection
        // takes no prologue roots: a write through a copied handle can land
        // in the copy.
        let prologue = match modifies {
            true => &f.body.stmts[..0],
            false => &f.body.stmts[..f.body.stmts.len() - 1],
        };
        rooted_where_the_site_owns(f, y, prologue.iter(), &mut push);
        let r = checker.function(f);
        if let Err(s) = r {
            push(s);
        }
        for s in checker.errors.borrow_mut().drain(..) {
            push(s);
        }
        let _ = imp;
    }
}

/// Checks an optional projection, `-> read Option<T>`: a prologue,
/// one `if <miss> { return None }`, the hit's statements, then
/// `return Some(<place>)`. A second miss test is refused: statements between
/// two would run after the first decided.
fn check_optional_place(checker: &Checker, f: &Function, push: &mut impl FnMut(Diagnostic)) {
    // `a[i]`, `a[i] = v` and `for` consume a place unconditionally, so the
    // members their sugar dispatches cannot be the optional kind.
    if ["at", "atSet", "nth"].contains(&f.name.as_str()) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            "`{}` is dispatched by sugar that consumes a place unconditionally \
             — an optional projection needs a name of its own",
            f.name
        ));
        return;
    }
    if f.params.first().map(|p| p.capability) != Some(crate::ast::Capability::Read) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            "optional projection `{}` must take `read self` — the hit is a \
             borrow the `if let` arm reads, and nothing writes through a miss",
            f.name
        ));
        return;
    }
    let shape = cerr_at!(
        f.line,
        f.name_span(),
        "optional projection `{}` must hold one `if <miss> {{ return None }}` \
         and end with `return Some(<place>)` — one prologue, one decision, \
         and statements after the decision run only on the hit",
        f.name
    );
    let n = f.body.stmts.len();
    if n < 2 || count_yields(&f.body) != 2 {
        push(shape);
        return;
    }
    // Statements after the miss exit run only on a hit, when the place exists.
    let Some(at) = f.body.stmts.iter().position(crate::project::is_miss_return) else {
        push(shape);
        return;
    };
    let Some(Stmt::Return { value: Some(v), .. }) = f.body.stmts.last() else {
        push(shape);
        return;
    };
    if at + 1 == n {
        push(shape);
        return;
    }
    let Expr::Call { name, args, .. } = v else {
        push(shape);
        return;
    };
    if name != "Some" || args.len() != 1 {
        push(shape);
        return;
    }
    let y = &args[0];
    if crate::project::has_try(&f.body) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            "projection `{}` uses `?`, which returns — a projection is \
                  inlined at the access site, so there is no frame to return \
                  from. Check the condition and `panic` instead.",
            f.name
        ));
        return;
    }
    if !crate::project::is_place(y) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            "optional projection `{}` answers `Some` of a value, not of a place \
             — the hit must be a field or element of `self`; a computed value \
             is an ordinary `fn` returning `-> Option<T>`",
            f.name
        ));
        return;
    }
    // Roots trace through borrowing `let`s before and after the miss exit;
    // `tryField` binds its payload after it.
    let prologue = f.body.stmts[..n - 1]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at)
        .map(|(_, s)| s);
    rooted_where_the_site_owns(f, y, prologue, push);
    if let Err(s) = checker.function(f) {
        push(s);
    }
    for s in checker.errors.borrow_mut().drain(..) {
        push(s);
    }
}

/// Refuses a projection whose place is not rooted in `self`, a parameter, or
/// a `let` in `prologue` that borrows from one.
fn rooted_where_the_site_owns<'s>(
    f: &Function,
    y: &Expr,
    prologue: impl Iterator<Item = &'s Stmt>,
    push: &mut impl FnMut(Diagnostic),
) {
    let mut roots: std::collections::HashSet<String> =
        f.params.iter().map(|p| p.name.clone()).collect();
    for s in prologue {
        if let Stmt::Let { name, value, .. } = s {
            if let_borrows_from(value, &roots) {
                roots.insert(name.clone());
            }
        }
    }
    match crate::project::place_root(y) {
        Some(root) if roots.contains(&root) => {}
        Some(root) => push(cerr_at!(
            f.line,
            f.name_span(),
            "projection `{}` returns a place rooted at `{root}`, which the \
             access site does not own — a projection may only return a place \
             inside `self`, a parameter, or a prologue `let` that borrows \
             from one",
            f.name
        )),
        None => {}
    }
}

/// Whether a `let`'s initializer borrows from one of `roots`: a
/// place rooted there, or the refutable-`let` desugar's `match` on a root
/// whose every arm panics or yields a binder of its own pattern.
fn let_borrows_from(e: &Expr, roots: &std::collections::HashSet<String>) -> bool {
    if crate::project::is_place(e) {
        return crate::project::place_root(e).is_some_and(|r| roots.contains(&r));
    }
    let Expr::Match {
        scrutinee, arms, ..
    } = e
    else {
        return false;
    };
    let Expr::Var { name, .. } = &**scrutinee else {
        return false;
    };
    if !roots.contains(name) {
        return false;
    }
    arms.iter().all(|arm| {
        let Some(body) = arm.body.as_expr() else {
            return false;
        };
        if let Expr::Call { name, .. } = body {
            if crate::ast::is_panic(name) {
                return true;
            }
        }
        let binders = arm.pattern.bindings();
        crate::project::is_place(body)
            && crate::project::place_root(body).is_some_and(|r| binders.contains(&r.as_str()))
    })
}

/// How many value `return`s a projection body has, counting every branch.
fn count_yields(b: &crate::ast::Block) -> usize {
    b.stmts
        .iter()
        .map(|s| match s {
            Stmt::Return { value: Some(_), .. } => 1,
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => count_yields(then_block) + else_block.as_ref().map(count_yields).unwrap_or(0),
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                count_yields(body)
            }
            _ => 0,
        })
        .sum()
}

/// Checks every `test` or `bench` body as a Unit function named
/// `<noun>@<index>`, with `host` raised so `assert` or `blackBox` is legal.
/// A name may not repeat inside one module.
fn check_named_blocks(
    checker: &Checker,
    blocks: &[NamedBlock],
    noun: &str,
    host: &RefCell<bool>,
    out: &mut Vec<Diagnostic>,
) {
    let mut seen: HashMap<(Option<String>, String), usize> = HashMap::new();
    for t in blocks {
        let key = (t.module.clone(), t.name.clone());
        if let Some(prev) = seen.get(&key) {
            let mut d = cerr!(
                t.line,
                "duplicate {noun} name {:?} (already declared on line {prev})",
                t.name
            );
            d.file = t.module.clone();
            out.push(d);
        } else {
            seen.insert(key, t.line);
        }
    }
    *host.borrow_mut() = true;
    for (i, t) in blocks.iter().enumerate() {
        // The head is synthetic but the body is the real node, so what the
        // checker records lands on the nodes `own` and the lowering walk.
        let synthetic = Function {
            name: format!("{noun}@{i}"),
            exported: false,
            module: t.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: Block { stmts: Vec::new() },
            line: t.line,
            col: 0,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        };
        if let Err(s) = checker.function_body(&synthetic, &t.body) {
            let mut d = s;
            d.file = t.module.clone();
            out.push(d);
        }
        for s in checker.errors.borrow_mut().drain(..) {
            let mut d = s;
            d.file = t.module.clone();
            out.push(d);
        }
    }
    *host.borrow_mut() = false;
}

/// Checks the program and returns every diagnostic. An error in one function
/// or type does not hide errors in the others.
pub fn check_accum(program: &Program) -> Vec<Diagnostic> {
    check_accum_with_binders(program).0
}

/// Checks the program and returns the first diagnostic, rendered.
pub fn check(program: &Program) -> Result<(), String> {
    match check_accum(program).into_iter().next() {
        Some(d) => Err(d.render()),
        None => Ok(()),
    }
}

// The record: the type of every expression and the substitution of every
// generic call, keyed by AST node address (the identity `own` uses). Off by
// default, so the editor's keystroke path does not pay for it.

thread_local! {
    static RECORDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Inside [`Checker::record_desugar`]: typing AST the lexer never made.
    static DESUGARING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RECORD: RefCell<Recorded> = RefCell::new(Recorded::new());
    /// The substitution the innermost generic call just solved, for the
    /// [`Checker::expr`] wrapper that knows the call node's address. A nested
    /// call consumes and clears it before its caller writes one.
    static PENDING_SUBST: RefCell<Option<(String, Vec<(String, Type)>)>> =
        const { RefCell::new(None) };
    /// The declaration of the call [`Checker::check_declared_call`] just typed
    /// `Err`, for the same wrapper.
    static PENDING_CALL: RefCell<Option<CallDecl>> = const { RefCell::new(None) };
}

/// The declaration a call was checked against; the typed judgment states the
/// call's refusal from it.
#[derive(Debug, Clone)]
pub struct CallDecl {
    /// The name a reader can write.
    pub shown: String,
    /// The parameter types, the receiver's first where `recv`.
    pub params: Vec<Type>,
    /// How many type parameters the callee declares.
    pub type_params: usize,
    /// A method call: the receiver is argument 1 and no type argument is read.
    pub recv: bool,
}

/// What the checker decided, keyed by AST node address.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    /// The static type of every expression the checker typed.
    pub node_types: HashMap<usize, Type>,
    /// The static type of every join (a `match` or `if` expression), before
    /// instantiation. A subset of [`Recorded::node_types`], kept apart for the
    /// emitters, which cannot tell a join from its address.
    pub joins: HashMap<usize, Type>,
    /// Every solved type parameter of a generic call or record literal, keyed
    /// by the [`Expr::Call`] or [`Expr::StructLit`] node: the callee or record
    /// name, and the solved arguments in its type-parameter order. The checker
    /// refines nothing later, so the solution governs its whole subtree.
    pub node_substs: HashMap<usize, (String, Vec<(String, Type)>)>,
    /// A declared call whose arity, type-argument count or argument the typed
    /// judgment refuses, keyed by the [`Expr::Call`] node.
    pub calls: HashMap<usize, CallDecl>,
}

impl Recorded {
    fn new() -> Self {
        Self::default()
    }
}

/// One pass that returns the diagnostics, the root's bindings and the record.
fn recording_check(program: &Program) -> (Vec<Diagnostic>, Vec<LocalBinding>, Recorded) {
    RECORD.with(|r| *r.borrow_mut() = Recorded::new());
    PENDING_SUBST.with(|p| *p.borrow_mut() = None);
    RECORDING.with(|c| c.set(true));
    let (diags, binders, _, _, _, _) = check_accum_full(program);
    RECORDING.with(|c| c.set(false));
    let made = RECORD.with(|r| std::mem::replace(&mut *r.borrow_mut(), Recorded::new()));
    (diags, binders, made)
}

/// Checks `program` and returns the record. Diagnostics are dropped: the
/// caller has already checked.
pub fn record(program: &Program) -> Recorded {
    recording_check(program).2
}

/// Checks the program and holds the record for [`recorded`], so the lowering
/// does not check again. It never reuses a body: a reused body would leave a
/// hole in the record.
pub fn check_accum_recording(program: &Program) -> (Vec<Diagnostic>, Vec<LocalBinding>) {
    let (diags, binders, made) = recording_check(program);
    hold(program, std::rc::Rc::new(made));
    (diags, binders)
}

thread_local! {
    /// The address of the program a record may be held for, or 0. Sound as a
    /// key because the guard that sets it borrows the program
    /// ([`crate::own::Memo::open`]).
    static HOLDING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static HELD: RefCell<Option<(usize, HeldRecord)>> = const { RefCell::new(None) };
}

/// `(generator host, test host, record)`. The host flags change what a check
/// decides, so a record answers only under the flags it was made with.
pub(crate) type HeldRecord = (bool, bool, std::rc::Rc<Recorded>);

/// Opens the record slot for `program`. Called by [`crate::own::Memo::open`],
/// so a record lives as long as its analysis.
pub(crate) fn hold_open(program: &Program) {
    HOLDING.with(|h| h.set(program as *const Program as usize));
    HELD.with(|h| *h.borrow_mut() = None);
}

/// Closes the record slot. Called by [`crate::own::Memo`]'s `Drop`.
pub(crate) fn hold_close() {
    HOLDING.with(|h| h.set(0));
    HELD.with(|h| *h.borrow_mut() = None);
}

/// The record slot as `crate::check_and_synthesize` holds it, from synthesis
/// to the last judgment, so its three readers share one record. It stands
/// aside where another holder has the slot, so it never closes theirs.
pub(crate) struct Held(bool);

impl Held {
    pub(crate) fn open(program: &Program) -> Held {
        if HOLDING.with(|h| h.get()) != 0 {
            return Held(false);
        }
        hold_open(program);
        Held(true)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.0 {
            hold_close();
        }
    }
}

fn hold(program: &Program, made: std::rc::Rc<Recorded>) {
    adopt(program, (gen_host(), test_host(), made));
}

/// Holds `record` for `program` where the slot is open for it. The CLI adopts
/// the load's record through [`crate::own::Memo::open`]: the `Program` moved,
/// but the nodes the record keys did not.
pub(crate) fn adopt(program: &Program, record: HeldRecord) {
    let key = program as *const Program as usize;
    if HOLDING.with(|h| h.get()) == key {
        HELD.with(|h| *h.borrow_mut() = Some((key, record)));
    }
}

/// The record held for `program`, with the host flags it was made under.
pub(crate) fn held(program: &Program) -> Option<HeldRecord> {
    let key = program as *const Program as usize;
    HELD.with(|h| {
        h.borrow()
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, r)| r.clone())
    })
}

/// Returns the record of `program`: the held one if it matches the program
/// and host flags, else a new one, held for the next ask.
pub fn recorded(program: &Program) -> std::rc::Rc<Recorded> {
    if let Some((_, _, r)) = held(program).filter(|(g, t, _)| (*g, *t) == (gen_host(), test_host()))
    {
        return r;
    }
    let made = std::rc::Rc::new(record(program));
    hold(program, made.clone());
    made
}

fn recording() -> bool {
    RECORDING.with(|c| c.get())
}

/// Whether the checker is inside AST the lexer never made. [`Expr::Int`]'s arm
/// reads it.
fn desugaring() -> bool {
    DESUGARING.with(|c| c.get())
}

/// Hands the solved type arguments to the [`Checker::expr`] wrapper.
fn note_subst(name: &str, subst: &HashMap<String, Type>, type_params: &[String]) {
    let args: Vec<(String, Type)> = type_params
        .iter()
        .filter_map(|p| subst.get(p).map(|t| (p.clone(), t.clone())))
        .collect();
    PENDING_SUBST.with(|p| *p.borrow_mut() = Some((name.to_string(), args)));
}

struct Checker<'a> {
    sigs: &'a HashMap<String, (Vec<Type>, Type)>,
    caps: &'a HashMap<String, Vec<Capability>>,
    /// Parameter capabilities by the Debug text of a stored function value's
    /// `Type::Fn`, which carries none.
    caps_by_sig: &'a HashMap<String, Vec<Capability>>,
    types: &'a HashMap<String, TypeDecl>,
    contracts: &'a HashMap<String, ContractDecl>,
    variants: &'a HashMap<String, VariantInfo>,
    /// Generic function name to its type-parameter names.
    generics: &'a HashMap<String, Vec<String>>,
    /// Function name to (type parameter to bounds).
    all_bounds: &'a HashMap<String, HashMap<String, Vec<String>>>,
    /// Method name to (protocol, signature).
    protocol_methods: &'a HashMap<String, Vec<(String, MethodSig)>>,
    /// Projection names protocols declare.
    protocol_places: &'a std::collections::HashSet<String>,
    /// Implemented (protocol, type key) pairs.
    impls: &'a std::collections::HashSet<(String, String)>,
    /// Every `impl` block, for resolving a projection, which has no mangled
    /// name, by receiver type.
    impl_blocks: &'a [crate::ast::ImplBlock],
    /// Bounds of the function being checked.
    cur_bounds: RefCell<HashMap<String, Vec<String>>>,
    /// Scope depths at each enclosing `region` entry. A binding below the top
    /// depth is outer: a heap value assigned to it would dangle when the
    /// region frees.
    region_floor: RefCell<Vec<usize>>,
    /// The type of every root-module binding, keyed by binder position, for
    /// editor hover. A binding in a statement that did not type has no row.
    binder_types: RefCell<HashMap<(usize, usize), Type>>,
    /// Whether the function being checked is the root module's. Only the root
    /// is indexed: two modules share a position.
    in_root: std::cell::Cell<bool>,
    /// A body's statement errors, cleared per function. A failed `let` or
    /// `for` binds its name to [`Type::Err`] so later uses do not cascade.
    errors: RefCell<Vec<Diagnostic>>,
    /// Module state, filled in declaration order before any body is checked.
    /// [`Scope`] reads it when its frames run out.
    globals: RefCell<HashMap<String, Binding>>,
    /// Inside a `test` body: `assert` and `assertEq` are legal.
    in_test: RefCell<bool>,
    /// Inside a `bench` body: `blackBox` is legal, as in a `test`.
    in_bench: RefCell<bool>,
    /// Inside a `gen fn` body: `Code` and the code-quote builtins are legal.
    /// A `gen fn` body is never emitted.
    in_gen: RefCell<bool>,
    /// Whether the unit [`Checker::unit`] runs typed a node [`Checker::judged`],
    /// or read a name typed [`Type::Err`].
    unknown: std::cell::Cell<bool>,
    /// The module being checked; `None` is the root. With `shadows`
    /// ([`ast::Program::surface_shadows`], filled by the loader), it decides
    /// whether `render`, `rawAt`, `raw` or `lex` is the builtin or a function
    /// this module declares or imports.
    here: RefCell<Option<String>>,
    shadows: std::collections::HashSet<(Option<String>, String)>,
    /// The line of the enclosing statement, for a literal, which carries none.
    stmt_line: RefCell<usize>,
    /// `extern` functions, which cannot be function values.
    extern_fns: &'a std::collections::HashSet<String>,
    /// `gen fn`s, which cannot be function values.
    gen_fns: &'a std::collections::HashSet<String>,
    cur_fn: RefCell<String>,
    /// Every lambda or named function that flows into a stored function value.
    stored_sources: RefCell<Vec<StoredSource>>,
    /// Every lambda or function name passed straight to a `fn`-typed
    /// parameter. See [`StoredFnEffects::arg_sources`].
    arg_sources: RefCell<Vec<StoredSource>>,
    /// Each call through a stored function value, as (enclosing function,
    /// signature).
    stored_calls: RefCell<Vec<(String, Type)>>,
    /// Every `toJson` argument type. Only the checker knows it, so it collects
    /// and `crate::check_and_synthesize` synthesizes the encoders.
    json_types: RefCell<Vec<Type>>,
    /// Every `fromJson<T>` target, for the decoders.
    json_dec_types: RefCell<Vec<Type>>,
}

/// A declaration a call is checked against: a user function, a seeded builtin
/// row, an impl method or a protocol member. [`Checker::check_declared_call`]
/// reads it.
struct DeclaredCall<'a> {
    /// The name `generics` and `all_bounds` are keyed by: a function name, or
    /// an impl method's mangled one.
    key: &'a str,
    /// The name a refusal prints: the surface name the reader wrote, never an
    /// `@` spelling or a mangled impl symbol.
    shown: &'a str,
    params: &'a [Type],
    ret: &'a Type,
    type_params: Option<&'a Vec<String>>,
    caps: Option<&'a Vec<Capability>>,
    bounds: Option<&'a HashMap<String, Vec<String>>>,
    /// The already typed receiver of a dispatched call. It is solved, not
    /// coerced, because the impl was selected by it.
    recv: Option<&'a Type>,
    /// The type arguments the caller wrote (`fromJson<Shape>(s)`), in
    /// declaration order; the arguments infer the rest.
    written: &'a [Type],
}

struct VariantInfo {
    enum_name: String,
    payload: Vec<Type>,
}

#[derive(Clone)]
struct Binding {
    ty: Type,
    mutable: bool,
}

/// A stack of lexical frames, innermost last, and whether a name the frames do
/// not answer falls through to module state. Do not copy module state into a
/// frame: that made the check quadratic in the number of globals.
///
/// [`Scope::open`] is a function body or a global's initializer.
/// [`Scope::closed`] is a `where` predicate or a member default, which see only
/// their fields. Both start with one empty frame; `region_floor` compares
/// depths.
#[derive(Clone)]
struct Scope {
    frames: Vec<HashMap<String, Binding>>,
    globals: bool,
}

impl Scope {
    /// A scope that sees module state.
    fn open() -> Self {
        Scope {
            frames: vec![HashMap::new()],
            globals: true,
        }
    }

    /// A scope that sees only what is put in it.
    fn closed() -> Self {
        Scope {
            frames: vec![HashMap::new()],
            globals: false,
        }
    }
}

impl std::ops::Deref for Scope {
    type Target = Vec<HashMap<String, Binding>>;
    fn deref(&self) -> &Self::Target {
        &self.frames
    }
}

impl std::ops::DerefMut for Scope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frames
    }
}

/// What [`Checker::reaches`] does at one type: answer, or look at its parts.
enum Reach {
    Yes,
    /// No, and the parts are not looked at.
    No,
    Parts,
}

impl<'a> Checker<'a> {
    // ---- type relations -------------------------------------------------

    /// The representation type: a named type decays to its base.
    fn base(&self, ty: &Type) -> Type {
        crate::types::resolve(ty, self.types)
    }

    /// Refuses a user `Map` key that is not heapless all the way down:
    /// integers, `Bool`, records of such and fieldless enums.
    fn check_key_shape(&self, key: &Type, ty: &Type, line: usize) -> Result<(), Diagnostic> {
        match ty {
            Type::Int | Type::IntN { .. } | Type::Bool => Ok(()),
            Type::Float | Type::Float32 => Err(cerr!(
                line,
                "`{key}` holds a float, and a `Map` key must hash and compare by equality: `NaN != NaN`, and `+0.0 == -0.0` would hash apart"
            )),
            Type::Record(fs) => fs
                .iter()
                .try_for_each(|f| self.check_key_shape(key, &self.base(&f.ty), line)),
            Type::Enum(vs) => {
                if vs.iter().all(|v| v.payload.is_empty()) {
                    Ok(())
                } else {
                    Err(cerr!(
                        line,
                        "`{key}` has a variant with a payload, and a payload-bearing enum key waits for real demand — a fieldless enum or a record of scalars keys today"
                    ))
                }
            }
            _ => Err(cerr!(
                line,
                "`{key}` owns heap somewhere in its fields, and a `Map` key is stored, compared and freed by the map — a key must be heapless all the way down"
            )),
        }
    }

    /// Refuses a projection whose receiver is a projection call the engines
    /// cannot resolve: one whose raw declared result is a type parameter, as
    /// `Slots<T>`'s `at` answers `T`. The engines' probes carry no
    /// substitution.
    fn refuse_chained_projection(
        &self,
        recv: &Expr,
        scope: &Scope,
        line: usize,
    ) -> Result<(), Diagnostic> {
        let Expr::Call { name, args, .. } = recv else {
            return Ok(());
        };
        if args.is_empty() {
            return Ok(());
        }
        let projection_shaped = name == crate::project::AT
            || (self.sigs.get(name.as_str()).is_none()
                && self
                    .impl_blocks
                    .iter()
                    .any(|i| i.places.iter().any(|p| p.name == *name)));
        if projection_shaped && self.chain_ty(recv, scope).is_none() {
            return Err(cerr!(
                line,
                "the inner projection's result is not a concrete named type \
                 here, so no engine can resolve the chain — bind the inner \
                 access with a `let`, then read through the binding"
            ));
        }
        Ok(())
    }

    /// The receiver type a chained projection resolves to, by the rules the
    /// engines' probes follow: a name's type, a projection's raw declared
    /// result when it keys concretely, or a builtin container's element.
    /// `None` when some engine cannot resolve it.
    fn chain_ty(&self, e: &Expr, scope: &Scope) -> Option<Type> {
        match e {
            Expr::Var { name, .. } => self.lookup(scope, name).map(|b| b.ty),
            Expr::Call { name, args, .. } if !args.is_empty() => {
                let m = if name == crate::project::AT {
                    "at"
                } else {
                    name.as_str()
                };
                let inner = self.chain_ty(&args[0], scope)?;
                if m == "at" {
                    if let Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) =
                        self.base(&inner)
                    {
                        return Some(*t);
                    }
                }
                let f = crate::project::lookup_in(self.impl_blocks, &inner, m).or_else(|| {
                    crate::project::lookup_in(self.impl_blocks, &self.base(&inner), m)
                })?;
                crate::types::type_key(&f.ret)?;
                Some(f.ret.clone())
            }
            _ => None,
        }
    }

    /// Types an `if let` scrutinee that is an optional projection call and
    /// records its expansion. Returns the declared `Option<T>` with the impl
    /// head solved from the receiver, or `None` for any other scrutinee. The
    /// pattern must be `Some(..)`.
    fn optional_scrutinee(
        &self,
        scrutinee: &Expr,
        pattern: &crate::ast::Pattern,
        scope: &Scope,
        fn_ret: &Type,
        line: usize,
    ) -> Result<Option<Type>, Diagnostic> {
        let Expr::Call { name, args, .. } = scrutinee else {
            return Ok(None);
        };
        if args.is_empty()
            || self.sigs.get(name.as_str()).is_some()
            || !self
                .impl_blocks
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            return Ok(None);
        }
        let recv = self.expr(&args[0], scope, None, Some(fn_ret))?;
        let found = crate::project::lookup_impl(self.impl_blocks, &recv, name)
            .or_else(|| crate::project::lookup_impl(self.impl_blocks, &self.base(&recv), name));
        let Some((imp, f)) = found else {
            return Ok(None);
        };
        if !crate::project::is_optional(f) {
            return Ok(None);
        }
        self.refuse_chained_projection(&args[0], scope, line)?;
        if !matches!(pattern, crate::ast::Pattern::Variant(v, _) if v == "Some") {
            return Err(cerr!(
                line,
                "an optional place is tested for its hit — write \
                 `if let Some(x) = ..{name}(..)`; the miss is the `else` arm"
            ));
        }
        let subst =
            self.solve_projection_call(imp, f, name, &recv, args, scope, Some(fn_ret), line)?;
        if recording() {
            if let Ok(Some(p)) = crate::project::optional_site(
                self.impl_blocks,
                Some(&recv),
                name,
                &args[0],
                &args[1..],
                line,
            ) {
                self.record_desugar(scope, |c, sc| {
                    sc.push(HashMap::new());
                    for s in &p.prologue {
                        if c.stmt(s, fn_ret, sc).is_err() {
                            return;
                        }
                    }
                    let _ = c.expr(&p.miss, sc, None, Some(fn_ret));
                    for s in &p.hit {
                        if c.stmt(s, fn_ret, sc).is_err() {
                            return;
                        }
                    }
                    let _ = c.expr(&p.place, sc, None, Some(fn_ret));
                });
            }
        }
        Ok(Some(crate::types::substitute(&f.ret, &subst)))
    }

    /// The type of a projection access on a user container: the declared
    /// result with the impl head solved from the receiver, the arguments
    /// checked. `None` for a builtin container or a type without the member.
    fn place_result(
        &self,
        recv: &Type,
        method: &str,
        args: &[Expr],
        scope: &Scope,
        fn_ret: Option<&Type>,
        line: usize,
    ) -> Result<Option<Type>, Diagnostic> {
        // Every `a[i]` reaches this; a builtin container leaves here.
        if crate::project::is_builtin_container(recv) || self.impl_blocks.is_empty() {
            return Ok(None);
        }
        // The declared type first, because the impl is keyed by its name;
        // then the base, for a validated type.
        let found = crate::project::lookup_impl(self.impl_blocks, recv, method)
            .or_else(|| crate::project::lookup_impl(self.impl_blocks, &self.base(recv), method));
        let Some((imp, f)) = found else {
            return Ok(None);
        };
        // An optional projection's `Option` exists only as the two arms of
        // the `if let` that tests it.
        if crate::project::is_optional(f) {
            return Err(cerr!(
                line,
                "an optional place is read where it is tested — write \
                 `if let Some(x) = ..{method}(..)`, or reach for the copying \
                 reader when the value must outlive the test"
            ));
        }
        let recv = recv.clone();
        let subst = self.solve_projection_call(imp, f, method, &recv, args, scope, fn_ret, line)?;
        Ok(Some(crate::types::substitute(&f.ret, &subst)))
    }

    /// Checks a projection call's arity and arguments, with the impl head
    /// solved against the receiver, and returns that substitution.
    #[allow(clippy::too_many_arguments)]
    fn solve_projection_call(
        &self,
        imp: &crate::ast::ImplBlock,
        f: &crate::ast::Function,
        name: &str,
        recv: &Type,
        args: &[Expr],
        scope: &Scope,
        fn_ret: Option<&Type>,
        line: usize,
    ) -> Result<HashMap<String, Type>, Diagnostic> {
        if args.len() - 1 != f.params.len() - 1 {
            return Err(cerr!(
                line,
                "projection `{name}` expects {} argument(s) besides `self`, got {}",
                f.params.len() - 1,
                args.len() - 1
            ));
        }
        let mut subst: HashMap<String, Type> = HashMap::new();
        self.unify(&imp.ty, recv, &mut subst, line)?;
        for (arg, p) in args[1..].iter().zip(&f.params[1..]) {
            let want = crate::types::substitute(&p.ty, &subst);
            let got = self.expr(arg, scope, Some(&want), fn_ret)?;
            if !self.coercible(&got, &want) {
                return Err(cerr!(
                    line,
                    "projection `{name}` argument is {got}, expected {want}"
                ));
            }
            self.prove_coercion(arg, &want, line)?;
        }
        Ok(subst)
    }

    /// Solves the impl head (`impl<T> .. for Slots<T>`) against the receiver
    /// and substitutes into `ty`. For `c[k] = v` and `for x in c`, which do not
    /// go through [`Self::place_result`].
    fn solve_head(&self, imp: &crate::ast::ImplBlock, recv: &Type, ty: &Type, line: usize) -> Type {
        let mut subst: HashMap<String, Type> = HashMap::new();
        match self.unify(&imp.ty, recv, &mut subst, line) {
            Ok(()) => crate::types::substitute(ty, &subst),
            Err(_) => ty.clone(),
        }
    }

    /// The first type in `ty`, itself or a part, that declares `impl Owned`.
    /// A record holding one cannot be copied field by field: the
    /// declared `release` would run over both copies. Do not bound the depth;
    /// `copy`'s refusal has no bound.
    fn declared_owned_in(
        &self,
        ty: &Type,
        seen: &mut std::collections::HashSet<String>,
    ) -> Option<String> {
        if let Some(k) = crate::types::type_key(ty) {
            if self
                .impls
                .contains(&(crate::types::OWNED.to_string(), k.clone()))
            {
                return Some(k);
            }
        }
        // Only a declaration head can close a cycle. Key it by its full shape:
        // a bare constructor would let `Option<Int64>` mask `Option<Ring>`.
        if matches!(ty, Type::Named(_) | Type::App(..)) && !seen.insert(format!("{ty:?}")) {
            return None;
        }
        let mut deeper = |t: &Type| self.declared_owned_in(t, seen);
        match self.base(ty) {
            Type::ArrayN(t, _)
            | Type::Array(t)
            | Type::SmallArray(t, _)
            | Type::Lazy(t)
            | Type::Stream(t) => deeper(&t),
            Type::Map(k, v) => deeper(&k).or_else(|| deeper(&v)),
            Type::Record(fs) => fs.iter().find_map(|f| deeper(&f.ty)),
            Type::Enum(vs) => vs
                .iter()
                .find_map(|v| v.payload.iter().find_map(|p| deeper(p))),
            _ => None,
        }
    }

    fn enum_type_params(&self, enum_name: &str) -> Vec<String> {
        self.types
            .get(enum_name)
            .map(|d| d.type_params.clone())
            .unwrap_or_default()
    }

    /// Whether any part of `ty` answers [`Reach::Yes`]. A named type is read
    /// through its type arguments and its declaration; each declaration head
    /// is entered once, so a recursive type terminates. A `fn` type is never
    /// descended: `at` decides at it.
    fn reaches(&self, ty: &Type, at: &dyn Fn(&Type) -> Reach) -> bool {
        fn go(
            ty: &Type,
            types: &HashMap<String, TypeDecl>,
            at: &dyn Fn(&Type) -> Reach,
            seen: &mut Vec<String>,
        ) -> bool {
            match at(ty) {
                Reach::Yes => return true,
                Reach::No => return false,
                Reach::Parts => {}
            }
            match ty {
                Type::Array(i)
                | Type::ArrayN(i, _)
                | Type::SmallArray(i, _)
                | Type::Partial(i)
                | Type::Stream(i)
                | Type::Lazy(i)
                | Type::Omit(i, _)
                | Type::Pick(i, _) => go(i, types, at, seen),
                Type::Map(a, b) | Type::Merge(a, b) => {
                    go(a, types, at, seen) || go(b, types, at, seen)
                }
                Type::Record(fs) => fs.iter().any(|f| go(&f.ty, types, at, seen)),
                Type::Enum(vs) => vs
                    .iter()
                    .any(|v| v.payload.iter().any(|p| go(p, types, at, seen))),
                Type::Named(n) | Type::App(n, _) => {
                    let args = match ty {
                        Type::App(_, a) => a.as_slice(),
                        _ => &[],
                    };
                    args.iter().any(|a| go(a, types, at, seen))
                        || (!seen.iter().any(|s| s == n)
                            && types.get(n).is_some_and(|d| {
                                seen.push(n.clone());
                                let r = go(&d.base, types, at, seen);
                                seen.pop();
                                r
                            }))
                }
                _ => false,
            }
        }
        go(ty, self.types, at, &mut Vec::new())
    }

    /// Whether `ty` contains a `Stream<T>` anywhere, through named types and
    /// type arguments.
    fn contains_stream(&self, ty: &Type) -> bool {
        self.reaches(ty, &|t| match t {
            Type::Stream(_) => Reach::Yes,
            // A `fn` type stores no stream: its parameters and return are
            // legal stream positions (`std/http`'s `Feed`).
            Type::Fn(_, _) => Reach::No,
            // A `lazy T` stores a T.
            _ => Reach::Parts,
        })
    }

    /// Whether `ty` contains a function-value type anywhere, for
    /// the positions a function value may not take.
    fn contains_fn(&self, ty: &Type) -> bool {
        self.reaches(ty, &|t| match t {
            // A `lazy T` field is a function value.
            Type::Fn(..) | Type::Lazy(_) => Reach::Yes,
            // A `Stream<T>`'s element is not looked at, unlike the other
            // walks. Looking would refuse programs that check today.
            Type::Stream(_) => Reach::No,
            _ => Reach::Parts,
        })
    }

    fn assignable(&self, from: &Type, to: &Type) -> bool {
        crate::types::assignable(from, to, self.types)
    }

    /// Whether `ty` mentions an open type parameter: one that is not the
    /// current function's own. An open parameter in an expectation says
    /// nothing, so the value decides: `Deque { front: [2, 1] }` passes
    /// `Array<T>` down with `T` unsolved.
    fn mentions_open_param(&self, ty: &Type) -> bool {
        !self.open_params(ty).is_empty()
    }

    /// The open parameters in `ty`, by name, in order, without repeats.
    fn open_params(&self, ty: &Type) -> Vec<String> {
        let cur = self.cur_fn.borrow();
        let rigid = self.generics.get(cur.as_str());
        let mut out: Vec<String> = Vec::new();
        walk_type(ty, &mut |t| {
            if let Type::Param(n) = t {
                if !rigid.is_some_and(|ps| ps.contains(n)) && !out.contains(n) {
                    out.push(n.clone());
                }
            }
        });
        out
    }

    /// Whether `ty` is itself an open type parameter. A compound that only
    /// mentions one (`Array<T>`) is not: a backend builds a literal inside
    /// out, so a nested element's type must be known first.
    fn is_open_param(&self, ty: &Type) -> bool {
        matches!(ty, Type::Param(_)) && self.mentions_open_param(ty)
    }

    /// Whether a key of type `k` fits a map keyed by `key`, both at their base.
    fn key_fits(&self, k: &Type, key: &Type) -> bool {
        self.coercible(&self.base(k), &self.base(key))
    }

    fn coercible(&self, from: &Type, to: &Type) -> bool {
        crate::types::coercible(from, to, self.types)
    }

    /// Refuses a constant, or a record literal of constants, that fails the
    /// predicate of the named type it flows into. Other values keep the
    /// runtime check.
    fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {
        let decl = match to {
            Type::Named(n) => match self.types.get(n) {
                Some(d) if d.predicate.is_some() => d,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let pred = decl.predicate.as_ref().unwrap();
        match crate::validate::constant_verdict(expr, decl) {
            Some((false, Some(cv))) => Err(cerr!(
                line,
                "{cv} does not satisfy `{}` (predicate `where {}` is false)",
                decl.name,
                pred_summary(pred),
            )),
            Some((false, None)) => Err(cerr!(
                line,
                "this value does not satisfy `{}` (predicate `where {}` is false)",
                decl.name,
                pred_summary(pred),
            )),
            _ => Ok(()),
        }
    }

    /// Refuses an interpolation or finite-string value that can fall outside
    /// the language of the validated string type it flows into, naming the
    /// shortest witness. Anything else keeps the runtime check.
    fn prove_string_interpolation(
        &self,
        expr: &Expr,
        to: &Type,
        scope: &Scope,
        fn_ret: Option<&Type>,
        line: usize,
    ) -> Result<(), Diagnostic> {
        let resolve = |e: &Expr| self.expr(e, scope, None, fn_ret).ok();
        match crate::finite::prove_string_flow(expr, to, self.types, &resolve) {
            crate::finite::Proof::Witness(witness) => {
                // A witness implies a named predicated target.
                let decl = match to {
                    Type::Named(n) => self.types.get(n).unwrap(),
                    _ => unreachable!("witness implies a named target"),
                };
                let pred = decl.predicate.as_ref().unwrap();
                Err(cerr!(
                    line,
                    "\"{witness}\" (a possible value of this interpolation) \
                     does not satisfy `{}` (predicate `where {}` is false)",
                    decl.name,
                    pred_summary(pred),
                ))
            }
            crate::finite::Proof::Proven | crate::finite::Proof::NotApplicable => Ok(()),
        }
    }

    /// Refuses a `Stream` in a record field or enum payload, which
    /// [`Self::ensure_type_exists`] sees at root position.
    fn ensure_no_stream(&self, ty: &Type, line: usize, where_: &str) -> Result<(), Diagnostic> {
        if self.contains_stream(ty) {
            return Err(cerr!(
                line,
                "`{ty}` may not be {where_} — a stream's lifetime is a scope, \
                 so it may be a binding, a parameter, or a return type, and nothing may \
                 store it"
            ));
        }
        Ok(())
    }

    fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), Diagnostic> {
        // A stream's lifetime is a scope: legal only at the root of
        // a binding, parameter or return type, where movecheck can see it.
        if !matches!(self.base(ty), Type::Stream(_)) && self.contains_stream(ty) {
            return Err(cerr!(
                line,
                "`{ty}` holds a `Stream`, but a stream's lifetime is a scope — \
                 it may be a binding, a parameter, or a return type, and nothing may \
                 store it"
            ));
        }
        match ty {
            // `Code` and `Token` are builtin and generation-only, so no backend
            // sees them. A user declaration of the name wins.
            Type::Named(n) if n == "Code" && !self.types.contains_key("Code") => {
                if !*self.in_gen.borrow() {
                    return Err(cerr!(
                        line,
                        "the `Code` type is only available during generation"
                    ));
                }
                return Ok(());
            }
            Type::Named(n) if n == "Token" && !self.types.contains_key("Token") => {
                if !*self.in_gen.borrow() {
                    return Err(cerr!(
                        line,
                        "the `Token` type is only available during generation"
                    ));
                }
                return Ok(());
            }
            // `Self` parses as an ordinary name and is not a type. Refused here,
            // so the diagnostic lands on the protocol, not on each impl.
            Type::Named(n) if n == "Self" && !self.types.contains_key("Self") => {
                return Err(cerr!(
                    line,
                    "`Self` is not a type in Vyrn — a protocol that must name the \
                     implementing type declares an associated type instead: `protocol P {{ type \
                     Out  fn m(self) -> Out }}`, and each impl binds it with `type Out = ..`"
                ))
            }
            Type::Named(n) => match self.types.get(n) {
                None => return Err(cerr!(line, "unknown type `{n}`")),
                Some(d) if !d.type_params.is_empty() => {
                    return Err(cerr!(
                        line,
                        "`{n}` is generic; write `{n}<...>` with type arguments"
                    ))
                }
                _ => {}
            },
            Type::App(name, args) => {
                // Only `SmallArray` takes an integer argument. Checked before
                // arity, so `Box<3>` gets the right diagnostic.
                if args.iter().any(|a| matches!(a, Type::ConstInt(_))) {
                    return Err(cerr!(line, "type {name} does not take an integer argument"));
                }
                let d = self
                    .types
                    .get(name)
                    .ok_or_else(|| cerr!(line, "unknown type `{name}`"))?;
                if d.type_params.len() != args.len() {
                    return Err(cerr!(
                        line,
                        "`{name}` takes {} type argument(s), got {}",
                        d.type_params.len(),
                        args.len()
                    ));
                }
                for a in args {
                    self.ensure_type_exists(a, line)?;
                }
            }
            Type::Record(fields) => {
                for f in fields {
                    self.ensure_type_exists(&f.ty, line)?;
                }
            }
            Type::Omit(base, keys) | Type::Pick(base, keys) => {
                self.ensure_type_exists(base, line)?;
                let fields = crate::types::record_fields(base, self.types)
                    .ok_or_else(|| cerr!(line, "the transformer's base must be a record type"))?;
                for k in keys {
                    if !fields.iter().any(|f| &f.name == k) {
                        return Err(cerr!(
                            line,
                            "field `{k}` is not in the transformer's base record"
                        ));
                    }
                }
            }
            Type::Merge(a, b) => {
                self.ensure_type_exists(a, line)?;
                self.ensure_type_exists(b, line)?;
                if crate::types::record_fields(a, self.types).is_none()
                    || crate::types::record_fields(b, self.types).is_none()
                {
                    return Err(cerr!(line, "`Merge` requires two record types"));
                }
            }
            Type::Partial(base) => {
                self.ensure_type_exists(base, line)?;
                if crate::types::record_fields(base, self.types).is_none() {
                    return Err(cerr!(line, "`Partial` requires a record type"));
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for p in &v.payload {
                        self.ensure_type_exists(p, line)?;
                    }
                }
            }
            Type::Array(inner) | Type::ArrayN(inner, _) => self.ensure_type_exists(inner, line)?,
            Type::Stream(inner) => self.ensure_type_exists(inner, line)?,
            // A `lazy T` stores a T, so T's rules apply through it.
            Type::Lazy(inner) => self.ensure_type_exists(inner, line)?,
            // The inline capacity is bounded to keep the inline footprint small.
            Type::SmallArray(inner, n) => {
                if *n < 1 || *n > 64 {
                    return Err(cerr!(line, "smallArray capacity must be between 1 and 64"));
                }
                self.ensure_type_exists(inner, line)?;
            }
            Type::ConstInt(_) => {
                return Err(cerr!(
                    line,
                    "an integer is not a type; only `SmallArray<T, N>` \
                     takes an integer argument"
                ))
            }
            // A key is `String`, `Int64`, or a heapless user type that
            // declares `impl Hashable`. Floats are refused by name.
            Type::Map(key, val) => {
                self.ensure_type_exists(key, line)?;
                self.ensure_type_exists(val, line)?;
                match crate::types::resolve(key, self.types) {
                    Type::Str | Type::Int => {}
                    shape @ (Type::Float | Type::Float32 | Type::Record(_) | Type::Enum(_)) => {
                        self.check_key_shape(key, &shape, line)?;
                        if !crate::types::hashable_impl(self.impl_blocks, key) {
                            return Err(cerr!(
                                line,
                                "`{key}` can be a `Map` key once it declares the obligation: `impl Hashable for {key}` — equal values must return equal hashes"
                            ));
                        }
                    }
                    _ => {
                        return Err(cerr!(
                            line,
                            "a `Map` key is `String`, `Int64`, or a heapless `Hashable` type, found `{key}`"
                        ));
                    }
                }
            }
            // The parser tags only names declared in `<...>` as parameters.
            Type::Param(_) => {}
            // A function type may not take or return a function value. The
            // `extern` and `gen` signature refusals sit at their own sites.
            Type::Fn(ptys, ret) => {
                for p in ptys {
                    if self.contains_fn(p) {
                        return Err(cerr!(
                            line,
                            "a function type may not take another function value"
                        ));
                    }
                    self.ensure_type_exists(p, line)?;
                }
                if self.contains_fn(ret) {
                    return Err(cerr!(
                        line,
                        "a function type may not return another function value"
                    ));
                }
                self.ensure_type_exists(ret, line)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Returns every unknown type in a protocol's method signatures. An
    /// associated type arrives as a [`Type::Param`], which is admitted.
    fn check_protocol_decl(&self, p: &ProtocolDecl) -> Vec<Diagnostic> {
        let mut errs = Vec::new();
        for m in &p.methods {
            for t in m.params.iter().chain(std::iter::once(&m.ret)) {
                if let Err(e) = self.ensure_type_exists(t, m.line) {
                    errs.push(e);
                }
            }
        }
        errs
    }

    /// Returns every problem in a contract's members: an unknown
    /// member type, or a default that does not have the member's type (a `fn`
    /// member's return type).
    fn check_contract_decl(&self, c: &ContractDecl) -> Vec<Diagnostic> {
        let mut errs = Vec::new();
        for m in &c.members {
            let where_ = format!("contract `{}` member `{}`", c.name, m.name);
            match &m.kind {
                ContractMemberKind::Value { ty, default } => {
                    if let Err(e) = self.ensure_type_exists(ty, m.line) {
                        errs.push(e);
                        continue;
                    }
                    self.check_member_default(default.as_deref(), ty, &where_, m.line, &mut errs);
                }
                ContractMemberKind::Fn {
                    params,
                    ret,
                    default,
                    ..
                } => {
                    for p in params {
                        if let Err(e) = self.ensure_type_exists(p, m.line) {
                            errs.push(e);
                        }
                    }
                    if *ret != Type::Unit {
                        if let Err(e) = self.ensure_type_exists(ret, m.line) {
                            errs.push(e);
                            continue;
                        }
                    }
                    self.check_member_default(default.as_deref(), ret, &where_, m.line, &mut errs);
                }
            }
        }
        errs
    }

    /// Checks a contract member's default in an empty scope against the member
    /// type. A member type with a type parameter (`Query<T>`) admits any
    /// instantiation, so the default is checked but not compared.
    fn check_member_default(
        &self,
        default: Option<&Expr>,
        ty: &Type,
        where_: &str,
        line: usize,
        errs: &mut Vec<Diagnostic>,
    ) {
        let Some(d) = default else { return };
        let scope = Scope::closed();
        let open = type_mentions_param(ty);
        let expected = if open { None } else { Some(ty) };
        match self.expr(d, &scope, expected, None) {
            Err(e) => errs.push(e),
            Ok(vty) if !open && !self.coercible(&vty, ty) => errs.push(cerr!(
                line,
                "{where_} defaults to {vty}, but is declared `{ty}`"
            )),
            Ok(_) => {}
        }
    }

    fn check_type_decl(&self, t: &TypeDecl) -> Result<(), Diagnostic> {
        if t.predicate.is_some() && self.contains_fn(&t.base) {
            return Err(cerr!(
                t.line,
                "a function type cannot carry a `where` predicate"
            ));
        }
        // A record's `where` clause names its fields: a cross-field invariant
        // checked at construction.
        if let Type::Record(fields) = &t.base {
            let mut seen = std::collections::HashSet::new();
            for f in fields {
                if !seen.insert(&f.name) {
                    return Err(cerr!(
                        t.line,
                        "duplicate field `{}` in record `{}`",
                        f.name,
                        t.name
                    ));
                }
                self.ensure_no_stream(&f.ty, t.line, "a record field")?;
                self.ensure_type_exists(&f.ty, t.line)?;
            }
            let binds = fields.iter().map(|f| (f.name.clone(), f.ty.clone()));
            self.check_predicate(t, "cross-field", binds)?;
            return Ok(());
        }
        if let Some(vs) = crate::types::declared_variants(&t.base) {
            if t.predicate.is_some() {
                return Err(cerr!(t.line, "an enum type cannot have a `where` clause"));
            }
            if vs.is_empty() {
                return Err(cerr!(t.line, "enum `{}` has no variants", t.name));
            }
            for v in vs {
                for p in &v.payload {
                    self.ensure_no_stream(p, t.line, "an enum payload")?;
                    self.ensure_type_exists(p, t.line)?;
                }
            }
            return Ok(());
        }
        // A transparent alias to `Option`, `Result`, `Map` or `Array`, so a
        // codable one can be named for `fromJson`. No `where` clause: the
        // elements carry their own.
        let wrapper = match &t.base {
            b if crate::types::is_sum_alias(b) => {
                Some(match crate::types::result_payloads(b).is_some() {
                    true => "Result",
                    false => "Option",
                })
            }
            Type::Map(..) => Some("Map"),
            Type::Array(_) | Type::ArrayN(..) => Some("Array"),
            _ => None,
        };
        if let Some(noun) = wrapper {
            if t.predicate.is_some() {
                return Err(cerr!(
                    t.line,
                    "a `{noun}` alias cannot have a `where` clause"
                ));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            return Ok(());
        }
        // A function type alias is interchangeable with its structural form.
        if matches!(t.base, Type::Fn(..)) {
            self.ensure_type_exists(&t.base, t.line)?;
            return Ok(());
        }
        if matches!(
            t.base,
            Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..)
        ) {
            if t.predicate.is_some() {
                return Err(cerr!(t.line, "a record type cannot have a `where` clause"));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            if crate::types::record_fields(&t.base, self.types).is_none() {
                return Err(cerr!(t.line, "`{}` does not resolve to a record", t.name));
            }
            return Ok(());
        }
        if !matches!(
            t.base,
            Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
        ) {
            return Err(cerr!(
                t.line,
                "`{}` must have a scalar base (Int64, sized int, Float64, Bool, or String)",
                t.name
            ));
        }
        // A refinement sees `value` at the base type.
        self.check_predicate(
            t,
            "refinement",
            std::iter::once(("value".to_string(), t.base.clone())),
        )
    }

    /// Checks a type's `where` predicate: no call, and Bool in a scope holding
    /// only `binds`. `kind` names it in a refusal ("cross-field" or
    /// "refinement").
    fn check_predicate(
        &self,
        t: &TypeDecl,
        kind: &str,
        binds: impl Iterator<Item = (String, Type)>,
    ) -> Result<(), Diagnostic> {
        let Some(pred) = &t.predicate else {
            return Ok(());
        };
        if consteval::contains_call(pred) {
            return Err(cerr!(
                t.line,
                "{kind} predicate for `{}` may not contain calls (v0.1)",
                t.name
            ));
        }
        let mut scope = Scope::closed();
        for (name, ty) in binds {
            scope[0].insert(name, Binding { ty, mutable: false });
        }
        let pty = self.expr(pred, &scope, None, None)?;
        if self.base(&pty) != Type::Bool {
            return Err(cerr!(
                t.line,
                "{kind} predicate for `{}` must be Bool, found {pty}",
                t.name
            ));
        }
        Ok(())
    }

    // ---- functions / statements ----------------------------------------

    /// Whether the current function's parameter `t` carries `bound`, where
    /// `Num` implies `Ord` and `Ord` implies `Eq`.
    fn param_has_bound(&self, t: &str, bound: &str) -> bool {
        let bounds = self.cur_bounds.borrow();
        let bs = match bounds.get(t) {
            Some(b) => b,
            None => return false,
        };
        bs.iter().any(|b| match bound {
            "Eq" => b == "Eq" || b == "Ord" || b == "Num",
            "Ord" => b == "Ord" || b == "Num",
            "Num" => b == "Num",
            other => b == other,
        })
    }

    /// Whether a concrete type satisfies a built-in bound.
    fn type_satisfies(&self, ty: &Type, bound: &str) -> bool {
        let base = self.base(ty);
        match bound {
            // What `print` and `@str` take: a type the language renders, or one
            // with an `impl Show`. The emitter checks `renders` first, so a
            // scalar needs no impl.
            crate::types::SHOW => match &base {
                // The impl is selected per specialization.
                Type::Param(p) => self.param_has_bound(p, bound),
                _ => crate::types::renders(&base) || self.declares_an_impl(ty, &base, bound),
            },
            "Num" | "Ord" => matches!(
                base,
                Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
            ),
            "Eq" => matches!(
                base,
                Type::Int
                    | Type::Float
                    | Type::Float32
                    | Type::IntN { .. }
                    | Type::Bool
                    | Type::Str
            ),
            // A user protocol: satisfied if the type implements it.
            _ if self
                .protocol_methods
                .values()
                .any(|entries| entries.iter().any(|(p, _)| p == bound))
                || self.impls.iter().any(|(p, _)| p == bound) =>
            {
                self.declares_an_impl(ty, &base, bound)
            }
            _ => false,
        }
    }

    /// Whether `ty` or its base declares an `impl <bound>`. The type's own key
    /// comes first: a record type resolves to a `Type::Record`, which has no
    /// key. The base serves a plain alias (`type Meters = Int64`).
    fn declares_an_impl(&self, ty: &Type, base: &Type, bound: &str) -> bool {
        [crate::types::type_key(ty), crate::types::type_key(base)]
            .into_iter()
            .flatten()
            .any(|k| self.impls.contains(&(bound.to_string(), k)))
    }

    /// Refuses an `extern` signature type outside the host ABI:
    /// integers, floats, `Bool`, `String`, and `Unit` as a return. The first
    /// refusal is the `Err`; the rest stay in `errors`.
    fn check_extern_sig(&self, f: &Function) -> Result<(), Diagnostic> {
        self.errors.borrow_mut().clear();
        for p in &f.params {
            if !extern_abi_type_ok(&p.ty, false) {
                self.errors.borrow_mut().push(cerr_at!(
                    f.line,
                    f.name_span(),
                    "extern fn `{}` parameter `{}` has type {}, which cannot cross \
                     the JS boundary (allowed: Int64, sized ints, Float64, Float32, Bool, String)",
                    f.name,
                    p.name,
                    p.ty
                ));
            }
            // The JS caller frees a String argument when the call returns
            // (`wasi-min.js`), so `consume` would deliver a dangling pointer.
            if matches!(p.capability, Capability::Consume) && matches!(p.ty, Type::Str) {
                self.errors.borrow_mut().push(cerr_at!(
                    f.line,
                    f.name_span(),
                    "extern fn `{}` parameter `{}` may not be `consume` — the caller \
                     across this boundary is JS, and it releases the String when the call \
                     returns\n  fix: take `{}: String` and store `{}.copy()`",
                    f.name,
                    p.name,
                    p.name,
                    p.name
                ));
            }
        }
        if !extern_abi_type_ok(&f.ret, true) {
            self.errors.borrow_mut().push(cerr_at!(
                f.line,
                f.name_span(),
                "extern fn `{}` returns {}, which cannot cross the JS boundary \
                 (allowed: Int64, sized ints, Float64, Float32, Bool, String, Unit)",
                f.name,
                f.ret
            ));
        }
        let mut errs = self.errors.borrow_mut();
        if let Some(first) = errs.first().cloned() {
            let rest: Vec<Diagnostic> = errs.drain(1..).collect();
            *errs = rest;
            Err(first)
        } else {
            Ok(())
        }
    }

    /// Checks every global in declaration order and records its type in
    /// `self.globals`. An initializer sees only earlier globals. A failed one
    /// binds as `Type::Err` and joins `refused`. Returns how many diagnostics
    /// the initializers gave.
    fn check_globals(
        &self,
        program: &Program,
        out: &mut Vec<Diagnostic>,
        refused: &mut HashSet<String>,
    ) -> usize {
        let before = out.len();
        // No initializer may call an `extern` function (the host is not ready
        // before `main`) or a protocol method. An ordinary function is legal
        // only from another module, which initializes first.
        let mut forbidden: HashSet<String> = program
            .functions
            .iter()
            .filter(|f| f.is_extern || f.is_export_extern)
            .map(|f| f.name.clone())
            .collect();
        for p in &program.protocols {
            for m in &p.methods {
                forbidden.insert(m.name.clone());
            }
        }
        let fn_module: HashMap<String, Option<String>> = program
            .functions
            .iter()
            .filter(|f| !f.is_extern && !f.is_export_extern)
            .map(|f| (f.name.clone(), f.module.clone()))
            .collect();
        let all_globals: HashSet<&str> = program.globals.iter().map(|g| g.name.as_str()).collect();
        let mut ready: HashSet<String> = HashSet::new();
        for g in &program.globals {
            // A literal initializer's range error names the global's line.
            *self.stmt_line.borrow_mut() = g.line;
            let bty = self.unit(|| -> Result<Type, Diagnostic> {
                // Before typing, so the refusal names the call or read.
                init_restrictions(
                    &g.init,
                    &forbidden,
                    &fn_module,
                    &g.module,
                    &all_globals,
                    &ready,
                    &g.name,
                    g.line,
                )?;
                if let Some(declared) = &g.ty {
                    self.ensure_type_exists(declared, g.line)?;
                }
                // `self.globals` holds exactly the earlier globals here.
                let scope = Scope::open();
                let vty = self.expr(&g.init, &scope, g.ty.as_ref(), None)?;
                if self.base(&vty) == Type::Unit {
                    return Err(cerr!(
                        g.line,
                        "cannot bind module state `{}` to a Unit value",
                        g.name
                    ));
                }
                if matches!(self.base(&vty), Type::Stream(_)) {
                    return Err(cerr!(
                        g.line,
                        "module state `{}` may not be a `Stream` — a stream's \
                         lifetime is a scope, and module state is never dropped",
                        g.name
                    ));
                }
                if let Some(declared) = &g.ty {
                    if !self.coercible(&vty, declared) {
                        return Err(cerr!(
                            g.line,
                            "`{}` declared {declared} but initializer is {vty}",
                            g.name
                        ));
                    }
                    self.prove_coercion(&g.init, declared, g.line)?;
                }
                Ok(g.ty.clone().unwrap_or(vty))
            });
            let binding = match bty {
                Ok(t) => Binding {
                    ty: t,
                    mutable: g.mutable,
                },
                Err(s) => {
                    out.extend(s.map(|mut d| {
                        d.file = g.module.clone();
                        d
                    }));
                    refused.insert(g.name.clone());
                    Binding {
                        ty: Type::Err,
                        mutable: g.mutable,
                    }
                }
            };
            // A block lambda in the initializer checks its statements into
            // `errors`, which the first function's check clears (#554). A
            // refused statement there refuses the binding and drops its type.
            let lambda: Vec<Diagnostic> = self.errors.borrow_mut().drain(..).collect();
            if !lambda.is_empty() {
                refused.insert(g.name.clone());
                let key = &g.init as *const Expr as usize;
                RECORD.with(|r| r.borrow_mut().node_types.remove(&key));
            }
            out.extend(lambda.into_iter().map(|mut d| {
                d.file = g.module.clone();
                d
            }));
            self.globals.borrow_mut().insert(g.name.clone(), binding);
            ready.insert(g.name.clone());
        }
        out.len() - before
    }

    fn function(&self, f: &Function) -> Result<(), Diagnostic> {
        self.function_body(f, &f.body)
    }

    /// Records a root-module binding's type for the editor, at its binder's
    /// position. The first answer stands: a desugar re-types a copy.
    fn bind_seen(&self, ty: Option<Type>, line: usize, col: usize) {
        let Some(ty) = ty else { return };
        if col == 0 || !self.in_root.get() {
            return;
        }
        self.binder_types
            .borrow_mut()
            .entry((line, col))
            .or_insert(ty);
    }

    /// Checks `f` with `body` as its body. A `test` or `bench` has a synthetic
    /// head and its real body node, because the record is keyed by address.
    fn function_body(&self, f: &Function, body: &Block) -> Result<(), Diagnostic> {
        *self.cur_bounds.borrow_mut() = f.type_bounds.clone();
        *self.cur_fn.borrow_mut() = f.name.clone();
        *self.in_gen.borrow_mut() = in_gen_of(f);
        self.in_root.set(f.module.is_none());
        self.errors.borrow_mut().clear();
        // A local shadows a global of the same name.
        let mut scope = Scope::open();
        scope.push(HashMap::new());
        for p in &f.params {
            let mutable = p.capability == Capability::Modify;
            self.bind_seen(Some(p.ty.clone()), p.line, p.col);
            scope.last_mut().unwrap().insert(
                p.name.clone(),
                Binding {
                    ty: p.ty.clone(),
                    mutable,
                },
            );
        }
        self.block(body, &f.ret, &mut scope);
        // The first error is the `Err`; the rest stay in `errors`.
        let mut errs = self.errors.borrow_mut();
        if let Some(first) = errs.first().cloned() {
            let rest: Vec<Diagnostic> = errs.drain(1..).collect();
            *errs = rest;
            Err(first)
        } else {
            Ok(())
        }
    }

    /// Types an expansion (an inlined projection, a `for` over a user
    /// container) in the caller's scope, recording its node types and nothing
    /// else. `project` leaks each expansion once ([`crate::project::Memo`]), so
    /// its node addresses are stable keys.
    ///
    /// Diagnostics, scope changes and [`PENDING_SUBST`] stay inside: an
    /// expansion fails only where its source already did, and the wrapper
    /// reads `PENDING_SUBST` for the access site's own node.
    fn record_desugar(&self, scope: &Scope, run: impl FnOnce(&Self, &mut Scope)) {
        let mark = self.errors.borrow().len();
        let saved = PENDING_SUBST.with(|s| s.borrow_mut().take());
        let was = DESUGARING.with(|c| c.replace(true));
        let mut sc = scope.clone();
        run(self, &mut sc);
        DESUGARING.with(|c| c.set(was));
        PENDING_SUBST.with(|s| *s.borrow_mut() = saved);
        self.errors.borrow_mut().truncate(mark);
    }

    fn block(&self, block: &Block, ret: &Type, scope: &mut Scope) {
        scope.push(HashMap::new());
        for stmt in &block.stmts {
            if let Err(msg) = self.unit(|| self.stmt(stmt, ret, scope)) {
                self.errors.borrow_mut().extend(msg);
                self.recover_binding(stmt, scope);
            }
        }
        scope.pop();
    }

    /// Runs one unit of checking: a statement, a type declaration or a
    /// global's initializer. A refusal in a unit that typed a node
    /// [`Checker::judged`] or read a name typed `Type::Err` is `Err(None)`:
    /// the typed judgment or the binding statement states it.
    fn unit<T>(&self, f: impl FnOnce() -> Result<T, Diagnostic>) -> Result<T, Option<Diagnostic>> {
        let outer = self.unknown.replace(false);
        let r = f();
        let read = self.unknown.replace(outer);
        r.map_err(|d| Some(d).filter(|_| !read))
    }

    /// Types a node whose refusal the typed judgment states as `Err`, and marks
    /// the unit's other refusals as following from it.
    fn judged(&self) -> Result<Type, Diagnostic> {
        self.unknown.set(true);
        Ok(Type::Err)
    }

    /// Binds the name of a failed `let` or `for` to `Type::Err`, so later uses
    /// do not cascade "unknown variable".
    fn recover_binding(&self, stmt: &Stmt, scope: &mut Scope) {
        match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                line,
                col,
                ..
            } => {
                // Hover still shows the annotation.
                self.bind_seen(ty.clone(), *line, *col);
                scope.last_mut().unwrap().insert(
                    name.clone(),
                    Binding {
                        ty: Type::Err,
                        mutable: *mutable,
                    },
                );
            }
            Stmt::ForIn { var, .. } => {
                // The failed arm never pushed the loop frame; bind in the block's.
                scope.last_mut().unwrap().insert(
                    var.clone(),
                    Binding {
                        ty: Type::Err,
                        mutable: false,
                    },
                );
            }
            _ => {}
        }
    }

    fn stmt(&self, stmt: &Stmt, ret: &Type, scope: &mut Scope) -> Result<(), Diagnostic> {
        *self.stmt_line.borrow_mut() = stmt.line();
        match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                value,
                line,
                col,
            } => {
                if let Some(declared) = ty {
                    self.ensure_type_exists(declared, *line)?;
                }
                let vty = self.expr(value, scope, ty.as_ref(), Some(ret))?;
                // The typed judgment refuses a value its slot does not take,
                // and a Unit bound to a name.
                if let Some(declared) = ty.as_ref().filter(|d| self.coercible(&vty, d)) {
                    self.prove_coercion(value, declared, *line)?;
                    self.prove_string_interpolation(value, declared, scope, Some(ret), *line)?;
                }
                // A refused binding is typed `Err`, so its uses add no refusal.
                let bty = match ty {
                    Some(t) if self.coercible(&vty, t) => t.clone(),
                    Some(_) => Type::Err,
                    None if self.base(&vty) == Type::Unit => Type::Err,
                    None => vty,
                };
                self.bind_seen(Some(bty.clone()), *line, *col);
                scope.last_mut().unwrap().insert(
                    name.clone(),
                    Binding {
                        ty: bty,
                        mutable: *mutable,
                    },
                );
                Ok(())
            }
            Stmt::Assign { name, value, line } => {
                let Some(b) = self.lookup(scope, name) else {
                    self.unknown.set(true);
                    return Ok(());
                };
                let vty = self.expr(value, scope, Some(&b.ty), Some(ret))?;
                if self.coercible(&vty, &b.ty) {
                    self.prove_coercion(value, &b.ty, *line)?;
                    self.prove_string_interpolation(value, &b.ty, scope, Some(ret), *line)?;
                }
                self.region_store_guard(name, &b.ty, scope, *line)?;
                Ok(())
            }
            // The typed judgment refuses a field or element store; where one
            // of its rules fails, this stops unrefused.
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                let Some(b) = self.lookup(scope, name) else {
                    self.unknown.set(true);
                    return Ok(());
                };
                if matches!(&b.ty, Type::Named(n) if self.types.get(n).is_some_and(|d| d.predicate.is_some()))
                {
                    return Ok(());
                }
                let Some(fty) = crate::types::record_fields(&b.ty, self.types)
                    .and_then(|fs| fs.into_iter().find(|f| &f.name == field))
                    .map(|f| f.ty)
                else {
                    return Ok(());
                };
                let vty = self.expr(value, scope, Some(&fty), Some(ret))?;
                // A predicated field takes only a value of its own type.
                let validated = matches!(&fty, Type::Named(n)
                    if self.types.get(n).is_some_and(|d| d.predicate.is_some()));
                if !(if validated {
                    self.assignable(&vty, &fty)
                } else {
                    self.coercible(&vty, &fty)
                }) {
                    return Ok(());
                }
                self.region_store_guard(name, &fty, scope, *line)?;
                Ok(())
            }
            // `name[index] = value`, in place.
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                let Some(b) = self.lookup(scope, name) else {
                    self.unknown.set(true);
                    return Ok(());
                };
                if let Type::Map(key, val) = self.base(&b.ty) {
                    let k = self.base(&self.expr(index, scope, Some(&key), Some(ret))?);
                    if !matches!(k, Type::Err) && !self.key_fits(&k, &key) {
                        return Ok(());
                    }
                    self.prove_coercion(index, &key, *line)?;
                    let vty = self.expr(value, scope, Some(&val), Some(ret))?;
                    if !self.coercible(&vty, &val) {
                        return Ok(());
                    }
                    self.prove_coercion(value, &val, *line)?;
                    self.prove_string_interpolation(value, &val, scope, Some(ret), *line)?;
                    self.region_store_guard(name, &val, scope, *line)?;
                    return Ok(());
                }
                // A builtin container is keyed by `Int64`, a user one by what
                // its `atSet` takes.
                let mut key = Type::Int;
                let elem = match self.base(&b.ty) {
                    Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                        (*inner).clone()
                    }
                    Type::Err => return Ok(()),
                    _ => {
                        // The element type is what `atSet` yields, looked up
                        // by the declared type, which the impl head names.
                        match crate::project::lookup_impl(self.impl_blocks, &b.ty, "atSet") {
                            Some((imp, f)) => {
                                if let Some(p) = f.params.get(1) {
                                    key = self.solve_head(imp, &b.ty, &p.ty, *line);
                                }
                                self.solve_head(imp, &b.ty, &f.ret, *line)
                            }
                            None => return Ok(()),
                        }
                    }
                };
                let i = self.expr(index, scope, Some(&key), Some(ret))?;
                if !self.coercible(&i, &key) && !matches!(self.base(&i), Type::Err) {
                    return Ok(());
                }
                let vty = self.expr(value, scope, Some(&elem), Some(ret))?;
                if !self.coercible(&vty, &elem) {
                    return Ok(());
                }
                self.prove_coercion(value, &elem, *line)?;
                self.prove_string_interpolation(value, &elem, scope, Some(ret), *line)?;
                self.region_store_guard(name, &elem, scope, *line)?;
                // Record the expansion the store lowers through: `atSet`
                // inlined, with the move-out and move-back around it.
                if recording() {
                    if let Ok(Some(blk)) =
                        crate::project::store_index(self.impl_blocks, name, index, value, &b.ty)
                    {
                        self.record_desugar(scope, |c, sc| {
                            c.block(blk, ret, sc);
                        });
                    }
                }
                Ok(())
            }
            Stmt::Return { value, line } => {
                let vty = match value {
                    Some(e) => self.expr(e, scope, Some(ret), Some(ret))?,
                    None => Type::Unit,
                };
                if self.coercible(&vty, ret) {
                    if let Some(e) = value {
                        if let Err(msg) = self.prove_coercion(e, ret, *line) {
                            self.errors.borrow_mut().push(msg);
                            return Ok(());
                        }
                        if let Err(msg) =
                            self.prove_string_interpolation(e, ret, scope, Some(ret), *line)
                        {
                            self.errors.borrow_mut().push(msg);
                            return Ok(());
                        }
                    }
                }
                // The typed judgment refuses a value the result does not take.
                Ok(())
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond, scope, None, Some(ret))?;
                self.block(then_block, ret, scope);
                if let Some(eb) = else_block {
                    self.block(eb, ret, scope);
                }
                Ok(())
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                line,
            } => {
                // An optional projection's one legal position,
                // typed before `place_result` can refuse it.
                let raw = match self.optional_scrutinee(scrutinee, pattern, scope, ret, *line)? {
                    Some(t) => t,
                    None => self.expr(scrutinee, scope, None, Some(ret))?,
                };
                let sty = self.resolve_scrutinee(&raw);
                let binders = self.pattern_binders(&sty, pattern, *line)?;
                // The binders are in scope in `then_block` only.
                scope.push(HashMap::new());
                for (name, ty) in &binders {
                    self.bind_seen(Some(ty.clone()), name.line, name.col);
                    scope.last_mut().unwrap().insert(
                        name.name.clone(),
                        Binding {
                            ty: ty.clone(),
                            mutable: false,
                        },
                    );
                }
                self.block(then_block, ret, scope);
                scope.pop();
                if let Some(eb) = else_block {
                    self.block(eb, ret, scope);
                }
                Ok(())
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => Ok(()),
            Stmt::While { cond, body, .. } => {
                self.expr(cond, scope, None, Some(ret))?;
                self.block(body, ret, scope);
                Ok(())
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                col,
                ..
            } => {
                let ity = self.expr(iter, scope, None, Some(ret))?;
                let elem = match self.base(&ity) {
                    Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                        (*inner).clone()
                    }
                    // The loop consumes a stream; movecheck checks that.
                    Type::Stream(inner) => (*inner).clone(),
                    // A String yields its bytes.
                    Type::Str => Type::Int,
                    _ => {
                        // A user container's element is what its `nth` yields,
                        // looked up by the declared type.
                        match crate::types::iterate_impl(self.impl_blocks, &ity) {
                            Some((_, nth)) => {
                                match crate::project::lookup_impl(
                                    self.impl_blocks,
                                    &ity,
                                    crate::types::ITERATE_NTH,
                                ) {
                                    Some((imp, _)) => self.solve_head(imp, &ity, &nth.ret, *line),
                                    None => nth.ret.clone(),
                                }
                            }
                            // The typed judgment refuses the loop.
                            None => Type::Err,
                        }
                    }
                };
                self.bind_seen(Some(elem.clone()), *line, *col);
                scope.push(HashMap::new());
                scope.last_mut().unwrap().insert(
                    var.clone(),
                    Binding {
                        ty: elem,
                        mutable: false,
                    },
                );
                self.block(body, ret, scope);
                scope.pop();
                // A `for` over a user container lowers to `nth` inlined around
                // a copy of the body; record that copy.
                if recording() {
                    if let Some(blk) = crate::types::iterate_impl(self.impl_blocks, &ity).and_then(
                        |(size_fn, nth)| {
                            crate::project::iterate_loop(
                                &size_fn,
                                nth,
                                var,
                                iter,
                                body,
                                iter.line(),
                            )
                            .ok()
                        },
                    ) {
                        self.record_desugar(scope, |c, sc| {
                            c.block(blk, ret, sc);
                        });
                    }
                }
                Ok(())
            }
            Stmt::Drop { .. } => Ok(()),
            Stmt::Expr(e) => self.expr(e, scope, None, Some(ret)).map(|_| ()),
            Stmt::Region { body, .. } => {
                self.region_floor.borrow_mut().push(scope.len());
                self.block(body, ret, scope);
                self.region_floor.borrow_mut().pop();
                Ok(())
            }
        }
    }

    /// Whether a value of this type can carry a heap allocation, for the
    /// `region` escape guard.
    fn contains_heap(&self, ty: &Type) -> bool {
        self.reaches(ty, &|t| match t {
            Type::Str => Reach::Yes,
            Type::Map(..) => Reach::Yes,
            // A function value or `lazy T` may capture heap.
            Type::Fn(..) | Type::Lazy(_) => Reach::Yes,
            // An array or stream buffer is malloc'd, never in the arena, so
            // only its contents can dangle.
            _ => Reach::Parts,
        })
    }

    /// Refuses, inside a `region`, a store of a heap-carrying `stored_ty` into
    /// a binding that outlives the region: it would dangle.
    fn region_store_guard(
        &self,
        name: &str,
        stored_ty: &Type,
        scope: &Scope,
        line: usize,
    ) -> Result<(), Diagnostic> {
        if let Some(&floor) = self.region_floor.borrow().last() {
            let idx = scope.iter().rposition(|f| f.contains_key(name));
            if idx.map_or(false, |i| i < floor) && self.contains_heap(stored_ty) {
                return Err(cerr!(
                    line,
                    "cannot store a heap value into `{name}`, which \
                     outlives the enclosing `region` (it would dangle when the \
                     region frees). Move `{name}` inside the region, or compute a \
                     non-heap result to carry out."
                ));
            }
        }
        Ok(())
    }

    /// Refuses, inside a `region`, a heap-carrying argument to a `consume`
    /// parameter: the arena frees it at the closing brace, so no callee can
    /// own it. Judged by the argument's type. It also refuses a value made
    /// before the region, because the type cannot say which allocation is the
    /// arena's.
    fn region_consume_guard(
        &self,
        callee: &str,
        idx: usize,
        arg_ty: &Type,
        line: usize,
    ) -> Result<(), Diagnostic> {
        if self.region_floor.borrow().is_empty() || !self.contains_heap(arg_ty) {
            return Ok(());
        }
        Err(cerr!(
            line,
            "cannot hand a heap value to argument {} of `{callee}`, which is \
             `consume`, inside a `region`. The region frees the value at its closing brace, \
             so the callee cannot own it. Move the call out of the region, or pass a value \
             that holds no heap.",
            idx + 1
        ))
    }

    // ---- expressions ----------------------------------------------------

    /// Types an expression and, while recording, records the answer against
    /// the node's address. All recording happens here.
    fn expr(
        &self,
        expr: &Expr,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        if !recording() {
            return self.expr_inner(expr, scope, expected, fn_ret);
        }
        let t = self.expr_inner(expr, scope, expected, fn_ret)?;
        let key = expr as *const Expr as usize;
        let pending = PENDING_SUBST.with(|p| p.borrow_mut().take());
        let call = PENDING_CALL.with(|p| p.borrow_mut().take());
        RECORD.with(|r| {
            let mut r = r.borrow_mut();
            if let Some(d) = call {
                r.calls.insert(key, d);
            }
            r.node_types.insert(key, t.clone());
            if matches!(expr, Expr::Match { .. } | Expr::IfExpr { .. }) {
                r.joins.insert(key, t.clone());
            }
            if let Some(call) = pending {
                r.node_substs.insert(key, call);
            }
        });
        Ok(t)
    }

    /// Types an expression. `expected` is the type the context wants; `fn_ret`
    /// is the enclosing function's return type, for `?`.
    fn expr_inner(
        &self,
        expr: &Expr,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        // An expected function type is a stored function value position.
        // A lambda or bare function name there is a
        // defunctionalization source; a binding (`let g = h`) falls through.
        if let Some(exp) = expected {
            if matches!(self.base(exp), Type::Fn(..)) {
                match expr {
                    Expr::Lambda { .. } => return self.stored_fn_lambda(expr, exp, scope, fn_ret),
                    Expr::Var { name, line }
                        if self.lookup(scope, name).is_none()
                            && self.sigs.contains_key(name.as_str()) =>
                    {
                        return self.stored_fn_named(name, exp, *line);
                    }
                    _ => {}
                }
            }
        }
        match expr {
            // Ownership is movecheck's question.
            Expr::Consume { place, .. } => self.expr(place, scope, expected, fn_ret),
            // An integer literal takes the expected sized type, else `Int`. The
            // lexer wraps literals above i64::MAX into the i64 bit pattern, so a
            // negative `n` here is one, valid only as a `UInt64`.
            Expr::Int(n) => match expected.map(|t| self.base(t)) {
                Some(t @ Type::IntN { .. }) => Ok(t),
                _ => {
                    // An expansion's `Expr::Int(-1)` (`project::iterate_loop`)
                    // never went through the lexer and means minus one.
                    if *n < 0 && !desugaring() {
                        Err(cerr!(
                            *self.stmt_line.borrow(),
                            "integer literal {} exceeds Int64's maximum \
                             (9223372036854775807); only `UInt64` can hold it — \
                             annotate the binding (`let x: UInt64 = ...`)",
                            *n as u64
                        ))
                    } else {
                        Ok(Type::Int)
                    }
                }
            },
            // A byte literal takes the expected integer type, else
            // `UInt8`.
            Expr::Byte(_) => match expected.map(|t| self.base(t)) {
                Some(Type::Int) => Ok(Type::Int),
                Some(t @ Type::IntN { .. }) => Ok(t),
                _ => Ok(Type::IntN {
                    bits: 8,
                    signed: false,
                }),
            },
            Expr::Float(_) => Ok(match expected.map(|t| self.base(t)) {
                Some(Type::Float32) => Type::Float32,
                _ => Type::Float,
            }),
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Str(_) => Ok(Type::Str),
            Expr::Var { name, line } => {
                if name == "None" {
                    // The expectation is resolved to find the Option, but the
                    // written type is returned, so an alias keeps its name.
                    return match expected.map(|t| self.base(t)) {
                        Some(b) if crate::types::option_payload(&b).is_some() => {
                            Ok(expected.unwrap().clone())
                        }
                        _ => Err(cerr!(
                            line,
                            "cannot infer the type of `None`; \
                             add an annotation (e.g. `let x: Option<Int64> = None;`)"
                        )),
                    };
                }
                if let Some(info) = self.variants.get(name) {
                    if !info.payload.is_empty() {
                        return self.judged();
                    }
                    let tps = self.enum_type_params(&info.enum_name);
                    if tps.is_empty() {
                        return Ok(Type::Named(info.enum_name.clone()));
                    }
                    // A generic enum's nullary variant takes its type
                    // arguments from context, as `None` does.
                    return match expected {
                        Some(Type::App(en, _)) if en == &info.enum_name => {
                            Ok(expected.unwrap().clone())
                        }
                        _ => Err(cerr!(
                            line,
                            "cannot infer the type of `{name}`; add an annotation"
                        )),
                    };
                }
                if let Some(b) = self.lookup(scope, name) {
                    // A name a failed statement bound: its refusals are that
                    // statement's.
                    if b.ty == Type::Err {
                        self.unknown.set(true);
                    }
                    return Ok(b.ty);
                }
                // A bare function name as a value is a stored function value
                // source: `let g = double` takes its signature.
                if let Some((sptys, sret)) = self.sigs.get(name.as_str()) {
                    self.storable_named_fn(name, *line)?;
                    let sig = Type::Fn(sptys.clone(), Box::new(sret.clone()));
                    self.stored_sources.borrow_mut().push(StoredSource {
                        sig: self.base(&sig),
                        named: Some(name.clone()),
                        lambda: None,
                    });
                    return Ok(sig);
                }
                self.judged()
            }
            Expr::Unary { op, expr, .. } => {
                // A negated integer literal is one literal: `-128` fits `Int8`
                // although `128` does not, and `-1` fits no unsigned type. It
                // takes the expected sized type, and the typed judgment checks
                // its value whole (`checker::misfit`).
                if *op == UnOp::Neg && matches!(**expr, Expr::Int(_)) {
                    return Ok(match expected.map(|t| self.base(t)) {
                        Some(t @ Type::IntN { .. }) => t,
                        _ => Type::Int,
                    });
                }
                // Every unary operator preserves its type, so the expectation
                // flows through: `let a: Float32 = -0.5`.
                let t = self.base(&self.expr(expr, scope, expected, fn_ret)?);
                match op {
                    // `-v` on a float vector flips each lane's sign bit, unlike
                    // `0.0 - v`, which loses the sign of a zero. On
                    // an `I32x4` it wraps, as the scalar does.
                    UnOp::Neg
                        if matches!(
                            t,
                            Type::Int
                                | Type::Float
                                | Type::Float32
                                | Type::IntN { .. }
                                | Type::F32x4
                                | Type::I32x4
                                | Type::F64x2
                        ) =>
                    {
                        Ok(t)
                    }
                    UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                    // `~` complements an integer within its width, or
                    // a mask lane-wise. `!` stays the Bool operator.
                    UnOp::BitNot
                        if matches!(
                            t,
                            Type::Int
                                | Type::IntN { .. }
                                | Type::Mask32x4
                                | Type::Mask64x2
                                | Type::I32x4
                        ) =>
                    {
                        Ok(t)
                    }
                    UnOp::Neg | UnOp::Not | UnOp::BitNot => self.judged(),
                }
            }
            Expr::Binary { op, lhs, rhs, line } => {
                let mut l = self.base(&self.expr(lhs, scope, None, fn_ret)?);
                let mut r = self.base(&self.expr(rhs, scope, None, fn_ret)?);
                if l == Type::Int {
                    if let Some(t) = adapt_int_literal(lhs, &r) {
                        l = t;
                    }
                }
                if r == Type::Int {
                    if let Some(t) = adapt_int_literal(rhs, &l) {
                        r = t;
                    }
                }
                if let Some(t) = adapt_byte_literal(lhs, &r) {
                    l = t;
                }
                if let Some(t) = adapt_byte_literal(rhs, &l) {
                    r = t;
                }
                // A float literal adapts to a `Float32` sibling.
                if l == Type::Float && r == Type::Float32 && matches!(**lhs, Expr::Float(_)) {
                    l = Type::Float32;
                }
                if r == Type::Float && l == Type::Float32 && matches!(**rhs, Expr::Float(_)) {
                    r = Type::Float32;
                }
                self.binop_type(*op, l, r, *line)
            }
            Expr::Call {
                dot: _,
                name,
                args,
                type_args,
                line,
            } => {
                let t = self.call(name, args, type_args, *line, scope, expected, fn_ret)?;
                // `schemaOf<T>()` lowers through the literal it stands for, so
                // the checker types those nodes too (`project::schema`).
                if let ("schemaOf", [Type::Named(tn) | Type::App(tn, _)], true) =
                    (name.as_str(), type_args.as_slice(), recording())
                {
                    if let Some(lit) = self
                        .types
                        .get(tn)
                        .and_then(|d| crate::project::schema(expr, d))
                    {
                        self.record_desugar(scope, |c, sc| {
                            let _ = c.expr(lit, sc, Some(&t), fn_ret);
                        });
                    }
                }
                Ok(t)
            }
            Expr::Match {
                scrutinee,
                arms,
                stmt_pos,
                line,
            } => self.check_match(scrutinee, arms, *stmt_pos, *line, scope, expected, fn_ret),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
            } => self.check_if_expr(
                cond,
                then_branch,
                else_branch.as_deref(),
                *line,
                scope,
                expected,
                fn_ret,
            ),
            Expr::Try { expr, line } => self.check_try(expr, *line, scope, fn_ret),
            Expr::StructLit { name, fields, line } => {
                self.check_struct_lit(expr, name, fields, *line, scope, expected, fn_ret)
            }
            Expr::Field { expr, field, .. } => {
                let ety = self.expr(expr, scope, None, fn_ret)?;
                match self.base(&ety) {
                    Type::Err => Ok(Type::Err),
                    // Only on an array, so a record's `length` field still reads.
                    Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..)
                        if field == "length" =>
                    {
                        Ok(Type::Int)
                    }
                    Type::Map(..) if field == "length" => Ok(Type::Int),
                    Type::Str if field == "byteLength" => Ok(Type::Int),
                    // Reading a `lazy T` field forces it and yields `T`.
                    Type::Record(rfields) => rfields
                        .iter()
                        .find(|f| &f.name == field)
                        .map(|f| crate::types::forced(&f.ty))
                        .map_or_else(|| self.judged(), Ok),
                    _ => self.judged(),
                }
            }
            Expr::TryConstruct { name, args, .. } => {
                let base = match self.types.get(name) {
                    Some(d) if matches!(d.base, Type::Int | Type::Bool | Type::Str) => {
                        d.base.clone()
                    }
                    _ => return self.judged(),
                };
                if args.len() != 1 {
                    return self.judged();
                }
                let aty = self.expr(&args[0], scope, Some(&base), fn_ret)?;
                if !self.assignable(&aty, &base) {
                    return self.judged();
                }
                Ok(Type::option(Type::Named(name.clone())))
            }
            Expr::ArrayLit { elems, line } => {
                // Match the resolved expectation: it may be an alias.
                let written = expected;
                let expected = expected.map(|t| self.base(t));
                let expected = expected.as_ref();
                // An empty `[]` takes its element type from the expectation.
                if elems.is_empty() {
                    return match expected {
                        Some(Type::Array(t)) => Ok(Type::Array(t.clone())),
                        Some(Type::SmallArray(t, n)) => Ok(Type::SmallArray(t.clone(), *n)),
                        Some(_) => Err(cerr!(
                            line,
                            "`[]` is an array literal, but {} is not an \
                             array type",
                            written.unwrap()
                        )),
                        None => Err(cerr!(
                            line,
                            "cannot infer the element type of `[]`; annotate it, \
                             e.g. `let a: Array<Int64> = [];`"
                        )),
                    };
                }
                // Against `Array<T>` the literal is that growable array;
                // otherwise it is a fixed-size `Array<T, N>`.
                let (elem_expected, growable) = match expected {
                    Some(Type::ArrayN(t, _)) => (Some((**t).clone()), false),
                    Some(Type::Array(t)) => (Some((**t).clone()), true),
                    Some(Type::SmallArray(t, _)) => (Some((**t).clone()), false),
                    _ => (None, false),
                };
                let first = self.expr(&elems[0], scope, elem_expected.as_ref(), fn_ret)?;
                // An open parameter settles nothing; the first element does.
                let elem_ty = match elem_expected {
                    Some(t) if self.is_open_param(&t) => first.clone(),
                    other => other.unwrap_or(first.clone()),
                };
                // Element 0 is not typed twice: that doubles what it records.
                for (i, e) in elems.iter().enumerate() {
                    let t = if i == 0 {
                        first.clone()
                    } else {
                        self.expr(e, scope, Some(&elem_ty), fn_ret)?
                    };
                    if !self.coercible(&t, &elem_ty) {
                        return Err(cerr!(
                            line,
                            "array elements must share a type: expected {elem_ty}, found {t}"
                        ));
                    }
                    self.prove_coercion(e, &elem_ty, *line)?;
                    self.prove_string_interpolation(e, &elem_ty, scope, fn_ret, *line)?;
                }
                if let Some(Type::SmallArray(_, n)) = expected {
                    Ok(Type::SmallArray(Box::new(elem_ty), *n))
                } else if growable {
                    Ok(Type::Array(Box::new(elem_ty)))
                } else {
                    Ok(Type::ArrayN(Box::new(elem_ty), elems.len()))
                }
            }
            Expr::MapLit { entries, line } => {
                // Match the resolved expectation: it may be an alias.
                let written = expected;
                let expected = expected.map(|t| self.base(t));
                let (key_ty, val_expected) = match &expected {
                    Some(Type::Map(k, v)) => (Some((**k).clone()), Some((**v).clone())),
                    _ => (None, None),
                };
                if entries.is_empty() {
                    return match (&key_ty, &val_expected) {
                        (Some(k), Some(v)) => {
                            Ok(Type::Map(Box::new(k.clone()), Box::new(v.clone())))
                        }
                        _ if written.is_some() => Err(cerr!(
                            line,
                            "`[:]` is a map literal, but {} is not a map type",
                            written.unwrap()
                        )),
                        _ => Err(cerr!(
                            line,
                            "cannot infer the type of `[:]`; annotate it, \
                             e.g. `let m: Map<String, Int64> = [:];`"
                        )),
                    };
                }
                // The key type is the expected one, else `String`.
                let key_ty = key_ty.unwrap_or(Type::Str);
                let first_val = self.expr(&entries[0].1, scope, val_expected.as_ref(), fn_ret)?;
                // An open parameter settles nothing; the first value does.
                let val_ty = match val_expected {
                    Some(t) if self.is_open_param(&t) => first_val.clone(),
                    other => other.unwrap_or_else(|| first_val.clone()),
                };
                for (i, (k, v)) in entries.iter().enumerate() {
                    let kt = self.expr(k, scope, Some(&key_ty), fn_ret)?;
                    if !self.key_fits(&kt, &key_ty) {
                        return Err(cerr!(
                            line,
                            "the map is keyed by {key_ty}, but this key is {kt}"
                        ));
                    }
                    self.prove_coercion(k, &key_ty, *line)?;
                    self.prove_string_interpolation(k, &key_ty, scope, fn_ret, *line)?;
                    // Entry 0's value is not typed twice.
                    let vt = if i == 0 {
                        first_val.clone()
                    } else {
                        self.expr(v, scope, Some(&val_ty), fn_ret)?
                    };
                    if !self.coercible(&vt, &val_ty) {
                        return Err(cerr!(
                            line,
                            "map values must share a type: expected {val_ty}, \
                             found {vt}"
                        ));
                    }
                    self.prove_coercion(v, &val_ty, *line)?;
                    self.prove_string_interpolation(v, &val_ty, scope, fn_ret, *line)?;
                }
                Ok(Type::Map(Box::new(key_ty), Box::new(val_ty)))
            }
            // A lambda here has no function type from context: a legal one is
            // taken above or by `call`'s `fn`-typed parameter.
            Expr::Lambda { line, .. } => Err(cerr!(
                line,
                "a lambda `|..|` needs a function type from context: \
                 pass it to a `fn`-typed parameter, or give the binding a function \
                 type (e.g. `let f: fn(Int64) -> Int64 = x -> x * 2`)"
            )),
        }
    }

    /// Checks `Name { field: expr, ... }`, solving a generic record's
    /// parameters. An unknown or non-record type, or a field unknown, repeated
    /// or missing, is [`Checker::judged`].
    fn check_struct_lit(
        &self,
        lit: &Expr,
        name: &str,
        fields: &[(String, Expr)],
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        let decl = self.types.get(name);
        // `Token` has no declaration; `types::record_fields` states its
        // fields. A synthesized decoder builds one in generation code
        // (`vyrn_genwasm`'s `Decoders::materialize`).
        if decl.is_none() && !(name == "Token" && *self.in_gen.borrow()) {
            return self.judged();
        }
        let Some(rfields) = crate::types::record_fields(&Type::Named(name.to_string()), self.types)
        else {
            return self.judged();
        };
        let mut provided = std::collections::HashSet::new();
        let mut subst: HashMap<String, Type> = HashMap::new();
        // The expected type seeds the solve: a field may not determine its
        // parameter (`[]` for `Array<T>`, or `Handle<T>`, which stores no `T`).
        if decl.is_some_and(|d| !d.type_params.is_empty()) {
            if let Some(want) = expected {
                let mine = Type::App(
                    name.to_string(),
                    decl.expect("a declaration with type parameters")
                        .type_params
                        .iter()
                        .map(|tp| Type::Param(tp.clone()))
                        .collect(),
                );
                let _ = self.unify(&mine, want, &mut subst, line);
                // Keep only what the expectation settles: `T = T` from an
                // enclosing open literal would look solved.
                subst.retain(|_, arg| !self.mentions_open_param(arg));
            }
        }
        for (fname, _) in fields {
            if !rfields.iter().any(|f| &f.name == fname) || !provided.insert(fname.clone()) {
                return self.judged();
            }
        }
        // Solve in declared field order, the order the backends emit and
        // solve in; the literal's order would answer differently.
        for field in &rfields {
            let Some((_, value)) = fields.iter().find(|(fname, _)| fname == &field.name) else {
                continue; // reported below as a missing field
            };
            let fty = crate::types::substitute(&field.ty, &subst);
            let vty = self.expr(value, scope, Some(&fty), fn_ret)?;
            self.unify(&field.ty, &vty, &mut subst, line)?;
            // A value that echoes the open parameter (`[]` as `Array<T>`)
            // settles nothing.
            subst.retain(|_, arg| !self.mentions_open_param(arg));
            self.prove_coercion(value, &fty, line)?;
            self.prove_string_interpolation(value, &fty, scope, fn_ret, line)?;
        }
        if rfields.iter().any(|f| !provided.contains(&f.name)) {
            return self.judged();
        }
        // A literal of constants that violates the predicate is refused now.
        if let Some(d) = decl.filter(|d| d.predicate.is_some()) {
            if let Some((false, _)) = crate::validate::constant_verdict(lit, d) {
                return Err(cerr!(
                    line,
                    "`{name} {{ .. }}` violates `where {}`",
                    pred_summary(d.predicate.as_ref().unwrap())
                ));
            }
        }
        let Some(decl) = decl.filter(|d| !d.type_params.is_empty()) else {
            return Ok(Type::Named(name.to_string()));
        };
        for tp in &decl.type_params {
            if !subst.contains_key(tp) {
                let shape: Vec<String> = decl
                    .type_params
                    .iter()
                    .map(|p| if p == tp { "..".to_string() } else { p.clone() })
                    .collect();
                return Err(cerr!(
                    line,
                    "cannot infer type parameter `{tp}` of `{name}`; no field \
                     value determines it, so annotate the binding (e.g. `let x: {name}<{}> = \
                     {name} {{ .. }}`)",
                    shape.join(", ")
                ));
            }
        }
        let args = decl
            .type_params
            .iter()
            .map(|tp| subst[tp].clone())
            .collect();
        // A field typed before `T` was solved recorded `Array<T>`; the
        // substitution lets the record's reader replace it.
        if recording() {
            note_subst(name, &subst, &decl.type_params);
        }
        Ok(Type::App(name.to_string(), args))
    }

    /// Checks `expr?` and returns the payload type. `expr` is an `Option`, a
    /// `Result` or a `Fallible` type, and the function returns one
    /// that can take the failure. See [`FALLIBLE`] for why `Option` and
    /// `Result` do not route through the protocol.
    fn check_try(
        &self,
        expr: &Expr,
        line: usize,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        let ety = self.expr(expr, scope, None, fn_ret)?;
        let ret = fn_ret.ok_or_else(|| cerr!(line, "`?` can only be used inside a function"))?;
        // Order is the rule: the built-in sums first, then `Fallible`, because
        // all of them are variant lists.
        if let Some(t) = crate::types::option_payload(&ety) {
            return match crate::types::option_payload(ret) {
                Some(_) => Ok(t.clone()),
                None => Err(cerr!(
                    line,
                    "`?` on an Option requires the function to return Option, \
                     but it returns {ret}"
                )),
            };
        }
        if let Some((t, e)) = crate::types::result_payloads(&ety) {
            return match crate::types::result_payloads(ret) {
                Some((_, re)) if self.assignable(e, re) => Ok(t.clone()),
                Some((_, re)) => Err(cerr!(
                    line,
                    "`?` propagates error {e}, but the function returns \
                     Result<_, {re}>"
                )),
                None => Err(cerr!(
                    line,
                    "`?` on a Result requires the function to return Result, \
                     but it returns {ret}"
                )),
            };
        }
        match &ety {
            other => {
                let key = crate::types::type_key(other)
                    .filter(|k| self.impls.contains(&(FALLIBLE.to_string(), k.clone())));
                let Some(key) = key else {
                    return Err(cerr!(
                        line,
                        "`?` needs an Option, a Result, or a type that implements \
                         `{FALLIBLE}`, found {other}"
                    ));
                };
                // Propagation copies the whole value, so the types must match.
                if !self.assignable(other, ret) {
                    return Err(cerr!(
                        line,
                        "`?` propagates the whole {other}, but the function \
                         returns {ret}"
                    ));
                }
                // `Output` is the type of the `success` call the backends emit,
                // so a generic impl solves through the ordinary call path.
                self.call(
                    &crate::types::impl_method_name(FALLIBLE, &key, "success"),
                    std::slice::from_ref(expr),
                    &[],
                    line,
                    scope,
                    None,
                    fn_ret,
                )
            }
        }
    }

    /// Checks a `match` over a sum type (an `Option`, a `Result` or an enum).
    #[allow(clippy::too_many_arguments)]
    fn check_match(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        stmt_pos: bool,
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        let raw_sty = self.expr(scrutinee, scope, None, fn_ret)?;
        // A transparent sum alias matches as its underlying shape.
        let sty = match &raw_sty {
            Type::Named(n) => match self.types.get(n) {
                Some(d) if d.predicate.is_none() && crate::types::is_sum_alias(&d.base) => {
                    crate::types::resolve(&raw_sty, self.types)
                }
                _ => raw_sty.clone(),
            },
            _ => raw_sty.clone(),
        };
        // `base` answers `Enum` for `Option` and `Result` too.
        let Type::Enum(evs) = self.base(&sty) else {
            return Err(cerr!(
                line,
                "`match` scrutinee must be an Option, Result, or enum, found {sty}"
            ));
        };
        self.check_match_enum(&sty, &evs, arms, line, scope, expected, fn_ret, stmt_pos)
    }

    /// Checks a block arm, legal only in statement position.
    fn arm_block(
        &self,
        b: &Block,
        stmt_pos: bool,
        line: usize,
        inner_scope: &mut Scope,
        fn_ret: Option<&Type>,
    ) -> Result<(), Diagnostic> {
        if !stmt_pos {
            return Err(cerr!(
                line,
                "a `match` used as a value has single-expression arms; a block arm needs statement position"
            ));
        }
        let Some(ret) = fn_ret else {
            return Err(cerr!(line, "a block arm needs a function body around it"));
        };
        self.block(b, ret, inner_scope);
        Ok(())
    }

    /// Checks a `match` over a sum: every arm a valid variant pattern, its
    /// bindings matching the payloads. Exhaustiveness is the core's rule,
    /// stated where it builds the switch.
    #[allow(clippy::too_many_arguments)]
    fn check_match_enum(
        &self,
        sty: &Type,
        evs: &[EnumVariant],
        arms: &[MatchArm],
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
        stmt_pos: bool,
    ) -> Result<Type, Diagnostic> {
        let mut result: Option<Type> = expected.cloned();
        for arm in arms {
            // The refutable-`let` desugar's last arm: any remaining
            // variant, no bindings. No source can write it.
            if matches!(arm.pattern, Pattern::Other) {
                let mut inner = scope.clone();
                match &arm.body {
                    ArmBody::Expr(e) => {
                        let bty = self.expr(e, &inner, result.as_ref(), fn_ret)?;
                        self.unify_arm(&mut result, bty, line)?;
                    }
                    ArmBody::Block(b) => {
                        self.arm_block(b, stmt_pos, line, &mut inner, fn_ret)?;
                    }
                }
                continue;
            }
            let (vname, bind) = match &arm.pattern {
                Pattern::Variant(n, b) => (n.clone(), b.clone()),
                // The `??` desugar's patterns name a tag of an
                // `Option` or `Result`: variant 1 succeeds, variant 0 fails.
                // Another enum has no success side as a pattern.
                Pattern::Success(b) | Pattern::Failure(b)
                    if crate::types::option_payload(&Type::Enum(evs.to_vec())).is_some()
                        || crate::types::result_payloads(&Type::Enum(evs.to_vec())).is_some() =>
                {
                    let at = usize::from(matches!(arm.pattern, Pattern::Success(_)));
                    let v = &evs[at];
                    (
                        v.name.clone(),
                        if v.payload.is_empty() {
                            Vec::new()
                        } else {
                            vec![b.clone()]
                        },
                    )
                }
                Pattern::Success(_) | Pattern::Failure(_) => {
                    return Err(cerr!(
                        line,
                        "`??` works on an Option or a Result, not on {sty} — \
                         `match` names the variant to fall back on"
                    ))
                }
                Pattern::Other => unreachable!("the default arm is checked above the match"),
            };
            let ev = evs
                .iter()
                .find(|v| v.name == vname)
                .ok_or_else(|| cerr!(line, "`{vname}` is not a variant of {sty}"))?;
            if ev.payload.len() != bind.len() {
                return Err(cerr!(
                    line,
                    "variant `{vname}` has {} payload(s), but the pattern binds {}",
                    ev.payload.len(),
                    bind.len()
                ));
            }
            let mut inner = scope.clone();
            if !bind.is_empty() {
                inner.push(HashMap::new());
                for (bname, pty) in bind.iter().zip(&ev.payload) {
                    self.bind_seen(Some(pty.clone()), bname.line, bname.col);
                    inner.last_mut().unwrap().insert(
                        bname.name.clone(),
                        Binding {
                            ty: pty.clone(),
                            mutable: false,
                        },
                    );
                }
            }
            match &arm.body {
                ArmBody::Expr(e) => {
                    let bty = self.expr(e, &inner, result.as_ref(), fn_ret)?;
                    self.unify_arm(&mut result, bty, line)?;
                }
                ArmBody::Block(b) => {
                    self.arm_block(b, stmt_pos, line, &mut inner, fn_ret)?;
                }
            }
        }
        match result {
            Some(t) => Ok(t),
            None if stmt_pos => Ok(Type::Unit),
            // No arm and no expectation: the core refuses the missed variants.
            None => self.judged(),
        }
    }

    /// Checks an `if` expression: `else` is required, and the
    /// branches join as match arms do. An `else if` is a nested `IfExpr`.
    #[allow(clippy::too_many_arguments)]
    fn check_if_expr(
        &self,
        cond: &Expr,
        then_branch: &Expr,
        else_branch: Option<&Expr>,
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        let Some(else_branch) = else_branch else {
            return Err(cerr!(
                line,
                "`if` used as an expression needs an `else` (every branch \
                 must yield a value)"
            ));
        };
        self.expr(cond, scope, Some(&Type::Bool), fn_ret)?;
        let mut result: Option<Type> = expected.cloned();
        let tty = self.expr(then_branch, scope, result.as_ref(), fn_ret)?;
        self.unify_arm(&mut result, tty, line)?;
        let ety = self.expr(else_branch, scope, result.as_ref(), fn_ret)?;
        self.unify_arm(&mut result, ety, line)
    }

    /// Joins an arm's type into the match's result type and returns the join.
    fn unify_arm(
        &self,
        result: &mut Option<Type>,
        bty: Type,
        line: usize,
    ) -> Result<Type, Diagnostic> {
        let joined = match result.take() {
            None => bty,
            Some(rt) if self.assignable(&bty, &rt) => rt,
            // The join is the wider type: keeping a validated `Age` would pass
            // a raw `Int` arm without its check.
            Some(rt) if self.assignable(&rt, &bty) => bty,
            Some(rt) => {
                return Err(cerr!(
                    line,
                    "`match` arms have differing types: {rt} vs {bty}"
                ))
            }
        };
        *result = Some(joined.clone());
        Ok(joined)
    }

    /// The type bound by pattern `tag` when matching a value of type `sty`.
    fn binding_type(&self, sty: &Type, tag: &str) -> Type {
        match tag {
            "Some" => crate::types::option_payload(sty).cloned(),
            "Ok" => crate::types::result_payloads(sty).map(|(t, _)| t.clone()),
            "Err" => crate::types::result_payloads(sty).map(|(_, e)| e.clone()),
            _ => None,
        }
        .unwrap_or(Type::Unit)
    }

    /// Resolves a scrutinee's type through a transparent sum alias.
    fn resolve_scrutinee(&self, raw: &Type) -> Type {
        match raw {
            Type::Named(n) => match self.types.get(n) {
                Some(d) if d.predicate.is_none() && matches!(d.base, Type::Enum(..)) => {
                    crate::types::resolve(raw, self.types)
                }
                _ => raw.clone(),
            },
            _ => raw.clone(),
        }
    }

    /// Checks an `if let` or `while let` pattern against the scrutinee type
    /// and returns its binders with their types.
    fn pattern_binders(
        &self,
        sty: &Type,
        pattern: &Pattern,
        line: usize,
    ) -> Result<Vec<(Binder, Type)>, Diagnostic> {
        if let Type::Enum(evs) = self.base(sty) {
            let (vname, binds) = match pattern {
                Pattern::Variant(n, b) => (n.clone(), b.clone()),
                _ => {
                    return Err(cerr!(
                        line,
                        "pattern does not match scrutinee of type {sty}"
                    ))
                }
            };
            let ev = evs
                .iter()
                .find(|v| v.name == vname)
                .ok_or_else(|| cerr!(line, "`{vname}` is not a variant of {sty}"))?;
            if ev.payload.len() != binds.len() {
                return Err(cerr!(
                    line,
                    "variant `{vname}` has {} payload(s), but the pattern binds {}",
                    ev.payload.len(),
                    binds.len()
                ));
            }
            return Ok(binds.into_iter().zip(ev.payload.iter().cloned()).collect());
        }
        let (tag, bind): (&str, Option<Binder>) = match pattern {
            Pattern::Variant(v, binds) => {
                sum_arm_arity(v, binds.len(), line)?;
                (v.as_str(), binds.first().cloned())
            }
            // Their desugars write a `match`, never an `if let`.
            Pattern::Success(_) | Pattern::Failure(_) | Pattern::Other => {
                unreachable!("the desugared patterns are produced only inside a `match`")
            }
        };
        let want: [&str; 2] = if crate::types::option_payload(sty).is_some() {
            ["Some", "None"]
        } else if crate::types::result_payloads(sty).is_some() {
            ["Ok", "Err"]
        } else {
            return Err(cerr!(
                line,
                "`if let` scrutinee must be an Option, Result, or enum, found {sty}"
            ));
        };
        if !want.contains(&tag) {
            return Err(cerr!(
                line,
                "pattern `{tag}` does not match scrutinee of type {sty}"
            ));
        }
        match bind {
            Some(name) => Ok(vec![(name, self.binding_type(sty, tag))]),
            None => Ok(vec![]),
        }
    }

    fn binop_type(&self, op: BinOp, l: Type, r: Type, line: usize) -> Result<Type, Diagnostic> {
        use BinOp::*;
        if matches!(l, Type::Err) || matches!(r, Type::Err) {
            return Ok(Type::Err);
        }
        // On a type parameter: both operands the same, and the bound present.
        if let Type::Param(t) = &l {
            if &r != &l {
                return Err(cerr!(line, "cannot combine type parameter `{t}` with {r}"));
            }
            return match op {
                Add | Sub | Mul | Div | Rem if self.param_has_bound(t, "Num") => {
                    Ok(Type::Param(t.clone()))
                }
                Lt | LtEq | Gt | GtEq if self.param_has_bound(t, "Ord") => Ok(Type::Bool),
                Eq | NotEq if self.param_has_bound(t, "Eq") => Ok(Type::Bool),
                Add | Sub | Mul | Div | Rem => {
                    Err(cerr!(line, "`{t}` needs a `Num` bound for arithmetic"))
                }
                Lt | LtEq | Gt | GtEq => Err(cerr!(line, "`{t}` needs an `Ord` bound to compare")),
                Eq | NotEq => Err(cerr!(line, "`{t}` needs an `Eq` bound")),
                And | Or => Err(cerr!(line, "`&&`/`||` need Bool operands")),
                Match => Err(cerr!(line, "`=~` needs a String operand, not `{t}`")),
                // No bound grants the bitwise operators.
                BitAnd | BitOr | BitXor | Shl | Shr => Err(cerr!(
                    line,
                    "bitwise operators need a concrete integer type, not `{t}`"
                )),
            };
        }
        let numeric = |t: &Type| {
            matches!(
                t,
                Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
            )
        };
        let code = Type::Named("Code".to_string());
        match op {
            // `Code + Code` concatenates fragments.
            Add if l == code && r == code => Ok(code.clone()),
            Add if l == Type::Str && r == Type::Str => Ok(Type::Str),
            // Lane-wise vector arithmetic. Not in `numeric`, which
            // also gates comparison: a vector comparison yields a mask.
            Add | Sub | Mul | Div if l == Type::F32x4 && r == Type::F32x4 => Ok(l),
            Add | Sub | Mul | Div if l == Type::F64x2 && r == Type::F64x2 => Ok(l),
            // No SIMD integer divide exists in wasm or the hardware targeted.
            Add | Sub | Mul if l == Type::I32x4 && r == Type::I32x4 => Ok(l),
            Div if l == Type::I32x4 && r == Type::I32x4 => Err(cerr!(
                line,
                "`I32x4` has no `/` — no hardware has SIMD integer \
                 divide, so there is no instruction to emit. Read the lanes out \
                 and divide them, or use `F32x4`"
            )),
            // Lane-wise comparison yields a mask, never a `Bool`. `I32x4`
            // shares `Mask32x4`: same lane count and width. Signed, because the
            // lane type is `Int32`.
            Lt | LtEq | Gt | GtEq | Eq | NotEq
                if l == r && matches!(l, Type::F32x4 | Type::I32x4) =>
            {
                Ok(Type::Mask32x4)
            }
            Lt | LtEq | Gt | GtEq | Eq | NotEq if l == r && l == Type::F64x2 => Ok(Type::Mask64x2),
            // Masks and `I32x4` combine with `&`, `|` and `^`, not `&&` and
            // `||`, which short-circuit. No vector `<<` or `>>`: wasm masks the
            // count, LLVM poisons past the width, and the scalar traps.
            BitAnd | BitOr | BitXor
                if l == r && matches!(l, Type::Mask32x4 | Type::Mask64x2 | Type::I32x4) =>
            {
                Ok(l)
            }
            Add | Sub | Mul | Div => {
                if l == r && numeric(&l) {
                    Ok(l)
                } else if op == Add && (l == Type::Str || r == Type::Str) {
                    Err(cerr!(
                        line,
                        "`+` concatenates two Strings, found {l} and {r}"
                    ))
                } else {
                    Err(cerr!(
                        line,
                        "arithmetic needs matching numeric operands, \
                         found {l} and {r}"
                    ))
                }
            }
            Rem => {
                if l == r && matches!(l, Type::Int | Type::IntN { .. }) {
                    Ok(l)
                } else if matches!(l, Type::Float | Type::Float32)
                    || matches!(r, Type::Float | Type::Float32)
                {
                    let f = if matches!(l, Type::Float | Type::Float32) {
                        &l
                    } else {
                        &r
                    };
                    Err(cerr!(line, "no `%` on {f}; integer remainder only"))
                } else {
                    Err(cerr!(
                        line,
                        "`%` needs matching integer operands, found {l} and {r}"
                    ))
                }
            }
            // Strings order byte-wise, not by locale.
            Lt | LtEq | Gt | GtEq => {
                if l == r && (numeric(&l) || l == Type::Str) {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(
                        line,
                        "comparison needs matching numeric or String operands, \
                         found {l} and {r}"
                    ))
                }
            }
            Eq | NotEq => {
                if l == r && (numeric(&l) || matches!(l, Type::Bool | Type::Str)) {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(
                        line,
                        "`==`/`!=` needs matching scalar operands, found {l} and {r}"
                    ))
                }
            }
            And | Or => {
                if l == Type::Bool && r == Type::Bool {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(
                        line,
                        "`&&`/`||` needs Bool operands, found {l} and {r}"
                    ))
                }
            }
            // A shift amount has the shifted value's type.
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let integral = |t: &Type| matches!(t, Type::Int | Type::IntN { .. });
                if l == r && integral(&l) {
                    Ok(l)
                } else if integral(&l) && integral(&r) {
                    Err(cerr!(
                        line,
                        "bitwise operators need matching integer operands, \
                         found {l} and {r}"
                    ))
                } else {
                    Err(cerr!(
                        line,
                        "bitwise operators need integer operands, found {l} and {r}"
                    ))
                }
            }
            Match => {
                if l == Type::Str && r == Type::Str {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(
                        line,
                        "`=~` needs a String and a pattern, found {l} and {r}"
                    ))
                }
            }
        }
    }

    /// Types the vector builtins for `F32x4`, `I32x4` and `F64x2`.
    /// A lane index must be a constant in range, so no backend emits a bounds
    /// check for it. `load` and `store` take a runtime index and check it once
    /// per vector.
    fn vector_call(
        &self,
        name: &str,
        args: &[Expr],
        line: usize,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        // A lane argument; a literal adapts to the lane type.
        let lane_arg = |e: &Expr, lane: &Type, what: &str| -> Result<Option<Type>, Diagnostic> {
            let t = self.base(&self.expr(e, scope, Some(lane), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Some(Type::Err));
            }
            if t != *lane {
                return Err(cerr!(line, "{what} takes {lane} lanes, found {t}"));
            }
            Ok(None)
        };
        // A builtin name's vector type, lane type, spelling and lane count.
        let width = |n: &str| -> (Type, Type, &'static str, i64) {
            if n.starts_with("@i32x4") || n == "I32x4" {
                (Type::I32x4, INT32, "I32x4", 4)
            } else if n.starts_with("@f64x2") || n == "F64x2" {
                (Type::F64x2, Type::Float, "F64x2", 2)
            } else {
                (Type::F32x4, Type::Float32, "F32x4", 4)
            }
        };
        // The lane count of a receiver, for the accessors named per operation.
        let lanes_of = |t: &Type| -> i64 {
            match t {
                Type::F64x2 | Type::Mask64x2 => 2,
                _ => 4,
            }
        };
        match name {
            "F32x4" | "I32x4" | "F64x2" => {
                let (vec, lane, what, lanes) = width(name);
                if args.len() as i64 != lanes {
                    return Err(cerr!(
                        line,
                        "`{what}(..)` takes {lanes} lanes, got {}",
                        args.len()
                    ));
                }
                for a in args {
                    if let Some(e) = lane_arg(a, &lane, &format!("`{what}(..)`"))? {
                        return Ok(e);
                    }
                }
                Ok(vec)
            }
            "@f32x4Splat" | "@i32x4Splat" | "@f64x2Splat" => {
                let (vec, lane, what, _) = width(name);
                if args.len() != 1 {
                    return Err(cerr!(
                        line,
                        "`{what}.splat(..)` takes 1 argument, got {}",
                        args.len()
                    ));
                }
                if let Some(e) = lane_arg(&args[0], &lane, &format!("`{what}.splat(..)`"))? {
                    return Ok(e);
                }
                Ok(vec)
            }
            "@lane" => {
                if args.len() != 2 {
                    return Err(cerr!(
                        line,
                        "`lane` takes a vector and a lane index, got {} argument(s)",
                        args.len()
                    ));
                }
                let v = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                // A mask lane reads as a `Bool`.
                let out = match v {
                    Type::F32x4 => Type::Float32,
                    Type::I32x4 => INT32,
                    Type::F64x2 => Type::Float,
                    Type::Mask32x4 | Type::Mask64x2 => Type::Bool,
                    other => {
                        return Err(cerr!(
                            line,
                            "`lane` must be called on a vector or a mask \
                             (e.g. `v.lane(0)`), found {other}"
                        ))
                    }
                };
                let lanes = lanes_of(&v);
                if crate::types::const_lane(&args[1], lanes).is_none() {
                    return Err(cerr!(line, "a lane index must be a compile-time constant in 0..{} \
                         (that is what makes `lane` total — there is no bounds check to fall back on)",
                        lanes - 1
                    ));
                }
                Ok(out)
            }
            // `v.replaceLane(k, x)`, on a vector only: masks come only from
            // comparison.
            "@replaceLane" => {
                if args.len() != 3 {
                    return Err(cerr!(
                        line,
                        "`replaceLane` takes a lane index and a value \
                         (it is `v.replaceLane(k, x)`), got {} argument(s)",
                        args.len() - 1
                    ));
                }
                let v = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                let lane = match v {
                    Type::F32x4 => Type::Float32,
                    Type::I32x4 => INT32,
                    Type::F64x2 => Type::Float,
                    _ => {
                        return Err(cerr!(
                            line,
                            "`replaceLane` must be called on a vector \
                             (e.g. `v.replaceLane(0, x)`), found {v}"
                        ))
                    }
                };
                // Constant, as for `lane`: the replace-lane opcodes take an
                // immediate.
                let lanes = lanes_of(&v);
                if crate::types::const_lane(&args[1], lanes).is_none() {
                    return Err(cerr!(
                        line,
                        "a lane index must be a compile-time constant in 0..{} \
                         (that is what makes `replaceLane` total — there is no bounds check \
                         to fall back on)",
                        lanes - 1
                    ));
                }
                if let Some(e) = lane_arg(&args[2], &lane, "`replaceLane`")? {
                    return Ok(e);
                }
                Ok(v)
            }
            // A mask reduced to one `Bool`. Masks only: on a float vector it
            // would hide the NaN rule that `v != F32x4.splat(0.0)` states.
            "@anyTrue" | "@allTrue" => {
                let what = if name == "@anyTrue" {
                    "anyTrue"
                } else {
                    "allTrue"
                };
                if args.len() != 1 {
                    return Err(cerr!(
                        line,
                        "`{what}` takes no arguments (it is `m.{what}()`), \
                         got {}",
                        args.len() - 1
                    ));
                }
                let m = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(m, Type::Err) {
                    return Ok(Type::Err);
                }
                if !matches!(m, Type::Mask32x4 | Type::Mask64x2) {
                    return Err(cerr!(
                        line,
                        "`{what}` must be called on a mask \
                         (e.g. `(a < b).{what}()`), found {m}"
                    ));
                }
                Ok(Type::Bool)
            }
            // `F32x4.load(xs, i)` and `.store(xs, i, v)`: consecutive lanes of
            // an array, `i` counted in elements, bounds-checked once.
            "@f32x4Load" | "@f32x4Store" | "@i32x4Load" | "@i32x4Store" | "@f64x2Load"
            | "@f64x2Store" => {
                let (vec, lane, what, _) = width(name);
                let store = name.ends_with("Store");
                let want = if store { 3 } else { 2 };
                if args.len() != want {
                    return Err(cerr!(
                        line,
                        "`{what}.{}(..)` takes {want} arguments, got {}",
                        if store { "store" } else { "load" },
                        args.len()
                    ));
                }
                // `store` writes through a binding; into a temporary it would
                // be lost, as for `xs.pop()`.
                if store && !matches!(&args[0], Expr::Var { .. }) {
                    return Err(cerr!(
                        line,
                        "`{what}.store` needs an array binding as its first \
                         argument, not an expression"
                    ));
                }
                let a = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(a, Type::Err) {
                    return Ok(Type::Err);
                }
                match &a {
                    Type::Array(inner) if self.base(inner) == lane => {}
                    other => {
                        return Err(cerr!(
                            line,
                            "`{what}.{}` needs an Array<{lane}>, found {other}",
                            if store { "store" } else { "load" }
                        ))
                    }
                }
                let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
                if matches!(i, Type::Err) {
                    return Ok(Type::Err);
                }
                if i != Type::Int {
                    return Err(cerr!(
                        line,
                        "a vector load/store index must be an Int64, found {i}"
                    ));
                }
                if !store {
                    return Ok(vec);
                }
                let v = self.base(&self.expr(&args[2], scope, Some(&vec), fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                if v != vec {
                    return Err(cerr!(line, "`{what}.store` stores an {what}, found {v}"));
                }
                Ok(Type::Unit)
            }
            // `min` and `max` follow IEEE-754-2019 `minimum` (wasm's rule): NaN
            // propagates and `-0.0 < +0.0`. Not `minNum` (`llvm.minnum`,
            // `f32::min`). `nearest` rounds ties to even, not away from zero.
            // They sit on the type name so `ceil` stays free for `std/math`.
            //
            // By measurement: `I32x4` has none of these, `abs` is
            // one line of `floatBits`, and `F64x2` has no rounding.
            "@f32x4Min" | "@f32x4Max" | "@f32x4Sqrt" | "@f32x4Ceil" | "@f32x4Floor"
            | "@f32x4Trunc" | "@f32x4Nearest" | "@f64x2Min" | "@f64x2Max" | "@f64x2Sqrt" => {
                let (vec, _, ty, _) = width(name);
                let m = &name[6..];
                let want = if m == "Min" || m == "Max" { 2 } else { 1 };
                let what = m.to_lowercase();
                if args.len() != want {
                    return Err(cerr!(
                        line,
                        "`{ty}.{what}(..)` takes {want} arguments, got {}",
                        args.len()
                    ));
                }
                for a in args {
                    let t = self.base(&self.expr(a, scope, Some(&vec), fn_ret)?);
                    if matches!(t, Type::Err) {
                        return Ok(Type::Err);
                    }
                    if t != vec {
                        return Err(cerr!(line, "`{ty}.{what}` takes {ty}, found {t}"));
                    }
                }
                Ok(vec)
            }
            // Undo the parser's capital, so the message names what was written.
            other => {
                let (_, _, ty, _) = width(other);
                let m = other
                    .trim_start_matches("@f32x4")
                    .trim_start_matches("@i32x4")
                    .trim_start_matches("@f64x2");
                let mut it = m.chars();
                let m = match it.next() {
                    Some(c) => c.to_lowercase().collect::<String>() + it.as_str(),
                    None => m.to_string(),
                };
                Err(cerr!(line, "`{ty}` has no `{m}`"))
            }
        }
    }

    /// The `impl Show for T` a value of the written type `t` renders through.
    fn show_dispatch(&self, t: &Type) -> Option<String> {
        crate::types::show_dispatch(self.impl_blocks, t, &self.base(t))
    }

    /// Whether an `impl Show` renders `args[0]`, of written type `t`. A
    /// concrete type's `show` call is typed here. A type parameter only needs
    /// the `Show` bound: the impl is selected per specialization.
    fn renders_by_declaration(
        &self,
        t: &Type,
        args: &[Expr],
        line: usize,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<bool, Diagnostic> {
        if let Type::Param(p) = t {
            return Ok(self.param_has_bound(p, crate::types::SHOW));
        }
        let Some(m) = self.show_dispatch(t) else {
            return Ok(false);
        };
        self.call(&m, args, &[], line, scope, Some(&Type::Str), fn_ret)?;
        Ok(true)
    }

    fn call(
        &self,
        name: &str,
        args: &[Expr],
        written: &[Type],
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        // A call through a binding of function type (parameter, local, field
        // or module state). Checked before the builtins, so a binding shadows
        // a same-named builtin.
        if let Some(binding) = self.lookup(scope, name) {
            if let Type::Fn(ptys, ret) = self.base(&binding.ty) {
                if ptys.len() != args.len() {
                    return Err(cerr!(
                        line,
                        "`{name}` is a function value taking {} argument(s), got {}",
                        ptys.len(),
                        args.len()
                    ));
                }
                // A `Type::Fn` carries no capabilities, so the region rule
                // reads the signature's `consume` slots from `caps_by_sig`,
                // keyed on the canonical `fn(..) -> R`.
                let sig_caps = self
                    .caps_by_sig
                    .get(&format!("{:?}", Type::Fn(ptys.clone(), ret.clone())));
                for (i, (arg, pty)) in args.iter().zip(&ptys).enumerate() {
                    let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                    if !self.coercible(&aty, pty) {
                        return Err(cerr!(
                            line,
                            "`{name}` argument {} expects {pty}, found {aty}",
                            i + 1
                        ));
                    }
                    self.prove_coercion(arg, pty, line)?;
                    if sig_caps.and_then(|cs| cs.get(i)) == Some(&Capability::Consume) {
                        self.region_consume_guard(name, i, &aty, line)?;
                    }
                }
                // A call through a stored value (any binding outside the
                // params frame, index 1) dispatches over the signature's
                // collected sources, so the effect fixpoint needs
                // (caller, signature). A parameter call keeps caller-site
                // attribution.
                let frame = scope.iter().rposition(|f| f.contains_key(name));
                if frame != Some(1) {
                    self.stored_calls
                        .borrow_mut()
                        .push((self.cur_fn.borrow().clone(), self.base(&binding.ty)));
                }
                return Ok((*ret).clone());
            }
        }
        // A removed free-function spelling. Asked here, not at the unknown-name
        // fall-through, because `at` is also a user's `place at`: `at(r, 0)`
        // would otherwise type as a projection.
        if let Some(g @ Gone::Removed(_)) = moved_to_std(name) {
            return Err(cerr!(line, "{}", g.hint(name)));
        }
        if (name == "assert" || name == "assertEq") && !*self.in_test.borrow() && !test_host() {
            return Err(cerr!(
                line,
                "`{name}` is only available inside a `test` block — in ordinary \
                 code, use a validated type or return a `Result` to signal failure"
            ));
        }
        if name == "assertEq" {
            if args.len() != 2 {
                return Err(cerr!(
                    line,
                    "`assertEq` takes 2 arguments, got {}",
                    args.len()
                ));
            }
            let a = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            let b = self.base(&self.expr(&args[1], scope, Some(&a), fn_ret)?);
            if matches!(a, Type::Err) || matches!(b, Type::Err) {
                return Ok(Type::Unit);
            }
            let equatable = |t: &Type| {
                matches!(
                    t,
                    Type::Int
                        | Type::Float
                        | Type::Float32
                        | Type::IntN { .. }
                        | Type::Bool
                        | Type::Str
                )
            };
            if a != b || !equatable(&a) {
                return Err(cerr!(
                    line,
                    "`assertEq` needs two equal, equatable values, found {a} and {b}"
                ));
            }
            return Ok(Type::Unit);
        }

        // `blackBox<T>(v: T) -> T`: identity the optimizer cannot see through,
        // so the work producing `v` survives and does not fold.
        if name == "blackBox" {
            if !*self.in_test.borrow() && !*self.in_bench.borrow() && !test_host() {
                return Err(cerr!(
                    line,
                    "`blackBox` is only available inside a `bench` or `test` block — \
                     it exists to defeat the optimizer while measuring, not for ordinary code"
                ));
            }
            if args.len() != 1 {
                return Err(cerr!(
                    line,
                    "`blackBox` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.expr(&args[0], scope, expected, fn_ret)?;
            return Ok(t);
        }

        // `panic(msg) -> Never`. The stamped form appends the site: a literal
        // the loader wrote, which no user can spell because `@panicAt` does
        // not lex.
        if crate::ast::is_panic(name) {
            let want = if name == "panic" { 1 } else { 2 };
            if args.len() != want {
                return Err(cerr!(
                    line,
                    "`panic` takes 1 String argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(cerr!(line, "`panic` needs a String, found {t}"));
            }
            // Cannot fail, but typed anyway: a backend reads every node's
            // recorded type, and an untyped node has none.
            if want == 2 {
                let _ = self.expr(&args[1], scope, Some(&Type::Str), fn_ret);
            }
            return Ok(Type::Never);
        }

        // Every builtin whose whole contract is a row in `prelude::rows` is
        // typed at the fall-through (`prelude::checkable`). The arms here carry
        // what a row cannot: a gate on where the call stands, a type name as an
        // argument, a result taken from context, a union parameter, or a
        // refusal about the element type.

        // Generation-only: no backend lowers it, so the one refusal lives here.
        if name == "moduleInterface" && !*self.in_gen.borrow() {
            return Err(cerr!(
                line,
                "`moduleInterface` is only available during generation"
            ));
        }
        // Code quotes, generation-only. `@codeText`/`@codeSplice`
        // are the desugar of a `vyrn"..."` literal. The surface names are
        // common words and not reserved: a function or binding of the same
        // name in THIS module shadows them (not one in another module).
        let is_surface_builtin = crate::ast::is_surface_builtin(name)
            && !self.shadows_here(name)
            && self.lookup(scope, name).is_none();
        if matches!(name, "@codeText" | "@codeSplice") || is_surface_builtin {
            if !*self.in_gen.borrow() {
                let surface = match name {
                    "render" => "`render` is",
                    "rawAt" => "`rawAt` is",
                    "raw" => "`raw` is",
                    "lex" => "`lex` is",
                    _ => "`vyrn\"…\"` code quotes are",
                };
                return Err(cerr!(line, "{surface} only available during generation"));
            }
            let code = || Type::Named("Code".to_string());
            match name {
                "@codeText" => {
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(code());
                }
                // A String spliced into a hole stays data: the hole's context
                // escapes or validates it at generation time.
                "@codeSplice" => {
                    let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                    self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?;
                    let ok = matches!(
                        t,
                        Type::Str
                            | Type::Int
                            | Type::IntN { .. }
                            | Type::Float
                            | Type::Float32
                            | Type::Bool
                            | Type::Err
                    ) || t == code();
                    if !ok {
                        return Err(cerr!(
                            line,
                            "cannot splice {t} into a code quote \
                             (expected String, number, Bool, or Code)"
                        ));
                    }
                    return Ok(code());
                }
                "render" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, "`render` takes 1 argument, got {}", args.len()));
                    }
                    let t = self.base(&self.expr(&args[0], scope, Some(&code()), fn_ret)?);
                    if !matches!(t, Type::Err) && t != code() {
                        return Err(cerr!(line, "`render` needs a Code value, found {t}"));
                    }
                    return Ok(Type::Str);
                }
                // The origin lets `render` map diagnostics inside the text back.
                "rawAt" => {
                    if args.len() != 4 {
                        return Err(cerr!(
                            line,
                            "`rawAt` takes 4 arguments (text, path, line, col), got {}",
                            args.len()
                        ));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[2], scope, Some(&Type::Int), fn_ret)?;
                    self.expr(&args[3], scope, Some(&Type::Int), fn_ret)?;
                    return Ok(code());
                }
                "raw" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, "`raw` takes 1 argument, got {}", args.len()));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(code());
                }
                "lex" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, "`lex` takes 1 argument, got {}", args.len()));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(Type::Array(Box::new(Type::Named("Token".to_string()))));
                }
                _ => unreachable!(),
            }
        }

        // `bytes(s)` is the whole string's UTF-8; `bytes(s, start, end)` a
        // half-open byte range, 19x faster than a byte loop. No
        // character-boundary check: `slice` checks before it calls. Out of
        // range traps with the wording of `s[i]`.
        if name == "bytes" {
            if args.len() != 1 && args.len() != 3 {
                return Err(cerr!(
                    line,
                    "`bytes` takes 1 argument, or 3 with a byte range, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(cerr!(line, "`bytes` needs a String, found {t}"));
            }
            for a in args.iter().skip(1) {
                let n = self.base(&self.expr(a, scope, Some(&Type::Int), fn_ret)?);
                if !matches!(n, Type::Err) && n != Type::Int {
                    return Err(cerr!(line, "`bytes` needs Int64 offsets, found {n}"));
                }
            }
            return Ok(Type::Array(Box::new(Type::IntN {
                bits: 8,
                signed: false,
            })));
        }

        if name == "@push" {
            if args.len() != 2 {
                return Err(cerr!(line, "`push` takes 2 arguments, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            // The result keeps the receiver's kind, so `xs = xs.push(v)` keeps
            // a `SmallArray<T, N>` binding.
            let (elem, rebuild): (Type, Box<dyn Fn(Type) -> Type>) = match self.base(&at) {
                Type::Array(inner) => ((*inner).clone(), Box::new(|e| Type::Array(Box::new(e)))),
                Type::SmallArray(inner, n) => (
                    (*inner).clone(),
                    Box::new(move |e| Type::SmallArray(Box::new(e), n)),
                ),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(cerr!(
                        line,
                        "`push` needs an Array as its first argument, found {other}"
                    ))
                }
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.coercible(&v, &elem) {
                return Err(cerr!(
                    line,
                    "`push` value is {v} but the array holds {elem}"
                ));
            }
            self.prove_coercion(&args[1], &elem, line)?;
            // Inside a `region`, a heap element pushed into an outer buffer
            // dies before the buffer. The Assign guard catches the rebind form;
            // this catches the statement form.
            if let Expr::Var { name: aname, .. } = &args[0] {
                self.region_store_guard(aname, &elem, scope, line)?;
            }
            return Ok(rebuild(elem));
        }
        // `a[i]` parses to `@at`, which dispatches to the receiver's `place at`.
        // `@slot` is the element-place primitive the seeded row bottoms out in.
        // The two type alike; only `@at` dispatches.
        if name == crate::project::AT || name == crate::project::ELEM {
            if args.len() != 2 {
                return Err(cerr!(line, "`at` takes 2 arguments, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            // A user container: the projection's declared return type, with
            // the impl head's type variables solved from the receiver.
            if name == crate::project::AT {
                if let Some(t) = self.place_result(&at, "at", args, scope, fn_ret, line)? {
                    self.refuse_chained_projection(&args[0], scope, line)?;
                    // Record the nodes the site lowers through: the
                    // projection's body inlined here ([`record_desugar`]).
                    if recording() {
                        if let Ok(Some(p)) = crate::project::site(
                            self.impl_blocks,
                            Some(&at),
                            "at",
                            &args[0],
                            &args[1..],
                            line,
                        ) {
                            let ret = fn_ret.cloned().unwrap_or(Type::Unit);
                            self.record_desugar(scope, |c, sc| {
                                sc.push(HashMap::new());
                                for s in &p.prologue {
                                    if c.stmt(s, &ret, sc).is_err() {
                                        return;
                                    }
                                }
                                let _ = c.expr(&p.place, sc, None, fn_ret);
                            });
                        }
                    }
                    return Ok(t);
                }
            }
            // `m[k]` on a Map: a missing key is `None`, never a trap.
            if let Type::Map(key, val) = self.base(&at) {
                let k = self.base(&self.expr(&args[1], scope, Some(&key), fn_ret)?);
                if matches!(k, Type::Err) {
                    return Ok(Type::Err);
                }
                if !self.key_fits(&k, &key) {
                    return Err(cerr!(
                        line,
                        "the map is keyed by {key}, but the key here is {k}"
                    ));
                }
                self.prove_coercion(&args[1], &key, line)?;
                return Ok(Type::option(*val));
            }
            let elem = match self.base(&at) {
                Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                    (*inner).clone()
                }
                // `s[i]` is a byte, as in `bytes(s)`.
                Type::Str => Type::IntN {
                    bits: 8,
                    signed: false,
                },
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(cerr!(
                        line,
                        "indexing needs an Array or String, found {other}"
                    ))
                }
            };
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if matches!(i, Type::Err) {
                return Ok(Type::Err);
            }
            if i != Type::Int {
                return Err(cerr!(line, "`at` index must be an Int64, found {i}"));
            }
            return Ok(elem);
        }
        // `boxStream(s)` moves a stream into a heap box and answers its address,
        // `unboxStream(a)` takes it back out, `pullAt(a)` pulls one element;
        // `std/stream` builds its lazy combinators on them. The address is an
        // `Int64` (no pointers), and `unboxStream` of anything else traps.
        // `movecheck` reads box as a disposal and unbox as an acquisition, so
        // a chain that fails to close its source does not compile.
        if name == "unboxStream" || name == "pullAt" {
            if args.len() != 1 {
                return Err(cerr!(line, "`{name}` takes 1 argument, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, Some(&Type::Int), fn_ret)?;
            let at = self.base(&at);
            if !matches!(at, Type::Err) && at != Type::Int {
                return Err(cerr!(
                    line,
                    "`{name}` needs a boxed stream's address, found {at}"
                ));
            }
            // The address says nothing of its element type, so the context must.
            let want = if name == "unboxStream" {
                "Stream<T>"
            } else {
                "Option<T>"
            };
            let Some(exp) = expected else {
                return Err(cerr!(
                    line,
                    "`{name}` needs the element type from context — \
                      write `let x: {want} = {name}(a)`"
                ));
            };
            let ok = match name {
                "unboxStream" => matches!(self.base(exp), Type::Stream(_)),
                _ => crate::types::option_payload(&self.base(exp)).is_some(),
            };
            if !ok {
                return Err(cerr!(line, "`{name}` answers a `{want}`, not {exp}"));
            }
            return Ok(exp.clone());
        }
        if name == "@pop" {
            if args.len() != 1 {
                return Err(cerr!(line, "`pop` takes no arguments"));
            }
            let elem = self.mut_array_receiver(&args[0], scope, line, "pop")?;
            return Ok(match elem {
                Type::Err => Type::Err,
                t => Type::option(t),
            });
        }
        // O(1) unordered remove: the last element moves into slot `i`.
        if name == "@swapRemove" {
            if args.len() != 2 {
                return Err(cerr!(
                    line,
                    "`swapRemove` takes 1 argument (an index), got {}",
                    args.len() - 1
                ));
            }
            let elem = self.mut_array_receiver(&args[0], scope, line, "swapRemove")?;
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if !matches!(i, Type::Int | Type::Err) {
                return Err(cerr!(
                    line,
                    "`swapRemove` index must be an Int64, found {i}"
                ));
            }
            return Ok(elem);
        }
        // The one conversion from `SmallArray<T, N>` to `Array<T>`; an `Array`
        // receiver is accepted too, as a copy.
        if name == "@toArray" {
            if args.len() != 1 {
                return Err(cerr!(line, "`toArray` takes no arguments"));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&at) {
                Type::SmallArray(inner, _) | Type::Array(inner) => (*inner).clone(),
                Type::Err => return Ok(Type::Err),
                other => return Err(cerr!(line, "`toArray` needs a SmallArray, found {other}")),
            };
            return Ok(Type::Array(Box::new(elem)));
        }
        // `x.copy()`: a deep copy the caller owns. Refused for a declared
        // `impl Owned` type (a field copy would run its release twice; `impl
        // Copy` overrides) and for a `Stream` (two consumers, one cursor). A
        // scalar is accepted, so one generic `x.copy()` serves every instance.
        if name == "@copy" {
            if args.len() != 1 {
                return Err(cerr!(line, "`copy` takes no arguments"));
            }
            let t = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(self.base(&t), Type::Err) {
                return Ok(Type::Err);
            }
            // A declared `impl Copy` answers first and overrides every refusal
            // below.
            if let Some(key) = crate::types::type_key(&t) {
                if self
                    .impls
                    .contains(&(crate::types::COPY.to_string(), key.clone()))
                {
                    let mangled = crate::types::impl_method_name(crate::types::COPY, &key, "copy");
                    return self.call(&mangled, args, &[], line, scope, expected, fn_ret);
                }
            }
            let mut owned_seen = std::collections::HashSet::new();
            if let Some(declared) = self.declared_owned_in(&t, &mut owned_seen) {
                return Err(cerr!(
                    line,
                    "`copy` cannot copy `{declared}`: it declares `impl Owned for \
                     {declared}`, so only `{declared}` knows what duplicating it means. Say what \
                     duplicating it means with `impl Copy for {declared}`, or copy the parts you \
                     need"
                ));
            }
            if matches!(self.base(&t), Type::Stream(_)) {
                return Err(cerr!(
                    line,
                    "`copy` cannot copy a Stream: a stream is a cursor over a \
                     producer, not a container. Collect it first (`collect`), then copy the array"
                ));
            }
            // A structural copy of a self-referring type never bottoms out (the
            // backends overflowed the stack), so it needs an `impl Copy`.
            if let Some(name) = crate::declared::self_referring(&t, &self.types) {
                return Err(cerr!(
                    line,
                    "`copy` cannot copy `{name}`: it refers to itself, so a \
                     structural copy has no bottom to stop at. Write a recursive function that \
                     copies it one variant at a time, and declare it with `impl Copy for {name}`"
                ));
            }
            return Ok(t);
        }
        if name == "@has" || name == "@remove" {
            let op = &name[1..];
            let mt = self.expr(&args[0], scope, None, fn_ret)?;
            let key_ty = match self.base(&mt) {
                Type::Map(k, _) => (*k).clone(),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(cerr!(
                        line,
                        "`{op}` needs a Map as its receiver, found {other}"
                    ))
                }
            };
            if args.len() != 2 {
                return Err(cerr!(line, "`{op}` takes 1 argument (a key)"));
            }
            if name == "@remove" {
                // A receiver declared without `mut` is refused as for `pop`.
                if let Expr::Var { name: recv, .. } = &args[0] {
                    if self.lookup(scope, recv).is_some_and(|b| !b.mutable) {
                        return self.judged();
                    }
                } else {
                    return Err(cerr!(
                        line,
                        "`remove` needs a plain map variable as its receiver"
                    ));
                }
            }
            let k = self.base(&self.expr(&args[1], scope, Some(&key_ty), fn_ret)?);
            if !matches!(k, Type::Err) && !self.key_fits(&k, &key_ty) {
                return Err(cerr!(
                    line,
                    "the map is keyed by {key_ty}, but the key here is {k}"
                ));
            }
            self.prove_coercion(&args[1], &key_ty, line)?;
            return Ok(Type::Bool);
        }
        if let Some(target) = crate::types::numeric_conv_target(name) {
            if args.len() != 1 {
                return Err(cerr!(
                    line,
                    "`{name}` conversion takes 1 argument, got {}",
                    args.len()
                ));
            }
            let src = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(src, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(
                src,
                Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
            ) {
                return Err(cerr!(line, "`{name}(..)` converts a number, found {src}"));
            }
            return Ok(target);
        }
        // Vector builtins. The methods arrive under internal names
        // the parser assigns, so a user `fn min` or `fn lane` is untouched.
        if matches!(
            name,
            "F32x4" | "I32x4" | "F64x2" | "@lane" | "@replaceLane" | "@anyTrue" | "@allTrue"
        ) || name.starts_with("@f32x4")
            || name.starts_with("@i32x4")
            || name.starts_with("@f64x2")
        {
            return self.vector_call(name, args, line, scope, fn_ret);
        }
        // The reflection builtins are rewritten at the call site, so the
        // emitters need the target type written on the node; an expected type
        // would reach only the checker.
        if written.is_empty() && matches!(name, "schemaOf" | "jsonSchema" | "fromJson") {
            // Name the spelling; the row's arity refusal would not.
            let was = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => tn.clone(),
                _ => "Type".to_string(),
            };
            return Err(cerr!(
                line,
                "`{name}` names its target as a type argument — write `{name}<{was}>({})`",
                match name {
                    "fromJson" => "s",
                    _ => "",
                }
            ));
        }
        // `contractOf(C)`: the argument is a contract name, not a value.
        if name == "contractOf" {
            if !*self.in_gen.borrow() {
                return Err(cerr!(
                    line,
                    "`contractOf` is only available during generation"
                ));
            }
            if args.len() != 1 {
                return Err(cerr!(
                    line,
                    "`contractOf` takes 1 argument (a contract name), got {}",
                    args.len()
                ));
            }
            match &args[0] {
                Expr::Var { name: cn, .. } if self.contracts.contains_key(cn) => {
                    return Ok(Type::Named("ContractInfo".to_string()))
                }
                Expr::Var { name: cn, .. } => {
                    return Err(cerr!(
                        line,
                        "`contractOf` needs a declared contract name; \
                          `{cn}` is not a contract"
                    ))
                }
                _ => return Err(cerr!(line, "`contractOf` needs a contract name")),
            }
        }
        if name == "toJson" {
            if args.len() != 1 {
                return Err(cerr!(
                    line,
                    "`toJson` takes 1 argument (a value), got {}",
                    args.len()
                ));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self.types) {
                return Err(cerr!(
                    line,
                    "`toJson` cannot encode `{off}` (not a codable type)"
                ));
            }
            // Recorded so the encoder exists in the linked program by lowering.
            self.json_types.borrow_mut().push(at);
            return Ok(Type::Str);
        }
        // A tagged template's hole desugars to `value(x)`.
        if name == "value" {
            if args.len() != 1 {
                return Err(cerr!(line, "`value` takes 1 argument, got {}", args.len()));
            }
            let written = self.expr(&args[0], scope, None, fn_ret)?;
            let t = self.base(&written);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(t, Type::Int | Type::Bool | Type::Str) {
                // Any type that declares how it renders boxes as its string;
                // `Value` stays the closed three-variant set.
                if self.renders_by_declaration(&written, args, line, scope, fn_ret)? {
                    return Ok(Type::Named("Value".to_string()));
                }
                return Err(cerr!(
                    line,
                    "`value` boxes an Int64, Bool, or String, found {t}{}",
                    crate::types::show_hint(&written)
                ));
            }
            return Ok(Type::Named("Value".to_string()));
        }
        // Tagged-template desugar: a fixed array as a growable one.
        if name == "@list" {
            if args.len() != 1 {
                return Err(cerr!(line, "`@list` takes 1 argument, got {}", args.len()));
            }
            let a = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&a) {
                Type::ArrayN(inner, _) | Type::Array(inner) => return Ok(Type::Array(inner)),
                Type::Err => return Ok(Type::Err),
                other => return Err(cerr!(line, "`@list` needs an Array, found {other}")),
            }
        }

        if name == "Some" {
            if args.len() != 1 {
                return Err(cerr!(line, "`Some` takes 1 argument, got {}", args.len()));
            }
            // Resolved, as `Ok`/`Err` below do: `type MaybeAge = Option<Age>` is
            // a named Option, and the payload's refinement rides on `Age`.
            let inner_expected = expected
                .map(|t| self.base(t))
                .and_then(|b| crate::types::option_payload(&b).cloned());
            let aty = self.expr(&args[0], scope, inner_expected.as_ref(), fn_ret)?;
            // An unsolved-parameter expectation still guides the payload but is
            // not the answer; the payload's type is.
            let inner_expected = inner_expected.filter(|want| !self.is_open_param(want));
            if let Some(want) = &inner_expected {
                if !self.coercible(&aty, want) {
                    return Err(cerr!(
                        line,
                        "`Some` payload is {aty} but Option<{want}> was expected"
                    ));
                }
                self.prove_coercion(&args[0], want, line)?;
                return Ok(Type::option(want.clone()));
            }
            return Ok(Type::option(aty));
        }

        // `Ok(x)` and `Err(e)` need the other type parameter from context.
        if name == "Ok" || name == "Err" {
            if args.len() != 1 {
                return Err(cerr!(line, "`{name}` takes 1 argument, got {}", args.len()));
            }
            // Resolved, so an alias of `Result<T, E>` informs the payload.
            let expected_res = expected.map(|e| crate::types::resolve(e, self.types));
            let res_pair = expected_res
                .as_ref()
                .and_then(crate::types::result_payloads)
                .map(|(t, e)| (t.clone(), e.clone()));
            let want = res_pair
                .as_ref()
                .map(|(t, e)| if name == "Ok" { t.clone() } else { e.clone() });
            let aty = self.expr(&args[0], scope, want.as_ref(), fn_ret)?;
            let (mut t, mut e) = match res_pair {
                Some(pair) => pair,
                _ => {
                    return Err(cerr!(
                        line,
                        "cannot infer the type of `{name}(..)`; add an annotation \
                         (e.g. `-> Result<Int64, Int64>`)"
                    ))
                }
            };
            // An open half the payload carries takes the payload's type; the
            // other half stays open for the enclosing literal to report.
            let carried = if name == "Ok" { &mut t } else { &mut e };
            if self.is_open_param(carried) {
                *carried = aty.clone();
            }
            let want_ty = if name == "Ok" { &t } else { &e };
            self.prove_coercion(&args[0], want_ty, line)?;
            if !self.coercible(&aty, want_ty) {
                return Err(cerr!(
                    line,
                    "`{name}` payload is {aty} but {want_ty} was expected"
                ));
            }
            return Ok(Type::result(t, e));
        }

        if let Some(info) = self.variants.get(name) {
            let payload = info.payload.clone();
            if payload.is_empty() {
                return Err(cerr!(line, "variant `{name}` takes no arguments"));
            }
            if args.len() != payload.len() {
                return Err(cerr!(
                    line,
                    "`{name}` takes {} argument(s), got {}",
                    payload.len(),
                    args.len()
                ));
            }
            let mut subst: HashMap<String, Type> = HashMap::new();
            for (arg, pty) in args.iter().zip(&payload) {
                let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                self.unify(pty, &aty, &mut subst, line)?;
            }
            let tps = self.enum_type_params(&info.enum_name);
            if tps.is_empty() {
                return Ok(Type::Named(info.enum_name.clone()));
            }
            // The expected type fills what the payload leaves open:
            // `Invalid(issues)` learns `T` from a `Validation<T>` return.
            if let Some(Type::App(en, targs)) = expected {
                if en == &info.enum_name {
                    for (tp, ta) in tps.iter().zip(targs) {
                        subst.entry(tp.clone()).or_insert_with(|| ta.clone());
                    }
                }
            }
            for tp in &tps {
                if !subst.contains_key(tp) {
                    return Err(cerr!(
                        line,
                        "cannot infer type parameter `{tp}` of `{}`",
                        info.enum_name
                    ));
                }
            }
            let targs = tps.iter().map(|tp| subst[tp].clone()).collect();
            return Ok(Type::App(info.enum_name.clone(), targs));
        }

        // Only a validated type constructs; an alias has no constructor.
        if let Some(decl) = self.types.get(name).filter(|d| d.predicate.is_some()) {
            return self.check_construction(decl, args, line, scope, fn_ret);
        }

        // Protocol method `x.m(..)`, desugared to `m(x, ..)`: dispatch on the
        // receiver's type. A bounded type-parameter receiver uses the
        // protocol's signature, and codegen dispatches.
        if let Some(candidates) = self.protocol_methods.get(name).cloned() {
            if args.is_empty() {
                return Err(cerr!(line, "`{name}` needs a `self` receiver"));
            }
            // The raw receiver type, so an enum keeps its name.
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            // The impl table picks between protocols that declare the same
            // name. With no impl, the first candidate reports below.
            let (proto, sig) = {
                let key = crate::types::type_key(&recv);
                let matching: Vec<_> = candidates
                    .iter()
                    .filter(|(p, _)| {
                        key.as_ref()
                            .map_or(false, |k| self.impls.contains(&(p.clone(), k.clone())))
                    })
                    .collect();
                match matching.len() {
                    1 => matching[0].clone(),
                    // A `Type::Param` has no impl-table key, so its bound picks
                    // the protocol, whichever candidate comes first.
                    0 if matches!(&recv, Type::Param(_)) => {
                        let Type::Param(t) = &recv else {
                            unreachable!("guarded by the match arm above")
                        };
                        let bounded: Vec<_> = candidates
                            .iter()
                            .filter(|(p, _)| self.param_has_bound(t, p))
                            .collect();
                        match bounded.len() {
                            1 => bounded[0].clone(),
                            0 => candidates[0].clone(),
                            _ => {
                                return Err(cerr!(
                                    line,
                                    "`{name}` is ambiguous: protocols {} all declare it for this receiver",
                                    bounded
                                        .iter()
                                        .map(|(p, _)| p.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ));
                            }
                        }
                    }
                    0 => candidates[0].clone(),
                    _ => {
                        return Err(cerr!(
                            line,
                            "`{name}` is ambiguous: protocols {} all implement it for this receiver",
                            matching
                                .iter()
                                .map(|(p, _)| p.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            };
            if let Type::Param(t) = &recv {
                if self.param_has_bound(t, &proto) {
                    // No impl is selected, so an associated type has nothing to
                    // resolve against and Vyrn has no `T::Output` spelling.
                    // Refused by name; typed as the bare parameter it would
                    // reach codegen as `void`.
                    let mut assoc = None;
                    for ty in sig.params.iter().chain(std::iter::once(&sig.ret)) {
                        walk_type(ty, &mut |t| {
                            if let Type::Param(a) = t {
                                assoc.get_or_insert_with(|| a.clone());
                            }
                        });
                    }
                    if let Some(a) = assoc {
                        return Err(cerr!(
                            line,
                            "`{name}` mentions `{proto}`'s associated type `{a}`, and \
                             a `<{t}: {proto}>` bound cannot name it — call `.{name}(..)` on a \
                             concrete type, where the impl (and so `{a}`) is known"
                        ));
                    }
                    // The protocol member is the declaration (conformance made
                    // the impls agree with it), capabilities included.
                    let mut params = vec![recv.clone()];
                    params.extend(sig.params.iter().cloned());
                    let mut caps = vec![sig.recv];
                    caps.extend(sig.param_caps.iter().copied());
                    return self.check_declared_call(
                        &DeclaredCall {
                            key: name,
                            shown: name,
                            params: &params,
                            ret: &sig.ret,
                            type_params: None,
                            caps: Some(&caps),
                            bounds: None,
                            recv: Some(&recv),
                            written: &[],
                        },
                        args,
                        scope,
                        expected,
                        fn_ret,
                        line,
                    );
                }
            }
            match crate::types::type_key(&recv) {
                Some(key) if self.impls.contains(&(proto.clone(), key.clone())) => {
                    let mangled = crate::types::impl_method_name(&proto, &key, name);
                    // Dispatch ends here; the impl method is read as any
                    // declaration, its receiver's capability at index 0.
                    let (mparams, mret) = self.sigs.get(mangled.as_str()).ok_or_else(|| {
                        cerr!(
                            line,
                            "{recv} does not implement protocol `{proto}` \
                             (needed for `.{name}(..)`)"
                        )
                    })?;
                    return self.check_declared_call(
                        &DeclaredCall {
                            key: mangled.as_str(),
                            shown: name,
                            params: mparams,
                            ret: mret,
                            type_params: self.generics.get(mangled.as_str()),
                            caps: self.caps.get(mangled.as_str()),
                            bounds: self.all_bounds.get(mangled.as_str()),
                            recv: Some(&recv),
                            written: &[],
                        },
                        args,
                        scope,
                        expected,
                        fn_ret,
                        line,
                    );
                }
                _ => {
                    return Err(cerr!(
                        line,
                        "{recv} does not implement protocol `{proto}` \
                         (needed for `.{name}(..)`)"
                    ))
                }
            }
        }

        // `x.f(..)` naming a projection on `x`'s type is an access, typed as
        // `x[i]` is. Asked only when no function has the name, so
        // a function always wins.
        if self.sigs.get(name).is_none()
            && !args.is_empty()
            && self
                .impl_blocks
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            if let Some(t) = self.place_result(&recv, name, args, scope, fn_ret, line)? {
                self.refuse_chained_projection(&args[0], scope, line)?;
                if recording() {
                    if let Ok(Some(p)) = crate::project::site(
                        self.impl_blocks,
                        Some(&recv),
                        name,
                        &args[0],
                        &args[1..],
                        line,
                    ) {
                        let ret = fn_ret.cloned().unwrap_or(Type::Unit);
                        self.record_desugar(scope, |c, sc| {
                            sc.push(HashMap::new());
                            for s in &p.prologue {
                                if c.stmt(s, &ret, sc).is_err() {
                                    return;
                                }
                            }
                            let _ = c.expr(&p.place, sc, None, fn_ret);
                        });
                    }
                }
                return Ok(t);
            }
        }
        // A projection inlines at its access site, and a bounded type
        // variable has no body to inline.
        if self.sigs.get(name).is_none() && !args.is_empty() && self.protocol_places.contains(name)
        {
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(self.base(&recv), Type::Param(_)) {
                return Err(cerr!(
                    line,
                    "`{name}` is a projection, and a projection inlines at its \
                     access site — a `<T: ..>` receiver has no body to inline, \
                     so call `.{name}(..)` on a concrete type"
                ));
            }
        }
        if let Some(t) = gen_host_primitive(name, args.len()) {
            for a in args {
                self.expr(a, scope, None, fn_ret)?;
            }
            return Ok(t);
        }
        // A seeded builtin's row is its declaration. A row that
        // cannot type its call answers `None` from `prelude::checkable`, and an
        // arm above holds that name. A user declaration shadows the row.
        let seeded = match self.sigs.contains_key(name) {
            true => None,
            false => crate::prelude::checkable(name),
        };
        let seeded_sig = seeded.map(|f| {
            (
                f.params.iter().map(|p| p.ty.clone()).collect::<Vec<Type>>(),
                f.ret.clone(),
            )
        });
        let (params, ret) = match (self.sigs.get(name), &seeded_sig) {
            (Some(sig), _) => sig,
            (None, Some(sig)) => sig,
            (None, None) => {
                return Err(match moved_to_std(name) {
                    Some(g) => cerr!(line, "{}", g.hint(name)),
                    None => cerr!(line, "call to unknown function `{name}`"),
                })
            }
        };
        let seeded_generics = seeded
            .map(|f| f.type_params.clone())
            .filter(|tps| !tps.is_empty());
        let seeded_caps = seeded.map(|f| {
            f.params
                .iter()
                .map(|p| p.capability)
                .collect::<Vec<Capability>>()
        });
        // A refusal names what the reader can write: `@str` prints as
        // `toString` (`parser::METHOD_BUILTINS`), and other `@` names lose the
        // `@`, which no source can lex.
        let shown = crate::parser::method_surface(name).trim_start_matches('@');
        self.check_declared_call(
            &DeclaredCall {
                key: name,
                shown,
                params,
                ret,
                type_params: self.generics.get(name).or(seeded_generics.as_ref()),
                caps: self.caps.get(name).or(seeded_caps.as_ref()),
                // A seeded row's bounds are the typed judgment's.
                bounds: self.all_bounds.get(name),
                recv: None,
                written,
            },
            args,
            scope,
            expected,
            fn_ret,
            line,
        )
    }

    /// Types a call against a declaration: a user function, a seeded builtin
    /// row, an impl method or a protocol member. The one path that reads a
    /// declaration at a call site; the dispatcher picks the impl.
    #[allow(clippy::too_many_arguments)]
    fn check_declared_call(
        &self,
        d: &DeclaredCall,
        args: &[Expr],
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
        line: usize,
    ) -> Result<Type, Diagnostic> {
        let DeclaredCall {
            shown,
            params,
            ret,
            recv,
            caps,
            ..
        } = *d;
        // Arity, type-argument count and argument types are refused by the
        // typed judgment (`core::judged`) from the declaration recorded here.
        let refused = || {
            if recording() {
                let decl = CallDecl {
                    shown: shown.to_string(),
                    params: params.to_vec(),
                    type_params: d.type_params.map_or(0, Vec::len),
                    recv: recv.is_some(),
                };
                PENDING_CALL.with(|p| *p.borrow_mut() = Some(decl));
            }
            self.judged()
        };
        if params.len() != args.len() {
            return refused();
        }
        if !d.written.is_empty() && d.type_params.is_none_or(|tps| tps.is_empty()) {
            return refused();
        }
        if let Some(type_params) = d.type_params {
            let mut subst: HashMap<String, Type> = HashMap::new();
            // Written type arguments seed the solve in declaration order; the
            // arguments infer the rest. They are the only source for a
            // parameter only in the result, such as `fromJson<T>`'s.
            if d.written.len() > type_params.len() {
                return refused();
            }
            for (tp, ty) in type_params.iter().zip(d.written) {
                self.ensure_type_exists(ty, line)?;
                subst.insert(tp.clone(), ty.clone());
            }
            let mut atys: Vec<Type> = vec![Type::Err; args.len()];
            // Pass 1: the non-`fn` arguments bind first, so the lambda in
            // `map<T, U>(xs: Array<T>, f: fn(T) -> U)` sees a concrete `T` and
            // its body infers `U`.
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                if matches!(pty, Type::Fn(..)) {
                    continue;
                }
                // A dispatched receiver is solved, not typed again: its type
                // chose the impl, and typing it twice doubles every record.
                if let (0, Some(r)) = (i, recv) {
                    crate::types::solve_param(pty, r, &mut subst);
                    atys[0] = r.clone();
                    continue;
                }
                // The parameter type is the expectation, so a bare `None`,
                // `Ok`, `Err` or `[]` types from it.
                let want = crate::types::substitute(pty, &subst);
                // An unsolved bare parameter is no expectation: a `match`
                // would unify its arms against `T`. Other shapes still name
                // their constructor.
                let want_hint = match want {
                    Type::Param(_) => None,
                    ref w => Some(w),
                };
                let aty = self.expr(arg, scope, want_hint, fn_ret)?;
                self.unify(pty, &aty, &mut subst, line)?;
                // Proved against the solved type, so `f(0)` against a
                // parameter already pinned to a validated type is refused here.
                self.prove_coercion(arg, &want, line)?;
                self.prove_string_interpolation(arg, &want, scope, fn_ret, line)?;
                atys[i] = aty;
            }
            // Pass 2: each `fn`-typed argument, against its parameter type with
            // pass 1's solution substituted; its return type binds `U`.
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                if let Type::Fn(..) = pty {
                    let expected_fn = crate::types::substitute(pty, &subst);
                    if !self.check_fn_arg(
                        shown,
                        i,
                        arg,
                        &expected_fn,
                        scope,
                        fn_ret,
                        &mut subst,
                        line,
                    )? {
                        if recording() {
                            note_subst(d.key, &subst, type_params);
                        }
                        return self.judged();
                    }
                }
            }
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                match caps.and_then(|c| c.get(i)) {
                    Some(&Capability::Modify) => {
                        let concrete_pty = crate::types::substitute(pty, &subst);
                        self.check_modify_arg(
                            shown,
                            i,
                            arg,
                            args,
                            &atys[i],
                            &concrete_pty,
                            scope,
                            line,
                        )?;
                    }
                    Some(&Capability::Consume) => {
                        self.region_consume_guard(shown, i, &atys[i], line)?
                    }
                    _ => {}
                }
            }
            // A parameter no argument mentions (`fn newSlots<T>() -> Slots<T>`)
            // is solved from the expected type, and only then.
            if type_params.iter().any(|tp| !subst.contains_key(tp)) {
                if let Some(want) = expected {
                    let mut from_ctx: HashMap<String, Type> = HashMap::new();
                    if self.unify(ret, want, &mut from_ctx, line).is_ok() {
                        for tp in type_params {
                            if let (false, Some(t)) = (subst.contains_key(tp), from_ctx.get(tp)) {
                                subst.insert(tp.clone(), t.clone());
                            }
                        }
                    }
                }
            }
            for tp in type_params {
                if !subst.contains_key(tp) {
                    // An argument already refused leaves its parameter open;
                    // answer `Err` rather than a second sentence.
                    if atys.iter().any(|t| matches!(t, Type::Err)) {
                        return Ok(Type::Err);
                    }
                    return Err(cerr!(
                        line,
                        "cannot infer type parameter `{tp}` of `{shown}`"
                    ));
                }
            }
            if let Some(bounds) = d.bounds {
                for (tp, bs) in bounds {
                    let Some(concrete) = subst.get(tp) else {
                        continue;
                    };
                    // No second sentence for an argument already refused.
                    if matches!(self.base(concrete), Type::Err) {
                        continue;
                    }
                    for b in bs {
                        if !self.type_satisfies(concrete, b) {
                            // A user `T: Show` bound is the union `print` and
                            // `toString` take, in their words.
                            if b == crate::types::SHOW {
                                return Err(cerr!(
                                    line,
                                    "{}",
                                    crate::types::needs_show(shown, concrete)
                                ));
                            }
                            return Err(cerr!(line, "`{shown}` requires `{tp}: {b}`, but {concrete} does not satisfy `{b}`"
                            ));
                        }
                    }
                }
            }
            // `fromJson<T>` needs `T`'s decoder linked before lowering, and the
            // solve is the one place `T` is known.
            if d.key == "fromJson" {
                if let Some(t) = subst.get("T") {
                    self.json_dec_types.borrow_mut().push(t.clone());
                }
            }
            let rty = crate::types::substitute(ret, &subst);
            // The one place a generic call's type arguments exist; recorded
            // for the backends.
            if recording() {
                note_subst(d.key, &subst, type_params);
            }
            return Ok(rty);
        }

        for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
            let aty = match (i, recv) {
                // The dispatched receiver is already typed; its capability
                // still applies.
                (0, Some(r)) => r.clone(),
                _ => {
                    if let Type::Fn(..) = pty {
                        let mut ignored: HashMap<String, Type> = HashMap::new();
                        if !self.check_fn_arg(
                            shown,
                            i,
                            arg,
                            pty,
                            scope,
                            fn_ret,
                            &mut ignored,
                            line,
                        )? {
                            return self.judged();
                        }
                        continue;
                    }
                    let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                    if !self.coercible(&aty, pty) {
                        return refused();
                    }
                    self.prove_coercion(arg, pty, line)?;
                    self.prove_string_interpolation(arg, pty, scope, fn_ret, line)?;
                    aty
                }
            };
            match caps.and_then(|c| c.get(i)) {
                Some(&Capability::Modify) => {
                    self.check_modify_arg(shown, i, arg, args, &aty, pty, scope, line)?
                }
                Some(&Capability::Consume) => self.region_consume_guard(shown, i, &aty, line)?,
                _ => {}
            }
        }
        Ok(ret.clone())
    }

    /// Solves a `fn` parameter's own parameter type against the function
    /// value's and returns what the caller will pass. The only source for `P`
    /// in `paramQuery(run: fn(P) -> T)`.
    ///
    /// A unification failure is swallowed: the assignability check that
    /// follows names two concrete types, which reads better.
    fn solve_fn_param(
        &self,
        want: &Type,
        actual: &Type,
        subst: &mut HashMap<String, Type>,
        line: usize,
    ) -> Type {
        let _ = self.unify(want, actual, subst, line);
        crate::types::substitute(want, subst)
    }

    /// Checks a `fn`-typed argument against `expected_fn` (its parameter types
    /// already substituted) and unifies the value's return type into `subst`
    /// against `R`. Answers whether the value fits; if not, the core states
    /// the refusal, reading `subst` as it stands.
    #[allow(clippy::too_many_arguments)]
    fn check_fn_arg(
        &self,
        callee: &str,
        i: usize,
        arg: &Expr,
        expected_fn: &Type,
        scope: &Scope,
        fn_ret: Option<&Type>,
        subst: &mut HashMap<String, Type>,
        line: usize,
    ) -> Result<bool, Diagnostic> {
        let (ptys, ret) = match expected_fn {
            Type::Fn(ps, r) => (ps.clone(), (**r).clone()),
            _ => return Ok(true),
        };
        // Contravariant: the value's parameter type must accept what the
        // callee passes. The reverse check would let the callee pass a
        // narrower record than the value reads.
        let params_accept = |vptys: &[Type],
                             vret: &Type,
                             subst: &mut HashMap<String, Type>|
         -> Result<bool, Diagnostic> {
            for (a, b) in vptys.iter().zip(&ptys) {
                let b = &self.solve_fn_param(b, a, subst, line);
                if !self.assignable(b, a) {
                    return Ok(false);
                }
            }
            self.unify(&ret, vret, subst, line).map(|()| true)
        };
        let value_matches = |vptys: &[Type],
                             vret: &Type,
                             subst: &mut HashMap<String, Type>|
         -> Result<bool, Diagnostic> {
            if vptys.len() != ptys.len() {
                return Ok(false);
            }
            params_accept(vptys, vret, subst)
        };
        match arg {
            Expr::Lambda {
                params,
                body,
                line: lline,
                ..
            } => {
                if params.len() != ptys.len() {
                    return Ok(false);
                }
                let mut inner = scope.clone();
                inner.push(HashMap::new());
                for (pn, pty) in params.iter().zip(&ptys) {
                    self.bind_seen(Some(pty.clone()), pn.line, pn.col);
                    inner.last_mut().unwrap().insert(
                        pn.name.clone(),
                        Binding {
                            ty: pty.clone(),
                            mutable: false,
                        },
                    );
                }
                let mut locals: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
                self.check_lambda_body_captures(body, scope, &mut locals, *lline)?;
                let ret_known = !matches!(ret, Type::Param(_));
                let body_ty = match body {
                    LambdaBody::Expr(e) => {
                        let exp = if ret_known { Some(&ret) } else { None };
                        let t = self.expr(e, &inner, exp, fn_ret)?;
                        if ret == Type::Unit {
                            Type::Unit
                        } else {
                            t
                        }
                    }
                    LambdaBody::Block(b) => {
                        if !ret_known {
                            return Err(cerr!(
                                lline,
                                "cannot infer the return type of a \
                                 block-bodied lambda passed to a generic `fn` parameter; \
                                 use an expression body `|..| expr`"
                            ));
                        }
                        self.block(b, &ret, &mut inner);
                        ret.clone()
                    }
                };
                if ret_known && ret != Type::Unit {
                    if !self.coercible(&body_ty, &ret) {
                        return Ok(false);
                    }
                    if let LambdaBody::Expr(e) = body {
                        self.prove_coercion(e, &ret, *lline)?;
                    }
                } else if !ret_known {
                    self.unify(&ret, &body_ty, subst, *lline)?;
                }
                let sig = crate::types::substitute(expected_fn, subst);
                self.record_arg_fn(&sig, None, Some(*lline));
                // The core types the literal's closure from this row (a
                // `consume` position names no target).
                if recording() {
                    let key = arg as *const Expr as usize;
                    RECORD.with(|r| r.borrow_mut().node_types.insert(key, sig));
                }
                Ok(true)
            }
            // A `fn`-typed binding or a named top-level function.
            Expr::Var { name: vn, .. } => {
                // Typed only so the node is recorded: `vyrn_lower` reads
                // `node_types`, and an untyped argument is invisible to the
                // ownership rules (a `consume` fn argument was released twice).
                // The error is discarded: a function name is not a binding.
                let _ = self.expr(arg, scope, None, fn_ret);
                // `base`, so a value under a named fn-type alias passes.
                if let Some(Type::Fn(vptys, vret)) =
                    self.lookup(scope, vn).map(|b| self.base(&b.ty))
                {
                    return value_matches(&vptys, &vret, subst);
                }
                let sig = self.sigs.get(vn).ok_or_else(|| {
                    cerr!(
                        line,
                        "`{callee}` argument {} expects a function; `{vn}` is \
                         neither a lambda nor a known function",
                        i + 1
                    )
                })?;
                // A generic function is no value: its type parameters have
                // nothing to solve against.
                if self.generics.contains_key(vn.as_str())
                    || sig.0.len() != ptys.len()
                    || !params_accept(&sig.0, &sig.1, subst)?
                {
                    return Ok(false);
                }
                self.record_arg_fn(
                    &crate::types::substitute(expected_fn, subst),
                    Some(vn),
                    None,
                );
                Ok(true)
            }
            // Any other `fn`-typed expression: a stored value carries its own
            // tag, and the instance dispatches on it.
            other => {
                let aty = self.expr(other, scope, None, fn_ret)?;
                let Type::Fn(vptys, vret) = self.base(&aty) else {
                    return Ok(false);
                };
                value_matches(&vptys, &vret, subst)
            }
        }
    }

    /// Checks a lambda literal stored as a function value of concrete type
    /// `exp`. Captures are a read-only snapshot. Records the source for
    /// defunctionalization (one variant per source) with its effect summary.
    fn stored_fn_lambda(
        &self,
        expr: &Expr,
        exp: &Type,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        let Expr::Lambda {
            params, body, line, ..
        } = expr
        else {
            unreachable!()
        };
        let sig = self.base(exp);
        let Type::Fn(ptys, ret) = &sig else {
            unreachable!()
        };
        let ret = (**ret).clone();
        if params.len() != ptys.len() {
            return Err(cerr!(
                line,
                "this lambda takes {} parameter(s), but the expected \
                 function type `{exp}` takes {}",
                params.len(),
                ptys.len()
            ));
        }
        let mut inner = scope.clone();
        inner.push(HashMap::new());
        for (pn, pty) in params.iter().zip(ptys) {
            self.bind_seen(Some(pty.clone()), pn.line, pn.col);
            inner.last_mut().unwrap().insert(
                pn.name.clone(),
                Binding {
                    ty: pty.clone(),
                    mutable: false,
                },
            );
        }
        let mut locals: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
        self.check_lambda_body_captures(body, scope, &mut locals, *line)?;
        // Stored calls in the body belong to this lambda's summary, not the
        // enclosing function's: the body runs wherever the value is invoked.
        let calls_before = self.stored_calls.borrow().len();
        match body {
            LambdaBody::Expr(e) => {
                // The core refuses a body the slot does not take.
                let t = self.expr(e, &inner, Some(&ret), fn_ret)?;
                if ret != Type::Unit && self.coercible(&t, &ret) {
                    self.prove_coercion(e, &ret, *line)?;
                }
            }
            LambdaBody::Block(b) => self.block(b, &ret, &mut inner),
        }
        let nested_sigs: Vec<Type> = self.stored_calls.borrow()[calls_before..]
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        // Effect summary for `--workers`: the body's call names and the first
        // module-state binding it touches.
        let mut calls: std::collections::HashSet<String> = Default::default();
        {
            let mut v = Calls(&mut calls);
            let mut locals = HashSet::new();
            match body {
                LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
                LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
            }
        }
        // Names that shadow module state: the lambda's own binders and every
        // frame (module state is `Scope`'s fall-through, not a frame).
        let mut local_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
        if let LambdaBody::Block(b) = body {
            collect_binders_block(b, &mut local_names);
        }
        for frame in scope.iter() {
            local_names.extend(frame.keys().cloned());
        }
        let globals = self.globals.borrow();
        let mut gnames: Vec<&String> = globals.keys().collect();
        gnames.sort();
        let touches_global = gnames
            .into_iter()
            .find(|g| {
                let single: std::collections::HashSet<String> =
                    std::iter::once((*g).clone()).collect();
                match body {
                    LambdaBody::Expr(e) => global_ref_expr(e, &single, &local_names),
                    LambdaBody::Block(b) => global_ref_block(b, &single, &local_names),
                }
            })
            .cloned();
        self.stored_sources.borrow_mut().push(StoredSource {
            sig: sig.clone(),
            named: None,
            lambda: Some(StoredLambda {
                defined_in: self.cur_fn.borrow().clone(),
                line: *line,
                calls,
                touches_global,
                nested_sigs,
            }),
        });
        Ok(exp.clone())
    }

    /// Checks a function name stored as a function value of type `exp` and
    /// records it as a source. The core refuses a signature that does not
    /// match.
    fn stored_fn_named(&self, name: &str, exp: &Type, line: usize) -> Result<Type, Diagnostic> {
        let sig = self.base(exp);
        self.storable_named_fn(name, line)?;
        // An unannotated generic record literal (`G { width: widthOf }`)
        // arrives with open parameters: `fn(T) -> Int64`. A comparison against
        // an unsolved `T` would print a sentence no reader can act on, so ask
        // for an annotation. A rigid parameter is a real type and passes.
        let open = self.open_params(&sig);
        if let Some(first) = open.first() {
            let names: Vec<String> = open.iter().map(|p| format!("`{p}`")).collect();
            return Err(cerr!(
                line,
                "`{name}` cannot be stored as `{exp}`: nothing has solved {} \
                 here, so there is no signature to check `{name}` against. Annotate the \
                 binding with a type that names `{first}`",
                names.join(" or ")
            ));
        }
        self.stored_sources.borrow_mut().push(StoredSource {
            sig: sig.clone(),
            named: Some(name.to_string()),
            lambda: None,
        });
        Ok(exp.clone())
    }

    /// Records the function a `fn`-typed argument hands its callee: a lambda
    /// literal or a function name (any other `fn` expression forwards a value
    /// already collected). `sig` is the parameter type under the call's
    /// solution, so the concrete signature the instance calls through.
    fn record_arg_fn(&self, sig: &Type, named: Option<&str>, lambda_line: Option<usize>) {
        self.arg_sources.borrow_mut().push(StoredSource {
            sig: self.base(sig),
            named: named.map(str::to_string),
            // Only the frame key is filled: the workers analysis reads
            // `sources` alone (see `StoredFnEffects::arg_sources`).
            lambda: lambda_line.map(|line| StoredLambda {
                defined_in: self.cur_fn.borrow().clone(),
                line,
                calls: HashSet::new(),
                touches_global: None,
                nested_sigs: Vec::new(),
            }),
        });
    }

    /// Refuses a generic, `extern` or `gen` function used as a value.
    fn storable_named_fn(&self, name: &str, line: usize) -> Result<(), Diagnostic> {
        if self.generics.contains_key(name) {
            return Err(cerr!(
                line,
                "`{name}` is generic and cannot be used as a function \
                 value in v1"
            ));
        }
        if self.extern_fns.contains(name) {
            return Err(cerr!(
                line,
                "an `extern` function cannot be used as a function \
                 value — the host boundary dispatches by name"
            ));
        }
        if self.gen_fns.contains(name) {
            return Err(cerr!(
                line,
                "a `gen fn` runs at generation time and cannot be \
                 used as a function value"
            ));
        }
        Ok(())
    }

    /// Refuses a lambda body that assigns, `drop`s or `consume`s a captured
    /// binding, or holds a nested lambda literal. A capture is read-only;
    /// names bound inside the lambda (`locals`) are exempt. Reports the first
    /// violation in source order.
    fn check_lambda_body_captures(
        &self,
        body: &LambdaBody,
        outer: &Scope,
        locals: &mut HashSet<String>,
        line: usize,
    ) -> Result<(), Diagnostic> {
        /// A capture is a binding visible outside and not shadowed by `locals`.
        struct Captures<'a, 'b> {
            ck: &'a Checker<'b>,
            outer: &'a Scope,
            err: Option<String>,
        }

        impl Captures<'_, '_> {
            fn is_capture(&self, n: &str, locals: &HashSet<String>) -> bool {
                !locals.contains(n) && self.ck.lookup(self.outer, n).is_some()
            }

            fn fail(&mut self, m: String) {
                if self.err.is_none() {
                    self.err = Some(m);
                }
            }
        }

        impl BodyVisit<'_> for Captures<'_, '_> {
            fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
                match s {
                    Stmt::Assign { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(format!(
                            "a lambda captures by read; it cannot assign to the captured \
                             binding `{name}` (line {line})"
                        ));
                    }
                    Stmt::SetField { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(format!(
                            "a lambda captures by read; it cannot mutate a field of the \
                             captured binding `{name}` (line {line})"
                        ));
                    }
                    Stmt::IndexSet { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(format!(
                            "a lambda captures by read; it cannot store into the captured \
                             binding `{name}` (line {line})"
                        ));
                    }
                    Stmt::Drop { name, line } if self.is_capture(name, locals) => {
                        self.fail(format!(
                            "a lambda cannot `drop` the captured binding `{name}` (line {line})"
                        ));
                    }
                    _ => {}
                }
            }

            fn expr(&mut self, e: &Expr, locals: &HashSet<String>) -> bool {
                if self.err.is_some() {
                    return false;
                }
                match e {
                    Expr::Call {
                        dot: _,
                        name,
                        args,
                        line,
                        type_args: _,
                    } => {
                        // Each argument is checked before it is walked, so the
                        // first violation in source order is reported.
                        let caps = self.ck.caps.get(name);
                        for (k, a) in args.iter().enumerate() {
                            if caps.and_then(|c| c.get(k)) == Some(&Capability::Consume) {
                                if let Expr::Var { name: vn, .. } = a {
                                    if self.is_capture(vn, locals) {
                                        self.fail(format!(
                                            "a lambda cannot consume the captured binding \
                                             `{vn}` (line {line})"
                                        ));
                                        return false;
                                    }
                                }
                            }
                            body_expr(a, locals, self);
                            if self.err.is_some() {
                                return false;
                            }
                        }
                        false
                    }
                    // Nesting would compound monomorphization.
                    Expr::Lambda { line, .. } => {
                        self.fail(format!(
                            "a lambda body may not contain another lambda literal in v1 \
                             (line {line})"
                        ));
                        false
                    }
                    _ => true,
                }
            }
        }

        let mut v = Captures {
            ck: self,
            outer,
            err: None,
        };
        let mut locals = locals.clone();
        match body {
            LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
            LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
        }
        match v.err {
            Some(m) => Err(cerr!(line, "{m}")),
            None => Ok(()),
        }
    }

    /// Checks a `modify` argument: a mutable variable, of exactly the
    /// parameter type, named by no other argument of the call. Width subtyping
    /// is unsound here: a whole reassignment written back through a wider
    /// record drops its extra fields. Exclusivity: two names for
    /// one value break the callee's promise of one.
    fn check_modify_arg(
        &self,
        fname: &str,
        i: usize,
        arg: &Expr,
        args: &[Expr],
        aty: &Type,
        pty: &Type,
        scope: &Scope,
        line: usize,
    ) -> Result<(), Diagnostic> {
        if let Some((root, path)) = crate::ast::place_path(arg) {
            for (j, b) in args.iter().enumerate() {
                if j != i && crate::ast::mentions(b, &root) {
                    let mut d = cerr!(
                        line,
                        "`{path}` is passed to `{fname}` as `modify` and read again in the \
                         same call — a `modify` borrow is exclusive"
                    );
                    d.message.push_str(&format!(
                        "
  fix: `{root}.copy()` for the second argument"
                    ));
                    d.message.push_str(
                        "
  fix: or split the call so the two accesses do not overlap",
                    );
                    return Err(d);
                }
            }
        }
        match arg {
            Expr::Var { name: vn, .. } => {
                if self.lookup(scope, vn).is_some_and(|b| !b.mutable) {
                    return Err(cerr!(
                        line,
                        "`{fname}` argument {} is `modify`, so `{vn}` must be \
                         declared `mut`",
                        i + 1
                    ));
                }
            }
            _ => {
                return Err(cerr!(
                    line,
                    "`{fname}` argument {} is `modify`; pass a mutable \
                     variable, not a temporary",
                    i + 1
                ))
            }
        }
        if !matches!(aty, Type::Err) && !matches!(pty, Type::Err) && aty != pty {
            return Err(cerr!(
                line,
                "`{fname}` argument {} is `modify` and needs exactly \
                 {pty}, found {aty} (width subtyping is read-only: a wider \
                 record could lose fields on write-back)",
                i + 1
            ));
        }
        Ok(())
    }

    /// Matches a parameter type against an argument type, binding type
    /// parameters in `subst`.
    fn unify(
        &self,
        pty: &Type,
        aty: &Type,
        subst: &mut HashMap<String, Type>,
        line: usize,
    ) -> Result<(), Diagnostic> {
        // `Err` and `Never` unify with anything: `f(panic(".."))` places no
        // demand on `T`, and `T = Never` would fit no value.
        if matches!(pty, Type::Err) || matches!(aty, Type::Err) || matches!(aty, Type::Never) {
            return Ok(());
        }
        match pty {
            Type::Param(t) => match subst.get(t) {
                Some(bound) => {
                    if !self.assignable(aty, bound) {
                        Err(cerr!(
                            line,
                            "type parameter `{t}` is both {bound} and {aty}"
                        ))
                    } else {
                        Ok(())
                    }
                }
                None => {
                    subst.insert(t.clone(), aty.clone());
                    Ok(())
                }
            },
            _ if crate::types::option_payload(pty).is_some() => {
                let inner = crate::types::option_payload(pty).expect("an Option payload");
                match crate::types::option_payload(aty) {
                    Some(a) => self.unify(inner, a, subst, line),
                    None => Err(cerr!(line, "expected Option, found {aty}")),
                }
            }
            _ if crate::types::result_payloads(pty).is_some() => {
                let (pt, pe) = crate::types::result_payloads(pty).expect("Result payloads");
                match crate::types::result_payloads(aty) {
                    Some((at, ae)) => {
                        self.unify(pt, at, subst, line)?;
                        self.unify(pe, ae, subst, line)
                    }
                    None => Err(cerr!(line, "expected Result, found {aty}")),
                }
            }
            Type::App(pn, pargs) => match aty {
                Type::App(an, aargs) if pn == an && pargs.len() == aargs.len() => {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.unify(p, a, subst, line)?;
                    }
                    Ok(())
                }
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            // Collections bind `T` from the element type; an alias resolves
            // first.
            Type::Array(inner) => match crate::types::resolve(aty, self.types) {
                Type::Array(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            Type::ArrayN(inner, n) => match aty {
                Type::ArrayN(a, m) if m == n => self.unify(inner, a, subst, line),
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            Type::Stream(inner) => match crate::types::resolve(aty, self.types) {
                Type::Stream(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            // `N` must match: integer arguments do not infer.
            Type::SmallArray(inner, n) => match crate::types::resolve(aty, self.types) {
                Type::SmallArray(a, m) if m == *n => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            // A generic function type binds through the value's signature. Two
            // concrete function types keep the assignability rule below.
            Type::Fn(pps, pr) if type_mentions_param(pty) => {
                match crate::types::resolve(aty, self.types) {
                    Type::Fn(aps, ar) if aps.len() == pps.len() => {
                        for (p, a) in pps.iter().zip(&aps) {
                            self.unify(p, a, subst, line)?;
                        }
                        self.unify(pr, &ar, subst, line)
                    }
                    _ => Err(cerr!(line, "expected {pty}, found {aty}")),
                }
            }
            // `lazy T` takes what `fn() -> T` takes (`types::assignable`), so a
            // generic one binds as that. Any other argument keeps the rule below.
            Type::Lazy(_)
                if type_mentions_param(pty)
                    && matches!(
                        crate::types::resolve(aty, self.types),
                        Type::Fn(ps, _) if ps.is_empty()
                    ) =>
            {
                self.unify(&crate::types::resolve(pty, self.types), aty, subst, line)
            }
            Type::Map(pk, pv) => match crate::types::resolve(aty, self.types) {
                Type::Map(ak, av) => {
                    self.unify(pk, &ak, subst, line)?;
                    self.unify(pv, &av, subst, line)
                }
                _ => Err(cerr!(line, "expected {pty}, found {aty}")),
            },
            _ => {
                if !self.coercible(aty, pty) {
                    Err(cerr!(line, "argument expects {pty}, found {aty}"))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Checks `TypeName(arg)`. A constant `arg` is validated now; any other
    /// is checked at run time. Arity and base type are the typed judgment's
    /// refusals.
    fn check_construction(
        &self,
        decl: &TypeDecl,
        args: &[Expr],
        line: usize,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        if args.len() != 1 {
            return self.judged();
        }
        let aty = self.expr(&args[0], scope, Some(&decl.base), fn_ret)?;
        if !self.assignable(&aty, &decl.base) {
            return self.judged();
        }
        if let Some((false, Some(cv))) = crate::validate::constant_verdict(&args[0], decl) {
            return Err(cerr!(
                line,
                "{} does not satisfy `{}` (predicate `where {}` is false)",
                cv,
                decl.name,
                pred_summary(decl.predicate.as_ref().unwrap()),
            ));
        }
        Ok(Type::Named(decl.name.clone()))
    }

    /// Whether the module being checked declares or imports `name`, which
    /// shadows an unreserved surface builtin in this module only.
    fn shadows_here(&self, name: &str) -> bool {
        self.shadows
            .contains(&(self.here.borrow().clone(), name.to_string()))
    }

    /// The innermost frame's binding of `name`, else module state when the
    /// scope is open.
    fn lookup(&self, scope: &Scope, name: &str) -> Option<Binding> {
        for frame in scope.iter().rev() {
            if let Some(b) = frame.get(name) {
                return Some(b.clone());
            }
        }
        if scope.globals {
            return self.globals.borrow().get(name).cloned();
        }
        None
    }

    /// The element type of the plain array variable a `pop` or `swapRemove`
    /// receiver names; `op` is the spelling a diagnostic quotes.
    ///
    /// An unknown or non-`mut` receiver is the typed judgment's refusal
    /// (`typed::stores`), typed `Err` before its type is read because a
    /// literal bound without `mut` is a fixed-size array. A receiver that is
    /// no growable array is `Builder::shrinks`'s refusal.
    fn mut_array_receiver(
        &self,
        recv: &Expr,
        scope: &Scope,
        line: usize,
        op: &str,
    ) -> Result<Type, Diagnostic> {
        let Expr::Var { name, .. } = recv else {
            return Err(cerr!(
                line,
                "`{op}` needs a plain array variable as its receiver"
            ));
        };
        let Some(b) = self.lookup(scope, name).filter(|b| b.mutable) else {
            return self.judged();
        };
        match self.base(&b.ty) {
            Type::Array(inner) => Ok((*inner).clone()),
            Type::SmallArray(inner, _) => Ok((*inner).clone()),
            Type::Err => Ok(Type::Err),
            _ => self.judged(),
        }
    }
}

/// A one-line rendering of a predicate for diagnostics.
pub(crate) fn pred_summary(expr: &Expr) -> String {
    match expr {
        Expr::Int(n) => n.to_string(),
        Expr::Byte(b) => b.to_string(),
        Expr::Float(x) => x.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("{s:?}"),
        Expr::Var { name, .. } => name.clone(),
        Expr::Unary { op, expr, .. } => {
            let s = pred_summary(expr);
            match op {
                UnOp::Neg => format!("-{s}"),
                UnOp::Not => format!("!{s}"),
                UnOp::BitNot => format!("~{s}"),
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let o = crate::parser::binop_text(*op);
            format!("{} {o} {}", pred_summary(lhs), pred_summary(rhs))
        }
        Expr::Call { name, args, .. } if name == crate::project::AT && args.len() == 2 => {
            format!("{}[{}]", pred_summary(&args[0]), pred_summary(&args[1]))
        }
        Expr::Call { name, .. } => format!("{name}(..)"),
        Expr::Match { .. } => "match { .. }".to_string(),
        Expr::IfExpr { .. } => "if .. { .. } else { .. }".to_string(),
        Expr::Try { expr, .. } => format!("{}?", pred_summary(expr)),
        Expr::StructLit { name, .. } => format!("{name} {{ .. }}"),
        Expr::Field { expr, field, .. } => format!("{}.{field}", pred_summary(expr)),
        Expr::TryConstruct { name, .. } => format!("{name}?(..)"),
        Expr::ArrayLit { .. } => "[..]".to_string(),
        Expr::MapLit { .. } => "[..:..]".to_string(),
        Expr::Consume { place, .. } => format!("consume {}", pred_summary(place)),
        Expr::Lambda { params, .. } => format!(
            "|{}| ..",
            params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Whether `ty` names `Self`, which is no type in Vyrn. The protocol's line
/// reports it, so an impl compared against it is not blamed.
fn type_mentions_self(ty: &Type) -> bool {
    let mut found = false;
    walk_type(ty, &mut |t| {
        if matches!(t, Type::Named(n) if n == "Self") {
            found = true;
        }
    });
    found
}

/// The value an integer literal `n` was written as. The lexer wraps u64-range
/// literals into the i64 bit pattern, so a negative `n` is a literal above
/// `i64::MAX`, and its unsigned reading is what was written.
pub fn literal_value(n: i64) -> i128 {
    if n < 0 {
        i128::from(n as u64)
    } else {
        i128::from(n)
    }
}

/// Returns the refusal of a `kind` literal whose value `v` falls outside a
/// sized integer type, or `None` when it fits. The one statement of the fit
/// rule.
pub fn misfit(kind: &str, v: i128, bits: u8, signed: bool) -> Option<String> {
    (!int_value_fits(v, bits, signed)).then(|| {
        format!(
            "{kind} literal {v} does not fit {} (its range is {})",
            intn_name(bits, signed),
            intn_range(bits, signed),
        )
    })
}

/// Returns the value an integer literal or its negation denotes (`-5` parses
/// as `Neg(Int(5))`), or `None` for any other expression.
pub fn int_literal_value(e: &Expr) -> Option<i128> {
    match e {
        Expr::Int(n) => Some(literal_value(*n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n) => Some(-literal_value(*n)),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a literal value (already negated) fits the sized type.
fn int_value_fits(v: i128, bits: u8, signed: bool) -> bool {
    if signed {
        let shift = 128 - u32::from(bits);
        (i128::MIN >> shift..=i128::MAX >> shift).contains(&v)
    } else {
        (0..(1i128 << bits)).contains(&v)
    }
}

/// The type an integer literal takes from a sized sibling operand, or `None`
/// when the sibling is not sized. The value must still fit ([`misfit`]), or
/// `x < 300` on a `UInt8` would compare against 44.
fn adapt_int_literal(lit: &Expr, sibling: &Type) -> Option<Type> {
    let Type::IntN { .. } = sibling else {
        return None;
    };
    int_literal_value(lit)?;
    Some(sibling.clone())
}

/// The type a byte literal (a `UInt8` by default) takes from an integer
/// sibling it fits, or `None` when the operator's own arm answers.
fn adapt_byte_literal(lit: &Expr, sibling: &Type) -> Option<Type> {
    let Expr::Byte(b) = lit else {
        return None;
    };
    match sibling {
        Type::Int => Some(Type::Int),
        Type::IntN { bits, signed } if int_value_fits(i128::from(*b), *bits, *signed) => {
            Some(sibling.clone())
        }
        _ => None,
    }
}

fn intn_name(bits: u8, signed: bool) -> String {
    format!("{}Int{bits}", if signed { "" } else { "U" })
}

fn intn_range(bits: u8, signed: bool) -> String {
    if signed {
        let shift = 64 - u32::from(bits);
        format!("{}..={}", i64::MIN >> shift, i64::MAX >> shift)
    } else {
        let max: u64 = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        format!("0..={max}")
    }
}

/// Whether a type may cross an `extern` boundary: a scalar by value, a
/// `String` as `(ptr, len)`. `allow_unit` is for the return position.
fn extern_abi_type_ok(ty: &Type, allow_unit: bool) -> bool {
    match ty {
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str => {
            true
        }
        Type::Unit => allow_unit,
        _ => false,
    }
}

/// Refuses a `gen fn` that reaches, through any call chain, an `extern`,
/// module state, or an atom [`crate::effects::gen_refusal`] refuses, naming
/// the effect and the chain. Every `gen fn` is checked, even one called only
/// at run time, because any may be an import target.
fn check_comptime_purity(program: &Program, out: &mut Vec<Diagnostic>) {
    let gen_fns: Vec<&Function> = program.functions.iter().filter(|f| f.is_gen).collect();
    if gen_fns.is_empty() {
        return;
    }
    let fn_map: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    // `hostNowMillis` and its neighbours are no host imports (the shim
    // implements them); `gen_refusal` refuses them as `clock` and `random`.
    let extern_fns: std::collections::HashSet<&str> = program
        .functions
        .iter()
        .filter(|f| f.is_extern && crate::trap::host_boundary_extern(&f.name).is_none())
        .map(|f| f.name.as_str())
        .collect();
    let global_names: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    // Method name to impl names, so a method call edge reaches the impl body.
    let mut method_impls: HashMap<String, Vec<String>> = HashMap::new();
    for imp in &program.impls {
        if let Some(key) = crate::types::type_key(&imp.ty) {
            for m in &imp.methods {
                method_impls
                    .entry(m.name.clone())
                    .or_default()
                    .push(crate::types::impl_method_name(&imp.protocol, &key, &m.name));
            }
        }
    }
    let expand = |calls: std::collections::HashSet<String>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in calls {
            if let Some(impls) = method_impls.get(&c) {
                out.extend(impls.iter().cloned());
            }
            out.push(c);
        }
        out
    };
    let direct = |f: &Function| -> Option<String> {
        if touches_globals(f, &global_names) {
            return Some("reads or writes module state".to_string());
        }
        for c in expand(fn_calls(&f.body)) {
            if let Some(why) = crate::effects::gen_refusal(&c) {
                return Some(why);
            }
            if extern_fns.contains(c.as_str()) {
                return Some(format!("calls the extern `{c}`"));
            }
        }
        None
    };
    const HINT: &str = "generators run at compile time — they may not use `extern`, \
                        module state, `print`, `writeFile`, `readLine`, `args`, `readFileBytes`, \
                        the clock, entropy, or logging sinks";
    // A function's own violation and call edges depend on its body alone, so
    // they are computed once and shared by every generator's search.
    let mut facts: HashMap<String, (Option<String>, Vec<String>)> = HashMap::new();

    for g in gen_fns {
        // Breadth-first, so the reported chain is the shortest.
        let mut queue: std::collections::VecDeque<Vec<&str>> =
            std::collections::VecDeque::from([vec![g.name.as_str()]]);
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::from([g.name.as_str()]);
        while let Some(path) = queue.pop_front() {
            let cur = *path.last().unwrap();
            let Some(f) = fn_map.get(cur) else { continue };
            if !facts.contains_key(cur) {
                facts.insert(cur.to_string(), (direct(f), expand(fn_calls(&f.body))));
            }
            let (violation, edges) = &facts[cur];
            if let Some(reason) = violation.clone() {
                let msg = if path.len() == 1 {
                    cerr!(
                        g.line,
                        "`gen fn {}` is not comptime-pure: it {reason} ({HINT})",
                        g.name
                    )
                } else {
                    let chain = path.join(" -> ");
                    cerr!(
                        g.line,
                        "`gen fn {}` is not comptime-pure: it reaches `{cur}` (via \
                         {chain}), which {reason} ({HINT})",
                        g.name
                    )
                };
                let mut d = msg;
                d.file = g.module.clone();
                out.push(d);
                break;
            }
            for callee in edges.clone() {
                if let Some(next) = fn_map.get(callee.as_str()) {
                    if seen.insert(next.name.as_str()) {
                        let mut np: Vec<&str> = path.clone();
                        np.push(next.name.as_str());
                        queue.push_back(np);
                    }
                }
            }
        }
    }
}

/// One source of a stored function value: a named function or a lambda
/// literal, one defunctionalization variant each, in collection order.
#[derive(Debug, Clone)]
pub struct StoredSource {
    /// The `fn(P..) -> R` signature, aliases resolved.
    pub sig: Type,
    pub named: Option<String>,
    pub lambda: Option<StoredLambda>,
}

/// A lambda source's effect summary, computed where the literal is checked;
/// the workers analysis unions these over a signature's sources.
#[derive(Debug, Clone)]
pub struct StoredLambda {
    /// The function whose body contains the literal.
    pub defined_in: String,
    pub line: usize,
    /// Every call name in the body (functions, builtins, methods).
    pub calls: std::collections::HashSet<String>,
    /// The first module-state binding the body reads or writes.
    pub touches_global: Option<String>,
    /// Signatures of other stored function values the body calls.
    pub nested_sigs: Vec<Type>,
}

/// Whole-program facts about stored function values.
#[derive(Debug, Clone, Default)]
pub struct StoredFnEffects {
    pub sources: Vec<StoredSource>,
    /// The functions handed to a `fn`-typed parameter at a call site.
    ///
    /// Kept apart from `sources` because an argument carries no
    /// defunctionalization tag; the `--workers` analysis reads `sources`
    /// alone, and the effect judgment reads both. Only `sig`, `named` and a
    /// lambda's `defined_in` and `line` are filled.
    pub arg_sources: Vec<StoredSource>,
    /// `(function, signature)` for each call through a stored fn value.
    pub calls: Vec<(String, Type)>,
}

impl StoredFnEffects {
    /// Returns every function a `fn`-typed value may be, stored or passed.
    pub fn every_source(&self) -> impl Iterator<Item = &StoredSource> {
        self.sources.iter().chain(self.arg_sources.iter())
    }
}

/// Whether two collected fn signatures could describe the same stored value:
/// structural equality where a `Type::Param` matches anything, because a
/// generic function's signatures are collected before substitution.
pub fn fn_sigs_match(a: &Type, b: &Type) -> bool {
    if matches!(a, Type::Param(_)) || matches!(b, Type::Param(_)) {
        return true;
    }
    match (a, b) {
        (Type::Fn(ap, ar), Type::Fn(bp, br)) => {
            ap.len() == bp.len()
                && ap.iter().zip(bp).all(|(x, y)| fn_sigs_match(x, y))
                && fn_sigs_match(ar, br)
        }
        (Type::Array(x), Type::Array(y)) | (Type::Stream(x), Type::Stream(y)) => {
            fn_sigs_match(x, y)
        }
        (Type::Map(x1, x2), Type::Map(y1, y2)) => fn_sigs_match(x1, y1) && fn_sigs_match(x2, y2),
        // Option and Result match through their payloads; a declared enum
        // compares equal.
        _ if crate::types::option_payload(a).is_some()
            && crate::types::option_payload(b).is_some() =>
        {
            fn_sigs_match(
                crate::types::option_payload(a).expect("an Option payload"),
                crate::types::option_payload(b).expect("an Option payload"),
            )
        }
        _ if crate::types::result_payloads(a).is_some()
            && crate::types::result_payloads(b).is_some() =>
        {
            let (x1, x2) = crate::types::result_payloads(a).expect("Result payloads");
            let (y1, y2) = crate::types::result_payloads(b).expect("Result payloads");
            fn_sigs_match(x1, y1) && fn_sigs_match(x2, y2)
        }
        _ => a == b,
    }
}

/// Returns the shortest call chain from `root` to a function that reads or
/// writes module state, with the global's name, or `None`. Gates `vyrn serve
/// --workers`: per-worker copies of module state would diverge. Output and
/// file I/O do not gate it.
pub fn module_state_use(
    program: &Program,
    root: &str,
    stored: &StoredFnEffects,
) -> Option<(Vec<String>, String)> {
    let global_names: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    if global_names.is_empty() {
        return None;
    }
    // Each signature called through a stored value is a pseudo node whose
    // callees are its sources. A lambda source that touches module state is
    // itself the offender.
    let pseudo_id = |sig: &Type| format!("a stored `{sig}` value");
    let mut pseudo_sigs: Vec<Type> = Vec::new();
    for (_, sig) in &stored.calls {
        if !pseudo_sigs.iter().any(|s| fn_sigs_match(s, sig)) {
            pseudo_sigs.push(sig.clone());
        }
    }
    let funcs: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    // A method name expands to every impl, so an impl reached through a
    // method call is walked.
    let mut method_impls: HashMap<String, Vec<String>> = HashMap::new();
    for imp in &program.impls {
        if let Some(key) = crate::types::type_key(&imp.ty) {
            for m in &imp.methods {
                method_impls
                    .entry(m.name.clone())
                    .or_default()
                    .push(crate::types::impl_method_name(&imp.protocol, &key, &m.name));
            }
        }
    }
    // Breadth-first with parent links, so the first hit is the shortest
    // chain; callees visit in sorted order.
    let mut parent: HashMap<String, Option<String>> = HashMap::from([(root.to_string(), None)]);
    let mut queue: std::collections::VecDeque<String> =
        std::collections::VecDeque::from([root.to_string()]);
    let chain_to = |cur: &String, parent: &HashMap<String, Option<String>>| {
        let mut chain = vec![cur.clone()];
        let mut p = parent[cur].clone();
        while let Some(prev) = p {
            p = parent[&prev].clone();
            chain.push(prev);
        }
        chain.reverse();
        chain
    };
    while let Some(cur) = queue.pop_front() {
        if let Some(sig) = pseudo_sigs.iter().find(|s| pseudo_id(s) == cur) {
            let mut callees: Vec<String> = Vec::new();
            for src in &stored.sources {
                if !fn_sigs_match(&src.sig, sig) {
                    continue;
                }
                if let Some(n) = &src.named {
                    callees.push(n.clone());
                }
                if let Some(l) = &src.lambda {
                    if let Some(g) = &l.touches_global {
                        return Some((chain_to(&cur, &parent), g.clone()));
                    }
                    callees.extend(l.calls.iter().cloned());
                    callees.extend(l.nested_sigs.iter().map(&pseudo_id));
                }
            }
            callees.sort();
            for callee in callees {
                let known = funcs.contains_key(callee.as_str())
                    || pseudo_sigs.iter().any(|s| pseudo_id(s) == callee);
                if known && !parent.contains_key(&callee) {
                    parent.insert(callee.clone(), Some(cur.clone()));
                    queue.push_back(callee);
                }
            }
            continue;
        }
        let Some(f) = funcs.get(cur.as_str()) else {
            continue;
        };
        if touches_globals(f, &global_names) {
            let mut names: Vec<&String> = program.globals.iter().map(|g| &g.name).collect();
            names.sort();
            let which = names
                .into_iter()
                .find(|g| {
                    let single: std::collections::HashSet<String> =
                        std::iter::once((*g).clone()).collect();
                    touches_globals(f, &single)
                })
                .cloned()
                .unwrap_or_default();
            return Some((chain_to(&cur, &parent), which));
        }
        let mut callees: Vec<String> = Vec::new();
        for c in fn_calls(&f.body) {
            if let Some(impls) = method_impls.get(&c) {
                callees.extend(impls.iter().cloned());
            }
            callees.push(c);
        }
        for (fname, sig) in &stored.calls {
            if fname == &cur {
                callees.push(pseudo_id(sig));
            }
        }
        callees.sort();
        for callee in callees {
            let known = funcs.contains_key(callee.as_str())
                || pseudo_sigs.iter().any(|s| pseudo_id(s) == callee);
            if known && !parent.contains_key(&callee) {
                parent.insert(callee.clone(), Some(cur.clone()));
                queue.push_back(callee);
            }
        }
    }
    None
}

/// Whether `f` reads or writes a global no local of the same name shadows.
/// Conservative: a name shadowed in one scope and global in another counts.
fn touches_globals(f: &Function, globals: &std::collections::HashSet<String>) -> bool {
    if globals.is_empty() {
        return false;
    }
    let mut local: std::collections::HashSet<String> =
        f.params.iter().map(|p| p.name.clone()).collect();
    collect_binders_block(&f.body, &mut local);
    global_ref_block(&f.body, globals, &local)
}

/// Collects every name a block binds (`let`, `for` variable); the caller
/// seeds the parameters.
fn collect_binders_block(b: &Block, out: &mut std::collections::HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::ForIn { var, body, .. } => {
                out.insert(var.clone());
                collect_binders_block(body, out);
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_binders_block(then_block, out);
                if let Some(eb) = else_block {
                    collect_binders_block(eb, out);
                }
            }
            Stmt::While { body, .. } | Stmt::Region { body, .. } => {
                collect_binders_block(body, out)
            }
            _ => {}
        }
    }
}

/// Checks a built-in sum's pattern arity: `None` binds nothing, `Some`,
/// `Ok` and `Err` bind one. They have no declaration to state it.
fn sum_arm_arity(name: &str, binds: usize, line: usize) -> Result<(), Diagnostic> {
    let want = usize::from(name != "None");
    if binds != want {
        return Err(cerr!(
            line,
            "variant `{name}` has {want} payload(s), but the pattern binds {binds}"
        ));
    }
    Ok(())
}

/// A name a global answers to and no local shadows is a reference, whether
/// read, written, dropped or called.
struct GlobalRef<'a> {
    globals: &'a HashSet<String>,
    found: bool,
}

impl GlobalRef<'_> {
    fn hit(&mut self, n: &str, locals: &HashSet<String>) {
        if self.globals.contains(n) && !locals.contains(n) {
            self.found = true;
        }
    }
}

impl BodyVisit<'_> for GlobalRef<'_> {
    fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
        match s {
            Stmt::Assign { name, .. }
            | Stmt::SetField { name, .. }
            | Stmt::IndexSet { name, .. }
            | Stmt::Drop { name, .. } => self.hit(name, locals),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr, locals: &HashSet<String>) -> bool {
        if self.found {
            return false;
        }
        match e {
            Expr::Var { name, .. } => self.hit(name, locals),
            // Calling a fn-typed global reads it. Functions and module state
            // share one namespace, so a declared function never collides.
            Expr::Call { name, .. } => self.hit(name, locals),
            _ => {}
        }
        !self.found
    }
}

/// Whether a block references a global that no local shadows. `local` is the
/// caller's flat set for the whole function, so a name read above its own
/// `let` counts as local; the walk adds lambda parameters and nested `let`s.
fn global_ref_block(
    b: &Block,
    globals: &std::collections::HashSet<String>,
    local: &std::collections::HashSet<String>,
) -> bool {
    let mut locals = local.clone();
    let mut v = GlobalRef {
        globals,
        found: false,
    };
    body_block(b, &mut locals, &mut v);
    v.found
}

/// [`global_ref_block`] for one expression.
fn global_ref_expr(
    e: &Expr,
    globals: &std::collections::HashSet<String>,
    local: &std::collections::HashSet<String>,
) -> bool {
    let mut v = GlobalRef {
        globals,
        found: false,
    };
    body_expr(e, local, &mut v);
    v.found
}

/// Holds the first violation of [`init_restrictions`].
struct InitRules<'a> {
    forbidden: &'a HashSet<String>,
    fn_module: &'a HashMap<String, Option<String>>,
    own_module: &'a Option<String>,
    all_globals: &'a HashSet<&'a str>,
    ready: &'a HashSet<String>,
    own_name: &'a str,
    line: usize,
    err: Option<Diagnostic>,
}

impl InitRules<'_> {
    fn fail(&mut self, d: Diagnostic) {
        if self.err.is_none() {
            self.err = Some(d);
        }
    }
}

impl BodyVisit<'_> for InitRules<'_> {
    const SCOPED: bool = false;

    fn expr(&mut self, e: &Expr, _: &HashSet<String>) -> bool {
        if self.err.is_some() {
            return false;
        }
        let (own_name, line) = (self.own_name, self.line);
        match e {
            Expr::Var { name, .. }
                if self.all_globals.contains(name.as_str()) && !self.ready.contains(name) =>
            {
                if name == own_name {
                    self.fail(cerr!(
                        line,
                        "module state `{own_name}` may not read itself in its \
                          own initializer"
                    ));
                } else {
                    self.fail(cerr!(
                        line,
                        "initializer of `{own_name}` reads `{name}`, a module-state \
                          binding declared later — a global may only read earlier ones"
                    ));
                }
                false
            }
            Expr::Call { name, .. }
                // Only imported modules are initialized first, so a
                // same-module function is forbidden with the externs.
                if self.forbidden.contains(name)
                    || matches!(self.fn_module.get(name), Some(m) if m == self.own_module) =>
            {
                self.fail(cerr!(
                    line,
                    "initializer of `{own_name}` may not call `{name}` — a \
                      module-state initializer runs before `main`, so it may use only \
                      literals, operators, built-ins, and functions imported from another \
                      module (whose state initializes first)"
                ));
                false
            }
            // No valid initializer holds a lambda. An expression body is
            // walked so the deeper diagnostic fires; a block body is not.
            Expr::Lambda {
                body: LambdaBody::Block(_),
                ..
            } => false,
            _ => true,
        }
    }
}

/// Returns the first violation of a module-state initializer's rules: it
/// reads no later global and not itself, and calls only builtins,
/// constructors and functions imported from another module, which
/// initialize first.
#[allow(clippy::too_many_arguments)]
fn init_restrictions(
    e: &Expr,
    forbidden: &HashSet<String>,
    fn_module: &HashMap<String, Option<String>>,
    own_module: &Option<String>,
    all_globals: &HashSet<&str>,
    ready: &HashSet<String>,
    own_name: &str,
    line: usize,
) -> Result<(), Diagnostic> {
    let mut v = InitRules {
        forbidden,
        fn_module,
        own_module,
        all_globals,
        ready,
        own_name,
        line,
        err: None,
    };
    body_expr(e, &HashSet::new(), &mut v);
    match v.err {
        Some(d) => Err(d),
        None => Ok(()),
    }
}

// Every reader here that asks one thing of a body, without typing it, is an
// impl of `BodyVisit`.
crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

/// Collects the names a call or a `try`-construct reaches. A call inside a
/// lambda counts for the enclosing function, its monomorphization site.
struct Calls<'a>(&'a mut HashSet<String>);

impl BodyVisit<'_> for Calls<'_> {
    const SCOPED: bool = false;

    fn expr(&mut self, e: &Expr, _: &HashSet<String>) -> bool {
        if let Expr::Call { name, .. } | Expr::TryConstruct { name, .. } = e {
            self.0.insert(name.clone());
        }
        true
    }
}

/// Returns every function name called anywhere in `b`.
pub fn fn_calls(b: &Block) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut locals = HashSet::new();
    body_block(b, &mut locals, &mut Calls(&mut out));
    out
}

#[cfg(test)]
mod tests {

    /// The method table, the seeded rows and the effect lattice each list the
    /// log levels; this keeps the three tables equal to `ast::LOG_LEVELS`.
    #[test]
    fn every_log_level_is_a_method_builtin_and_an_effect() {
        for lvl in crate::ast::LOG_LEVELS {
            let internal = crate::parser::method_builtin(lvl)
                .unwrap_or_else(|| panic!("`{lvl}` is not a method-form builtin"));
            assert_eq!(
                crate::ast::log_internal(internal),
                crate::ast::log_level_ordinal(lvl),
                "`{lvl}` maps to `{internal}`, which is not its internal spelling"
            );
            assert!(
                crate::prelude::signature(internal).is_some(),
                "`{internal}` has no seeded row"
            );
            assert!(
                crate::effects::gen_refusal(internal).is_some(),
                "`{lvl}` is a log level a `gen fn` may call"
            );
        }
    }

    /// `logging { level: .. }` compares against the ordinal, so reordering the
    /// table would change which calls a threshold suppresses.
    #[test]
    fn the_log_level_ordinals_are_the_table_order() {
        use crate::ast::{log_level_ordinal, DEFAULT_LOG_LEVEL};
        assert_eq!(log_level_ordinal("trace"), Some(0));
        assert_eq!(log_level_ordinal("debug"), Some(1));
        assert_eq!(log_level_ordinal("info"), Some(2));
        assert_eq!(log_level_ordinal("warn"), Some(3));
        assert_eq!(log_level_ordinal("error"), Some(4));
        assert_eq!(log_level_ordinal("shout"), None);
        assert_eq!(log_level_ordinal("info"), Some(DEFAULT_LOG_LEVEL));
    }
    use super::*;
    use crate::{lexer::lex, parser::parse};

    fn check_src(s: &str) -> Result<(), String> {
        check(&parse(lex(s).unwrap()).unwrap())
    }

    /// The sink is off until [`record`] turns it on.
    #[test]
    fn recording_keeps_the_type_of_every_node_and_the_type_arguments_of_a_call() {
        let p = parse(
            lex("fn id<T>(x: T) -> T {\n    return x\n}\n\n\
                 fn main() -> Int64 {\n    let n: Int64 = id(1)\n    return n\n}\n")
            .unwrap(),
        )
        .unwrap();

        assert!(!recording(), "the sink is off unless `record` turns it on");
        let r = record(&p);
        assert!(!recording(), "and off again afterwards");

        let types: Vec<String> = {
            let mut v: Vec<String> = r.node_types.values().map(|t| t.to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(types, vec!["Int64", "Int64", "Int64", "T"], "{types:?}");

        let calls: Vec<&(String, Vec<(String, Type)>)> = r.node_substs.values().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "id");
        assert_eq!(calls[0].1, vec![("T".to_string(), Type::Int)]);
    }

    /// What [`check_accum_recording`] holds equals what [`record`] makes. A
    /// record is keyed by the program's address only while a
    /// [`crate::own::Memo`] borrows it, so another program gets its own.
    #[test]
    fn the_analysiss_own_check_records_what_the_lowering_reads() {
        let src = "fn id<T>(x: T) -> T {\n    return x\n}\n\n\
                   fn main() -> Int64 {\n    let n: Int64 = id(1)\n    return n\n}\n";
        let p = parse(lex(src).unwrap()).unwrap();
        let want = record(&p);

        {
            let _memo = crate::own::Memo::open(&p);
            let (diags, _) = check_accum_recording(&p);
            assert!(diags.is_empty(), "{diags:?}");
            let got = recorded(&p);
            assert_eq!(got.node_types, want.node_types);
            assert_eq!(got.joins, want.joins);
            assert_eq!(got.node_substs.len(), want.node_substs.len());
            let q = parse(lex(src).unwrap()).unwrap();
            let other = recorded(&q);
            assert!(
                other
                    .node_types
                    .keys()
                    .all(|k| !got.node_types.contains_key(k)),
                "a record served for the wrong program"
            );
        }

        let after = recorded(&p);
        assert_eq!(after.node_types, want.node_types);
    }

    const RING: &str = "type Ring = { data: Array<Int64> }\n";

    #[test]
    fn a_projection_types_an_index_of_a_user_container() {
        assert!(check_src(&format!(
            "{RING}\
             impl Index for Ring {{ fn at(read self, i: Int64) -> read Int64 \
             {{ return self.data[i] }} }}\n\
             fn main() -> Int64 {{ let mut d: Array<Int64> = []\n d.push(7)\n \
             let r = Ring {{ data: d }}\n return r[0] }}"
        ))
        .is_ok());
    }

    /// `r.at(0)` reaches the impl like `r[0]`; only the free call `at(r, 0)`
    /// is refused.
    #[test]
    fn a_projection_answers_the_method_form_too() {
        let head = format!(
            "{RING}\
             impl Index for Ring {{ fn at(read self, i: Int64) -> read Int64 \
             {{ return self.data[i] }} }}\n\
             fn main() -> Int64 {{ let mut d: Array<Int64> = []\n d.push(7)\n \
             let r = Ring {{ data: d }}\n"
        );
        assert!(check_src(&format!("{head} return r.at(0) }}")).is_ok());
        let e = check_src(&format!("{head} return at(r, 0) }}")).unwrap_err();
        assert!(e.contains("`at(xs, i)` was removed"), "{e}");
    }

    /// A store coerces into the type `atSet` yields.
    #[test]
    fn a_projection_types_a_store_into_a_user_container() {
        assert!(check_src(&format!(
            "{RING}\
             impl Index for Ring {{ fn atSet(modify self, i: Int64) -> modify Int64 \
             {{ return self.data[i] }} }}\n\
             fn main() -> Int64 {{ let mut d: Array<Int64> = []\n d.push(7)\n \
             let mut r = Ring {{ data: d }}\n r[0] = 9\n return 0 }}"
        ))
        .is_ok());
    }

    /// A user container iterates through `Iterate`; the loop variable takes
    /// what `place nth` yields.
    #[test]
    fn a_for_loop_types_over_a_user_container() {
        let head = "type Ring = { data: Array<Int64> }\n\
                    impl Iterate for Ring {\n\
                      fn size(self) -> Int64 { return self.data.length }\n\
                      fn nth(read self, i: Int64) -> read Int64 { return self.data[i] }\n\
                    }\n";
        assert!(check_src(&format!(
            "{head}fn main() -> Int64 {{ let r = Ring {{ data: [] }}\n let mut s = 0\n \
             for x in r {{ s = s + x }}\n return s }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_projection_must_yield_a_place_not_a_value() {
        let e = check_src(&format!(
            "{RING}\
             impl Index for Ring {{ fn at(read self, i: Int64) -> read Int64 \
             {{ return i + 1 }} }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("returns a value, not a place"), "{e}");
    }

    #[test]
    fn a_projection_yields_once_and_last() {
        let e = check_src(&format!(
            "{RING}\
             impl Index for Ring {{ fn at(read self, i: Int64) -> read Int64 {{\n\
                 if i < 0 {{ return self.data[0] }}\n\
                 return self.data[i]\n\
             }} }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("exactly one `return <place>`"), "{e}");
    }

    #[test]
    fn a_projection_may_not_yield_somebody_elses_place() {
        let e = check_src(&format!(
            "let mut spare: Array<Int64> = []\n\
             {RING}\
             impl Index for Ring {{ fn at(read self, i: Int64) -> read Int64 \
             {{ return spare[i] }} }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("the access site does not own"), "{e}");
    }

    #[test]
    fn a_projection_may_not_propagate_with_a_question_mark() {
        let e = check_src(&format!(
            "type Box = {{ v: Option<Array<Int64>> }}\n\
             impl Index for Box {{ fn at(read self, i: Int64) -> read Int64 \
             {{ return self.v?[i] }} }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("uses `?`, which returns"), "{e}");
    }

    /// An enum container and the projection that reads through its payload.
    const LEDGER: &str = "type E = | Tag(String) | Blank\n\
                          type Led = { rows: Array<E> }\n\
                          impl Index for Led {\n\
                              fn at(read self, i: Int64) -> read E {\n\
                                  return self.rows[i]\n\
                              }\n\
                          }\n";

    #[test]
    fn a_place_may_root_in_a_borrowing_prologue_let() {
        // Two links: a field borrow, then a payload borrow through it. The
        // root trace is transitive.
        assert!(check_src(
            "type E = | Tag(Array<Int64>) | Blank\n\
             type Box = { v: E }\n\
             impl Index for Box {\n\
                 fn at(read self, i: Int64) -> read Int64 {\n\
                     let e = self.v\n\
                     let Tag(xs) = e\n\
                     return xs[i]\n\
                 }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        )
        .is_ok());
    }

    #[test]
    fn a_prologue_let_that_owns_is_not_a_root() {
        let e = check_src(
            "type Box = { v: Array<Int64> }\n\
             impl Index for Box {\n\
                 fn at(read self, i: Int64) -> read Int64 {\n\
                     let xs = [1, 2, 3]\n\
                     return xs[i]\n\
                 }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("the access site does not own"), "{e}");
    }

    #[test]
    fn a_concrete_chain_dispatches() {
        // Every link declares a concrete named result (`E`, then `String`),
        // so the chain resolves.
        assert!(check_src(&format!(
            "{LEDGER}\
             impl Index for E {{\n\
                 fn s(read self) -> read String {{\n\
                     let Tag(v) = self\n\
                     return v\n\
                 }}\n\
             }}\n\
             fn main() -> Int64 {{\n\
                 let led = Led {{ rows: [] }}\n\
                 return led.at(0).s().byteLength\n\
             }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_generic_chain_is_still_refused() {
        // The inner `at` declares `-> read T`, which has no impl key without a
        // substitution, so the next projection cannot resolve.
        let e = check_src(
            "type E = | Tag(String) | Blank\n\
             type Slab<T> = { vals: Array<T> }\n\
             impl<T> Index for Slab<T> {\n\
                 fn at(read self, i: Int64) -> read T {\n\
                     return self.vals[i]\n\
                 }\n\
             }\n\
             impl Index for E {\n\
                 fn s(read self) -> read String {\n\
                     let Tag(v) = self\n\
                     return v\n\
                 }\n\
             }\n\
             fn main() -> Int64 {\n\
                 let s: Slab<E> = Slab { vals: [Tag(\"x\")] }\n\
                 return s.at(0).s().byteLength\n\
             }",
        )
        .unwrap_err();
        assert!(e.contains("not a concrete named type"), "{e}");
    }

    /// A container with an optional projection.
    const OPT: &str = "type Row = {{ n: Int64 }}\n\
                       type B = {{ rows: Array<Row> }}\n\
                       impl Index for B {{\n\
                           fn tryAt(read self, i: Int64) -> read Option<Row> {{\n\
                               if i < 0 || i >= self.rows.length {{\n\
                                   return None\n\
                               }}\n\
                               return Some(self.rows[i])\n\
                           }}\n\
                       }}\n";

    #[test]
    fn an_optional_projection_is_read_where_it_is_tested() {
        let head = format!(
            "{}fn main() -> Int64 {{\n    let b = B {{ rows: [] }}\n",
            OPT.replace("{{", "{").replace("}}", "}")
        );
        assert!(check_src(&format!(
            "{head}    if let Some(r) = b.tryAt(0) {{ return r.n }}\n    return 0\n}}"
        ))
        .is_ok());
        let e = check_src(&format!("{head}    let o = b.tryAt(0)\n    return 0\n}}")).unwrap_err();
        assert!(e.contains("read where it is tested"), "{e}");
        // The miss has no place to bind, so only `Some` tests it.
        let e = check_src(&format!(
            "{head}    if let None = b.tryAt(0) {{ return 1 }}\n    return 0\n}}"
        ))
        .unwrap_err();
        assert!(e.contains("tested for its hit"), "{e}");
    }

    #[test]
    fn an_optional_projection_needs_its_shape() {
        // Two miss tests: statements between them would run after the first
        // miss decided.
        let e = check_src(
            "type B = { rows: Array<Int64> }\n\
             impl Index for B {\n\
                 fn tryAt(read self, i: Int64) -> read Option<Int64> {\n\
                     if i < 0 {\n\
                         return None\n\
                     }\n\
                     if i >= self.rows.length {\n\
                         return None\n\
                     }\n\
                     return Some(self.rows[i])\n\
                 }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("one `if <miss> { return None }`"), "{e}");
    }

    #[test]
    fn a_hit_prologue_binds_after_the_miss_is_decided() {
        // Statements after the one decision run only on the hit, so a payload
        // binding may live there.
        assert!(check_src(
            "type E = | Tag(Array<Int64>) | Blank\n\
             type B = { v: E }\n\
             impl Index for B {\n\
                 fn tryAt(read self, i: Int64) -> read Option<Int64> {\n\
                     let mut n = 0\n\
                     if let Tag(scan) = self.v {\n\
                         n = scan.length\n\
                     }\n\
                     if i < 0 || i >= n {\n\
                         return None\n\
                     }\n\
                     let e = self.v\n\
                     let Tag(xs) = e\n\
                     return Some(xs[i])\n\
                 }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        )
        .is_ok());
    }

    #[test]
    fn the_sugar_names_cannot_be_optional() {
        let e = check_src(
            "type B = { rows: Array<Int64> }\n\
             impl Index for B {\n\
                 fn at(read self, i: Int64) -> read Option<Int64> {\n\
                     if i < 0 {\n\
                         return None\n\
                     }\n\
                     return Some(self.rows[i])\n\
                 }\n\
             }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("consumes a place unconditionally"), "{e}");
    }

    /// A protocol that requires a projection, and a type to implement it.
    const SHELF: &str = "protocol Shelf { fn tag(read self, i: Int64) -> read String }\n\
                         type Crate = { labels: Array<String> }\n";

    #[test]
    fn a_protocol_projection_is_satisfied_by_a_places_member() {
        assert!(check_src(&format!(
            "{SHELF}\
             impl Shelf for Crate {{\n\
                 fn tag(read self, i: Int64) -> read String {{\n\
                     return self.labels[i]\n\
                 }}\n\
             }}\n\
             fn main() -> Int64 {{\n\
                 let c = Crate {{ labels: [\"a\"] }}\n\
                 return c.tag(0).byteLength\n\
             }}"
        ))
        .is_ok());
    }

    #[test]
    fn a_missing_or_mismatched_projection_is_a_conformance_error() {
        let e = check_src(&format!(
            "{SHELF}\
             impl Shelf for Crate {{}}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("does not provide the projection"), "{e}");
        // A plain method returns a copy, which is not the contract.
        let e = check_src(&format!(
            "{SHELF}\
             impl Shelf for Crate {{\n\
                 fn tag(self, i: Int64) -> String {{\n\
                     return self.labels[i].copy()\n\
                 }}\n\
             }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("does not provide the projection"), "{e}");
        let e = check_src(&format!(
            "{SHELF}\
             impl Shelf for Crate {{\n\
                 fn tag(read self, i: Int64, j: Int64) -> read String {{\n\
                     return self.labels[i + j]\n\
                 }}\n\
             }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("does not match protocol"), "{e}");
    }

    /// A protocol the compiler knows by name is held to its capabilities too:
    /// `print(c)` reads `c`, so a `modify self` show would write through a read.
    #[test]
    fn a_known_protocols_impl_takes_the_declared_capabilities() {
        for (imp, want) in [
            (
                "impl Show for C { fn show(modify self) -> String { return \"c\" } }",
                "it declares `fn show(self) -> String`",
            ),
            (
                "impl Copy for C { fn copy(consume self) -> C { return self } }",
                "it declares `fn copy(self) -> C`",
            ),
            (
                "impl Owned for C { fn release(self) {} }",
                "it declares `fn release(consume self) -> Unit`",
            ),
        ] {
            let e = check_src(&format!(
                "type C = {{ n: Int64 }}\n{imp}\nfn main() -> Int64 {{ return 0 }}"
            ))
            .unwrap_err();
            assert!(
                e.contains("does not match protocol") && e.contains(want),
                "{imp}: {e}"
            );
        }
    }

    #[test]
    fn an_impl_of_an_undeclared_protocol_is_refused() {
        let e = check_src(
            "type C = { n: Int64 }\n\
             impl NoSuchProto for C { fn foo(self) -> Int64 { return 1 } }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("no protocol named `NoSuchProto`"), "{e}");
    }

    #[test]
    fn a_bounded_receiver_cannot_dispatch_a_projection() {
        let e = check_src(&format!(
            "{SHELF}\
             impl Shelf for Crate {{\n\
                 fn tag(read self, i: Int64) -> read String {{\n\
                     return self.labels[i]\n\
                 }}\n\
             }}\n\
             fn first<T: Shelf>(s: T) -> Int64 {{\n\
                 return s.tag(0).byteLength\n\
             }}\n\
             fn main() -> Int64 {{ return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("no body to inline"), "{e}");
    }

    /// A record rendered through its own `impl Show`. The program declares
    /// the protocol; the compiler knows only its name.
    const SHOW_SRC: &str = "protocol Show { fn show(self) -> String }\n\
                            type P = { x: Int64 }\n\
                            impl Show for P {\n\
                            fn show(self) -> String { return \"p\" }\n\
                            }\n";

    #[test]
    fn a_declared_show_renders_in_all_three_renderers() {
        for call in [
            "print(p)",
            "let s: String = p.toString()",
            "let s: String = \"\\{p}\"",
            "let v: Value = value(p)",
        ] {
            assert!(
                check_src(&format!(
                    "{SHOW_SRC}fn main() -> Int64 {{ let p = P {{ x: 1 }}\n {call}\n return 0 }}"
                ))
                .is_ok(),
                "`{call}` should render through `impl Show for P`"
            );
        }
    }

    /// A scalar never reaches the dispatch: `examples/protocol.vyrn` declares
    /// this impl with body `self.toString()`, which would recurse forever.
    #[test]
    fn a_scalar_renders_by_the_language_whatever_is_declared() {
        assert!(check_src(
            "protocol Show { fn show(self) -> String }\n\
             impl Show for Int64 { fn show(self) -> String { return self.toString() } }\n\
             fn main() -> Int64 { print(7)\n let s: String = \"\\{7}\"\n return 0 }"
        )
        .is_ok());
    }

    #[test]
    fn a_show_that_is_not_a_string_is_refused() {
        let e = check_src(
            "protocol Show { fn show(self) -> Int64 }\n\
             type P = { x: Int64 }\n\
             impl Show for P { fn show(self) -> Int64 { return 1 } }\n\
             fn main() -> Int64 { let p = P { x: 1 }\n print(p)\n return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("must hand back a String"), "{e}");
    }

    #[test]
    fn copy_types_as_its_receiver() {
        assert!(check_src(
            "fn main() -> Int64 { let s = \"a\" + \"b\"\n let t: String = s.copy()\n \
             return t.byteLength }"
        )
        .is_ok());
        assert!(check_src(
            "type R = { name: String }\n\
             fn main() -> Int64 { let r = R { name: \"a\" }\n let q: R = r.copy()\n \
             return q.name.byteLength }"
        )
        .is_ok());
        assert!(check_src(
            "fn main() -> Int64 { let m: Map<String, String> = [:]\n let n: Map<String, String> = \
             m.copy()\n return n.length }"
        )
        .is_ok());
    }

    /// A scalar copies to itself, so one generic `x.copy()` serves every
    /// instance.
    #[test]
    fn copy_of_a_scalar_is_the_value() {
        assert!(check_src("fn main() -> Int64 { let n = 5\n return n.copy() }").is_ok());
    }

    /// A field copy of an `impl Owned` type would run its release twice. The
    /// refusal reaches through a record that holds one.
    #[test]
    fn copy_refuses_a_declared_container() {
        let src = "protocol Owned { fn release(self) }\n\
                   type Ring = { buf: Array<Int64> }\n\
                   impl Owned for Ring { fn release(self) { print(1) } }\n\
                   fn main() -> Int64 { let r = Ring { buf: [] }\n let q = r.copy()\n return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("`copy` cannot copy `Ring`"), "{e}");
        let nested = "protocol Owned { fn release(self) }\n\
                      type Ring = { buf: Array<Int64> }\n\
                      impl Owned for Ring { fn release(self) { print(1) } }\n\
                      type Holder = { r: Ring }\n\
                      fn main() -> Int64 { let h = Holder { r: Ring { buf: [] } }\n \
                      let q = h.copy()\n return 0 }";
        let e = check_src(nested).unwrap_err();
        assert!(e.contains("`copy` cannot copy `Ring`"), "{e}");
    }

    /// `impl Copy for T` overrides that refusal; the result is the impl's type.
    #[test]
    fn copy_dispatches_to_a_declared_impl() {
        let src = "protocol Owned { fn release(self) }\n\
                   type Ring = { buf: Array<Int64> }\n\
                   impl Owned for Ring { fn release(self) { print(1) } }\n\
                   impl Copy for Ring { fn copy(self) -> Ring { return Ring { buf: [] } } }\n\
                   fn main() -> Int64 { let r = Ring { buf: [] }\n let q = r.copy()\n \
                   return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A self-referring type has no structural bottom; an `impl Copy` lifts
    /// the refusal.
    #[test]
    fn copy_dispatches_for_a_self_referring_type() {
        let src = "type Node = { next: Option<Node>, n: Int64 }\n\
                   impl Copy for Node { fn copy(self) -> Node { \
                   return Node { next: None, n: self.n } } }\n\
                   fn main() -> Int64 { let a = Node { next: None, n: 1 }\n \
                   let b = a.copy()\n return b.n }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A `Stream<T>` is a cursor over a producer, not a container.
    #[test]
    fn copy_refuses_a_stream() {
        let src = "fn main() -> Int64 { let xs: Array<Int64> = []\n let s = fromArray(xs)\n \
                   let t = s.copy()\n close(t)\n return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("`copy` cannot copy a Stream"), "{e}");
    }

    #[test]
    fn copy_takes_no_arguments() {
        let e = check_src("fn main() -> Int64 { let s = \"a\"\n let t = s.copy(1)\n return 0 }")
            .unwrap_err();
        assert!(e.contains("`copy` takes no arguments"), "{e}");
    }

    /// Five expressions take their type from the annotation, which may be an
    /// alias.
    #[test]
    fn an_alias_supplies_a_contextual_literals_type() {
        let decls = "type IntMap = Map<String, Int64> \
                     type Nums = Array<Int64> \
                     type Age = Int64 where value >= 18 \
                     type MaybeAge = Option<Age> ";
        let ok = |body: &str| {
            let src = format!("{decls} fn main() -> Int64 {{ {body} return 0 }}");
            check_src(&src).map_err(|e| format!("{body}: {e}")).unwrap();
        };
        ok("let m: IntMap = [:]");
        ok("let a: Nums = []");
        // A filled array literal takes the alias, not a fixed `Array<Int64, 3>`.
        ok("let a: Nums = [1, 2, 3]");
        ok("let m: IntMap = [\"a\": 1]");
        ok("let m: MaybeAge = None");
        ok("let m: MaybeAge = Some(21)");
        // The payload's refinement travels with the alias: this is a predicate
        // failure naming `Age`, not "expected MaybeAge, found Option<Int64>".
        let e = check_src(&format!(
            "{decls} fn main() -> Int64 {{ let m: MaybeAge = Some(3) return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("`Age`"), "{e}");
    }

    #[test]
    fn if_let_binds_the_payload_type_and_rejects_wrong_patterns() {
        // The binder takes the Option payload type and is usable in the body.
        assert!(check_src(
            "fn f() -> Option<Int64> { return Some(1) } \
             fn main() -> Int64 { if let Some(v) = f() { return v } return 0 }"
        )
        .is_ok());
        // Wrong pattern for the scrutinee shape.
        let bad = check_src(
            "fn f() -> Option<Int64> { return Some(1) } \
             fn main() -> Int64 { if let Ok(v) = f() { return v } return 0 }",
        )
        .unwrap_err();
        // A name that is not one of the scrutinee's variants.
        assert!(bad.contains("is not a variant of"), "{bad}");
    }

    #[test]
    fn remainder_on_floats_gets_the_integer_only_hint() {
        let e = check_src("fn main() -> Int64 { let a = 1.5 % 2.0 return 0 }").unwrap_err();
        assert!(
            e.contains("no `%` on Float64") && e.contains("integer remainder only"),
            "{e}"
        );
    }

    #[test]
    fn module_state_use_reports_the_call_chain_and_global() {
        let src = "let mut hits: Int64 = 0\n\
                   fn bump() -> Int64 { hits = hits + 1\n return hits }\n\
                   fn respond() -> Int64 { return bump() }\n\
                   fn handle(n: Int64) -> Int64 { return respond() }\n";
        let program = parse(lex(src).unwrap()).unwrap();
        let (chain, global) =
            module_state_use(&program, "handle", &Default::default()).expect("stateful");
        assert_eq!(chain, vec!["handle", "respond", "bump"]);
        assert_eq!(global, "hits");
    }

    #[test]
    fn workers_gate_walks_through_stored_values() {
        // The global is reached only through the stored value's source, so
        // the chain names the pseudo node and the source function.
        let src = "let mut hits: Int64 = 0\n\
             fn stateful(x: Int64) -> Int64 { return x + hits }\n\
             fn make() -> fn(Int64) -> Int64 { return stateful }\n\
             fn respond(x: Int64) -> Int64 { let m = make()  return m(x) }\n\
             fn handle(n: Int64) -> Int64 { return respond(n) }\n";
        let program = parse(lex(src).unwrap()).unwrap();
        let stored = stored_fn_effects(&program);
        let (chain, global) =
            module_state_use(&program, "handle", &stored).expect("stateful through storage");
        assert_eq!(global, "hits");
        assert_eq!(
            chain,
            vec![
                "handle".to_string(),
                "respond".to_string(),
                "a stored `fn(Int64) -> Int64` value".to_string(),
                "stateful".to_string(),
            ],
            "{chain:?}"
        );
    }

    #[test]
    fn workers_gate_ignores_isolated_stored_values() {
        let src = "let mut hits: Int64 = 0\n\
             fn pure(x: Int64) -> Int64 { return x * 2 }\n\
             fn make() -> fn(Int64) -> Int64 { return pure }\n\
             fn respond(x: Int64) -> Int64 { let m = make()  return m(x) }\n\
             fn handle(n: Int64) -> Int64 { return respond(n) }\n";
        let program = parse(lex(src).unwrap()).unwrap();
        let stored = stored_fn_effects(&program);
        assert!(module_state_use(&program, "handle", &stored).is_none());
    }

    #[test]
    fn module_state_use_gates_on_non_root_module_state() {
        // After linking, an imported module's global is an ordinary global.
        // Tagging the state and its accessor with a foreign module simulates
        // the linked program; the gate still fires.
        let src = "let mut count: Int64 = 0\n\
                   fn bump() -> Int64 { count = count + 1\n return count }\n\
                   fn handle(n: Int64) -> Int64 { return bump() }\n";
        let mut program = parse(lex(src).unwrap()).unwrap();
        for g in &mut program.globals {
            g.module = Some("store.vyrn".into());
        }
        if let Some(f) = program.functions.iter_mut().find(|f| f.name == "bump") {
            f.module = Some("store.vyrn".into());
        }
        let (chain, global) =
            module_state_use(&program, "handle", &Default::default()).expect("stateful");
        assert_eq!(chain, vec!["handle", "bump"]);
        assert_eq!(global, "count");
    }

    #[test]
    fn module_state_use_is_none_for_a_pure_tree_even_with_globals_present() {
        // Only `handle`'s call tree counts, and output and file I/O do not gate.
        let src = "let mut hits: Int64 = 0\n\
                   fn other() -> Int64 { hits = hits + 1\n return hits }\n\
                   fn pure(n: Int64) -> Int64 { return n * 2 }\n\
                   fn handle(n: Int64) -> Int64 { print(n)\n let r = readFile(\"x\")\n \
                       return pure(n) }\n";
        let program = parse(lex(src).unwrap()).unwrap();
        assert!(module_state_use(&program, "handle", &Default::default()).is_none());
    }

    #[test]
    fn payload_enum_is_codable() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit \
                   fn f(s: Shape) -> String { return toJson(s) } \
                   fn g(s: String) -> Validation<Shape> { return fromJson<Shape>(s) } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn result_is_codable_and_named_aliases_work() {
        let src = "type R = Result<Bool, String> \
                   fn f(x: R) -> String { return toJson(x) } \
                   fn g(s: String) -> Validation<R> { return fromJson<R>(s) } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn enum_with_noncodable_payload_names_the_variant() {
        let src = "type Bad = | Boxed(Logger) | Empty \
                   fn f(b: Bad) -> String { return toJson(b) } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot encode"), "{e}");
        assert!(
            e.contains("variant `Boxed`"),
            "names the offending variant: {e}"
        );
    }

    #[test]
    fn validation_stays_non_codable() {
        let src = "fn f(s: String) -> String { \
                       let v = fromJson<Issue>(s) \
                       return toJson(v) } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot encode `Validation`"), "{e}");
    }

    #[test]
    fn option_of_result_is_codable() {
        let src = "type Wrap = { r: Option<Result<Int64, String>> } \
                   fn f(w: Wrap) -> String { return toJson(w) } \
                   fn g(s: String) -> Validation<Wrap> { return fromJson<Wrap>(s) } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn pure_gen_fn_is_accepted() {
        // readFile is mediated (permitted); the rest is ordinary pure code.
        let src = "gen fn g(dir: String) -> String { \
                       let r = readFile(dir) \
                       return \"fn x() -> Int64 { return 0 }\" } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A match arm's binder shadows a module global of the same name, so
    /// `JArr(items) => emitArr(items)` does not read a global `items`.
    #[test]
    fn a_match_binder_named_like_a_global_is_still_pure() {
        let src = "let mut items: Int64 = 0 \
                   gen fn g(o: Option<Int64>) -> String { \
                       let n = match o { Some(items) => items, None => 0 } \
                       return \"\" } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// An arm that binds nothing still reads the global.
    #[test]
    fn a_global_read_in_a_sibling_arm_is_still_impure() {
        let src = "let mut items: Int64 = 0 \
                   gen fn g(o: Option<Int64>) -> String { \
                       let n = match o { Some(items) => items, None => items } \
                       return \"\" } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("module state"), "{e}");
    }

    #[test]
    fn gen_fn_using_writefile_is_rejected() {
        let e = check_src(
            "gen fn g() -> String { let w = writeFile(\"x\", \"y\") return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("not comptime-pure"), "{e}");
        assert!(e.contains("`writeFile`"), "{e}");
    }

    /// `print` and `writeStdout` are one effect with one verdict. A generator
    /// reruns only on a cache miss, so its output would depend on the cache.
    #[test]
    fn gen_fn_using_print_is_rejected() {
        let src = "gen fn g() -> String { print(\"hi\") return \"\" } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(
            e.contains("not comptime-pure") && e.contains("`print`"),
            "{e}"
        );
    }

    #[test]
    fn gen_fn_calling_extern_is_rejected() {
        let e = check_src(
            "extern fn host() -> Int64 \
             gen fn g() -> String { let n = host() return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("not comptime-pure") && e.contains("extern"),
            "{e}"
        );
    }

    #[test]
    fn gen_fn_touching_module_state_is_rejected() {
        let e = check_src(
            "let mut counter = 0 \
             gen fn g() -> String { return counter.toString() } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("not comptime-pure") && e.contains("module state"),
            "{e}"
        );
    }

    #[test]
    fn gen_fn_transitive_impurity_names_the_chain() {
        let e = check_src(
            "fn helper() -> String { let w = writeFile(\"a\", \"b\") return \"\" } \
             gen fn g() -> String { return helper() } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("not comptime-pure"), "{e}");
        assert!(e.contains("helper") && e.contains("`writeFile`"), "{e}");
    }

    #[test]
    fn io_builtins_have_their_signatures() {
        // Types flow: args() -> Array<String>, readLine() -> Option<String>,
        // readFile -> Result<String, String>, writeFile -> Result<Bool, String>,
        // readFileBytes -> Result<Array<UInt8>, String>,
        // stringFromBytes(Array<UInt8>) -> Result<String, String>.
        let src = "fn main() -> Int64 { \
                       let a: Array<String> = args() \
                       let l: Option<String> = readLine() \
                       let r: Result<String, String> = readFile(\"p\") \
                       let w: Result<Bool, String> = writeFile(\"p\", \"c\") \
                       let b: Result<Array<UInt8>, String> = readFileBytes(\"p\") \
                       let s: Result<String, String> = stringFromBytes(bytes(\"x\")) \
                       return a.length }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// The float bit views take and answer a `UInt64`.
    #[test]
    fn the_bit_views_answer_a_uint64() {
        let src = "fn main() -> Int64 { \
                       let b: UInt64 = floatBits(1.0) \
                       let f: Float64 = floatFromBits(b) \
                       return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A std function called without its import names the module to import.
    #[test]
    fn a_moved_builtin_names_the_module_it_moved_to() {
        for (call, want) in [
            ("slice(\"hi\", 0, 1)", "`slice` is `std/strpred`'s"),
            ("contains(\"hi\", \"h\")", "`contains` is `std/strpred`'s"),
            ("chars(\"hi\")", "`chars` is `std/text`'s"),
            ("hexEncode(\"hi\")", "`hexEncode` is `std/codecs`'s"),
        ] {
            let e = check_src(&format!("fn main() -> Int64 {{ let x = {call} return 0 }}"))
                .unwrap_err();
            assert!(e.contains(want), "{e}");
            assert!(e.contains("import {"), "the fix is an import line: {e}");
        }
    }

    /// A user function of that name wins over the hint.
    #[test]
    fn a_moved_name_is_declarable_again() {
        let src = "fn contains(s: String, n: String) -> Bool { return s == n } \
                   fn main() -> Int64 { if contains(\"a\", \"a\") { return 1 } return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// The rows of `@reserve` and `@tally` answer `Array<T>` and
    /// `Map<K, Int64>`, and a `type Buf = Array<Int64>` receiver takes the
    /// result back through ordinary coercion.
    #[test]
    fn a_rebuilt_receiver_keeps_its_alias() {
        let src = "type Buf = Array<Int64> \
                   type Counts = Map<String, Int64> \
                   type Box = { b: Buf, c: Counts } \
                   fn fill(b: Buf) -> Int64 { return b.length } \
                   fn main() -> Int64 { \
                       let mut x = Box { b: [], c: [:] } \
                       x.b.reserve(8) \
                       x.b.push(3) \
                       x.c.tally(\"k\", 5) \
                       x.b.clear() \
                       x.b.append([1, 2]) \
                       x.b.copyFrom([3, 4]) \
                       x.c.tallyBytes(bytes(\"k\"), 1) \
                       return fill(x.b) + x.c.keys().length }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A `Gone::Module` name is not reserved, or the import the hint names
    /// could not be written. A `Gone::Removed` name is reserved, or a user
    /// `fn push` would hide the hint.
    #[test]
    fn every_moved_name_is_gone_from_reserved() {
        for (n, g) in MOVED_TO_STD {
            match g {
                // A desugared name is an ordinary export: the `Module` rule.
                Gone::Module(_) | Gone::Desugared { .. } => assert!(
                    !RESERVED.contains(n),
                    "`{n}` is both reserved and said to live in a std module"
                ),
                Gone::Removed(_) => assert!(
                    RESERVED.contains(n),
                    "`{n}` is said to be removed but a program may declare it, \
                     which would shadow the hint"
                ),
            }
        }
    }

    /// The sentence each removed spelling in [`MOVED_TO_STD`] gives.
    #[test]
    fn removed_spellings_are_rows_of_one_table() {
        let removed: Vec<&str> = MOVED_TO_STD
            .iter()
            .filter(|(_, g)| matches!(g, Gone::Removed(_)))
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(
            removed,
            vec!["str", "concat", "len", "list", "toString", "push", "at", "alen", "array"]
        );
    }

    #[test]
    fn io_builtins_are_not_constant_in_predicates() {
        // `where` predicates are const-only; an I/O call can never satisfy one.
        let e = check_src(
            "type P = String where readFile(value) == Ok(\"x\") \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("call"), "{e}");
    }

    #[test]
    fn bytes_returns_uint8_array() {
        let ok = "fn main() -> Int64 { let b: Array<UInt8> = bytes(\"hi\") return b.length }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
    }

    #[test]
    fn string_index_is_uint8() {
        let ok = "fn main() -> Int64 { let s = \"hi\" let b: UInt8 = s[0] return Int64(b) }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
    }

    #[test]
    fn string_ordering_type_rule() {
        // `< <= > >=` compare two Strings; a String and a non-String do not.
        for op in ["<", "<=", ">", ">="] {
            let ok =
                format!("fn main() -> Int64 {{ if \"a\" {op} \"b\" {{ return 1 }} return 0 }}");
            assert!(check_src(&ok).is_ok(), "{op}: {:?}", check_src(&ok));
        }
        let e = check_src("fn main() -> Int64 { if \"a\" < 3 { return 1 } return 0 }").unwrap_err();
        assert!(e.contains("numeric or String"), "{e}");
    }

    #[test]
    fn rename_and_fsync_have_the_io_signatures() {
        let src = "fn main() -> Int64 { \
                       let r: Result<Bool, String> = renameFile(\"a\", \"b\") \
                       let s: Result<Bool, String> = fsyncFile(\"a\") \
                       return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn rename_and_fsync_are_rejected_in_a_generator() {
        for io in ["renameFile(\"a\", \"b\")", "fsyncFile(\"a\")"] {
            let src = format!(
                "gen fn g() -> String {{ let w = {io} return \"\" }} \
                 fn main() -> Int64 {{ return 0 }}"
            );
            let e = check_src(&src).unwrap_err();
            assert!(e.contains("not comptime-pure"), "{io}: {e}");
        }
    }

    #[test]
    fn load_result_prelude_enum_is_matchable() {
        // `load` is a call-site desugar; the injected `LoadResult<T>` enum lets a
        // caller match all three outcomes without importing anything.
        let src = "type Rec = { n: Int64 } \
                   fn describe(r: LoadResult<Rec>) -> Int64 { \
                       return match r { \
                           Missing => 1, Corrupt(iss) => 2, Loaded(x) => x.n } } \
                   fn main() -> Int64 { return describe(load(Rec, \"p\")) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A file with exports is a library and needs no `main`; a file with
    /// neither is refused.
    #[test]
    fn library_modules_do_not_need_main() {
        assert!(check_src("export fn double(x: Int64) -> Int64 { return x * 2 }").is_ok());
        assert!(check_src("export type Age = Int64 where value >= 18").is_ok());
        let e = check_src("fn helper(x: Int64) -> Int64 { return x }").unwrap_err();
        assert!(e.contains("no `main` function found"), "{e}");
    }

    #[test]
    fn file_with_tests_needs_no_main() {
        // A file consisting only of tests is a valid library-like module.
        assert!(check_src("test \"ok\" { assert(1 == 1) }").is_ok());
    }

    #[test]
    fn assert_and_asserteq_check_inside_a_test() {
        let src = "test \"t\" { assert(true) assertEq(1 + 1, 2) assertEq(\"a\", \"a\") }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn assert_outside_a_test_is_rejected() {
        let e = check_src("fn main() -> Int64 { assert(true) return 0 }").unwrap_err();
        assert!(e.contains("only available inside a `test` block"), "{e}");
        let e = check_src("fn main() -> Int64 { assertEq(1, 1) return 0 }").unwrap_err();
        assert!(e.contains("only available inside a `test` block"), "{e}");
    }

    #[test]
    fn asserteq_needs_equal_equatable_types() {
        // Mismatched types on the two sides.
        let e = check_src("test \"t\" { assertEq(1, true) }").unwrap_err();
        assert!(e.contains("equatable"), "{e}");
    }

    #[test]
    fn duplicate_test_names_are_rejected() {
        let e =
            check_src("test \"dup\" { assert(true) } test \"dup\" { assert(true) }").unwrap_err();
        assert!(e.contains("duplicate test name"), "{e}");
    }

    #[test]
    fn duplicate_bench_names_are_rejected() {
        // The refusal in a bench says `bench`.
        let e = check_src("bench \"dup\" { let a = 1 } bench \"dup\" { let a = 1 }").unwrap_err();
        assert!(e.contains("duplicate bench name"), "{e}");
    }

    #[test]
    fn extern_signatures_accept_the_abi_domain() {
        // Scalars in; a scalar, String or Unit out.
        let src = "extern fn jsLog(msg: String) \
                   extern fn jsNow() -> Float64 \
                   extern fn jsAdd(a: Int64, b: Int64) -> Int64 \
                   extern fn jsFlag(on: Bool, small: UInt8) -> Bool \
                   fn main() -> Int64 { return 0; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn extern_rejects_non_abi_types() {
        // A composite parameter cannot cross the JS boundary.
        let e = check_src(
            "extern fn bad(xs: Array<Int64>) \
             fn main() -> Int64 { return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("cannot cross the JS boundary"), "{e}");
        // Same for a composite return.
        let e = check_src(
            "extern fn bad() -> Option<Int64> \
             fn main() -> Int64 { return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("cannot cross the JS boundary"), "{e}");
    }

    /// An extern cannot take `consume`: the JS caller frees a String buffer
    /// when the call returns, whatever the declaration says.
    #[test]
    fn an_extern_string_parameter_may_not_be_consume() {
        let e = check_src(
            "let mut kept = \"x\" \
             export extern fn setIt(arg: consume String) { kept = arg } \
             fn main() -> Int64 { return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("may not be `consume`"), "{e}");
        assert!(e.contains("arg.copy()"), "{e}");
        // A scalar carries no ownership, so it is untouched.
        assert!(check_src(
            "extern fn jsAdd(a: consume Int64) -> Int64 fn main() -> Int64 { return 0; }"
        )
        .is_ok());
    }

    // The clock and the entropy seed are host effects; the seeded PRNG is
    // arithmetic and usable anywhere, generators included.

    #[test]
    fn host_clock_extern_is_rejected_in_a_generator() {
        // The reason is the lattice's `clock` row: `hostNowMillis` is no host
        // import, the runtime shim implements it.
        let e = check_src(
            "extern fn hostNowMillis() -> Int64 \
             fn now() -> Int64 { return hostNowMillis() } \
             gen fn g() -> String { let t = now() return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("not comptime-pure") && e.contains("which reads the clock"),
            "{e}"
        );
        assert!(!e.contains("the extern `hostNowMillis`"), "{e}");
    }

    #[test]
    fn pure_prng_is_comptime_usable() {
        // The seeded PRNG (MINSTD) is pure; only the host seed is an effect.
        let src = "gen fn g() -> String { \
                     let seed = 42 \
                     let next = (seed * 48271) % 2147483647 \
                     return \"\" \
                   } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn extern_with_body_is_a_parse_error() {
        let toks =
            lex("extern fn f() -> Int64 { return 1; } fn main() -> Int64 { return 0; }").unwrap();
        let e = parse(toks).unwrap_err();
        assert!(
            e.message.contains("an `extern fn` has no body"),
            "{}",
            e.message
        );
    }

    #[test]
    fn export_extern_without_a_body_is_a_parse_error() {
        // An exported extern needs a body; a body-less form is an import.
        let toks = lex("export extern fn f() -> Int64 fn main() -> Int64 { return 0 }").unwrap();
        let e = parse(toks).unwrap_err();
        assert!(
            e.message.contains("an exported extern needs a body"),
            "{}",
            e.message
        );
    }

    #[test]
    fn export_extern_with_body_checks_and_enforces_the_abi_domain() {
        let src = "export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b } \
                   export extern fn greet(name: String) -> String { return name } \
                   fn main() -> Int64 { return vyrnAdd(1, 2) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));

        // The signature obeys the import's ABI domain.
        let e = check_src(
            "export extern fn bad(xs: Array<Int64>) -> Int64 { return 0 } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("cannot cross the JS boundary"), "{e}");
    }

    #[test]
    fn export_extern_body_is_checked_like_any_fn() {
        let e = check_src(
            "export extern fn f(a: Int64) -> Int64 { return a + \"x\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            !e.is_empty(),
            "a type error in the body must be reported: {e}"
        );
    }

    #[test]
    fn accepts_modify_with_mut_argument() {
        let src = "type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                   fn main() -> Int64 { let mut c = C { x: 0 }; f(c); return c.x; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_modify_with_immutable_argument() {
        let e = check_src(
            "type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int64 { let c = C { x: 0 }; f(c); return c.x; }",
        )
        .unwrap_err();
        assert!(e.contains("must be declared `mut`"), "{e}");
    }

    #[test]
    fn rejects_modify_with_temporary_argument() {
        let e = check_src(
            "type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int64 { f(C { x: 0 }); return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("pass a mutable variable"), "{e}");
    }

    #[test]
    fn accepts_field_mutation() {
        let src = "type P = { x: Int64, y: Int64 }; \
                   fn main() -> Int64 { let mut p = P { x: 1, y: 2 }; \
                   p.x = 10; return p.x + p.y; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn accepts_array_operations() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[0] + a.length; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_push_wrong_element_type() {
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                           a.push(\"x\"); return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("the array holds"), "{e}");
    }

    #[test]
    fn accepts_index_store_pop_swapremove() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30]; \
                   a[1] = 25; let g = a.swapRemove(0); let p = a.pop(); \
                   return a.length + g; }";
        assert!(check_src(src).is_ok());
    }

    /// A fixed-size array stores in place; `refusals.rs` pins that it cannot
    /// shrink.
    #[test]
    fn arrayn_allows_store() {
        assert!(check_src(
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3]; a[0] = 9; return a[0]; }"
        )
        .is_ok());
    }

    #[test]
    fn index_store_validated_element_rejected_at_compile_time() {
        // A constant that violates the element predicate is refused.
        let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut a: Array<Age> = [Age(20)]; a[0] = 5; return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn pop_yields_option_swapremove_yields_element() {
        // `pop()` is `Option<T>` (must be unwrapped); `swapRemove` is `T`.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g: Int64 = a.swapRemove(0); \
                   let p: Int64 = match a.pop() { Some(x) => x, None => 0 }; \
                   return g + p; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn free_pop_is_not_callable() {
        // `pop`/`swapRemove` are method-only; a free `pop(a)` is not a builtin.
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = [1]; let p = pop(a); return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("pop"), "{e}");
    }

    #[test]
    fn accepts_region_with_nonheap_result() {
        // A heap temporary lives and dies inside the region; only an Int escapes.
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; let mut n = 0; \
                       region { let s = a + b; n = s.byteLength; } \
                       return n; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_heap_escaping_region_via_field() {
        // Storing an arena string into an outer record's field dangles too.
        let src = "type Holder = { s: String } \
                   fn main() -> Int64 { \
                       let mut h = Holder { s: \"init\" } \
                       region { h.s = \"a\" + \"b\" } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn rejects_heap_escaping_region_via_push() {
        // Pushing an arena string into an outer array outlives the region.
        let src = "fn main() -> Int64 { \
                       let mut a: Array<String> = [] \
                       region { a.push(\"x\" + \"y\") } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn allows_a_consume_of_a_value_that_owns_no_heap_inside_a_region() {
        // The rule is about arena memory, so an Int64 crosses freely.
        let src = "fn tally(n: consume Int64) -> Int64 { return n } \
                   fn main() -> Int64 { \
                       let mut c = 0 \
                       region { c = tally(7) } \
                       return c }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_nonheap_stores_out_of_region() {
        // An Int64 carries no arena memory, so it may go into an outer array.
        let src = "fn main() -> Int64 { \
                       let mut a: Array<Int64> = [] \
                       region { a.push(2) } \
                       return a[0] }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_region_local_heap_binding() {
        // A heap value assigned to a region-local `mut` dies with it.
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; \
                       region { let mut s = a; s = a + b; print(s); } \
                       return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn structural_record_into_predicated_named_is_auto_checked() {
        // A compatible record flows into a predicated record type, and the
        // boundary runs the invariant. A violating constant literal is refused,
        let bad = "type Range = { start: Int64, end: Int64 } where start < end \
                   fn span(r: Range) -> Int64 { return r.end - r.start } \
                   fn main() -> Int64 { \
                       return span(Range { start: 10, end: 3 }) }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("violates `where start < end`"), "{e}");
        // a constant plain record at the boundary is proven there too,
        let bad2 = "type Range = { start: Int64, end: Int64 } where start < end \
                    type Plain = { start: Int64, end: Int64 } \
                    fn span(r: Range) -> Int64 { return r.end - r.start } \
                    fn main() -> Int64 { \
                        return span(Plain { start: 10, end: 3 }) }";
        let e2 = check_src(bad2).unwrap_err();
        assert!(e2.contains("does not satisfy `Range`"), "{e2}");
        // and a dynamic one compiles, with a run-time check.
        let dynamic = "type Range = { start: Int64, end: Int64 } where start < end \
                       type Plain = { start: Int64, end: Int64 } \
                       fn span(r: Range) -> Int64 { return r.end - r.start } \
                       fn mk(a: Int64, b: Int64) -> Plain { return Plain { start: a, end: b } } \
                       fn main() -> Int64 { return span(mk(1, 5)) }";
        assert!(check_src(dynamic).is_ok(), "{:?}", check_src(dynamic));
    }

    #[test]
    fn accepts_predicated_named_record_itself() {
        let src = "type Range = { start: Int64, end: Int64 } where start < end \
                   fn span(r: Range) -> Int64 { return r.end - r.start } \
                   fn main() -> Int64 { \
                       let r = Range { start: 1, end: 5 } \
                       return span(r) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn match_arm_result_is_auto_validated_at_return() {
        // A raw-Int arm joins the match to Int64; returning it as `Age` is a
        // checked coercion at the return boundary.
        let src = "type Age = Int64 where value >= 18 \
                   fn pick(o: Option<Int64>) -> Age { \
                       return match o { Some(x) => x, None => 18 } } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
        // A provably-invalid CONSTANT return is rejected at compile time.
        let bad = "type Age = Int64 where value >= 18 \
                   fn five() -> Age { return 5 } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn rejects_modify_with_wider_record() {
        // The callee may reassign a `modify` param whole; written back through
        // a wider record, that would lose fields.
        let src = "type Named = { name: Int64 } \
                   type User = { name: Int64, age: Int64 } \
                   fn clobber(n: modify Named) { n = Named { name: 5 } } \
                   fn main() -> Int64 { \
                       let mut u = User { name: 1, age: 30 } \
                       clobber(u) \
                       return u.age }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("needs exactly"), "{e}");
    }

    #[test]
    fn generic_calls_enforce_modify_discipline() {
        // The generic-inference path must run the same capability checks.
        let src = "type C = { x: Int64 } \
                   fn f<T>(c: modify C, tag: T) -> Int64 { c.x = 99 return 0 } \
                   fn main() -> Int64 { \
                       let c = C { x: 1 } \
                       let r = f(c, 0) \
                       return c.x }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("must be declared `mut`"), "{e}");
    }

    /// Every atom the generation fence refuses is a name the compiler owns, so
    /// a deleted builtin cannot leave a stale row in the lattice's `gen`
    /// column.
    #[test]
    fn comptime_forbidden_names_are_reserved() {
        for (n, _) in crate::effects::ATOMS {
            if crate::effects::gen_allows(n) {
                continue;
            }
            // A name no source can spell needs no reservation: the runtime's
            // primitives and the `@` internals, log levels included.
            if n.contains('$') || n.starts_with('@') {
                continue;
            }
            assert!(
                RESERVED.contains(n) || crate::trap::host_boundary_extern(n).is_some(),
                "`{n}` is forbidden inside a `gen fn` but is not a name the \
                 compiler owns — it now forbids any user function spelled that way"
            );
        }
    }

    /// Inference may produce an `Option<Option<..>>`: `T` solves to
    /// `Option<Int64>` here. `examples/nestedsum.vyrn` runs the shape.
    #[test]
    fn accepts_nested_option_via_generic_inference() {
        let src = "fn wrap<T>(x: T) -> Option<T> { return Some(x) } \
                   fn main() -> Int64 { \
                       let o = wrap(Some(1)) \
                       return 0 }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn setfield_into_predicated_field_takes_a_constructed_value() {
        let ok = "type Age = Int64 where value >= 18 \
                  type User = { age: Age } \
                  fn main() -> Int64 { \
                      let mut u = User { age: 30 } \
                      u.age = Age(21) \
                      return 0 }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
    }

    #[test]
    fn constant_violations_are_compile_errors_at_every_boundary() {
        let cases = [
            // let annotation
            "type Age = Int64 where value >= 18 \
             fn main() -> Int64 { let a: Age = 5 return 0 }",
            // call argument
            "type Age = Int64 where value >= 18 \
             fn g(a: Age) -> Int64 { return 0 } \
             fn main() -> Int64 { return g(5) }",
            // assignment
            "type Age = Int64 where value >= 18 \
             fn main() -> Int64 { let mut a: Age = 20 a = 5 return 0 }",
            // record field
            "type Age = Int64 where value >= 18 \
             type User = { age: Age } \
             fn main() -> Int64 { let u = User { age: 5 } return 0 }",
            // array element
            "type Age = Int64 where value >= 18 \
             fn main() -> Int64 { let xs: Array<Age, 2> = [20, 5] return 0 }",
        ];
        for src in cases {
            let e = check_src(src).unwrap_err();
            assert!(
                e.contains("does not satisfy `Age`"),
                "case: {src}\ngot: {e}"
            );
        }
    }

    #[test]
    fn accepts_u64_range_literal_as_uint64_and_i64_min() {
        let src = "fn main() -> Int64 { \
                       let x: UInt64 = 18446744073709551615 \
                       let m = -9223372036854775808 \
                       if m < 0 { return 0 } return 1 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn accepts_in_range_sized_literals() {
        let src = "fn main() -> Int64 { \
                       let a: Int8 = 127 \
                       let b: UInt8 = 255 \
                       let c: Int32 = 2147483647 \
                       if a < 127 { return 1 } return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn accepts_valid_compile_time_construction() {
        let src = "type Age = Int64 where value >= 18; \
                   fn main() -> Int64 { let a = Age(25); return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn validated_decays_to_base_and_reverse_is_auto_checked() {
        // an Age is usable as an Int64...
        let ok = "type Age = Int64 where value >= 18; \
                  fn f(n: Int64) -> Int64 { return n; } \
                  fn main() -> Int64 { return f(Age(20)); }";
        assert!(check_src(ok).is_ok());
        // ...and a raw Int64 flows into an Age with an automatic check: a
        // valid constant is proven free, an invalid one is a compile error.
        let ok2 = "type Age = Int64 where value >= 18; \
                   fn g(a: Age) -> Int64 { return 0; } \
                   fn main() -> Int64 { return g(20); }";
        assert!(check_src(ok2).is_ok(), "{:?}", check_src(ok2));
        let bad = "type Age = Int64 where value >= 18; \
                   fn g(a: Age) -> Int64 { return 0; } \
                   fn main() -> Int64 { return g(5); }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn rejects_invalid_constant_return() {
        // A literal returned where a predicated type is expected is proven at
        // compile time, exactly like a let/argument boundary.
        let src = "type Age = Int64 where value >= 18 \
                   fn birth() -> Age { return 5 } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn accepts_folded_constant_in_range() {
        // A foldable expression that passes the predicate is proven and costs
        // nothing.
        let src = "type Age = Int64 where value >= 18 \
                   fn birth() -> Age { return 12 + 9 } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn rejects_predicate_with_call() {
        let src = "type Bad = Int64 where print(value) == value; \
                   fn main() -> Int64 { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("may not contain calls"), "{e}");
    }

    #[test]
    fn accepts_option_and_match() {
        let src = "fn f(b: Bool) -> Option<Int64> { if b { return Some(1); } return None; } \
                   fn main() -> Int64 { return match f(true) { Some(x) => x, None => 0 }; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_mismatched_match_arms() {
        let src = "fn main() -> Int64 { let o: Option<Int64> = Some(1); \
                   return match o { Some(x) => x, None => true }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("differing types"), "{e}");
    }

    #[test]
    fn accepts_result_and_question_mark() {
        let src = "fn f(n: Int64) -> Result<Int64, Int64> { if n == 0 { return Err(1); } return Ok(n); } \
                   fn g(n: Int64) -> Result<Int64, Int64> { let x = f(n)?; return Ok(x + 1); } \
                   fn main() -> Int64 { return match g(5) { Ok(v) => v, Err(e) => e }; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_question_mark_when_function_returns_scalar() {
        let src = "fn f() -> Result<Int64, Int64> { return Ok(1); } \
                   fn main() -> Int64 { let x = f()?; return x; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("requires the function to return Result"), "{e}");
    }

    #[test]
    fn rejects_uninferable_ok() {
        let e = check_src("fn main() -> Int64 { let x = Ok(1); return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the type of `Ok"), "{e}");
    }

    #[test]
    fn rejects_wrong_pattern_for_scrutinee() {
        let src = "fn main() -> Int64 { let o: Option<Int64> = Some(1); \
                   return match o { Ok(x) => x, None => 0 }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("is not a variant of"), "{e}");
    }

    #[test]
    fn accepts_generic_function() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn main() -> Int64 { print(id(\"hi\")); return id(5); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn generic_calls_generic() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn wrap<U>(x: U) -> U { return id(x); } \
                   fn main() -> Int64 { return wrap(7); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_operation_on_unbounded_type_param() {
        let src = "fn bad<T>(x: T) -> T { return x + x; } \
                   fn main() -> Int64 { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("needs a `Num` bound"), "{e}");
    }

    #[test]
    fn constrained_generic_operators() {
        let src = "fn max<T: Ord>(a: T, b: T) -> T { if a > b { return a; } return b; } \
                   fn main() -> Int64 { return max(3, 9); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_bound_violation_at_call() {
        let src = "fn max<T: Ord>(a: T, b: T) -> T { if a > b { return a; } return b; } \
                   fn main() -> Int64 { let x = max(true, false); return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Ord`"), "{e}");
    }

    #[test]
    fn rejects_inconsistent_type_param() {
        let src = "fn two<T>(a: T, b: T) -> Int64 { return 0; } \
                   fn main() -> Int64 { return two(1, \"s\"); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("both"), "{e}");
    }

    #[test]
    fn accepts_generic_record() {
        let src = "type Box<T> = { value: T }; \
                   fn open<T>(b: Box<T>) -> T { return b.value; } \
                   fn main() -> Int64 { let n = Box { value: 41 }; return open(n); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_generic_type_without_args() {
        let src = "type Box<T> = { value: T }; \
                   fn f(b: Box) -> Int64 { return 0; } \
                   fn main() -> Int64 { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("is generic"), "{e}");
    }

    #[test]
    fn rejects_wrong_type_arg_count() {
        let src = "type Pair<A, B> = { a: A, b: B }; \
                   fn f(p: Pair<Int64>) -> Int64 { return 0; } \
                   fn main() -> Int64 { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("type argument"), "{e}");
    }

    #[test]
    fn accepts_generic_enum() {
        let src = "type Opt<T> = | Wrap(T) | Empty; \
                   fn oe<T>(o: Opt<T>, d: T) -> T { return match o { Wrap(x) => x, Empty => d }; } \
                   fn main() -> Int64 { let a = Wrap(41); let b: Opt<Int64> = Empty; \
                                      return oe(a, 0) + oe(b, 1); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn a_string_alias_assigns_both_ways() {
        let ok = "type UserId = String; \
                  fn show(id: UserId) -> String { print(id); return id; } \
                  fn main() -> Int64 { let s: String = show(\"a\"); print(s); return 0; }";
        assert!(check_src(ok).is_ok());
    }

    #[test]
    fn accepts_strings() {
        let src = "fn main() -> Int64 { let s = \"hi\"; print(s); \
                   if s == \"hi\" { return 1; } return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn string_plus_is_concatenation() {
        // `+` on two Strings concatenates.
        let src = "fn main() -> Int64 { let x = \"a\" + \"bc\"; return x.byteLength; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn string_record_field() {
        let src = "type U = { name: String, age: Int64 }; \
                   fn nm(u: U) -> Int64 { print(u.name); return u.age; } \
                   fn main() -> Int64 { return nm(U { name: \"x\", age: 7 }); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn removed_builtins_emit_migration_hints() {
        let cases = [
            (
                "fn main() -> Int64 { let s = str(1); return 0; }",
                "`str(x)` was removed",
            ),
            (
                "fn main() -> Int64 { let s = concat(\"a\", \"b\"); return 0; }",
                "`concat(a, b)` was removed",
            ),
            (
                "fn main() -> Int64 { let s = \"a\"; return len(s); }",
                "`len(s)` was removed",
            ),
            (
                "fn main() -> Int64 { let a: Array<Int64> = list([1, 2]); return 0; }",
                "`list([..])` was removed",
            ),
            (
                "fn main() -> Int64 { let s = toString(1); return 0; }",
                "`toString` is a method",
            ),
            // The collection verbs are reserved, so the hint answers.
            (
                "fn main() -> Int64 { let mut xs: Array<Int64> = [] xs = push(xs, 1) return 0 }",
                "`push(xs, v)` was removed",
            ),
            (
                "fn main() -> Int64 { let xs: Array<Int64> = [1] return at(xs, 0) }",
                "`at(xs, i)` was removed",
            ),
            (
                "fn main() -> Int64 { let xs: Array<Int64> = [1] return alen(xs) }",
                "`alen(xs)` was removed",
            ),
            (
                "fn main() -> Int64 { let xs: Array<Int64> = array() return 0 }",
                "`array()` was removed",
            ),
        ];
        for (src, want) in cases {
            let e = check_src(src).unwrap_err();
            assert!(e.contains(want), "for `{src}` expected `{want}`, got `{e}`");
        }
    }

    #[test]
    fn to_string_renders_scalar_receivers() {
        // `x.toString()` on Int64, a sized int, Float64, Bool, and String.
        let src = "fn main() -> Int64 { \
                       let a = (42).toString(); let b: Int8 = 3; let c = b.toString(); \
                       let d = (1.5).toString(); let e = true.toString(); let f = \"hi\".toString(); \
                       return a.byteLength + c.byteLength + d.byteLength + e.byteLength + f.byteLength; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn contextual_array_literal_in_let_param_return() {
        // A literal in an `Array<T>` position becomes a growable heap array:
        // in a `let` annotation, as a call argument, and as a return value.
        let src = "fn take(a: Array<Int64>) -> Int64 { return a.length } \
                   fn make() -> Array<String> { return [\"a\", \"b\"] } \
                   fn main() -> Int64 { \
                       let xs: Array<Int64> = [1, 2, 3] \
                       let n = take([4, 5]) \
                       return xs.length + n + make().length }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn accepts_enum_and_match() {
        let src = "type Shape = | Circle(Int64) | Empty; \
                   fn area(s: Shape) -> Int64 { return match s { Circle(r) => r * r, Empty => 0 }; } \
                   fn main() -> Int64 { return area(Circle(3)) + area(Empty); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_unknown_variant_pattern() {
        let src = "type E = | A | B; \
                   fn f(e: E) -> Int64 { return match e { A => 1, B => 2, C => 3 }; } \
                   fn main() -> Int64 { return f(A); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("not a variant"), "{e}");
    }

    #[test]
    fn rejects_payload_variant_without_binding() {
        let src = "type E = | Val(Int64) | Empty; \
                   fn f(e: E) -> Int64 { return match e { Val => 1, Empty => 0 }; } \
                   fn main() -> Int64 { return f(Empty); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("payload") && e.contains("binds"), "{e}");
    }

    #[test]
    fn multi_payload_variant() {
        let src = "type Shape = | Rect(Int64, Int64) | Empty; \
                   fn area(s: Shape) -> Int64 { return match s { Rect(w, h) => w * h, Empty => 0 }; } \
                   fn main() -> Int64 { return area(Rect(3, 4)); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn merge_combines_fields() {
        let src = "type A = { x: Int64 }; type B = { y: Int64 }; type C = Merge<A, B>; \
                   fn main() -> Int64 { let c = C { x: 1, y: 2 }; return c.x + c.y; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn partial_wraps_fields_in_option() {
        let src = "type U = { a: Int64 }; type P = Partial<U>; \
                   fn f(p: P) -> Int64 { return match p.a { Some(n) => n, None => 0 }; } \
                   fn main() -> Int64 { return f(P { a: Some(5) }); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_unknown_transformer_key() {
        let src = "type U = { a: Int64 }; type B = Omit<U, zzz>; fn main() -> Int64 { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("not in the transformer"), "{e}");
    }

    #[test]
    fn intersection_type_merges_fields() {
        let src = "type User = { name: Int64, age: Int64 }; \
                   type Employee = User & { salary: Int64 }; \
                   fn total(e: Employee) -> Int64 { return e.age + e.salary; } \
                   fn main() -> Int64 { let e = Employee { name: 1, age: 30, salary: 100 }; \
                                      return total(e); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn fallible_construction_returns_option() {
        let src = "type Age = Int64 where value >= 18; \
                   fn f(n: Int64) -> Int64 { return match Age?(n) { Some(a) => a, None => 0 }; } \
                   fn main() -> Int64 { return f(20); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn range_style_predicate() {
        let src = "type Port = Int64 where value >= 1 && value <= 65535; \
                   fn main() -> Int64 { let p = Port(8080); return 0; }";
        assert!(check_src(src).is_ok());
        let bad = "type Port = Int64 where value >= 1 && value <= 65535; \
                   fn main() -> Int64 { let p = Port(70000); return 0; }";
        assert!(
            check_src(bad).unwrap_err().contains("does not satisfy"),
            "port"
        );
    }

    #[test]
    fn string_length_refinement() {
        // A `String where value.byteLength ..` type-checks and const-validates.
        let ok = "type Name = String where value.byteLength >= 3; \
                  fn main() -> Int64 { let n = Name(\"bob\"); return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-too-short constant is rejected at compile time.
        let bad = "type Name = String where value.byteLength >= 3; \
                   fn main() -> Int64 { let n = Name(\"ab\"); return 0; }";
        assert!(
            check_src(bad)
                .unwrap_err()
                .contains("does not satisfy `Name`"),
            "short"
        );
    }

    #[test]
    fn string_length_is_int() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s.byteLength; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn an_array_keeps_length() {
        assert!(
            check_src("fn main() -> Int64 { let a: Array<Int64> = [1, 2] return a.length }")
                .is_ok()
        );
    }

    #[test]
    fn char_count_is_int() {
        assert!(check_src("fn main() -> Int64 { return \"héllo\".charCount() }").is_ok());
        // A user function may still be named `charCount` (the builtin is a method).
        let src = "fn charCount(s: String) -> Int64 { return 0 } \
                   fn main() -> Int64 { return charCount(\"x\") }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn byte_literal_defaults_to_uint8_and_coerces() {
        // Default (unconstrained) type is `UInt8`.
        assert!(check_src("fn main() -> Int64 { let b = 'a' return 0 }").is_ok());
        // It coerces to an annotated integer type exactly like `97` does.
        assert!(check_src("fn main() -> Int64 { let x: Int64 = 'a' return x }").is_ok());
        // It compares against a byte from `bytes(..)` (both UInt8).
        assert!(check_src(
            "fn main() -> Int64 { if bytes(\"{\")[0] == '{' { return 1 } return 0 }"
        )
        .is_ok());
    }

    #[test]
    fn cross_field_record_predicate() {
        let ok = "type R = { a: Int64, b: Int64 } where a < b; \
                  fn main() -> Int64 { let r = R { a: 1, b: 2 }; return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-violating constant literal is rejected at compile time.
        let bad = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn main() -> Int64 { let r = R { a: 5, b: 1 }; return 0; }";
        assert!(
            check_src(bad).unwrap_err().contains("violates"),
            "cross-field"
        );
    }

    #[test]
    fn regex_operator_requires_literal_pattern() {
        let ok = "fn f(s: String) -> Bool { return s =~ \"[a-z]+\"; } \
                  fn main() -> Int64 { return 0; }";
        assert!(check_src(ok).is_ok());
    }

    #[test]
    fn global_inferred_and_annotated_types_check() {
        let ok = "let mut hits = 0\n\
                  let banner: String = \"hi\"\n\
                  fn bump() -> Int64 { hits = hits + 1 return hits }\n\
                  fn name() -> String { return banner }\n\
                  fn main() -> Int64 { return bump() }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
    }

    #[test]
    fn validated_global_rejects_provably_invalid_constant() {
        let e = check_src(
            "type Age = Int64 where value >= 0\n\
             let mut a: Age = -1\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("does not satisfy") || e.contains("Age"), "{e}");
    }

    #[test]
    fn initializer_may_not_call_user_function() {
        let e = check_src(
            "fn seed() -> Int64 { return 7 }\n\
             let x = seed()\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("may not call"), "{e}");
    }

    #[test]
    fn initializer_may_not_read_a_later_global() {
        let e = check_src(
            "let a = b\n\
             let b = 1\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("declared later"), "{e}");
    }

    #[test]
    fn initializer_may_not_read_itself() {
        let e = check_src(
            "let a = a\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("read itself"), "{e}");
    }

    #[test]
    fn index_field_assign_accepts_plain_record_element() {
        assert!(check_src(
            "type P = { x: Int64, y: Int64 }\n\
             fn main() -> Int64 { let mut a: Array<P> = [P { x: 1, y: 2 }]  a[0].x = 9  return 0 }",
        )
        .is_ok());
    }

    /// A common prelude: a finite key type, a finite section type, and a `t`
    /// consumer.
    const KEYS: &str = "type TransKey = String where value =~ \"(home\\\\.(title|subtitle)|nav\\\\.(home|about|settings)\\\\.label)\"\n\
         type Section = String where value =~ \"home|about|settings\"\n\
         fn t(key: TransKey) -> Int64 { return 0 }\n";

    #[test]
    fn interpolation_witness_is_a_compile_error() {
        // A Section that includes `profile` produces `nav.profile.label`, which
        // TransKey does not contain; the witness names it exactly.
        let src = "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
             type Section = String where value =~ \"home|about|profile\"\n\
             fn t(key: TransKey) -> Int64 { return 0 }\n\
             fn main() -> Int64 { let s: Section = \"home\"  return t(\"nav.\\{s}.label\") }";
        let e = check_src(src).unwrap_err();
        assert!(
            e.contains(
                "\"nav.profile.label\" (a possible value of this interpolation) does not satisfy `TransKey`"
            ),
            "{e}"
        );
    }

    #[test]
    fn interpolation_with_nonfinite_hole_is_left_to_runtime() {
        // A plain-String hole is not finite, so no containment and no error;
        // the boundary keeps its run-time validation.
        let src = format!(
            "{KEYS}fn build(x: String) -> Int64 {{ return t(\"nav.\\{{x}}.label\") }}\n\
             fn main() -> Int64 {{ return build(\"home\") }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn finite_var_contained_is_accepted_and_uncontained_is_left_to_runtime() {
        // Section is no subset of TransKey, but a Section variable may hold a
        // conforming value, so the flow compiles with its run-time check.
        let runtime =
            format!("{KEYS}fn main() -> Int64 {{ let s: Section = \"home\"  return t(s) }}");
        assert!(check_src(&runtime).is_ok(), "{:?}", check_src(&runtime));

        // A finite type that IS contained in the target flows with no error.
        let proven = "type Wide = String where value =~ \"a|b|c\"\n\
             type Narrow = String where value =~ \"a|b\"\n\
             fn want(x: Wide) -> Int64 { return 0 }\n\
             fn main() -> Int64 { let n: Narrow = \"a\"  return want(n) }";
        assert!(check_src(proven).is_ok(), "{:?}", check_src(proven));
    }

    #[test]
    fn mixed_predicate_target_falls_back_to_runtime() {
        // A length clause beside a regex clause is no pure regex language, so
        // containment does not apply and the length is checked at run time.
        let src = "type Key = String where value =~ \"[a-z]+\" && value.byteLength < 3\n\
             type Section = String where value =~ \"home|about\"\n\
             fn t(k: Key) -> Int64 { return 0 }\n\
             fn main() -> Int64 { let s: Section = \"home\"  return t(\"\\{s}\") }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn proven_interpolation_at_return_and_let_boundaries() {
        // Return boundary.
        let ret = "type Section = String where value =~ \"home|about\"\n\
             type Key = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
             fn build(s: Section) -> Key { return \"nav.\\{s}.label\" }\n\
             fn main() -> Int64 { let s: Section = \"home\"  build(s)  return 0 }";
        assert!(check_src(ret).is_ok(), "{:?}", check_src(ret));

        // Let-annotation boundary.
        let letb = "type Section = String where value =~ \"home|about\"\n\
             type Key = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
             fn main() -> Int64 { let s: Section = \"home\"  let k: Key = \"nav.\\{s}.label\"  return 0 }";
        assert!(check_src(letb).is_ok(), "{:?}", check_src(letb));
    }

    const TWICE: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n";

    #[test]
    fn accepts_lambda_and_named_fn_argument() {
        let src = format!(
            "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{ let a = twice([1, 2], x -> x * 2)  let b = twice([1, 2], dbl)  return 0 }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    /// A `fn`-typed argument may be any expression of `fn` type; one of the
    /// wrong type is refused with the type it found.
    #[test]
    fn a_fn_argument_may_be_read_from_where_it_is_stored() {
        let field = format!(
            "{TWICE}type R = {{ f: fn(Int64) -> Int64 }}\n\
             fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{ let r = R {{ f: dbl }}  let a = twice([1], r.f)  return 0 }}"
        );
        assert!(check_src(&field).is_ok(), "{:?}", check_src(&field));

        let elem = format!(
            "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{\n\
             let mut xs: Array<fn(Int64) -> Int64> = []\n\
             xs.push(dbl)\n\
             let a = twice([1], xs[0])\n\
             return 0 }}"
        );
        assert!(check_src(&elem).is_ok(), "{:?}", check_src(&elem));

        let call = format!(
            "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn pick() -> fn(Int64) -> Int64 {{ return dbl }}\n\
             fn main() -> Int64 {{ let a = twice([1], pick())  return 0 }}"
        );
        assert!(check_src(&call).is_ok(), "{:?}", check_src(&call));
    }

    #[test]
    fn fn_types_are_storable() {
        // `fn(..) -> ..` is legal in returns, `let` annotations, record
        // fields, array elements, Option payloads and module state.
        let ret = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn pick() -> fn(Int64) -> Int64 { return dbl }\n\
             fn main() -> Int64 { let f = pick()  return f(21) }";
        assert!(check_src(ret).is_ok(), "{:?}", check_src(ret));
        let letb = "fn main() -> Int64 { let g: fn(Int64) -> Int64 = x -> x * 2  return g(3) }";
        assert!(check_src(letb).is_ok(), "{:?}", check_src(letb));
        let rec = "type R = { f: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 { let r = R { f: x -> x + 1 }  let g = r.f  return g(1) }";
        assert!(check_src(rec).is_ok(), "{:?}", check_src(rec));
        let arr = "fn main() -> Int64 {\n\
             let mut xs: Array<fn(Int64) -> Int64> = []\n\
             xs.push(x -> x * 2)\n\
             let f = xs[0]\n\
             return f(4) }";
        assert!(check_src(arr).is_ok(), "{:?}", check_src(arr));
        let opt = "fn main() -> Int64 {\n\
             let o: Option<fn(Int64) -> Int64> = Some(x -> x - 1)\n\
             return match o { Some(f) => f(1), None => 0 } }";
        assert!(check_src(opt).is_ok(), "{:?}", check_src(opt));
        let state = "type Middleware = fn(Int64) -> Int64\n\
             let mut chain: Array<Middleware> = []\n\
             fn add() { chain.push(x -> x + 1) }\n\
             fn main() -> Int64 { add()  let m = chain[0]  return m(1) }";
        assert!(check_src(state).is_ok(), "{:?}", check_src(state));
        // Composition: value-to-value flow creates no new source and stays legal.
        let compose = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let g = dbl  let h = g  return h(5) }";
        assert!(check_src(compose).is_ok(), "{:?}", check_src(compose));
    }

    #[test]
    fn fn_types_still_rejected_where_illegal() {
        // No higher-order-of-higher-order: a stored fn type may not take or
        // return another function value.
        let hof =
            "fn main() -> Int64 { let g: fn(fn(Int64) -> Int64) -> Int64 = x -> 0  return 0 }";
        assert!(
            check_src(hof)
                .unwrap_err()
                .contains("may not take another function value"),
            "{:?}",
            check_src(hof)
        );
        // A function type has no `where` domain.
        let pred = "type F = fn(Int64) -> Int64 where value > 0\n fn main() -> Int64 { return 0 }";
        assert!(
            check_src(pred)
                .unwrap_err()
                .contains("cannot carry a `where` predicate"),
            "{:?}",
            check_src(pred)
        );
        // Function values have no `==`.
        let eq = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let a = dbl  let b = dbl  if a == b { return 1 }  return 0 }";
        assert!(
            check_src(eq).unwrap_err().contains("`==`/`!=`"),
            "{:?}",
            check_src(eq)
        );
        // `toJson` rejects fn-typed data with the type named (functions don't
        // go on the wire).
        let wire = "type H = { f: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 { let h = H { f: x -> x }  let s = toJson(h)  return 0 }";
        assert!(
            check_src(wire).unwrap_err().contains("cannot encode"),
            "{:?}",
            check_src(wire)
        );
    }

    #[test]
    fn cannot_assign_to_captured_binding() {
        let src = format!(
            "{TWICE}fn main() -> Int64 {{ let mut c = 0  let a = twice([1], x -> {{ c = c + x  return c }})  return 0 }}"
        );
        assert!(
            check_src(&src)
                .unwrap_err()
                .contains("cannot assign to the captured"),
            "{:?}",
            check_src(&src)
        );
    }

    #[test]
    fn cannot_drop_captured_binding() {
        let src = "fn apply(s: String, f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n\
             fn main() -> Int64 { let name = \"hi\"  let r = apply(name, x -> { drop name  return x })  return 0 }";
        assert!(
            check_src(src)
                .unwrap_err()
                .contains("cannot `drop` the captured"),
            "{:?}",
            check_src(src)
        );
    }

    #[test]
    fn cannot_consume_captured_binding() {
        let src = "fn take(s: consume String) -> Int64 { return 1 }\n\
             fn apply(s: String, f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n\
             fn main() -> Int64 { let name = \"hi\"  let r = apply(name, x -> { let z = take(name)  return x })  return 0 }";
        assert!(
            check_src(src)
                .unwrap_err()
                .contains("cannot consume the captured"),
            "{:?}",
            check_src(src)
        );
    }

    #[test]
    fn nested_lambda_literal_is_rejected() {
        let src = format!(
            "{TWICE}fn main() -> Int64 {{ let a = twice([1], x -> twice([x], y -> y + 1).length)  return 0 }}"
        );
        assert!(
            check_src(&src)
                .unwrap_err()
                .contains("another lambda literal"),
            "{:?}",
            check_src(&src)
        );
    }

    #[test]
    fn passthrough_fn_param_is_accepted() {
        let src = format!(
            "{TWICE}fn outer(xs: Array<Int64>, g: fn(Int64) -> Int64) -> Array<Int64> {{ return twice(xs, g) }}\n\
             fn main() -> Int64 {{ let a = outer([1, 2], x -> x + 1)  return 0 }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn generic_map_infers_return_type() {
        let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []\n\
             for x in xs { out.push(f(x)) }\n\
             return out }\n\
             fn main() -> Int64 { let ys: Array<Int64> = [1, 2]  let zs = map(ys, x -> x > 0)  return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn extern_cannot_take_fn_param() {
        let src = "extern fn e(f: fn(Int64) -> Int64) -> Int64\nfn main() -> Int64 { return 0 }";
        assert!(
            check_src(src).unwrap_err().contains("extern"),
            "{:?}",
            check_src(src)
        );
    }

    #[test]
    fn map_surface_typechecks() {
        // Insert, honest Option lookup, has/remove/length/keys over the surface.
        let src = "fn main() -> Int64 {\n\
             let mut m: Map<String, Int64> = [:]\n\
             m[\"a\"] = 1\n\
             let hit = match m[\"a\"] { Some(v) => v, None => 0 }\n\
             let present = m.has(\"a\")\n\
             let gone = m.remove(\"a\")\n\
             let n = m.length\n\
             for k in m.keys() { print(k) }\n\
             return hit + n }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// A `Hashable` protocol and a heapless record.
    const HASHKEY: &str = "protocol Hashable {\n\
                               fn hash(self) -> UInt64\n\
                           }\n\
                           type Point = { x: Int64, y: Int64 }\n\
                           impl Hashable for Point {\n\
                               fn hash(self) -> UInt64 {\n\
                                   return UInt64(self.x)\n\
                               }\n\
                           }\n";

    #[test]
    fn a_hashable_record_keys_a_map() {
        // A heapless record with `impl Hashable` is a key, every operation
        // included.
        let src = format!(
            "{HASHKEY}\
             fn main() -> Int64 {{\n\
                 let mut m: Map<Point, Int64> = [:]\n\
                 m[Point {{ x: 1, y: 2 }}] = 1\n\
                 m.tally(Point {{ x: 1, y: 2 }}, 2)\n\
                 let hit = match m[Point {{ x: 1, y: 2 }}] {{ Some(v) => v, None => 0 }}\n\
                 let present = m.has(Point {{ x: 1, y: 2 }})\n\
                 let gone = m.remove(Point {{ x: 1, y: 2 }})\n\
                 for k in m.keys() {{ print(\"\\{{k.x}}\") }}\n\
                 return hit + m.length }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn map_allows_validated_string_key() {
        // A validated string type resolves to `String`, so it is a legal key.
        let src = "type Name = String where value.byteLength >= 1\n\
                   fn f(m: Map<Name, Int64>) -> Int64 { return 0 }\n\
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn map_lookup_is_option() {
        // `m[k]` is `Option<V>`, so reading `V` takes a match.
        let src = "fn main() -> Int64 {\n\
             let mut m: Map<String, Int64> = [:]\n\
             m[\"a\"] = 1\n\
             let v: Option<Int64> = m[\"a\"]\n\
             return v ?? 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn map_has_no_equality() {
        let src = "fn main() -> Int64 {\n\
             let a: Map<String, Int64> = [:]\n\
             let b: Map<String, Int64> = [:]\n\
             if a == b { return 1 }\n\
             return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("scalar operands"), "{e}");
    }

    #[test]
    fn map_alias_is_codable_by_name() {
        let src = "type M = Map<String, Int64>\n\
             fn main() -> Int64 {\n\
             let v = fromJson<M>(\"{}\")\n\
             print(jsonSchema<M>())\n\
             return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn if_expression_typechecks_in_let_init() {
        let src = "fn main() -> Int64 {\n\
             let x = if true { 1 } else { 2 }\n\
             return x }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn if_expression_needs_an_else() {
        // A missing `else` in expression position names the totality rule;
        // the statement form allows it.
        let src = "fn main() -> Int64 {\n\
             let x = if true { 1 }\n\
             return x }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("needs an `else`"), "{e}");
    }

    #[test]
    fn if_expression_branches_must_unify() {
        let src = "fn main() -> Int64 {\n\
             let x = if true { 1 } else { \"no\" }\n\
             return x }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("differing types"), "{e}");
    }

    #[test]
    fn if_expression_unifies_with_a_validated_type() {
        // Raw-Int branches coerce into the validated `Age` at the `let` boundary.
        let src = "type Age = Int64 where value >= 0\n\
             fn main() -> Int64 {\n\
             let a: Age = if true { 5 } else { 10 }\n\
             return a }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn if_expression_chain_and_nesting_typecheck() {
        let src = "fn tier(s: Int64) -> String {\n\
             return if s >= 90 { \"gold\" } else if s >= 50 { \"silver\" } else { \"bronze\" } }\n\
             fn main() -> Int64 {\n\
             let n = if true { if false { 1 } else { 2 } } else { 3 }\n\
             print(tier(n))\n\
             return n }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn statement_if_still_allows_a_missing_else() {
        // The statement form: no `else`, statements inside, no value.
        let src = "fn main() -> Int64 {\n\
             let mut x = 0\n\
             if true { x = 1 }\n\
             return x }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn smallarray_basic_surface_type_checks() {
        let src = "fn main() -> Int64 {\n\
             let mut xs: SmallArray<Int64, 4> = []\n\
             xs.push(1)\n\
             xs.push(2)\n\
             xs[0] = 9\n\
             let a = xs.toArray()\n\
             let p = match xs.pop() { Some(v) => v, None => 0 }\n\
             let r = xs.swapRemove(0)\n\
             drop a\n\
             drop xs\n\
             return xs.length + p + r }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn integer_argument_on_a_non_smallarray_is_rejected() {
        // Only `SmallArray` takes an integer type argument.
        let src = "type Box = { v: Int64 }\n\
             fn main() -> Int64 { let b: Box<3> = Box { v: 1 }  return b.v }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not take an integer argument"), "{e}");
    }

    #[test]
    fn smallarray_capacity_below_one_is_rejected() {
        let src = "fn main() -> Int64 { let xs: SmallArray<Int64, 0> = []  return xs.length }";
        let e = check_src(src).unwrap_err();
        assert!(
            e.contains("smallArray capacity must be between 1 and 64"),
            "{e}"
        );
    }

    #[test]
    fn smallarray_capacity_above_64_is_rejected() {
        let src = "fn main() -> Int64 { let xs: SmallArray<Int64, 65> = []  return xs.length }";
        let e = check_src(src).unwrap_err();
        assert!(
            e.contains("smallArray capacity must be between 1 and 64"),
            "{e}"
        );
    }

    #[test]
    fn smallarray_at_a_contract_boundary_is_a_named_error() {
        // `SmallArray` has no JSON codec, so a contract type refuses it by name.
        let src = "fn main() -> Int64 {\n\
             let xs: SmallArray<Int64, 4> = [1, 2]\n\
             let s = toJson(xs)\n\
             return xs.length }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("SmallArray") && e.contains("codable"), "{e}");
    }

    #[test]
    fn coexisting_smallarray_monomorphizations_type_check() {
        // Two different `N`s coexist as distinct types.
        let src = "fn main() -> Int64 {\n\
             let mut a: SmallArray<Int64, 4> = []\n\
             let mut b: SmallArray<Int64, 8> = []\n\
             a.push(1)\n\
             b.push(2)\n\
             let n = a.length + b.length\n\
             drop a\n\
             drop b\n\
             return n }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn nested_smallarray_type_checks() {
        let src = "fn main() -> Int64 {\n\
             let row: SmallArray<Int64, 2> = [1, 2]\n\
             let mut grid: SmallArray<SmallArray<Int64, 2>, 2> = []\n\
             grid.push(row)\n\
             let got = grid[0]\n\
             let n = got[1]\n\
             drop grid\n\
             return n }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn contract_members_may_name_declared_types_and_open_type_params() {
        let src = "type Head = { title: String }\n\
                   contract Page {\n\
                   let head: Head = Head { title: \"\" }\n\
                   let data: Array<T>\n\
                   fn load(id: Int64) -> String\n\
                   fn *(input: String) -> String\n\
                   }\n\
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn a_misspelled_member_type_is_an_error_not_a_type_parameter() {
        // Only a single letter is a type parameter, so the typo `Haed` must
        // resolve and is caught.
        let src = "type Head = { title: String }\n\
                   contract Page { let head: Haed }\n\
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("unknown type `Haed`"), "{e}");
    }

    #[test]
    fn a_default_must_have_the_members_declared_type() {
        let src = "type Head = { title: String }\n\
                   contract Page { let head: Head = \"Title\" }\n\
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("head"), "{e}");
    }

    #[test]
    fn contract_of_types_as_contract_info() {
        let src = "contract Page { let head: String = \"\" }\n\
                   gen fn g(p: String) -> String { let c = contractOf(Page)  return c.name }\n\
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn contract_of_rejects_a_name_that_is_not_a_contract() {
        let src = "type Page = { a: Int64 }\n\
                   gen fn g(p: String) -> String { let c = contractOf(Page)  return \"\" }\n\
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("is not a contract"), "{e}");
    }

    #[test]
    fn a_module_that_only_exports_a_contract_is_a_library() {
        // An exported contract is a public surface, so no `main` is needed.
        let src = "export contract Page { let head: String = \"\" }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// The row states the buffer type; `refusals.rs` pins the refusals, and
    /// this pins the accepted shape.
    #[test]
    fn line_at_and_col_at_demand_a_byte_buffer() {
        // The shape every real caller passes.
        assert!(check_src(
            "fn main() -> Int64 { let b = bytes(\"a\\nb\")  print(lineAt(b, 2))  \
                 print(colAt(b, 2))  return 0 }"
        )
        .is_ok());
    }

    /// A lane index is checked at compile time or not at all: a backend emits
    /// `f32x4.extract_lane` with no guard.
    #[test]
    fn a_lane_index_must_be_a_constant_in_range() {
        let ok = "fn main() -> Int64 { let v = F32x4(1.0, 2.0, 3.0, 4.0)  \
                  print(v.lane(0))  print(v.lane(3))  return 0 }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
        for bad in [
            "print(v.lane(4))",
            "print(v.lane(0 - 1))",
            "let i = 0  print(v.lane(i))",
        ] {
            let src = format!("fn main() -> Int64 {{ let v = F32x4.splat(1.0)  {bad}  return 0 }}");
            let e = check_src(&src).unwrap_err();
            assert!(e.contains("compile-time constant in 0..3"), "{bad}: {e}");
        }
    }

    /// A vector takes `Float32` lanes and combines only with another vector.
    /// `Float64` is refused rather than truncated, like every other numeric flow.
    #[test]
    fn a_vector_does_not_mix_with_a_scalar() {
        let f64lane = check_src(
            "fn main() -> Int64 { let d = 1.0  let v = F32x4(d, d, d, d)  \
             print(v.lane(0))  return 0 }",
        );
        // A float LITERAL adapts to the lane type; a `Float64` binding does not.
        assert!(f64lane.is_err(), "{f64lane:?}");
        let mixed = check_src(
            "fn main() -> Int64 { let v = F32x4.splat(1.0)  let w = v + 1.0  \
             print(w.lane(0))  return 0 }",
        )
        .unwrap_err();
        assert!(mixed.contains("F32x4"), "{mixed}");
    }

    /// An associated type resolves at the impl, so a concrete receiver types
    /// and a `<T: P>` receiver is refused by name.
    #[test]
    fn an_associated_type_resolves_at_the_impl_and_not_through_a_bound() {
        let proto = "protocol Unwrap { type Output  fn valueOr(self, f: Output) -> Output }\n\
             impl Unwrap for Int64 { type Output = Int64\n\
               fn valueOr(self, f: Output) -> Output { return self } }\n";
        // A concrete receiver selects the impl, so `Output` is `Int64`.
        let ok = check_src(&format!(
            "{proto}fn main() -> Int64 {{ return 7.valueOr(0) + 1 }}"
        ));
        assert!(ok.is_ok(), "{ok:?}");
        // Through a bound: refused, naming the protocol and the member.
        let e = check_src(&format!(
            "{proto}fn pick<T: Unwrap>(x: T) -> Int64 {{ return 0 }}\n\
             fn viaBound<T: Unwrap>(x: T, f: Int64) -> Int64 {{ return x.valueOr(f) }}\n\
             fn main() -> Int64 {{ return viaBound(7, 0) + pick(1) }}"
        ))
        .unwrap_err();
        assert!(
            e.contains("associated type `Output`") && e.contains("cannot name it"),
            "{e}"
        );
    }

    /// `Self` is no type in Vyrn and is refused on the protocol's line, in
    /// each of three positions, not at every impl.
    #[test]
    fn a_protocol_may_not_name_self() {
        let want = "`Self` is not a type in Vyrn";
        let each = [
            ("a return type", "protocol Grow { fn grow(self) -> Self }"),
            (
                "a parameter",
                "protocol Merge { fn merge(self, other: Self) -> Int64 }",
            ),
            (
                "a type argument",
                "protocol All { fn all(self) -> Array<Self> }",
            ),
        ];
        for (what, proto) in each {
            // The declaration is the error, so it fires with no impl.
            let bare =
                check_src(&format!("{proto}\nfn main() -> Int64 {{ return 0 }}")).unwrap_err();
            assert!(bare.contains(want), "{what}, alone: {bare}");
            // The way out is named rather than implied.
            assert!(
                bare.contains("associated type"),
                "{what}: no way out named: {bare}"
            );
        }
        // With an impl: one error, on the protocol's line.
        let with_impl = check_src(
            "protocol Grow { fn grow(self) -> Self }\n\
             type Ring = { n: Int64 }\n\
             impl Grow for Ring { fn grow(self) -> Ring { return Ring { n: self.n } } }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(with_impl.contains(want), "{with_impl}");
        assert!(
            !with_impl.contains("this provides"),
            "the impl is still blamed: {with_impl}"
        );
    }

    /// A protocol's method signatures name types that exist, checked at the
    /// protocol, not through its impls.
    #[test]
    fn a_protocol_signature_names_types_that_exist() {
        let e =
            check_src("protocol Grow { fn grow(self) -> Blah }\nfn main() -> Int64 { return 0 }")
                .unwrap_err();
        assert!(e.contains("unknown type `Blah`"), "{e}");
        // An associated type reaches the walk as a type parameter.
        check_src(
            "protocol Unwrap { type Output  fn get(self) -> Output }\n\
             impl Unwrap for Int64 { type Output = Int64  fn get(self) -> Output { return self } }\n\
             fn main() -> Int64 { return 7.get() }",
        )
        .expect("an associated type is not an unknown type");
    }

    /// The receiver's capability is part of the signature: inside
    /// `fn f<T: Bump>(x: T)` no impl is visible, so an impl's `modify self`
    /// against the protocol's `self` would mutate unseen.
    #[test]
    fn an_impl_matches_the_receiver_capability_its_protocol_declared() {
        let src = |proto: &str, imp: &str| {
            format!(
                "type C = {{ n: Int64 }}\n\
                 protocol Bump {{ fn bump({proto} self) -> Unit }}\n\
                 impl Bump for C {{ fn bump({imp} self) -> Unit {{ self.n = 1 }} }}\n\
                 fn main() -> Int64 {{ let mut c = C {{ n: 0 }} c.bump() return c.n }}"
            )
        };
        assert!(check_src(&src("modify", "modify")).is_ok());
        let e = check_src(&src("", "modify")).unwrap_err();
        assert!(
            e.contains("it declares `fn bump(self) -> Unit`")
                && e.contains("provides `fn bump(modify self) -> Unit`"),
            "{e}"
        );
        // A `modify` receiver demands a `mut` binding, as a `modify` parameter
        // does.
        let imm = check_src(
            "type C = { n: Int64 }\n\
             protocol Bump { fn bump(modify self) -> Unit }\n\
             impl Bump for C { fn bump(modify self) -> Unit { self.n = 1 } }\n\
             fn main() -> Int64 { let c = C { n: 0 } c.bump() return c.n }",
        )
        .unwrap_err();
        assert!(imm.contains("declared `mut`"), "{imm}");
    }

    /// The rule admits an impl head's type variables and an associated type.
    /// The second is unprovable here: after parsing, `Output` is the bound
    /// type and `ImplBlock::assoc` keeps only the name.
    #[test]
    fn conformance_admits_generic_impls_and_associated_types() {
        let ok = check_src(
            "protocol Unwrap { type Output  fn valueOr(self, f: Output) -> Output }\n\
             impl<T> Unwrap for Option<T> {\n\
               type Output = T\n\
               fn valueOr(self, f: T) -> T { return match self { Some(v) => v, None => f } } }\n\
             fn main() -> Int64 { let n: Option<Int64> = Some(7)  return n.valueOr(0) }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// The field values settle the type parameter, through every carrier that
    /// reads its type off the expectation.
    #[test]
    fn an_unannotated_generic_literal_solves_from_its_fields() {
        for (decl, lit) in [
            ("type G<T> = { xs: Array<T> }", "G { xs: [2, 1] }"),
            ("type G<T> = { xs: Array<T, 2> }", "G { xs: [2, 1] }"),
            ("type G<T> = { xs: SmallArray<T, 4> }", "G { xs: [2] }"),
            ("type G<T> = { o: Option<T> }", "G { o: Some(2) }"),
            ("type G<T> = { m: Map<String, T> }", "G { m: [\"k\": 2] }"),
            ("type G<T> = { r: Result<T, String> }", "G { r: Ok(2) }"),
            (
                "type P<T> = { v: T }\ntype G<T> = { p: P<T> }",
                "G { p: P { v: 2 } }",
            ),
        ] {
            let r = check_src(&format!(
                "{decl}\nfn main() -> Int64 {{ let g = {lit}\n return 0 }}"
            ));
            assert!(r.is_ok(), "{lit}: {r:?}");
        }
    }

    /// Field order does not decide. `back: []` settles nothing and leaves the
    /// parameter for `front`; binding `T = T` would refuse the literal.
    #[test]
    fn an_empty_field_leaves_the_parameter_for_a_later_one() {
        let head = "type D<T> = { front: Array<T>, back: Array<T> }\n";
        for lit in [
            "D { front: [2, 1], back: [] }",
            "D { back: [], front: [2, 1] }",
        ] {
            let r = check_src(&format!(
                "{head}fn main() -> Int64 {{ let d = {lit}\n return 0 }}"
            ));
            assert!(r.is_ok(), "{lit}: {r:?}");
        }
    }

    /// The same for a `fn`-typed field: a stored `fn` needs a solved
    /// signature, so `width` reads what `tag` settled, in either written order.
    #[test]
    fn a_fn_field_reads_the_same_solve_in_either_literal_order() {
        let head = "type G<T> = { tag: T, width: fn(T) -> Int64 }\n\
                    fn widthOf(s: String) -> Int64 { return 1 }\n";
        for lit in [
            "G { tag: \"x\", width: widthOf }",
            "G { width: widthOf, tag: \"x\" }",
        ] {
            let r = check_src(&format!(
                "{head}fn main() -> Int64 {{ let g = {lit}\n return 0 }}"
            ));
            assert!(r.is_ok(), "{lit}: {r:?}");
        }
    }

    /// Declared order decides, because the backends emit fields in it. With
    /// the `fn` field declared first nothing has solved `T` when it is placed,
    /// so both written orders are refused.
    #[test]
    fn a_fn_field_declared_before_its_solver_is_refused_in_either_order() {
        let head = "type G<T> = { width: fn(T) -> Int64, tag: T }\n\
                    fn widthOf(s: String) -> Int64 { return 1 }\n";
        for lit in [
            "G { tag: \"x\", width: widthOf }",
            "G { width: widthOf, tag: \"x\" }",
        ] {
            let e = check_src(&format!(
                "{head}fn main() -> Int64 {{ let g = {lit}\n return 0 }}"
            ))
            .unwrap_err();
            assert!(e.contains("nothing has solved `T` here"), "{lit}: {e}");
        }
        // The annotation is the fix, in either order.
        for lit in [
            "G { tag: \"x\", width: widthOf }",
            "G { width: widthOf, tag: \"x\" }",
        ] {
            let ok = check_src(&format!(
                "{head}fn main() -> Int64 {{ let g: G<String> = {lit}\n return 0 }}"
            ));
            assert!(ok.is_ok(), "{lit}: {ok:?}");
        }
    }

    /// Fields that disagree are a mismatch, not a wrong solve: the first field
    /// settles the parameter and the second is measured against the answer.
    #[test]
    fn disagreeing_fields_report_the_mismatch() {
        let e = check_src(
            "type D<T> = { front: Array<T>, back: Array<T> }\n\
             fn main() -> Int64 { let d = D { front: [1], back: [\"x\"] }\n return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("array elements must share a type: expected Int64, found String"),
            "{e}"
        );
        let e = check_src(
            "type P<T> = { a: T, b: T }\n\
             fn main() -> Int64 { let p = P { a: 1, b: \"x\" }\n return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("type parameter `T` is both Int64 and String"),
            "{e}"
        );
    }

    /// Fields that say nothing are an error that names the fix; otherwise an
    /// open `D<T>` would reach a concrete program.
    #[test]
    fn a_literal_that_settles_nothing_names_the_annotation() {
        let e = check_src(
            "type D<T> = { front: Array<T>, back: Array<T> }\n\
             fn main() -> Int64 { let d = D { front: [], back: [] }\n return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("cannot infer type parameter `T` of `D`"), "{e}");
        assert!(e.contains("let x: D<..> = D { .. }"), "{e}");
        // The annotation is the fix.
        let ok = check_src(
            "type D<T> = { front: Array<T>, back: Array<T> }\n\
             fn main() -> Int64 { let d: D<Int64> = D { front: [], back: [] }\n return 0 }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// Values do not answer a parameter under a compound slot: the backends
    /// build a literal inside out, so the inner `[2]` and the stored `fn` would
    /// need a type not yet solved. The annotation is the fix.
    #[test]
    fn a_parameter_under_a_compound_slot_is_still_refused() {
        let e = check_src(
            "type G<T> = { xs: Array<Array<T>> }\n\
             fn main() -> Int64 { let g = G { xs: [[2]] }\n return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("array elements must share a type"), "{e}");
        let ok = check_src(
            "type G<T> = { xs: Array<Array<T>> }\n\
             fn main() -> Int64 { let g: G<Int64> = G { xs: [[2]] }\n return 0 }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// A rigid parameter still answers: `T` in `emptyG` is the caller's type,
    /// known only to the expectation.
    #[test]
    fn a_rigid_parameter_still_answers_for_the_fields() {
        let ok = check_src(
            "type G<T> = { xs: Array<T> }\n\
             type H<T> = { tag: Int64 }\n\
             fn emptyG<T>() -> G<T> { return G { xs: [] } }\n\
             fn brand<T>() -> H<T> { return H { tag: 1 } }\n\
             fn main() -> Int64 { let g: G<String> = emptyG()\n \
             let h: H<String> = brand()\n return 0 }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// Field values do not answer a stored `fn` field, so an unannotated
    /// literal reaches the check with the parameter open and is asked for an
    /// annotation.
    #[test]
    fn an_unsolved_parameter_in_a_stored_fn_type_names_the_annotation() {
        const G: &str = "type G<T> = { width: fn(T) -> Int64 }\n\
                         fn widthOf(s: String) -> Int64 { return 1 }\n";
        let e = check_src(&format!(
            "{G}fn main() -> Int64 {{ let g = G {{ width: widthOf }}\n return 0 }}"
        ))
        .unwrap_err();
        assert!(e.contains("nothing has solved `T` here"), "{e}");
        assert!(e.contains("Annotate the binding"), "{e}");
        assert!(!e.contains("will pass it T"), "{e}");
        // The annotation is the fix.
        let ok = check_src(&format!(
            "{G}fn main() -> Int64 {{ let g: G<String> = G {{ width: widthOf }}\n return 0 }}"
        ));
        assert!(ok.is_ok(), "{ok:?}");
        // A field that solves the parameter first also works: the guard fires
        // only on an open parameter.
        let ok = check_src(
            "type G<T> = { tag: T, width: fn(T) -> Int64 }\n\
             fn widthOf(s: String) -> Int64 { return 1 }\n\
             fn main() -> Int64 { let g = G { tag: \"x\", width: widthOf }\n return 0 }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// A generic call's `fn`-typed argument arrives with its return parameter
    /// open and still compiles: `check_fn_arg` solves it from the value, and
    /// only a stored position reaches `stored_fn_named`'s guard.
    #[test]
    fn a_generic_higher_order_call_still_compiles() {
        let ok = check_src(
            "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> { return [] }\n\
             fn twice(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let xs: Array<Int64> = [1, 2]\n \
             let a = map(xs, x -> x * 2)\n let b = map(xs, twice)\n return 0 }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// A fn value's parameter check is contravariant: the value must accept
    /// what the callee passes. `refusals.rs` pins the refusal; this pins the
    /// accepted direction.
    #[test]
    fn a_fn_value_parameter_takes_a_wider_record() {
        let ok = check_src(
            "type P = { x: Int64 } \
             type Q = { x: Int64, y: Int64 } \
             fn g(v: Q) -> Int64 { return v.y } \
             fn apply(f: fn(Q) -> Int64, q: Q) -> Int64 { return f(q) } \
             fn main() -> Int64 { let h: fn(Q) -> Int64 = g \
             return apply(h, Q { x: 1, y: 2 }) }",
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// The type `lazy T` names must exist.
    #[test]
    fn a_lazy_field_must_name_a_real_type() {
        let e = check_src("type Bad = { f: lazy NoSuchType } fn main() -> Int64 { return 0 }")
            .unwrap_err();
        assert!(e.contains("unknown type `NoSuchType`"), "{e}");
    }

    /// A generic call passes its parameter type as the argument's expectation,
    /// so `Ok(1)` types from the parameter.
    #[test]
    fn a_generic_call_types_a_literal_from_its_parameter() {
        let src = "fn unwrapOr<T>(r: Result<T, String>, d: T) -> T { \
                   return match r { Ok(v) => v, Err(m) => d } } \
                   fn main() -> Int64 { return unwrapOr(Ok(1), 5) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    /// The comptime-purity probe descends into lambda bodies: an effect behind
    /// a stored lambda is an effect the generator can run.
    #[test]
    fn comptime_purity_sees_an_effect_inside_a_lambda() {
        let src = "fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(2) }                    gen fn g() -> String {                    let n = apply(x -> { print(x) return x })                    return \"\" }                    fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("not comptime-pure"), "{e}");
    }

    /// A T's storage rules reach through `lazy T`, so a stream behind one is
    /// refused.
    #[test]
    fn a_lazy_field_may_not_hold_a_stream() {
        let e = check_src("type B = { f: lazy Stream<Int64> } fn main() -> Int64 { return 0 }")
            .unwrap_err();
        assert!(e.contains("nothing may store it"), "{e}");
    }

    /// `Option<M>` for `type M = Option<Int64>` is an accepted nesting, the one
    /// spelling where it is invisible until `resolve` runs.
    #[test]
    fn a_transparent_alias_may_name_a_nested_option() {
        let src = "type M = Option<Int64> \
                   fn wrap<T>(x: T) -> Option<T> { return Some(x) } \
                   fn main() -> Int64 { let m: M = None let w = wrap(m) return 0 }";
        assert!(check_src(src).is_ok());
    }

    /// The owned-container walk under `copy` is unbounded (cycle-guarded): an
    /// `impl Owned` type twelve wrappers deep is still refused, because a
    /// structural copy would run its `release` over two owners.
    #[test]
    fn copy_finds_an_owned_container_past_eight_wrappers() {
        let mut decls = String::from(
            "protocol Owned { fn release(self) } \
             type Ring = { buf: Array<Int64> } \
             impl Owned for Ring { fn release(self) { print(1) } } \
             type W0 = { r: Ring }",
        );
        for i in 1..12 {
            decls.push_str(&format!(" type W{i} = {{ w: W{} }}", i - 1));
        }
        let src = format!(
            "{decls} fn f(x: W11) -> Int64 {{ let q = x.copy() return 0 }} \
             fn main() -> Int64 {{ return 0 }}"
        );
        let e = check_src(&src).unwrap_err();
        assert!(e.contains("`copy` cannot copy `Ring`"), "{e}");
    }

    /// Method names dispatch to impls before free functions, so a top-level
    /// fn sharing a protocol method's name could never run; it is refused at
    /// the declaration.
    #[test]
    fn a_top_level_fn_may_not_take_a_protocol_methods_name() {
        let src = "type T = { v: Int64 } \
                   protocol P { fn frob(self) -> Int64 } \
                   impl P for T { fn frob(self) -> Int64 { return self.v } } \
                   fn frob(x: Int64) -> Int64 { return x * 2 } \
                   fn main() -> Int64 { return frob(5) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("could never run"), "{e}");
    }

    /// Inside a `region`, `h(s)` through a stored `consume`-taking `h` is
    /// refused like the direct `keep(s)`.
    #[test]
    fn rejects_a_consume_handover_through_a_stored_fn_value() {
        let src = "let mut kept: Array<String> = [] \
                   fn keep(s: consume String) -> Int64 { kept.push(s) return 0 } \
                   fn main() -> Int64 { \
                       let h: fn(String) -> Int64 = keep \
                       region { h(\"a\" + \"b\") } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("which is `consume`, inside a `region`"), "{e}");
    }

    /// Two distinct recursive records compared by width subtyping answer "not
    /// assignable": the capped field walk does not diverge.
    #[test]
    fn recursive_records_compare_to_a_type_error_not_a_crash() {
        let src = "type NodeA = { v: Int64, next: Option<NodeA> } \
                   type NodeB = { v: Int64, next: Option<NodeB> }";
        let decls = crate::types::decl_map(&parse(lex(src).unwrap()).unwrap());
        let named = |n: &str| Type::Named(n.to_string());
        assert!(!crate::types::coercible(
            &named("NodeA"),
            &named("NodeB"),
            &decls
        ));
    }

    /// The region escape guard's heap walk terminates on a recursive record
    /// and refuses the store. The heap field sits after the self-reference,
    /// where an unguarded walk would overflow first.
    #[test]
    fn the_region_heap_guard_terminates_on_a_recursive_record() {
        let src = "type Node = { next: Option<Node>, name: String } \
                   fn main() -> Int64 { \
                       let mut out = Node { next: None, name: \"n\" } \
                       region { out = Node { next: None, name: \"n\" } } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot store a heap value"), "{e}");
    }

    /// A sibling's `Option<Int64>` does not mask a later `Option<Ring>` in the
    /// owned walk, so `copy` refuses the `impl Owned` container.
    #[test]
    fn copy_sees_an_owned_container_behind_a_masked_sibling_option() {
        let src = "protocol Owned { fn release(self) } \
                   type Ring = { buf: Array<Int64> } \
                   impl Owned for Ring { fn release(self) { print(1) } } \
                   type Holder = { a: Option<Int64>, b: Option<Ring> } \
                   fn f(h: Holder) -> Int64 { let q = h.copy() return 0 } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("`copy` cannot copy"), "{e}");
    }

    /// The exact minimum of every signed sized type is writable negated: the
    /// bare magnitude does not fit, but its negation does.
    #[test]
    fn sized_int_minimums_are_writable_negated() {
        let ok = "fn main() -> Int64 { \
                  let a: Int8 = -128 let b: Int16 = -32768 \
                  let c: Int32 = -2147483648 return 0 }";
        assert!(check_src(ok).is_ok());
    }

    /// A negative literal adapts to a sized sibling exactly as a bare one
    /// does, down to the type's minimum.
    #[test]
    fn negative_literals_adapt_to_sized_operands() {
        let ok = "fn main() -> Int64 { let mut x: Int32 = 0 \
                  if x == -5 { x = -2147483648 } return 0 }";
        assert!(check_src(ok).is_ok());
    }

    /// Direct construction of a nesting whose inner layer hides behind an
    /// alias; the inference route is the case above.
    #[test]
    fn some_accepts_a_payload_alias_that_names_an_option() {
        let src = "type M = Option<Int64> \
                   fn main() -> Int64 { let m: M = None let w = Some(m) return 0 }";
        assert!(check_src(src).is_ok());
    }

    /// A parameter bounded by a protocol other than the first to declare the
    /// method dispatches through its bound.
    #[test]
    fn a_param_bounded_by_a_later_protocol_dispatches_through_it() {
        let src = "type T = { v: Int64 } \
                   protocol P1 { fn frob(self) -> Int64 } \
                   protocol P2 { fn frob(self) -> Int64 } \
                   impl P1 for T { fn frob(self) -> Int64 { return self.v } } \
                   impl P2 for T { fn frob(self) -> Int64 { return self.v } } \
                   fn g<U: P2>(u: U) -> Int64 { return u.frob() } \
                   fn main() -> Int64 { return g(T { v: 7 }) }";
        assert!(check_src(src).is_ok());
    }

    /// An index store takes a named key type as the lookup does: both ask
    /// [`Checker::key_fits`] (#509). A key of another type is the typed
    /// judgment's refusal.
    #[test]
    fn an_index_store_takes_the_key_type_a_lookup_takes() {
        let head = "protocol Hashable { fn hash(self) -> UInt64 }                     type Suit = | Clubs | Hearts                     impl Hashable for Suit { fn hash(self) -> UInt64 { return UInt64(1) } }                     fn main() -> Int64 { let mut s: Map<Suit, Int64> = [:] ";
        let ok = format!("{head} let h: Suit = Hearts s[h] = 1 s[Clubs] = 2 return s.length }}");
        assert_eq!(check_src(&ok), Ok(()));
    }
}
