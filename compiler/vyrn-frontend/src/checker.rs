//! Type checker for the v0.1 subset (scalars + validated types, RFC-0003).
//!
//! Verifies function signatures, variable use, operator operand types, `mut`
//! assignment, call arity/types, and all-paths return. For validated types it
//! also: type-checks each refinement predicate, validates compile-time-constant
//! constructions (rejecting provably-invalid ones), and enforces that a raw base
//! value cannot be used where a validated type is expected without construction.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::ast::*;
use crate::consteval::{self, ConstVal};
use crate::diagnostics::Diagnostic;

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
pub fn check_accum_with_let_types(
    program: &Program,
) -> (Vec<Diagnostic>, HashMap<(usize, String), Type>) {
    let mut out = Vec::new();

    // 1. Collect and validate type declarations.
    let mut types: HashMap<String, TypeDecl> = HashMap::new();
    for t in &program.type_decls {
        if matches!(t.name.as_str(), "Int64" | "Bool" | "Unit") {
            let mut d = Diagnostic::from_rendered(
                format!("line {}: cannot redefine built-in type `{}`", t.line, t.name),
                "check",
            );
            d.file = t.module.clone();
            out.push(d);
            continue;
        }
        if types.contains_key(&t.name) {
            let mut d = Diagnostic::from_rendered(
                format!("line {}: type `{}` defined twice", t.line, t.name),
                "check",
            );
            d.file = t.module.clone();
            out.push(d);
            continue;
        }
        types.insert(t.name.clone(), t.clone());
    }

    const RESERVED: &[&str] = &[
        "print", "len", "concat", "Some", "None", "Ok", "Err", "match", "cell", "get", "set",
        "release", "array", "push", "at", "alen", "afree", "str", "parse", "join", "logger",
        "contains", "startsWith", "endsWith", "bytes", "chars",
        "hexEncode", "hexDecode", "base64Encode", "base64Decode", "urlEncode", "urlDecode",
        "args", "readLine", "readFile", "writeFile", "readFileBytes", "stringFromBytes",
        "trace", "debug", "info", "warn", "error", "value", "list", "schemaOf", "jsonSchema",
        "toJson", "fromJson",
        "toString", "pop", "swapRemove", "assert", "assertEq",
        "Int", "Int64", "Int32", "Int16", "Int8", "Float", "Float64", "Float32",
        "UInt8", "UInt16", "UInt32", "UInt64",
    ];

    // 1b. Collect enum variants into a global constructor table.
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    for t in &program.type_decls {
        if let Type::Enum(vs) = &t.base {
            for v in vs {
                if RESERVED.contains(&v.name.as_str()) {
                    out.push(Diagnostic::from_rendered(
                        format!("line {}: `{}` is a reserved name", t.line, v.name),
                        "check",
                    ));
                    continue;
                }
                if variants.contains_key(&v.name) {
                    out.push(Diagnostic::from_rendered(
                        format!("line {}: enum variant `{}` is defined twice", t.line, v.name),
                        "check",
                    ));
                    continue;
                }
                if types.contains_key(&v.name) {
                    out.push(Diagnostic::from_rendered(
                        format!("line {}: enum variant `{}` clashes with a type name", t.line, v.name),
                        "check",
                    ));
                    continue;
                }
                variants.insert(
                    v.name.clone(),
                    VariantInfo { enum_name: t.name.clone(), payload: v.payload.clone() },
                );
            }
        }
    }

    // 2. Collect function signatures (forward references allowed).
    let mut sigs: HashMap<String, (Vec<Type>, Type)> = HashMap::new();
    let mut generics: HashMap<String, Vec<String>> = HashMap::new();
    for f in &program.functions {
        if RESERVED.contains(&f.name.as_str()) {
            out.push(Diagnostic::from_rendered(
                format!("line {}: `{}` is a reserved name", f.line, f.name),
                "check",
            ));
            continue;
        }
        if variants.contains_key(&f.name) {
            out.push(Diagnostic::from_rendered(
                format!("line {}: `{}` is both a function and an enum variant", f.line, f.name),
                "check",
            ));
            continue;
        }
        if sigs.contains_key(&f.name) {
            out.push(Diagnostic::from_rendered(
                format!("line {}: function `{}` defined twice", f.line, f.name),
                "check",
            ));
            continue;
        }
        if types.contains_key(&f.name) {
            out.push(Diagnostic::from_rendered(
                format!("line {}: `{}` is both a type and a function name", f.line, f.name),
                "check",
            ));
            continue;
        }
        let params = f.params.iter().map(|p| p.ty.clone()).collect();
        sigs.insert(f.name.clone(), (params, f.ret.clone()));
        if !f.type_params.is_empty() {
            generics.insert(f.name.clone(), f.type_params.clone());
        }
    }
    let all_bounds: HashMap<String, HashMap<String, Vec<String>>> =
        program.functions.iter().map(|f| (f.name.clone(), f.type_bounds.clone())).collect();
    // Each function's parameter capabilities, for checking `modify` call sites.
    let caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.capability).collect()))
        .collect();

    // Which functions are "spawn-safe" — pure enough to run as a concurrent task:
    // no I/O (`print`), no shared-mutable-state ops (`cell`/`set`/`release`), no
    // `modify` params, and (transitively) only calls to other spawn-safe functions.
    // A monotone fixpoint over the call graph (starts optimistic, shrinks).
    let fn_names: std::collections::HashSet<String> =
        program.functions.iter().map(|f| f.name.clone()).collect();
    // Module-state bindings (RFC-0013). A function that reads OR writes any
    // global is not spawn-safe (module state is shared by definition), and the
    // fixpoint below spreads that transitively to every caller.
    let global_names: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    // A protocol-method call site (`n.burp()`) collects the *surface* name, but
    // impl bodies live under mangled names (`Noise__Int__burp`). Expand each
    // surface method name to every registered impl so those call-graph edges
    // are visible to the fixpoint — otherwise an impure impl (one that prints)
    // would be spawnable through a method call.
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
    let expand = |calls: std::collections::HashSet<String>| -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::with_capacity(calls.len());
        for c in calls {
            if let Some(impls) = method_impls.get(&c) {
                out.extend(impls.iter().cloned());
            }
            out.insert(c);
        }
        out
    };
    let mut spawn_safe: std::collections::HashSet<String> = program
        .functions
        .iter()
        .filter(|f| {
            let calls = expand(fn_calls(&f.body));
            let no_modify = f.params.iter().all(|p| p.capability != Capability::Modify);
            // An `extern` (RFC-0012) is a host effect (I/O by definition), so it is
            // never spawn-safe — and any function that calls one becomes unsafe
            // transitively through the fixpoint below.
            !f.is_extern
                && no_modify
                && !calls.iter().any(|c| SPAWN_FORBIDDEN.contains(&c.as_str()))
                && !contains_drop(&f.body)
                && !touches_globals(f, &global_names)
        })
        .map(|f| f.name.clone())
        .collect();
    loop {
        let mut changed = false;
        let snapshot = spawn_safe.clone();
        for f in &program.functions {
            if snapshot.contains(&f.name) {
                let callees = expand(fn_calls(&f.body));
                let ok = callees
                    .iter()
                    .filter(|c| fn_names.contains(*c))
                    .all(|c| snapshot.contains(c));
                if !ok {
                    spawn_safe.remove(&f.name);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Protocol registries (RFC-0002 §5): map each method name to its protocol +
    // signature, and record which (protocol, type-key) pairs are implemented.
    let mut protocol_methods: HashMap<String, (String, MethodSig)> = HashMap::new();
    for p in &program.protocols {
        for m in &p.methods {
            protocol_methods.insert(m.name.clone(), (p.name.clone(), m.clone()));
        }
    }
    let mut impls: std::collections::HashSet<(String, String)> = Default::default();
    for imp in &program.impls {
        // A named target must be an enum: validated/nominal scalars erase to their
        // base at runtime, so the interpreter could not dispatch them (it would
        // diverge from native). Records have no runtime identity either.
        let ok_target = match &imp.ty {
            Type::Int | Type::Bool | Type::Str => true,
            Type::Named(n) => matches!(types.get(n).map(|d| &d.base), Some(Type::Enum(_))),
            _ => false,
        };
        match crate::types::type_key(&imp.ty) {
            Some(key) if ok_target => {
                impls.insert((imp.protocol.clone(), key));
            }
            _ => out.push(Diagnostic::from_rendered(
                format!(
                    "line {}: `impl {} for {}` is not supported — implement protocols for \
                     Int64/Bool/String or an enum (validated scalars and records erase at runtime)",
                    imp.line, imp.protocol, imp.ty
                ),
                "check",
            )),
        }
    }

    let checker = Checker {
        sigs: &sigs,
        caps: &caps,
        spawn_safe: &spawn_safe,
        types: &types,
        variants: &variants,
        generics: &generics,
        all_bounds: &all_bounds,
        protocol_methods: &protocol_methods,
        impls: &impls,
        cur_bounds: RefCell::new(HashMap::new()),
        region_floor: RefCell::new(Vec::new()),
        let_types: RefCell::new(HashMap::new()),
        errors: RefCell::new(Vec::new()),
        globals: RefCell::new(HashMap::new()),
        in_test: RefCell::new(false),
    };

    // 2b. Module state (RFC-0013): check each initializer in declaration order,
    //     record the binding's type, and register it so functions see it. Errors
    //     accumulate; a failed global still binds (as `Err`) so bodies referring
    //     to it don't cascade "unknown variable".
    checker.check_globals(program, &mut out);

    // 3. Validate each type decl (base kind, referenced-type existence, predicate).
    for t in &program.type_decls {
        if let Err(s) = checker.check_type_decl(t) {
            out.push(Diagnostic::from_rendered(s, "check"));
        }
    }

    // 4. main signature. A missing/wrong `main` is a whole-program error (line 0);
    // it does not stop function bodies from being checked below. A program with
    // no `main` but WITH exported declarations is a LIBRARY MODULE (RFC-0010) —
    // it exists to be imported, so demanding an entry point is noise (both in
    // `vyrn check lib.vyrn` and in the editor). Running one still fails
    // cleanly: the interpreter/backends report the missing `main` themselves.
    // A file with tests (RFC-0015) needs no `main` either — it exists to be
    // tested (`vyrn test`), extending the library-module rule (exports OR tests
    // ⇒ no `main` required). RFC-0016 extends it once more: a file that defines
    // `handle` with EXACTLY `fn handle(req: Request) -> Response` is a served
    // module (`vyrn serve` drives it), so it needs no `main`. The signature
    // must match exactly — any other `handle` is an ordinary function that does
    // not exempt `main`.
    let has_served_handle = sigs.get("handle").is_some_and(|(params, ret)| {
        params.as_slice() == [Type::Named("Request".to_string())]
            && *ret == Type::Named("Response".to_string())
    });
    let is_library = program.functions.iter().map(|f| f.exported).any(|e| e)
        || program.type_decls.iter().any(|t| t.exported)
        || program.protocols.iter().any(|p| p.exported)
        || !program.tests.is_empty()
        || has_served_handle;
    match sigs.get("main") {
        None if !is_library => out.push(Diagnostic::from_rendered(
            "no `main` function found".to_string(),
            "check",
        )),
        None => {}
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => out.push(Diagnostic::from_rendered(
            "`main` must have signature `fn main() -> Int64`".to_string(),
            "check",
        )),
        _ => {}
    }

    // 5. Check functions. Each function is checked independently: a bad parameter
    //    or return type pushes one diagnostic and we move on. Within a single
    //    function body, errors now ACCUMULATE at the statement boundary: `block`
    //    pushes each statement's error to the `errors` sink and continues, so
    //    every statement-level error is reported (within one expression the
    //    check is still first-error). `function` returns the first as `Err`
    //    (preserving the historical single-error `check()` surface) and leaves
    //    the rest in the sink, which we drain here.
    for f in &program.functions {
        let r = (|| -> Result<(), String> {
            for p in &f.params {
                checker.ensure_type_exists(&p.ty, f.line)?;
            }
            checker.ensure_type_exists(&f.ret, f.line)?;
            // An `extern` import (RFC-0012 M1) has no body to check; instead
            // enforce the JS-boundary ABI type domain on its signature. An
            // `export extern` (M2) is a normal function that ADDITIONALLY crosses
            // the boundary, so its signature must satisfy the same ABI domain AND
            // its body is checked like any other.
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
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = f.module.clone();
            out.push(d);
        }
        // Drain the rest of this function's accumulated body errors.
        for s in checker.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = f.module.clone();
            out.push(d);
        }
    }

    // 6. Check test bodies (RFC-0015). Each is checked as a Unit-returning
    //    function body under a synthetic unspellable name (`test@<index>`), so
    //    every existing analysis (movecheck runs separately; ownership, spawn
    //    purity, region) applies unchanged. Tests are NOT registered in `sigs`,
    //    so user code can never call one. Duplicate names within a single file
    //    are rejected here (a better message than a parse error).
    check_tests(&checker, program, &mut out);

    // 7. Comptime-purity (RFC-0021): every `gen fn` and its transitive callees
    //    must be pure enough to run in the compiler's interpreter at generation
    //    time. Reported after the ordinary body checks so a broken generator's
    //    type errors surface first.
    check_comptime_purity(program, &mut out);

    let let_types = checker.let_types.borrow().clone();
    (out, let_types)
}

/// Check every `test` body (RFC-0015). Duplicate names per module are reported;
/// each body is checked with `in_test` set so `assert`/`assertEq` are legal.
fn check_tests(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {
    // Duplicate test names are per-file (per-module): group by module so the same
    // name in two different files is fine, but twice in one file is an error.
    let mut seen: HashMap<(Option<String>, String), usize> = HashMap::new();
    for t in &program.tests {
        let key = (t.module.clone(), t.name.clone());
        if let Some(prev) = seen.get(&key) {
            let mut d = Diagnostic::from_rendered(
                format!(
                    "line {}: duplicate test name {:?} (already declared on line {prev})",
                    t.line, t.name
                ),
                "check",
            );
            d.file = t.module.clone();
            out.push(d);
        } else {
            seen.insert(key, t.line);
        }
    }
    *checker.in_test.borrow_mut() = true;
    for (i, t) in program.tests.iter().enumerate() {
        // A synthetic Unit-returning function with an unspellable name; its body
        // is a clone (the checker keys nothing on node identity — only ownership
        // does, and that pass analyses the real body directly).
        let synthetic = Function {
            name: format!("test@{i}"),
            exported: false,
            module: t.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: t.body.clone(),
            line: t.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        };
        if let Err(s) = checker.function(&synthetic) {
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = t.module.clone();
            out.push(d);
        }
        for s in checker.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = t.module.clone();
            out.push(d);
        }
    }
    *checker.in_test.borrow_mut() = false;
}

/// Type-check the program, returning **all** problems found across functions
/// and types as structured [`Diagnostic`]s. Thin shim over
/// [`check_accum_with_let_types`] that drops the inferred-`let`-type table (the
/// CLI/`diagnostics()` path doesn't need it). See that function for the
/// bounded accumulation behavior and the retained `let`/`for`-var types.
pub fn check_accum(program: &Program) -> Vec<Diagnostic> {
    check_accum_with_let_types(program).0
}

/// Type-check the program, returning the first problem found (rendered as the
/// historical `"line {N}: {message}"` string). Thin shim over [`check_accum`];
/// see that function for the bounded accumulation behavior.
pub fn check(program: &Program) -> Result<(), String> {
    match check_accum(program).into_iter().next() {
        Some(d) => Err(d.render()),
        None => Ok(()),
    }
}

struct Checker<'a> {
    sigs: &'a HashMap<String, (Vec<Type>, Type)>,
    /// Each function's parameter capabilities (for `modify` call-site checks).
    caps: &'a HashMap<String, Vec<Capability>>,
    /// Functions that may be run as a concurrent task (`spawn`) — isolated/pure.
    spawn_safe: &'a std::collections::HashSet<String>,
    types: &'a HashMap<String, TypeDecl>,
    variants: &'a HashMap<String, VariantInfo>,
    /// Generic functions: name -> type-parameter names.
    generics: &'a HashMap<String, Vec<String>>,
    /// Every function's type-parameter bounds: fn -> (param -> bounds).
    all_bounds: &'a HashMap<String, HashMap<String, Vec<String>>>,
    /// Protocol methods (RFC-0002 §5): method name -> (protocol, signature).
    protocol_methods: &'a HashMap<String, (String, MethodSig)>,
    /// Implemented (protocol, type-key) pairs, for dispatch and bound checking.
    impls: &'a std::collections::HashSet<(String, String)>,
    /// Bounds of the function currently being checked (for operators on `T`).
    cur_bounds: RefCell<HashMap<String, Vec<String>>>,
    /// Stack of `region` entry depths (scope-frame counts). Non-empty means we
    /// are lexically inside a `region`; the top value is the frame index below
    /// which a binding is "outer" — a heap value must not be assigned there, or
    /// it would dangle when the region frees at block exit.
    region_floor: RefCell<Vec<usize>>,
    /// Inferred (or declared) type of each `let` binding and each `for`-in loop
    /// variable that checked cleanly, keyed by `(line, name)`. Populated as a
    /// side effect of checking so the symbol-query layer can show `let x: Int`
    /// on hover for an unannotated `let x = 5` (the checker computes the type
    /// either way; this just retains it). Best-effort: a binding after a
    /// same-function error isn't reached, so it simply won't appear here.
    let_types: RefCell<HashMap<(usize, String), Type>>,
    /// Inside-body error sink (RFC-0006 accumulation). Cleared per function;
    /// `block` pushes a statement's error here and continues to the next
    /// statement instead of `?`-aborting the whole body, so every statement-level
    /// error in a function is reported (within a single expression the check is
    /// still first-error — recovery is at the statement boundary, mirroring the
    /// top-level "every function's error, within a function first-error" rule).
    /// A failed `let`/`for` binds its name to [`Type::Err`] so later uses don't
    /// cascade "unknown variable".
    errors: RefCell<Vec<String>>,
    /// Module-state bindings (RFC-0013): name -> (type, mutable). Populated once
    /// (in declaration order) before any function is checked; every function's
    /// scope stack bottoms out on these as an outermost frame below its params.
    /// The declared type is the annotation, or the initializer's inferred type.
    globals: RefCell<HashMap<String, Binding>>,
    /// True while checking a `test` body (RFC-0015). `assert`/`assertEq` are legal
    /// only when this is set; in ordinary code they are a checker error pointing
    /// at validated types / `Result`.
    in_test: RefCell<bool>,
}

/// What an enum variant name resolves to.
struct VariantInfo {
    enum_name: String,
    payload: Vec<Type>,
}

/// A binding's type plus whether it is reassignable.
#[derive(Clone)]
struct Binding {
    ty: Type,
    mutable: bool,
}

impl<'a> Checker<'a> {
    // ---- type relations -------------------------------------------------

    /// The underlying representation type: a validated `Named` type decays to
    /// its base (`Int`/`Bool`); everything else is itself.
    fn base(&self, ty: &Type) -> Type {
        crate::types::resolve(ty, self.types)
    }

    /// The generic parameters of the enum a variant belongs to (empty if the
    /// enum is not generic).
    fn enum_type_params(&self, enum_name: &str) -> Vec<String> {
        self.types.get(enum_name).map(|d| d.type_params.clone()).unwrap_or_default()
    }

    /// Whether a value of type `from` can be used where `to` is expected.
    /// Validated types decay to their base (an `Age` is an `Int`), but the
    /// reverse requires explicit construction.
    fn assignable(&self, from: &Type, to: &Type) -> bool {
        if from == to {
            return true;
        }
        // An `Err` (a recovered type-check failure) is compatible with anything:
        // it should flow through without manufacturing a second diagnostic. This
        // is what keeps inside-body error recovery cascade-free.
        if matches!(from, Type::Err) || matches!(to, Type::Err) {
            return true;
        }
        // A nominal/validated `Named` type decays to its base scalar for reading
        // (an `Age` is an `Int`, a `UserId` is a `String`).
        if let Type::Named(_) = from {
            if matches!(to, Type::Int | Type::Bool | Type::Str) {
                return &self.base(from) == to;
            }
        }
        // Option/Result are covariant in their payloads (values are immutable).
        if let (Type::Option(a), Type::Option(b)) = (from, to) {
            return self.assignable(a, b);
        }
        if let (Type::Result(a, e1), Type::Result(b, e2)) = (from, to) {
            return self.assignable(a, b) && self.assignable(e1, e2);
        }
        // `assignable` is the STRICT relation: a predicated named type admits
        // only itself here. Value boundaries use `coercible`, which adds the
        // automatic-validation rule on top.
        if let Type::Named(n) = to {
            if let Some(d) = self.types.get(n) {
                if d.predicate.is_some() {
                    return matches!(from, Type::Named(m) if m == n);
                }
            }
        }
        // Structural width subtyping: `from` is usable as `to` if it has every
        // field `to` requires, with an assignable type. Extra fields are fine.
        if let (Type::Record(ff), Type::Record(tf)) = (&self.base(from), &self.base(to)) {
            return tf.iter().all(|need| {
                ff.iter()
                    .any(|have| have.name == need.name && self.assignable(&have.ty, &need.ty))
            });
        }
        false
    }

    /// Whether `from` may flow into `to` at a **value boundary** (a `let`
    /// annotation, an assignment, a call argument, a return, a record field, an
    /// array element): everything `assignable` allows, **plus automatic
    /// validation** — a value structurally compatible with a predicated named
    /// type's base may flow in, and the boundary itself runs the `where`
    /// predicate (a provably-false constant is rejected at compile time by
    /// [`Self::prove_coercion`]; anything else is checked at runtime by both
    /// backends, trapping with `validation failed for \`T\``).
    ///
    /// The rule applies at the top level only: a payload inside an
    /// `Option`/`Result`/`Array` *type* does not auto-coerce — each element is
    /// validated at its own literal/argument boundary instead.
    fn coercible(&self, from: &Type, to: &Type) -> bool {
        if self.assignable(from, to) {
            return true;
        }
        if let Type::Named(n) = to {
            if let Some(d) = self.types.get(n) {
                if d.predicate.is_some() {
                    return self.assignable(from, &d.base);
                }
            }
        }
        false
    }

    /// Compile-time half of automatic validation: when a constant expression
    /// flows into a predicated named type, evaluate the predicate now — a
    /// provably-false value is a compile error (RFC-0003's rule: what the
    /// compiler can prove costs nothing and fails early). Non-constant values
    /// pass through to the runtime check. Also proves record literals whose
    /// fields are all constants against a cross-field predicate.
    fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), String> {
        let decl = match to {
            Type::Named(n) => match self.types.get(n) {
                Some(d) if d.predicate.is_some() => d,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let pred = decl.predicate.as_ref().unwrap();
        // Scalar constant: bind `value`.
        if let Some(cv) = consteval::eval(expr, &HashMap::new()) {
            let mut env = HashMap::new();
            env.insert("value".to_string(), cv.clone());
            if consteval::eval(pred, &env).and_then(ConstVal::as_bool) == Some(false) {
                return Err(format!(
                    "line {line}: {cv} does not satisfy `{}` (predicate `where {}` is false)",
                    decl.name,
                    pred_summary(pred),
                ));
            }
            return Ok(());
        }
        // Record literal with all-constant fields: bind each field name.
        if let (Expr::StructLit { fields, .. }, Type::Record(_)) = (expr, &decl.base) {
            let mut env = HashMap::new();
            for (fname, fexpr) in fields {
                match consteval::eval(fexpr, &HashMap::new()) {
                    Some(cv) => {
                        env.insert(fname.clone(), cv);
                    }
                    None => return Ok(()), // not fully constant — runtime check
                }
            }
            if consteval::eval(pred, &env).and_then(ConstVal::as_bool) == Some(false) {
                return Err(format!(
                    "line {line}: this value does not satisfy `{}` (predicate `where {}` \
                     is false)",
                    decl.name,
                    pred_summary(pred),
                ));
            }
        }
        Ok(())
    }

    fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), String> {
        match ty {
            Type::Named(n) => match self.types.get(n) {
                None => return Err(format!("line {line}: unknown type `{n}`")),
                Some(d) if !d.type_params.is_empty() => {
                    return Err(format!(
                        "line {line}: `{n}` is generic; write `{n}<...>` with type arguments"
                    ))
                }
                _ => {}
            },
            Type::App(name, args) => {
                let d = self
                    .types
                    .get(name)
                    .ok_or_else(|| format!("line {line}: unknown type `{name}`"))?;
                if d.type_params.len() != args.len() {
                    return Err(format!(
                        "line {line}: `{name}` takes {} type argument(s), got {}",
                        d.type_params.len(),
                        args.len()
                    ));
                }
                for a in args {
                    self.ensure_type_exists(a, line)?;
                }
            }
            Type::Option(inner) => self.ensure_type_exists(inner, line)?,
            Type::Result(ok, err) => {
                self.ensure_type_exists(ok, line)?;
                self.ensure_type_exists(err, line)?;
            }
            Type::Record(fields) => {
                for f in fields {
                    self.ensure_type_exists(&f.ty, line)?;
                }
            }
            Type::Omit(base, keys) | Type::Pick(base, keys) => {
                self.ensure_type_exists(base, line)?;
                let fields = crate::types::record_fields(base, self.types).ok_or_else(|| {
                    format!("line {line}: the transformer's base must be a record type")
                })?;
                for k in keys {
                    if !fields.iter().any(|f| &f.name == k) {
                        return Err(format!(
                            "line {line}: field `{k}` is not in the transformer's base record"
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
                    return Err(format!("line {line}: `Merge` requires two record types"));
                }
            }
            Type::Partial(base) => {
                self.ensure_type_exists(base, line)?;
                if crate::types::record_fields(base, self.types).is_none() {
                    return Err(format!("line {line}: `Partial` requires a record type"));
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for p in &v.payload {
                        self.ensure_type_exists(p, line)?;
                    }
                }
            }
            // A generic parameter is always valid in the context the parser
            // produced it (it only tags names declared in `<...>`).
            Type::Param(_) => {}
            _ => {}
        }
        Ok(())
    }

    fn check_type_decl(&self, t: &TypeDecl) -> Result<(), String> {
        // Structural record declaration (RFC-0002). A record may carry a `where`
        // clause referencing its fields by name — a cross-field invariant checked
        // at construction (e.g. `{ start: Int, end: Int } where start < end`).
        if let Type::Record(fields) = &t.base {
            let mut seen = std::collections::HashSet::new();
            for f in fields {
                if !seen.insert(&f.name) {
                    return Err(format!(
                        "line {}: duplicate field `{}` in record `{}`",
                        t.line, f.name, t.name
                    ));
                }
                self.ensure_type_exists(&f.ty, t.line)?;
            }
            if let Some(pred) = &t.predicate {
                if consteval::contains_call(pred) {
                    return Err(format!(
                        "line {}: cross-field predicate for `{}` may not contain calls (v0.1)",
                        t.line, t.name
                    ));
                }
                // The predicate sees every field in scope, by name.
                let mut scope: Vec<HashMap<String, Binding>> = vec![HashMap::new()];
                for f in fields {
                    scope[0].insert(f.name.clone(), Binding { ty: f.ty.clone(), mutable: false });
                }
                let pty = self.expr(pred, &scope, None, None)?;
                if self.base(&pty) != Type::Bool {
                    return Err(format!(
                        "line {}: cross-field predicate for `{}` must be Bool, found {pty}",
                        t.line, t.name
                    ));
                }
            }
            return Ok(());
        }
        // Enum declaration (RFC-0002 §4).
        if let Type::Enum(vs) = &t.base {
            if t.predicate.is_some() {
                return Err(format!("line {}: an enum type cannot have a `where` clause", t.line));
            }
            if vs.is_empty() {
                return Err(format!("line {}: enum `{}` has no variants", t.line, t.name));
            }
            for v in vs {
                for p in &v.payload {
                    self.ensure_type_exists(p, t.line)?;
                }
            }
            return Ok(());
        }
        // A transformer alias, e.g. `type Public = Omit<User, password>;`.
        if matches!(t.base, Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..)) {
            if t.predicate.is_some() {
                return Err(format!("line {}: a record type cannot have a `where` clause", t.line));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            if crate::types::record_fields(&t.base, self.types).is_none() {
                return Err(format!("line {}: `{}` does not resolve to a record", t.line, t.name));
            }
            return Ok(());
        }
        // Validated / nominal scalar declaration (RFC-0002 §2, RFC-0003). Numeric
        // bases (Int, sized IntN, Float/Float32) may carry a refinement predicate;
        // a `String` base is allowed but cannot yet (that needs runtime string eval).
        if !matches!(
            t.base,
            Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
        ) {
            return Err(format!(
                "line {}: `{}` must have a scalar base (Int64, sized int, Float64, Bool, or String)",
                t.line, t.name
            ));
        }
        // `String` refinements are allowed (e.g. `value.length >= 3`); like all
        // predicates they must be call-free and const-analyzable (checked below).
        if let Some(pred) = &t.predicate {
            if consteval::contains_call(pred) {
                return Err(format!(
                    "line {}: refinement predicate for `{}` may not contain calls (v0.1)",
                    t.line, t.name
                ));
            }
            // Predicate is checked in an environment where `value` has the base type.
            let mut scope: Vec<HashMap<String, Binding>> = vec![HashMap::new()];
            scope[0].insert("value".into(), Binding { ty: t.base.clone(), mutable: false });
            let pty = self.expr(pred, &scope, None, None)?;
            if self.base(&pty) != Type::Bool {
                return Err(format!(
                    "line {}: refinement predicate for `{}` must be Bool, found {pty}",
                    t.line, t.name
                ));
            }
        }
        Ok(())
    }

    // ---- functions / statements ----------------------------------------

    /// Whether the current function's parameter `t` carries (at least) `bound`,
    /// respecting the implication chain Num ⊇ Ord ⊇ Eq.
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
            // A user protocol bound matches by name.
            other => b == other,
        })
    }

    /// Whether a concrete type satisfies a built-in bound.
    fn type_satisfies(&self, ty: &Type, bound: &str) -> bool {
        let base = self.base(ty);
        match bound {
            "Num" | "Ord" => matches!(base, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }),
            "Eq" => matches!(base, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. } | Type::Bool | Type::Str),
            // A user protocol: satisfied iff the concrete type implements it.
            _ if self.protocol_methods.values().any(|(p, _)| p == bound)
                || self.impls.iter().any(|(p, _)| p == bound) =>
            {
                crate::types::type_key(&base)
                    .map(|k| self.impls.contains(&(bound.to_string(), k)))
                    .unwrap_or(false)
            }
            // Unknown bound names: unsatisfiable.
            _ => false,
        }
    }

    /// Enforce the `extern` ABI type domain (RFC-0012): every parameter type
    /// must be one of `Int64`/sized ints/`Float64`/`Float32`/`Bool`/`String`,
    /// and the return type may additionally be `Unit`. Anything else (a named
    /// validated type, a record, an option, an array, …) cannot cross the JS
    /// boundary in v1 — reject it, naming the offending parameter/return type.
    /// Accumulates every offender (mirroring `function`): the first is the `Err`,
    /// the rest stay in the sink for `check_accum` to drain.
    fn check_extern_sig(&self, f: &Function) -> Result<(), String> {
        self.errors.borrow_mut().clear();
        for p in &f.params {
            if !extern_abi_type_ok(&p.ty, false) {
                self.errors.borrow_mut().push(format!(
                    "line {}: extern fn `{}` parameter `{}` has type {}, which cannot cross \
                     the JS boundary (allowed: Int64, sized ints, Float64, Float32, Bool, String)",
                    f.line, f.name, p.name, p.ty
                ));
            }
        }
        if !extern_abi_type_ok(&f.ret, true) {
            self.errors.borrow_mut().push(format!(
                "line {}: extern fn `{}` returns {}, which cannot cross the JS boundary \
                 (allowed: Int64, sized ints, Float64, Float32, Bool, String, Unit)",
                f.line, f.name, f.ret
            ));
        }
        let mut errs = self.errors.borrow_mut();
        if let Some(first) = errs.first().cloned() {
            let rest: Vec<String> = errs.drain(1..).collect();
            *errs = rest;
            Err(first)
        } else {
            Ok(())
        }
    }

    /// Check every module-state binding (RFC-0013) in declaration order and
    /// record its type in `self.globals`. Each initializer is checked in a scope
    /// containing only the *earlier* globals (so a later-global read is rejected)
    /// and may not call user or extern functions. A failed global still binds (as
    /// `Type::Err`) so function bodies referring to it don't cascade.
    fn check_globals(&self, program: &Program, out: &mut Vec<Diagnostic>) {
        // Names whose call is forbidden in an initializer: every user/extern
        // function and every protocol method. Builtins (`print`, `cell`, `str`,
        // …), constructors (`Some`, enum variants, `Age(n)`) are not in this set.
        let mut forbidden: HashSet<String> =
            program.functions.iter().map(|f| f.name.clone()).collect();
        for p in &program.protocols {
            for m in &p.methods {
                forbidden.insert(m.name.clone());
            }
        }
        let all_globals: HashSet<&str> =
            program.globals.iter().map(|g| g.name.as_str()).collect();
        // Ready-so-far names (the earlier globals) grow as we go.
        let mut ready: HashSet<String> = HashSet::new();
        for g in &program.globals {
            let bty = (|| -> Result<Type, String> {
                // Initializer restrictions (walked before typing so the messages
                // are precise): no user/extern call, no later-global read.
                init_restrictions(&g.init, &forbidden, &all_globals, &ready, &g.name, g.line)?;
                if let Some(declared) = &g.ty {
                    self.ensure_type_exists(declared, g.line)?;
                }
                // Type-check the initializer against the annotation, seeing only
                // the earlier globals.
                let scope: Vec<HashMap<String, Binding>> =
                    vec![self.globals.borrow().clone()];
                let vty = self.expr(&g.init, &scope, g.ty.as_ref(), None)?;
                if self.base(&vty) == Type::Unit {
                    return Err(format!(
                        "line {}: cannot bind module state `{}` to a Unit value",
                        g.line, g.name
                    ));
                }
                if let Some(declared) = &g.ty {
                    if !self.coercible(&vty, declared) {
                        return Err(format!(
                            "line {}: `{}` declared {declared} but initializer is {vty}",
                            g.line, g.name
                        ));
                    }
                    self.prove_coercion(&g.init, declared, g.line)?;
                }
                Ok(g.ty.clone().unwrap_or(vty))
            })();
            let binding = match bty {
                Ok(t) => Binding { ty: t, mutable: g.mutable },
                Err(s) => {
                    let mut d = Diagnostic::from_rendered(s, "check");
                    d.file = g.module.clone();
                    out.push(d);
                    Binding { ty: Type::Err, mutable: g.mutable }
                }
            };
            self.globals.borrow_mut().insert(g.name.clone(), binding);
            ready.insert(g.name.clone());
        }
    }

    fn function(&self, f: &Function) -> Result<(), String> {
        *self.cur_bounds.borrow_mut() = f.type_bounds.clone();
        self.errors.borrow_mut().clear();
        // Frame 0 is the module-state bindings (RFC-0013) — the outermost scope,
        // below the parameters; a local (param/let/for) with the same name
        // shadows a global, since `lookup` walks frames from the top.
        let mut scope: Vec<HashMap<String, Binding>> = vec![self.globals.borrow().clone()];
        scope.push(HashMap::new());
        for p in &f.params {
            // A `modify` parameter is mutable inside the body (that is the point);
            // others are read-only bindings.
            let mutable = p.capability == Capability::Modify;
            scope.last_mut().unwrap().insert(p.name.clone(), Binding { ty: p.ty.clone(), mutable });
        }
        // `block` no longer propagates the first error via `?`; it pushes each
        // statement's error to the `errors` sink and continues, so every
        // statement-level error in the body is reported.
        let returns = self.block(&f.body, &f.ret, &mut scope);
        if f.ret != Type::Unit && !returns {
            // A missing-return diagnostic is reported alongside any body errors
            // (it is about the function as a whole, not one statement).
            self.errors
                .borrow_mut()
                .push(format!("line {}: function `{}` must return {} on all paths", f.line, f.name, f.ret));
        }
        // Surface this function's collected errors as the "result": the first
        // becomes the `Err` (preserving the historical single-error surface for
        // `check()`), and any others are drained into the caller's sink via the
        // `errors` RefCell (already populated) — `check_accum` reads both.
        let mut errs = self.errors.borrow_mut();
        if let Some(first) = errs.first().cloned() {
            // Keep the remaining (2..n) errors in the sink for `check_accum` to
            // drain after this `Err`; drop the first (it's the `Err` payload).
            let rest: Vec<String> = errs.drain(1..).collect();
            *errs = rest;
            Err(first)
        } else {
            Ok(())
        }
    }

    fn block(
        &self,
        block: &Block,
        ret: &Type,
        scope: &mut Vec<HashMap<String, Binding>>,
    ) -> bool {
        scope.push(HashMap::new());
        let mut always_returns = false;
        for stmt in &block.stmts {
            match self.stmt(stmt, ret, scope) {
                Ok(r) => {
                    if r {
                        always_returns = true;
                    }
                }
                Err(msg) => {
                    // Record and continue: the next statement is checked too.
                    self.errors.borrow_mut().push(msg);
                    // Cascade-free recovery: a `let`/`for` that failed still
                    // binds its name to `Type::Err`, so later uses don't spawn
                    // "unknown variable" diagnostics — they flow through
                    // permissively (see `assignable`/`unify`/`binop_type`).
                    self.recover_binding(stmt, scope);
                }
            }
        }
        scope.pop();
        always_returns
    }

    /// Bind the name of a failed `let`/`for`-in to `Type::Err` in the current
    /// scope frame, so subsequent uses don't cascade "unknown variable". Other
    /// statement kinds leave the scope untouched (their effects hadn't applied
    /// yet when they errored). Best-effort: only the simple name is recovered,
    /// not the declared/element type (the check that computes it failed).
    fn recover_binding(&self, stmt: &Stmt, scope: &mut Vec<HashMap<String, Binding>>) {
        match stmt {
            Stmt::Let { name, mutable, .. } => {
                scope
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), Binding { ty: Type::Err, mutable: *mutable });
            }
            Stmt::ForIn { var, .. } => {
                // The loop variable's frame is pushed inside `stmt`'s `ForIn`
                // arm; on error that arm returned before pushing it, so bind in
                // the current (block) frame as a best-effort recovery.
                scope
                    .last_mut()
                    .unwrap()
                    .insert(var.clone(), Binding { ty: Type::Err, mutable: false });
            }
            _ => {}
        }
    }

    fn stmt(
        &self,
        stmt: &Stmt,
        ret: &Type,
        scope: &mut Vec<HashMap<String, Binding>>,
    ) -> Result<bool, String> {
        match stmt {
            Stmt::Let { name, mutable, ty, value, line } => {
                if let Some(declared) = ty {
                    self.ensure_type_exists(declared, *line)?;
                }
                let vty = self.expr(value, scope, ty.as_ref(), Some(ret))?;
                if let Some(declared) = ty {
                    if !self.coercible(&vty, declared) {
                        return Err(format!(
                            "line {line}: `{name}` declared {declared} but initializer is {vty}"
                        ));
                    }
                    self.prove_coercion(value, declared, *line)?;
                }
                if self.base(&vty) == Type::Unit {
                    return Err(format!("line {line}: cannot bind `{name}` to a Unit value"));
                }
                // The binding takes the declared type when present, else the value's.
                let bty = ty.clone().unwrap_or(vty);
                // Retain it for the symbol-query layer so hovering an
                // unannotated `let x = 5` shows `let x: Int`.
                self.let_types.borrow_mut().insert((*line, name.clone()), bty.clone());
                scope
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), Binding { ty: bty, mutable: *mutable });
                Ok(false)
            }
            Stmt::Assign { name, value, line } => {
                let b = self
                    .lookup(scope, name)
                    .ok_or_else(|| format!("line {line}: assignment to unknown variable `{name}`"))?;
                if !b.mutable {
                    return Err(format!(
                        "line {line}: cannot assign to `{name}` (declared without `mut`)"
                    ));
                }
                let vty = self.expr(value, scope, Some(&b.ty), Some(ret))?;
                if !self.coercible(&vty, &b.ty) {
                    return Err(format!(
                        "line {line}: `{name}` is {} but assigned {}",
                        b.ty, vty
                    ));
                }
                self.prove_coercion(value, &b.ty, *line)?;
                self.region_store_guard(name, &b.ty, scope, *line)?;
                Ok(false)
            }
            Stmt::SetField { name, field, value, line } => {
                let b = self.lookup(scope, name).ok_or_else(|| {
                    format!("line {line}: assignment to field of unknown variable `{name}`")
                })?;
                if !b.mutable {
                    return Err(format!(
                        "line {line}: cannot mutate a field of `{name}` (declared without `mut`)"
                    ));
                }
                // Validated data is rebuilt, not mutated: a field write on a
                // record with a cross-field `where` could break the invariant
                // mid-update, so it must go through whole-value reassignment
                // (`r = T { .. }`), which re-validates automatically.
                if let Type::Named(n) = &b.ty {
                    if self.types.get(n).is_some_and(|d| d.predicate.is_some()) {
                        return Err(format!(
                            "line {line}: cannot mutate a field of `{n}` in place (its \
                             `where` invariant could be broken mid-update); rebuild it: \
                             `{name} = {n} {{ .. }}`"
                        ));
                    }
                }
                let fields = crate::types::record_fields(&b.ty, self.types).ok_or_else(|| {
                    format!("line {line}: `{name}` is not a record, so it has no field `{field}`")
                })?;
                let fty = fields
                    .iter()
                    .find(|f| &f.name == field)
                    .map(|f| f.ty.clone())
                    .ok_or_else(|| format!("line {line}: record `{name}` has no field `{field}`"))?;
                // A predicated FIELD type cannot be written in place either: the
                // interpreter's record values are type-erased, so the field's
                // check has no reliable runtime hook there. Only the exact named
                // type (already validated at its own construction) may flow in.
                let field_is_predicated = matches!(&fty, Type::Named(fnm)
                    if self.types.get(fnm).is_some_and(|d| d.predicate.is_some()));
                let vty = self.expr(value, scope, Some(&fty), Some(ret))?;
                if field_is_predicated {
                    if !self.assignable(&vty, &fty) {
                        return Err(format!(
                            "line {line}: field `{field}` is {fty} (validated); assign an \
                             already-constructed `{fty}` value, e.g. `{fty}(..)`"
                        ));
                    }
                } else if !self.coercible(&vty, &fty) {
                    return Err(format!(
                        "line {line}: field `{field}` is {fty} but assigned {vty}"
                    ));
                }
                self.region_store_guard(name, &fty, scope, *line)?;
                Ok(false)
            }
            // `name[index] = value` — in-place element store (RFC-0011). Same
            // `mut` rule as `Assign`/`push`; the index coerces to Int64 and the
            // value coerces into the element type (validated element types are
            // rejected at compile time here via `prove_coercion`, at runtime via
            // the coerce the interpreter/codegen emit on store).
            Stmt::IndexSet { name, index, value, line } => {
                let b = self.lookup(scope, name).ok_or_else(|| {
                    format!("line {line}: index-assignment to unknown variable `{name}`")
                })?;
                if !b.mutable {
                    return Err(format!(
                        "line {line}: cannot store into `{name}` (declared without `mut`)"
                    ));
                }
                let elem = match self.base(&b.ty) {
                    Type::Array(inner) | Type::ArrayN(inner, _) => (*inner).clone(),
                    Type::Err => return Ok(false),
                    other => {
                        return Err(format!(
                            "line {line}: `{name}[i] = ..` needs an Array, found {other}"
                        ))
                    }
                };
                let i = self.base(&self.expr(index, scope, Some(&Type::Int), Some(ret))?);
                if !matches!(i, Type::Int | Type::Err) {
                    return Err(format!(
                        "line {line}: array index must be an Int64, found {i}"
                    ));
                }
                let vty = self.expr(value, scope, Some(&elem), Some(ret))?;
                if !self.coercible(&vty, &elem) {
                    return Err(format!(
                        "line {line}: `{name}` holds {elem} but the stored value is {vty}"
                    ));
                }
                self.prove_coercion(value, &elem, *line)?;
                self.region_store_guard(name, &elem, scope, *line)?;
                Ok(false)
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
                            return Ok(true);
                        }
                    }
                }
                if !self.coercible(&vty, ret) {
                    // Report the mismatch but still count this path as returning:
                    // a `return <wrong type>` does return, so it must NOT also
                    // trigger the "must return on all paths" diagnostic (that
                    // would be a cascade). Push to the sink and return `Ok(true)`.
                    self.errors.borrow_mut().push(format!(
                        "line {line}: return type mismatch: expected {ret}, found {vty}"
                    ));
                    return Ok(true);
                }
                Ok(true)
            }
            Stmt::If { cond, then_block, else_block, line } => {
                let cty = self.expr(cond, scope, None, Some(ret))?;
                if self.base(&cty) != Type::Bool {
                    return Err(format!("line {line}: `if` condition must be Bool, found {cty}"));
                }
                let then_ret = self.block(then_block, ret, scope);
                match else_block {
                    Some(eb) => {
                        let else_ret = self.block(eb, ret, scope);
                        Ok(then_ret && else_ret)
                    }
                    None => Ok(false),
                }
            }
            Stmt::While { cond, body, line } => {
                let cty = self.expr(cond, scope, None, Some(ret))?;
                if self.base(&cty) != Type::Bool {
                    return Err(format!("line {line}: `while` condition must be Bool, found {cty}"));
                }
                self.block(body, ret, scope);
                Ok(false)
            }
            Stmt::ForIn { var, iter, body, line } => {
                let ity = self.expr(iter, scope, None, Some(ret))?;
                let elem = match self.base(&ity) {
                    Type::Array(inner) | Type::ArrayN(inner, _) => (*inner).clone(),
                    // Iterating a String yields each byte as an Int.
                    Type::Str => Type::Int,
                    other => {
                        return Err(format!(
                            "line {line}: `for` needs an Array or String to iterate, found {other}"
                        ))
                    }
                };
                // Bind the loop variable (immutable, element-typed) in a scope
                // frame that wraps the body, so it is not visible after the loop.
                // Retain the element type so `for s in arr` hovers as `for s: Int`.
                self.let_types.borrow_mut().insert((*line, var.clone()), elem.clone());
                scope.push(HashMap::new());
                scope
                    .last_mut()
                    .unwrap()
                    .insert(var.clone(), Binding { ty: elem, mutable: false });
                self.block(body, ret, scope);
                scope.pop();
                // A `for` may run zero times, so it never guarantees a return.
                Ok(false)
            }
            Stmt::Drop { name, line } => {
                // `drop name;` reclaims a heap value. The binding must exist and
                // hold something that owns heap memory. (Use-after-drop is caught
                // separately by move checking, which treats this as a consume.)
                // Module state (RFC-0013) is never dropped — it has module
                // lifetime and is reclaimed only at process exit.
                if self.resolves_to_global(scope, name) {
                    return Err(format!(
                        "line {line}: cannot `drop` module state `{name}` — it lives for the \
                         whole module and is reclaimed at process exit"
                    ));
                }
                let b = self
                    .lookup(scope, name)
                    .ok_or_else(|| format!("line {line}: `drop` of unbound variable `{name}`"))?;
                match self.base(&b.ty) {
                    Type::Str | Type::Array(_) | Type::Ref(_) => Ok(false),
                    other => Err(format!(
                        "line {line}: `drop` needs a heap value (String, Array, or Ref), \
                         but `{name}` is {other}"
                    )),
                }
            }
            Stmt::Expr(e) => {
                self.expr(e, scope, None, Some(ret))?;
                Ok(false)
            }
            Stmt::Region { body, .. } => {
                // Record the frame count at entry so the escape guard can tell
                // region-local bindings (freed at exit) from outer ones.
                self.region_floor.borrow_mut().push(scope.len());
                let body_returns = self.block(body, ret, scope);
                self.region_floor.borrow_mut().pop();
                // Unlike a loop, a region body runs exactly once — if it
                // returns on all paths, so does the enclosing block (both
                // backends already handle the early exit: the arena is leaked,
                // never double-freed).
                Ok(body_returns)
            }
        }
    }

    /// Whether a value of this type can carry a heap allocation (a dynamic
    /// `String`, an `Array` buffer, a `Ref` cell, a `Task` payload, or a
    /// record/enum/Option/Result that transitively contains one).
    /// Used by the `region` escape guard.
    fn contains_heap(&self, ty: &Type) -> bool {
        match self.base(ty) {
            Type::Str => true,
            // Array buffers and the cell slab are always malloc'd (never in the
            // region arena), so only their *contents* can dangle.
            Type::Array(inner) | Type::ArrayN(inner, _) => self.contains_heap(&inner),
            Type::Ref(inner) | Type::Task(inner) => self.contains_heap(&inner),
            Type::Record(fs) => fs.iter().any(|f| self.contains_heap(&f.ty)),
            Type::Enum(vs) => vs.iter().any(|v| v.payload.iter().any(|p| self.contains_heap(p))),
            Type::Option(inner) => self.contains_heap(&inner),
            Type::Result(a, b) => self.contains_heap(&a) || self.contains_heap(&b),
            _ => false,
        }
    }

    /// The `region` escape guard for a store into the binding `name`: inside a
    /// `region`, a heap-carrying value may not be stored into a binding that
    /// outlives the region — it would dangle once the region frees. `stored_ty`
    /// is the type actually being stored (the binding's, a field's, or a call
    /// argument's).
    fn region_store_guard(
        &self,
        name: &str,
        stored_ty: &Type,
        scope: &Vec<HashMap<String, Binding>>,
        line: usize,
    ) -> Result<(), String> {
        if let Some(&floor) = self.region_floor.borrow().last() {
            let idx = scope.iter().rposition(|f| f.contains_key(name));
            if idx.map_or(false, |i| i < floor) && self.contains_heap(stored_ty) {
                return Err(format!(
                    "line {line}: cannot store a heap value into `{name}`, which \
                     outlives the enclosing `region` (it would dangle when the \
                     region frees). Move `{name}` inside the region, or compute a \
                     non-heap result to carry out."
                ));
            }
        }
        Ok(())
    }

    // ---- expressions ----------------------------------------------------

    /// Type-check an expression. `expected` is the type the context wants (used
    /// to infer `None`/`Ok`/`Err` and to target `Some`/`match`). `fn_ret` is the
    /// enclosing function's return type, needed to check the `?` operator.
    fn expr(
        &self,
        expr: &Expr,
        scope: &Vec<HashMap<String, Binding>>,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        match expr {
            // An integer literal takes the expected sized-integer type if there is
            // one (`let x: Int32 = 5`), otherwise the default `Int` — but only if
            // the value actually fits (`let x: Int8 = 300` is an error, not a
            // silent wrap to 44). The lexer parses literals up to u64::MAX by
            // wrapping into the i64 bit pattern, so a *negative* `n` here can only
            // be a literal above i64::MAX — valid solely as a `UInt64`.
            Expr::Int(n) => match expected.map(|t| self.base(t)) {
                Some(t @ Type::IntN { bits, signed }) => {
                    if int_literal_fits(*n, bits, signed) {
                        Ok(t)
                    } else {
                        Err(format!(
                            "integer literal {} does not fit {} (its range is {})",
                            render_int_literal(*n),
                            intn_name(bits, signed),
                            intn_range(bits, signed),
                        ))
                    }
                }
                _ => {
                    if *n < 0 {
                        Err(format!(
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
            // A float literal takes the expected float type (`let x: Float32 = 1.5`),
            // otherwise the default `Float` (f64).
            Expr::Float(_) => Ok(match expected.map(|t| self.base(t)) {
                Some(Type::Float32) => Type::Float32,
                _ => Type::Float,
            }),
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Str(_) => Ok(Type::Str),
            Expr::Var { name, line } => {
                // `None` is the empty-Option constructor, not a variable.
                if name == "None" {
                    return match expected {
                        Some(Type::Option(_)) => Ok(expected.unwrap().clone()),
                        _ => Err(format!(
                            "line {line}: cannot infer the type of `None`; \
                             add an annotation (e.g. `let x: Option<Int64> = None;`)"
                        )),
                    };
                }
                // A nullary enum variant used as a value, e.g. `Empty`.
                if let Some(info) = self.variants.get(name) {
                    if !info.payload.is_empty() {
                        return Err(format!(
                            "line {line}: variant `{name}` needs {} argument(s)",
                            info.payload.len()
                        ));
                    }
                    let tps = self.enum_type_params(&info.enum_name);
                    if tps.is_empty() {
                        return Ok(Type::Named(info.enum_name.clone()));
                    }
                    // Generic enum: a nullary variant needs its type argument from
                    // context (like `None`).
                    return match expected {
                        Some(Type::App(en, _)) if en == &info.enum_name => {
                            Ok(expected.unwrap().clone())
                        }
                        _ => Err(format!(
                            "line {line}: cannot infer the type of `{name}`; add an annotation"
                        )),
                    };
                }
                self.lookup(scope, name)
                    .map(|b| b.ty)
                    .ok_or_else(|| format!("line {line}: unknown variable `{name}`"))
            }
            Expr::Unary { op, expr, line } => {
                // `-9223372036854775808` is the one literal whose magnitude only
                // exists negated (i64::MIN): the bare literal wraps to MIN in the
                // lexer, and negation wraps it straight back — accept it here
                // rather than diagnosing the (unrepresentable) positive half.
                if *op == UnOp::Neg && matches!(**expr, Expr::Int(i64::MIN)) {
                    return Ok(Type::Int);
                }
                let t = self.base(&self.expr(expr, scope, None, fn_ret)?);
                match op {
                    UnOp::Neg if matches!(t, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }) => Ok(t),
                    UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                    UnOp::Neg => Err(format!("line {line}: unary `-` needs a numeric type, found {t}")),
                    UnOp::Not => Err(format!("line {line}: unary `!` needs Bool, found {t}")),
                }
            }
            Expr::Binary { op, lhs, rhs, line } => {
                let mut l = self.base(&self.expr(lhs, scope, None, fn_ret)?);
                let mut r = self.base(&self.expr(rhs, scope, None, fn_ret)?);
                // A plain integer literal adapts to a sized sibling operand, so
                // `x + 5` (x: Int32) and `5 + x` both type-check — but only if it
                // fits (`x < 300` on a UInt8 would otherwise silently truncate
                // 300 to 44 in the comparison).
                if l == Type::Int {
                    if let (Expr::Int(n), Type::IntN { bits, signed }) = (&**lhs, &r) {
                        if !int_literal_fits(*n, *bits, *signed) {
                            return Err(format!(
                                "line {line}: integer literal {} does not fit {} \
                                 (its range is {})",
                                render_int_literal(*n),
                                intn_name(*bits, *signed),
                                intn_range(*bits, *signed),
                            ));
                        }
                        l = r.clone();
                    }
                }
                if r == Type::Int {
                    if let (Expr::Int(n), Type::IntN { bits, signed }) = (&**rhs, &l) {
                        if !int_literal_fits(*n, *bits, *signed) {
                            return Err(format!(
                                "line {line}: integer literal {} does not fit {} \
                                 (its range is {})",
                                render_int_literal(*n),
                                intn_name(*bits, *signed),
                                intn_range(*bits, *signed),
                            ));
                        }
                        r = l.clone();
                    }
                }
                // Likewise a plain float literal adapts to a `Float32` sibling.
                if l == Type::Float && r == Type::Float32 && matches!(**lhs, Expr::Float(_)) {
                    l = Type::Float32;
                }
                if r == Type::Float && l == Type::Float32 && matches!(**rhs, Expr::Float(_)) {
                    r = Type::Float32;
                }
                // `=~` requires the right operand to be a *string literal* (the
                // pattern is compiled to a DFA at compile time) and that pattern
                // must be valid.
                if *op == BinOp::Match {
                    match &**rhs {
                        Expr::Str(pat) => {
                            if let Err(e) = crate::regex::compile(pat) {
                                return Err(format!("line {line}: invalid regex `{pat}`: {e}"));
                            }
                        }
                        _ => {
                            return Err(format!(
                                "line {line}: the right side of `=~` must be a string-literal pattern"
                            ))
                        }
                    }
                }
                self.binop_type(*op, l, r, *line)
            }
            Expr::Call { name, args, line } => self.call(name, args, *line, scope, expected, fn_ret),
            Expr::Match { scrutinee, arms, line } => {
                self.check_match(scrutinee, arms, *line, scope, expected, fn_ret)
            }
            Expr::Try { expr, line } => self.check_try(expr, *line, scope, fn_ret),
            Expr::StructLit { name, fields, line } => {
                self.check_struct_lit(name, fields, *line, scope, fn_ret)
            }
            Expr::Field { expr, field, line } => {
                let ety = self.expr(expr, scope, None, fn_ret)?;
                match self.base(&ety) {
                    // A recovered `Err` receiver yields `Err` — no spurious
                    // "cannot access field" cascade (mirrors the binop guard).
                    Type::Err => Ok(Type::Err),
                    // `arr.length` is the element count (like TS). Sugar for the
                    // `alen` builtin, resolved here so it doesn't shadow record
                    // fields (a `.length` on a record still reads its field).
                    Type::Array(_) | Type::ArrayN(..) if field == "length" => Ok(Type::Int),
                    // `str.length` is the byte length (matches `strlen`/`Str::len`).
                    Type::Str if field == "length" => Ok(Type::Int),
                    Type::Record(rfields) => rfields
                        .iter()
                        .find(|f| &f.name == field)
                        .map(|f| f.ty.clone())
                        .ok_or_else(|| {
                            format!("line {line}: type {ety} has no field `{field}`")
                        }),
                    other => Err(format!(
                        "line {line}: cannot access field `{field}` on non-record type {other}"
                    )),
                }
            }
            Expr::TryConstruct { name, args, line } => {
                let base = match self.types.get(name) {
                    Some(d) if matches!(d.base, Type::Int | Type::Bool | Type::Str) => d.base.clone(),
                    Some(_) => {
                        return Err(format!(
                            "line {line}: `{name}?(..)` is only for validated/nominal scalar types"
                        ))
                    }
                    None => return Err(format!("line {line}: unknown type `{name}`")),
                };
                if args.len() != 1 {
                    return Err(format!("line {line}: `{name}?` takes 1 argument, got {}", args.len()));
                }
                let aty = self.expr(&args[0], scope, Some(&base), fn_ret)?;
                if !self.assignable(&aty, &base) {
                    return Err(format!(
                        "line {line}: `{name}` is built from {base}, but the argument is {aty}"
                    ));
                }
                Ok(Type::Option(Box::new(Type::Named(name.clone()))))
            }
            Expr::Spawn { name, args, line } => {
                let (params, ret) = self.sigs.get(name).ok_or_else(|| {
                    format!("line {line}: cannot spawn unknown function `{name}`")
                })?;
                if !self.spawn_safe.contains(name) {
                    return Err(format!(
                        "line {line}: `spawn {name}(..)` is not allowed: `{name}` (or something it \
                         calls) does I/O or touches shared mutable state, so running it as a task \
                         could race or interleave. A spawned function must be isolated (pure)."
                    ));
                }
                if params.len() != args.len() {
                    return Err(format!(
                        "line {line}: `{name}` expects {} argument(s), got {}",
                        params.len(),
                        args.len()
                    ));
                }
                for (arg, pty) in args.iter().zip(params) {
                    let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                    if !self.coercible(&aty, pty) {
                        return Err(format!(
                            "line {line}: `spawn {name}` argument expects {pty}, found {aty}"
                        ));
                    }
                    self.prove_coercion(arg, pty, *line)?;
                }
                Ok(Type::Task(Box::new(ret.clone())))
            }
            Expr::ArrayLit { elems, line } => {
                // An empty `[]` has no elements to infer from — it is a growable
                // empty array (like `array()`), with its element type taken from
                // the expected type.
                if elems.is_empty() {
                    return match expected {
                        Some(Type::Array(t)) => Ok(Type::Array(t.clone())),
                        _ => Err(format!(
                            "line {line}: cannot infer the element type of `[]`; annotate it, \
                             e.g. `let a: Array<Int64> = [];`"
                        )),
                    };
                }
                // All elements share a type. In a context expecting a growable
                // `Array<T>` the literal *is* that heap array directly (replacing
                // the old `list([..])`); otherwise it is a fixed-size `Array<T,N>`.
                // The `Array<T>` route is restricted to literal expressions here,
                // so a stack-allocated `ArrayN` value can never silently alias
                // into a heap array (the codegen copies element-wise).
                let (elem_expected, growable) = match expected {
                    Some(Type::ArrayN(t, _)) => (Some((**t).clone()), false),
                    Some(Type::Array(t)) => (Some((**t).clone()), true),
                    _ => (None, false),
                };
                let first = self.expr(&elems[0], scope, elem_expected.as_ref(), fn_ret)?;
                let elem_ty = elem_expected.unwrap_or(first.clone());
                // Every element (including the first) is a value boundary into
                // the element type — auto-validated when it is predicated.
                if !self.coercible(&first, &elem_ty) {
                    return Err(format!(
                        "line {line}: array elements must share a type: expected {elem_ty},                          found {first}"
                    ));
                }
                self.prove_coercion(&elems[0], &elem_ty, *line)?;
                for e in &elems[1..] {
                    let t = self.expr(e, scope, Some(&elem_ty), fn_ret)?;
                    if !self.coercible(&t, &elem_ty) {
                        return Err(format!(
                            "line {line}: array elements must share a type: expected {elem_ty}, found {t}"
                        ));
                    }
                    self.prove_coercion(e, &elem_ty, *line)?;
                }
                if growable {
                    Ok(Type::Array(Box::new(elem_ty)))
                } else {
                    Ok(Type::ArrayN(Box::new(elem_ty), elems.len()))
                }
            }
        }
    }

    /// Check `Name { field: expr, ... }`: `Name` must be a record type, every
    /// field present exactly once, each value assignable to its field's type.
    fn check_struct_lit(
        &self,
        name: &str,
        fields: &[(String, Expr)],
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let decl = self
            .types
            .get(name)
            .ok_or_else(|| format!("line {line}: unknown type `{name}`"))?;
        // Field types (they may mention this type's generic parameters).
        let rfields = crate::types::record_fields(&Type::Named(name.to_string()), self.types)
            .ok_or_else(|| format!("line {line}: `{name}` is not a record type"))?;
        // Each provided field must exist and typecheck; generic parameters are
        // inferred from the field values (there is no turbofish in expressions).
        let mut provided = std::collections::HashSet::new();
        let mut subst: HashMap<String, Type> = HashMap::new();
        for (fname, value) in fields {
            let field = rfields
                .iter()
                .find(|f| &f.name == fname)
                .ok_or_else(|| format!("line {line}: record `{name}` has no field `{fname}`"))?;
            if !provided.insert(fname.clone()) {
                return Err(format!("line {line}: field `{fname}` set twice"));
            }
            let vty = self.expr(value, scope, Some(&field.ty), fn_ret)?;
            self.unify(&field.ty, &vty, &mut subst, line)?;
            self.prove_coercion(value, &field.ty, line)?;
        }
        // Every declared field must be provided.
        for f in &rfields {
            if !provided.contains(&f.name) {
                return Err(format!("line {line}: missing field `{}` for `{name}`", f.name));
            }
        }
        // Cross-field predicate: if every field is a compile-time constant, the
        // invariant is checked now and a provable violation is a compile error.
        if let Some(pred) = &decl.predicate {
            let mut env = HashMap::new();
            let mut all_const = true;
            for (fname, value) in fields {
                match consteval::eval(value, &HashMap::new()) {
                    Some(cv) => {
                        env.insert(fname.clone(), cv);
                    }
                    None => {
                        all_const = false;
                        break;
                    }
                }
            }
            if all_const {
                if let Some(false) = consteval::eval(pred, &env).and_then(ConstVal::as_bool) {
                    return Err(format!(
                        "line {line}: `{name} {{ .. }}` violates `where {}`",
                        pred_summary(pred)
                    ));
                }
            }
        }
        if decl.type_params.is_empty() {
            return Ok(Type::Named(name.to_string()));
        }
        for tp in &decl.type_params {
            if !subst.contains_key(tp) {
                return Err(format!(
                    "line {line}: cannot infer type parameter `{tp}` of `{name}`"
                ));
            }
        }
        let args = decl.type_params.iter().map(|tp| subst[tp].clone()).collect();
        Ok(Type::App(name.to_string(), args))
    }

    /// Check `expr?`: `expr` must be an `Option`/`Result`, and the enclosing
    /// function must return a matching `Option`/`Result` so the `None`/`Err` can
    /// be propagated. Yields the unwrapped payload type.
    fn check_try(
        &self,
        expr: &Expr,
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let ety = self.expr(expr, scope, None, fn_ret)?;
        let ret = fn_ret.ok_or_else(|| {
            format!("line {line}: `?` can only be used inside a function")
        })?;
        match &ety {
            Type::Option(t) => match ret {
                Type::Option(_) => Ok((**t).clone()),
                _ => Err(format!(
                    "line {line}: `?` on an Option requires the function to return Option, \
                     but it returns {ret}"
                )),
            },
            Type::Result(t, e) => match ret {
                Type::Result(_, re) if self.assignable(e, re) => Ok((**t).clone()),
                Type::Result(_, re) => Err(format!(
                    "line {line}: `?` propagates error {e}, but the function returns \
                     Result<_, {re}>"
                )),
                _ => Err(format!(
                    "line {line}: `?` on a Result requires the function to return Result, \
                     but it returns {ret}"
                )),
            },
            other => Err(format!("line {line}: `?` needs an Option or Result, found {other}")),
        }
    }

    /// Check a `match` over an `Option` or `Result`: both variants covered
    /// exactly once with the right patterns, all arm bodies a common type.
    fn check_match(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let sty = self.expr(scrutinee, scope, None, fn_ret)?;
        // A user enum dispatches to its own (N-variant) checker.
        if let Type::Enum(evs) = self.base(&sty) {
            return self.check_match_enum(&sty, &evs, arms, line, scope, expected, fn_ret);
        }
        // The two patterns an Option/Result scrutinee requires.
        let want: [&str; 2] = match &sty {
            Type::Option(_) => ["Some", "None"],
            Type::Result(_, _) => ["Ok", "Err"],
            other => {
                return Err(format!(
                    "line {line}: `match` scrutinee must be an Option, Result, or enum, found {other}"
                ))
            }
        };
        let mut seen: Vec<&str> = Vec::new();
        let mut result: Option<Type> = expected.cloned();
        for arm in arms {
            let (tag, bind): (&str, Option<&str>) = match &arm.pattern {
                Pattern::Some(b) => ("Some", Some(b)),
                Pattern::None => ("None", None),
                Pattern::Ok(b) => ("Ok", Some(b)),
                Pattern::Err(b) => ("Err", Some(b)),
                Pattern::Variant(n, _) => {
                    return Err(format!(
                        "line {line}: pattern `{n}` does not match scrutinee of type {sty}"
                    ))
                }
            };
            if !want.contains(&tag) {
                return Err(format!(
                    "line {line}: pattern `{tag}` does not match scrutinee of type {sty}"
                ));
            }
            if seen.contains(&tag) {
                return Err(format!("line {line}: duplicate `{tag}` arm"));
            }
            seen.push(tag);

            let mut inner_scope = scope.clone();
            if let Some(name) = bind {
                let bty = self.binding_type(&sty, tag);
                inner_scope.push(HashMap::new());
                inner_scope
                    .last_mut()
                    .unwrap()
                    .insert(name.to_string(), Binding { ty: bty, mutable: false });
            }
            let bty = self.expr(&arm.body, &inner_scope, result.as_ref(), fn_ret)?;
            self.unify_arm(&mut result, bty, line)?;
        }
        if !want.iter().all(|w| seen.contains(w)) {
            return Err(format!(
                "line {line}: `match` must cover both `{}` and `{}`",
                want[0], want[1]
            ));
        }
        result.ok_or_else(|| format!("line {line}: empty `match`"))
    }

    /// Check a `match` over a user enum: every arm a valid variant pattern,
    /// bindings matching payloads, all variants covered exactly once.
    fn check_match_enum(
        &self,
        sty: &Type,
        evs: &[EnumVariant],
        arms: &[MatchArm],
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let mut seen: Vec<String> = Vec::new();
        let mut result: Option<Type> = expected.cloned();
        for arm in arms {
            let (vname, bind) = match &arm.pattern {
                Pattern::Variant(n, b) => (n.clone(), b.clone()),
                _ => return Err(format!("line {line}: expected an enum variant pattern")),
            };
            let ev = evs
                .iter()
                .find(|v| v.name == vname)
                .ok_or_else(|| format!("line {line}: `{vname}` is not a variant of {sty}"))?;
            if seen.contains(&vname) {
                return Err(format!("line {line}: duplicate `{vname}` arm"));
            }
            seen.push(vname.clone());

            if ev.payload.len() != bind.len() {
                return Err(format!(
                    "line {line}: variant `{vname}` has {} payload(s), but the pattern binds {}",
                    ev.payload.len(),
                    bind.len()
                ));
            }
            let mut inner = scope.clone();
            if !bind.is_empty() {
                inner.push(HashMap::new());
                for (bname, pty) in bind.iter().zip(&ev.payload) {
                    inner
                        .last_mut()
                        .unwrap()
                        .insert(bname.clone(), Binding { ty: pty.clone(), mutable: false });
                }
            }
            let bty = self.expr(&arm.body, &inner, result.as_ref(), fn_ret)?;
            self.unify_arm(&mut result, bty, line)?;
        }
        for v in evs {
            if !seen.contains(&v.name) {
                return Err(format!("line {line}: `match` is missing variant `{}`", v.name));
            }
        }
        result.ok_or_else(|| format!("line {line}: empty `match`"))
    }

    /// Fold an arm's body type into the match's result type.
    fn unify_arm(&self, result: &mut Option<Type>, bty: Type, line: usize) -> Result<(), String> {
        match result {
            None => *result = Some(bty),
            Some(rt) => {
                if self.assignable(&bty, rt) {
                    // The arm fits the current result type — keep it.
                } else if self.assignable(rt, &bty) {
                    // The current result only fits the arm's WIDER type (e.g. a
                    // validated `Age` arm meeting a raw-`Int` arm): the join is
                    // the wider type. Keeping the narrow named type would
                    // launder the raw arm past its refinement without any check.
                    *result = Some(bty);
                } else {
                    return Err(format!(
                        "line {line}: `match` arms have differing types: {rt} vs {bty}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The type bound by pattern `tag` when matching a value of type `sty`.
    fn binding_type(&self, sty: &Type, tag: &str) -> Type {
        match (sty, tag) {
            (Type::Option(t), "Some") => (**t).clone(),
            (Type::Result(t, _), "Ok") => (**t).clone(),
            (Type::Result(_, e), "Err") => (**e).clone(),
            _ => Type::Unit,
        }
    }

    fn binop_type(&self, op: BinOp, l: Type, r: Type, line: usize) -> Result<Type, String> {
        use BinOp::*;
        // A recovered `Err` operand yields `Err` without a spurious "needs Int"
        // diagnostic (cascade-free recovery).
        if matches!(l, Type::Err) || matches!(r, Type::Err) {
            return Ok(Type::Err);
        }
        // Operators on a bounded generic parameter: both operands must be the
        // same `Param`, and the required bound must be present.
        if let Type::Param(t) = &l {
            if &r != &l {
                return Err(format!(
                    "line {line}: cannot combine type parameter `{t}` with {r}"
                ));
            }
            return match op {
                Add | Sub | Mul | Div | Rem if self.param_has_bound(t, "Num") => {
                    Ok(Type::Param(t.clone()))
                }
                Lt | LtEq | Gt | GtEq if self.param_has_bound(t, "Ord") => Ok(Type::Bool),
                Eq | NotEq if self.param_has_bound(t, "Eq") => Ok(Type::Bool),
                Add | Sub | Mul | Div | Rem => {
                    Err(format!("line {line}: `{t}` needs a `Num` bound for arithmetic"))
                }
                Lt | LtEq | Gt | GtEq => {
                    Err(format!("line {line}: `{t}` needs an `Ord` bound to compare"))
                }
                Eq | NotEq => Err(format!("line {line}: `{t}` needs an `Eq` bound")),
                And | Or => Err(format!("line {line}: `&&`/`||` need Bool operands")),
                Match => Err(format!("line {line}: `=~` needs a String operand, not `{t}`")),
            };
        }
        let numeric = |t: &Type| matches!(t, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. });
        match op {
            // `+` on two Strings is concatenation (replacing the old `concat`
            // builtin); it lowers to the same heap allocation and drop analysis.
            Add if l == Type::Str && r == Type::Str => Ok(Type::Str),
            // Arithmetic works on Int, Float, or a sized integer; operands must
            // match exactly (no implicit widening). `Rem` (%) is integer-only.
            Add | Sub | Mul | Div => {
                if l == r && numeric(&l) {
                    Ok(l)
                } else if op == Add && (l == Type::Str || r == Type::Str) {
                    Err(format!(
                        "line {line}: `+` concatenates two Strings, found {l} and {r}"
                    ))
                } else {
                    Err(format!(
                        "line {line}: arithmetic needs matching numeric operands, \
                         found {l} and {r}"
                    ))
                }
            }
            Rem => {
                if l == r && matches!(l, Type::Int | Type::IntN { .. }) {
                    Ok(l)
                } else {
                    Err(format!("line {line}: `%` needs matching integer operands, found {l} and {r}"))
                }
            }
            Lt | LtEq | Gt | GtEq => {
                if l == r && numeric(&l) {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: comparison needs matching numeric operands, \
                         found {l} and {r}"
                    ))
                }
            }
            Eq | NotEq => {
                if l == r && (numeric(&l) || matches!(l, Type::Bool | Type::Str)) {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: `==`/`!=` needs matching scalar operands, found {l} and {r}"
                    ))
                }
            }
            And | Or => {
                if l == Type::Bool && r == Type::Bool {
                    Ok(Type::Bool)
                } else {
                    Err(format!("line {line}: `&&`/`||` needs Bool operands, found {l} and {r}"))
                }
            }
            // `=~` matches a String against a regex literal → Bool (the literal
            // requirement and pattern validity are checked at the `Expr::Binary`
            // site, which has the syntax).
            Match => {
                if l == Type::Str && r == Type::Str {
                    Ok(Type::Bool)
                } else {
                    Err(format!("line {line}: `=~` needs a String and a pattern, found {l} and {r}"))
                }
            }
        }
    }

    fn call(
        &self,
        name: &str,
        args: &[Expr],
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        // Removed free-function builtins → their method/operator replacements.
        // These fire only for the *bare* user-written spelling; the desugaring
        // and method forms use the unspellable `@`-prefixed internal names
        // (`@str`/`@concat`/`@list`/`@join`), which flow past this guard.
        match name {
            "str" => {
                return Err(format!(
                    "line {line}: `str(x)` was removed; render a value with `x.toString()`"
                ))
            }
            "concat" => {
                return Err(format!(
                    "line {line}: `concat(a, b)` was removed; concatenate Strings with `a + b`"
                ))
            }
            "len" => {
                return Err(format!(
                    "line {line}: `len(s)` was removed; a String's byte length is `s.length`"
                ))
            }
            "list" => {
                return Err(format!(
                    "line {line}: `list([..])` was removed; write the array literal `[..]` \
                     directly where an `Array<T>` is expected"
                ))
            }
            "join" => {
                return Err(format!(
                    "line {line}: `join(t)` was removed; await a task's result with `t.join()`"
                ))
            }
            "toString" => {
                return Err(format!(
                    "line {line}: `toString` is a method; write `x.toString()`"
                ))
            }
            _ => {}
        }
        // Test builtins (RFC-0015): `assert`/`assertEq` are legal ONLY inside a
        // `test` body. In ordinary code they are a checker error steering the
        // programmer to the production tools (validated types / `Result`).
        if name == "assert" || name == "assertEq" {
            if !*self.in_test.borrow() {
                return Err(format!(
                    "line {line}: `{name}` is only available inside a `test` block — in ordinary \
                     code, use a validated type or return a `Result` to signal failure"
                ));
            }
            if name == "assert" {
                if args.len() != 1 {
                    return Err(format!(
                        "line {line}: `assert` takes 1 Bool argument, got {}",
                        args.len()
                    ));
                }
                let t = self.base(&self.expr(&args[0], scope, Some(&Type::Bool), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Unit);
                }
                if t != Type::Bool {
                    return Err(format!("line {line}: `assert` needs a Bool, found {t}"));
                }
                return Ok(Type::Unit);
            }
            // assertEq(a, b): both sides the same equatable scalar type.
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `assertEq` takes 2 arguments, got {}",
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
                    Type::Int | Type::Float | Type::Float32 | Type::IntN { .. } | Type::Bool | Type::Str
                )
            };
            if a != b || !equatable(&a) {
                return Err(format!(
                    "line {line}: `assertEq` needs two equal, equatable values, found {a} and {b}"
                ));
            }
            return Ok(Type::Unit);
        }

        // built-in: print(Int|Bool) -> Unit
        if name == "print" {
            if args.len() != 1 {
                return Err(format!("line {line}: print expects 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(t, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. } | Type::Bool | Type::Str) {
                return Err(format!(
                    "line {line}: print needs a number, Bool, or String, found {t}"
                ));
            }
            return Ok(Type::Unit);
        }

        // built-in: logger(String) -> Logger (RFC-0008).
        if name == "logger" {
            if args.len() != 1 {
                return Err(format!("line {line}: `logger` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `logger` needs a String name, found {t}"));
            }
            return Ok(Type::Logger);
        }
        // built-in log methods: <level>(Logger, String) -> Unit. Written
        // subject-first via method sugar: `log.info("..")`.
        if matches!(name, "trace" | "debug" | "info" | "warn" | "error") {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `{name}` takes a Logger and a String, got {} argument(s)",
                    args.len()
                ));
            }
            let l = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(l, Type::Err) {
                return Ok(Type::Err);
            }
            if l != Type::Logger {
                return Err(format!(
                    "line {line}: `{name}` must be called on a Logger (e.g. `log.{name}(..)`), \
                     found {l}"
                ));
            }
            let m = self.base(&self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?);
            if matches!(m, Type::Err) {
                return Ok(Type::Err);
            }
            if m != Type::Str {
                return Err(format!("line {line}: `{name}` message must be a String, found {m}"));
            }
            return Ok(Type::Unit);
        }

        // Input I/O effects (RFC-0014). Free builtins like `print`/`logger`; each
        // joins `SPAWN_FORBIDDEN` and is never constant (`Expr::Call` never folds).
        // Error payloads are canonical Vyrn wording (never OS text) — the parity
        // rule; the strings are built at the use site in the interpreter and by
        // the codegen, kept byte-identical.
        if name == "args" {
            if !args.is_empty() {
                return Err(format!("line {line}: `args` takes no arguments, got {}", args.len()));
            }
            return Ok(Type::Array(Box::new(Type::Str)));
        }
        if name == "readLine" {
            if !args.is_empty() {
                return Err(format!(
                    "line {line}: `readLine` takes no arguments, got {}",
                    args.len()
                ));
            }
            return Ok(Type::Option(Box::new(Type::Str)));
        }
        if name == "readFile" {
            if args.len() != 1 {
                return Err(format!("line {line}: `readFile` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `readFile` needs a String path, found {t}"));
            }
            return Ok(Type::Result(Box::new(Type::Str), Box::new(Type::Str)));
        }
        if name == "writeFile" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `writeFile` takes 2 arguments, got {}",
                    args.len()
                ));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, Some(&Type::Str), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!("line {line}: `writeFile` needs String arguments, found {t}"));
                }
            }
            return Ok(Type::Result(Box::new(Type::Bool), Box::new(Type::Str)));
        }
        // RFC-0014 M2 (bytes): binary read + the byte<->String bridge.
        if name == "readFileBytes" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `readFileBytes` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `readFileBytes` needs a String path, found {t}"));
            }
            return Ok(Type::Result(
                Box::new(Type::Array(Box::new(Type::IntN { bits: 8, signed: false }))),
                Box::new(Type::Str),
            ));
        }
        if name == "stringFromBytes" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `stringFromBytes` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let want = Type::Array(Box::new(Type::IntN { bits: 8, signed: false }));
            let t = self.base(&self.expr(&args[0], scope, Some(&want), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != want {
                return Err(format!(
                    "line {line}: `stringFromBytes` needs an Array<UInt8>, found {t}"
                ));
            }
            return Ok(Type::Result(Box::new(Type::Str), Box::new(Type::Str)));
        }

        // (`len(String)` was removed — see the migration hint above; its byte
        // length now lives on the `String.length` field, resolved at `Expr::Field`.)

        // built-in string predicates: contains / startsWith / endsWith (via UFCS
        // also `s.contains(sub)` etc.). Each takes two Strings and yields a Bool.
        if matches!(name, "contains" | "startsWith" | "endsWith") {
            if args.len() != 2 {
                return Err(format!("line {line}: `{name}` takes 2 arguments, got {}", args.len()));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, Some(&Type::Str), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!("line {line}: `{name}` needs String arguments, found {t}"));
                }
            }
            return Ok(Type::Bool);
        }

        // Text encodings. Encoders: String -> String. Decoders: String ->
        // Option<String> (None on malformed input or a non-UTF-8 result).
        if matches!(name, "hexEncode" | "base64Encode" | "urlEncode") {
            if args.len() != 1 {
                return Err(format!("line {line}: `{name}` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `{name}` needs a String, found {t}"));
            }
            return Ok(Type::Str);
        }
        if matches!(name, "hexDecode" | "base64Decode" | "urlDecode") {
            if args.len() != 1 {
                return Err(format!("line {line}: `{name}` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `{name}` needs a String, found {t}"));
            }
            return Ok(Type::Option(Box::new(Type::Str)));
        }

        // built-in: bytes(String) -> Array<UInt8> (the raw UTF-8 bytes — RFC-0014
        // M2; `stringFromBytes` is the fallible inverse) and chars(String) ->
        // Array<Int> (the Unicode scalar values / code points).
        if matches!(name, "bytes" | "chars") {
            if args.len() != 1 {
                return Err(format!("line {line}: `{name}` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `{name}` needs a String, found {t}"));
            }
            let elem =
                if name == "bytes" { Type::IntN { bits: 8, signed: false } } else { Type::Int };
            return Ok(Type::Array(Box::new(elem)));
        }

        // Internal string concat (`a + b` on Strings, and interpolation): the
        // `@concat` spelling is produced by the desugarer / the `+` lowering,
        // never by user source. Heap-allocated result.
        if name == "@concat" {
            if args.len() != 2 {
                return Err(format!("line {line}: `@concat` takes 2 arguments, got {}", args.len()));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, None, fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!("line {line}: `@concat` needs Strings, found {t}"));
                }
            }
            return Ok(Type::Str);
        }

        // `@join` — the internal spelling of `t.join()`: await a spawned task.
        if name == "@join" {
            if args.len() != 1 {
                return Err(format!("line {line}: `join` takes no arguments"));
            }
            match self.base(&self.expr(&args[0], scope, None, fn_ret)?) {
                Type::Task(inner) => return Ok((*inner).clone()),
                Type::Err => return Ok(Type::Err),
                other => return Err(format!("line {line}: `.join()` needs a Task, found {other}")),
            }
        }

        // `@str` — the internal spelling of `x.toString()` and of interpolation
        // holes: render a scalar to a fresh String. `parse` (below) is the
        // fallible inverse.
        if name == "@str" {
            if args.len() != 1 {
                return Err(format!("line {line}: `toString` takes no arguments"));
            }
            // `str` renders a scalar to a fresh String — Int, sized IntN, Float,
            // Bool, or String (String is copied). Interpolation lowers to this.
            let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(
                t,
                Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
            ) {
                return Err(format!(
                    "line {line}: `toString` renders a number, Bool, or String, found {t}"
                ));
            }
            return Ok(Type::Str);
        }
        if name == "parse" {
            if args.len() != 1 {
                return Err(format!("line {line}: `parse` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `parse` needs a String, found {t}"));
            }
            return Ok(Type::Option(Box::new(Type::Int)));
        }

        // built-in: generational references (RFC-0004 §4, Path B).
        //   cell(Int) -> Ref    get(Ref) -> Int    set(Ref, Int) -> Unit
        //   release(Ref) -> Unit
        // cell(v: T) -> Ref<T> — the element type is inferred from `v`.
        if name == "cell" {
            if args.len() != 1 {
                return Err(format!("line {line}: `cell` takes 1 argument, got {}", args.len()));
            }
            let elem_expected = match expected {
                Some(Type::Ref(t)) => Some((**t).clone()),
                _ => None,
            };
            let t = self.expr(&args[0], scope, elem_expected.as_ref(), fn_ret)?;
            if matches!(self.base(&t), Type::Err) {
                return Ok(Type::Err);
            }
            if self.base(&t) == Type::Unit {
                return Err(format!("line {line}: `cell` cannot hold a Unit value"));
            }
            return Ok(Type::Ref(Box::new(elem_expected.unwrap_or(t))));
        }
        // get(r: Ref<T>) -> T
        if name == "get" {
            if args.len() != 1 {
                return Err(format!("line {line}: `get` takes 1 argument, got {}", args.len()));
            }
            let rt = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&rt) {
                Type::Ref(inner) => return Ok((*inner).clone()),
                Type::Err => return Ok(Type::Err),
                other => return Err(format!("line {line}: `get` needs a Ref, found {other}")),
            }
        }
        // set(r: Ref<T>, v: T) -> Unit
        if name == "set" {
            if args.len() != 2 {
                return Err(format!("line {line}: `set` takes 2 arguments, got {}", args.len()));
            }
            let rt = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&rt) {
                Type::Ref(inner) => (*inner).clone(),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: `set` needs a Ref as its first argument, found {other}"
                    ))
                }
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.coercible(&v, &elem) {
                return Err(format!(
                    "line {line}: `set` value is {v} but the cell holds {elem}"
                ));
            }
            self.prove_coercion(&args[1], &elem, line)?;
            // A store through a cell is a store into wherever the cell lives:
            // `set(outer, <heap>)` inside a `region` would dangle at exit.
            if let Expr::Var { name: cname, .. } = &args[0] {
                self.region_store_guard(cname, &elem, scope, line)?;
            }
            return Ok(Type::Unit);
        }
        // release(r: Ref<T>) -> Unit
        if name == "release" {
            if args.len() != 1 {
                return Err(format!("line {line}: `release` takes 1 argument, got {}", args.len()));
            }
            let rt = self.expr(&args[0], scope, None, fn_ret)?;
            let rt = self.base(&rt);
            if matches!(rt, Type::Err) {
                return Ok(Type::Unit);
            }
            if !matches!(rt, Type::Ref(_)) {
                return Err(format!("line {line}: `release` needs a Ref, found {rt}"));
            }
            return Ok(Type::Unit);
        }

        // Growable arrays: array() -> Array<T> (T from context), push(Array<T>, T)
        // -> Array<T>, at(Array<T>, Int) -> T, alen(Array<T>) -> Int.
        if name == "array" {
            if !args.is_empty() {
                return Err(format!("line {line}: `array` takes no arguments, got {}", args.len()));
            }
            match expected {
                Some(Type::Array(t)) => return Ok(Type::Array(t.clone())),
                _ => {
                    return Err(format!(
                        "line {line}: cannot infer the element type of `array()`; annotate it, \
                         e.g. `let a: Array<Int64> = array();`"
                    ))
                }
            }
        }
        if name == "push" {
            if args.len() != 2 {
                return Err(format!("line {line}: `push` takes 2 arguments, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&at) {
                Type::Array(inner) => (*inner).clone(),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: `push` needs an Array as its first argument, found {other}"
                    ))
                }
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.coercible(&v, &elem) {
                return Err(format!(
                    "line {line}: `push` value is {v} but the array holds {elem}"
                ));
            }
            self.prove_coercion(&args[1], &elem, line)?;
            // `push(outer, <heap elem>)` inside a `region` stores a value that
            // dies with the region into a buffer that outlives it (the rebind
            // form `a = push(a, ..)` is caught by the Assign guard; this catches
            // the statement/method form). The buffer itself is malloc'd, so
            // pushing a non-heap element is fine.
            if let Expr::Var { name: aname, .. } = &args[0] {
                self.region_store_guard(aname, &elem, scope, line)?;
            }
            return Ok(Type::Array(Box::new(elem)));
        }
        if name == "at" {
            if args.len() != 2 {
                return Err(format!("line {line}: `at` takes 2 arguments, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&at) {
                Type::Array(inner) | Type::ArrayN(inner, _) => (*inner).clone(),
                // `s[i]` on a String yields the byte at that index as an Int.
                Type::Str => Type::Int,
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: indexing needs an Array or String, found {other}"
                    ))
                }
            };
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if matches!(i, Type::Err) {
                return Ok(Type::Err);
            }
            if i != Type::Int {
                return Err(format!("line {line}: `at` index must be an Int64, found {i}"));
            }
            return Ok(elem);
        }
        if name == "alen" {
            if args.len() != 1 {
                return Err(format!("line {line}: `alen` takes 1 argument, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let at = self.base(&at);
            if matches!(at, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(at, Type::Array(_) | Type::ArrayN(..)) {
                return Err(format!("line {line}: `alen` needs an Array, found {at}"));
            }
            return Ok(Type::Int);
        }
        if name == "afree" {
            if args.len() != 1 {
                return Err(format!("line {line}: `afree` takes 1 argument, got {}", args.len()));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let at = self.base(&at);
            if matches!(at, Type::Err) {
                return Ok(Type::Unit);
            }
            if !matches!(at, Type::Array(_)) {
                return Err(format!("line {line}: `afree` needs an Array, found {at}"));
            }
            return Ok(Type::Unit);
        }
        // `a.pop()` (RFC-0011) — remove and return the last element as
        // `Option<T>`. Method-only (`@pop`); the receiver must be a `mut`
        // `Array<T>` binding. A fixed-size `Array<T, N>` cannot shrink, so it is
        // rejected with a message naming `Array<T>`.
        if name == "@pop" {
            if args.len() != 1 {
                return Err(format!("line {line}: `pop` takes no arguments"));
            }
            let elem = self.mut_array_receiver(&args[0], scope, line, "pop")?;
            return Ok(match elem {
                Type::Err => Type::Err,
                t => Type::Option(Box::new(t)),
            });
        }
        // `a.swapRemove(i)` (RFC-0011) — O(1) unordered remove: move the last
        // element into slot `i`, shrink by one, return the old element `i`.
        // Traps out-of-bounds (same wording as reads). Same `mut`/`Array<T>`
        // rules as `pop`.
        if name == "@swapRemove" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `swapRemove` takes 1 argument (an index), got {}",
                    args.len() - 1
                ));
            }
            let elem = self.mut_array_receiver(&args[0], scope, line, "swapRemove")?;
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if !matches!(i, Type::Int | Type::Err) {
                return Err(format!(
                    "line {line}: `swapRemove` index must be an Int64, found {i}"
                ));
            }
            return Ok(elem);
        }
        // Numeric conversion: `Int32(x)`, `Float64(x)`, etc. — resize/round a
        // number to the named numeric type. No implicit conversions elsewhere.
        if let Some(target) = crate::types::numeric_conv_target(name) {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `{name}` conversion takes 1 argument, got {}",
                    args.len()
                ));
            }
            let src = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(src, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(src, Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }) {
                return Err(format!(
                    "line {line}: `{name}(..)` converts a number, found {src}"
                ));
            }
            return Ok(target);
        }
        // built-in: schemaOf(TypeName) -> Schema — compile-time reflection of a
        // validated type's `where` predicate (RFC-0003). The argument is a *type
        // name*, not a value; the bounds are extracted from the type declaration.
        if name == "schemaOf" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `schemaOf` takes 1 argument (a type name), got {}",
                    args.len()
                ));
            }
            match &args[0] {
                Expr::Var { name: tn, .. } if self.types.contains_key(tn) => {
                    return Ok(Type::Named("Schema".to_string()))
                }
                Expr::Var { name: tn, .. } => {
                    return Err(format!(
                        "line {line}: `schemaOf` needs a declared type name; `{tn}` is not a type"
                    ))
                }
                _ => return Err(format!("line {line}: `schemaOf` needs a type name")),
            }
        }
        // built-in: jsonSchema(TypeName) -> String — compile-time rendering of a
        // declared type as a JSON Schema (draft 2020-12) document. Like `schemaOf`,
        // the argument is a *type name*; the string is computed from the declaration.
        if name == "jsonSchema" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `jsonSchema` takes 1 argument (a type name), got {}",
                    args.len()
                ));
            }
            match &args[0] {
                Expr::Var { name: tn, .. } if self.types.contains_key(tn) => return Ok(Type::Str),
                Expr::Var { name: tn, .. } => {
                    return Err(format!(
                        "line {line}: `jsonSchema` needs a declared type name; `{tn}` is not a type"
                    ))
                }
                _ => return Err(format!("line {line}: `jsonSchema` needs a type name")),
            }
        }
        // built-in: toJson(x) -> String (RFC-0018) — encode any *codable* value
        // to canonical JSON. Pure (not constant: kept out of consteval), never
        // traps. The argument's type must be encodable (scalars, validated
        // scalars, records, Option, Array/ArrayN, payload-less enums).
        if name == "toJson" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `toJson` takes 1 argument (a value), got {}",
                    args.len()
                ));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            if matches!(at, Type::Err) {
                return Ok(Type::Str);
            }
            if let Err(off) = crate::codec::encodable(&at, self.types) {
                return Err(format!(
                    "line {line}: `toJson` cannot encode `{off}` (not a codable type)"
                ));
            }
            return Ok(Type::Str);
        }
        // built-in: fromJson(TypeName, s) -> Validation<T> (RFC-0018) —
        // type-directed decode (the `schemaOf`/`jsonSchema` precedent: the first
        // argument is a *type name*). Never traps; every problem is an `Issue`
        // accumulated into the returned `Validation<T>`.
        if name == "fromJson" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `fromJson` takes 2 arguments (a type name and a String), got {}",
                    args.len()
                ));
            }
            let tn = match &args[0] {
                Expr::Var { name: tn, .. } if self.types.contains_key(tn) => tn.clone(),
                Expr::Var { name: tn, .. } => {
                    return Err(format!(
                        "line {line}: `fromJson` needs a declared type name; `{tn}` is not a type"
                    ))
                }
                _ => return Err(format!("line {line}: `fromJson` needs a type name")),
            };
            let target = Type::Named(tn.clone());
            if let Err(off) = crate::codec::decodable(&target, self.types) {
                return Err(format!(
                    "line {line}: `fromJson` cannot decode into `{off}` (not a codable type)"
                ));
            }
            let sty = self.base(&self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?);
            if !matches!(sty, Type::Str | Type::Err) {
                return Err(format!(
                    "line {line}: `fromJson`'s second argument must be a String, found {sty}"
                ));
            }
            return Ok(Type::App("Validation".to_string(), vec![target]));
        }
        // built-in: value(x) -> Value — box a scalar into the interpolation value
        // type (RFC-0007). What a tagged template's holes desugar to.
        if name == "value" {
            if args.len() != 1 {
                return Err(format!("line {line}: `value` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(t, Type::Int | Type::Bool | Type::Str) {
                return Err(format!(
                    "line {line}: `value` boxes an Int64, Bool, or String, found {t}"
                ));
            }
            return Ok(Type::Named("Value".to_string()));
        }
        // built-in: list(Array<T, N>) -> Array<T> — a fixed array as a growable one
        // (RFC-0007 tagged-template desugar; the tag takes size-erased arrays).
        // `@list` — the internal spelling of the removed `list` builtin, still
        // produced by tagged-template desugaring: coerce a fixed array to a
        // growable one. (User source uses a contextual array literal instead.)
        if name == "@list" {
            if args.len() != 1 {
                return Err(format!("line {line}: `@list` takes 1 argument, got {}", args.len()));
            }
            let a = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&a) {
                Type::ArrayN(inner, _) | Type::Array(inner) => {
                    return Ok(Type::Array(inner))
                }
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!("line {line}: `@list` needs an Array, found {other}"))
                }
            }
        }

        // built-in: Some(x) -> Option<typeof x>
        if name == "Some" {
            if args.len() != 1 {
                return Err(format!("line {line}: `Some` takes 1 argument, got {}", args.len()));
            }
            let inner_expected = match expected {
                Some(Type::Option(t)) => Some((**t).clone()),
                _ => None,
            };
            let aty = self.expr(&args[0], scope, inner_expected.as_ref(), fn_ret)?;
            if matches!(aty, Type::Option(_) | Type::Result(..)) {
                return Err(format!("line {line}: nested Option/Result is not supported in v0.1"));
            }
            if let Some(want) = &inner_expected {
                if !self.coercible(&aty, want) {
                    return Err(format!(
                        "line {line}: `Some` payload is {aty} but Option<{want}> was expected"
                    ));
                }
                self.prove_coercion(&args[0], want, line)?;
                return Ok(Type::Option(Box::new(want.clone())));
            }
            return Ok(Type::Option(Box::new(aty)));
        }

        // built-in: Ok(x) / Err(e) — need the other type parameter from context.
        if name == "Ok" || name == "Err" {
            if args.len() != 1 {
                return Err(format!("line {line}: `{name}` takes 1 argument, got {}", args.len()));
            }
            let want = match expected {
                Some(Type::Result(t, e)) => Some((name == "Ok").then(|| (**t).clone()).unwrap_or_else(|| (**e).clone())),
                _ => None,
            };
            let aty = self.expr(&args[0], scope, want.as_ref(), fn_ret)?;
            if matches!(aty, Type::Option(_) | Type::Result(..)) {
                return Err(format!("line {line}: nested Option/Result is not supported in v0.1"));
            }
            let (t, e) = match expected {
                Some(Type::Result(t, e)) => ((**t).clone(), (**e).clone()),
                _ => {
                    return Err(format!(
                        "line {line}: cannot infer the type of `{name}(..)`; add an annotation \
                         (e.g. `-> Result<Int64, Int64>`)"
                    ))
                }
            };
            let want_ty = if name == "Ok" { &t } else { &e };
            self.prove_coercion(&args[0], want_ty, line)?;
            if !self.coercible(&aty, want_ty) {
                return Err(format!(
                    "line {line}: `{name}` payload is {aty} but {want_ty} was expected"
                ));
            }
            return Ok(Type::Result(Box::new(t), Box::new(e)));
        }

        // enum variant construction with payload(s): `Circle(r)`, `Rect(w, h)`.
        if let Some(info) = self.variants.get(name) {
            let payload = info.payload.clone();
            if payload.is_empty() {
                return Err(format!("line {line}: variant `{name}` takes no arguments"));
            }
            if args.len() != payload.len() {
                return Err(format!(
                    "line {line}: `{name}` takes {} argument(s), got {}",
                    payload.len(),
                    args.len()
                ));
            }
            // Check/infer each payload argument.
            let mut subst: HashMap<String, Type> = HashMap::new();
            for (arg, pty) in args.iter().zip(&payload) {
                let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                self.unify(pty, &aty, &mut subst, line)?;
            }
            let tps = self.enum_type_params(&info.enum_name);
            if tps.is_empty() {
                return Ok(Type::Named(info.enum_name.clone()));
            }
            // Fill any type params the payload didn't determine from the expected
            // type — e.g. `Invalid(issues)` learns `T` from a `Validation<T>` return.
            if let Some(Type::App(en, targs)) = expected {
                if en == &info.enum_name {
                    for (tp, ta) in tps.iter().zip(targs) {
                        subst.entry(tp.clone()).or_insert_with(|| ta.clone());
                    }
                }
            }
            for tp in &tps {
                if !subst.contains_key(tp) {
                    return Err(format!(
                        "line {line}: cannot infer type parameter `{tp}` of `{}`",
                        info.enum_name
                    ));
                }
            }
            let targs = tps.iter().map(|tp| subst[tp].clone()).collect();
            return Ok(Type::App(info.enum_name.clone(), targs));
        }

        // construction of a validated type: `Age(expr)`
        if let Some(decl) = self.types.get(name) {
            return self.check_construction(decl, args, line, scope, fn_ret);
        }

        // Protocol-method call (RFC-0002 §5): `x.m(..)` desugared to `m(x, ..)`.
        // Dispatch to the impl for the receiver's type. Inside a generic bounded
        // by the protocol the receiver is a type parameter — allow the call and
        // use the protocol's declared signature (dispatch is deferred to codegen).
        if let Some((proto, sig)) = self.protocol_methods.get(name).cloned() {
            if args.is_empty() {
                return Err(format!("line {line}: `{name}` needs a `self` receiver"));
            }
            // Key on the raw (non-decayed) receiver type so enums keep their name.
            let recv = self.expr(&args[0], scope, None, fn_ret)?;
            if let Type::Param(t) = &recv {
                if self.param_has_bound(t, &proto) {
                    // Check arity, then EVERY remaining argument against the
                    // signature (a bare `zip` would silently drop extras and
                    // leave them entirely unchecked).
                    if args.len() - 1 != sig.params.len() {
                        return Err(format!(
                            "line {line}: `{name}` expects {} argument(s) besides `self`, got {}",
                            sig.params.len(),
                            args.len() - 1
                        ));
                    }
                    for (arg, pty) in args[1..].iter().zip(&sig.params) {
                        let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                        if !self.coercible(&aty, pty) {
                            return Err(format!(
                                "line {line}: `{name}` argument is {aty}, expected {pty}"
                            ));
                        }
                        self.prove_coercion(arg, pty, line)?;
                    }
                    return Ok(sig.ret.clone());
                }
            }
            match crate::types::type_key(&recv) {
                Some(key) if self.impls.contains(&(proto.clone(), key.clone())) => {
                    let mangled = crate::types::impl_method_name(&proto, &key, name);
                    return self.call(&mangled, args, line, scope, expected, fn_ret);
                }
                _ => {
                    return Err(format!(
                        "line {line}: {recv} does not implement protocol `{proto}` \
                         (needed for `.{name}(..)`)"
                    ))
                }
            }
        }

        let (params, ret) = self
            .sigs
            .get(name)
            .ok_or_else(|| format!("line {line}: call to unknown function `{name}`"))?;
        if params.len() != args.len() {
            return Err(format!(
                "line {line}: `{name}` expects {} argument(s), got {}",
                params.len(),
                args.len()
            ));
        }

        // Generic call: infer the type parameters from the argument types.
        if let Some(type_params) = self.generics.get(name) {
            let mut subst: HashMap<String, Type> = HashMap::new();
            let mut atys: Vec<Type> = Vec::with_capacity(args.len());
            for (arg, pty) in args.iter().zip(params) {
                let aty = self.expr(arg, scope, None, fn_ret)?;
                self.unify(pty, &aty, &mut subst, line)?;
                atys.push(aty);
            }
            // Capability discipline applies to generic calls exactly as to
            // concrete ones (this path used to return early and skip it,
            // letting `f<T>(c: modify C, ..)` mutate immutable bindings).
            let caps = self.caps.get(name);
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                if caps.and_then(|c| c.get(i)) == Some(&Capability::Modify) {
                    let concrete_pty = crate::types::substitute(pty, &subst);
                    self.check_modify_arg(name, i, arg, &atys[i], &concrete_pty, scope, line)?;
                }
            }
            for tp in type_params {
                if !subst.contains_key(tp) {
                    return Err(format!(
                        "line {line}: cannot infer type parameter `{tp}` of `{name}`"
                    ));
                }
            }
            // Check each inferred type argument against the parameter's bounds.
            if let Some(bounds) = self.all_bounds.get(name) {
                for (tp, bs) in bounds {
                    let concrete = &subst[tp];
                    for b in bs {
                        if !self.type_satisfies(concrete, b) {
                            return Err(format!(
                                "line {line}: `{name}` requires `{tp}: {b}`, but {concrete} does not satisfy `{b}`"
                            ));
                        }
                    }
                }
            }
            // The v0.1 "no nested Option/Result" rule holds through inference
            // too: `wrap(Some(1))` with `fn wrap<T>(x: T) -> Option<T>` must
            // not materialize an Option<Option<Int>>.
            let rty = crate::types::substitute(ret, &subst);
            let nested = params
                .iter()
                .map(|p| crate::types::substitute(p, &subst))
                .chain(std::iter::once(rty.clone()))
                .any(|t| has_nested_wrap(&t));
            if nested {
                return Err(format!(
                    "line {line}: nested Option/Result is not supported in v0.1 \
                     (inferred through `{name}`)"
                ));
            }
            return Ok(rty);
        }

        let caps = self.caps.get(name);
        for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
            let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
            if !self.coercible(&aty, pty) {
                return Err(format!(
                    "line {line}: `{name}` argument {} expects {pty}, found {aty}",
                    i + 1
                ));
            }
            self.prove_coercion(arg, pty, line)?;
            // A `modify` parameter receives the caller's binding by reference —
            // full discipline checked in the shared helper.
            if caps.and_then(|c| c.get(i)) == Some(&Capability::Modify) {
                self.check_modify_arg(name, i, arg, &aty, pty, scope, line)?;
            }
        }
        Ok(ret.clone())
    }

    /// The call-site discipline for a `modify` parameter, shared by concrete
    /// and generic calls: the argument must be a *mutable variable* (not a
    /// temporary), and its type must be EXACTLY the parameter type. Width
    /// subtyping is unsound here: the callee may whole-reassign the parameter
    /// (`n = Named { .. }`), and writing that back through a wider caller
    /// record would silently drop the caller's extra fields.
    fn check_modify_arg(
        &self,
        fname: &str,
        i: usize,
        arg: &Expr,
        aty: &Type,
        pty: &Type,
        scope: &Vec<HashMap<String, Binding>>,
        line: usize,
    ) -> Result<(), String> {
        match arg {
            Expr::Var { name: vn, .. } => {
                let b = self
                    .lookup(scope, vn)
                    .ok_or_else(|| format!("line {line}: unknown variable `{vn}`"))?;
                if !b.mutable {
                    return Err(format!(
                        "line {line}: `{fname}` argument {} is `modify`, so `{vn}` must be \
                         declared `mut`",
                        i + 1
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "line {line}: `{fname}` argument {} is `modify`; pass a mutable \
                     variable, not a temporary",
                    i + 1
                ))
            }
        }
        if !matches!(aty, Type::Err) && !matches!(pty, Type::Err) && aty != pty {
            return Err(format!(
                "line {line}: `{fname}` argument {} is `modify` and needs exactly \
                 {pty}, found {aty} (width subtyping is read-only: a wider \
                 record could lose fields on write-back)",
                i + 1
            ));
        }
        Ok(())
    }

    /// Match a (possibly generic) parameter type against a concrete argument
    /// type, binding type parameters in `subst`.
    fn unify(
        &self,
        pty: &Type,
        aty: &Type,
        subst: &mut HashMap<String, Type>,
        line: usize,
    ) -> Result<(), String> {
        // A recovered `Err` unifies with anything (no spurious mismatch).
        if matches!(pty, Type::Err) || matches!(aty, Type::Err) {
            return Ok(());
        }
        match pty {
            Type::Param(t) => match subst.get(t) {
                Some(bound) => {
                    if !self.assignable(aty, bound) {
                        Err(format!(
                            "line {line}: type parameter `{t}` is both {bound} and {aty}"
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
            Type::Option(inner) => match aty {
                Type::Option(a) => self.unify(inner, a, subst, line),
                _ => Err(format!("line {line}: expected Option, found {aty}")),
            },
            Type::Result(pt, pe) => match aty {
                Type::Result(at, ae) => {
                    self.unify(pt, at, subst, line)?;
                    self.unify(pe, ae, subst, line)
                }
                _ => Err(format!("line {line}: expected Result, found {aty}")),
            },
            Type::App(pn, pargs) => match aty {
                Type::App(an, aargs) if pn == an && pargs.len() == aargs.len() => {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.unify(p, a, subst, line)?;
                    }
                    Ok(())
                }
                _ => Err(format!("line {line}: expected {pty}, found {aty}")),
            },
            _ => {
                if !self.coercible(aty, pty) {
                    Err(format!("line {line}: argument expects {pty}, found {aty}"))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Check `TypeName(arg)`. If `arg` is a compile-time constant, the predicate
    /// is evaluated now and a provably-invalid value is a compile error
    /// (RFC-0003). Otherwise the check is deferred to runtime by the backends.
    fn check_construction(
        &self,
        decl: &TypeDecl,
        args: &[Expr],
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        if args.len() != 1 {
            return Err(format!(
                "line {line}: `{}` construction takes 1 argument, got {}",
                decl.name,
                args.len()
            ));
        }
        let aty = self.expr(&args[0], scope, Some(&decl.base), fn_ret)?;
        if !self.assignable(&aty, &decl.base) {
            return Err(format!(
                "line {line}: `{}` is built from {}, but the argument is {aty}",
                decl.name, decl.base
            ));
        }
        // Compile-time validation when the argument is a constant.
        if let Some(cv) = consteval::eval(&args[0], &HashMap::new()) {
            if let Some(pred) = &decl.predicate {
                let mut env = HashMap::new();
                env.insert("value".to_string(), cv.clone());
                match consteval::eval(pred, &env).and_then(ConstVal::as_bool) {
                    Some(true) => {}
                    Some(false) => {
                        return Err(format!(
                            "line {line}: {} does not satisfy `{}` (predicate `where {}` is false)",
                            cv,
                            decl.name,
                            pred_summary(pred),
                        ));
                    }
                    None => {} // couldn't fully fold; fall through to runtime
                }
            }
        }
        Ok(Type::Named(decl.name.clone()))
    }

    fn lookup(&self, scope: &Vec<HashMap<String, Binding>>, name: &str) -> Option<Binding> {
        for frame in scope.iter().rev() {
            if let Some(b) = frame.get(name) {
                return Some(b.clone());
            }
        }
        None
    }

    /// Whether `name` resolves to a module-state binding (frame 0) rather than a
    /// local: the topmost frame that binds it is the globals frame. A local of
    /// the same name (any higher frame) shadows it, so this returns `false` then.
    fn resolves_to_global(&self, scope: &Vec<HashMap<String, Binding>>, name: &str) -> bool {
        for (i, frame) in scope.iter().enumerate().rev() {
            if frame.contains_key(name) {
                return i == 0;
            }
        }
        false
    }

    /// The element type of the array a `pop`/`swapRemove` receiver names, after
    /// checking it is a plain `mut` `Array<T>` binding (RFC-0011). A fixed-size
    /// `Array<T, N>` cannot shrink, so it is rejected with a message naming
    /// `Array<T>`. Returns `Type::Err` (already reported upstream) transparently.
    fn mut_array_receiver(
        &self,
        recv: &Expr,
        scope: &Vec<HashMap<String, Binding>>,
        line: usize,
        op: &str,
    ) -> Result<Type, String> {
        let Expr::Var { name, .. } = recv else {
            return Err(format!(
                "line {line}: `{op}` needs a plain array variable as its receiver"
            ));
        };
        let b = self
            .lookup(scope, name)
            .ok_or_else(|| format!("line {line}: `{op}` on unknown variable `{name}`"))?;
        if !b.mutable {
            return Err(format!(
                "line {line}: cannot `{op}` from `{name}` (declared without `mut`)"
            ));
        }
        match self.base(&b.ty) {
            Type::Array(inner) => Ok((*inner).clone()),
            Type::Err => Ok(Type::Err),
            Type::ArrayN(..) => Err(format!(
                "line {line}: `{op}` is not available on a fixed-size array \
                 (it cannot shrink); use a growable `Array<T>`"
            )),
            other => Err(format!(
                "line {line}: `{op}` needs an `Array<T>`, found {other}"
            )),
        }
    }
}

/// A terse one-line rendering of a predicate for diagnostics.
pub(crate) fn pred_summary(expr: &Expr) -> String {
    match expr {
        Expr::Int(n) => n.to_string(),
        Expr::Float(x) => x.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("{s:?}"),
        Expr::Var { name, .. } => name.clone(),
        Expr::Unary { op, expr, .. } => {
            let s = pred_summary(expr);
            match op {
                UnOp::Neg => format!("-{s}"),
                UnOp::Not => format!("!{s}"),
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let o = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Rem => "%",
                BinOp::Lt => "<",
                BinOp::LtEq => "<=",
                BinOp::Gt => ">",
                BinOp::GtEq => ">=",
                BinOp::Eq => "==",
                BinOp::NotEq => "!=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                BinOp::Match => "=~",
            };
            format!("{} {o} {}", pred_summary(lhs), pred_summary(rhs))
        }
        // `at(s, i)` is the desugaring of indexing — render it back as `s[i]`.
        Expr::Call { name, args, .. } if name == "at" && args.len() == 2 => {
            format!("{}[{}]", pred_summary(&args[0]), pred_summary(&args[1]))
        }
        Expr::Call { name, .. } => format!("{name}(..)"),
        Expr::Match { .. } => "match { .. }".to_string(),
        Expr::Try { expr, .. } => format!("{}?", pred_summary(expr)),
        Expr::StructLit { name, .. } => format!("{name} {{ .. }}"),
        Expr::Field { expr, field, .. } => format!("{}.{field}", pred_summary(expr)),
        Expr::TryConstruct { name, .. } => format!("{name}?(..)"),
        Expr::ArrayLit { .. } => "[..]".to_string(),
        Expr::Spawn { name, .. } => format!("spawn {name}(..)"),
    }
}

/// Whether a type contains a directly nested `Option`/`Result` (the v0.1
/// prohibition), anywhere inside it.
fn has_nested_wrap(ty: &Type) -> bool {
    let wrapped = |t: &Type| matches!(t, Type::Option(_) | Type::Result(..));
    match ty {
        Type::Option(t) => wrapped(t) || has_nested_wrap(t),
        Type::Result(a, b) => {
            wrapped(a) || wrapped(b) || has_nested_wrap(a) || has_nested_wrap(b)
        }
        Type::Array(t) | Type::ArrayN(t, _) | Type::Ref(t) | Type::Task(t) => has_nested_wrap(t),
        Type::Record(fs) => fs.iter().any(|f| has_nested_wrap(&f.ty)),
        _ => false,
    }
}

/// Whether an integer literal `n` fits the sized type. The lexer wraps
/// u64-range literals into the i64 bit pattern, so a *negative* `n` means "a
/// literal above i64::MAX" — it fits only `UInt64`. (True negative literals
/// never reach this: unary `-` does not adapt to sized types.)
fn int_literal_fits(n: i64, bits: u8, signed: bool) -> bool {
    if bits == 64 {
        return !signed || n >= 0;
    }
    let max = if signed { i64::MAX >> (64 - u32::from(bits)) } else { (1i64 << bits) - 1 };
    (0..=max).contains(&n)
}

/// The user-facing name of a sized integer type (`Int8` … `UInt64`).
fn intn_name(bits: u8, signed: bool) -> String {
    format!("{}Int{bits}", if signed { "" } else { "U" })
}

/// The inclusive value range of a sized integer type, for diagnostics.
fn intn_range(bits: u8, signed: bool) -> String {
    if signed {
        let shift = 64 - u32::from(bits);
        format!("{}..={}", i64::MIN >> shift, i64::MAX >> shift)
    } else {
        let max: u64 = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        format!("0..={max}")
    }
}

/// Render a literal as the user wrote it: a negative `n` is a wrapped
/// u64-range literal, so show its unsigned value.
fn render_int_literal(n: i64) -> String {
    if n < 0 { (n as u64).to_string() } else { n.to_string() }
}

/// Builtins a concurrent task may not use: `print` (observable ordering),
/// `cell`/`set`/`release` (mutate the shared reference slab), `afree` (frees a
/// buffer the caller may still hold across the task boundary), and the log
/// methods. `get` is a read-only slab access and is allowed.
const SPAWN_FORBIDDEN: &[&str] = &[
    "print", "cell", "set", "release", "afree", "trace", "debug", "info", "warn", "error",
    // Input I/O effects (RFC-0014): observe/mutate the outside world (stdin
    // cursor, the filesystem), so they must not cross a task boundary.
    "args", "readLine", "readFile", "writeFile", "readFileBytes", "stringFromBytes",
];

/// Whether a type may appear in an `extern` signature (RFC-0012 ABI). The scalar
/// primitives cross by value; a `String` crosses as a `(ptr, len)` pair. Nothing
/// else — named/validated types, records, options, arrays, refs — has a v1
/// wire representation. `allow_unit` permits `Unit` in return position only.
fn extern_abi_type_ok(ty: &Type, allow_unit: bool) -> bool {
    match ty {
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str => {
            true
        }
        Type::Unit => allow_unit,
        _ => false,
    }
}

/// Whether a block contains a `drop` statement anywhere (including nested blocks).
/// Used by spawn-safety: `drop` can release a shared `Ref`, so a task must not.
fn contains_drop(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Drop { .. } => true,
        Stmt::If { then_block, else_block, .. } => {
            contains_drop(then_block) || else_block.as_ref().is_some_and(contains_drop)
        }
        Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
            contains_drop(body)
        }
        _ => false,
    })
}

/// Whether an expression tree uses `spawn` anywhere.
fn expr_contains_spawn(e: &Expr) -> bool {
    match e {
        Expr::Spawn { .. } => true,
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            expr_contains_spawn(expr)
        }
        Expr::Binary { lhs, rhs, .. } => expr_contains_spawn(lhs) || expr_contains_spawn(rhs),
        Expr::Call { args, .. } | Expr::TryConstruct { args, .. } | Expr::ArrayLit { elems: args, .. } => {
            args.iter().any(expr_contains_spawn)
        }
        Expr::Match { scrutinee, arms, .. } => {
            expr_contains_spawn(scrutinee) || arms.iter().any(|a| expr_contains_spawn(&a.body))
        }
        Expr::StructLit { fields, .. } => fields.iter().any(|(_, v)| expr_contains_spawn(v)),
        _ => false,
    }
}

/// Whether a block uses `spawn` anywhere (including nested blocks) — used by the
/// comptime-purity analysis (RFC-0021): a generator may not spawn.
fn contains_spawn(b: &Block) -> bool {
    fn stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::SetField { value, .. }
            | Stmt::Expr(value) => expr_contains_spawn(value),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(expr_contains_spawn),
            Stmt::IndexSet { index, value, .. } => {
                expr_contains_spawn(index) || expr_contains_spawn(value)
            }
            Stmt::If { cond, then_block, else_block, .. } => {
                expr_contains_spawn(cond)
                    || contains_spawn(then_block)
                    || else_block.as_ref().is_some_and(contains_spawn)
            }
            Stmt::While { cond, body, .. } => expr_contains_spawn(cond) || contains_spawn(body),
            Stmt::ForIn { iter, body, .. } => expr_contains_spawn(iter) || contains_spawn(body),
            Stmt::Region { body, .. } => contains_spawn(body),
            Stmt::Drop { .. } => false,
        }
    }
    b.stmts.iter().any(stmt)
}

/// Builtins a `gen fn` (RFC-0021) may not use at generation time: they observe
/// or mutate the outside world in a way the deterministic, cache-keyed sandbox
/// cannot mediate. `readFile`/`listDir`/`moduleInterface` are deliberately
/// ABSENT — they route through the loader's resolver at generation time and are
/// recorded as cache inputs. Logging sinks (`trace`..`error`) are here too.
const COMPTIME_FORBIDDEN: &[&str] = &[
    "writeFile", "readLine", "args", "readFileBytes", "trace", "debug", "info", "warn", "error",
];

/// Comptime-purity analysis (RFC-0021), the spawn-isolation sibling. Every
/// `gen fn` — and everything it transitively calls — must be pure enough to run
/// deterministically in the compiler's interpreter at generation time: no
/// `extern`, `spawn`, module state, or the [`COMPTIME_FORBIDDEN`] effect
/// builtins. Because a `gen fn` may be *used* as an import target anywhere it is
/// visible, the restriction is enforced on EVERY `gen fn` unconditionally (v1:
/// simpler and sound than a whole-program "reached as a generation target"
/// analysis; a `gen fn` called only at runtime pays the same discipline, which
/// keeps the rule one sentence long). Diagnostics name the offending effect and
/// the call chain that reaches it.
fn check_comptime_purity(program: &Program, out: &mut Vec<Diagnostic>) {
    let gen_fns: Vec<&Function> = program.functions.iter().filter(|f| f.is_gen).collect();
    if gen_fns.is_empty() {
        return;
    }
    let fn_map: HashMap<&str, &Function> =
        program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let extern_fns: std::collections::HashSet<&str> =
        program.functions.iter().filter(|f| f.is_extern).map(|f| f.name.as_str()).collect();
    let global_names: std::collections::HashSet<String> =
        program.globals.iter().map(|g| g.name.clone()).collect();
    // Surface method name -> impl mangled names, so a protocol-method call edge
    // is followed into the impl body (an impure impl reached through a method).
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
    // A function's own (non-transitive) purity violation, if any.
    let direct = |f: &Function| -> Option<String> {
        if contains_spawn(&f.body) {
            return Some("uses `spawn`".to_string());
        }
        if touches_globals(f, &global_names) {
            return Some("reads or writes module state".to_string());
        }
        for c in expand(fn_calls(&f.body)) {
            if COMPTIME_FORBIDDEN.contains(&c.as_str()) {
                return Some(format!("calls `{c}`"));
            }
            if extern_fns.contains(c.as_str()) {
                return Some(format!("calls the extern `{c}`"));
            }
        }
        None
    };
    const HINT: &str = "generators run at compile time — they may not use `extern`, `spawn`, \
                        module state, `writeFile`, `readLine`, `args`, `readFileBytes`, or logging \
                        sinks";
    for g in gen_fns {
        // BFS the call graph from this generator to the nearest direct violation.
        let mut queue: std::collections::VecDeque<Vec<&str>> =
            std::collections::VecDeque::from([vec![g.name.as_str()]]);
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::from([g.name.as_str()]);
        while let Some(path) = queue.pop_front() {
            let cur = *path.last().unwrap();
            let Some(f) = fn_map.get(cur) else { continue };
            if let Some(reason) = direct(f) {
                let msg = if path.len() == 1 {
                    format!(
                        "line {}: `gen fn {}` is not comptime-pure: it {reason} ({HINT})",
                        g.line, g.name
                    )
                } else {
                    let chain = path.join(" -> ");
                    format!(
                        "line {}: `gen fn {}` is not comptime-pure: it reaches `{cur}` (via \
                         {chain}), which {reason} ({HINT})",
                        g.line, g.name
                    )
                };
                let mut d = Diagnostic::from_rendered(msg, "check");
                d.file = g.module.clone();
                out.push(d);
                break;
            }
            for callee in expand(fn_calls(&f.body)) {
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

/// Whether a function reads or writes any module-state binding (RFC-0013), so it
/// cannot be spawned. A local (param / `let` / `for`-var) with the same name
/// shadows the global and does not count. Slightly conservative: a name both
/// shadowed and used as a global in disjoint scopes is treated as a touch.
fn touches_globals(f: &Function, globals: &std::collections::HashSet<String>) -> bool {
    if globals.is_empty() {
        return false;
    }
    let mut local: std::collections::HashSet<String> =
        f.params.iter().map(|p| p.name.clone()).collect();
    collect_binders_block(&f.body, &mut local);
    global_ref_block(&f.body, globals, &local)
}

/// Collect every name a block binds locally (`let`, `for`-in variable). Params
/// are seeded by the caller.
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
            Stmt::If { then_block, else_block, .. } => {
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

/// Whether a block references a global (reads it via `Var`, or writes it via
/// `Assign`/`SetField`/`IndexSet`) that no local of the same name shadows.
fn global_ref_block(
    b: &Block,
    globals: &std::collections::HashSet<String>,
    local: &std::collections::HashSet<String>,
) -> bool {
    let is_global = |n: &str| globals.contains(n) && !local.contains(n);
    b.stmts.iter().any(|s| match s {
        Stmt::Let { value, .. } | Stmt::Expr(value) => global_ref_expr(value, globals, local),
        Stmt::Assign { name, value, .. }
        | Stmt::SetField { name, value, .. } => {
            is_global(name) || global_ref_expr(value, globals, local)
        }
        Stmt::IndexSet { name, index, value, .. } => {
            is_global(name)
                || global_ref_expr(index, globals, local)
                || global_ref_expr(value, globals, local)
        }
        Stmt::Return { value: Some(e), .. } => global_ref_expr(e, globals, local),
        Stmt::Return { value: None, .. } => false,
        Stmt::If { cond, then_block, else_block, .. } => {
            global_ref_expr(cond, globals, local)
                || global_ref_block(then_block, globals, local)
                || else_block.as_ref().is_some_and(|eb| global_ref_block(eb, globals, local))
        }
        Stmt::While { cond, body, .. } => {
            global_ref_expr(cond, globals, local) || global_ref_block(body, globals, local)
        }
        Stmt::ForIn { iter, body, .. } => {
            global_ref_expr(iter, globals, local) || global_ref_block(body, globals, local)
        }
        Stmt::Drop { name, .. } => is_global(name),
        Stmt::Region { body, .. } => global_ref_block(body, globals, local),
    })
}

fn global_ref_expr(
    e: &Expr,
    globals: &std::collections::HashSet<String>,
    local: &std::collections::HashSet<String>,
) -> bool {
    let is_global = |n: &str| globals.contains(n) && !local.contains(n);
    match e {
        Expr::Var { name, .. } => is_global(name),
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => false,
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            global_ref_expr(expr, globals, local)
        }
        Expr::Binary { lhs, rhs, .. } => {
            global_ref_expr(lhs, globals, local) || global_ref_expr(rhs, globals, local)
        }
        Expr::Call { args, .. } | Expr::Spawn { args, .. } | Expr::TryConstruct { args, .. } => {
            args.iter().any(|a| global_ref_expr(a, globals, local))
        }
        Expr::Match { scrutinee, arms, .. } => {
            global_ref_expr(scrutinee, globals, local)
                || arms.iter().any(|a| global_ref_expr(&a.body, globals, local))
        }
        Expr::StructLit { fields, .. } => {
            fields.iter().any(|(_, v)| global_ref_expr(v, globals, local))
        }
        Expr::ArrayLit { elems, .. } => elems.iter().any(|v| global_ref_expr(v, globals, local)),
    }
}

/// Enforce a module-state initializer's restrictions (RFC-0013): it may not call
/// a user or extern function (or protocol method), and may not read a global
/// that is declared later (or itself). Returns the first violation.
fn init_restrictions(
    e: &Expr,
    forbidden: &HashSet<String>,
    all_globals: &HashSet<&str>,
    ready: &HashSet<String>,
    own_name: &str,
    line: usize,
) -> Result<(), String> {
    match e {
        Expr::Var { name, .. } => {
            if all_globals.contains(name.as_str()) && !ready.contains(name) {
                if name == own_name {
                    return Err(format!(
                        "line {line}: module state `{own_name}` may not read itself in its \
                         own initializer"
                    ));
                }
                return Err(format!(
                    "line {line}: initializer of `{own_name}` reads `{name}`, a module-state \
                     binding declared later — a global may only read earlier ones"
                ));
            }
            Ok(())
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            init_restrictions(expr, forbidden, all_globals, ready, own_name, line)
        }
        Expr::Binary { lhs, rhs, .. } => {
            init_restrictions(lhs, forbidden, all_globals, ready, own_name, line)?;
            init_restrictions(rhs, forbidden, all_globals, ready, own_name, line)
        }
        Expr::Call { name, args, .. } => {
            if forbidden.contains(name) {
                return Err(format!(
                    "line {line}: initializer of `{own_name}` may not call `{name}` — a \
                     module-state initializer runs before `main`, so it may use only \
                     literals, operators, and built-ins (no user or extern calls)"
                ));
            }
            for a in args {
                init_restrictions(a, forbidden, all_globals, ready, own_name, line)?;
            }
            Ok(())
        }
        Expr::Spawn { name, .. } => Err(format!(
            "line {line}: initializer of `{own_name}` may not `spawn {name}` — a \
             module-state initializer runs before `main` (no user calls)"
        )),
        Expr::TryConstruct { args, .. } => {
            for a in args {
                init_restrictions(a, forbidden, all_globals, ready, own_name, line)?;
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms, .. } => {
            init_restrictions(scrutinee, forbidden, all_globals, ready, own_name, line)?;
            for a in &arms.iter().map(|a| &a.body).collect::<Vec<_>>() {
                init_restrictions(a, forbidden, all_globals, ready, own_name, line)?;
            }
            Ok(())
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                init_restrictions(v, forbidden, all_globals, ready, own_name, line)?;
            }
            Ok(())
        }
        Expr::ArrayLit { elems, .. } => {
            for v in elems {
                init_restrictions(v, forbidden, all_globals, ready, own_name, line)?;
            }
            Ok(())
        }
    }
}

/// The names of every function/builtin called (or spawned) anywhere in a block.
fn fn_calls(b: &Block) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    calls_block(b, &mut out);
    out
}
fn calls_block(b: &Block, out: &mut std::collections::HashSet<String>) {
    for s in &b.stmts {
        calls_stmt(s, out);
    }
}
fn calls_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. }
        | Stmt::Expr(value) => calls_expr(value, out),
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                calls_expr(e, out);
            }
        }
        Stmt::If { cond, then_block, else_block, .. } => {
            calls_expr(cond, out);
            calls_block(then_block, out);
            if let Some(eb) = else_block {
                calls_block(eb, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            calls_expr(cond, out);
            calls_block(body, out);
        }
        Stmt::ForIn { iter, body, .. } => {
            calls_expr(iter, out);
            calls_block(body, out);
        }
        Stmt::IndexSet { index, value, .. } => {
            calls_expr(index, out);
            calls_expr(value, out);
        }
        Stmt::Region { body, .. } => calls_block(body, out),
        Stmt::Drop { .. } => {}
    }
}
fn calls_expr(e: &Expr, out: &mut std::collections::HashSet<String>) {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Var { .. } => {}
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            calls_expr(expr, out)
        }
        Expr::Binary { lhs, rhs, .. } => {
            calls_expr(lhs, out);
            calls_expr(rhs, out);
        }
        Expr::Call { name, args, .. } | Expr::TryConstruct { name, args, .. } => {
            out.insert(name.clone());
            for a in args {
                calls_expr(a, out);
            }
        }
        Expr::Spawn { name, args, .. } => {
            out.insert(name.clone());
            for a in args {
                calls_expr(a, out);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            calls_expr(scrutinee, out);
            for a in arms {
                calls_expr(&a.body, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                calls_expr(v, out);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for v in elems {
                calls_expr(v, out);
            }
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

    fn check_src(s: &str) -> Result<(), String> {
        check(&parse(lex(s).unwrap()).unwrap())
    }

    #[test]
    fn accepts_valid_program() {
        assert!(check_src("fn main() -> Int64 { let x = 2 + 3; print(x); return x; }").is_ok());
    }

    #[test]
    fn rejects_type_mismatch() {
        let e = check_src("fn main() -> Int64 { return true; }").unwrap_err();
        assert!(e.contains("return type mismatch"), "{e}");
    }

    // ---- comptime purity (RFC-0021) -------------------------------------

    #[test]
    fn pure_gen_fn_is_accepted() {
        // readFile is mediated (permitted); the rest is ordinary pure code.
        let src = "gen fn g(dir: String) -> String { \
                       let r = readFile(dir) \
                       return \"fn x() -> Int64 { return 0 }\" } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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

    #[test]
    fn gen_fn_using_spawn_is_rejected() {
        let e = check_src(
            "fn sq(x: Int64) -> Int64 { return x * x } \
             gen fn g() -> String { let t = spawn sq(2) return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("not comptime-pure") && e.contains("spawn"), "{e}");
    }

    #[test]
    fn gen_fn_calling_extern_is_rejected() {
        let e = check_src(
            "extern fn host() -> Int64 \
             gen fn g() -> String { let n = host() return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("not comptime-pure") && e.contains("extern"), "{e}");
    }

    #[test]
    fn gen_fn_touching_module_state_is_rejected() {
        let e = check_src(
            "let mut counter = 0 \
             gen fn g() -> String { return counter.toString() } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("not comptime-pure") && e.contains("module state"), "{e}");
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
    fn rejects_assign_to_immutable() {
        let e = check_src("fn main() -> Int64 { let x = 1; x = 2; return x; }").unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn rejects_missing_return() {
        let e = check_src("fn f() -> Int64 { } fn main() -> Int64 { return 0; }").unwrap_err();
        assert!(e.contains("must return"), "{e}");
    }

    // ---- generational references ----------------------------------------

    #[test]
    fn accepts_reference_roundtrip() {
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c)); \
                   let v = get(c); release(c); return v; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn cell_is_generic_over_element_type() {
        // A cell can hold any type; `get` returns exactly that type.
        let src = "fn main() -> Int64 { let c = cell(\"hi\"); \
                   let n = get(c).length; release(c); return n; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_set_of_wrong_element_type() {
        // `c : Ref<Int>`, so setting a String is a type error.
        let e = check_src("fn main() -> Int64 { let c = cell(1); set(c, \"x\"); return 0; }")
            .unwrap_err();
        assert!(e.contains("the cell holds"), "{e}");
    }

    // ---- input I/O (RFC-0014) ---------------------------------------------

    #[test]
    fn io_builtins_have_the_rfc_signatures() {
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

    #[test]
    fn io_builtins_reject_wrong_arguments() {
        let e = check_src("fn main() -> Int64 { let r = readFile(5); return 0 }").unwrap_err();
        assert!(e.contains("`readFile` needs a String path"), "{e}");
        let e = check_src("fn main() -> Int64 { let r = writeFile(\"p\"); return 0 }")
            .unwrap_err();
        assert!(e.contains("`writeFile` takes 2 arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let a = args(1); return 0 }").unwrap_err();
        assert!(e.contains("`args` takes no arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let l = readLine(\"x\"); return 0 }")
            .unwrap_err();
        assert!(e.contains("`readLine` takes no arguments"), "{e}");
        let e =
            check_src("fn main() -> Int64 { let s = stringFromBytes(\"x\"); return 0 }")
                .unwrap_err();
        assert!(e.contains("`stringFromBytes` needs an Array<UInt8>"), "{e}");
    }

    #[test]
    fn io_builtins_are_spawn_forbidden() {
        // A function touching stdin/files/argv is an effect — never a task.
        for body in ["let l = readLine()", "let r = readFile(\"p\")",
                     "let w = writeFile(\"p\", \"c\")", "let a = args()"] {
            let src = format!(
                "fn job() -> Int64 {{ {body} return 0 }} \
                 fn main() -> Int64 {{ let t = spawn job() return t.join() }}"
            );
            let e = check_src(&src).unwrap_err();
            assert!(e.contains("is not allowed"), "{body}: {e}");
        }
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
        // RFC-0014 M2: `bytes(s)` is Array<UInt8> (was Array<Int64>).
        let ok = "fn main() -> Int64 { let b: Array<UInt8> = bytes(\"hi\") return b.length }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
        let e = check_src(
            "fn main() -> Int64 { let b: Array<Int64> = bytes(\"hi\") return b.length }",
        )
        .unwrap_err();
        assert!(e.contains("Array<UInt8>"), "{e}");
    }

    #[test]
    fn rejects_get_of_non_ref() {
        let e = check_src("fn main() -> Int64 { return get(5); }").unwrap_err();
        assert!(e.contains("`get` needs a Ref"), "{e}");
    }

    #[test]
    fn rejects_binding_unit_release() {
        // `release` yields Unit, which cannot be bound.
        let e = check_src("fn main() -> Int64 { let c = cell(1); let x = release(c); return 0; }")
            .unwrap_err();
        assert!(e.contains("Unit"), "{e}");
    }

    // ---- structured concurrency -----------------------------------------

    #[test]
    fn accepts_spawn_of_pure_function() {
        let src = "fn sq(n: Int64) -> Int64 { return n * n; } \
                   fn main() -> Int64 { let t = spawn sq(5); return t.join(); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_spawn_of_impure_function() {
        let e = check_src("fn noisy(n: Int64) -> Int64 { print(n); return n; } \
                           fn main() -> Int64 { let t = spawn noisy(5); return t.join(); }")
            .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_transitively_impure_function() {
        let e = check_src("fn inner(n: Int64) -> Int64 { print(n); return n; } \
                           fn outer(n: Int64) -> Int64 { return inner(n); } \
                           fn main() -> Int64 { let t = spawn outer(5); return t.join(); }")
            .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_join_of_non_task() {
        let e = check_src("fn main() -> Int64 { let x = 5; return x.join(); }").unwrap_err();
        assert!(e.contains("`.join()` needs a Task"), "{e}");
    }

    // ---- extern (RFC-0012 M1) --------------------------------------------

    /// A file with exported declarations is a library module (RFC-0010): it
    /// exists to be imported, so `check` must not demand a `main`. A file with
    /// neither exports nor `main` is still an error (a script missing its
    /// entry point).
    #[test]
    fn library_modules_do_not_need_main() {
        assert!(check_src("export fn double(x: Int64) -> Int64 { return x * 2 }").is_ok());
        assert!(check_src("export type Age = Int64 where value >= 18").is_ok());
        let e = check_src("fn helper(x: Int64) -> Int64 { return x }").unwrap_err();
        assert!(e.contains("no `main` function found"), "{e}");
    }

    // ---- testing (RFC-0015) ---------------------------------------------

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
    fn assert_needs_a_bool() {
        let e = check_src("test \"t\" { assert(5) }").unwrap_err();
        assert!(e.contains("needs a Bool"), "{e}");
    }

    #[test]
    fn duplicate_test_names_are_rejected() {
        let e = check_src("test \"dup\" { assert(true) } test \"dup\" { assert(true) }")
            .unwrap_err();
        assert!(e.contains("duplicate test name"), "{e}");
    }

    #[test]
    fn test_body_analyses_apply() {
        // A use-after-consume-style type error inside a test body is caught: the
        // body is checked exactly like a function body.
        let e = check_src("test \"t\" { let x: Int64 = true }").unwrap_err();
        assert!(e.contains("mismatch") || e.contains("Bool"), "{e}");
    }

    #[test]
    fn extern_signatures_accept_the_abi_domain() {
        // Scalars in, scalar/String/Unit out — the whole v1 boundary domain.
        let src = "extern fn jsLog(msg: String) \
                   extern fn jsNow() -> Float64 \
                   extern fn jsAdd(a: Int64, b: Int64) -> Int64 \
                   extern fn jsFlag(on: Bool, small: UInt8) -> Bool \
                   fn main() -> Int64 { return 0; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn extern_rejects_non_abi_types() {
        // A composite parameter cannot cross the JS boundary in v1.
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

    #[test]
    fn extern_calls_are_not_spawn_safe() {
        // An extern is a host effect; a task calling one (even transitively)
        // is not isolated.
        let e = check_src(
            "extern fn jsNow() -> Float64 \
             fn sample(n: Int64) -> Int64 { let t = jsNow(); return n; } \
             fn main() -> Int64 { let t = spawn sample(1); return t.join(); }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn extern_with_body_is_a_parse_error() {
        let toks = lex("extern fn f() -> Int64 { return 1; } fn main() -> Int64 { return 0; }")
            .unwrap();
        let e = parse(toks).unwrap_err();
        assert!(e.message.contains("an `extern fn` has no body"), "{}", e.message);
    }

    // ---- export extern (RFC-0012 M2) -------------------------------------

    #[test]
    fn export_extern_without_a_body_is_a_parse_error() {
        // The exported direction MUST supply an implementation; a body-less form
        // is an import, which is not how you write `export`.
        let toks =
            lex("export extern fn f() -> Int64 fn main() -> Int64 { return 0 }").unwrap();
        let e = parse(toks).unwrap_err();
        assert!(
            e.message.contains("an exported extern needs a body"),
            "{}",
            e.message
        );
    }

    #[test]
    fn export_extern_with_body_checks_and_enforces_the_abi_domain() {
        // A well-formed exported extern: normal body, ABI-domain signature.
        let src = "export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b } \
                   export extern fn greet(name: String) -> String { return name } \
                   fn main() -> Int64 { return vyrnAdd(1, 2) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));

        // The signature must satisfy the same ABI domain as an import — a
        // composite parameter cannot cross the JS boundary even with a body.
        let e = check_src(
            "export extern fn bad(xs: Array<Int64>) -> Int64 { return 0 } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("cannot cross the JS boundary"), "{e}");
    }

    #[test]
    fn export_extern_body_is_checked_like_any_fn() {
        // The body is a normal Vyrn body — a type error inside it is reported.
        let e = check_src(
            "export extern fn f(a: Int64) -> Int64 { return a + \"x\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(!e.is_empty(), "a type error in the body must be reported: {e}");
    }

    #[test]
    fn export_extern_participates_in_spawn_purity_by_its_body() {
        // A pure-bodied exported extern is spawn-safe (it is a normal fn); one
        // whose body calls an import extern is not (transitive host effect).
        let ok = "export extern fn dbl(n: Int64) -> Int64 { return n + n } \
                  fn main() -> Int64 { let t = spawn dbl(3); return t.join() }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));

        let bad = "extern fn jsNow() -> Float64 \
                   export extern fn impure(n: Int64) -> Int64 { let t = jsNow(); return n } \
                   fn main() -> Int64 { let t = spawn impure(1); return t.join() }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_function_that_drops() {
        // `drop` can release a shared Ref (a shared-state mutation), so a task
        // must not contain it — even though `drop` is a statement, not a call.
        let e = check_src(
            "fn work(r: Ref<Int64>) -> Int64 { let v = get(r); drop r; return v; } \
             fn main() -> Int64 { let c = cell(1); let t = spawn work(c); return t.join(); }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    // ---- modify capability ----------------------------------------------

    #[test]
    fn accepts_modify_with_mut_argument() {
        let src = "type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                   fn main() -> Int64 { let mut c = C { x: 0 }; f(c); return c.x; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_modify_with_immutable_argument() {
        let e = check_src("type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int64 { let c = C { x: 0 }; f(c); return c.x; }")
            .unwrap_err();
        assert!(e.contains("must be declared `mut`"), "{e}");
    }

    #[test]
    fn rejects_modify_with_temporary_argument() {
        let e = check_src("type C = { x: Int64 }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int64 { f(C { x: 0 }); return 0; }")
            .unwrap_err();
        assert!(e.contains("pass a mutable variable"), "{e}");
    }

    // ---- mutable record fields ------------------------------------------

    #[test]
    fn accepts_field_mutation() {
        let src = "type P = { x: Int64, y: Int64 }; \
                   fn main() -> Int64 { let mut p = P { x: 1, y: 2 }; \
                   p.x = 10; return p.x + p.y; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_field_mutation_without_mut() {
        let e = check_src("type P = { x: Int64 }; \
                           fn main() -> Int64 { let p = P { x: 1 }; p.x = 2; return p.x; }")
            .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn rejects_field_mutation_wrong_type() {
        let e = check_src("type P = { x: Int64 }; \
                           fn main() -> Int64 { let mut p = P { x: 1 }; p.x = \"s\"; return 0; }")
            .unwrap_err();
        assert!(e.contains("field `x`"), "{e}");
    }

    // ---- growable arrays ------------------------------------------------

    #[test]
    fn accepts_array_operations() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); return at(a, 0) + alen(a); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_push_wrong_element_type() {
        let e = check_src("fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                           a = push(a, \"x\"); return 0; }")
            .unwrap_err();
        assert!(e.contains("the array holds"), "{e}");
    }

    #[test]
    fn rejects_array_without_element_annotation() {
        let e = check_src("fn main() -> Int64 { let a = array(); return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the element type"), "{e}");
    }

    // ---- in-place array mutation (RFC-0011) -----------------------------

    #[test]
    fn accepts_index_store_pop_swapremove() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30]; \
                   a[1] = 25; let g = a.swapRemove(0); let p = a.pop(); \
                   return a.length + g; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn index_store_requires_mut() {
        let e = check_src("fn main() -> Int64 { let a: Array<Int64> = [1, 2]; a[0] = 9; return 0; }")
            .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn pop_requires_mut() {
        let e = check_src("fn main() -> Int64 { let a: Array<Int64> = [1, 2]; let p = a.pop(); return 0; }")
            .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn index_store_rejects_wrong_element_type() {
        let e = check_src("fn main() -> Int64 { let mut a: Array<Int64> = [1, 2]; a[0] = \"x\"; return 0; }")
            .unwrap_err();
        assert!(e.contains("holds Int64"), "{e}");
    }

    #[test]
    fn index_store_rejects_non_int_index() {
        let e = check_src("fn main() -> Int64 { let mut a: Array<Int64> = [1, 2]; a[\"i\"] = 9; return 0; }")
            .unwrap_err();
        assert!(e.contains("index must be an Int64"), "{e}");
    }

    #[test]
    fn arrayn_allows_store_rejects_pop() {
        // A fixed-size array can store in place but cannot shrink.
        assert!(check_src(
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3]; a[0] = 9; return a[0]; }"
        )
        .is_ok());
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3]; let p = a.pop(); return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("fixed-size array"), "{e}");
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3]; let g = a.swapRemove(0); return g; }",
        )
        .unwrap_err();
        assert!(e.contains("fixed-size array"), "{e}");
    }

    #[test]
    fn index_store_validated_element_rejected_at_compile_time() {
        // A provably-constant value that violates the element predicate is a
        // compile-time error (routes through `prove_coercion` for free).
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
        let e = check_src("fn main() -> Int64 { let mut a: Array<Int64> = [1]; let p = pop(a); return 0; }")
            .unwrap_err();
        assert!(e.contains("pop"), "{e}");
    }

    // ---- region / arena -------------------------------------------------

    #[test]
    fn accepts_region_with_nonheap_result() {
        // A heap temporary lives and dies inside the region; only an Int escapes.
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; let mut n = 0; \
                       region { let s = a + b; n = s.length; } \
                       return n; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_heap_escaping_region() {
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; let mut out = \"\"; \
                       region { out = a + b; } \
                       return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
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
                       let mut a: Array<String> = array() \
                       region { a = push(a, \"x\" + \"y\") } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn rejects_heap_escaping_region_via_set() {
        // Storing an arena string through an outer cell dangles at region exit.
        let src = "fn main() -> Int64 { \
                       let c = cell(\"seed\") \
                       region { set(c, \"a\" + \"b\") } \
                       print(get(c)) release(c) return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn allows_nonheap_stores_out_of_region() {
        // Ints carry no arena memory: pushing into an outer Array<Int> and
        // setting an outer Ref<Int> from inside a region are both fine.
        let src = "fn main() -> Int64 { \
                       let mut a: Array<Int64> = array() \
                       let c = cell(1) \
                       region { a = push(a, 2) set(c, 3) } \
                       release(c) return at(a, 0) }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_region_local_cell_and_array_heap_stores() {
        // A region-local cell/array dies with the region — heap stores are fine.
        let src = "fn main() -> Int64 { \
                       region { \
                           let c = cell(\"seed\") \
                           set(c, \"a\" + \"b\") \
                           print(get(c)) release(c) \
                       } \
                       return 0 }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_region_local_heap_binding() {
        // Assigning a heap value to a region-local `mut` is fine — it dies here.
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; \
                       region { let mut s = a; s = a + b; print(s); } \
                       return 0; }";
        assert!(check_src(src).is_ok());
    }

    // ---- automatic validation: no path skips a `where` predicate ----------

    #[test]
    fn structural_record_into_predicated_named_is_auto_checked() {
        // A structurally-compatible record may flow into a predicated record
        // type — the boundary runs the invariant. A provably-violating
        // constant literal is a COMPILE error…
        let bad = "type Range = { start: Int64, end: Int64 } where start < end \
                   fn span(r: Range) -> Int64 { return r.end - r.start } \
                   fn main() -> Int64 { \
                       return span(Range { start: 10, end: 3 }) }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("violates `where start < end`"), "{e}");
        // …a constant PLAIN record at the boundary is proven there too…
        let bad2 = "type Range = { start: Int64, end: Int64 } where start < end \
                    type Plain = { start: Int64, end: Int64 } \
                    fn span(r: Range) -> Int64 { return r.end - r.start } \
                    fn main() -> Int64 { \
                        return span(Plain { start: 10, end: 3 }) }";
        let e2 = check_src(bad2).unwrap_err();
        assert!(e2.contains("does not satisfy `Range`"), "{e2}");
        // …and a dynamic one compiles (the runtime check traps if violated).
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
        // checked coercion at the return boundary (runtime trap if invalid) —
        // never an unchecked laundering.
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
        // The callee may whole-reassign a `modify` param; writing back through
        // a wider caller record would lose fields — exact type required.
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

    #[test]
    fn rejects_spawn_of_protocol_method_that_prints() {
        // Purity must see through protocol dispatch: the impl body does I/O.
        let src = "protocol Noise { fn burp(self) -> Int64 } \
                   impl Noise for Int64 { fn burp(self) -> Int64 { print(self) return self } } \
                   fn task(n: Int64) -> Int64 { return n.burp() } \
                   fn main() -> Int64 { let t = spawn task(5) return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_function_that_afrees() {
        let src = "fn task(a: Array<Int64>) -> Int64 { afree(a) return 0 } \
                   fn main() -> Int64 { \
                       let a: Array<Int64> = [1, 2] \
                       let t = spawn task(a) \
                       return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn protocol_call_arity_is_checked() {
        let src = "protocol P { fn m(self, k: Int64) -> Int64 } \
                   impl P for Int64 { fn m(self, k: Int64) -> Int64 { return self + k } } \
                   fn go<T: P>(x: T) -> Int64 { return x.m(1, 2, 3) } \
                   fn main() -> Int64 { return go(4) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("expects 1 argument(s) besides `self`"), "{e}");
    }

    #[test]
    fn rejects_nested_option_via_generic_inference() {
        let src = "fn wrap<T>(x: T) -> Option<T> { return Some(x) } \
                   fn main() -> Int64 { \
                       let o = wrap(Some(1)) \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("nested Option/Result"), "{e}");
    }

    #[test]
    fn setfield_on_predicated_record_is_rejected() {
        // In-place field mutation could break the cross-field invariant —
        // rebuild the whole value instead (which re-validates).
        let src = "type Range = { start: Int64, end: Int64 } where start < end \
                   fn main() -> Int64 { \
                       let mut r = Range { start: 1, end: 5 } \
                       r.start = 10 \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot mutate a field of `Range`"), "{e}");
    }

    #[test]
    fn setfield_into_predicated_field_needs_constructed_value() {
        let src = "type Age = Int64 where value >= 18 \
                   type User = { age: Age } \
                   fn main() -> Int64 { \
                       let mut u = User { age: 30 } \
                       u.age = 5 \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("assign an already-constructed `Age`"), "{e}");
        // With an explicitly constructed (and therefore validated) value it's fine.
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
            assert!(e.contains("does not satisfy `Age`"), "case: {src}\ngot: {e}");
        }
    }

    // ---- integer literal ranges ------------------------------------------

    #[test]
    fn rejects_out_of_range_sized_literal() {
        let e = check_src("fn main() -> Int64 { let y: Int8 = 300; return 0; }").unwrap_err();
        assert!(e.contains("does not fit Int8"), "{e}");
        assert!(e.contains("-128..=127"), "{e}");
    }

    #[test]
    fn rejects_out_of_range_literal_adapting_to_sized_sibling() {
        // `x < 300` on a UInt8 would silently truncate 300 to 44 in the compare.
        let e = check_src(
            "fn main() -> Int64 { let x: UInt8 = 200; if x < 300 { return 1 } return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("does not fit UInt8"), "{e}");
    }

    #[test]
    fn rejects_bare_u64_range_literal_in_int_context() {
        // The lexer wraps literals above i64::MAX into the i64 bit pattern;
        // without a UInt64 context that would silently print a negative number.
        let e = check_src("fn main() -> Int64 { let x = 9223372036854775808; return 0; }")
            .unwrap_err();
        assert!(e.contains("exceeds Int64's maximum"), "{e}");
        assert!(e.contains("9223372036854775808"), "{e}");
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

    // ---- validated types ------------------------------------------------

    #[test]
    fn accepts_valid_compile_time_construction() {
        let src = "type Age = Int64 where value >= 18; \
                   fn main() -> Int64 { let a = Age(25); return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_invalid_compile_time_construction() {
        let src = "type Age = Int64 where value >= 18; \
                   fn main() -> Int64 { let a = Age(5); return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
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
    fn rejects_folded_constant_construction() {
        // The offending value need not be a bare literal: any consteval-foldable
        // expression is evaluated and, if provably out of range, rejected.
        let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let a = Age(10 + 5) return 0 }";
        let e = check_src(src).unwrap_err();
        // 10 + 5 folds to 15, which is < 18.
        assert!(e.contains("15 does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn accepts_folded_constant_in_range() {
        // The dual: a foldable expression that PASSES the predicate is proven
        // valid at compile time and costs nothing — never a false rejection.
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

    // ---- Option / match -------------------------------------------------

    #[test]
    fn accepts_option_and_match() {
        let src = "fn f(b: Bool) -> Option<Int64> { if b { return Some(1); } return None; } \
                   fn main() -> Int64 { return match f(true) { Some(x) => x, None => 0 }; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_uninferable_none() {
        let e = check_src("fn main() -> Int64 { let x = None; return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the type of `None`"), "{e}");
    }

    #[test]
    fn rejects_non_exhaustive_match() {
        let src = "fn main() -> Int64 { let o: Option<Int64> = Some(1); \
                   return match o { Some(x) => x }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cover both"), "{e}");
    }

    #[test]
    fn rejects_mismatched_match_arms() {
        let src = "fn main() -> Int64 { let o: Option<Int64> = Some(1); \
                   return match o { Some(x) => x, None => true }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("differing types"), "{e}");
    }

    // ---- Result / ? -----------------------------------------------------

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
        assert!(e.contains("does not match"), "{e}");
    }

    // ---- generic functions ---------------------------------------------

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
                   fn unbox<T>(b: Box<T>) -> T { return b.value; } \
                   fn main() -> Int64 { let n = Box { value: 41 }; return unbox(n); }";
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
    fn rejects_uninferable_generic_nullary() {
        let src = "type Opt<T> = | Wrap(T) | Empty; \
                   fn main() -> Int64 { let x = Empty; return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot infer"), "{e}");
    }

    // ---- strings --------------------------------------------------------

    #[test]
    fn nominal_string_type() {
        // A UserId decays to String for reading, but a raw String is not a UserId.
        let ok = "type UserId = String; \
                  fn show(id: UserId) -> Int64 { print(id); return 0; } \
                  fn main() -> Int64 { return show(UserId(\"a\")); }";
        assert!(check_src(ok).is_ok());
        let bad = "type UserId = String; \
                   fn f(x: UserId) -> Int64 { return 0; } \
                   fn main() -> Int64 { return f(\"raw\"); }";
        assert!(check_src(bad).unwrap_err().contains("UserId"), "raw string rejected");
    }

    #[test]
    fn accepts_strings() {
        let src = "fn main() -> Int64 { let s = \"hi\"; print(s); \
                   if s == \"hi\" { return 1; } return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn string_plus_is_concatenation() {
        // `+` on two Strings concatenates (replacing `concat`); its length is 3.
        let src = "fn main() -> Int64 { let x = \"a\" + \"bc\"; return x.length; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn rejects_mixed_string_int_plus() {
        // `+` needs matching operands: a String and an Int don't concatenate.
        let e = check_src("fn main() -> Int64 { let n = 1; let x = \"a\" + n; return 0; }")
            .unwrap_err();
        assert!(e.contains("`+` concatenates two Strings"), "{e}");
    }

    #[test]
    fn string_record_field() {
        let src = "type U = { name: String, age: Int64 }; \
                   fn nm(u: U) -> Int64 { print(u.name); return u.age; } \
                   fn main() -> Int64 { return nm(U { name: \"x\", age: 7 }); }";
        assert!(check_src(src).is_ok());
    }

    // ---- surface migration: removed free builtins → method/operator forms ----

    #[test]
    fn removed_builtins_emit_migration_hints() {
        let cases = [
            ("fn main() -> Int64 { let s = str(1); return 0; }", "`str(x)` was removed"),
            ("fn main() -> Int64 { let s = concat(\"a\", \"b\"); return 0; }", "`concat(a, b)` was removed"),
            ("fn main() -> Int64 { let s = \"a\"; return len(s); }", "`len(s)` was removed"),
            ("fn main() -> Int64 { let a: Array<Int64> = list([1, 2]); return 0; }", "`list([..])` was removed"),
            ("fn main() -> Int64 { let n = 5; return join(n); }", "`join(t)` was removed"),
            ("fn main() -> Int64 { let s = toString(1); return 0; }", "`toString` is a method"),
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
                       return a.length + c.length + d.length + e.length + f.length; }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn to_string_rejects_non_scalar_receiver() {
        let e = check_src(
            "type P = { x: Int64 } fn main() -> Int64 { let p = P { x: 1 } let s = p.toString() return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("toString"), "{e}");
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
    fn task_join_method_awaits() {
        let src = "fn sq(n: Int64) -> Int64 { return n * n } \
                   fn main() -> Int64 { let t = spawn sq(6); return t.join() }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    // ---- user enums (sum types) ----------------------------------------

    #[test]
    fn accepts_enum_and_match() {
        let src = "type Shape = | Circle(Int64) | Empty; \
                   fn area(s: Shape) -> Int64 { return match s { Circle(r) => r * r, Empty => 0 }; } \
                   fn main() -> Int64 { return area(Circle(3)) + area(Empty); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_non_exhaustive_enum_match() {
        let src = "type E = | A | B; \
                   fn f(e: E) -> Int64 { return match e { A => 1 }; } \
                   fn main() -> Int64 { return f(A); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("missing variant `B`"), "{e}");
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

    // ---- utility transformers (Omit/Pick/Merge) ------------------------

    #[test]
    fn omit_used_via_width_subtyping() {
        let src = "type User = { id: Int64, name: Int64, pw: Int64 }; \
                   type Public = Omit<User, pw>; \
                   fn f(p: Public) -> Int64 { return p.name; } \
                   fn main() -> Int64 { let u = User { id: 1, name: 2, pw: 3 }; return f(u); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn pick_drops_unlisted_fields() {
        let src = "type User = { id: Int64, name: Int64 }; type Id = Pick<User, id>; \
                   fn main() -> Int64 { let i: Id = User { id: 1, name: 2 }; return i.name; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("no field `name`"), "{e}");
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

    // ---- structural records --------------------------------------------

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
    fn accepts_record_width_subtyping() {
        let src = "type Named = { name: Int64 }; type User = { name: Int64, age: Int64 }; \
                   fn greet(w: Named) -> Int64 { return w.name; } \
                   fn main() -> Int64 { let u = User { name: 7, age: 30 }; return greet(u); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_missing_field_in_literal() {
        let src = "type User = { name: Int64, age: Int64 }; \
                   fn main() -> Int64 { let u = User { name: 1 }; return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("missing field"), "{e}");
    }

    #[test]
    fn rejects_narrow_used_as_wide() {
        let src = "type Named = { name: Int64 }; type User = { name: Int64, age: Int64 }; \
                   fn f(u: User) -> Int64 { return u.age; } \
                   fn main() -> Int64 { let n = Named { name: 1 }; return f(n); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("expects"), "{e}");
    }

    #[test]
    fn rejects_unknown_field_access() {
        let src = "type User = { name: Int64 }; \
                   fn main() -> Int64 { let u = User { name: 1 }; return u.age; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("no field `age`"), "{e}");
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
        assert!(check_src(bad).unwrap_err().contains("does not satisfy"), "port");
    }

    #[test]
    fn string_length_refinement() {
        // A `String where value.length ..` type-checks and const-validates.
        let ok = "type Name = String where value.length >= 3; \
                  fn main() -> Int64 { let n = Name(\"bob\"); return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-too-short constant is rejected at compile time.
        let bad = "type Name = String where value.length >= 3; \
                   fn main() -> Int64 { let n = Name(\"ab\"); return 0; }";
        assert!(check_src(bad).unwrap_err().contains("does not satisfy `Name`"), "short");
    }

    #[test]
    fn string_length_is_int() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s.length; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn cross_field_record_predicate() {
        let ok = "type R = { a: Int64, b: Int64 } where a < b; \
                  fn main() -> Int64 { let r = R { a: 1, b: 2 }; return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-violating constant literal is rejected at compile time.
        let bad = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn main() -> Int64 { let r = R { a: 5, b: 1 }; return 0; }";
        assert!(check_src(bad).unwrap_err().contains("violates"), "cross-field");
    }

    #[test]
    fn regex_operator_requires_literal_pattern() {
        let ok = "fn f(s: String) -> Bool { return s =~ \"[a-z]+\"; } \
                  fn main() -> Int64 { return 0; }";
        assert!(check_src(ok).is_ok());
        // A non-literal pattern is rejected.
        let dyn_pat = "fn f(s: String, p: String) -> Bool { return s =~ p; } \
                       fn main() -> Int64 { return 0; }";
        assert!(check_src(dyn_pat).unwrap_err().contains("string-literal pattern"));
        // An invalid regex is rejected at compile time (reversed class range).
        let bad = "fn f(s: String) -> Bool { return s =~ \"[z-a]\"; } \
                   fn main() -> Int64 { return 0; }";
        assert!(check_src(bad).unwrap_err().contains("invalid regex"));
    }

    // ---- module state (RFC-0013) ---------------------------------------

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
    fn assigning_non_mut_global_is_an_error() {
        let e = check_src(
            "let banner = \"hi\"\n\
             fn f() -> Int64 { banner = \"bye\" return 0 }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
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
    fn function_touching_a_global_is_not_spawnable() {
        // `bump` writes a global, so it is not isolated; spawning it is rejected.
        let e = check_src(
            "let mut hits = 0\n\
             fn bump() -> Int64 { hits = hits + 1 return hits }\n\
             fn main() -> Int64 { let t = spawn bump() return t.join() }",
        )
        .unwrap_err();
        assert!(e.contains("isolated") || e.contains("spawn") || e.contains("pure"), "{e}");
    }

    #[test]
    fn spawn_impurity_is_transitive_through_globals() {
        // `outer` calls `bump` (which touches a global); spawning `outer` fails.
        let e = check_src(
            "let mut hits = 0\n\
             fn bump() -> Int64 { hits = hits + 1 return hits }\n\
             fn outer() -> Int64 { return bump() }\n\
             fn main() -> Int64 { let t = spawn outer() return t.join() }",
        )
        .unwrap_err();
        assert!(e.contains("isolated") || e.contains("spawn") || e.contains("pure"), "{e}");
    }

    #[test]
    fn local_shadowing_a_global_may_be_spawned() {
        // A local `hits` shadows the global inside `pure`, so `pure` is isolated.
        let ok = "let mut hits = 0\n\
                  fn pure() -> Int64 { let hits = 5 return hits + 1 }\n\
                  fn main() -> Int64 { let t = spawn pure() return t.join() }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
    }

    #[test]
    fn dropping_a_global_is_an_error() {
        let e = check_src(
            "let s = \"hi\"\n\
             fn f() -> Int64 { drop s return 0 }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("module state"), "{e}");
    }

    #[test]
    fn a_local_shadows_a_global() {
        // `hits` as a local `let` shadows the global; assigning the immutable
        // local is the error (not the global's mutability).
        let e = check_src(
            "let mut hits = 0\n\
             fn f() -> Int64 { let hits = 1 hits = 2 return hits }\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn where_predicate_may_not_reference_a_global() {
        // A global is not a constant; a refinement predicate can't see it.
        let e = check_src(
            "let lo = 3\n\
             type T = Int64 where value >= lo\n\
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("unknown variable") || e.contains("lo"), "{e}");
    }

    // ---- RFC-0011 addendum: `a[i].field = v` write-through --------------

    #[test]
    fn index_field_assign_requires_mut_array() {
        // Storing the modified element back needs a `mut` array (the IndexSet leg).
        let e = check_src(
            "type P = { x: Int64 }\n\
             fn main() -> Int64 { let a: Array<P> = [P { x: 1 }]  a[0].x = 9  return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn index_field_assign_unknown_field_is_rejected() {
        let e = check_src(
            "type P = { x: Int64 }\n\
             fn main() -> Int64 { let mut a: Array<P> = [P { x: 1 }]  a[0].z = 9  return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("no field `z`"), "{e}");
    }

    #[test]
    fn index_field_assign_into_validated_field_is_rejected() {
        // A predicated field type cannot be written in place — same rule (and
        // wording) SetField enforces for a plain record.
        let e = check_src(
            "type Age = Int64 where value >= 0\n\
             type P = { age: Age }\n\
             fn main() -> Int64 { let mut a: Array<P> = []  a.push(P { age: 1 })  a[0].age = 5  return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("(validated)"), "{e}");
    }

    #[test]
    fn index_field_assign_accepts_plain_record_element() {
        assert!(check_src(
            "type P = { x: Int64, y: Int64 }\n\
             fn main() -> Int64 { let mut a: Array<P> = [P { x: 1, y: 2 }]  a[0].x = 9  return 0 }",
        )
        .is_ok());
    }
}
