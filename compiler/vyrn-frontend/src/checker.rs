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
use crate::rules::{rule, DeclName, Hole, Rule};
use crate::types::mentions_param as type_mentions_param;
use crate::types::walk_type;
use crate::types::Decls;
use crate::types::FALLIBLE;

pub mod recheck;
use recheck::Text;

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

/// Whether a body is checked as generation code: its own `gen fn` marker, or a
/// whole-program generator host.
fn in_gen_of(f: &Function, host: Host) -> bool {
    f.is_gen || host.gen
}

/// The atom-stream primitives a generator host's decoders call: one starts a
/// reflected answer, two take the next atom. `vyrn-codegen` lowers them. They
/// exist only in a generator host ([`Host::gen`]), so no program can name them.
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
fn gen_host_primitive(name: &str, argc: usize, host: Host) -> Option<Type> {
    if !host.gen {
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
    /// Whether the source writes the type: an annotated `let`, a parameter.
    pub annotated: bool,
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

/// The root module's bindings and what each of its name occurrences names.
#[derive(Debug, Clone, Default)]
pub struct Binders {
    /// Every binding, in source order ([`local_index`]).
    pub locals: Vec<LocalBinding>,
    /// Each name occurrence a root body resolved, by its `(line, col)`: the
    /// position of the local binder it names, or `None` for a name past the
    /// locals (module state, a function, a variant). Empty unless the editor
    /// asked ([`with_uses`]).
    pub uses: Uses,
}

/// Occurrence position to binder position ([`Binders::uses`]).
pub type Uses = HashMap<(usize, usize), Option<(usize, usize)>>;

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
            annotated: declared.is_some(),
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

/// Names the compiler owns: builtin functions, builtin type names and the sum
/// constructors. A top-level declaration may not take one. The loader reads it
/// too, so a user `fn at` does not claim the builtin `at` in every `std/` module.
pub const RESERVED: &[&str] = &[
    "print",
    "Some",
    "None",
    "Ok",
    "Err",
    "match",
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
    "schemaOf",
    "contractOf",
    "jsonSchema",
    "toJson",
    "fromJson",
    "derive",
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
    "Int64",
    "Int32",
    "Int16",
    "Int8",
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
    /// A name a desugar writes; the hint names the sugar, then the import.
    Desugared {
        module: &'static str,
        sugar: &'static str,
    },
}

impl Gone {
    /// Returns the rule a program that wrote `name` breaks.
    pub fn rule(&self, name: &str) -> Rule {
        match self {
            Gone::Module(module) => rule!(GoneModule, name, module),
            Gone::Desugared { module, sugar } => rule!(GoneDesugared, name, module, sugar),
        }
    }
}

/// Names a program may write that do not resolve, and what to write instead.
/// [`Checker::call`] reads it only for a name that does not resolve.
///
/// A name here must not be in [`RESERVED`]
/// (`every_moved_name_is_gone_from_reserved`).
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
];

/// Returns where a name went, or `None` for one that was never a builtin.
fn moved_to_std(name: &str) -> Option<&'static Gone> {
    MOVED_TO_STD
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, g)| g)
}

use crate::types::INT32;

/// Returns the diagnostics, every `derive` site, the refused set, the root's
/// bindings and the record. The refused set holds the functions and module
/// state the diagnostics all belong to, so every other body is typed; it is
/// `None` when a refusal stands anywhere else.
pub fn check_accum_with_sites(program: &Program) -> (Appended, Binders, Recorded) {
    let (out, binders, _, derived, refused, made) = check_accum_inner(program, true, 0, &[]);
    ((out, derived, refused), binders, made.unwrap_or_default())
}

/// Checks `program`, whose functions before `at` passed a check alone, typing
/// only the bodies from `at` on. Returns the diagnostics, the `derive` sites of
/// those bodies, and the refused set, as a whole check would, and the record of
/// those bodies, which [`Recorded::extend`] adds to the earlier check's.
///
/// A body is typed against the declarations alone and reads them by name, so
/// an earlier body keeps its verdict.
pub fn check_appended(
    program: &Program,
    at: usize,
    earlier: &[(String, String)],
) -> (Appended, Recorded) {
    let (out, _, _, derived, typed, made) = check_accum_inner(program, true, at, earlier);
    ((out, derived, typed), made.unwrap_or_default())
}

pub type Appended = (
    Vec<Diagnostic>,
    Vec<crate::gen::Site>,
    Option<HashSet<String>>,
);

/// Whether an impl for `ty` can dispatch: a value of `ty` carries the name
/// the impl is keyed by. A validated scalar erases to its base, so it carries
/// none.
fn dispatches_on(ty: &Type, types: &HashMap<String, (DeclId, TypeDecl)>) -> bool {
    match ty {
        Type::Int | Type::Bool | Type::Str => true,
        // The two built-in sums, under either spelling.
        _ if crate::types::is_sum_alias(ty) => true,
        Type::Named(n) | Type::App(n, _) => matches!(
            types.get(n).map(|(_, d)| &d.base),
            Some(Type::Enum(_) | Type::Record(_))
        ),
        _ => false,
    }
}

/// An impl head as written (`impl<T> Show for Option<T>`), for the overlap
/// diagnostic.
fn render_impl_head(imp: &crate::ast::ImplBlock) -> Hole {
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
    Hole::Parts(vec![
        Hole::Text(format!("impl{binder} {} for ", imp.protocol)),
        Hole::Type(imp.ty.clone()),
    ])
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
) -> Hole {
    let word = |c: Capability| match c {
        Capability::Read => "",
        Capability::Modify => "modify ",
        Capability::Consume => "consume ",
    };
    let mut parts = vec![Hole::Text(format!("fn {name}({}self", word(recv)))];
    for (i, t) in params.iter().enumerate() {
        let c = caps.get(i).copied().unwrap_or(Capability::Read);
        parts.push(Hole::Text(format!(", {}", word(c))));
        parts.push(Hole::Type(t.clone()));
    }
    parts.push(Hole::Text(") -> ".to_string()));
    parts.push(Hole::Type(ret.clone()));
    Hole::Parts(parts)
}

/// Type-checks `program` and returns every diagnostic, the root's bindings
/// by `(line, name)`, the stored-function facts, the `derive` sites, the
/// refused set and, when `recording`, the record.
///
/// Accumulation is bounded: the top-level loops over `program.functions` and
/// `program.type_decls` push-and-continue, so an error in one function or type
/// does not suppress errors in the others. Inside a single function body the
/// check is first-error per statement.
#[allow(clippy::type_complexity)]
fn check_accum_inner(
    program: &Program,
    recording: bool,
    bodies_from: usize,
    earlier: &[(String, String)],
) -> (
    Vec<Diagnostic>,
    Binders,
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
    let mut types: HashMap<String, (DeclId, TypeDecl)> = HashMap::new();
    for (i, t) in program.type_decls.iter().enumerate() {
        let name = DeclName(&t.name);
        // A declared type keys its impls by its name (`types::type_key`), and
        // these are the keys of built-in types.
        if matches!(
            t.name.as_str(),
            "Int64" | "Bool" | "Unit" | "String" | "Option" | "Result"
        ) {
            out.push(cerr!(t.line, RedefinesBuiltinType, name).in_file(t.module.clone()));
            continue;
        }
        if types.contains_key(&t.name) {
            out.push(cerr!(t.line, TypeDefinedTwice, name).in_file(t.module.clone()));
            continue;
        }
        types.insert(t.name.clone(), (DeclId::nth(DeclKind::Type, i), t.clone()));
    }

    // 1b. Collect enum variants into a global constructor table.
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    let mut ids = (0..).map(|i| DeclId::nth(DeclKind::Variant, i));
    for t in &program.type_decls {
        if let Some(vs) = crate::types::declared_variants(&t.base) {
            for (v, id) in vs.iter().zip(&mut ids) {
                if RESERVED.contains(&v.name.as_str()) {
                    out.push(cerr!(t.line, ReservedName, name = v.name));
                    continue;
                }
                if variants.contains_key(&v.name) {
                    out.push(cerr!(t.line, EnumVariantDefinedTwice, name = v.name));
                    continue;
                }
                if let Some((_, ty)) = types.get(&v.name) {
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
                        id,
                        enum_name: t.name.clone(),
                        payload: v.payload.clone(),
                    },
                );
            }
        }
    }

    // 2. Collect function signatures (forward references allowed).
    let sigs: Vec<(Vec<Type>, Type)> = (program.functions.iter())
        .map(|f| {
            (
                f.params.iter().map(|p| p.ty.clone()).collect(),
                f.ret.clone(),
            )
        })
        .collect();
    let mut fn_decls: HashMap<String, DeclId> = HashMap::new();
    for (i, f) in program.functions.iter().enumerate() {
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
        if fn_decls.contains_key(&f.name) {
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
        fn_decls.insert(f.name.clone(), DeclId::nth(DeclKind::Fn, i));
    }
    let all_bounds: HashMap<String, HashMap<String, Vec<String>>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.type_bounds.clone()))
        .collect();
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
    // The program's own declaration of a prelude protocol's name replaces it.
    let protocol_decls: HashMap<&str, &crate::ast::ProtocolDecl> = (crate::prelude::protocols())
        .iter()
        .chain(&program.protocols)
        .map(|p| (p.name.as_str(), p))
        .collect();
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

        // Rendering calls `show` whatever declares `Show`, so a program that
        // replaces the prelude's declaration must still return a String.
        if imp.protocol == crate::types::SHOW
            && program.protocols.iter().any(|p| p.name == imp.protocol)
        {
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
        match crate::types::type_key(&imp.ty) {
            Some(key) if dispatches_on(&imp.ty, &types) => {
                // A second impl for one (protocol, type constructor) key is
                // refused at its declaration, naming the first. Not "overlaps":
                // `Option<Int64>` and `Option<String>` are disjoint, but
                // dispatch keys on the constructor (`types::type_key`).
                let first = program.impls.get(&imp.protocol, &key);
                if let Some(first) = first.filter(|f| !std::ptr::eq(*f, imp)) {
                    out.push(cerr_at!(
                        imp.line,
                        imp.head_span(),
                        ImplHeadCollides,
                        head = render_impl_head(imp),
                        prev = render_impl_head(first),
                        prev_line = first.line,
                        key = DeclName(&key),
                        protocol = imp.protocol
                    ));
                }
            }
            _ => {
                let named_scalar = match &imp.ty {
                    Type::Named(n) | Type::App(n, _) => types
                        .get(n)
                        .map(|(_, d)| &d.base)
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

    let cx = Cx {
        host: program.host,
        functions: &program.functions,
        fn_decls: &fn_decls,
        sigs: &sigs,
        types: &types,
        contracts: &contracts,
        variants: &variants,
        all_bounds: &all_bounds,
        protocol_methods: &protocol_methods,
        protocol_places: &protocol_places,
        impls: &program.impls,
        expansions: &program.expansions,
        shadows: &program.surface_shadows,
        extern_fns: &extern_fns,
        gen_fns: &gen_fns,
        record_reads: recording && program.session.get().is_some(),
        record_uses: USES.with(|u| u.get()),
    };
    let mut checker = Checker::new(&cx, recording);
    checker.recheck = (program.session.get())
        .filter(|_| recording)
        .map(|s| recheck::Session::open(program, s));

    // 2b. Module state, in declaration order. A failed global still binds, as
    //     `Err`, so bodies that read it do not cascade "unknown variable".
    in_bodies += checker.check_globals(program, &mut out, &mut refused);

    // 3. Validate each type decl (base kind, referenced-type existence, predicate).
    for (i, t) in (0..).zip(&program.type_decls) {
        checker.reading(SourceBody::TypeDecl(i));
        if let Err(s) = checker.unit(|| checker.check_type_decl(t)) {
            out.extend(s);
        }
    }
    checker.reader.set(None);

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
    let sig = |name: &str| fn_decls.get(name).map(|d| &sigs[d.index()]);
    let has_served_handle = sig("handle").is_some_and(|(params, ret)| {
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
    match sig("main") {
        None if !is_library => out.push(cerr!(0, NoMain)),
        None => {}
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => {
            out.push(cerr!(0, MainSignature))
        }
        _ => {}
    }

    // 5. Check functions, each independently, on every thread. A body reads
    //    another only through `sigs`, so none waits for another. A worker
    //    types into a checker of its own, and the merge takes each body's
    //    diagnostics and record in source order, as one thread would.
    let typing = crate::prof::phase("check: bodies");
    let bodies: Vec<(u32, &Function)> = (0..).zip(&program.functions).skip(bodies_from).collect();
    let replayed: Vec<Option<Typed>> = (bodies.iter())
        .map(|&(i, f)| checker.replayed(SourceBody::Fn(i), Text::Fn(f)))
        .collect();
    let fresh: Vec<(u32, &Function)> = (bodies.iter().zip(&replayed))
        .filter(|(_, r)| r.is_none())
        .map(|(b, _)| *b)
        .collect();
    let globals = checker.globals.borrow().clone();
    let typed = crate::par::in_parallel(
        &fresh,
        |(_, f)| weight(f),
        || {
            let c = Checker::new(&cx, recording);
            *c.globals.borrow_mut() = globals.clone();
            c
        },
        |c, &(i, f)| c.signature_and_body(SourceBody::Fn(i), f),
    );
    // One allocation for the types the bodies add, not one per doubling.
    if let Some(r) = &mut checker.acc.borrow_mut().record {
        let all = typed.iter().chain(replayed.iter().flatten());
        let n = (all.filter_map(|t| t.record.as_ref())).map(|p| p.node_types.len());
        r.node_types.reserve(n.sum());
    }
    let mut typed = typed.into_iter();
    for (&(i, f), r) in bodies.iter().zip(replayed) {
        let t = r.unwrap_or_else(|| {
            let mut t = typed.next().expect("one typed body per body not replayed");
            checker.store(SourceBody::Fn(i), Text::Fn(f), &mut t);
            t
        });
        if !t.diags.is_empty() {
            refused.insert(f.name.clone());
            in_bodies += t.diags.len();
        }
        out.extend(checker.absorb(t));
    }
    drop(typing);

    // 6. Projection, test and bench bodies. A test or bench is a Unit body
    //    under an unspellable name (`test@<index>`), absent from `fn_decls`, so no
    //    code can call it.
    if bodies_from == 0 {
        check_places(&checker, program, &mut out);
        let (tests, benches) = (&program.tests, &program.benches);
        let (in_test, in_bench) = (&checker.in_test, &checker.in_bench);
        check_named_blocks(&checker, tests, "test", in_test, SourceBody::Test, &mut out);
        check_named_blocks(
            &checker,
            benches,
            "bench",
            in_bench,
            SourceBody::Bench,
            &mut out,
        );
        checker.reader.set(None);
    }

    let mut acc = checker.acc.take();
    let mut effects = std::mem::take(&mut acc.stored);

    // 7. Comptime purity of every `gen fn` and its callees, after
    //    the body checks so a generator's type errors come first.
    let dispatched = earlier.iter().chain(&effects.dispatched);
    check_comptime_purity(program, &effects.through, dispatched, &mut out);
    for d in &mut out {
        d.speak(&program.spellings);
    }

    // The outputs name each type parameter as written, not as one solve
    // renamed it ([`Checker::rename_apart`]). The record keeps the renamed
    // names ([`Recorded::node_substs`]).
    let written = |t: &mut Type| {
        if let Some(w) = crate::types::written_params(t) {
            *t = w;
        }
    };
    for s in effects.sources.iter_mut().chain(&mut effects.arg_sources) {
        written(&mut s.sig);
        for t in s.lambda.iter_mut().flat_map(|l| &mut l.nested_sigs) {
            written(t);
        }
    }
    effects.calls.iter_mut().for_each(|(_, t)| written(t));
    acc.binders.values_mut().for_each(written);
    let mut derived = acc.derive;
    for site in &mut derived {
        written(&mut site.ty);
        written(&mut site.entry);
    }
    let binders = Binders {
        locals: local_index(program, &acc.binders),
        uses: acc.uses,
    };
    let typed = (in_bodies == out.len()).then_some(refused);
    let mut seen = HashSet::new();
    let mut reads = acc.reads;
    reads.retain(|r| seen.insert(r.clone()));
    let record = acc.record.map(|r| Recorded {
        stored: effects.clone(),
        reads,
        ..r
    });
    (out, binders, effects, derived, typed, record)
}

/// What typing adds to a checker. [`Checker::acc`] holds the sum over every
/// body typed so far; [`Checker::taken`] cuts one body's part out and
/// [`Checker::absorb`] adds it back. A field is listed here and in
/// [`Typed::extend`], nowhere else.
#[derive(Clone, Default)]
struct Typed {
    /// A body's diagnostics. The accumulator never holds any: `errors` does.
    diags: Vec<Diagnostic>,
    /// The record [`record`] asks for, or `None`, so the editor's keystroke
    /// path does not pay for it.
    record: Option<Recorded>,
    /// The read rows, in the order read ([`Recorded::reads`]).
    reads: Vec<(SourceBody, Key)>,
    /// The type of every root-module binding, keyed by binder position, for
    /// editor hover. A binding in a statement that did not type has no row.
    binders: HashMap<(usize, usize), Type>,
    /// What each root-module name occurrence names ([`Binders::uses`]), first answer kept.
    uses: Uses,
    stored: StoredFnEffects,
    derive: Vec<crate::gen::Site>,
}

impl Typed {
    /// An empty accumulation that records when `recording`.
    fn empty(recording: bool) -> Typed {
        Typed {
            record: recording.then(Default::default),
            ..Default::default()
        }
    }

    /// Adds `t` after `self`. A binder's first answer stands: a desugar types
    /// a copy again.
    fn extend(&mut self, t: Typed) {
        self.diags.extend(t.diags);
        if let (Some(r), Some(part)) = (&mut self.record, t.record) {
            r.extend(part);
        }
        self.reads.extend(t.reads);
        for (at, ty) in t.binders {
            self.binders.entry(at).or_insert(ty);
        }
        for (at, to) in t.uses {
            self.uses.entry(at).or_insert(to);
        }
        self.stored.extend(t.stored);
        self.derive.extend(t.derive);
    }
}

/// The expressions `f`'s body holds, the measure
/// [`crate::par::in_parallel`] orders by.
fn weight(f: &Function) -> usize {
    crate::body_scope_descent!(Count, count_block, count_stmt, count_expr);
    struct N(usize);
    impl Count<'_> for N {
        const SCOPED: bool = false;

        fn expr(&mut self, _: &Expr, _: &HashSet<String>) -> bool {
            self.0 += 1;
            true
        }
    }
    let mut n = N(0);
    count_block(&f.body, &mut HashSet::new(), &mut n);
    n.0
}

/// Checks every projection body as a function body, plus three rules of its
/// own. It is inlined, so it returns once, as its last statement. It returns
/// a place, because a value would be a hidden copy. The place is
/// rooted in `self` or a parameter, which the access site owns.
fn check_places(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {
    for (i, (_, f)) in (0..).zip(crate::project::all(program)) {
        checker.body(SourceBody::Place(i), Text::Fn(f), out, |out| {
            let mut push = |d: Diagnostic| out.push(d.in_file(f.module.clone()));
            if crate::project::is_optional(f) {
                check_optional_place(checker, f, &mut push);
                return;
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
                return;
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
                return;
            }
            // A `modify` result rooted at a parameter that is not `modify` is the
            // argument's value at the site, with nowhere to write.
            let modifies =
                f.params.first().map(|p| p.capability) == Some(crate::ast::Capability::Modify);
            let root = crate::project::place_root(y);
            let read_root = modifies
                && root.as_ref().is_some_and(|r| {
                    f.params
                        .iter()
                        .any(|p| p.name == *r && p.capability != crate::ast::Capability::Modify)
                });
            if root.is_none() || read_root {
                push(cerr_at!(
                    f.line,
                    f.name_span(),
                    ProjectionReturnsValue,
                    name = f.name
                ));
                return;
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
        });
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
    if crate::project::place_root(y).is_none() {
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
    // A `let` that borrows from no root shadows a root of its name.
    for s in prologue {
        if let Stmt::Let { name, value, .. } = s {
            if let_borrows_from(value, &roots) {
                roots.insert(name.clone());
            } else {
                roots.remove(name);
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
    if let Some(r) = crate::project::place_root(e) {
        return roots.contains(&r);
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
        crate::project::place_root(body).is_some_and(|r| binders.contains(&r.as_str()))
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
    body: fn(u32) -> SourceBody,
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
        checker.body(body(i as u32), Text::Block(t), out, |out| {
            // The head is synthetic but the body is the real node, so what the
            // checker records lands on the nodes `own` and the lowering walk.
            let synthetic = Function {
                module: t.module.clone(),
                line: t.line,
                ..Function::synth(format!("{noun}@{i}"), Vec::new(), Type::Unit, Vec::new())
            };
            if let Err(s) = checker.function_body(&synthetic, &t.body) {
                out.push(s.in_file(t.module.clone()));
            }
            for s in checker.errors.borrow_mut().drain(..) {
                out.push(s.in_file(t.module.clone()));
            }
        });
    }
    *host.borrow_mut() = false;
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
    /// refines nothing later, so the solution governs its whole subtree. A
    /// parameter goes by its renamed-apart name (`T'n`,
    /// [`crate::ast::written_param`]), as the subtree's types name it, so a
    /// caller's `T` stays apart from the callee's.
    pub node_substs: HashMap<NodeId, (String, Vec<(String, Type)>)>,
    /// A declared call whose arity, type-argument count or argument the typed
    /// judgment refuses, keyed by the [`Expr::Call`] node.
    pub calls: HashMap<NodeId, CallDecl>,
    /// The stored-function-value collection the `--workers` gate reads.
    pub stored: StoredFnEffects,
    /// Each source body's name lookups, each key once per body, in the order
    /// read: a function, module state, a type declaration and an enum
    /// variant ([`Checker::read`]). A body the check typed again repeats its
    /// rows.
    pub reads: Vec<(SourceBody, Key)>,
    /// Each body a [`recheck`] entry holds, replayed or stored by this check,
    /// with the entry's serial. A serial names one record of one body, so a
    /// reader keyed by it may reuse what it derived from that record.
    pub entries: Vec<(SourceBody, u64)>,
}

impl Recorded {
    /// Adds the record of the bodies a synthesis appended ([`check_appended`]).
    /// The earlier bodies' entries stand, because a body is typed against the
    /// declarations alone. The tail's module-state initializers were typed
    /// again, so its stored sources repeat theirs; every reader treats the
    /// sources as a set.
    pub fn extend(&mut self, tail: Recorded) {
        self.node_types.extend(tail.node_types);
        self.joins.extend(tail.joins);
        self.node_substs.extend(tail.node_substs);
        self.calls.extend(tail.calls);
        self.stored.extend(tail.stored);
        self.reads.extend(tail.reads);
        self.entries.extend(tail.entries);
    }
}

/// One pass that returns the diagnostics, the root's bindings and the record.
fn recording_check(program: &Program) -> (Vec<Diagnostic>, Binders, Recorded) {
    let (diags, binders, _, _, _, made) = check_accum_inner(program, true, 0, &[]);
    (diags, binders, made.unwrap_or_default())
}

/// Checks `program` and returns the record. Diagnostics are dropped: the
/// caller has already checked.
pub fn record(program: &Program) -> Recorded {
    recording_check(program).2
}

/// Checks the program as [`record`] does and returns the diagnostics and the
/// root's bindings. It never reuses a body.
pub fn check_accum_recording(program: &Program) -> (Vec<Diagnostic>, Binders) {
    let (diags, binders, _) = recording_check(program);
    (diags, binders)
}

thread_local! {
    /// Set by [`with_uses`].
    static USES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with this thread's checks recording [`Binders::uses`]. The editor
/// asks; `vyrn check` does not, and a row costs a lookup per name.
pub fn with_uses<T>(f: impl FnOnce() -> T) -> T {
    let outer = USES.with(|u| u.replace(true));
    let r = f();
    USES.with(|u| u.set(outer));
    r
}

/// What every body is typed against: the declarations and tables steps 1 to 2
/// of [`check_accum_inner`] build. Shared by the threads that type bodies.
struct Cx<'a> {
    functions: &'a [Function],
    /// The first function declared under each name that the checker accepts;
    /// [`Checker::resolve_fn`] reads it.
    fn_decls: &'a HashMap<String, DeclId>,
    /// Each function's parameter types and result, by [`DeclId::index`].
    sigs: &'a [(Vec<Type>, Type)],
    /// The type declarations by name with their ids, which the checker reads
    /// as [`crate::types::Decls`], recording each lookup.
    types: &'a HashMap<String, (DeclId, TypeDecl)>,
    contracts: &'a HashMap<String, ContractDecl>,
    variants: &'a HashMap<String, VariantInfo>,
    /// Function name to (type parameter to bounds).
    all_bounds: &'a HashMap<String, HashMap<String, Vec<String>>>,
    /// Method name to (protocol, signature).
    protocol_methods: &'a HashMap<String, Vec<(String, MethodSig)>>,
    /// Projection names protocols declare.
    protocol_places: &'a std::collections::HashSet<String>,
    /// Every `impl` block, by type key.
    impls: &'a crate::types::Impls,
    /// The program's projection expansions, which the checker makes.
    expansions: &'a crate::project::Expansions,
    /// The program's [`Host`].
    host: Host,
    shadows: &'a std::collections::HashSet<(Option<String>, String)>,
    /// `extern` functions, which cannot be function values.
    extern_fns: &'a std::collections::HashSet<String>,
    /// `gen fn`s, which cannot be function values.
    gen_fns: &'a std::collections::HashSet<String>,
    /// Whether a recording check records read rows ([`Recorded::reads`]): yes
    /// in a host with a session ([`crate::session`]), which rechecks per
    /// function. `vyrn check` has none, and a row costs a push per name lookup.
    record_reads: bool,
    /// Whether the check records [`Binders::uses`] ([`with_uses`]), read once
    /// on the calling thread.
    record_uses: bool,
}

/// One body's typing state over a [`Cx`], which it reads through `Deref`.
struct Checker<'a> {
    cx: &'a Cx<'a>,
    /// The function being checked ([`Checker::enter`]).
    frame: RefCell<Frame>,
    /// Scope depths at each enclosing `region` entry. A binding below the top
    /// depth is outer: a heap value assigned to it would dangle when the
    /// region frees.
    region_floor: RefCell<Vec<usize>>,
    /// Everything typing has added so far ([`Typed`]).
    acc: RefCell<Typed>,
    /// A body's statement errors, cleared per function. A failed `let` or
    /// `for` binds its name to [`Type::Err`] so later uses do not cascade.
    errors: RefCell<Vec<Diagnostic>>,
    /// Module state, filled in declaration order before any body is checked.
    /// [`Scope`] reads it when its frames run out
    /// ([`Checker::resolve_global`]).
    globals: RefCell<HashMap<String, (DeclId, Binding)>>,
    /// Inside a `test` body: `assert` and `assertEq` are legal.
    in_test: RefCell<bool>,
    /// Inside a `bench` body: `blackBox` is legal, as in a `test`.
    in_bench: RefCell<bool>,
    /// Whether the unit [`Checker::unit`] runs typed a node [`Checker::judged`],
    /// or read a name typed [`Type::Err`].
    unknown: std::cell::Cell<bool>,
    /// The line of the enclosing statement, for a literal, which carries none.
    stmt_line: RefCell<usize>,
    /// The source body being checked, when the check records: every name
    /// lookup then records a read row for it ([`Checker::reading`]).
    reader: std::cell::Cell<Option<SourceBody>>,
    /// The substitution the innermost generic call just solved, for the
    /// [`Checker::expr`] wrapper that knows the call node's address. A nested
    /// call consumes and clears it before its caller writes one.
    pending_subst: RefCell<Option<(String, Vec<(String, Type)>)>>,
    /// The declaration of the call [`Checker::check_declared_call`] just typed
    /// `Err`, for the same wrapper.
    pending_call: RefCell<Option<CallDecl>>,
    /// The serial [`Checker::rename_apart`] gives the next instantiation.
    fresh: std::cell::Cell<u32>,
    /// The per-body reuse, on the loading thread's checker when the host
    /// rechecks per function ([`Cx::record_reads`]).
    recheck: Option<recheck::Session<'a>>,
}

/// What the checker knows of the function it is typing. [`Checker::enter`]
/// replaces it whole at the start of a body and nothing restores it, so
/// between two bodies it is the last one's.
#[derive(Default)]
struct Frame {
    /// Inside a `gen fn` body: `Code` and the code-quote builtins are legal.
    /// A `gen fn` body is never emitted.
    in_gen: bool,
    /// The module being checked; `None` is the root. With `shadows`
    /// ([`ast::Program::surface_shadows`], filled by the loader), it decides
    /// whether `render`, `rawAt`, `raw` or `lex` is the builtin or a function
    /// this module declares or imports.
    here: Option<String>,
    /// Whether the module is the root. Only the root is indexed: two modules
    /// share a position.
    in_root: bool,
    /// The function's type parameters and their bounds.
    bounds: HashMap<String, Vec<String>>,
    /// The function's name.
    name: String,
}

impl Frame {
    fn of(f: &Function, host: Host) -> Frame {
        Frame {
            in_gen: in_gen_of(f, host),
            here: f.module.clone(),
            in_root: f.module.is_none(),
            bounds: f.type_bounds.clone(),
            name: f.name.clone(),
        }
    }
}

impl<'a> std::ops::Deref for Checker<'a> {
    type Target = Cx<'a>;

    fn deref(&self) -> &Cx<'a> {
        self.cx
    }
}

/// A declaration a call is checked against: a user function, a seeded builtin
/// row, an impl method or a protocol member. [`Checker::check_declared_call`]
/// reads it.
struct DeclaredCall<'a> {
    /// The name `all_bounds` is keyed by: a function name, or
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
    id: DeclId,
    enum_name: String,
    payload: Vec<Type>,
}

#[derive(Clone)]
struct Binding {
    ty: Type,
    mutable: bool,
    /// The binder's `(line, col)`; `(0, 0)` for module state, a predicate's
    /// field and a desugar's binder.
    at: (usize, usize),
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

impl Decls for Checker<'_> {
    fn decl(&self, name: &str) -> Option<&TypeDecl> {
        let types = self.types;
        self.read(name, types.get(name).map(|(d, t)| (*d, t)))
    }
}

impl<'a> Checker<'a> {
    /// A checker with no body typed yet, recording when `recording`.
    fn new(cx: &'a Cx<'a>, recording: bool) -> Checker<'a> {
        Checker {
            cx,
            frame: Default::default(),
            region_floor: Default::default(),
            acc: RefCell::new(Typed::empty(recording)),
            errors: Default::default(),
            globals: Default::default(),
            in_test: Default::default(),
            in_bench: Default::default(),
            unknown: Default::default(),
            stmt_line: Default::default(),
            reader: Default::default(),
            pending_subst: Default::default(),
            pending_call: Default::default(),
            fresh: Default::default(),
            recheck: None,
        }
    }

    fn recording(&self) -> bool {
        self.acc.borrow().record.is_some()
    }

    /// Hands the solved type arguments to the [`Checker::expr`] wrapper,
    /// keyed by the names [`Checker::rename_apart`] gave them in `pairs`.
    fn note_subst(&self, name: &str, subst: &HashMap<String, Type>, pairs: &[(String, String)]) {
        let args: Vec<(String, Type)> = (pairs.iter())
            .filter_map(|(_, f)| subst.get(f).map(|t| (f.clone(), t.clone())))
            .collect();
        *self.pending_subst.borrow_mut() = Some((name.to_string(), args));
    }

    /// Names each of `type_params` apart for one instantiation, as `T'n`:
    /// the caller's own `T` and the callee's `T` are two parameters while a
    /// call or a record literal solves. Returns the renaming and, in order,
    /// each parameter with its fresh name. A sentence prints the written name
    /// ([`crate::ast::written_param`]), and the check's outputs other than the
    /// record carry it ([`crate::types::written_params`]).
    fn rename_apart(
        &self,
        type_params: &[String],
    ) -> (HashMap<String, Type>, Vec<(String, String)>) {
        let n = self.fresh.get();
        self.fresh.set(n + 1);
        let pairs: Vec<(String, String)> = (type_params.iter())
            .map(|tp| (tp.clone(), format!("{tp}'{n}")))
            .collect();
        let ren = (pairs.iter())
            .map(|(tp, f)| (tp.clone(), Type::Param(f.clone())))
            .collect();
        (ren, pairs)
    }

    /// `subst`, solved under [`Checker::rename_apart`]'s `pairs`, keyed by
    /// the written parameters again.
    fn solved_as_written(
        subst: &HashMap<String, Type>,
        pairs: &[(String, String)],
    ) -> HashMap<String, Type> {
        (pairs.iter())
            .filter_map(|(tp, f)| Some((tp.clone(), subst.get(f)?.clone())))
            .collect()
    }

    // ---- type relations -------------------------------------------------

    /// The representation type: a named type decays to its base.
    fn base(&self, ty: &Type) -> Type {
        crate::types::resolve(ty, self)
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
            || (self.sig(name).is_none()
                && self
                    .impls
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
                    if let Some(t) = self.base(&inner).elem() {
                        return Some(t.clone());
                    }
                }
                let (_, f) = (self.impls.place(&inner, m))
                    .or_else(|| self.impls.place(&self.base(&inner), m))?;
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
            || self.sig(name).is_some()
            || !self
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            return Ok(None);
        }
        let recv = self.expr(&args[0], scope, None, Some(fn_ret))?;
        let found =
            (self.impls.place(&recv, name)).or_else(|| self.impls.place(&self.base(&recv), name));
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
            if let Ok(Some(p)) = self.expansions.optional_site(
                self.impls,
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
        if crate::project::is_builtin_container(recv) || self.impls.is_empty() {
            return Ok(None);
        }
        // The declared type first, because the impl is keyed by its name;
        // then the base, for a validated type.
        let found =
            (self.impls.place(recv, method)).or_else(|| self.impls.place(&self.base(recv), method));
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

    /// Whether the type key `key` implements `protocol`: it declares the impl,
    /// and the impl's target dispatches ([`dispatches_on`]).
    fn implements(&self, protocol: &str, key: &str) -> bool {
        (self.impls.get(protocol, key)).is_some_and(|i| dispatches_on(&i.ty, self.types))
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
            if self.implements(crate::types::OWNED, &k) {
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
        self.decl(enum_name)
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
            types: &dyn Decls,
            at: &dyn Fn(&Type) -> Reach,
            seen: &mut Vec<String>,
        ) -> bool {
            match at(ty) {
                Reach::Yes => return true,
                Reach::No => return false,
                Reach::Parts => {}
            }
            match ty {
                Type::Named(n) | Type::App(n, _) => {
                    let args = match ty {
                        Type::App(_, a) => a.as_slice(),
                        _ => &[],
                    };
                    args.iter().any(|a| go(a, types, at, seen))
                        || (!seen.iter().any(|s| s == n)
                            && types.decl(n).is_some_and(|d| {
                                seen.push(n.clone());
                                let r = go(&d.base, types, at, seen);
                                seen.pop();
                                r
                            }))
                }
                Type::Fn(..) => false,
                _ => ty.children().any(|c| go(c, types, at, seen)),
            }
        }
        go(ty, self, at, &mut Vec::new())
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
        crate::types::assignable(from, to, self)
    }

    /// Whether `ty` mentions an open type parameter: one that is not the
    /// current function's own. An open parameter in an expectation says
    /// nothing, so the value decides: `Deque { front: [2, 1] }` passes
    /// `Array<T>` down with `T` unsolved.
    fn mentions_open_param(&self, ty: &Type) -> bool {
        !self.open_params(ty).is_empty()
    }

    /// The open parameters in `ty`, by written name, in order, without
    /// repeats.
    fn open_params(&self, ty: &Type) -> Vec<String> {
        let cur = self.frame.borrow();
        let rigid = self.type_params(&cur.name);
        let mut out: Vec<String> = Vec::new();
        walk_type(ty, &mut |t| {
            if let Type::Param(n) = t {
                let w = crate::ast::written_param(n);
                if !rigid.is_some_and(|ps| ps.contains(n)) && !out.iter().any(|o| o == w) {
                    out.push(w.to_string());
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
        crate::types::coercible(from, to, self)
    }

    /// Refuses a constant, or a record literal of constants, that fails the
    /// predicate of the named type it flows into. Other values keep the
    /// runtime check.
    fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {
        let decl = match to {
            Type::Named(n) => match self.decl(n) {
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
        match crate::finite::prove_string_flow(expr, to, self, &resolve) {
            crate::finite::Proof::Witness(witness) => {
                // A witness implies a named predicated target.
                let decl = match to {
                    Type::Named(n) => self.decl(n).unwrap(),
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
            Type::Named(n) if n == "Code" && self.decl("Code").is_none() => {
                if !self.frame.borrow().in_gen {
                    return Err(cerr!(line, GenOnlyType, name = "Code"));
                }
                return Ok(());
            }
            Type::Named(n) if n == "Token" && self.decl("Token").is_none() => {
                if !self.frame.borrow().in_gen {
                    return Err(cerr!(line, GenOnlyType, name = "Token"));
                }
                return Ok(());
            }
            // `Self` parses as an ordinary name and is not a type. Refused here,
            // so the diagnostic lands on the protocol, not on each impl.
            Type::Named(n) if n == "Self" && self.decl("Self").is_none() => {
                return Err(cerr!(line, SelfNotType))
            }
            Type::Named(n) => match self.decl(n) {
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
                let d = (self.decl(name)).ok_or_else(|| cerr!(line, UnknownType, n = name))?;
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
                let fields = crate::types::record_fields(base, self)
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
                if crate::types::record_fields(a, self).is_none()
                    || crate::types::record_fields(b, self).is_none()
                {
                    return Err(cerr!(line, MergeNeedsRecords));
                }
            }
            Type::Partial(base) => {
                self.ensure_type_exists(base, line)?;
                if crate::types::record_fields(base, self).is_none() {
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
                match crate::types::resolve(key, self) {
                    Type::Str | Type::Int => {}
                    shape @ (Type::Float | Type::Float32 | Type::Record(_) | Type::Enum(_)) => {
                        self.check_key_shape(key, &shape, line)?;
                        if self
                            .impls
                            .method(crate::types::HASHABLE, key, "hash")
                            .is_none()
                        {
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
                return Err(cerr!(t.line, EnumEmpty, name = DeclName(&t.name)));
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
            if crate::types::record_fields(&t.base, self).is_none() {
                return Err(cerr!(t.line, NotRecord, name = DeclName(&t.name)));
            }
            return Ok(());
        }
        if !t.base.is_scalar() {
            let name = DeclName(&t.name);
            return Err(cerr!(t.line, ValidatedBaseNotScalar, name));
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
        let decl = DeclName(&t.name);
        if consteval::contains_call(pred) {
            return Err(cerr!(t.line, PredicateCalls, kind, name = decl));
        }
        let mut scope = Scope::closed();
        for (name, ty) in binds {
            scope[0].insert(
                name,
                Binding {
                    ty,
                    mutable: false,
                    at: (0, 0),
                },
            );
        }
        let pty = self.expr(pred, &scope, None, None)?;
        if self.base(&pty) != Type::Bool {
            return Err(cerr!(t.line, PredicateNotBool, kind, name = decl, pty));
        }
        Ok(())
    }

    // ---- functions / statements ----------------------------------------

    /// Whether the current function's parameter `t` carries `bound`, where
    /// `Num` implies `Ord` and `Ord` implies `Eq`.
    fn param_has_bound(&self, t: &str, bound: &str) -> bool {
        let frame = self.frame.borrow();
        let bs = match frame.bounds.get(t) {
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
            "Num" | "Ord" => base.is_numeric(),
            "Eq" => base.is_scalar(),
            // A user protocol: satisfied if the type implements it.
            _ if self
                .protocol_methods
                .values()
                .any(|entries| entries.iter().any(|(p, _)| p == bound))
                || self.impls.iter().any(|i| i.protocol == bound) =>
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
            .any(|k| self.implements(bound, &k))
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
        for (i, g) in program.globals.iter().enumerate() {
            self.reading(SourceBody::Global(i as u32));
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
                    return Err(cerr!(g.line, GlobalUnit, name = DeclName(&g.name)));
                }
                if matches!(self.base(&vty), Type::Stream(_)) {
                    return Err(cerr!(g.line, GlobalStream, name = DeclName(&g.name)));
                }
                if let Some(declared) = &g.ty {
                    if !self.coercible(&vty, declared) {
                        return Err(cerr!(
                            g.line,
                            InitMismatch,
                            name = DeclName(&g.name),
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
                    at: (0, 0),
                },
                Err(s) => {
                    out.extend(s.map(|d| d.in_file(g.module.clone())));
                    refused.insert(g.name.clone());
                    Binding {
                        ty: Type::Err,
                        mutable: g.mutable,
                        at: (0, 0),
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
                if let Some(r) = &mut self.acc.borrow_mut().record {
                    r.node_types.remove(&key);
                }
            }
            out.extend(lambda.into_iter().map(|d| d.in_file(g.module.clone())));
            let decl = DeclId::nth(DeclKind::Global, i);
            self.globals
                .borrow_mut()
                .insert(g.name.clone(), (decl, binding));
            ready.insert(g.name.clone());
        }
        self.reader.set(None);
        out.len() - before
    }

    /// Types `f`'s signature and body as the reader `body`, and hands back
    /// what that added: its diagnostics, each in `f`'s file, its part of the
    /// record and its read rows. In a
    /// body, errors accumulate per statement in `errors`; `function` returns
    /// the first and this drains the rest. Within one expression the check
    /// is first-error.
    fn signature_and_body(&self, body: SourceBody, f: &Function) -> Typed {
        self.reading(body);
        // Signature validation runs outside `function()` and must accept a
        // `Code` type in a `gen fn` signature.
        self.enter(Frame::of(f, self.host));
        let r = (|| -> Result<(), Diagnostic> {
            for p in &f.params {
                // A function value cannot cross the host boundary, nor a
                // generation-time signature.
                if self.contains_fn(&p.ty) && (f.is_extern || f.is_export_extern) {
                    return Err(cerr_at!(f.line, f.name_span(), ExternTakesFn));
                }
                if self.contains_fn(&p.ty) && f.is_gen {
                    return Err(cerr_at!(f.line, f.name_span(), GenTakesFn));
                }
                self.ensure_type_exists(&p.ty, f.line)?;
            }
            if self.contains_fn(&f.ret) && (f.is_extern || f.is_export_extern) {
                return Err(cerr_at!(f.line, f.name_span(), ExternReturnsFn));
            }
            if self.contains_fn(&f.ret) && f.is_gen {
                return Err(cerr_at!(f.line, f.name_span(), GenReturnsFn));
            }
            self.ensure_type_exists(&f.ret, f.line)?;
            // An `extern` import has no body; an `export extern` has one. Both
            // signatures must fit the host ABI.
            if f.is_extern {
                self.check_extern_sig(f)?;
            } else {
                if f.is_export_extern {
                    self.check_extern_sig(f)?;
                }
                self.function(f)?;
            }
            Ok(())
        })();
        let mut diags: Vec<Diagnostic> = r.err().into_iter().collect();
        diags.append(&mut self.errors.borrow_mut());
        let diags = (diags.into_iter()).map(|d| d.in_file(f.module.clone()));
        self.taken(diags.collect())
    }

    /// Takes what this checker accumulated, as one body's [`Typed`] with
    /// `diags`, and leaves the accumulation empty.
    fn taken(&self, diags: Vec<Diagnostic>) -> Typed {
        let mut acc = self.acc.borrow_mut();
        let empty = Typed::empty(acc.record.is_some());
        Typed {
            diags,
            ..std::mem::replace(&mut *acc, empty)
        }
    }

    /// Puts back an accumulation [`Checker::taken`] took, over an empty one.
    fn put(&self, t: Typed) {
        *self.acc.borrow_mut() = t;
    }

    /// Adds one body's [`Typed`] to this checker's accumulation, as typing
    /// it here would have, and returns its diagnostics.
    fn absorb(&self, mut t: Typed) -> Vec<Diagnostic> {
        let diags = std::mem::take(&mut t.diags);
        self.acc.borrow_mut().extend(t);
        diags
    }

    fn function(&self, f: &Function) -> Result<(), Diagnostic> {
        self.function_body(f, &f.body)
    }

    /// Records the binder the root's name occurrence `id` at `line` names
    /// ([`Binders::uses`]), when the editor asked. A binder a desugar made
    /// has no position, and an expansion's node is spelled at its projection.
    fn note_use(&self, scope: &Scope, name: &str, line: usize, id: Id) {
        let expanded = id.0.unit() >= NodeId::EXPANDED;
        if !self.record_uses || id.col() == 0 || !self.frame.borrow().in_root || expanded {
            return;
        }
        let at = match scope.iter().rev().find_map(|f| f.get(name)) {
            Some(b) if b.at.1 == 0 => return,
            Some(b) => Some(b.at),
            None => None,
        };
        self.acc
            .borrow_mut()
            .uses
            .entry((line, id.col()))
            .or_insert(at);
    }

    /// Binds `name` in the innermost frame at its binder's position `at`, and
    /// records its type for the editor.
    fn bind(&self, scope: &mut Scope, name: &str, ty: Type, mutable: bool, at: (usize, usize)) {
        self.bind_seen(Some(ty.clone()), at.0, at.1);
        let frame = scope.last_mut().expect("a scope has a frame");
        frame.insert(name.to_string(), Binding { ty, mutable, at });
    }

    /// Makes `frame` the function being checked.
    fn enter(&self, frame: Frame) {
        *self.frame.borrow_mut() = frame;
    }

    /// Records a root-module binding's type for the editor, at its binder's
    /// position. The first answer stands: a desugar re-types a copy.
    fn bind_seen(&self, ty: Option<Type>, line: usize, col: usize) {
        let Some(ty) = ty else { return };
        if col == 0 || !self.frame.borrow().in_root {
            return;
        }
        (self.acc.borrow_mut().binders)
            .entry((line, col))
            .or_insert(ty);
    }

    /// Checks `f` with `body` as its body. A `test` or `bench` has a synthetic
    /// head and its real body node, because the record is keyed by address.
    fn function_body(&self, f: &Function, body: &Block) -> Result<(), Diagnostic> {
        self.enter(Frame::of(f, self.host));
        self.errors.borrow_mut().clear();
        // A local shadows a global of the same name.
        let mut scope = Scope::open();
        scope.push(HashMap::new());
        for p in &f.params {
            let mutable = p.capability == Capability::Modify;
            self.bind(&mut scope, &p.name, p.ty.clone(), mutable, (p.line, p.col));
        }
        self.block(body, &f.ret, &mut scope);
        self.first_error()
    }

    /// Records that the function being checked calls the impl methods
    /// `to` ([`StoredFnEffects::dispatched`]).
    fn dispatch(&self, to: impl IntoIterator<Item = String>) {
        let from = self.frame.borrow();
        let edges = to.into_iter().map(|m| (from.name.clone(), m));
        self.acc.borrow_mut().stored.dispatched.extend(edges);
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
    /// recording its node types and nothing else. A shared
    /// [`crate::project::Expansions`] leaks each expansion once, so its node
    /// addresses are stable keys.
    ///
    /// Diagnostics, scope changes and [`Checker::pending_subst`] stay inside: an
    /// expansion fails only where its source already did, and the wrapper
    /// reads `pending_subst` for the access site's own node.
    fn record_desugar(&self, scope: &Scope, run: impl FnOnce(&Self, &mut Scope)) {
        let mark = self.errors.borrow().len();
        let stored = {
            let s = &self.acc.borrow().stored;
            (
                s.sources.len(),
                s.arg_sources.len(),
                s.calls.len(),
                s.dispatched.len(),
            )
        };
        let saved = self.pending_subst.take();
        let mut sc = scope.clone();
        run(self, &mut sc);
        *self.pending_subst.borrow_mut() = saved;
        self.errors.borrow_mut().truncate(mark);
        let s = &mut self.acc.borrow_mut().stored;
        s.sources.truncate(stored.0);
        s.arg_sources.truncate(stored.1);
        s.calls.truncate(stored.2);
        s.dispatched.truncate(stored.3);
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
                        at: (*line, *col),
                    },
                );
            }
            Stmt::ForIn { var, line, col, .. } => {
                // The failed arm never pushed the loop frame; bind in the block's.
                scope.last_mut().unwrap().insert(
                    var.clone(),
                    Binding {
                        ty: Type::Err,
                        mutable: false,
                        at: (*line, *col),
                    },
                );
            }
            _ => {}
        }
    }

    fn stmt(&self, stmt: &Stmt, ret: &Type, scope: &mut Scope) -> Result<(), Diagnostic> {
        *self.stmt_line.borrow_mut() = stmt.line();
        if let Stmt::Assign { name, line, id, .. }
        | Stmt::Store { name, line, id, .. }
        | Stmt::Drop { name, line, id } = stmt
        {
            self.note_use(scope, name, *line, *id);
        }
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
                self.bind(scope, name, bty, *mutable, (*line, *col));
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
            Stmt::Store {
                name,
                leaf,
                value,
                line,
                id: _,
            } => {
                let Some(b) = self.lookup(scope, name) else {
                    self.unknown.set(true);
                    return Ok(());
                };
                match leaf {
                    Step::Field(field) => {
                        let ruled = matches!(&b.ty, Type::Named(n) if self.decl(n).is_some_and(|d| d.predicate.is_some()));
                        let Some(fty) = crate::types::record_fields(&b.ty, self)
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
                    if self.decl(n).is_some_and(|d| d.predicate.is_some()));
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
                    Step::Index(index) => {
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
                        let base = self.base(&b.ty);
                        let elem = match (base.elem(), &base) {
                            (Some(e), _) => e.clone(),
                            (None, Type::Err) => return Ok(()),
                            (None, _) => {
                                // The element type is what `atSet` yields, looked up
                                // by the declared type, which the impl head names.
                                match self.impls.place(&b.ty, "atSet") {
                                    Some((imp, f)) => {
                                        if let Some(p) = f.params.get(1) {
                                            key = crate::types::under_head(imp, &b.ty, &p.ty);
                                        }
                                        crate::types::under_head(imp, &b.ty, &f.ret)
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
                            if let Ok(Some(blk)) = self
                                .expansions
                                .store_index(self.impls, name, index, value, &b.ty)
                            {
                                self.record_desugar(scope, |c, sc| {
                                    c.block(blk, ret, sc);
                                });
                            }
                        }
                        Ok(())
                    }
                }
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
                let base = self.base(&ity);
                let elem = match (base.elem(), &base) {
                    (Some(e), _) => e.clone(),
                    // The loop consumes a stream; movecheck checks that.
                    (None, Type::Stream(inner)) => (**inner).clone(),
                    // A String yields its bytes.
                    (None, Type::Str) => Type::Int,
                    (None, _) => {
                        // A user container's element is what its `nth` yields,
                        // looked up by the declared type.
                        match crate::types::iterate_impl(self.impls, &ity) {
                            Some((imp, _, nth)) => crate::types::under_head(imp, &ity, &nth.ret),
                            // The typed judgment refuses the loop.
                            None => Type::Err,
                        }
                    }
                };
                scope.push(HashMap::new());
                self.bind(scope, var, elem, false, (*line, *col));
                self.block(body, ret, scope);
                scope.pop();
                // A `for` over a user container reads each element through its
                // `nth`; record that read.
                if self.recording() {
                    if let Ok(Some(p)) = self.expansions.for_element(self.impls, &ity, iter, *line)
                    {
                        self.record_desugar(scope, |c, sc| {
                            let bind = |ty| Binding {
                                ty,
                                mutable: false,
                                at: (0, 0),
                            };
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
        let callee = DeclName(callee);
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
        if let Expr::Var { name, line, id }
        | Expr::Call {
            name,
            line,
            id,
            dot: false,
            ..
        } = expr
        {
            self.note_use(scope, name, *line, *id);
        }
        if !self.recording() {
            return self.expr_inner(expr, scope, expected, fn_ret);
        }
        let t = self.expr_inner(expr, scope, expected, fn_ret)?;
        let key = expr.id();
        assert_ne!(
            key,
            NodeId::NONE,
            "the checker typed a node no numbering reached: {expr:?}"
        );
        let pending = self.pending_subst.take();
        let call = self.pending_call.take();
        if let Some(r) = &mut self.acc.borrow_mut().record {
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
                        if self.lookup(scope, name).is_none() && self.sig(name).is_some() =>
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
                // A binding shadows a variant of its name, as the core reads it.
                if let Some(b) = self.lookup(scope, name) {
                    // A name a failed statement bound: its refusals are that
                    // statement's.
                    if b.ty == Type::Err {
                        self.unknown.set(true);
                    }
                    return Ok(b.ty);
                }
                if let Some(info) = self.resolve_variant(name) {
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
                // A bare function name as a value is a stored function value
                // source: `let g = double` takes its signature.
                if let Some((sptys, sret)) = self.sig(name) {
                    self.storable_named_fn(name, *line)?;
                    let sig = Type::Fn(sptys.clone(), Box::new(sret.clone()));
                    let source = StoredSource {
                        sig: self.base(&sig),
                        named: Some(name.clone()),
                        lambda: None,
                    };
                    self.acc.borrow_mut().stored.sources.push(source);
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
                        if t.is_numeric()
                            || matches!(t, Type::F32x4 | Type::I32x4 | Type::F64x2) =>
                    {
                        Ok(t)
                    }
                    UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                    // `~` complements an integer within its width, or
                    // a mask lane-wise. `!` stays the Bool operator.
                    UnOp::BitNot
                        if t.is_integral()
                            || matches!(t, Type::Mask32x4 | Type::Mask64x2 | Type::I32x4) =>
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
                dot,
                name,
                args,
                type_args,
                line,
                id: _,
            } => {
                let at = (expr.id(), *dot);
                let t = self.call(name, at, args, type_args, *line, scope, expected, fn_ret)?;
                // `schemaOf<T>()` lowers through the literal it stands for, so
                // the checker types those nodes too (`project::schema`).
                if let ("schemaOf", [Type::Named(tn) | Type::App(tn, _)], true) =
                    (name.as_str(), type_args.as_slice(), self.recording())
                {
                    if let Some(lit) = self.decl(tn).and_then(|d| self.expansions.schema(expr, d)) {
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
                    t if t.is_seq() && field == "length" => Ok(Type::Int),
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
                let base = match self.decl(name) {
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
        let decl = self.decl(name);
        // `Token` has no declaration; `types::record_fields` states its
        // fields. A synthesized decoder builds one in generation code
        // (`vyrn_genwasm`'s `Decoders::materialize`).
        if decl.is_none() && !(name == "Token" && self.frame.borrow().in_gen) {
            return self.judged();
        }
        let Some(rfields) = crate::types::record_fields(&Type::Named(name.to_string()), self)
        else {
            return self.judged();
        };
        let mut provided = std::collections::HashSet::new();
        let mut subst: HashMap<String, Type> = HashMap::new();
        // The record's parameters are renamed apart from the enclosing
        // function's, as a callee's are ([`Checker::check_declared_call`]).
        let (ren, pairs) = self.rename_apart(decl.map_or(&[][..], |d| &d.type_params));
        // The expected type seeds the solve: a field may not determine its
        // parameter (`[]` for `Array<T>`, or `Handle<T>`, which stores no `T`).
        if !pairs.is_empty() {
            if let Some(want) = expected {
                let mine = Type::App(
                    name.to_string(),
                    (pairs.iter())
                        .map(|(_, f)| Type::Param(f.clone()))
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
            let field_ty = crate::types::substitute(&field.ty, &ren);
            let fty = crate::types::substitute(&field_ty, &subst);
            let vty = self.expr(value, scope, Some(&fty), fn_ret)?;
            self.unify(&field_ty, &vty, &mut subst, line)?;
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
        let solved = Self::solved_as_written(&subst, &pairs);
        for tp in &decl.type_params {
            if !solved.contains_key(tp) {
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
            .map(|tp| solved[tp].clone())
            .collect();
        // A field typed before `T` was solved recorded `Array<T>`; the
        // substitution lets the record's reader replace it.
        if self.recording() {
            self.note_subst(name, &subst, &pairs);
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
                let key = crate::types::type_key(other).filter(|k| self.implements(FALLIBLE, k));
                let Some(key) = key else {
                    return Err(cerr!(line, TryOperand, other));
                };
                // Propagation copies the whole value, so the types must match.
                if !self.assignable(other, ret) {
                    return Err(cerr!(line, TryFallibleReturn, other, ret));
                }
                // `Output` is the type of the `success` call the backends emit,
                // so a generic impl solves through the ordinary call path.
                self.call_declared(
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
            Type::Named(n) => match self.decl(n) {
                Some(d) if d.predicate.is_none() && crate::types::is_sum_alias(&d.base) => {
                    crate::types::resolve(&raw_sty, self)
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
                .ok_or_else(|| cerr!(line, NotAVariant, vname = DeclName(&vname), sty))?;
            if ev.payload.len() != bind.len() {
                return Err(cerr!(
                    line,
                    PatternArity,
                    vname = DeclName(&vname),
                    want = ev.payload.len(),
                    got = bind.len()
                ));
            }
            let mut inner = scope.clone();
            if !bind.is_empty() {
                inner.push(HashMap::new());
                for (bname, pty) in bind.iter().zip(&ev.payload) {
                    self.bind(
                        &mut inner,
                        &bname.name,
                        pty.clone(),
                        false,
                        (bname.line, bname.col),
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
                let t = crate::ast::written_param(t);
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
                if l == r && l.is_numeric() {
                    Ok(l)
                } else if op == Add && (l == Type::Str || r == Type::Str) {
                    Err(cerr!(line, ConcatOperands, l, r))
                } else {
                    Err(cerr!(line, ArithOperands, l, r))
                }
            }
            Rem => {
                if l == r && l.is_integral() {
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
                if l == r && (l.is_numeric() || l == Type::Str) {
                    Ok(Type::Bool)
                } else {
                    Err(cerr!(line, CompareOperands, l, r))
                }
            }
            Eq | NotEq => {
                if l == r && l.is_scalar() {
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
                if l == r && l.is_integral() {
                    Ok(l)
                } else if l.is_integral() && r.is_integral() {
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

    /// Types a call to a builtin its row types alone ([`crate::prelude::Typed`]):
    /// each operand against its parameter, in the row's words. The count is
    /// checked before ([`crate::prelude::Arity`]).
    fn typed_row(
        &self,
        name: &str,
        row: &crate::prelude::Typed,
        args: &[Expr],
        line: usize,
        scope: &Scope,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        for (i, (a, want)) in args.iter().zip(&row.params).enumerate() {
            let t = self.base(&self.expr(a, scope, Some(want), fn_ret)?);
            let Some(refuse) = row.wrong_type else {
                continue;
            };
            match t {
                Type::Err if i < row.stops => return Ok(Type::Err),
                Type::Err => {}
                t if t != *want => return Err(cerr!(line; refuse(name, i, want, &t))),
                _ => {}
            }
        }
        Ok(row.ret.clone())
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
        match name {
            "@lane" => {
                let v = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                let Some((lanes, out)) = v.lanes() else {
                    return Err(cerr!(line, LaneReceiver, other = v));
                };
                let lanes = i64::from(lanes);
                if crate::types::const_lane(&args[1], lanes).is_none() {
                    return Err(cerr!(line, LaneIndex, max = lanes - 1));
                }
                Ok(out)
            }
            // `v.replaceLane(k, x)`, on a vector only: masks come only from
            // comparison.
            "@replaceLane" => {
                let v = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
                if matches!(v, Type::Err) {
                    return Ok(Type::Err);
                }
                let (lanes, lane) = match v.lanes() {
                    Some((n, lane)) if lane != Type::Bool => (i64::from(n), lane),
                    _ => return Err(cerr!(line, ReplaceLaneReceiver, v)),
                };
                // Constant, as for `lane`: the replace-lane opcodes take an
                // immediate.
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
                let what = &name[1..];
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
                let what = crate::prelude::simd_words(name).0;
                let (vec, lane) = match what.as_str() {
                    "I32x4" => (Type::I32x4, INT32),
                    "F64x2" => (Type::F64x2, Type::Float),
                    _ => (Type::F32x4, Type::Float32),
                };
                let store = name.ends_with("Store");
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
            // Undo the parser's capital, so the message names what was written.
            other => {
                let (ty, m) = crate::prelude::simd_words(other);
                Err(cerr!(line, VectorNoMethod, ty, m))
            }
        }
    }

    /// The `impl Show for T` a value of the written type `t` renders through.
    fn show_dispatch(&self, t: &Type) -> Option<String> {
        crate::types::show_dispatch(self.impls, t, &self.base(t))
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
        self.call_declared(&m, args, &[], line, scope, Some(&Type::Str), fn_ret)?;
        Ok(true)
    }

    /// Types the call `at` (its node, and whether it is written `recv.name(..)`).
    #[allow(clippy::too_many_arguments)]
    fn call(
        &self,
        name: &str,
        (node, dot): (NodeId, bool),
        args: &[Expr],
        written: &[Type],
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        // A call resolves to the nearest binding in scope, before the builtins
        // and the declared functions: a binding of function type (parameter,
        // local or module state) is called through. A bare call that a local
        // of another type binds is refused; a dot call skips that local,
        // because no such local can be its target.
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
                // Every argument is `read` ([`Checker::reads_every_param`]).
                for (i, (arg, pty)) in args.iter().zip(&ptys).enumerate() {
                    let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                    if !self.coercible(&aty, pty) {
                        return Err(cerr!(line, FnValueArgType, name, arg = i + 1, pty, aty));
                    }
                    self.prove_coercion(arg, pty, line)?;
                }
                // A call through a stored value (any binding outside the
                // params frame, index 1) dispatches over the signature's
                // collected sources, so the effect fixpoint needs
                // (caller, signature). A parameter call keeps caller-site
                // attribution.
                let frame = scope.iter().rposition(|f| f.contains_key(name));
                if frame != Some(1) {
                    let caller = self.frame.borrow().name.clone();
                    let sig = self.base(&binding.ty);
                    self.acc.borrow_mut().stored.calls.push((caller, sig));
                }
                self.acc.borrow_mut().stored.through.insert(node);
                return Ok((*ret).clone());
            }
            if !dot && scope.iter().any(|f| f.contains_key(name)) {
                return match self.base(&binding.ty) {
                    // The binding's own refusal was stated where it was bound.
                    Type::Err => Ok(Type::Err),
                    ty => Err(cerr!(line, CallsLocal, name, ty)),
                };
            }
        }
        self.call_declared(name, args, written, line, scope, expected, fn_ret)
    }

    /// Types a call that no binding answers for: a builtin, a constructor, a
    /// declared function, an impl method by its flattened name, or a method
    /// dispatched on its receiver.
    #[allow(clippy::too_many_arguments)]
    fn call_declared(
        &self,
        name: &str,
        args: &[Expr],
        written: &[Type],
        line: usize,
        scope: &Scope,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, Diagnostic> {
        if (name == "assert" || name == "assertEq") && !*self.in_test.borrow() && !self.host.test {
            return Err(cerr!(line, TestOnly, name));
        }
        if name == "blackBox"
            && !*self.in_test.borrow()
            && !*self.in_bench.borrow()
            && !self.host.test
        {
            return Err(cerr!(line, BlackBoxOutsideBench));
        }

        // Every builtin whose whole contract is a row in `prelude::rows` is
        // typed at the fall-through (`prelude::checkable`). The arms here carry
        // what a row cannot: a gate on where the call stands, a type name as an
        // argument, a result taken from context, a union parameter, or a
        // refusal about the element type.

        // Generation-only: no backend lowers it, so the one refusal lives here.
        if name == "moduleInterface" && !self.frame.borrow().in_gen {
            return Err(cerr!(line, GenOnly, name = "moduleInterface"));
        }
        // Code quotes, generation-only. `@codeText`/`@codeSplice`
        // are the desugar of a `vyrn"..."` literal. The surface names are
        // common words and not reserved: a function or binding of the same
        // name in THIS module shadows them (not one in another module).
        let surface = crate::ast::is_surface_builtin(name);
        let unshadowed = !self.shadows_here(name) && self.lookup(scope, name).is_none();
        if matches!(name, "@codeText" | "@codeSplice") || (surface && unshadowed) {
            if !self.frame.borrow().in_gen {
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
                    let ok = t.is_scalar() || t == Type::Err || t == code();
                    if !ok {
                        return Err(cerr!(line, QuoteSplice, t));
                    }
                    return Ok(code());
                }
                _ => {}
            }
        }
        // A surface builtin a declaration here shadows is that declaration's.
        if let Some(row) = crate::prelude::builtin(name).filter(|_| !surface || unshadowed) {
            if let Some(a) = &row.arity {
                if !a.counts.contains(&args.len()) {
                    return Err(cerr!(line; (a.refuse)(name, a.counts[0], args.len())));
                }
            }
            if let Some(typed) = &row.typed {
                return self.typed_row(name, typed, args, line, scope, fn_ret);
            }
        }
        if name == "assertEq" {
            let a = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            let b = self.base(&self.expr(&args[1], scope, Some(&a), fn_ret)?);
            if matches!(a, Type::Err) || matches!(b, Type::Err) {
                return Ok(Type::Unit);
            }
            if a != b || !a.is_scalar() {
                return Err(cerr!(line, AssertEqOperands, a, b));
            }
            return Ok(Type::Unit);
        }
        // `blackBox<T>(v: T) -> T`: identity the optimizer cannot see through,
        // so the work producing `v` survives and does not fold.
        if name == "blackBox" {
            return self.expr(&args[0], scope, expected, fn_ret);
        }

        if name == "@push" {
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
                        if let Ok(Some(p)) = self.expansions.site(
                            self.impls,
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
            let base = self.base(&at);
            let elem = match (base.elem(), &base) {
                (Some(e), _) => e.clone(),
                // `s[i]` is a byte, as in `bytes(s)`.
                (None, Type::Str) => Type::IntN {
                    bits: 8,
                    signed: false,
                },
                (None, Type::Err) => return Ok(Type::Err),
                (None, other) => return Err(cerr!(line, IndexReceiver, other)),
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
            let elem = self.mut_array_receiver(&args[0], scope, line, "pop")?;
            return Ok(match elem {
                Type::Err => Type::Err,
                t => Type::option(t),
            });
        }
        // O(1) unordered remove: the last element moves into slot `i`.
        if name == "@swapRemove" {
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
            let t = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(self.base(&t), Type::Err) {
                return Ok(Type::Err);
            }
            // A declared `impl Copy` answers first and overrides every refusal
            // below.
            if let Some(key) = crate::types::type_key(&t) {
                if self.implements(crate::types::COPY, &key) {
                    let mangled = crate::types::impl_method_name(crate::types::COPY, &key, "copy");
                    return self.call_declared(&mangled, args, &[], line, scope, expected, fn_ret);
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
            if let Some(name) = crate::declared::self_referring(&t, self) {
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
            if !src.is_numeric() {
                return Err(cerr!(line, ConversionType, name, src));
            }
            return Ok(target);
        }
        // Vector builtins. The methods arrive under internal names
        // the parser assigns, so a user `fn min` or `fn lane` is untouched.
        if matches!(name, "@lane" | "@replaceLane" | "@anyTrue" | "@allTrue")
            || name.starts_with("@f32x4")
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
                Some(Expr::Var { name: tn, .. }) if self.decl(tn).is_some() => tn.clone(),
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
            if !self.frame.borrow().in_gen {
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
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self) {
                return Err(cerr!(line, ToJsonUncodable, off));
            }
            self.acc.borrow_mut().derive.push(crate::gen::Site {
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
            if self.sig(g) != Some(&(vec![arg], Type::Str)) || !self.gen_fns.contains(g) {
                return Err(cerr!(line, DeriveUnknownGen, g));
            }
            let at = self.expr(&args[1], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self) {
                return Err(cerr!(line, DeriveUncodable, g, off));
            }
            self.acc.borrow_mut().derive.push(crate::gen::Site {
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
                return Err(match crate::types::show_key(&written) {
                    Some(key) => cerr!(line, ValueTypeNoShow, t, key = DeclName(&key)),
                    None => cerr!(line, ValueType, t),
                });
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
            let expected_res = expected.map(|e| crate::types::resolve(e, self));
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
                _ => return Err(cerr!(line, InferVariant, name = DeclName(name))),
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
                    VariantPayload,
                    name = DeclName(name),
                    aty,
                    want_ty
                ));
            }
            return Ok(Type::result(t, e));
        }

        if let Some(info) = self.resolve_variant(name) {
            let payload = info.payload.clone();
            if payload.is_empty() {
                return Err(cerr!(line, VariantNoArgs, name = DeclName(name)));
            }
            if args.len() != payload.len() {
                return Err(cerr!(
                    line,
                    VariantArity,
                    name = DeclName(name),
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
        if let Some(decl) = self.decl(name).filter(|d| d.predicate.is_some()) {
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
                    .filter(|(p, _)| key.as_ref().is_some_and(|k| self.implements(p, k)))
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
                    let every = (self.impls.iter())
                        .filter(|i| i.protocol == proto)
                        .filter_map(|i| crate::types::type_key(&i.ty))
                        .map(|key| crate::types::impl_method_name(&proto, &key, name));
                    self.dispatch(every);
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
                Some(key) if self.implements(&proto, &key) => {
                    let mangled = crate::types::impl_method_name(&proto, &key, name);
                    self.dispatch([mangled.clone()]);
                    // Dispatch ends here; the impl method is read as any
                    // declaration, its receiver's capability at index 0.
                    let (mparams, mret) = self
                        .sig(&mangled)
                        .ok_or_else(|| cerr!(line, NotImplemented, recv, proto, name))?;
                    let mcaps = self.caps(&mangled);
                    return self.check_declared_call(
                        &DeclaredCall {
                            key: mangled.as_str(),
                            shown: name,
                            params: mparams,
                            ret: mret,
                            type_params: self.type_params(&mangled),
                            caps: mcaps.as_ref(),
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
        if self.sig(name).is_none()
            && !args.is_empty()
            && self
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            if let Some(t) = self.place_result(&recv, name, args, scope, fn_ret, line)? {
                self.refuse_chained_projection(&args[0], scope, line)?;
                if self.recording() {
                    if let Ok(Some(p)) = self.expansions.site(
                        self.impls,
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
        if self.sig(name).is_none() && !args.is_empty() && self.protocol_places.contains(name) {
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(self.base(&recv), Type::Param(_)) {
                return Err(cerr!(line, ProjectionOnBound, name));
            }
        }
        if let Some(t) = gen_host_primitive(name, args.len(), self.host) {
            for a in args {
                self.expr(a, scope, None, fn_ret)?;
            }
            return Ok(t);
        }
        // A seeded builtin's row is its declaration. A row that
        // cannot type its call answers `None` from `prelude::checkable`, and an
        // arm above holds that name. A user declaration shadows the row.
        let seeded = match self.sig(name).is_some() {
            true => None,
            false => crate::prelude::checkable(name),
        };
        let seeded_sig = seeded.map(|f| {
            (
                f.params.iter().map(|p| p.ty.clone()).collect::<Vec<Type>>(),
                f.ret.clone(),
            )
        });
        let (params, ret) = match (self.sig(name), &seeded_sig) {
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
        let caps = self.caps(name);
        self.check_declared_call(
            &DeclaredCall {
                key: name,
                shown,
                params,
                ret,
                type_params: self.type_params(name).or(seeded_generics.as_ref()),
                caps: caps.as_ref().or(seeded_caps.as_ref()),
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
            let (ren, pairs) = self.rename_apart(type_params);
            let params: &[Type] = &(params.iter())
                .map(|p| crate::types::substitute(p, &ren))
                .collect::<Vec<_>>();
            let ret = &crate::types::substitute(ret, &ren);
            let mut subst: HashMap<String, Type> = HashMap::new();
            // Written type arguments seed the solve in declaration order; the
            // arguments infer the rest. They are the only source for a
            // parameter only in the result, such as `fromJson<T>`'s.
            if d.written.len() > type_params.len() {
                return refused();
            }
            for ((_, f), ty) in pairs.iter().zip(d.written) {
                self.ensure_type_exists(ty, line)?;
                subst.insert(f.clone(), ty.clone());
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
                            self.note_subst(d.key, &subst, &pairs);
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
            if pairs.iter().any(|(_, f)| !subst.contains_key(f)) {
                if let Some(want) = expected {
                    let mut from_ctx: HashMap<String, Type> = HashMap::new();
                    if self.unify(ret, want, &mut from_ctx, line).is_ok() {
                        for (_, f) in &pairs {
                            if let (false, Some(t)) = (subst.contains_key(f), from_ctx.get(f)) {
                                subst.insert(f.clone(), t.clone());
                            }
                        }
                    }
                }
            }
            let solved = Self::solved_as_written(&subst, &pairs);
            for tp in type_params {
                if !solved.contains_key(tp) {
                    // An argument already refused leaves its parameter open;
                    // answer `Err` rather than a second sentence.
                    if atys.iter().any(|t| matches!(t, Type::Err)) {
                        return Ok(Type::Err);
                    }
                    return Err(cerr!(line, InferParam, tp, name = DeclName(shown)));
                }
            }
            if let Some(bounds) = d.bounds {
                for (tp, bs) in bounds {
                    let Some(concrete) = solved.get(tp) else {
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
                            let shown = DeclName(shown);
                            return Err(cerr!(line, BoundUnsatisfied, shown, tp, b, concrete));
                        }
                    }
                }
            }
            let rty = crate::types::substitute(ret, &subst);
            // `fromJson<T>(s)` calls what `std/jsondec`'s generator writes for
            // `T`, and the solve is the one place `T` is known.
            if d.key == "fromJson" {
                if let Some(t) = solved.get("T") {
                    self.acc.borrow_mut().derive.push(crate::gen::Site {
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
                self.note_subst(d.key, &subst, &pairs);
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
                col: lcol,
                ..
            } => {
                if params.len() != ptys.len() {
                    return Ok(false);
                }
                let mut inner = scope.clone();
                inner.push(HashMap::new());
                for (pn, pty) in params.iter().zip(&ptys) {
                    self.bind(&mut inner, &pn.name, pty.clone(), false, (pn.line, pn.col));
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
                self.record_arg_fn(&sig, None, Some((*lline, *lcol)));
                // The core types the literal's closure from this row (a
                // `consume` position names no target).
                if let Some(r) = &mut self.acc.borrow_mut().record {
                    r.node_types.insert(arg.id(), sig);
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
                let sig = self.sig(vn).ok_or_else(|| {
                    cerr!(line, ArgNotFn, callee = DeclName(callee), arg = i + 1, vn)
                })?;
                // A generic function is no value: its type parameters have
                // nothing to solve against.
                if self.type_params(vn).is_some()
                    || sig.0.len() != ptys.len()
                    || !params_accept(&sig.0, &sig.1, subst)?
                {
                    return Ok(false);
                }
                self.reads_every_param(vn, line)?;
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
            params,
            body,
            line,
            col,
            ..
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
            self.bind(&mut inner, &pn.name, pty.clone(), false, (pn.line, pn.col));
        }
        let mut locals: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
        self.check_lambda_body_captures(body, scope, &mut locals, *line)?;
        // Stored calls in the body belong to this lambda's summary, not the
        // enclosing function's: the body runs wherever the value is invoked.
        let calls_before = self.acc.borrow().stored.calls.len();
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
        let nested_sigs: Vec<Type> = self.acc.borrow().stored.calls[calls_before..]
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        // Effect summary for `--workers`: the body's call names and the first
        // module-state binding it touches.
        let mut calls: std::collections::HashSet<String> = Default::default();
        {
            let acc = self.acc.borrow();
            let mut v = Calls(&mut calls, &acc.stored.through);
            let mut locals = HashSet::new();
            match body {
                LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
                LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
            }
        }
        // Names that shadow module state: the lambda's parameters and every
        // frame (module state is `Scope`'s fall-through, not a frame). The
        // walk adds the body's binders where their scopes start.
        let mut local_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
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
        self.acc.borrow_mut().stored.sources.push(StoredSource {
            sig: sig.clone(),
            named: None,
            lambda: Some(StoredLambda {
                defined_in: self.frame.borrow().name.clone(),
                line: *line,
                col: *col,
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
        self.acc.borrow_mut().stored.sources.push(StoredSource {
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
    fn record_arg_fn(&self, sig: &Type, named: Option<&str>, lambda_at: Option<(usize, usize)>) {
        let source = StoredSource {
            sig: self.base(sig),
            named: named.map(str::to_string),
            // Only the frame key is filled: the workers analysis reads
            // `sources` alone (see `StoredFnEffects::arg_sources`).
            lambda: lambda_at.map(|(line, col)| StoredLambda {
                defined_in: self.frame.borrow().name.clone(),
                line,
                col,
                calls: HashSet::new(),
                touches_global: None,
                nested_sigs: Vec::new(),
            }),
        };
        self.acc.borrow_mut().stored.arg_sources.push(source);
    }

    /// Refuses a generic, `extern` or `gen` function used as a value.
    fn storable_named_fn(&self, name: &str, line: usize) -> Result<(), Diagnostic> {
        if self.type_params(name).is_some() {
            return Err(cerr!(line, GenericFnValue, name));
        }
        if self.extern_fns.contains(name) {
            return Err(cerr!(line, ExternFnValue));
        }
        if self.gen_fns.contains(name) {
            return Err(cerr!(line, GenFnValue));
        }
        self.reads_every_param(name, line)
    }

    /// Refuses a function value whose target takes a parameter by other than
    /// `read`. A `Type::Fn` carries no capabilities, so a call through a
    /// value passes every argument as `read`.
    fn reads_every_param(&self, name: &str, line: usize) -> Result<(), Diagnostic> {
        let Some(d) = self.resolve_fn(name) else {
            return Ok(());
        };
        match (self.functions[d.index()].params.iter()).find(|p| p.capability != Capability::Read) {
            Some(p) => Err(cerr!(
                line,
                FnValueCapability,
                name = DeclName(name),
                param = p.name.as_str(),
                cap = p.capability.word()
            )),
            None => Ok(()),
        }
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

            /// Whether the call `name(args)` takes argument `k` by `consume`.
            /// A call through a function value reads every argument. A method
            /// is each protocol's that declares it, narrowed by the type of a
            /// receiver bound outside the lambda.
            fn consumes(&self, name: &str, args: &[Expr], k: usize) -> bool {
                let ck = self.ck;
                let typed = |n: &str| ck.lookup(self.outer, n).map(|b| b.ty);
                if typed(name).is_some_and(|t| matches!(ck.base(&t), Type::Fn(..))) {
                    return false;
                }
                if let Some(cs) = ck.caps(name) {
                    return cs.get(k) == Some(&Capability::Consume);
                }
                let key = match args.first() {
                    Some(Expr::Var { name: r, .. }) => {
                        typed(r).and_then(|t| crate::types::type_key(&t))
                    }
                    _ => None,
                };
                (ck.protocol_methods.get(name).into_iter().flatten())
                    .filter(|(p, _)| key.as_ref().is_none_or(|key| ck.implements(p, key)))
                    .any(|(_, m)| {
                        let mut cs = std::iter::once(m.recv).chain(m.param_caps.iter().copied());
                        cs.nth(k) == Some(Capability::Consume)
                    })
            }
        }

        impl BodyVisit<'_> for Captures<'_, '_> {
            fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
                match s {
                    Stmt::Assign { name, line, .. } if self.is_capture(name, locals) => {
                        self.fail(rule!(LambdaAssignsCapture, name, line));
                    }
                    Stmt::Store {
                        name, leaf, line, ..
                    } if self.is_capture(name, locals) => {
                        self.fail(match leaf {
                            Step::Field(_) => rule!(LambdaMutatesCapture, name, line),
                            Step::Index(_) => rule!(LambdaStoresIntoCapture, name, line),
                        });
                    }
                    Stmt::Drop { name, line, id: _ } if self.is_capture(name, locals) => {
                        self.fail(rule!(LambdaDropsCapture, name, line));
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
                        for (k, a) in args.iter().enumerate() {
                            if self.consumes(name, args, k) {
                                if let Expr::Var { name: vn, .. } = a {
                                    if self.is_capture(vn, locals) {
                                        self.fail(rule!(LambdaConsumesCapture, name = vn, line));
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
                        self.fail(rule!(LambdaNestsLambda, line));
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
        let fname = DeclName(fname);
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
                        let t = crate::ast::written_param(t);
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
            Type::Array(inner) => match crate::types::resolve(aty, self) {
                Type::Array(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            Type::ArrayN(inner, n) => match aty {
                Type::ArrayN(a, m) if m == n => self.unify(inner, a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            Type::Stream(inner) => match crate::types::resolve(aty, self) {
                Type::Stream(a) => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            // `N` must match: integer arguments do not infer.
            Type::SmallArray(inner, n) => match crate::types::resolve(aty, self) {
                Type::SmallArray(a, m) if m == *n => self.unify(inner, &a, subst, line),
                _ => Err(cerr!(line, Expected, pty, aty)),
            },
            // A generic function type binds through the value's signature. Two
            // concrete function types keep the assignability rule below.
            Type::Fn(pps, pr) if type_mentions_param(pty) => {
                match crate::types::resolve(aty, self) {
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
                        crate::types::resolve(aty, self),
                        Type::Fn(ps, _) if ps.is_empty()
                    ) =>
            {
                self.unify(&crate::types::resolve(pty, self), aty, subst, line)
            }
            Type::Map(pk, pv) => match crate::types::resolve(aty, self) {
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
            .contains(&(self.frame.borrow().here.clone(), name.to_string()))
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
            return self.resolve_global(name);
        }
        None
    }

    /// Makes `body` the reader of every lookup until the next call, when the
    /// check records read rows ([`Cx::record_reads`]).
    fn reading(&self, body: SourceBody) {
        self.reader.set(self.record_reads.then_some(body));
    }

    /// Records that the body being checked read `hit` under `name`, or
    /// missed it in this module ([`Key`]).
    fn read<T>(&self, name: &str, hit: Option<(DeclId, T)>) -> Option<T> {
        if let Some(f) = self.reader.get() {
            let key = match &hit {
                Some((d, _)) => Key::Decl(*d),
                None => Key::Miss(
                    ScopeId {
                        module: self.frame.borrow().here.clone(),
                    },
                    name.to_string(),
                ),
            };
            self.acc.borrow_mut().reads.push((f, key));
        }
        hit.map(|(_, t)| t)
    }

    /// The function declared as `name`.
    fn resolve_fn(&self, name: &str) -> Option<DeclId> {
        let hit = self.fn_decls.get(name).map(|d| (*d, *d));
        self.read(name, hit)
    }

    /// The enum variant named `name`.
    fn resolve_variant(&self, name: &str) -> Option<&'a VariantInfo> {
        let variants = self.variants;
        self.read(name, variants.get(name).map(|v| (v.id, v)))
    }

    /// The module state named `name`, once its initializer is checked.
    fn resolve_global(&self, name: &str) -> Option<Binding> {
        let hit = self.globals.borrow().get(name).cloned();
        self.read(name, hit)
    }

    /// The parameter types and result of the function named `name`.
    fn sig(&self, name: &str) -> Option<&'a (Vec<Type>, Type)> {
        let sigs = self.sigs;
        self.resolve_fn(name).map(|d| &sigs[d.index()])
    }

    /// The parameter capabilities of the function named `name`.
    fn caps(&self, name: &str) -> Option<Vec<Capability>> {
        let f = &self.functions[self.resolve_fn(name)?.index()];
        Some(f.params.iter().map(|p| p.capability).collect())
    }

    /// The type parameters of the generic function named `name`.
    fn type_params(&self, name: &str) -> Option<&'a Vec<String>> {
        let f = &self.functions[self.resolve_fn(name)?.index()];
        Some(&f.type_params).filter(|ps| !ps.is_empty())
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
    ty.is_scalar() || (allow_unit && *ty == Type::Unit)
}

/// Refuses a `gen fn` that reaches, through any call chain, an `extern`,
/// module state, or an atom [`crate::effects::gen_refusal`] refuses, naming
/// the effect and the chain. Every `gen fn` is checked, even one called only
/// at run time, because any may be an import target.
fn check_comptime_purity<'a>(
    program: &Program,
    through: &HashSet<NodeId>,
    dispatched: impl Iterator<Item = &'a (String, String)>,
    out: &mut Vec<Diagnostic>,
) {
    let gen_fns: Vec<&Function> = program.functions.iter().filter(|f| f.is_gen).collect();
    if gen_fns.is_empty() {
        return;
    }
    let fn_map: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    // A call to a declared extern is refused by its declaration, so a
    // function spelled like a host-boundary extern is an ordinary one.
    let extern_refusals: HashMap<&str, String> = (program.functions.iter())
        .filter_map(|f| Some((f.name.as_str(), crate::effects::extern_gen_refusal(f)?)))
        .collect();
    let global_names: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    // A method call edge reaches the impl the check dispatched it to.
    let mut impls_called: HashMap<&str, Vec<String>> = HashMap::new();
    for (from, to) in dispatched {
        impls_called.entry(from).or_default().push(to.clone());
    }
    let direct = |f: &Function| -> Option<String> {
        if touches_globals(f, &global_names) {
            return Some("reads or writes module state".to_string());
        }
        for c in fn_calls(&f.body, through) {
            let why = extern_refusals.get(c.as_str()).cloned();
            if let Some(why) = why.or_else(|| crate::effects::gen_refusal(&c)) {
                return Some(why);
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
                let mut edges: Vec<String> = fn_calls(&f.body, through).into_iter().collect();
                edges.extend(impls_called.get(cur).into_iter().flatten().cloned());
                facts.insert(cur.to_string(), (direct(f), edges));
            }
            let (violation, edges) = &facts[cur];
            if let Some(reason) = violation.clone() {
                let msg = if path.len() == 1 {
                    cerr!(g.line, GenImpure, name = DeclName(&g.name), reason)
                } else {
                    let written = path.iter().map(|n| program.spellings.written(n));
                    let chain = written.collect::<Vec<_>>().join(" -> ");
                    let (name, cur) = (DeclName(&g.name), DeclName(cur));
                    cerr!(g.line, GenImpureVia, name, cur, chain, reason)
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
    pub col: usize,
    /// Every call name in the body (functions, builtins, methods).
    pub calls: std::collections::HashSet<String>,
    /// The first module-state binding the body reads or writes.
    pub touches_global: Option<String>,
    /// Signatures of other stored function values the body calls.
    pub nested_sigs: Vec<Type>,
}

/// Whole-program call facts a call name does not carry: stored function
/// values and dispatched protocol methods.
#[derive(Debug, Clone, Default)]
pub struct StoredFnEffects {
    pub sources: Vec<StoredSource>,
    /// The functions handed to a `fn`-typed parameter at a call site.
    ///
    /// Kept apart from `sources` because an argument carries no
    /// defunctionalization tag; the `--workers` analysis reads `sources`
    /// alone, and the effect judgment reads both. Only `sig`, `named` and a
    /// lambda's `defined_in`, `line` and `col` are filled.
    pub arg_sources: Vec<StoredSource>,
    /// `(function, signature)` for each call through a stored fn value.
    pub calls: Vec<(String, Type)>,
    /// Every call node resolved through a binding of `fn` type, parameter
    /// calls included ([`Checker::call`]); [`fn_calls`] leaves them out.
    pub through: HashSet<NodeId>,
    /// `(function, impl method)` for each protocol method call the check
    /// dispatched: the impl a concrete receiver selects, or every impl of the
    /// protocol for a bounded type parameter. A call names the method, which
    /// two protocols may share.
    pub dispatched: Vec<(String, String)>,
}

impl StoredFnEffects {
    /// Adds `tail`'s facts after `self`'s.
    fn extend(&mut self, tail: StoredFnEffects) {
        self.sources.extend(tail.sources);
        self.arg_sources.extend(tail.arg_sources);
        self.calls.extend(tail.calls);
        self.through.extend(tail.through);
        self.dispatched.extend(tail.dispatched);
    }

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
        let written = |n: String| program.spellings.written(&n).to_string();
        chain.into_iter().map(written).collect::<Vec<String>>()
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
                        return Some((
                            chain_to(&cur, &parent),
                            program.spellings.written(g).to_string(),
                        ));
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
            return Some((
                chain_to(&cur, &parent),
                program.spellings.written(&which).to_string(),
            ));
        }
        let mut callees: Vec<String> = fn_calls(&f.body, &stored.through).into_iter().collect();
        for (fname, to) in &stored.dispatched {
            if fname == &cur {
                callees.push(to.clone());
            }
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
    let params: std::collections::HashSet<String> =
        f.params.iter().map(|p| p.name.clone()).collect();
    global_ref_block(&f.body, globals, &params)
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
            Stmt::Assign { name, .. } | Stmt::Store { name, .. } | Stmt::Drop { name, .. } => {
                self.hit(name, locals)
            }
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

/// Whether a block references a global that no local shadows where it is
/// read. `local` holds the names bound outside the block; the walk adds each
/// binder of the block for the scope it starts.
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
    fn expr(&mut self, e: &Expr, locals: &HashSet<String>) -> bool {
        if self.err.is_some() {
            return false;
        }
        let (own_name, line) = (DeclName(self.own_name), self.line);
        match e {
            // A binder of the initializer shadows module state and a function
            // of its name.
            Expr::Var { name, .. } | Expr::Call { name, .. } if locals.contains(name) => true,
            Expr::Var { name, .. }
                if self.all_globals.contains(name.as_str()) && !self.ready.contains(name) =>
            {
                if name == self.own_name {
                    self.fail(cerr!(line, GlobalReadsItself, own_name));
                } else {
                    self.fail(cerr!(line, GlobalReadsLater, own_name, name = DeclName(name)));
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

/// Collects the names a call or a `try`-construct reaches, except each call
/// node in `.1`. A call inside a lambda counts for the enclosing function, its
/// monomorphization site.
struct Calls<'a>(&'a mut HashSet<String>, &'a HashSet<NodeId>);

impl BodyVisit<'_> for Calls<'_> {
    const SCOPED: bool = false;

    fn expr(&mut self, e: &Expr, _: &HashSet<String>) -> bool {
        match e {
            Expr::Call { .. } if self.1.contains(&e.id()) => {}
            Expr::Call { name, .. } | Expr::TryConstruct { name, .. } => {
                self.0.insert(name.clone());
            }
            _ => {}
        }
        true
    }
}

/// Returns every function name called anywhere in `b`. `through` is the
/// check's [`StoredFnEffects::through`]: a call the checker resolved through
/// a binding reaches no function. An empty `through` counts every call by its
/// name.
pub fn fn_calls(b: &Block, through: &HashSet<NodeId>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut locals = HashSet::new();
    body_block(b, &mut locals, &mut Calls(&mut out, through));
    out
}

/// Collects what a body calls and every name it reads or assigns that is not a
/// local: the functions and module state it reaches.
struct Refs<'a>(&'a mut HashSet<String>);

impl BodyVisit<'_> for Refs<'_> {
    fn stmt(&mut self, s: &Stmt, locals: &HashSet<String>) {
        if let Stmt::Assign { name, .. } | Stmt::Store { name, .. } = s {
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
        let program = parse(lex(s).unwrap()).unwrap();
        match check_accum_inner(&program, false, 0, &[]).0.first() {
            Some(d) => Err(d.render()),
            None => Ok(()),
        }
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
        let [(p, t)] = &calls[0].1[..] else {
            panic!("one solved argument: {:?}", calls[0].1)
        };
        assert_eq!((crate::ast::written_param(p), t), ("T", &Type::Int));
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
        let stored = record(&program).stored;
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
    fn workers_gate_walks_the_impl_a_method_call_dispatches_to() {
        // `Pa` and `Qa` both declare `describe`; `handle` calls `P`'s alone.
        let src = "let mut hits: Int64 = 0
             type P = { x: Int64 }
             type Q = { y: Int64 }
             protocol Pa { fn describe(self) -> Int64 }
             protocol Qa { fn describe(self) -> Int64 }
             impl Pa for P { fn describe(self) -> Int64 { return self.x } }
             impl Qa for Q { fn describe(self) -> Int64 { return self.y + hits } }
             fn handle(n: Int64) -> Int64 { let p = P { x: n }  return p.describe() }
";
        let program = parse(lex(src).unwrap()).unwrap();
        let stored = record(&program).stored;
        assert_eq!(module_state_use(&program, "handle", &stored), None);
        let reaches_q = src.replace("P { x: n }", "Q { y: n }");
        let program = parse(lex(&reaches_q).unwrap()).unwrap();
        let stored = record(&program).stored;
        let (chain, global) = module_state_use(&program, "handle", &stored).expect("stateful");
        assert_eq!(
            (chain, global.as_str()),
            (
                vec!["handle".to_string(), "Qa$Q$describe".to_string()],
                "hits"
            )
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
        let stored = record(&program).stored;
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

    /// A name with a hint is not reserved, or the import the hint names
    /// could not be written.
    #[test]
    fn every_moved_name_is_gone_from_reserved() {
        for (n, _) in MOVED_TO_STD {
            assert!(
                !RESERVED.contains(n),
                "`{n}` is both reserved and said to live in a std module"
            );
        }
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
                RESERVED.contains(&n),
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
