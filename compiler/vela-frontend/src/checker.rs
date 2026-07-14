//! Type checker for the v0.1 subset (scalars + validated types, RFC-0003).
//!
//! Verifies function signatures, variable use, operator operand types, `mut`
//! assignment, call arity/types, and all-paths return. For validated types it
//! also: type-checks each refinement predicate, validates compile-time-constant
//! constructions (rejecting provably-invalid ones), and enforces that a raw base
//! value cannot be used where a validated type is expected without construction.

use std::cell::RefCell;
use std::collections::HashMap;

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
        if matches!(t.name.as_str(), "Int" | "Bool" | "Unit") {
            out.push(Diagnostic::from_rendered(
                format!("line {}: cannot redefine built-in type `{}`", t.line, t.name),
                "check",
            ));
            continue;
        }
        if types.contains_key(&t.name) {
            out.push(Diagnostic::from_rendered(
                format!("line {}: type `{}` defined twice", t.line, t.name),
                "check",
            ));
            continue;
        }
        types.insert(t.name.clone(), t.clone());
    }

    const RESERVED: &[&str] = &[
        "print", "len", "concat", "Some", "None", "Ok", "Err", "match", "cell", "get", "set",
        "release", "array", "push", "at", "alen", "afree", "str", "parse", "join", "logger",
        "contains", "startsWith", "endsWith", "bytes", "chars",
        "hexEncode", "hexDecode", "base64Encode", "base64Decode", "urlEncode", "urlDecode",
        "trace", "debug", "info", "warn", "error", "value", "list", "schemaOf", "jsonSchema",
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
            // `drop` is a statement (not a call), but `drop`ping a `Ref` releases a
            // shared cell — a shared-state mutation. A spawn-safe task must not do
            // it, so exclude any body containing `drop` (conservatively).
            no_modify
                && !calls.iter().any(|c| SPAWN_FORBIDDEN.contains(&c.as_str()))
                && !contains_drop(&f.body)
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
                    "line {}: `impl {} for {:?}` is not supported — implement protocols for \
                     Int/Bool/String or an enum (validated scalars and records erase at runtime)",
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
    };

    // 3. Validate each type decl (base kind, referenced-type existence, predicate).
    for t in &program.type_decls {
        if let Err(s) = checker.check_type_decl(t) {
            out.push(Diagnostic::from_rendered(s, "check"));
        }
    }

    // 4. main signature. A missing/wrong `main` is a whole-program error (line 0);
    // it does not stop function bodies from being checked below.
    match sigs.get("main") {
        None => out.push(Diagnostic::from_rendered(
            "no `main` function found".to_string(),
            "check",
        )),
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => out.push(Diagnostic::from_rendered(
            "`main` must have signature `fn main() -> Int`".to_string(),
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
            checker.function(f)?;
            Ok(())
        })();
        if let Err(s) = r {
            out.push(Diagnostic::from_rendered(s, "check"));
        }
        // Drain the rest of this function's accumulated body errors.
        for s in checker.errors.borrow_mut().drain(..) {
            out.push(Diagnostic::from_rendered(s, "check"));
        }
    }

    let let_types = checker.let_types.borrow().clone();
    (out, let_types)
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
        // A record type with a cross-field `where` predicate is NOMINAL: only
        // the exact named type may flow in. Width subtyping would smuggle in
        // structurally-identical values that never ran the invariant check
        // (`Plain { start: 10, end: 3 }` as a `Range where start < end`);
        // explicit construction `Range { .. }` is what validates.
        if let Type::Named(n) = to {
            if let Some(d) = self.types.get(n) {
                if d.predicate.is_some() && matches!(d.base, Type::Record(_)) {
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
                        "line {}: cross-field predicate for `{}` must be Bool, found {pty:?}",
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
                "line {}: `{}` must have a scalar base (Int, sized int, Float, Bool, or String)",
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
                    "line {}: refinement predicate for `{}` must be Bool, found {pty:?}",
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

    fn function(&self, f: &Function) -> Result<(), String> {
        *self.cur_bounds.borrow_mut() = f.type_bounds.clone();
        self.errors.borrow_mut().clear();
        let mut scope: Vec<HashMap<String, Binding>> = vec![HashMap::new()];
        for p in &f.params {
            // A `modify` parameter is mutable inside the body (that is the point);
            // others are read-only bindings.
            let mutable = p.capability == Capability::Modify;
            scope[0].insert(p.name.clone(), Binding { ty: p.ty.clone(), mutable });
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
                .push(format!("line {}: function `{}` must return {:?} on all paths", f.line, f.name, f.ret));
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
                    if !self.assignable(&vty, declared) {
                        return Err(format!(
                            "line {line}: `{name}` declared {declared:?} but initializer is {vty:?}"
                        ));
                    }
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
                if !self.assignable(&vty, &b.ty) {
                    return Err(format!(
                        "line {line}: `{name}` is {:?} but assigned {:?}",
                        b.ty, vty
                    ));
                }
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
                let fields = crate::types::record_fields(&b.ty, self.types).ok_or_else(|| {
                    format!("line {line}: `{name}` is not a record, so it has no field `{field}`")
                })?;
                let fty = fields
                    .iter()
                    .find(|f| &f.name == field)
                    .map(|f| f.ty.clone())
                    .ok_or_else(|| format!("line {line}: record `{name}` has no field `{field}`"))?;
                let vty = self.expr(value, scope, Some(&fty), Some(ret))?;
                if !self.assignable(&vty, &fty) {
                    return Err(format!(
                        "line {line}: field `{field}` is {fty:?} but assigned {vty:?}"
                    ));
                }
                self.region_store_guard(name, &fty, scope, *line)?;
                Ok(false)
            }
            Stmt::Return { value, line } => {
                let vty = match value {
                    Some(e) => self.expr(e, scope, Some(ret), Some(ret))?,
                    None => Type::Unit,
                };
                if !self.assignable(&vty, ret) {
                    // Report the mismatch but still count this path as returning:
                    // a `return <wrong type>` does return, so it must NOT also
                    // trigger the "must return on all paths" diagnostic (that
                    // would be a cascade). Push to the sink and return `Ok(true)`.
                    self.errors.borrow_mut().push(format!(
                        "line {line}: return type mismatch: expected {ret:?}, found {vty:?}"
                    ));
                    return Ok(true);
                }
                Ok(true)
            }
            Stmt::If { cond, then_block, else_block, line } => {
                let cty = self.expr(cond, scope, None, Some(ret))?;
                if self.base(&cty) != Type::Bool {
                    return Err(format!("line {line}: `if` condition must be Bool, found {cty:?}"));
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
                    return Err(format!("line {line}: `while` condition must be Bool, found {cty:?}"));
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
                            "line {line}: `for` needs an Array or String to iterate, found {other:?}"
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
                let b = self
                    .lookup(scope, name)
                    .ok_or_else(|| format!("line {line}: `drop` of unbound variable `{name}`"))?;
                match self.base(&b.ty) {
                    Type::Str | Type::Array(_) | Type::Ref(_) => Ok(false),
                    other => Err(format!(
                        "line {line}: `drop` needs a heap value (String, Array, or Ref), \
                         but `{name}` is {other:?}"
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
                self.block(body, ret, scope);
                self.region_floor.borrow_mut().pop();
                // A region never guarantees a return for its enclosing block.
                Ok(false)
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
                            "integer literal {} exceeds Int's maximum \
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
                             add an annotation (e.g. `let x: Option<Int> = None;`)"
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
                    UnOp::Neg => Err(format!("line {line}: unary `-` needs Int or Float, found {t:?}")),
                    UnOp::Not => Err(format!("line {line}: unary `!` needs Bool, found {t:?}")),
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
                            format!("line {line}: type {ety:?} has no field `{field}`")
                        }),
                    other => Err(format!(
                        "line {line}: cannot access field `{field}` on non-record type {other:?}"
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
                        "line {line}: `{name}` is built from {base:?}, but the argument is {aty:?}"
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
                    if !self.assignable(&aty, pty) {
                        return Err(format!(
                            "line {line}: `spawn {name}` argument expects {pty:?}, found {aty:?}"
                        ));
                    }
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
                             e.g. `let a: Array<Int> = [];`"
                        )),
                    };
                }
                // All elements share a type; the result is a fixed-size array.
                let elem_expected = match expected {
                    Some(Type::ArrayN(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                let first = self.expr(&elems[0], scope, elem_expected.as_ref(), fn_ret)?;
                let elem_ty = elem_expected.unwrap_or(first);
                for e in &elems[1..] {
                    let t = self.expr(e, scope, Some(&elem_ty), fn_ret)?;
                    if !self.assignable(&t, &elem_ty) {
                        return Err(format!(
                            "line {line}: array elements must share a type: expected {elem_ty:?}, found {t:?}"
                        ));
                    }
                }
                Ok(Type::ArrayN(Box::new(elem_ty), elems.len()))
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
                     but it returns {ret:?}"
                )),
            },
            Type::Result(t, e) => match ret {
                Type::Result(_, re) if self.assignable(e, re) => Ok((**t).clone()),
                Type::Result(_, re) => Err(format!(
                    "line {line}: `?` propagates error {e:?}, but the function returns \
                     Result<_, {re:?}>"
                )),
                _ => Err(format!(
                    "line {line}: `?` on a Result requires the function to return Result, \
                     but it returns {ret:?}"
                )),
            },
            other => Err(format!("line {line}: `?` needs an Option or Result, found {other:?}")),
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
                    "line {line}: `match` scrutinee must be an Option, Result, or enum, found {other:?}"
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
                        "line {line}: pattern `{n}` does not match scrutinee of type {sty:?}"
                    ))
                }
            };
            if !want.contains(&tag) {
                return Err(format!(
                    "line {line}: pattern `{tag}` does not match scrutinee of type {sty:?}"
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
                .ok_or_else(|| format!("line {line}: `{vname}` is not a variant of {sty:?}"))?;
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
                        "line {line}: `match` arms have differing types: {rt:?} vs {bty:?}"
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
                    "line {line}: cannot combine type parameter `{t}` with {r:?}"
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
            // Arithmetic works on Int, Float, or a sized integer; operands must
            // match exactly (no implicit widening). `Rem` (%) is integer-only.
            Add | Sub | Mul | Div => {
                if l == r && numeric(&l) {
                    Ok(l)
                } else {
                    Err(format!(
                        "line {line}: arithmetic needs matching numeric operands, \
                         found {l:?} and {r:?}"
                    ))
                }
            }
            Rem => {
                if l == r && matches!(l, Type::Int | Type::IntN { .. }) {
                    Ok(l)
                } else {
                    Err(format!("line {line}: `%` needs matching Int operands, found {l:?} and {r:?}"))
                }
            }
            Lt | LtEq | Gt | GtEq => {
                if l == r && numeric(&l) {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: comparison needs matching numeric operands, \
                         found {l:?} and {r:?}"
                    ))
                }
            }
            Eq | NotEq => {
                if l == r && (numeric(&l) || matches!(l, Type::Bool | Type::Str)) {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: `==`/`!=` needs matching scalar operands, found {l:?} and {r:?}"
                    ))
                }
            }
            And | Or => {
                if l == Type::Bool && r == Type::Bool {
                    Ok(Type::Bool)
                } else {
                    Err(format!("line {line}: `&&`/`||` needs Bool operands, found {l:?} and {r:?}"))
                }
            }
            // `=~` matches a String against a regex literal → Bool (the literal
            // requirement and pattern validity are checked at the `Expr::Binary`
            // site, which has the syntax).
            Match => {
                if l == Type::Str && r == Type::Str {
                    Ok(Type::Bool)
                } else {
                    Err(format!("line {line}: `=~` needs a String and a pattern, found {l:?} and {r:?}"))
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
                    "line {line}: print needs a number, Bool, or String, found {t:?}"
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
                return Err(format!("line {line}: `logger` needs a String name, found {t:?}"));
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
                     found {l:?}"
                ));
            }
            let m = self.base(&self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?);
            if matches!(m, Type::Err) {
                return Ok(Type::Err);
            }
            if m != Type::Str {
                return Err(format!("line {line}: `{name}` message must be a String, found {m:?}"));
            }
            return Ok(Type::Unit);
        }

        // built-in: len(String) -> Int
        if name == "len" {
            if args.len() != 1 {
                return Err(format!("line {line}: `len` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `len` needs a String, found {t:?}"));
            }
            return Ok(Type::Int);
        }

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
                    return Err(format!("line {line}: `{name}` needs String arguments, found {t:?}"));
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
                return Err(format!("line {line}: `{name}` needs a String, found {t:?}"));
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
                return Err(format!("line {line}: `{name}` needs a String, found {t:?}"));
            }
            return Ok(Type::Option(Box::new(Type::Str)));
        }

        // built-in: bytes(String) -> Array<Int> (the raw UTF-8 bytes) and
        // chars(String) -> Array<Int> (the Unicode scalar values / code points).
        if matches!(name, "bytes" | "chars") {
            if args.len() != 1 {
                return Err(format!("line {line}: `{name}` takes 1 argument, got {}", args.len()));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `{name}` needs a String, found {t:?}"));
            }
            return Ok(Type::Array(Box::new(Type::Int)));
        }

        // built-in: concat(String, String) -> String (heap-allocated)
        if name == "concat" {
            if args.len() != 2 {
                return Err(format!("line {line}: `concat` takes 2 arguments, got {}", args.len()));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, None, fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!("line {line}: `concat` needs Strings, found {t:?}"));
                }
            }
            return Ok(Type::Str);
        }

        // join(Task<T>) -> T — await a spawned task's result.
        if name == "join" {
            if args.len() != 1 {
                return Err(format!("line {line}: `join` takes 1 argument, got {}", args.len()));
            }
            match self.base(&self.expr(&args[0], scope, None, fn_ret)?) {
                Type::Task(inner) => return Ok((*inner).clone()),
                Type::Err => return Ok(Type::Err),
                other => return Err(format!("line {line}: `join` needs a Task, found {other:?}")),
            }
        }

        // Int/String conversions (checked narrowing). str is total; parse is
        // fallible (returns None on a non-integer string).
        if name == "str" {
            if args.len() != 1 {
                return Err(format!("line {line}: `str` takes 1 argument, got {}", args.len()));
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
                    "line {line}: `str` renders an Int, sized int, Float, Bool, or String, found {t:?}"
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
                return Err(format!("line {line}: `parse` needs a String, found {t:?}"));
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
                other => return Err(format!("line {line}: `get` needs a Ref, found {other:?}")),
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
                        "line {line}: `set` needs a Ref as its first argument, found {other:?}"
                    ))
                }
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.assignable(&v, &elem) {
                return Err(format!(
                    "line {line}: `set` value is {v:?} but the cell holds {elem:?}"
                ));
            }
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
                return Err(format!("line {line}: `release` needs a Ref, found {rt:?}"));
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
                         e.g. `let a: Array<Int> = array();`"
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
                        "line {line}: `push` needs an Array as its first argument, found {other:?}"
                    ))
                }
            };
            let v = self.expr(&args[1], scope, Some(&elem), fn_ret)?;
            if !self.assignable(&v, &elem) {
                return Err(format!(
                    "line {line}: `push` value is {v:?} but the array holds {elem:?}"
                ));
            }
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
                        "line {line}: indexing needs an Array or String, found {other:?}"
                    ))
                }
            };
            let i = self.base(&self.expr(&args[1], scope, Some(&Type::Int), fn_ret)?);
            if matches!(i, Type::Err) {
                return Ok(Type::Err);
            }
            if i != Type::Int {
                return Err(format!("line {line}: `at` index must be an Int, found {i:?}"));
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
                return Err(format!("line {line}: `alen` needs an Array, found {at:?}"));
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
                return Err(format!("line {line}: `afree` needs an Array, found {at:?}"));
            }
            return Ok(Type::Unit);
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
                    "line {line}: `{name}(..)` converts a number, found {src:?}"
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
                    "line {line}: `value` boxes an Int, Bool, or String, found {t:?}"
                ));
            }
            return Ok(Type::Named("Value".to_string()));
        }
        // built-in: list(Array<T, N>) -> Array<T> — a fixed array as a growable one
        // (RFC-0007 tagged-template desugar; the tag takes size-erased arrays).
        if name == "list" {
            if args.len() != 1 {
                return Err(format!("line {line}: `list` takes 1 argument, got {}", args.len()));
            }
            let a = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&a) {
                Type::ArrayN(inner, _) | Type::Array(inner) => {
                    return Ok(Type::Array(inner))
                }
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!("line {line}: `list` needs an Array, found {other:?}"))
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
                if !self.assignable(&aty, want) {
                    return Err(format!(
                        "line {line}: `Some` payload is {aty:?} but Option<{want:?}> was expected"
                    ));
                }
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
                         (e.g. `-> Result<Int, Int>`)"
                    ))
                }
            };
            let want_ty = if name == "Ok" { &t } else { &e };
            if !self.assignable(&aty, want_ty) {
                return Err(format!(
                    "line {line}: `{name}` payload is {aty:?} but {want_ty:?} was expected"
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
                        if !self.assignable(&aty, pty) {
                            return Err(format!(
                                "line {line}: `{name}` argument is {aty:?}, expected {pty:?}"
                            ));
                        }
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
                        "line {line}: {recv:?} does not implement protocol `{proto}` \
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
                                "line {line}: `{name}` requires `{tp}: {b}`, but {concrete:?} does not satisfy `{b}`"
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
            if !self.assignable(&aty, pty) {
                return Err(format!(
                    "line {line}: `{name}` argument {} expects {pty:?}, found {aty:?}",
                    i + 1
                ));
            }
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
                 {pty:?}, found {aty:?} (width subtyping is read-only: a wider \
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
                            "line {line}: type parameter `{t}` is both {bound:?} and {aty:?}"
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
                _ => Err(format!("line {line}: expected Option, found {aty:?}")),
            },
            Type::Result(pt, pe) => match aty {
                Type::Result(at, ae) => {
                    self.unify(pt, at, subst, line)?;
                    self.unify(pe, ae, subst, line)
                }
                _ => Err(format!("line {line}: expected Result, found {aty:?}")),
            },
            Type::App(pn, pargs) => match aty {
                Type::App(an, aargs) if pn == an && pargs.len() == aargs.len() => {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.unify(p, a, subst, line)?;
                    }
                    Ok(())
                }
                _ => Err(format!("line {line}: expected {pty:?}, found {aty:?}")),
            },
            _ => {
                if !self.assignable(aty, pty) {
                    Err(format!("line {line}: argument expects {pty:?}, found {aty:?}"))
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
                "line {line}: `{}` is built from {:?}, but the argument is {aty:?}",
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
                            "line {line}: {:?} does not satisfy `{}` (predicate `where {}` is false)",
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
];

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
        assert!(check_src("fn main() -> Int { let x = 2 + 3; print(x); return x; }").is_ok());
    }

    #[test]
    fn rejects_type_mismatch() {
        let e = check_src("fn main() -> Int { return true; }").unwrap_err();
        assert!(e.contains("return type mismatch"), "{e}");
    }

    #[test]
    fn rejects_assign_to_immutable() {
        let e = check_src("fn main() -> Int { let x = 1; x = 2; return x; }").unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn rejects_missing_return() {
        let e = check_src("fn f() -> Int { } fn main() -> Int { return 0; }").unwrap_err();
        assert!(e.contains("must return"), "{e}");
    }

    // ---- generational references ----------------------------------------

    #[test]
    fn accepts_reference_roundtrip() {
        let src = "fn main() -> Int { let c = cell(1); set(c, get(c)); \
                   let v = get(c); release(c); return v; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn cell_is_generic_over_element_type() {
        // A cell can hold any type; `get` returns exactly that type.
        let src = "fn main() -> Int { let c = cell(\"hi\"); \
                   let n = len(get(c)); release(c); return n; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_set_of_wrong_element_type() {
        // `c : Ref<Int>`, so setting a String is a type error.
        let e = check_src("fn main() -> Int { let c = cell(1); set(c, \"x\"); return 0; }")
            .unwrap_err();
        assert!(e.contains("the cell holds"), "{e}");
    }

    #[test]
    fn rejects_get_of_non_ref() {
        let e = check_src("fn main() -> Int { return get(5); }").unwrap_err();
        assert!(e.contains("`get` needs a Ref"), "{e}");
    }

    #[test]
    fn rejects_binding_unit_release() {
        // `release` yields Unit, which cannot be bound.
        let e = check_src("fn main() -> Int { let c = cell(1); let x = release(c); return 0; }")
            .unwrap_err();
        assert!(e.contains("Unit"), "{e}");
    }

    // ---- structured concurrency -----------------------------------------

    #[test]
    fn accepts_spawn_of_pure_function() {
        let src = "fn sq(n: Int) -> Int { return n * n; } \
                   fn main() -> Int { let t = spawn sq(5); return join(t); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_spawn_of_impure_function() {
        let e = check_src("fn noisy(n: Int) -> Int { print(n); return n; } \
                           fn main() -> Int { let t = spawn noisy(5); return join(t); }")
            .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_transitively_impure_function() {
        let e = check_src("fn inner(n: Int) -> Int { print(n); return n; } \
                           fn outer(n: Int) -> Int { return inner(n); } \
                           fn main() -> Int { let t = spawn outer(5); return join(t); }")
            .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_join_of_non_task() {
        let e = check_src("fn main() -> Int { return join(5); }").unwrap_err();
        assert!(e.contains("`join` needs a Task"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_function_that_drops() {
        // `drop` can release a shared Ref (a shared-state mutation), so a task
        // must not contain it — even though `drop` is a statement, not a call.
        let e = check_src(
            "fn work(r: Ref<Int>) -> Int { let v = get(r); drop r; return v; } \
             fn main() -> Int { let c = cell(1); let t = spawn work(c); return join(t); }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    // ---- modify capability ----------------------------------------------

    #[test]
    fn accepts_modify_with_mut_argument() {
        let src = "type C = { x: Int }; fn f(c: modify C) { c.x = 1; } \
                   fn main() -> Int { let mut c = C { x: 0 }; f(c); return c.x; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_modify_with_immutable_argument() {
        let e = check_src("type C = { x: Int }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int { let c = C { x: 0 }; f(c); return c.x; }")
            .unwrap_err();
        assert!(e.contains("must be declared `mut`"), "{e}");
    }

    #[test]
    fn rejects_modify_with_temporary_argument() {
        let e = check_src("type C = { x: Int }; fn f(c: modify C) { c.x = 1; } \
                           fn main() -> Int { f(C { x: 0 }); return 0; }")
            .unwrap_err();
        assert!(e.contains("pass a mutable variable"), "{e}");
    }

    // ---- mutable record fields ------------------------------------------

    #[test]
    fn accepts_field_mutation() {
        let src = "type P = { x: Int, y: Int }; \
                   fn main() -> Int { let mut p = P { x: 1, y: 2 }; \
                   p.x = 10; return p.x + p.y; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_field_mutation_without_mut() {
        let e = check_src("type P = { x: Int }; \
                           fn main() -> Int { let p = P { x: 1 }; p.x = 2; return p.x; }")
            .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn rejects_field_mutation_wrong_type() {
        let e = check_src("type P = { x: Int }; \
                           fn main() -> Int { let mut p = P { x: 1 }; p.x = \"s\"; return 0; }")
            .unwrap_err();
        assert!(e.contains("field `x`"), "{e}");
    }

    // ---- growable arrays ------------------------------------------------

    #[test]
    fn accepts_array_operations() {
        let src = "fn main() -> Int { let mut a: Array<Int> = array(); \
                   a = push(a, 1); return at(a, 0) + alen(a); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_push_wrong_element_type() {
        let e = check_src("fn main() -> Int { let mut a: Array<Int> = array(); \
                           a = push(a, \"x\"); return 0; }")
            .unwrap_err();
        assert!(e.contains("the array holds"), "{e}");
    }

    #[test]
    fn rejects_array_without_element_annotation() {
        let e = check_src("fn main() -> Int { let a = array(); return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the element type"), "{e}");
    }

    // ---- region / arena -------------------------------------------------

    #[test]
    fn accepts_region_with_nonheap_result() {
        // A heap temporary lives and dies inside the region; only an Int escapes.
        let src = "fn main() -> Int { \
                       let a = \"x\"; let b = \"y\"; let mut n = 0; \
                       region { let s = concat(a, b); n = len(s); } \
                       return n; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_heap_escaping_region() {
        let src = "fn main() -> Int { \
                       let a = \"x\"; let b = \"y\"; let mut out = \"\"; \
                       region { out = concat(a, b); } \
                       return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn rejects_heap_escaping_region_via_field() {
        // Storing an arena string into an outer record's field dangles too.
        let src = "type Holder = { s: String } \
                   fn main() -> Int { \
                       let mut h = Holder { s: \"init\" } \
                       region { h.s = concat(\"a\", \"b\") } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn rejects_heap_escaping_region_via_push() {
        // Pushing an arena string into an outer array outlives the region.
        let src = "fn main() -> Int { \
                       let mut a: Array<String> = array() \
                       region { a = push(a, concat(\"x\", \"y\")) } \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn rejects_heap_escaping_region_via_set() {
        // Storing an arena string through an outer cell dangles at region exit.
        let src = "fn main() -> Int { \
                       let c = cell(\"seed\") \
                       region { set(c, concat(\"a\", \"b\")) } \
                       print(get(c)) release(c) return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("outlives the enclosing `region`"), "{e}");
    }

    #[test]
    fn allows_nonheap_stores_out_of_region() {
        // Ints carry no arena memory: pushing into an outer Array<Int> and
        // setting an outer Ref<Int> from inside a region are both fine.
        let src = "fn main() -> Int { \
                       let mut a: Array<Int> = array() \
                       let c = cell(1) \
                       region { a = push(a, 2) set(c, 3) } \
                       release(c) return at(a, 0) }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_region_local_cell_and_array_heap_stores() {
        // A region-local cell/array dies with the region — heap stores are fine.
        let src = "fn main() -> Int { \
                       region { \
                           let c = cell(\"seed\") \
                           set(c, concat(\"a\", \"b\")) \
                           print(get(c)) release(c) \
                       } \
                       return 0 }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn allows_region_local_heap_binding() {
        // Assigning a heap value to a region-local `mut` is fine — it dies here.
        let src = "fn main() -> Int { \
                       let a = \"x\"; let b = \"y\"; \
                       region { let mut s = a; s = concat(a, b); print(s); } \
                       return 0; }";
        assert!(check_src(src).is_ok());
    }

    // ---- soundness: validated types cannot be bypassed --------------------

    #[test]
    fn rejects_structural_record_as_predicated_named() {
        // A predicated record is nominal: a structurally-identical plain record
        // must not flow in without running the invariant.
        let src = "type Range = { start: Int, end: Int } where start < end \
                   type Plain = { start: Int, end: Int } \
                   fn span(r: Range) -> Int { return r.end - r.start } \
                   fn main() -> Int { \
                       let p = Plain { start: 10, end: 3 } \
                       return span(p) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("expects Named(\"Range\")"), "{e}");
    }

    #[test]
    fn accepts_predicated_named_record_itself() {
        let src = "type Range = { start: Int, end: Int } where start < end \
                   fn span(r: Range) -> Int { return r.end - r.start } \
                   fn main() -> Int { \
                       let r = Range { start: 1, end: 5 } \
                       return span(r) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn match_arm_cannot_launder_validated_scalar() {
        // A raw-Int arm joins the match to Int, so returning it as `Age`
        // fails — the refinement can't be skipped via arm unification.
        let src = "type Age = Int where value >= 18 \
                   fn pick(o: Option<Int>) -> Age { \
                       return match o { Some(x) => 5, None => 5 } } \
                   fn main() -> Int { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("return type mismatch"), "{e}");
    }

    #[test]
    fn rejects_modify_with_wider_record() {
        // The callee may whole-reassign a `modify` param; writing back through
        // a wider caller record would lose fields — exact type required.
        let src = "type Named = { name: Int } \
                   type User = { name: Int, age: Int } \
                   fn clobber(n: modify Named) { n = Named { name: 5 } } \
                   fn main() -> Int { \
                       let mut u = User { name: 1, age: 30 } \
                       clobber(u) \
                       return u.age }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("needs exactly"), "{e}");
    }

    #[test]
    fn generic_calls_enforce_modify_discipline() {
        // The generic-inference path must run the same capability checks.
        let src = "type C = { x: Int } \
                   fn f<T>(c: modify C, tag: T) -> Int { c.x = 99 return 0 } \
                   fn main() -> Int { \
                       let c = C { x: 1 } \
                       let r = f(c, 0) \
                       return c.x }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("must be declared `mut`"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_protocol_method_that_prints() {
        // Purity must see through protocol dispatch: the impl body does I/O.
        let src = "protocol Noise { fn burp(self) -> Int } \
                   impl Noise for Int { fn burp(self) -> Int { print(self) return self } } \
                   fn task(n: Int) -> Int { return n.burp() } \
                   fn main() -> Int { let t = spawn task(5) return join(t) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_function_that_afrees() {
        let src = "fn task(a: Array<Int>) -> Int { afree(a) return 0 } \
                   fn main() -> Int { \
                       let a = list([1, 2]) \
                       let t = spawn task(a) \
                       return join(t) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn protocol_call_arity_is_checked() {
        let src = "protocol P { fn m(self, k: Int) -> Int } \
                   impl P for Int { fn m(self, k: Int) -> Int { return self + k } } \
                   fn go<T: P>(x: T) -> Int { return x.m(1, 2, 3) } \
                   fn main() -> Int { return go(4) }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("expects 1 argument(s) besides `self`"), "{e}");
    }

    #[test]
    fn rejects_nested_option_via_generic_inference() {
        let src = "fn wrap<T>(x: T) -> Option<T> { return Some(x) } \
                   fn main() -> Int { \
                       let o = wrap(Some(1)) \
                       return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("nested Option/Result"), "{e}");
    }

    // ---- integer literal ranges ------------------------------------------

    #[test]
    fn rejects_out_of_range_sized_literal() {
        let e = check_src("fn main() -> Int { let y: Int8 = 300; return 0; }").unwrap_err();
        assert!(e.contains("does not fit Int8"), "{e}");
        assert!(e.contains("-128..=127"), "{e}");
    }

    #[test]
    fn rejects_out_of_range_literal_adapting_to_sized_sibling() {
        // `x < 300` on a UInt8 would silently truncate 300 to 44 in the compare.
        let e = check_src(
            "fn main() -> Int { let x: UInt8 = 200; if x < 300 { return 1 } return 0 }",
        )
        .unwrap_err();
        assert!(e.contains("does not fit UInt8"), "{e}");
    }

    #[test]
    fn rejects_bare_u64_range_literal_in_int_context() {
        // The lexer wraps literals above i64::MAX into the i64 bit pattern;
        // without a UInt64 context that would silently print a negative number.
        let e = check_src("fn main() -> Int { let x = 9223372036854775808; return 0; }")
            .unwrap_err();
        assert!(e.contains("exceeds Int's maximum"), "{e}");
        assert!(e.contains("9223372036854775808"), "{e}");
    }

    #[test]
    fn accepts_u64_range_literal_as_uint64_and_i64_min() {
        let src = "fn main() -> Int { \
                       let x: UInt64 = 18446744073709551615 \
                       let m = -9223372036854775808 \
                       if m < 0 { return 0 } return 1 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn accepts_in_range_sized_literals() {
        let src = "fn main() -> Int { \
                       let a: Int8 = 127 \
                       let b: UInt8 = 255 \
                       let c: Int32 = 2147483647 \
                       if a < 127 { return 1 } return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    // ---- validated types ------------------------------------------------

    #[test]
    fn accepts_valid_compile_time_construction() {
        let src = "type Age = Int where value >= 18; \
                   fn main() -> Int { let a = Age(25); return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_invalid_compile_time_construction() {
        let src = "type Age = Int where value >= 18; \
                   fn main() -> Int { let a = Age(5); return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Age`"), "{e}");
    }

    #[test]
    fn validated_decays_to_base_but_not_reverse() {
        // an Age is usable as an Int...
        let ok = "type Age = Int where value >= 18; \
                  fn f(n: Int) -> Int { return n; } \
                  fn main() -> Int { return f(Age(20)); }";
        assert!(check_src(ok).is_ok());
        // ...but a raw Int is NOT usable as an Age without construction
        let bad = "type Age = Int where value >= 18; \
                   fn g(a: Age) -> Int { return 0; } \
                   fn main() -> Int { return g(20); }";
        let e = check_src(bad).unwrap_err();
        assert!(e.contains("expects Named(\"Age\")"), "{e}");
    }

    #[test]
    fn rejects_predicate_with_call() {
        let src = "type Bad = Int where print(value) == value; \
                   fn main() -> Int { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("may not contain calls"), "{e}");
    }

    // ---- Option / match -------------------------------------------------

    #[test]
    fn accepts_option_and_match() {
        let src = "fn f(b: Bool) -> Option<Int> { if b { return Some(1); } return None; } \
                   fn main() -> Int { return match f(true) { Some(x) => x, None => 0 }; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_uninferable_none() {
        let e = check_src("fn main() -> Int { let x = None; return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the type of `None`"), "{e}");
    }

    #[test]
    fn rejects_non_exhaustive_match() {
        let src = "fn main() -> Int { let o: Option<Int> = Some(1); \
                   return match o { Some(x) => x }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cover both"), "{e}");
    }

    #[test]
    fn rejects_mismatched_match_arms() {
        let src = "fn main() -> Int { let o: Option<Int> = Some(1); \
                   return match o { Some(x) => x, None => true }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("differing types"), "{e}");
    }

    // ---- Result / ? -----------------------------------------------------

    #[test]
    fn accepts_result_and_question_mark() {
        let src = "fn f(n: Int) -> Result<Int, Int> { if n == 0 { return Err(1); } return Ok(n); } \
                   fn g(n: Int) -> Result<Int, Int> { let x = f(n)?; return Ok(x + 1); } \
                   fn main() -> Int { return match g(5) { Ok(v) => v, Err(e) => e }; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_question_mark_when_function_returns_scalar() {
        let src = "fn f() -> Result<Int, Int> { return Ok(1); } \
                   fn main() -> Int { let x = f()?; return x; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("requires the function to return Result"), "{e}");
    }

    #[test]
    fn rejects_uninferable_ok() {
        let e = check_src("fn main() -> Int { let x = Ok(1); return 0; }").unwrap_err();
        assert!(e.contains("cannot infer the type of `Ok"), "{e}");
    }

    #[test]
    fn rejects_wrong_pattern_for_scrutinee() {
        let src = "fn main() -> Int { let o: Option<Int> = Some(1); \
                   return match o { Ok(x) => x, None => 0 }; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not match"), "{e}");
    }

    // ---- generic functions ---------------------------------------------

    #[test]
    fn accepts_generic_function() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn main() -> Int { print(id(\"hi\")); return id(5); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn generic_calls_generic() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn wrap<U>(x: U) -> U { return id(x); } \
                   fn main() -> Int { return wrap(7); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_operation_on_unbounded_type_param() {
        let src = "fn bad<T>(x: T) -> T { return x + x; } \
                   fn main() -> Int { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("needs a `Num` bound"), "{e}");
    }

    #[test]
    fn constrained_generic_operators() {
        let src = "fn max<T: Ord>(a: T, b: T) -> T { if a > b { return a; } return b; } \
                   fn main() -> Int { return max(3, 9); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_bound_violation_at_call() {
        let src = "fn max<T: Ord>(a: T, b: T) -> T { if a > b { return a; } return b; } \
                   fn main() -> Int { let x = max(true, false); return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does not satisfy `Ord`"), "{e}");
    }

    #[test]
    fn rejects_inconsistent_type_param() {
        let src = "fn two<T>(a: T, b: T) -> Int { return 0; } \
                   fn main() -> Int { return two(1, \"s\"); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("both"), "{e}");
    }

    #[test]
    fn accepts_generic_record() {
        let src = "type Box<T> = { value: T }; \
                   fn unbox<T>(b: Box<T>) -> T { return b.value; } \
                   fn main() -> Int { let n = Box { value: 41 }; return unbox(n); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_generic_type_without_args() {
        let src = "type Box<T> = { value: T }; \
                   fn f(b: Box) -> Int { return 0; } \
                   fn main() -> Int { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("is generic"), "{e}");
    }

    #[test]
    fn rejects_wrong_type_arg_count() {
        let src = "type Pair<A, B> = { a: A, b: B }; \
                   fn f(p: Pair<Int>) -> Int { return 0; } \
                   fn main() -> Int { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("type argument"), "{e}");
    }

    #[test]
    fn accepts_generic_enum() {
        let src = "type Opt<T> = | Wrap(T) | Empty; \
                   fn oe<T>(o: Opt<T>, d: T) -> T { return match o { Wrap(x) => x, Empty => d }; } \
                   fn main() -> Int { let a = Wrap(41); let b: Opt<Int> = Empty; \
                                      return oe(a, 0) + oe(b, 1); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_uninferable_generic_nullary() {
        let src = "type Opt<T> = | Wrap(T) | Empty; \
                   fn main() -> Int { let x = Empty; return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot infer"), "{e}");
    }

    // ---- strings --------------------------------------------------------

    #[test]
    fn nominal_string_type() {
        // A UserId decays to String for reading, but a raw String is not a UserId.
        let ok = "type UserId = String; \
                  fn show(id: UserId) -> Int { print(id); return 0; } \
                  fn main() -> Int { return show(UserId(\"a\")); }";
        assert!(check_src(ok).is_ok());
        let bad = "type UserId = String; \
                   fn f(x: UserId) -> Int { return 0; } \
                   fn main() -> Int { return f(\"raw\"); }";
        assert!(check_src(bad).unwrap_err().contains("UserId"), "raw string rejected");
    }

    #[test]
    fn accepts_strings() {
        let src = "fn main() -> Int { let s = \"hi\"; print(s); \
                   if s == \"hi\" { return 1; } return 0; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_string_arithmetic() {
        let e = check_src("fn main() -> Int { let x = \"a\" + \"b\"; return 0; }").unwrap_err();
        assert!(e.contains("arithmetic needs matching numeric"), "{e}");
    }

    #[test]
    fn string_record_field() {
        let src = "type U = { name: String, age: Int }; \
                   fn nm(u: U) -> Int { print(u.name); return u.age; } \
                   fn main() -> Int { return nm(U { name: \"x\", age: 7 }); }";
        assert!(check_src(src).is_ok());
    }

    // ---- user enums (sum types) ----------------------------------------

    #[test]
    fn accepts_enum_and_match() {
        let src = "type Shape = | Circle(Int) | Empty; \
                   fn area(s: Shape) -> Int { return match s { Circle(r) => r * r, Empty => 0 }; } \
                   fn main() -> Int { return area(Circle(3)) + area(Empty); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_non_exhaustive_enum_match() {
        let src = "type E = | A | B; \
                   fn f(e: E) -> Int { return match e { A => 1 }; } \
                   fn main() -> Int { return f(A); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("missing variant `B`"), "{e}");
    }

    #[test]
    fn rejects_unknown_variant_pattern() {
        let src = "type E = | A | B; \
                   fn f(e: E) -> Int { return match e { A => 1, B => 2, C => 3 }; } \
                   fn main() -> Int { return f(A); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("not a variant"), "{e}");
    }

    #[test]
    fn rejects_payload_variant_without_binding() {
        let src = "type E = | Val(Int) | Empty; \
                   fn f(e: E) -> Int { return match e { Val => 1, Empty => 0 }; } \
                   fn main() -> Int { return f(Empty); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("payload") && e.contains("binds"), "{e}");
    }

    #[test]
    fn multi_payload_variant() {
        let src = "type Shape = | Rect(Int, Int) | Empty; \
                   fn area(s: Shape) -> Int { return match s { Rect(w, h) => w * h, Empty => 0 }; } \
                   fn main() -> Int { return area(Rect(3, 4)); }";
        assert!(check_src(src).is_ok());
    }

    // ---- utility transformers (Omit/Pick/Merge) ------------------------

    #[test]
    fn omit_used_via_width_subtyping() {
        let src = "type User = { id: Int, name: Int, pw: Int }; \
                   type Public = Omit<User, pw>; \
                   fn f(p: Public) -> Int { return p.name; } \
                   fn main() -> Int { let u = User { id: 1, name: 2, pw: 3 }; return f(u); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn pick_drops_unlisted_fields() {
        let src = "type User = { id: Int, name: Int }; type Id = Pick<User, id>; \
                   fn main() -> Int { let i: Id = User { id: 1, name: 2 }; return i.name; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("no field `name`"), "{e}");
    }

    #[test]
    fn merge_combines_fields() {
        let src = "type A = { x: Int }; type B = { y: Int }; type C = Merge<A, B>; \
                   fn main() -> Int { let c = C { x: 1, y: 2 }; return c.x + c.y; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn partial_wraps_fields_in_option() {
        let src = "type U = { a: Int }; type P = Partial<U>; \
                   fn f(p: P) -> Int { return match p.a { Some(n) => n, None => 0 }; } \
                   fn main() -> Int { return f(P { a: Some(5) }); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_unknown_transformer_key() {
        let src = "type U = { a: Int }; type B = Omit<U, zzz>; fn main() -> Int { return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("not in the transformer"), "{e}");
    }

    // ---- structural records --------------------------------------------

    #[test]
    fn intersection_type_merges_fields() {
        let src = "type User = { name: Int, age: Int }; \
                   type Employee = User & { salary: Int }; \
                   fn total(e: Employee) -> Int { return e.age + e.salary; } \
                   fn main() -> Int { let e = Employee { name: 1, age: 30, salary: 100 }; \
                                      return total(e); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn accepts_record_width_subtyping() {
        let src = "type Named = { name: Int }; type User = { name: Int, age: Int }; \
                   fn greet(w: Named) -> Int { return w.name; } \
                   fn main() -> Int { let u = User { name: 7, age: 30 }; return greet(u); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn rejects_missing_field_in_literal() {
        let src = "type User = { name: Int, age: Int }; \
                   fn main() -> Int { let u = User { name: 1 }; return 0; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("missing field"), "{e}");
    }

    #[test]
    fn rejects_narrow_used_as_wide() {
        let src = "type Named = { name: Int }; type User = { name: Int, age: Int }; \
                   fn f(u: User) -> Int { return u.age; } \
                   fn main() -> Int { let n = Named { name: 1 }; return f(n); }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("expects"), "{e}");
    }

    #[test]
    fn rejects_unknown_field_access() {
        let src = "type User = { name: Int }; \
                   fn main() -> Int { let u = User { name: 1 }; return u.age; }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("no field `age`"), "{e}");
    }

    #[test]
    fn fallible_construction_returns_option() {
        let src = "type Age = Int where value >= 18; \
                   fn f(n: Int) -> Int { return match Age?(n) { Some(a) => a, None => 0 }; } \
                   fn main() -> Int { return f(20); }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn range_style_predicate() {
        let src = "type Port = Int where value >= 1 && value <= 65535; \
                   fn main() -> Int { let p = Port(8080); return 0; }";
        assert!(check_src(src).is_ok());
        let bad = "type Port = Int where value >= 1 && value <= 65535; \
                   fn main() -> Int { let p = Port(70000); return 0; }";
        assert!(check_src(bad).unwrap_err().contains("does not satisfy"), "port");
    }

    #[test]
    fn string_length_refinement() {
        // A `String where value.length ..` type-checks and const-validates.
        let ok = "type Name = String where value.length >= 3; \
                  fn main() -> Int { let n = Name(\"bob\"); return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-too-short constant is rejected at compile time.
        let bad = "type Name = String where value.length >= 3; \
                   fn main() -> Int { let n = Name(\"ab\"); return 0; }";
        assert!(check_src(bad).unwrap_err().contains("does not satisfy `Name`"), "short");
    }

    #[test]
    fn string_length_is_int() {
        let src = "fn main() -> Int { let s = \"hi\"; return s.length; }";
        assert!(check_src(src).is_ok());
    }

    #[test]
    fn cross_field_record_predicate() {
        let ok = "type R = { a: Int, b: Int } where a < b; \
                  fn main() -> Int { let r = R { a: 1, b: 2 }; return 0; }";
        assert!(check_src(ok).is_ok());
        // A provably-violating constant literal is rejected at compile time.
        let bad = "type R = { a: Int, b: Int } where a < b; \
                   fn main() -> Int { let r = R { a: 5, b: 1 }; return 0; }";
        assert!(check_src(bad).unwrap_err().contains("violates"), "cross-field");
    }

    #[test]
    fn regex_operator_requires_literal_pattern() {
        let ok = "fn f(s: String) -> Bool { return s =~ \"[a-z]+\"; } \
                  fn main() -> Int { return 0; }";
        assert!(check_src(ok).is_ok());
        // A non-literal pattern is rejected.
        let dyn_pat = "fn f(s: String, p: String) -> Bool { return s =~ p; } \
                       fn main() -> Int { return 0; }";
        assert!(check_src(dyn_pat).unwrap_err().contains("string-literal pattern"));
        // An invalid regex is rejected at compile time (`{..}` is unsupported).
        let bad = "fn f(s: String) -> Bool { return s =~ \"a{2,3}\"; } \
                   fn main() -> Int { return 0; }";
        assert!(check_src(bad).unwrap_err().contains("invalid regex"));
    }
}
