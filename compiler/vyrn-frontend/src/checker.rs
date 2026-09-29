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
use crate::rules::Rule;
use crate::types::mentions_param as type_mentions_param;
use crate::types::walk_type;
use crate::types::FALLIBLE;

/// A checker error on a whole line (column 0): most AST nodes carry no column.
/// `cerr!(line, Rule, hole = expr, ..)` fills each hole with `expr`'s
/// `Display`; a bare `hole` reads the binding of that name.
macro_rules! cerr {
    ($line:expr; $rule:expr) => {
        $crate::diagnostics::Diagnostic::refusal($crate::checker::line_of($line), 0, "check", $rule)
    };
    ($line:expr, $rule:ident $(, $h:ident $(= $e:expr)?)* $(,)?) => {
        $crate::rules::refuse!("check", $crate::checker::line_of($line), 0, $rule $(, $h $(= $e)?)*)
    };
}

/// A checker error at a column span. `$span` is `(col, end_col)`, 1-based;
/// `(0, 0)`, from a synthesized declaration, means the whole line.
macro_rules! cerr_at {
    ($line:expr, $span:expr, $($arg:tt)*) => {{
        let (col, end_col) = $span;
        cerr!($line, $($arg)*).at(col, end_col)
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
pub const GEN_ENTRY_TYPE_ARG: &str = "__vyrnGenTypeArg";

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
    let (out, binders, _, _, _) = check_accum_full(program);
    (out, binders)
}

/// Returns the stored-function-value collection the `--workers`
/// gate needs: the held record's, else a check's. Diagnostics are discarded:
/// callers have already checked.
pub fn stored_fn_effects(program: &Program) -> StoredFnEffects {
    match held_now(program) {
        Some(r) => r.stored.clone(),
        None => check_accum_full(program).2,
    }
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
    // (see `crate::prelude::Builtin::method`).
    "value",
    "list",
    "schemaOf",
    "contractOf",
    "jsonSchema",
    "toJson",
    "fromJson",
    "derive",
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
    /// Returns the rule a program that wrote `name` breaks.
    pub fn rule(&self, name: &str) -> Rule {
        let name = name.to_string();
        match self {
            Gone::Module(m) => Rule::GoneModule {
                name,
                module: m.to_string(),
            },
            Gone::Removed(s) => Rule::GoneRemoved {
                hint: s.to_string(),
            },
            Gone::Desugared { module, sugar } => Rule::GoneDesugared {
                name,
                module: module.to_string(),
                sugar: sugar.to_string(),
            },
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

/// Returns the diagnostics, every `derive` site, the refused set, and the
/// root's bindings. The refused set holds the functions and module state the
/// diagnostics all belong to, so every other body is typed; it is `None` when
/// a refusal stands anywhere else.
pub fn check_accum_with_sites(program: &Program) -> (Appended, Vec<LocalBinding>) {
    let (out, binders, _, derived, refused) = check_accum_full(program);
    ((out, derived, refused), binders)
}

fn check_accum_full(
    program: &Program,
) -> (
    Vec<Diagnostic>,
    Vec<LocalBinding>,
    StoredFnEffects,
    Vec<crate::gen::Site>,
    Option<HashSet<String>>,
) {
    let (out, binders, effects, derived, typed, _) = check_accum_inner(program, false, 0);
    (out, binders, effects, derived, typed)
}

/// Checks `program`, whose functions before `at` passed a check alone, typing
/// only the bodies from `at` on. Returns the diagnostics, the `derive` sites of
/// those bodies, and the refused set, as a whole check would.
///
/// A body is typed against the declarations alone, so an earlier body keeps
/// its verdict unless the appended functions change a table it reads by
/// something other than their names. `None` names the case where a whole
/// check must run instead: an appended signature makes a stored function
/// value's parameter `consume`.
pub fn check_appended(program: &Program, at: usize) -> Option<Appended> {
    let before = caps_by_sig(&program.functions[..at]);
    let widened = caps_by_sig(&program.functions)
        .into_iter()
        .any(|(k, caps)| before.get(&k).is_some_and(|b| *b != caps));
    if widened {
        return None;
    }
    let (out, _, _, derived, typed, _) = check_accum_inner(program, false, at);
    Some((out, derived, typed))
}

pub type Appended = (
    Vec<Diagnostic>,
    Vec<crate::gen::Site>,
    Option<HashSet<String>>,
);

/// Parameter capabilities keyed by the Debug text of a `Type::Fn`, which
/// carries none, for a call through a stored function value. When two
/// declarations share a signature, `consume` wins: refusing is the sound side.
fn caps_by_sig(functions: &[crate::ast::Function]) -> HashMap<String, Vec<Capability>> {
    let mut caps_by_sig: HashMap<String, Vec<Capability>> = HashMap::new();
    for f in functions {
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
    caps_by_sig
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

#[allow(clippy::type_complexity)]
fn check_accum_inner(
    program: &Program,
    recording: bool,
    bodies_from: usize,
) -> (
    Vec<Diagnostic>,
    Vec<LocalBinding>,
    StoredFnEffects,
    Vec<crate::gen::Site>,
    Option<HashSet<String>>,
    Option<Recorded>,
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
            out.push(cerr!(t.line, RedefinesBuiltinType, name = t.name).in_file(t.module.clone()));
            continue;
        }
        if types.contains_key(&t.name) {
            out.push(cerr!(t.line, TypeDefinedTwice, name = t.name).in_file(t.module.clone()));
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
                    out.push(cerr!(t.line, ReservedName, name = v.name));
                    continue;
                }
                if variants.contains_key(&v.name) {
                    out.push(cerr!(t.line, EnumVariantDefinedTwice, name = v.name));
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
                    out.push(cerr!(t.line, VariantClashesWithType, name = v.name, from));
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
            out.push(cerr_at!(f.line, f.name_span(), ReservedName, name = f.name));
            continue;
        }
        if variants.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                FunctionIsVariant,
                name = f.name
            ));
            continue;
        }
        if sigs.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                FunctionDefinedTwice,
                name = f.name
            ));
            continue;
        }
        if types.contains_key(&f.name) {
            out.push(cerr_at!(
                f.line,
                f.name_span(),
                FunctionIsType,
                name = f.name
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
    let caps_by_sig = caps_by_sig(&program.functions);

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
                FunctionShadowsProtocolMethod,
                name = f.name,
                owners = owners.join(", ")
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
                        ImplMissesAssociatedType,
                        protocol = imp.protocol,
                        ty = imp.ty,
                        name
                    ));
                }
            }
            for name in &imp.assoc {
                if !declared.contains(name) {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        ImplBindsUndeclaredType,
                        protocol = imp.protocol,
                        ty = imp.ty,
                        name
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
                            ImplMissesProjection,
                            protocol = imp.protocol,
                            ty = imp.ty,
                            want = want()
                        ));
                        continue;
                    };
                    let (agrees, got) = provided(f);
                    if !agrees {
                        out.push(cerr_at!(
                            f.line,
                            f.name_span(),
                            ProjectionMismatchesProtocol,
                            name = f.name,
                            protocol = imp.protocol,
                            want = want(),
                            got
                        ));
                    }
                    continue;
                }
                let Some(f) = imp.methods.iter().find(|m| m.name == sig.name) else {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        ImplMissesMethod,
                        protocol = imp.protocol,
                        ty = imp.ty,
                        want = want()
                    ));
                    continue;
                };
                let (agrees, got) = provided(f);
                if !agrees {
                    out.push(cerr_at!(
                        f.line,
                        f.name_span(),
                        MethodMismatchesProtocol,
                        head = render_impl_head(imp),
                        protocol = imp.protocol,
                        want = want(),
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
                    ImplMethodUndeclared,
                    head = render_impl_head(imp),
                    sig = render_method_sig(&m.name, recv, &got, &got_caps, &m.ret),
                    protocol = imp.protocol,
                    fix
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
                        MethodMismatchesProtocol,
                        head = render_impl_head(imp),
                        protocol = imp.protocol,
                        want = render_method_sig(&f.name, *recv, &got, caps, &f.ret),
                        got = render_method_sig(&f.name, got_recv, &got, &got_caps, &f.ret)
                    ));
                }
            }
        } else {
            out.push(cerr_at!(
                imp.line,
                imp.head_span(),
                ImplUnknownProtocol,
                protocol = imp.protocol,
                ty = imp.ty
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
                        ShowReturnsString,
                        ret = m.ret
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
                        ImplHeadCollides,
                        head,
                        prev,
                        prev_line,
                        key,
                        protocol = imp.protocol
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
                    ImplUnsupported,
                    protocol = imp.protocol,
                    ty = imp.ty,
                    why
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
        derive_sites: RefCell::new(Vec::new()),
        record: recording.then(RefCell::default),
        pending_subst: RefCell::new(None),
        pending_call: RefCell::new(None),
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
            out.push(s.in_file(c.module.clone()));
        }
    }

    // 3c. Validate each protocol decl at its own line: its method signatures
    //     name types that exist.
    for p in &program.protocols {
        for s in checker.check_protocol_decl(p) {
            out.push(s.in_file(p.module.clone()));
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
        None if !is_library => out.push(cerr!(0, NoMain)),
        None => {}
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => {
            out.push(cerr!(0, MainSignature))
        }
        _ => {}
    }

    // 5. Check functions, each independently. In a body, errors accumulate
    //    per statement in `errors`; `function` returns the first and this
    //    drains the rest. Within one expression the check is first-error.
    for f in &program.functions[bodies_from..] {
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
                    return Err(cerr_at!(f.line, f.name_span(), ExternTakesFn));
                }
                if checker.contains_fn(&p.ty) && f.is_gen {
                    return Err(cerr_at!(f.line, f.name_span(), GenTakesFn));
                }
                checker.ensure_type_exists(&p.ty, f.line)?;
            }
            if checker.contains_fn(&f.ret) && (f.is_extern || f.is_export_extern) {
                return Err(cerr_at!(f.line, f.name_span(), ExternReturnsFn));
            }
            if checker.contains_fn(&f.ret) && f.is_gen {
                return Err(cerr_at!(f.line, f.name_span(), GenReturnsFn));
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
            out.push(s.in_file(f.module.clone()));
        }
        for s in checker.errors.borrow_mut().drain(..) {
            out.push(s.in_file(f.module.clone()));
        }
        if out.len() > produced_from {
            refused.insert(f.name.clone());
            in_bodies += out.len() - produced_from;
        }
    }

    // 6. Projection, test and bench bodies. A test or bench is a Unit body
    //    under an unspellable name (`test@<index>`), absent from `sigs`, so no
    //    code can call it.
    if bodies_from == 0 {
        check_places(&checker, program, &mut out);
        check_named_blocks(&checker, &program.tests, "test", &checker.in_test, &mut out);
        check_named_blocks(
            &checker,
            &program.benches,
            "bench",
            &checker.in_bench,
            &mut out,
        );
    }

    // 7. Comptime purity of every `gen fn` and its callees, after
    //    the body checks so a generator's type errors come first.
    check_comptime_purity(program, &mut out);

    let effects = StoredFnEffects {
        sources: checker.stored_sources.borrow().clone(),
        arg_sources: checker.arg_sources.borrow().clone(),
        calls: checker.stored_calls.borrow().clone(),
    };
    let binders = local_index(program, &checker.binder_types.borrow());
    let typed = (in_bodies == out.len()).then_some(refused);
    let record = checker.record.map(|r| Recorded {
        stored: effects.clone(),
        ..r.into_inner()
    });
    (
        out,
        binders,
        effects,
        checker.derive_sites.take(),
        typed,
        record,
    )
}

/// Checks every projection body as a function body, plus three rules of its
/// own. It is inlined, so it returns once, as its last statement. It returns
/// a place, because a value would be a hidden copy. The place is
/// rooted in `self` or a parameter, which the access site owns.
fn check_places(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {
    for (imp, f) in crate::project::all(program) {
        let mut push = |d: Diagnostic| out.push(d.in_file(f.module.clone()));
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
                ProjectionOneReturn,
                name = f.name
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
                ProjectionUsesTry,
                name = f.name
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
                ProjectionReturnsValue,
                name = f.name
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
            OptionalProjectionSugarName,
            name = f.name
        ));
        return;
    }
    if f.params.first().map(|p| p.capability) != Some(crate::ast::Capability::Read) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            OptionalProjectionReadSelf,
            name = f.name
        ));
        return;
    }
    let shape = cerr_at!(
        f.line,
        f.name_span(),
        OptionalProjectionShape,
        name = f.name
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
            ProjectionUsesTry,
            name = f.name
        ));
        return;
    }
    if !crate::project::is_place(y) {
        push(cerr_at!(
            f.line,
            f.name_span(),
            OptionalProjectionReturnsValue,
            name = f.name
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
            ProjectionForeignRoot,
            name = f.name,
            root
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
            _ => crate::ast::sub_blocks(s)
                .into_iter()
                .map(count_yields)
                .sum(),
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
            let d = cerr!(t.line, DuplicateBlockName, noun, name = t.name, prev);
            out.push(d.in_file(t.module.clone()));
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
            body: Block {
                id: Id::NEW,
                stmts: Vec::new(),
            },
            line: t.line,
            col: 0,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        };
        if let Err(s) = checker.function_body(&synthetic, &t.body) {
            out.push(s.in_file(t.module.clone()));
        }
        for s in checker.errors.borrow_mut().drain(..) {
            out.push(s.in_file(t.module.clone()));
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

/// What the checker decided, keyed by node.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    /// The static type of every expression the checker typed.
    pub node_types: HashMap<NodeId, Type>,
    /// The static type of every join (a `match` or `if` expression), before
    /// instantiation. A subset of [`Recorded::node_types`], kept apart for the
    /// emitters, which cannot tell a join from its address.
    pub joins: HashMap<NodeId, Type>,
    /// Every solved type parameter of a generic call or record literal, keyed
    /// by the [`Expr::Call`] or [`Expr::StructLit`] node: the callee or record
    /// name, and the solved arguments in its type-parameter order. The checker
    /// refines nothing later, so the solution governs its whole subtree.
    pub node_substs: HashMap<NodeId, (String, Vec<(String, Type)>)>,
    /// A declared call whose arity, type-argument count or argument the typed
    /// judgment refuses, keyed by the [`Expr::Call`] node.
    pub calls: HashMap<NodeId, CallDecl>,
    /// What a check that records nothing returns as [`stored_fn_effects`].
    pub stored: StoredFnEffects,
}

/// One pass that returns the diagnostics, the root's bindings and the record.
fn recording_check(program: &Program) -> (Vec<Diagnostic>, Vec<LocalBinding>, Recorded) {
    let (diags, binders, _, _, _, made) = check_accum_inner(program, true, 0);
    (diags, binders, made.unwrap_or_default())
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
    hold_forget();
}

/// Drops the held record and keeps the slot open, for a caller that changes
/// the program the record typed.
pub fn hold_forget() {
    HELD.with(|h| *h.borrow_mut() = None);
}

/// Closes the record slot. Called by [`crate::own::Memo`]'s `Drop`.
pub(crate) fn hold_close() {
    HOLDING.with(|h| h.set(0));
    hold_forget();
}

/// The record slot as `vyrn_lower::check_and_synthesize` holds it, from
/// synthesis to the last judgment, so its three readers share one record. It
/// stands aside where another holder has the slot, so it never closes theirs.
pub struct Held(bool);

impl Held {
    pub fn open(program: &Program) -> Held {
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

/// The record held for `program` under the host flags in force.
fn held_now(program: &Program) -> Option<std::rc::Rc<Recorded>> {
    held(program)
        .filter(|(g, t, _)| (*g, *t) == (gen_host(), test_host()))
        .map(|(_, _, r)| r)
}

/// Returns the record of `program`: the held one if it matches the program
/// and host flags, else a new one, held for the next ask.
pub fn recorded(program: &Program) -> std::rc::Rc<Recorded> {
    if let Some(r) = held_now(program) {
        return r;
    }
    let made = std::rc::Rc::new(record(program));
    hold(program, made.clone());
    made
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
    derive_sites: RefCell<Vec<crate::gen::Site>>,
    /// The record [`record`] asks for, or `None`, so the editor's keystroke
    /// path does not pay for it.
    record: Option<RefCell<Recorded>>,
    /// The substitution the innermost generic call just solved, for the
    /// [`Checker::expr`] wrapper that knows the call node's address. A nested
    /// call consumes and clears it before its caller writes one.
    pending_subst: RefCell<Option<(String, Vec<(String, Type)>)>>,
    /// The declaration of the call [`Checker::check_declared_call`] just typed
    /// `Err`, for the same wrapper.
    pending_call: RefCell<Option<CallDecl>>,
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
    fn recording(&self) -> bool {
        self.record.is_some()
    }

    /// Hands the solved type arguments to the [`Checker::expr`] wrapper.
    fn note_subst(&self, name: &str, subst: &HashMap<String, Type>, type_params: &[String]) {
        let args: Vec<(String, Type)> = type_params
            .iter()
            .filter_map(|p| subst.get(p).map(|t| (p.clone(), t.clone())))
            .collect();
        *self.pending_subst.borrow_mut() = Some((name.to_string(), args));
    }

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
            Type::Float | Type::Float32 => Err(cerr!(line, MapKeyFloat, key)),
            Type::Record(fs) => fs
                .iter()
                .try_for_each(|f| self.check_key_shape(key, &self.base(&f.ty), line)),
            Type::Enum(vs) => {
                if vs.iter().all(|v| v.payload.is_empty()) {
                    Ok(())
                } else {
                    Err(cerr!(line, MapKeyPayloadEnum, key))
                }
            }
            _ => Err(cerr!(line, MapKeyOwnsHeap, key)),
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
            return Err(cerr!(line, ChainedProjectionUnresolved));
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
            return Err(cerr!(line, OptionalPlaceTested, name));
        }
        let subst =
            self.solve_projection_call(imp, f, name, &recv, args, scope, Some(fn_ret), line)?;
        if self.recording() {
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
            return Err(cerr!(line, OptionalPlaceRead, method));
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
                ProjectionArity,
                name,
                want = f.params.len() - 1,
                got = args.len() - 1
            ));
        }
        let mut subst: HashMap<String, Type> = HashMap::new();
        self.unify(&imp.ty, recv, &mut subst, line)?;
        for (arg, p) in args[1..].iter().zip(&f.params[1..]) {
            let want = crate::types::substitute(&p.ty, &subst);
            let got = self.expr(arg, scope, Some(&want), fn_ret)?;
            if !self.coercible(&got, &want) {
                return Err(cerr!(line, ProjectionArgType, name, got, want));
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
                ConstFailsPredicate,
                cv,
                name = decl.name,
                pred = pred_summary(pred)
            )),
            Some((false, None)) => Err(cerr!(
                line,
                ValueFailsPredicate,
                name = decl.name,
                pred = pred_summary(pred)
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
                    InterpolationFailsPredicate,
                    witness,
                    name = decl.name,
                    pred = pred_summary(pred)
                ))
            }
            crate::finite::Proof::Proven | crate::finite::Proof::NotApplicable => Ok(()),
        }
    }

    /// Refuses a `Stream` in a record field or enum payload, which
    /// [`Self::ensure_type_exists`] sees at root position.
    fn ensure_no_stream(&self, ty: &Type, line: usize, where_: &str) -> Result<(), Diagnostic> {
        if self.contains_stream(ty) {
            return Err(cerr!(line, StreamStored, ty, where_));
        }
        Ok(())
    }

    fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), Diagnostic> {
        // A stream's lifetime is a scope: legal only at the root of
        // a binding, parameter or return type, where movecheck can see it.
        if !matches!(self.base(ty), Type::Stream(_)) && self.contains_stream(ty) {
            return Err(cerr!(line, TypeHoldsStream, ty));
        }
        match ty {
            // `Code` and `Token` are builtin and generation-only, so no backend
            // sees them. A user declaration of the name wins.
            Type::Named(n) if n == "Code" && !self.types.contains_key("Code") => {
                if !*self.in_gen.borrow() {
                    return Err(cerr!(line, GenOnlyType, name = "Code"));
                }
                return Ok(());
            }
            Type::Named(n) if n == "Token" && !self.types.contains_key("Token") => {
                if !*self.in_gen.borrow() {
                    return Err(cerr!(line, GenOnlyType, name = "Token"));
                }
                return Ok(());
            }
            // `Self` parses as an ordinary name and is not a type. Refused here,
            // so the diagnostic lands on the protocol, not on each impl.
            Type::Named(n) if n == "Self" && !self.types.contains_key("Self") => {
                return Err(cerr!(line, SelfNotType))
            }
            Type::Named(n) => match self.types.get(n) {
                None => return Err(cerr!(line, UnknownType, n)),
                Some(d) if !d.type_params.is_empty() => {
                    return Err(cerr!(line, GenericNeedsArgs, n))
                }
                _ => {}
            },
            Type::App(name, args) => {
                // Only `SmallArray` takes an integer argument. Checked before
                // arity, so `Box<3>` gets the right diagnostic.
                if args.iter().any(|a| matches!(a, Type::ConstInt(_))) {
                    return Err(cerr!(line, TypeTakesNoInteger, name));
                }
                let d = self
                    .types
                    .get(name)
                    .ok_or_else(|| cerr!(line, UnknownType, n = name))?;
                if d.type_params.len() != args.len() {
                    return Err(cerr!(
                        line,
                        TypeArity,
                        name,
                        want = d.type_params.len(),
                        got = args.len()
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
                    .ok_or_else(|| cerr!(line, TransformerBaseNotRecord))?;
                for k in keys {
                    if !fields.iter().any(|f| &f.name == k) {
                        return Err(cerr!(line, TransformerFieldMissing, k));
                    }
                }
            }
            Type::Merge(a, b) => {
                self.ensure_type_exists(a, line)?;
                self.ensure_type_exists(b, line)?;
                if crate::types::record_fields(a, self.types).is_none()
                    || crate::types::record_fields(b, self.types).is_none()
                {
                    return Err(cerr!(line, MergeNeedsRecords));
                }
            }
            Type::Partial(base) => {
                self.ensure_type_exists(base, line)?;
                if crate::types::record_fields(base, self.types).is_none() {
                    return Err(cerr!(line, PartialNeedsRecord));
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
                    return Err(cerr!(line, SmallArrayCapacity));
                }
                self.ensure_type_exists(inner, line)?;
            }
            Type::ConstInt(_) => return Err(cerr!(line, IntegerNotType)),
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
                            return Err(cerr!(line, MapKeyNeedsHashable, key));
                        }
                    }
                    _ => {
                        return Err(cerr!(line, MapKeyType, key));
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
                        return Err(cerr!(line, FnTypeTakesFn));
                    }
                    self.ensure_type_exists(p, line)?;
                }
                if self.contains_fn(ret) {
                    return Err(cerr!(line, FnTypeReturnsFn));
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
            Ok(vty) if !open && !self.coercible(&vty, ty) => {
                errs.push(cerr!(line, DefaultMismatch, where_, vty, ty))
            }
            Ok(_) => {}
        }
    }

    fn check_type_decl(&self, t: &TypeDecl) -> Result<(), Diagnostic> {
        if t.predicate.is_some() && self.contains_fn(&t.base) {
            return Err(cerr!(t.line, FnTypeWhere));
        }
        // A record's `where` clause names its fields: a cross-field invariant
        // checked at construction.
        if let Type::Record(fields) = &t.base {
            let mut seen = std::collections::HashSet::new();
            for f in fields {
                if !seen.insert(&f.name) {
                    return Err(cerr!(
                        t.line,
                        DuplicateField,
                        field = f.name,
                        record = t.name
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
                return Err(cerr!(t.line, EnumWhere));
            }
            if vs.is_empty() {
                return Err(cerr!(t.line, EnumEmpty, name = t.name));
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
                return Err(cerr!(t.line, AliasWhere, noun));
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
                return Err(cerr!(t.line, RecordWhere));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            if crate::types::record_fields(&t.base, self.types).is_none() {
                return Err(cerr!(t.line, NotRecord, name = t.name));
            }
            return Ok(());
        }
        if !matches!(
            t.base,
            Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
        ) {
            return Err(cerr!(t.line, ValidatedBaseNotScalar, name = t.name));
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
            return Err(cerr!(t.line, PredicateCalls, kind, name = t.name));
        }
        let mut scope = Scope::closed();
        for (name, ty) in binds {
            scope[0].insert(name, Binding { ty, mutable: false });
        }
        let pty = self.expr(pred, &scope, None, None)?;
        if self.base(&pty) != Type::Bool {
            return Err(cerr!(t.line, PredicateNotBool, kind, name = t.name, pty));
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
                    ExternParamType,
                    func = f.name,
                    param = p.name,
                    ty = p.ty
                ));
            }
            // The JS caller frees a String argument when the call returns
            // (`wasi-min.js`), so `consume` would deliver a dangling pointer.
            if matches!(p.capability, Capability::Consume) && matches!(p.ty, Type::Str) {
                self.errors.borrow_mut().push(cerr_at!(
                    f.line,
                    f.name_span(),
                    ExternConsume,
                    func = f.name,
                    param = p.name
                ));
            }
        }
        if !extern_abi_type_ok(&f.ret, true) {
            self.errors.borrow_mut().push(cerr_at!(
                f.line,
                f.name_span(),
                ExternReturnType,
                name = f.name,
                ret = f.ret
            ));
        }
        self.first_error()
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
                    return Err(cerr!(g.line, GlobalUnit, name = g.name));
                }
                if matches!(self.base(&vty), Type::Stream(_)) {
                    return Err(cerr!(g.line, GlobalStream, name = g.name));
                }
                if let Some(declared) = &g.ty {
                    if !self.coercible(&vty, declared) {
                        return Err(cerr!(
                            g.line,
                            GlobalInitMismatch,
                            name = g.name,
                            declared,
                            vty
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
                    out.extend(s.map(|d| d.in_file(g.module.clone())));
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
                let key = g.init.id();
                if let Some(r) = &self.record {
                    r.borrow_mut().node_types.remove(&key);
                }
            }
            out.extend(lambda.into_iter().map(|d| d.in_file(g.module.clone())));
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
        self.first_error()
    }

    /// Hands out the first recorded error as the `Err`; the rest stay in
    /// `errors`.
    fn first_error(&self) -> Result<(), Diagnostic> {
        let mut errs = self.errors.borrow_mut();
        match errs.is_empty() {
            true => Ok(()),
            false => Err(errs.remove(0)),
        }
    }

    /// Types an expansion (an inlined projection) in the caller's scope,
    /// recording its node types and nothing else. `project` leaks each
    /// expansion once ([`crate::project::Memo`]), so its node addresses are
    /// stable keys.
    ///
    /// Diagnostics, scope changes and [`Checker::pending_subst`] stay inside: an
    /// expansion fails only where its source already did, and the wrapper
    /// reads `pending_subst` for the access site's own node.
    fn record_desugar(&self, scope: &Scope, run: impl FnOnce(&Self, &mut Scope)) {
        let mark = self.errors.borrow().len();
        let stored = (
            self.stored_sources.borrow().len(),
            self.arg_sources.borrow().len(),
            self.stored_calls.borrow().len(),
        );
        let saved = self.pending_subst.take();
        let mut sc = scope.clone();
        run(self, &mut sc);
        *self.pending_subst.borrow_mut() = saved;
        self.errors.borrow_mut().truncate(mark);
        self.stored_sources.borrow_mut().truncate(stored.0);
        self.arg_sources.borrow_mut().truncate(stored.1);
        self.stored_calls.borrow_mut().truncate(stored.2);
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
                id: _,
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
            Stmt::Assign {
                name,
                value,
                line,
                id: _,
            } => {
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
                id: _,
            } => {
                let Some(b) = self.lookup(scope, name) else {
                    self.unknown.set(true);
                    return Ok(());
                };
                let ruled = matches!(&b.ty, Type::Named(n) if self.types.get(n).is_some_and(|d| d.predicate.is_some()));
                let Some(fty) = crate::types::record_fields(&b.ty, self.types)
                    .and_then(|fs| fs.into_iter().find(|f| &f.name == field))
                    .map(|f| f.ty)
                else {
                    return Ok(());
                };
                let vty = self.expr(value, scope, Some(&fty), Some(ret))?;
                if ruled {
                    return Ok(());
                }
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
                id: _,
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
                if self.recording() {
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
            Stmt::Return { value, line, id: _ } => {
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
                            Some((imp, _, nth)) => self.solve_head(imp, &ity, &nth.ret, *line),
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
                // A `for` over a user container reads each element through its
                // `nth`; record that read.
                if self.recording() {
                    if let Ok(Some(p)) =
                        crate::project::for_element(self.impl_blocks, &ity, iter, *line)
                    {
                        self.record_desugar(scope, |c, sc| {
                            let bind = |ty| Binding { ty, mutable: false };
                            sc.push(HashMap::from([
                                (crate::project::FOR_RECV.to_string(), bind(ity.clone())),
                                (crate::project::FOR_INDEX.to_string(), bind(Type::Int)),
                            ]));
                            for s in &p.prologue {
                                if c.stmt(s, ret, sc).is_err() {
                                    return;
                                }
                            }
                            let _ = c.expr(&p.place, sc, None, Some(ret));
                        });
                    }
                }
                Ok(())
            }
            Stmt::Drop { .. } => Ok(()),
            Stmt::Expr(e, _) => self.expr(e, scope, None, Some(ret)).map(|_| ()),
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
                return Err(cerr!(line, RegionEscape, name));
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
        Err(cerr!(line, RegionConsume, arg = idx + 1, callee))
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
        let Some(record) = &self.record else {
            return self.expr_inner(expr, scope, expected, fn_ret);
        };
        let t = self.expr_inner(expr, scope, expected, fn_ret)?;
        let key = expr.id();
        assert_ne!(
            key.0, 0,
            "the checker typed a node no numbering reached: {expr:?}"
        );
        let pending = self.pending_subst.take();
        let call = self.pending_call.take();
        {
            let mut r = record.borrow_mut();
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
        }
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
                    Expr::Var { name, line, id: _ }
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
            Expr::Int(n, _) => match expected.map(|t| self.base(t)) {
                Some(t @ Type::IntN { .. }) => Ok(t),
                _ => {
                    if *n < 0 {
                        Err(cerr!(
                            *self.stmt_line.borrow(),
                            IntLiteralOverflow,
                            n = *n as u64
                        ))
                    } else {
                        Ok(Type::Int)
                    }
                }
            },
            // A byte literal takes the expected integer type, else
            // `UInt8`.
            Expr::Byte(_, _) => match expected.map(|t| self.base(t)) {
                Some(Type::Int) => Ok(Type::Int),
                Some(t @ Type::IntN { .. }) => Ok(t),
                _ => Ok(Type::IntN {
                    bits: 8,
                    signed: false,
                }),
            },
            Expr::Float(_, _) => Ok(match expected.map(|t| self.base(t)) {
                Some(Type::Float32) => Type::Float32,
                _ => Type::Float,
            }),
            Expr::Bool(_, _) => Ok(Type::Bool),
            Expr::Str(_, _) => Ok(Type::Str),
            Expr::Var { name, line, id: _ } => {
                if name == "None" {
                    // The expectation is resolved to find the Option, but the
                    // written type is returned, so an alias keeps its name.
                    return match expected.map(|t| self.base(t)) {
                        Some(b) if crate::types::option_payload(&b).is_some() => {
                            Ok(expected.unwrap().clone())
                        }
                        _ => Err(cerr!(line, InferNone)),
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
                        _ => Err(cerr!(line, InferBinding, name)),
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
                if *op == UnOp::Neg && matches!(**expr, Expr::Int(_, _)) {
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
            Expr::Binary {
                op,
                lhs,
                rhs,
                line,
                id: _,
            } => {
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
                if l == Type::Float && r == Type::Float32 && matches!(**lhs, Expr::Float(_, _)) {
                    l = Type::Float32;
                }
                if r == Type::Float && l == Type::Float32 && matches!(**rhs, Expr::Float(_, _)) {
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
                id: _,
            } => {
                let t = self.call(name, args, type_args, *line, scope, expected, fn_ret)?;
                // `schemaOf<T>()` lowers through the literal it stands for, so
                // the checker types those nodes too (`project::schema`).
                if let ("schemaOf", [Type::Named(tn) | Type::App(tn, _)], true) =
                    (name.as_str(), type_args.as_slice(), self.recording())
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
                id: _,
            } => self.check_match(scrutinee, arms, *stmt_pos, *line, scope, expected, fn_ret),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
                id: _,
            } => self.check_if_expr(
                cond,
                then_branch,
                else_branch.as_deref(),
                *line,
                scope,
                expected,
                fn_ret,
            ),
            Expr::Try { expr, line, id: _ } => self.check_try(expr, *line, scope, fn_ret),
            Expr::StructLit {
                name,
                fields,
                line,
                id: _,
            } => self.check_struct_lit(expr, name, fields, *line, scope, expected, fn_ret),
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
            Expr::ArrayLit { elems, line, id: _ } => {
                // Match the resolved expectation: it may be an alias.
                let written = expected;
                let expected = expected.map(|t| self.base(t));
                let expected = expected.as_ref();
                // An empty `[]` takes its element type from the expectation.
                if elems.is_empty() {
                    return match expected {
                        Some(Type::Array(t)) => Ok(Type::Array(t.clone())),
                        Some(Type::SmallArray(t, n)) => Ok(Type::SmallArray(t.clone(), *n)),
                        Some(_) => Err(cerr!(line, ArrayLiteralNotArray, ty = written.unwrap())),
                        None => Err(cerr!(line, InferEmptyArray)),
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
                        return Err(cerr!(line, ArrayElementMismatch, elem_ty, t));
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
            Expr::MapLit {
                entries,
                line,
                id: _,
            } => {
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
                        _ if written.is_some() => {
                            Err(cerr!(line, MapLiteralNotMap, ty = written.unwrap()))
                        }
                        _ => Err(cerr!(line, InferEmptyMap)),
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
                        return Err(cerr!(line, MapLiteralKeyMismatch, key_ty, kt));
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
                        return Err(cerr!(line, MapValueMismatch, val_ty, vt));
                    }
                    self.prove_coercion(v, &val_ty, *line)?;
                    self.prove_string_interpolation(v, &val_ty, scope, fn_ret, *line)?;
                }
                Ok(Type::Map(Box::new(key_ty), Box::new(val_ty)))
            }
            // A lambda here has no function type from context: a legal one is
            // taken above or by `call`'s `fn`-typed parameter.
            Expr::Lambda { line, .. } => Err(cerr!(line, LambdaNeedsFnType)),
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
                    RecordFailsPredicate,
                    name,
                    pred = pred_summary(d.predicate.as_ref().unwrap())
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
                    InferRecordParam,
                    tp,
                    name,
                    shape = shape.join(", ")
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
        if self.recording() {
            self.note_subst(name, &subst, &decl.type_params);
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
        let ret = fn_ret.ok_or_else(|| cerr!(line, TryOutsideFn))?;
        // Order is the rule: the built-in sums first, then `Fallible`, because
        // all of them are variant lists.
        if let Some(t) = crate::types::option_payload(&ety) {
            return match crate::types::option_payload(ret) {
                Some(_) => Ok(t.clone()),
                None => Err(cerr!(line, TryOptionReturn, ret)),
            };
        }
        if let Some((t, e)) = crate::types::result_payloads(&ety) {
            return match crate::types::result_payloads(ret) {
                Some((_, re)) if self.assignable(e, re) => Ok(t.clone()),
                Some((_, re)) => Err(cerr!(line, TryErrorMismatch, e, re)),
                None => Err(cerr!(line, TryResultReturn, ret)),
            };
        }
        match &ety {
            other => {
                let key = crate::types::type_key(other)
                    .filter(|k| self.impls.contains(&(FALLIBLE.to_string(), k.clone())));
                let Some(key) = key else {
                    return Err(cerr!(line, TryOperand, other));
                };
                // Propagation copies the whole value, so the types must match.
                if !self.assignable(other, ret) {
                    return Err(cerr!(line, TryFallibleReturn, other, ret));
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
        let if_let = stmt_pos && matches!(arms.last(), Some(a) if a.pattern == Pattern::Other);
        // An optional projection's one legal position, typed before
        // `place_result` can refuse it.
        let optional = match fn_ret {
            Some(ret) if if_let => {
                self.optional_scrutinee(scrutinee, &arms[0].pattern, scope, ret, line)?
            }
            _ => None,
        };
        let raw_sty = match optional {
            Some(t) => t,
            None => self.expr(scrutinee, scope, None, fn_ret)?,
        };
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
            let form = if if_let { "if let" } else { "match" };
            return Err(cerr!(line, MatchScrutinee, form, sty));
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
            return Err(cerr!(line, BlockArmAsValue));
        }
        let Some(ret) = fn_ret else {
            return Err(cerr!(line, BlockArmOutsideFn));
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
            // A desugar's last arm: any remaining variant, no bindings. No
            // source can write it.
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
                    return Err(cerr!(line, DefaultOperand, sty))
                }
                Pattern::Other => unreachable!("the default arm is checked above the match"),
            };
            let ev = evs
                .iter()
                .find(|v| v.name == vname)
                .ok_or_else(|| cerr!(line, NotAVariant, vname, sty))?;
            if ev.payload.len() != bind.len() {
                return Err(cerr!(
                    line,
                    PatternArity,
                    vname,
                    want = ev.payload.len(),
                    got = bind.len()
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
            return Err(cerr!(line, IfNeedsElse));
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
            Some(rt) => return Err(cerr!(line, MatchArmMismatch, rt, bty)),
        };
        *result = Some(joined.clone());
        Ok(joined)
    }

    fn binop_type(&self, op: BinOp, l: Type, r: Type, line: usize) -> Result<Type, Diagnostic> {
        use BinOp::*;
        if matches!(l, Type::Err) || matches!(r, Type::Err) {
            return Ok(Type::Err);
        }
        // On a type parameter: both operands the same, and the bound present.
        if let Type::Param(t) = &l {
            if &r != &l {
                return Err(cerr!(line, ParamOperand, t, r));
            }
            return match op {
                Add | Sub | Mul | Div | Rem if self.param_has_bound(t, "Num") => {
                    Ok(Type::Param(t.clone()))
                }
                Lt | LtEq | Gt | GtEq if self.param_has_bound(t, "Ord") => Ok(Type::Bool),
                Eq | NotEq if self.param_has_bound(t, "Eq") => Ok(Type::Bool),
                Add | Sub | Mul | Div | Rem => Err(cerr!(line, ParamNeedsNum, t)),
                Lt | LtEq | Gt | GtEq => Err(cerr!(line, ParamNeedsOrd, t)),
                Eq | NotEq => Err(cerr!(line, ParamNeedsEq, t)),
                And | Or => Err(cerr!(line, ParamLogic)),
                Match => Err(cerr!(line, ParamMatch, t)),
                // No bound grants the bitwise operators.
                BitAnd | BitOr | BitXor | Shl | Shr => Err(cerr!(line, ParamBitwise, t)),
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
            Div if l == Type::I32x4 && r == Type::I32x4 => Err(cerr!(line, SimdIntDivide)),
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
                    Err(cerr!(line, ConcatOperands, l, r))
                } else {
                    Err(cerr!(line, ArithOperands, l, r))
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
                    Err(cerr!(line, FloatRemainder, f))
                } else {
                    Err(cerr!(line, RemainderOperands, l, r))
                }
            }
            // Strings order byte-wise, not by locale.
            Lt | LtEq | Gt | GtEq => {
                if l == r && (numeric(&l) || l == Type::Str) {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(line, CompareOperands, l, r))
                }
            }
            Eq | NotEq => {
                if l == r && (numeric(&l) || matches!(l, Type::Bool | Type::Str)) {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(line, EqualityOperands, l, r))
                }
            }
            And | Or => {
                if l == Type::Bool && r == Type::Bool {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(line, LogicOperands, l, r))
                }
            }
            // A shift amount has the shifted value's type.
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let integral = |t: &Type| matches!(t, Type::Int | Type::IntN { .. });
                if l == r && integral(&l) {
                    Ok(l)
                } else if integral(&l) && integral(&r) {
                    Err(cerr!(line, BitwiseMismatch, l, r))
                } else {
                    Err(cerr!(line, BitwiseOperands, l, r))
                }
            }
            Match => {
                if l == Type::Str && r == Type::Str {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(line, MatchOperands, l, r))
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
                return Err(cerr!(line, LaneType, what, lane, t));
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
                    return Err(cerr!(line, LaneCount, what, lanes, got = args.len()));
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
                    return Err(cerr!(line, SplatArity, what, got = args.len()));
                }
                if let Some(e) = lane_arg(&args[0], &lane, &format!("`{what}.splat(..)`"))? {
                    return Ok(e);
                }
                Ok(vec)
            }
            "@lane" => {
                if args.len() != 2 {
                    return Err(cerr!(line, LaneArity, got = args.len()));
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
                    other => return Err(cerr!(line, LaneReceiver, other)),
                };
                let lanes = lanes_of(&v);
                if crate::types::const_lane(&args[1], lanes).is_none() {
                    return Err(cerr!(line, LaneIndex, max = lanes - 1));
                }
                Ok(out)
            }
            // `v.replaceLane(k, x)`, on a vector only: masks come only from
            // comparison.
            "@replaceLane" => {
                if args.len() != 3 {
                    return Err(cerr!(line, ReplaceLaneArity, got = args.len() - 1));
                }
                let v = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                let lane = match v {
                    Type::F32x4 => Type::Float32,
                    Type::I32x4 => INT32,
                    Type::F64x2 => Type::Float,
                    _ => return Err(cerr!(line, ReplaceLaneReceiver, v)),
                };
                // Constant, as for `lane`: the replace-lane opcodes take an
                // immediate.
                let lanes = lanes_of(&v);
                if crate::types::const_lane(&args[1], lanes).is_none() {
                    return Err(cerr!(line, ReplaceLaneIndex, max = lanes - 1));
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
                    return Err(cerr!(line, MaskArity, what, got = args.len() - 1));
                }
                let m = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(m, Type::Err) {
                    return Ok(Type::Err);
                }
                if !matches!(m, Type::Mask32x4 | Type::Mask64x2) {
                    return Err(cerr!(line, MaskReceiver, what, m));
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
                        VectorOpArity,
                        what,
                        op = if store { "store" } else { "load" },
                        want,
                        got = args.len()
                    ));
                }
                // `store` writes through a binding; into a temporary it would
                // be lost, as for `xs.pop()`.
                if store && !matches!(&args[0], Expr::Var { .. }) {
                    return Err(cerr!(line, VectorStoreTarget, what));
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
                            VectorArrayType,
                            what,
                            op = if store { "store" } else { "load" },
                            lane,
                            other
                        ))
                    }
                }
                let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
                if matches!(i, Type::Err) {
                    return Ok(Type::Err);
                }
                if i != Type::Int {
                    return Err(cerr!(line, VectorIndexType, i));
                }
                if !store {
                    return Ok(vec);
                }
                let v = self.base(&self.expr(&args[2], scope, Some(&vec), fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                if v != vec {
                    return Err(cerr!(line, VectorStoreValue, what, v));
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
                        VectorOpArity,
                        what = ty,
                        op = what,
                        want,
                        got = args.len()
                    ));
                }
                for a in args {
                    let t = self.base(&self.expr(a, scope, Some(&vec), fn_ret)?);
                    if matches!(t, Type::Err) {
                        return Ok(Type::Err);
                    }
                    if t != vec {
                        return Err(cerr!(line, VectorOpType, ty, what, t));
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
                Err(cerr!(line, VectorNoMethod, ty, m))
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
                        FnValueArity,
                        name,
                        want = ptys.len(),
                        got = args.len()
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
                        return Err(cerr!(line, FnValueArgType, name, arg = i + 1, pty, aty));
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
            return Err(cerr!(line; g.rule(name)));
        }
        if (name == "assert" || name == "assertEq") && !*self.in_test.borrow() && !test_host() {
            return Err(cerr!(line, TestOnly, name));
        }
        if name == "assertEq" {
            if args.len() != 2 {
                return Err(cerr!(line, TakesTwo, name = "assertEq", got = args.len()));
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
                return Err(cerr!(line, AssertEqOperands, a, b));
            }
            return Ok(Type::Unit);
        }

        // `blackBox<T>(v: T) -> T`: identity the optimizer cannot see through,
        // so the work producing `v` survives and does not fold.
        if name == "blackBox" {
            if !*self.in_test.borrow() && !*self.in_bench.borrow() && !test_host() {
                return Err(cerr!(line, BlackBoxOutsideBench));
            }
            if args.len() != 1 {
                return Err(cerr!(line, TakesOne, name = "blackBox", got = args.len()));
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
                return Err(cerr!(line, PanicArity, got = args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(cerr!(line, PanicType, t));
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
            return Err(cerr!(line, GenOnly, name = "moduleInterface"));
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
                return Err(cerr!(line, GenOnlySurface, surface));
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
                        return Err(cerr!(line, QuoteSplice, t));
                    }
                    return Ok(code());
                }
                "render" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, TakesOne, name = "render", got = args.len()));
                    }
                    let t = self.base(&self.expr(&args[0], scope, Some(&code()), fn_ret)?);
                    if !matches!(t, Type::Err) && t != code() {
                        return Err(cerr!(line, RenderType, t));
                    }
                    return Ok(Type::Str);
                }
                // The origin lets `render` map diagnostics inside the text back.
                "rawAt" => {
                    if args.len() != 4 {
                        return Err(cerr!(line, RawAtArity, got = args.len()));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[2], scope, Some(&Type::Int), fn_ret)?;
                    self.expr(&args[3], scope, Some(&Type::Int), fn_ret)?;
                    return Ok(code());
                }
                "raw" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, TakesOne, name = "raw", got = args.len()));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(code());
                }
                "lex" => {
                    if args.len() != 1 {
                        return Err(cerr!(line, TakesOne, name = "lex", got = args.len()));
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
                return Err(cerr!(line, BytesArity, got = args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(cerr!(line, BytesType, t));
            }
            for a in args.iter().skip(1) {
                let n = self.base(&self.expr(a, scope, Some(&Type::Int), fn_ret)?);
                if !matches!(n, Type::Err) && n != Type::Int {
                    return Err(cerr!(line, BytesOffsets, n));
                }
            }
            return Ok(Type::Array(Box::new(Type::IntN {
                bits: 8,
                signed: false,
            })));
        }

        if name == "@push" {
            if args.len() != 2 {
                return Err(cerr!(line, TakesTwo, name = "push", got = args.len()));
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
                other => return Err(cerr!(line, PushReceiver, other)),
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.coercible(&v, &elem) {
                return Err(cerr!(line, PushValue, v, elem));
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
                return Err(cerr!(line, TakesTwo, name = "at", got = args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            // A user container: the projection's declared return type, with
            // the impl head's type variables solved from the receiver.
            if name == crate::project::AT {
                if let Some(t) = self.place_result(&at, "at", args, scope, fn_ret, line)? {
                    self.refuse_chained_projection(&args[0], scope, line)?;
                    // Record the nodes the site lowers through: the
                    // projection's body inlined here ([`record_desugar`]).
                    if self.recording() {
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
                    return Err(cerr!(line, MapKeyMismatch, key, k));
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
                other => return Err(cerr!(line, IndexReceiver, other)),
            };
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if matches!(i, Type::Err) {
                return Ok(Type::Err);
            }
            if i != Type::Int {
                return Err(cerr!(line, IndexType, i));
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
                return Err(cerr!(line, TakesOne, name, got = args.len()));
            }
            let at = self.expr(&args[0], scope, Some(&Type::Int), fn_ret)?;
            let at = self.base(&at);
            if !matches!(at, Type::Err) && at != Type::Int {
                return Err(cerr!(line, StreamAddress, name, at));
            }
            // The address says nothing of its element type, so the context must.
            let want = if name == "unboxStream" {
                "Stream<T>"
            } else {
                "Option<T>"
            };
            let Some(exp) = expected else {
                return Err(cerr!(line, StreamElementContext, name, want));
            };
            let ok = match name {
                "unboxStream" => matches!(self.base(exp), Type::Stream(_)),
                _ => crate::types::option_payload(&self.base(exp)).is_some(),
            };
            if !ok {
                return Err(cerr!(line, StreamElementMismatch, name, want, exp));
            }
            return Ok(exp.clone());
        }
        if name == "@pop" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesNone, name = "pop"));
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
                return Err(cerr!(line, SwapRemoveArity, got = args.len() - 1));
            }
            let elem = self.mut_array_receiver(&args[0], scope, line, "swapRemove")?;
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if !matches!(i, Type::Int | Type::Err) {
                return Err(cerr!(line, SwapRemoveIndex, i));
            }
            return Ok(elem);
        }
        // The one conversion from `SmallArray<T, N>` to `Array<T>`; an `Array`
        // receiver is accepted too, as a copy.
        if name == "@toArray" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesNone, name = "toArray"));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&at) {
                Type::SmallArray(inner, _) | Type::Array(inner) => (*inner).clone(),
                Type::Err => return Ok(Type::Err),
                other => return Err(cerr!(line, ToArrayReceiver, other)),
            };
            return Ok(Type::Array(Box::new(elem)));
        }
        // `x.copy()`: a deep copy the caller owns. Refused for a declared
        // `impl Owned` type (a field copy would run its release twice; `impl
        // Copy` overrides) and for a `Stream` (two consumers, one cursor). A
        // scalar is accepted, so one generic `x.copy()` serves every instance.
        if name == "@copy" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesNone, name = "copy"));
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
                return Err(cerr!(line, CopyOwned, declared));
            }
            if matches!(self.base(&t), Type::Stream(_)) {
                return Err(cerr!(line, CopyStream));
            }
            // A structural copy of a self-referring type never bottoms out (the
            // backends overflowed the stack), so it needs an `impl Copy`.
            if let Some(name) = crate::declared::self_referring(&t, &self.types) {
                return Err(cerr!(line, CopyRecursive, name));
            }
            return Ok(t);
        }
        if name == "@has" || name == "@remove" {
            let op = &name[1..];
            let mt = self.expr(&args[0], scope, None, fn_ret)?;
            let key_ty = match self.base(&mt) {
                Type::Map(k, _) => (*k).clone(),
                Type::Err => return Ok(Type::Err),
                other => return Err(cerr!(line, MapOpReceiver, op, other)),
            };
            if args.len() != 2 {
                return Err(cerr!(line, MapOpArity, op));
            }
            if name == "@remove" {
                // A receiver declared without `mut` is refused as for `pop`.
                if let Expr::Var { name: recv, .. } = &args[0] {
                    if self.lookup(scope, recv).is_some_and(|b| !b.mutable) {
                        return self.judged();
                    }
                } else {
                    return Err(cerr!(line, MapRemoveReceiver));
                }
            }
            let k = self.base(&self.expr(&args[1], scope, Some(&key_ty), fn_ret)?);
            if !matches!(k, Type::Err) && !self.key_fits(&k, &key_ty) {
                return Err(cerr!(line, MapKeyMismatch, key = key_ty, k));
            }
            self.prove_coercion(&args[1], &key_ty, line)?;
            return Ok(Type::Bool);
        }
        if let Some(target) = crate::types::numeric_conv_target(name) {
            if args.len() != 1 {
                return Err(cerr!(line, ConversionArity, name, got = args.len()));
            }
            let src = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(src, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(
                src,
                Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
            ) {
                return Err(cerr!(line, ConversionType, name, src));
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
                TargetAsTypeArg,
                name,
                was,
                suffix = match name {
                    "fromJson" => "s",
                    _ => "",
                }
            ));
        }
        // `contractOf(C)`: the argument is a contract name, not a value.
        if name == "contractOf" {
            if !*self.in_gen.borrow() {
                return Err(cerr!(line, GenOnly, name = "contractOf"));
            }
            if args.len() != 1 {
                return Err(cerr!(line, ContractOfArity, got = args.len()));
            }
            match &args[0] {
                Expr::Var { name: cn, .. } if self.contracts.contains_key(cn) => {
                    return Ok(Type::Named("ContractInfo".to_string()))
                }
                Expr::Var { name: cn, .. } => return Err(cerr!(line, ContractOfUnknown, cn)),
                _ => return Err(cerr!(line, ContractOfName)),
            }
        }
        if name == "toJson" {
            if args.len() != 1 {
                return Err(cerr!(line, ToJsonArity, got = args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self.types) {
                return Err(cerr!(line, ToJsonUncodable, off));
            }
            self.derive_sites.borrow_mut().push(crate::gen::Site {
                g: crate::loader::JSON_ENCODERS.to_string(),
                entry: Type::Fn(vec![at.clone()], Box::new(Type::Str)),
                ty: at,
                line,
            });
            return Ok(Type::Str);
        }
        // `derive(g, x)`: generator `g` writes a function for `x`'s type after
        // the check (`gen::derive`); the call site calls it. `toJson` above is
        // one.
        if name == "derive" {
            let g = match args {
                [Expr::Var { name: g, .. }, _] => g,
                _ => return Err(cerr!(line, DeriveArity)),
            };
            let arg = Type::Named("TypeArg".to_string());
            if !self.gen_fns.contains(g) || self.sigs.get(g) != Some(&(vec![arg], Type::Str)) {
                return Err(cerr!(line, DeriveUnknownGen, g));
            }
            let at = self.expr(&args[1], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self.types) {
                return Err(cerr!(line, DeriveUncodable, g, off));
            }
            self.derive_sites.borrow_mut().push(crate::gen::Site {
                g: g.clone(),
                entry: Type::Fn(vec![at.clone()], Box::new(Type::Str)),
                ty: at,
                line,
            });
            return Ok(Type::Str);
        }
        // A tagged template's hole desugars to `value(x)`.
        if name == "value" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesOne, name = "value", got = args.len()));
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
                    ValueType,
                    t,
                    hint = crate::types::show_hint(&written)
                ));
            }
            return Ok(Type::Named("Value".to_string()));
        }
        // Tagged-template desugar: a fixed array as a growable one.
        if name == "@list" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesOne, name = "@list", got = args.len()));
            }
            let a = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&a) {
                Type::ArrayN(inner, _) | Type::Array(inner) => return Ok(Type::Array(inner)),
                Type::Err => return Ok(Type::Err),
                other => return Err(cerr!(line, ListType, other)),
            }
        }

        if name == "Some" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesOne, name = "Some", got = args.len()));
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
                    return Err(cerr!(line, SomePayload, aty, want));
                }
                self.prove_coercion(&args[0], want, line)?;
                return Ok(Type::option(want.clone()));
            }
            return Ok(Type::option(aty));
        }

        // `Ok(x)` and `Err(e)` need the other type parameter from context.
        if name == "Ok" || name == "Err" {
            if args.len() != 1 {
                return Err(cerr!(line, TakesOne, name, got = args.len()));
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
                _ => return Err(cerr!(line, InferVariant, name)),
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
                return Err(cerr!(line, VariantPayload, name, aty, want_ty));
            }
            return Ok(Type::result(t, e));
        }

        if let Some(info) = self.variants.get(name) {
            let payload = info.payload.clone();
            if payload.is_empty() {
                return Err(cerr!(line, VariantNoArgs, name));
            }
            if args.len() != payload.len() {
                return Err(cerr!(
                    line,
                    VariantArity,
                    name,
                    want = payload.len(),
                    got = args.len()
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
                    return Err(cerr!(line, InferParam, tp, name = info.enum_name));
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
                return Err(cerr!(line, NeedsReceiver, name));
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
                                    AmbiguousBound,
                                    name,
                                    protocols = bounded
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
                            AmbiguousImpl,
                            name,
                            protocols = matching
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
                        return Err(cerr!(line, BoundAssociatedType, name, proto, a, t));
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
                    let (mparams, mret) = self
                        .sigs
                        .get(mangled.as_str())
                        .ok_or_else(|| cerr!(line, NotImplemented, recv, proto, name))?;
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
                _ => return Err(cerr!(line, NotImplemented, recv, proto, name)),
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
                if self.recording() {
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
                return Err(cerr!(line, ProjectionOnBound, name));
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
                    Some(g) => cerr!(line; g.rule(name)),
                    None => cerr!(line, UnknownFunction, name),
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
        // `toString` (`prelude::method_surface`), and other `@` names lose the
        // `@`, which no source can lex.
        let shown = crate::prelude::method_surface(name).trim_start_matches('@');
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
            if self.recording() {
                let decl = CallDecl {
                    shown: shown.to_string(),
                    params: params.to_vec(),
                    type_params: d.type_params.map_or(0, Vec::len),
                    recv: recv.is_some(),
                };
                *self.pending_call.borrow_mut() = Some(decl);
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
                        if self.recording() {
                            self.note_subst(d.key, &subst, type_params);
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
                    return Err(cerr!(line, InferParam, tp, name = shown));
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
                                return Err(cerr!(line; crate::types::needs_show(shown, concrete)));
                            }
                            return Err(cerr!(line, BoundUnsatisfied, shown, tp, b, concrete));
                        }
                    }
                }
            }
            let rty = crate::types::substitute(ret, &subst);
            // `fromJson<T>(s)` calls what `std/jsondec`'s generator writes for
            // `T`, and the solve is the one place `T` is known.
            if d.key == "fromJson" {
                if let Some(t) = subst.get("T") {
                    self.derive_sites.borrow_mut().push(crate::gen::Site {
                        g: crate::loader::JSON_DECODERS.to_string(),
                        ty: t.clone(),
                        line,
                        entry: Type::Fn(
                            params
                                .iter()
                                .map(|p| crate::types::substitute(p, &subst))
                                .collect(),
                            Box::new(rty.clone()),
                        ),
                    });
                }
            }
            // The one place a generic call's type arguments exist; recorded
            // for the backends.
            if self.recording() {
                self.note_subst(d.key, &subst, type_params);
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
                            return Err(cerr!(lline, InferLambdaReturn));
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
                if let Some(r) = &self.record {
                    let key = arg.id();
                    r.borrow_mut().node_types.insert(key, sig);
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
                let sig = self
                    .sigs
                    .get(vn)
                    .ok_or_else(|| cerr!(line, ArgNotFn, callee, arg = i + 1, vn))?;
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
                LambdaArity,
                got = params.len(),
                exp,
                want = ptys.len()
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
                FnValueUnsolved,
                name,
                exp,
                params = names.join(" or "),
                first
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
            return Err(cerr!(line, GenericFnValue, name));
        }
        if self.extern_fns.contains(name) {
            return Err(cerr!(line, ExternFnValue));
        }
        if self.gen_fns.contains(name) {
            return Err(cerr!(line, GenFnValue));
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
            err: Option<Rule>,
        }

        impl Captures<'_, '_> {
            fn is_capture(&self, n: &str, locals: &HashSet<String>) -> bool {
                !locals.contains(n) && self.ck.lookup(self.outer, n).is_some()
            }

            fn fail(&mut self, m: Rule) {
                if self.err.is_none() {
                    self.err = Some(m);
                }
            }
        }

        impl BodyVisit<'_> for Captures<'_, '_> {
            fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
                match s {
                    Stmt::Assign { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(Rule::LambdaAssignsCapture {
                            name: name.clone(),
                            line: line.to_string(),
                        });
                    }
                    Stmt::SetField { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(Rule::LambdaMutatesCapture {
                            name: name.clone(),
                            line: line.to_string(),
                        });
                    }
                    Stmt::IndexSet { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(Rule::LambdaStoresIntoCapture {
                            name: name.clone(),
                            line: line.to_string(),
                        });
                    }
                    Stmt::Drop { name, line, id: _ } if self.is_capture(name, locals) => {
                        self.fail(Rule::LambdaDropsCapture {
                            name: name.clone(),
                            line: line.to_string(),
                        });
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
                        id: _,
                    } => {
                        // Each argument is checked before it is walked, so the
                        // first violation in source order is reported.
                        let caps = self.ck.caps.get(name);
                        for (k, a) in args.iter().enumerate() {
                            if caps.and_then(|c| c.get(k)) == Some(&Capability::Consume) {
                                if let Expr::Var { name: vn, .. } = a {
                                    if self.is_capture(vn, locals) {
                                        self.fail(Rule::LambdaConsumesCapture {
                                            name: vn.clone(),
                                            line: line.to_string(),
                                        });
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
                        self.fail(Rule::LambdaNestsLambda {
                            line: line.to_string(),
                        });
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
            Some(m) => Err(cerr!(line; m)),
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
                    return Err(cerr!(line, ModifyAliased, path, fname, root));
                }
            }
        }
        match arg {
            Expr::Var { name: vn, .. } => {
                if self.lookup(scope, vn).is_some_and(|b| !b.mutable) {
                    return Err(cerr!(line, ModifyNotMut, fname, arg = i + 1, vn));
                }
            }
            _ => return Err(cerr!(line, ModifyTemporary, fname, arg = i + 1)),
        }
        if !matches!(aty, Type::Err) && !matches!(pty, Type::Err) && aty != pty {
            return Err(cerr!(line, ModifyExactType, fname, arg = i + 1, pty, aty));
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
                        Err(cerr!(line, ParamConflict, t, bound, aty))
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
                    None => Err(cerr!(line, ExpectedOption, aty)),
                }
            }
            _ if crate::types::result_payloads(pty).is_some() => {
                let (pt, pe) = crate::types::result_payloads(pty).expect("Result payloads");
                match crate::types::result_payloads(aty) {
                    Some((at, ae)) => {
                        self.unify(pt, at, subst, line)?;
                        self.unify(pe, ae, subst, line)
                    }
                    None => Err(cerr!(line, ExpectedResult, aty)),
                }
            }
            Type::App(pn, pargs) => match aty {
                Type::App(an, aargs) if pn == an && pargs.len() == aargs.len() => {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.unify(p, a, subst, line)?;
                    }
                    Ok(())
                }
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            // Collections bind `T` from the element type; an alias resolves
            // first.
            Type::Array(inner) => match crate::types::resolve(aty, self.types) {
                Type::Array(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            Type::ArrayN(inner, n) => match aty {
                Type::ArrayN(a, m) if m == n => self.unify(inner, a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            Type::Stream(inner) => match crate::types::resolve(aty, self.types) {
                Type::Stream(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            // `N` must match: integer arguments do not infer.
            Type::SmallArray(inner, n) => match crate::types::resolve(aty, self.types) {
                Type::SmallArray(a, m) if m == *n => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
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
                    _ => Err(cerr!(line, Expected, pty, aty)),
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
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            _ => {
                if !self.coercible(aty, pty) {
                    Err(cerr!(line, ArgExpected, pty, aty))
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
                ConstFailsPredicate,
                cv,
                name = decl.name,
                pred = pred_summary(decl.predicate.as_ref().unwrap())
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
            return Err(cerr!(line, ArrayOpReceiver, op));
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
        Expr::Int(n, _) => n.to_string(),
        Expr::Byte(b, _) => b.to_string(),
        Expr::Float(x, _) => x.to_string(),
        Expr::Bool(b, _) => b.to_string(),
        Expr::Str(s, _) => format!("{s:?}"),
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
        Expr::Int(n, _) => Some(literal_value(*n)),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n, _) => Some(-literal_value(*n)),
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
    let Expr::Byte(b, _) = lit else {
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
                    cerr!(g.line, GenImpure, name = g.name, reason)
                } else {
                    let chain = path.join(" -> ");
                    cerr!(g.line, GenImpureVia, name = g.name, cur, chain, reason)
                };
                out.push(msg.in_file(g.module.clone()));
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
                    self.fail(cerr!(line, GlobalReadsItself, own_name));
                } else {
                    self.fail(cerr!(line, GlobalReadsLater, own_name, name));
                }
                false
            }
            Expr::Call { name, .. }
                // Only imported modules are initialized first, so a
                // same-module function is forbidden with the externs.
                if self.forbidden.contains(name)
                    || matches!(self.fn_module.get(name), Some(m) if m == self.own_module) =>
            {
                self.fail(cerr!(line, GlobalCalls, own_name, name));
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

/// Collects what a body calls and every name it reads or assigns that is not a
/// local: the functions and module state it reaches.
struct Refs<'a>(&'a mut HashSet<String>);

impl BodyVisit<'_> for Refs<'_> {
    fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
        if let Stmt::Assign { name, .. }
        | Stmt::SetField { name, .. }
        | Stmt::IndexSet { name, .. } = s
        {
            if !locals.contains(name) {
                self.0.insert(name.clone());
            }
        }
    }

    fn expr(&mut self, e: &Expr, locals: &HashSet<String>) -> bool {
        match e {
            Expr::Call { name, .. } | Expr::TryConstruct { name, .. } => {
                self.0.insert(name.clone());
            }
            Expr::Var { name, .. } if !locals.contains(name) => {
                self.0.insert(name.clone());
            }
            _ => {}
        }
        true
    }
}

/// Returns the functions and module state `f` reaches (see [`Refs`]).
pub fn fn_refs(f: &Function) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut locals = f.params.iter().map(|p| p.name.clone()).collect();
    body_block(&f.body, &mut locals, &mut Refs(&mut out));
    out
}

/// Returns the functions and module state `e` reaches (see [`Refs`]).
pub fn expr_refs(e: &Expr) -> HashSet<String> {
    let mut out = HashSet::new();
    body_expr(e, &HashSet::new(), &mut Refs(&mut out));
    out
}

#[cfg(test)]
mod tests {

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

        let r = record(&p);

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
            let q = parse(
                lex("fn main() -> Int64 {
    return 0
}
")
                .unwrap(),
            )
            .unwrap();
            let other = recorded(&q);
            assert_ne!(
                other.node_types, got.node_types,
                "a record served for the wrong program"
            );
        }

        let after = recorded(&p);
        assert_eq!(after.node_types, want.node_types);
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

    /// A file with exports is a library and needs no `main`; a file with
    /// neither is refused.
    #[test]
    fn library_modules_do_not_need_main() {
        assert!(check_src("export fn double(x: Int64) -> Int64 { return x * 2 }").is_ok());
        assert!(check_src("export type Age = Int64 where value >= 18").is_ok());
        let e = check_src("fn helper(x: Int64) -> Int64 { return x }").unwrap_err();
        assert!(e.contains("no `main` function found"), "{e}");
    }

    // The clock and the entropy seed are host effects; the seeded PRNG is
    // arithmetic and usable anywhere, generators included.

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

    /// Every atom the generation fence refuses is a name the compiler owns, so
    /// a deleted builtin cannot leave a stale row in the lattice's `gen`
    /// column.
    #[test]
    fn comptime_forbidden_names_are_reserved() {
        for (n, _) in crate::effects::atoms() {
            if crate::effects::gen_allows(n) {
                continue;
            }
            // A name no source can spell needs no reservation: the runtime's
            // primitives and the `@` internals, log levels included.
            if n.contains('$') || n.starts_with('@') {
                continue;
            }
            assert!(
                RESERVED.contains(&n) || crate::trap::host_boundary_extern(n).is_some(),
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
    fn constrained_generic_operators() {
        let src = "fn max<T: Ord>(a: T, b: T) -> T { if a > b { return a; } return b; } \
                   fn main() -> Int64 { return max(3, 9); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn accepts_generic_record() {
        let src = "type Box<T> = { value: T }; \
                   fn open<T>(b: Box<T>) -> T { return b.value; } \
                   fn main() -> Int64 { let n = Box { value: 41 }; return open(n); }";
        assert!(check_src(src).is_ok());
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
    fn to_string_renders_scalar_receivers() {
        // `x.toString()` on Int64, a sized int, Float64, Bool, and String.
        let src = "fn main() -> Int64 { \
                       let a = (42).toString(); let b: Int8 = 3; let c = b.toString(); \
                       let d = (1.5).toString(); let e = true.toString(); let f = \"hi\".toString(); \
                       return a.byteLength + c.byteLength + d.byteLength + e.byteLength + f.byteLength; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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
    fn global_inferred_and_annotated_types_check() {
        let ok = "let mut hits = 0\n\
                  let banner: String = \"hi\"\n\
                  fn bump() -> Int64 { hits = hits + 1 return hits }\n\
                  fn name() -> String { return banner }\n\
                  fn main() -> Int64 { return bump() }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
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
    fn contract_of_types_as_contract_info() {
        let src = "contract Page { let head: String = \"\" }\n\
                   gen fn g(p: String) -> String { let c = contractOf(Page)  return c.name }\n\
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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

    /// A generic call passes its parameter type as the argument's expectation,
    /// so `Ok(1)` types from the parameter.
    #[test]
    fn a_generic_call_types_a_literal_from_its_parameter() {
        let src = "fn unwrapOr<T>(r: Result<T, String>, d: T) -> T { \
                   return match r { Ok(v) => v, Err(m) => d } } \
                   fn main() -> Int64 { return unwrapOr(Ok(1), 5) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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
}
