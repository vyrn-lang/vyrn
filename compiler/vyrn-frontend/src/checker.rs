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
    let (out, let_types, _) = check_accum_full(program);
    (out, let_types)
}

/// The stored-function-value facts (RFC-0037) the `--workers` gate needs:
/// runs the checker and returns its defunctionalization collection. Diagnostics
/// are discarded — callers gate on a prior successful check.
pub fn stored_fn_effects(program: &Program) -> StoredFnEffects {
    check_accum_full(program).2
}

/// The full checking pass: diagnostics, the inferred-`let`-type table, and the
/// RFC-0037 stored-function-value collection.
fn check_accum_full(
    program: &Program,
) -> (
    Vec<Diagnostic>,
    HashMap<(usize, String), Type>,
    StoredFnEffects,
) {
    let mut out = Vec::new();

    // 1. Collect and validate type declarations.
    let mut types: HashMap<String, TypeDecl> = HashMap::new();
    for t in &program.type_decls {
        if matches!(t.name.as_str(), "Int64" | "Bool" | "Unit") {
            let mut d = Diagnostic::from_rendered(
                format!(
                    "line {}: cannot redefine built-in type `{}`",
                    t.line, t.name
                ),
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
        "print",
        "len",
        "concat",
        "Some",
        "None",
        "Ok",
        "Err",
        "match",
        "cell",
        "get",
        "set",
        "release",
        "array",
        "push",
        "at",
        "alen",
        "afree",
        "str",
        "parse",
        "join",
        "logger",
        "contains",
        "startsWith",
        "endsWith",
        "slice",
        "bytes",
        "chars",
        "hexEncode",
        "hexDecode",
        "base64Encode",
        "base64Decode",
        "urlEncode",
        "urlDecode",
        "args",
        "readLine",
        "readFile",
        "writeFile",
        "renameFile",
        "fsyncFile",
        "readFileBytes",
        "stringFromBytes",
        "listDir",
        "moduleInterface",
        "trace",
        "debug",
        "info",
        "warn",
        "error",
        "value",
        "list",
        "schemaOf",
        "jsonSchema",
        "toJson",
        "fromJson",
        "toString",
        "pop",
        "swapRemove",
        "assert",
        "assertEq",
        "blackBox",
        "Int",
        "Int64",
        "Int32",
        "Int16",
        "Int8",
        "Float",
        "Float64",
        "Float32",
        "UInt8",
        "UInt16",
        "UInt32",
        "UInt64",
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
                        format!(
                            "line {}: enum variant `{}` is defined twice",
                            t.line, v.name
                        ),
                        "check",
                    ));
                    continue;
                }
                if types.contains_key(&v.name) {
                    out.push(Diagnostic::from_rendered(
                        format!(
                            "line {}: enum variant `{}` clashes with a type name",
                            t.line, v.name
                        ),
                        "check",
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
            out.push(Diagnostic::from_rendered(
                format!("line {}: `{}` is a reserved name", f.line, f.name),
                "check",
            ));
            continue;
        }
        if variants.contains_key(&f.name) {
            out.push(Diagnostic::from_rendered(
                format!(
                    "line {}: `{}` is both a function and an enum variant",
                    f.line, f.name
                ),
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
                format!(
                    "line {}: `{}` is both a type and a function name",
                    f.line, f.name
                ),
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
    let all_bounds: HashMap<String, HashMap<String, Vec<String>>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.type_bounds.clone()))
        .collect();
    // Each function's parameter capabilities, for checking `modify` call sites.
    let caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
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

    // `extern` / `gen fn` name sets (RFC-0037: neither may become a stored
    // function value — the checker names the restriction at the use site).
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
        in_bench: RefCell::new(false),
        in_gen: RefCell::new(false),
        extern_fns: &extern_fns,
        gen_fns: &gen_fns,
        cur_fn: RefCell::new(String::new()),
        stored_sources: RefCell::new(Vec::new()),
        stored_calls: RefCell::new(Vec::new()),
        spawn_sites: RefCell::new(Vec::new()),
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
        || !program.benches.is_empty()
        || has_served_handle;
    match sigs.get("main") {
        None if !is_library => out.push(Diagnostic::from_rendered(
            "no `main` function found".to_string(),
            "check",
        )),
        None => {}
        Some(main) if !main.0.is_empty() || main.1 != Type::Int => {
            out.push(Diagnostic::from_rendered(
                "`main` must have signature `fn main() -> Int64`".to_string(),
                "check",
            ))
        }
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
        // Signature validation (params/return) runs outside `function()`, so make
        // it gen-aware here too — a `Code` type in a `gen fn` signature is legal.
        *checker.in_gen.borrow_mut() = f.is_gen;
        let r = (|| -> Result<(), String> {
            for p in &f.params {
                // A function-value type is legal only on an ordinary function
                // (RFC-0023/0037): never on an `extern` import/export, whose ABI
                // domain crosses the host boundary (closures do not cross wasm),
                // and never on a `gen fn` (generation-time signatures are wire-ish).
                if checker.contains_fn(&p.ty) && (f.is_extern || f.is_export_extern) {
                    return Err(format!(
                        "line {}: an `extern` function may not take a `fn`-typed \
                         parameter (RFC-0023)",
                        f.line
                    ));
                }
                if checker.contains_fn(&p.ty) && f.is_gen {
                    return Err(format!(
                        "line {}: a `gen fn` may not take a `fn`-typed parameter in v1 \
                         (RFC-0023)",
                        f.line
                    ));
                }
                checker.ensure_param_type(&p.ty, f.line)?;
            }
            if checker.contains_fn(&f.ret) && (f.is_extern || f.is_export_extern) {
                return Err(format!(
                    "line {}: an `extern` function may not return a function value \
                     (RFC-0037 — closures do not cross the host boundary)",
                    f.line
                ));
            }
            if checker.contains_fn(&f.ret) && f.is_gen {
                return Err(format!(
                    "line {}: a `gen fn` may not return a function value (RFC-0037)",
                    f.line
                ));
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

    // 6b. Check bench bodies (RFC-0055). Identical treatment to tests: each is a
    //     Unit-returning function body under a synthetic `bench@<index>` name, with
    //     `in_bench` set so `blackBox` is legal. Benches are never registered in
    //     `sigs`, so user code cannot call one; duplicate names per file are caught.
    check_benches(&checker, program, &mut out);

    // 7. Comptime-purity (RFC-0021): every `gen fn` and its transitive callees
    //    must be pure enough to run in the compiler's interpreter at generation
    //    time. Reported after the ordinary body checks so a broken generator's
    //    type errors surface first.
    check_comptime_purity(program, &mut out);

    // 8. RFC-0037: re-verify every accepted `spawn` site against the
    //    stored-closure-EXTENDED spawn-safety fixpoint. The pre-check fixpoint
    //    cannot see calls through stored function values (their callee set is
    //    the signature's collected sources), so a function whose only impurity
    //    flows through a stored value passed the inline check; catch it here.
    let effects = StoredFnEffects {
        sources: checker.stored_sources.borrow().clone(),
        calls: checker.stored_calls.borrow().clone(),
    };
    if !effects.calls.is_empty() {
        let ext = extend_spawn_safe(program, &spawn_safe, &effects);
        for (caller, callee, line) in checker.spawn_sites.borrow().iter() {
            if !ext.contains(callee.as_str()) {
                let mut d = Diagnostic::from_rendered(
                    format!(
                        "line {line}: `spawn {callee}(..)` is not allowed: `{callee}` \
                         (or something it calls) invokes a stored function value \
                         (RFC-0037) whose possible targets do I/O or touch shared \
                         mutable state, so running it as a task could race. A \
                         spawned function must be isolated (pure)."
                    ),
                    "check",
                );
                d.file = program
                    .functions
                    .iter()
                    .find(|f| &f.name == caller)
                    .and_then(|f| f.module.clone());
                out.push(d);
            }
        }
    }

    let let_types = checker.let_types.borrow().clone();
    (out, let_types, effects)
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

/// Check every `bench` body (RFC-0055). Structurally identical to [`check_tests`]:
/// duplicate names per module are reported; each body is checked as a Unit-
/// returning function under a synthetic `bench@<index>` name with `in_bench` set so
/// `blackBox` is legal.
fn check_benches(checker: &Checker, program: &Program, out: &mut Vec<Diagnostic>) {
    let mut seen: HashMap<(Option<String>, String), usize> = HashMap::new();
    for b in &program.benches {
        let key = (b.module.clone(), b.name.clone());
        if let Some(prev) = seen.get(&key) {
            let mut d = Diagnostic::from_rendered(
                format!(
                    "line {}: duplicate bench name {:?} (already declared on line {prev})",
                    b.line, b.name
                ),
                "check",
            );
            d.file = b.module.clone();
            out.push(d);
        } else {
            seen.insert(key, b.line);
        }
    }
    *checker.in_bench.borrow_mut() = true;
    for (i, b) in program.benches.iter().enumerate() {
        let synthetic = Function {
            name: format!("bench@{i}"),
            exported: false,
            module: b.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: b.body.clone(),
            line: b.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        };
        if let Err(s) = checker.function(&synthetic) {
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = b.module.clone();
            out.push(d);
        }
        for s in checker.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "check");
            d.file = b.module.clone();
            out.push(d);
        }
    }
    *checker.in_bench.borrow_mut() = false;
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
    /// True while checking a `bench` body (RFC-0055). `blackBox` is legal only
    /// inside a `test` or `bench` body (`in_test || in_bench`); `assert`/`assertEq`
    /// stay `test`-only. Set for the duration of [`check_benches`].
    in_bench: RefCell<bool>,
    /// True while checking a `gen fn` body (RFC-0021/0054). The `Code` type and the
    /// code-quote builtins (`vyrn"…"`, `render`, `rawAt`, `raw`, `lex`) are legal
    /// only when this is set — using them outside generation is a compile error,
    /// which is also what keeps them out of any backend (gen fn bodies are never
    /// emitted).
    in_gen: RefCell<bool>,
    /// Names of `extern` (host-provided) functions — not usable as function
    /// values (RFC-0037: closures do not cross the host boundary).
    extern_fns: &'a std::collections::HashSet<String>,
    /// Names of `gen fn`s — not usable as function values (RFC-0037).
    gen_fns: &'a std::collections::HashSet<String>,
    /// The function whose body is currently being checked (RFC-0037 collection).
    cur_fn: RefCell<String>,
    /// RFC-0037 defunctionalization sources collected during checking: every
    /// lambda literal or named function that flows into a stored fn value.
    stored_sources: RefCell<Vec<StoredSource>>,
    /// RFC-0037: each call through a stored (non-parameter) fn-typed binding,
    /// as (enclosing function, signature).
    stored_calls: RefCell<Vec<(String, Type)>>,
    /// `spawn` sites that passed the pre-check spawn-safety test, re-verified
    /// after checking against the stored-closure-extended fixpoint (RFC-0037):
    /// (caller, callee, line).
    spawn_sites: RefCell<Vec<(String, String, usize)>>,
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
        self.types
            .get(enum_name)
            .map(|d| d.type_params.clone())
            .unwrap_or_default()
    }

    /// Whether `ty` transitively contains a function-value type (RFC-0037),
    /// resolving named types (cycle-safe). Used to keep function values out of
    /// the positions that stay illegal: `extern`/`gen` signatures, `Ref`/`Task`
    /// payloads, and nested function signatures.
    fn contains_fn(&self, ty: &Type) -> bool {
        fn walk(ty: &Type, types: &HashMap<String, TypeDecl>, seen: &mut Vec<String>) -> bool {
            match ty {
                Type::Fn(..) => true,
                Type::Option(i)
                | Type::Array(i)
                | Type::ArrayN(i, _)
                | Type::SmallArray(i, _)
                | Type::Ref(i)
                | Type::Task(i)
                | Type::Partial(i) => walk(i, types, seen),
                Type::Result(a, b) | Type::Map(a, b) | Type::Merge(a, b) => {
                    walk(a, types, seen) || walk(b, types, seen)
                }
                Type::Omit(b, _) | Type::Pick(b, _) => walk(b, types, seen),
                Type::Record(fs) => fs.iter().any(|f| walk(&f.ty, types, seen)),
                Type::Enum(vs) => vs
                    .iter()
                    .any(|v| v.payload.iter().any(|p| walk(p, types, seen))),
                Type::App(n, args) => {
                    args.iter().any(|a| walk(a, types, seen))
                        || (!seen.iter().any(|s| s == n)
                            && types.get(n).is_some_and(|d| {
                                seen.push(n.clone());
                                let r = walk(&d.base, types, seen);
                                seen.pop();
                                r
                            }))
                }
                Type::Named(n) => {
                    !seen.iter().any(|s| s == n)
                        && types.get(n).is_some_and(|d| {
                            seen.push(n.clone());
                            let r = walk(&d.base, types, seen);
                            seen.pop();
                            r
                        })
                }
                _ => false,
            }
        }
        walk(ty, self.types, &mut Vec::new())
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
        // A transparent alias to `Result`/`Option` (RFC-0024, e.g. `type
        // DeleteResult = Result<Bool, String>`) is interchangeable with its
        // resolved form — it carries no `where` obligation of its own.
        let transparent = |b: &Type| {
            matches!(
                b,
                Type::Result(..)
                    | Type::Option(..)
                    | Type::Map(..)
                    | Type::Array(_)
                    | Type::ArrayN(..)
                    // A named function type (`type Middleware = fn(..) -> ..`,
                    // RFC-0037) is interchangeable with its structural form.
                    | Type::Fn(..)
            )
        };
        if let Type::Named(n) = to {
            if let Some(d) = self.types.get(n) {
                if d.predicate.is_none() && transparent(&d.base) {
                    return self.assignable(from, &d.base);
                }
            }
        }
        if let Type::Named(n) = from {
            if let Some(d) = self.types.get(n) {
                if d.predicate.is_none() && transparent(&d.base) {
                    return self.assignable(&d.base, to);
                }
            }
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
        // A Map is covariant in its value type (keys are always String; values
        // are immutable at a read boundary) — RFC-0028.
        if let (Type::Map(ka, va), Type::Map(kb, vb)) = (from, to) {
            return self.assignable(ka, kb) && self.assignable(va, vb);
        }
        if let (Type::Array(a), Type::Array(b)) = (from, to) {
            return self.assignable(a, b);
        }
        // A `SmallArray<T, N>` (RFC-0056) is covariant in `T` and invariant in
        // `N` (the capacity is part of the type — no widening/narrowing).
        if let (Type::SmallArray(a, na), Type::SmallArray(b, nb)) = (from, to) {
            return na == nb && self.assignable(a, b);
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

    /// RFC-0020 M1: prove a string **interpolation** (or a finite-string
    /// variable) flowing into a validated string type is contained in that
    /// type's language. Runs alongside [`Self::prove_coercion`] at every value
    /// boundary. An interpolation that can produce a value outside `to`'s
    /// language is a hard compile error carrying the shortest witness; a proven
    /// flow silently lets the backends skip the runtime check; anything else
    /// (a non-finite hole, a non-regex target) leaves runtime validation in
    /// place. `scope`/`fn_ret` are used to infer hole types.
    fn prove_string_interpolation(
        &self,
        expr: &Expr,
        to: &Type,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
        line: usize,
    ) -> Result<(), String> {
        let resolve = |e: &Expr| self.expr(e, scope, None, fn_ret).ok();
        match crate::finite::prove_string_flow(expr, to, self.types, &resolve) {
            crate::finite::Proof::Witness(witness) => {
                // `to` is a named predicated type here (Proof::Witness only
                // arises when the target resolved to one).
                let decl = match to {
                    Type::Named(n) => self.types.get(n).unwrap(),
                    _ => unreachable!("witness implies a named target"),
                };
                let pred = decl.predicate.as_ref().unwrap();
                Err(format!(
                    "line {line}: \"{witness}\" (a possible value of this interpolation) \
                     does not satisfy `{}` (predicate `where {}` is false)",
                    decl.name,
                    pred_summary(pred),
                ))
            }
            crate::finite::Proof::Proven | crate::finite::Proof::NotApplicable => Ok(()),
        }
    }

    fn ensure_type_exists(&self, ty: &Type, line: usize) -> Result<(), String> {
        match ty {
            // `Code` (RFC-0054) is a builtin opaque type, legal only in a
            // generation context — using it elsewhere is a compile error (which
            // also keeps it out of every backend: a `gen fn` body is never
            // emitted). It has no fields and no declaration.
            Type::Named(n) if n == "Code" && !self.types.contains_key("Code") => {
                if !*self.in_gen.borrow() {
                    return Err(format!(
                        "line {line}: the `Code` type is only available during generation"
                    ));
                }
                return Ok(());
            }
            // `Token` (RFC-0054) — the `lex()` record. Gen-only (its only source is
            // the gen-only `lex()`), so it never reaches a backend; a user
            // `type Token` wins and is validated normally.
            Type::Named(n) if n == "Token" && !self.types.contains_key("Token") => {
                if !*self.in_gen.borrow() {
                    return Err(format!(
                        "line {line}: the `Token` type is only available during generation"
                    ));
                }
                return Ok(());
            }
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
                // RFC-0056: only `SmallArray` consumes an integer type argument.
                // A user generic carrying one (`Box<3>`) is a checker error —
                // checked before arity so the diagnostic names the real problem.
                if args.iter().any(|a| matches!(a, Type::ConstInt(_))) {
                    return Err(format!(
                        "line {line}: type {name} does not take an integer argument"
                    ));
                }
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
            // Container types recurse so their element types are validated.
            Type::Array(inner) | Type::ArrayN(inner, _) => self.ensure_type_exists(inner, line)?,
            // `SmallArray<T, N>` (RFC-0056): the inline capacity is bounded
            // `1 <= N <= 64` (keeps the worst-case stack/inline footprint sane),
            // and the element type must be a real type (not a stray integer).
            Type::SmallArray(inner, n) => {
                if *n < 1 || *n > 64 {
                    return Err(format!(
                        "line {line}: smallArray capacity must be between 1 and 64"
                    ));
                }
                self.ensure_type_exists(inner, line)?;
            }
            // A bare integer type argument reached a position no constructor
            // consumes it (only `SmallArray<T, N>` takes one) — reject it.
            Type::ConstInt(_) => {
                return Err(format!(
                    "line {line}: an integer is not a type; only `SmallArray<T, N>` \
                     takes an integer argument"
                ))
            }
            // A `Ref`/`Task` cannot hold a function value (RFC-0037 defers it):
            // a cell would make the closure's capture snapshot mutable-by-alias,
            // and a task result slot has no dispatcher to receive one yet.
            Type::Ref(inner) | Type::Task(inner) => {
                if self.contains_fn(inner) {
                    return Err(format!(
                        "line {line}: a `Ref`/`Task` cannot hold a function value \
                         (RFC-0037 defers it)"
                    ));
                }
                self.ensure_type_exists(inner, line)?
            }
            // `Map<String, V>` (RFC-0028): keys are `String` in v1. A validated
            // string type is a legal key (it resolves to `String`); any other
            // key spelling is rejected here with the named diagnostic.
            Type::Map(key, val) => {
                self.ensure_type_exists(key, line)?;
                self.ensure_type_exists(val, line)?;
                if crate::types::resolve(key, self.types) != Type::Str {
                    return Err(format!(
                        "line {line}: a `Map` key must be `String` in v1, found `{key}` \
                         (RFC-0028; validated string types are allowed)"
                    ));
                }
            }
            // A generic parameter is always valid in the context the parser
            // produced it (it only tags names declared in `<...>`).
            Type::Param(_) => {}
            // A function-value type (RFC-0037): legal in storage positions —
            // `let` annotations, record fields, `Array`/`Map` values,
            // `Option`/`Result`, enum payloads, returns, and module state.
            // Its own parameter and return types must not themselves carry a
            // function type (no higher-order-of-higher-order), and the
            // still-illegal positions (`extern`/`gen` signatures, `Ref`/`Task`,
            // codec/schema) are rejected at their own sites with named
            // diagnostics.
            Type::Fn(ptys, ret) => {
                for p in ptys {
                    if self.contains_fn(p) {
                        return Err(format!(
                            "line {line}: a function type may not take another \
                             function value (RFC-0037 defers higher-order function \
                             types)"
                        ));
                    }
                    self.ensure_type_exists(p, line)?;
                }
                if self.contains_fn(ret) {
                    return Err(format!(
                        "line {line}: a function type may not return another \
                         function value (RFC-0037 defers higher-order function \
                         types)"
                    ));
                }
                self.ensure_type_exists(ret, line)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Validate a function parameter's type (RFC-0023). A top-level `fn(..) -> R`
    /// is the one legal function-type position; its own parameter and return types
    /// must not themselves be function types (no higher-order-of-higher-order in
    /// v1). Every other parameter type is validated normally.
    fn ensure_param_type(&self, ty: &Type, line: usize) -> Result<(), String> {
        match ty {
            Type::Fn(ptys, ret) => {
                for p in ptys {
                    if self.contains_fn(p) {
                        return Err(format!(
                            "line {line}: a `fn`-typed parameter may not itself take a \
                             function value in v1 (RFC-0023)"
                        ));
                    }
                    self.ensure_type_exists(p, line)?;
                }
                if self.contains_fn(ret) {
                    return Err(format!(
                        "line {line}: a `fn`-typed parameter may not return a function \
                         value in v1 (RFC-0023)"
                    ));
                }
                self.ensure_type_exists(ret, line)?;
                Ok(())
            }
            _ => self.ensure_type_exists(ty, line),
        }
    }

    fn check_type_decl(&self, t: &TypeDecl) -> Result<(), String> {
        // A function type (RFC-0037) has no value domain a `where` predicate
        // could constrain — a named fn type is a transparent alias only.
        if t.predicate.is_some() && self.contains_fn(&t.base) {
            return Err(format!(
                "line {}: a function type cannot carry a `where` predicate (RFC-0037)",
                t.line
            ));
        }
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
                    scope[0].insert(
                        f.name.clone(),
                        Binding {
                            ty: f.ty.clone(),
                            mutable: false,
                        },
                    );
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
                return Err(format!(
                    "line {}: an enum type cannot have a `where` clause",
                    t.line
                ));
            }
            if vs.is_empty() {
                return Err(format!(
                    "line {}: enum `{}` has no variants",
                    t.line, t.name
                ));
            }
            for v in vs {
                for p in &v.payload {
                    self.ensure_type_exists(p, t.line)?;
                }
            }
            return Ok(());
        }
        // A transparent alias to a built-in generic wrapper: `type DeleteResult =
        // Result<Bool, String>` / `type Maybe = Option<Int64>`. Allowed so a
        // codable `Result`/`Option` can be named and handed to `fromJson`/
        // `jsonSchema` by name (RFC-0024's RPC ripple). No `where` clause (its
        // payloads carry their own refinements); the payload types must exist.
        if matches!(t.base, Type::Result(..) | Type::Option(..)) {
            if t.predicate.is_some() {
                return Err(format!(
                    "line {}: a `{}` alias cannot have a `where` clause",
                    t.line,
                    if matches!(t.base, Type::Result(..)) {
                        "Result"
                    } else {
                        "Option"
                    }
                ));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            return Ok(());
        }
        // A transparent alias to a `Map`/`Array` (RFC-0028/RFC-0011), so a codable
        // collection can be named and handed to `fromJson`/`jsonSchema` by name
        // (the same rationale as the `Result`/`Option` aliases above). No `where`
        // clause; the element/value types must exist.
        if matches!(t.base, Type::Map(..) | Type::Array(_) | Type::ArrayN(..)) {
            if t.predicate.is_some() {
                return Err(format!(
                    "line {}: a `{}` alias cannot have a `where` clause",
                    t.line,
                    if matches!(t.base, Type::Map(..)) {
                        "Map"
                    } else {
                        "Array"
                    }
                ));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            return Ok(());
        }
        // A transparent alias to a function type (RFC-0037): `type Middleware =
        // fn(Request) -> Option<Response>`. Interchangeable with its structural
        // form (see `assignable`); the `where` rejection happened above.
        if matches!(t.base, Type::Fn(..)) {
            self.ensure_type_exists(&t.base, t.line)?;
            return Ok(());
        }
        // A transformer alias, e.g. `type Public = Omit<User, password>;`.
        if matches!(
            t.base,
            Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..)
        ) {
            if t.predicate.is_some() {
                return Err(format!(
                    "line {}: a record type cannot have a `where` clause",
                    t.line
                ));
            }
            self.ensure_type_exists(&t.base, t.line)?;
            if crate::types::record_fields(&t.base, self.types).is_none() {
                return Err(format!(
                    "line {}: `{}` does not resolve to a record",
                    t.line, t.name
                ));
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
            scope[0].insert(
                "value".into(),
                Binding {
                    ty: t.base.clone(),
                    mutable: false,
                },
            );
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
        // Names whose call is forbidden in ANY initializer: every `extern`
        // function (a host call — unavailable before `main`) and every protocol
        // method. Builtins (`print`, `cell`, `str`, …), constructors (`Some`,
        // enum variants, `Age(n)`) are not in this set.
        //
        // An ORDINARY (non-extern) function's callability depends on WHERE it
        // lives (RFC-0029): a function imported from another module is legal to
        // call, because imported modules initialize first (post-order over the
        // import graph) so their state is ready; a SAME-MODULE function stays
        // forbidden (no own user code runs before `main`). `fn_module` maps each
        // ordinary function to its owning module for that comparison.
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
        // Ready-so-far names (the earlier globals) grow as we go.
        let mut ready: HashSet<String> = HashSet::new();
        for g in &program.globals {
            let bty = (|| -> Result<Type, String> {
                // Initializer restrictions (walked before typing so the messages
                // are precise): no user/extern call, no later-global read.
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
                // Type-check the initializer against the annotation, seeing only
                // the earlier globals.
                let scope: Vec<HashMap<String, Binding>> = vec![self.globals.borrow().clone()];
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
                Ok(t) => Binding {
                    ty: t,
                    mutable: g.mutable,
                },
                Err(s) => {
                    let mut d = Diagnostic::from_rendered(s, "check");
                    d.file = g.module.clone();
                    out.push(d);
                    Binding {
                        ty: Type::Err,
                        mutable: g.mutable,
                    }
                }
            };
            self.globals.borrow_mut().insert(g.name.clone(), binding);
            ready.insert(g.name.clone());
        }
    }

    fn function(&self, f: &Function) -> Result<(), String> {
        *self.cur_bounds.borrow_mut() = f.type_bounds.clone();
        *self.cur_fn.borrow_mut() = f.name.clone();
        *self.in_gen.borrow_mut() = f.is_gen;
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
            scope.last_mut().unwrap().insert(
                p.name.clone(),
                Binding {
                    ty: p.ty.clone(),
                    mutable,
                },
            );
        }
        // `block` no longer propagates the first error via `?`; it pushes each
        // statement's error to the `errors` sink and continues, so every
        // statement-level error in the body is reported.
        let returns = self.block(&f.body, &f.ret, &mut scope);
        if f.ret != Type::Unit && !returns {
            // A missing-return diagnostic is reported alongside any body errors
            // (it is about the function as a whole, not one statement).
            self.errors.borrow_mut().push(format!(
                "line {}: function `{}` must return {} on all paths",
                f.line, f.name, f.ret
            ));
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

    fn block(&self, block: &Block, ret: &Type, scope: &mut Vec<HashMap<String, Binding>>) -> bool {
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
                scope.last_mut().unwrap().insert(
                    name.clone(),
                    Binding {
                        ty: Type::Err,
                        mutable: *mutable,
                    },
                );
            }
            Stmt::ForIn { var, .. } => {
                // The loop variable's frame is pushed inside `stmt`'s `ForIn`
                // arm; on error that arm returned before pushing it, so bind in
                // the current (block) frame as a best-effort recovery.
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

    fn stmt(
        &self,
        stmt: &Stmt,
        ret: &Type,
        scope: &mut Vec<HashMap<String, Binding>>,
    ) -> Result<bool, String> {
        match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                value,
                line,
            } => {
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
                    self.prove_string_interpolation(value, declared, scope, Some(ret), *line)?;
                }
                if self.base(&vty) == Type::Unit {
                    return Err(format!("line {line}: cannot bind `{name}` to a Unit value"));
                }
                // The binding takes the declared type when present, else the value's.
                let bty = ty.clone().unwrap_or(vty);
                // Retain it for the symbol-query layer so hovering an
                // unannotated `let x = 5` shows `let x: Int`.
                self.let_types
                    .borrow_mut()
                    .insert((*line, name.clone()), bty.clone());
                scope.last_mut().unwrap().insert(
                    name.clone(),
                    Binding {
                        ty: bty,
                        mutable: *mutable,
                    },
                );
                Ok(false)
            }
            Stmt::Assign { name, value, line } => {
                let b = self.lookup(scope, name).ok_or_else(|| {
                    format!("line {line}: assignment to unknown variable `{name}`")
                })?;
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
                self.prove_string_interpolation(value, &b.ty, scope, Some(ret), *line)?;
                self.region_store_guard(name, &b.ty, scope, *line)?;
                Ok(false)
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
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
                    .ok_or_else(|| {
                        format!("line {line}: record `{name}` has no field `{field}`")
                    })?;
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
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                let b = self.lookup(scope, name).ok_or_else(|| {
                    format!("line {line}: index-assignment to unknown variable `{name}`")
                })?;
                if !b.mutable {
                    return Err(format!(
                        "line {line}: cannot store into `{name}` (declared without `mut`)"
                    ));
                }
                // `m[k] = v` on a Map (RFC-0028) inserts or updates in place: the
                // key coerces to `String`, the value to `V` (auto-validated when
                // `V` is predicated, exactly like an array element store).
                if let Type::Map(_, val) = self.base(&b.ty) {
                    let k = self.base(&self.expr(index, scope, Some(&Type::Str), Some(ret))?);
                    if !matches!(k, Type::Err) && crate::types::resolve(&k, self.types) != Type::Str
                    {
                        return Err(format!(
                            "line {line}: a map key must be a String, found {k}"
                        ));
                    }
                    let vty = self.expr(value, scope, Some(&val), Some(ret))?;
                    if !self.coercible(&vty, &val) {
                        return Err(format!(
                            "line {line}: `{name}` holds values of type {val} but the stored \
                             value is {vty}"
                        ));
                    }
                    self.prove_coercion(value, &val, *line)?;
                    self.prove_string_interpolation(value, &val, scope, Some(ret), *line)?;
                    self.region_store_guard(name, &val, scope, *line)?;
                    return Ok(false);
                }
                let elem = match self.base(&b.ty) {
                    Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                        (*inner).clone()
                    }
                    Type::Err => return Ok(false),
                    other => {
                        return Err(format!(
                            "line {line}: `{name}[i] = ..` needs an Array or Map, found {other}"
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
                self.prove_string_interpolation(value, &elem, scope, Some(ret), *line)?;
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
                        if let Err(msg) =
                            self.prove_string_interpolation(e, ret, scope, Some(ret), *line)
                        {
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
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
            } => {
                let cty = self.expr(cond, scope, None, Some(ret))?;
                if self.base(&cty) != Type::Bool {
                    return Err(format!(
                        "line {line}: `if` condition must be Bool, found {cty}"
                    ));
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
                    return Err(format!(
                        "line {line}: `while` condition must be Bool, found {cty}"
                    ));
                }
                self.block(body, ret, scope);
                Ok(false)
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
            } => {
                let ity = self.expr(iter, scope, None, Some(ret))?;
                let elem = match self.base(&ity) {
                    Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                        (*inner).clone()
                    }
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
                self.let_types
                    .borrow_mut()
                    .insert((*line, var.clone()), elem.clone());
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
                    Type::Str | Type::Array(_) | Type::SmallArray(..) | Type::Ref(_)
                    | Type::Map(..) => Ok(false),
                    other => Err(format!(
                        "line {line}: `drop` needs a heap value (String, Array, Map, or Ref), \
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
            Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                self.contains_heap(&inner)
            }
            // A Map's buffers are malloc'd; its keys are always heap (String) and
            // its values may be — either way it carries heap (RFC-0028).
            Type::Map(..) => true,
            Type::Ref(inner) | Type::Task(inner) => self.contains_heap(&inner),
            Type::Record(fs) => fs.iter().any(|f| self.contains_heap(&f.ty)),
            Type::Enum(vs) => vs
                .iter()
                .any(|v| v.payload.iter().any(|p| self.contains_heap(p))),
            Type::Option(inner) => self.contains_heap(&inner),
            Type::Result(a, b) => self.contains_heap(&a) || self.contains_heap(&b),
            // A stored function value (RFC-0037) may hold heap captures
            // (a snapshotted String/Array/record), so treat it as heap-carrying.
            Type::Fn(..) => true,
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
        // RFC-0037: an expected function type makes this a STORED-function-value
        // position (a `let` annotation, record field, array/map element,
        // `Option`/`Result` payload, return, assignment, or module state). A
        // lambda literal or a bare function name is accepted here and recorded
        // as a defunctionalization source; an fn-typed binding (composition
        // `let g = h`) falls through to the ordinary paths below.
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
                if let Some(b) = self.lookup(scope, name) {
                    return Ok(b.ty);
                }
                // RFC-0037: a bare (non-generic, non-extern, non-gen) function
                // name in a value position is a stored-function-value source —
                // `let g = double` infers `fn(..) -> ..` from its signature.
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
                Err(format!("line {line}: unknown variable `{name}`"))
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
                    UnOp::Neg
                        if matches!(
                            t,
                            Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
                        ) =>
                    {
                        Ok(t)
                    }
                    UnOp::Not if t == Type::Bool => Ok(Type::Bool),
                    // `~x` complements within the operand's integer width
                    // (RFC-0045): a sized integer, or the literal `Int`. Not
                    // Bool (use `!`), not a float.
                    UnOp::BitNot if matches!(t, Type::Int | Type::IntN { .. }) => Ok(t),
                    UnOp::Neg => Err(format!(
                        "line {line}: unary `-` needs a numeric type, found {t}"
                    )),
                    UnOp::Not => Err(format!("line {line}: unary `!` needs Bool, found {t}")),
                    UnOp::BitNot => Err(format!(
                        "line {line}: unary `~` needs an integer type, found {t}"
                    )),
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
                        _ => return Err(format!(
                            "line {line}: the right side of `=~` must be a string-literal pattern"
                        )),
                    }
                }
                // A shift by a COMPILE-TIME-CONSTANT amount out of range is a
                // compile error, not a runtime trap (RFC-0045): the width comes
                // from the (already literal-adapted) shifted operand's type.
                if matches!(op, BinOp::Shl | BinOp::Shr) {
                    let bits: i64 = match &l {
                        Type::IntN { bits, .. } => (*bits).into(),
                        _ => 64, // the literal `Int` shifts at 64 bits
                    };
                    if let Some(crate::consteval::ConstVal::Int(amt)) =
                        crate::consteval::eval(rhs, &std::collections::HashMap::new())
                    {
                        if amt < 0 || amt >= bits {
                            return Err(format!(
                                "line {line}: shift amount {amt} is out of range for a \
                                 {bits}-bit value (valid range is 0..{bits})"
                            ));
                        }
                    }
                }
                self.binop_type(*op, l, r, *line)
            }
            Expr::Call { name, args, line } => {
                self.call(name, args, *line, scope, expected, fn_ret)
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
            } => self.check_match(scrutinee, arms, *line, scope, expected, fn_ret),
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
                    Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..)
                        if field == "length" =>
                    {
                        Ok(Type::Int)
                    }
                    // `map.length` is the entry count (RFC-0028).
                    Type::Map(..) if field == "length" => Ok(Type::Int),
                    // `str.length` is the byte length (matches `strlen`/`Str::len`).
                    Type::Str if field == "length" => Ok(Type::Int),
                    Type::Record(rfields) => rfields
                        .iter()
                        .find(|f| &f.name == field)
                        .map(|f| f.ty.clone())
                        .ok_or_else(|| format!("line {line}: type {ety} has no field `{field}`")),
                    other => Err(format!(
                        "line {line}: cannot access field `{field}` on non-record type {other}"
                    )),
                }
            }
            Expr::TryConstruct { name, args, line } => {
                let base = match self.types.get(name) {
                    Some(d) if matches!(d.base, Type::Int | Type::Bool | Type::Str) => {
                        d.base.clone()
                    }
                    Some(_) => {
                        return Err(format!(
                            "line {line}: `{name}?(..)` is only for validated/nominal scalar types"
                        ))
                    }
                    None => return Err(format!("line {line}: unknown type `{name}`")),
                };
                if args.len() != 1 {
                    return Err(format!(
                        "line {line}: `{name}?` takes 1 argument, got {}",
                        args.len()
                    ));
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
                // A spawned callee cannot take function-value parameters: its
                // per-callee thunk carries plain data only (RFC-0037 keeps the
                // v1 rejection, now with a named diagnostic).
                if params.iter().any(|p| self.contains_fn(p)) {
                    return Err(format!(
                        "line {line}: cannot `spawn {name}(..)`: a spawned function \
                         may not take function-value parameters (RFC-0037)"
                    ));
                }
                // The pre-check spawn-safety fixpoint cannot see calls through
                // stored function values (RFC-0037) — record the site and
                // re-verify it against the extended fixpoint after checking.
                self.spawn_sites.borrow_mut().push((
                    self.cur_fn.borrow().clone(),
                    name.clone(),
                    *line,
                ));
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
                        // An empty `[]` against a `SmallArray<T, N>` slot is the
                        // empty small-buffer array (RFC-0056), inline state.
                        Some(Type::SmallArray(t, n)) => {
                            Ok(Type::SmallArray(t.clone(), *n))
                        }
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
                // A `SmallArray<T, N>` slot (RFC-0056): the literal supplies the
                // inline elements. The capacity is known, so a literal LONGER
                // than `N` is a checker error (pushing past `N` at runtime spills
                // — that is the feature, but a literal cannot exceed it).
                let small_cap = match expected {
                    Some(Type::SmallArray(_, n)) => Some(*n),
                    _ => None,
                };
                if let Some(n) = small_cap {
                    if elems.len() > n {
                        return Err(format!(
                            "line {line}: this literal has {} elements but the slot is \
                             SmallArray<_, {n}>",
                            elems.len()
                        ));
                    }
                }
                let (elem_expected, growable) = match expected {
                    Some(Type::ArrayN(t, _)) => (Some((**t).clone()), false),
                    Some(Type::Array(t)) => (Some((**t).clone()), true),
                    Some(Type::SmallArray(t, _)) => (Some((**t).clone()), false),
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
                self.prove_string_interpolation(&elems[0], &elem_ty, scope, fn_ret, *line)?;
                for e in &elems[1..] {
                    let t = self.expr(e, scope, Some(&elem_ty), fn_ret)?;
                    if !self.coercible(&t, &elem_ty) {
                        return Err(format!(
                            "line {line}: array elements must share a type: expected {elem_ty}, found {t}"
                        ));
                    }
                    self.prove_coercion(e, &elem_ty, *line)?;
                    self.prove_string_interpolation(e, &elem_ty, scope, fn_ret, *line)?;
                }
                if let Some(n) = small_cap {
                    Ok(Type::SmallArray(Box::new(elem_ty), n))
                } else if growable {
                    Ok(Type::Array(Box::new(elem_ty)))
                } else {
                    Ok(Type::ArrayN(Box::new(elem_ty), elems.len()))
                }
            }
            // A map literal (RFC-0028): `[:]` (empty, contextual) or
            // `["k": v, ...]`. Keys coerce to `String`, values to the expected
            // map's value type (auto-validated when predicated).
            Expr::MapLit { entries, line } => {
                let (key_ty, val_expected) = match expected {
                    Some(Type::Map(k, v)) => (Some((**k).clone()), Some((**v).clone())),
                    _ => (None, None),
                };
                if entries.is_empty() {
                    return match (&key_ty, &val_expected) {
                        (Some(k), Some(v)) => {
                            Ok(Type::Map(Box::new(k.clone()), Box::new(v.clone())))
                        }
                        _ => Err(format!(
                            "line {line}: cannot infer the type of `[:]`; annotate it, \
                             e.g. `let m: Map<String, Int64> = [:];`"
                        )),
                    };
                }
                // The value type is the expected one, else inferred from the
                // first value. The key type is `String` (the expected key type
                // when present — a validated string type stays honest).
                let key_ty = key_ty.unwrap_or(Type::Str);
                let first_val = self.expr(&entries[0].1, scope, val_expected.as_ref(), fn_ret)?;
                let val_ty = val_expected.unwrap_or(first_val);
                for (k, v) in entries {
                    let kt = self.expr(k, scope, Some(&key_ty), fn_ret)?;
                    if crate::types::resolve(&self.base(&kt), self.types) != Type::Str {
                        return Err(format!(
                            "line {line}: a map key must be a String, found {kt}"
                        ));
                    }
                    self.prove_coercion(k, &key_ty, *line)?;
                    self.prove_string_interpolation(k, &key_ty, scope, fn_ret, *line)?;
                    let vt = self.expr(v, scope, Some(&val_ty), fn_ret)?;
                    if !self.coercible(&vt, &val_ty) {
                        return Err(format!(
                            "line {line}: map values must share a type: expected {val_ty}, \
                             found {vt}"
                        ));
                    }
                    self.prove_coercion(v, &val_ty, *line)?;
                    self.prove_string_interpolation(v, &val_ty, scope, fn_ret, *line)?;
                }
                Ok(Type::Map(Box::new(key_ty), Box::new(val_ty)))
            }
            // A lambda literal reaching the general expression checker is in a
            // position with no function type to adopt (RFC-0037 storage
            // positions supply one through `expected`) — in an
            // illegal position (RFC-0023): a valid one is intercepted inside
            // `call` where its `fn`-typed parameter supplies its types. Everywhere
            // else — `let` initializers, returns, operands, non-`fn` arguments — it
            // is rejected here.
            Expr::Lambda { line, .. } => Err(format!(
                "line {line}: a lambda `|..|` needs a function type from context: \
                 pass it to a `fn`-typed parameter, or give the binding a function \
                 type (e.g. `let f: fn(Int64) -> Int64 = |x| x * 2`) (RFC-0037)"
            )),
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
            self.prove_string_interpolation(value, &field.ty, scope, fn_ret, line)?;
        }
        // Every declared field must be provided.
        for f in &rfields {
            if !provided.contains(&f.name) {
                return Err(format!(
                    "line {line}: missing field `{}` for `{name}`",
                    f.name
                ));
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
        let args = decl
            .type_params
            .iter()
            .map(|tp| subst[tp].clone())
            .collect();
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
        let ret =
            fn_ret.ok_or_else(|| format!("line {line}: `?` can only be used inside a function"))?;
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
            other => Err(format!(
                "line {line}: `?` needs an Option or Result, found {other}"
            )),
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
        let raw_sty = self.expr(scrutinee, scope, None, fn_ret)?;
        // Resolve a transparent alias so `match` over `type X = Result<..>` (or an
        // `Option`/enum alias) dispatches on the underlying shape (RFC-0024).
        let sty = match &raw_sty {
            Type::Named(n) => match self.types.get(n) {
                Some(d)
                    if d.predicate.is_none()
                        && matches!(d.base, Type::Result(..) | Type::Option(..)) =>
                {
                    crate::types::resolve(&raw_sty, self.types)
                }
                _ => raw_sty.clone(),
            },
            _ => raw_sty.clone(),
        };
        // A user enum dispatches to its own (N-variant) checker.
        if let Type::Enum(evs) = self.base(&sty) {
            return self.check_match_enum(&sty, &evs, arms, line, scope, expected, fn_ret);
        }
        // The two patterns an Option/Result scrutinee requires.
        let want: [&str; 2] = match &sty {
            Type::Option(_) => ["Some", "None"],
            Type::Result(_, _) => ["Ok", "Err"],
            other => return Err(format!(
                "line {line}: `match` scrutinee must be an Option, Result, or enum, found {other}"
            )),
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
                inner_scope.last_mut().unwrap().insert(
                    name.to_string(),
                    Binding {
                        ty: bty,
                        mutable: false,
                    },
                );
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
                    inner.last_mut().unwrap().insert(
                        bname.clone(),
                        Binding {
                            ty: pty.clone(),
                            mutable: false,
                        },
                    );
                }
            }
            let bty = self.expr(&arm.body, &inner, result.as_ref(), fn_ret)?;
            self.unify_arm(&mut result, bty, line)?;
        }
        for v in evs {
            if !seen.contains(&v.name) {
                return Err(format!(
                    "line {line}: `match` is missing variant `{}`",
                    v.name
                ));
            }
        }
        result.ok_or_else(|| format!("line {line}: empty `match`"))
    }

    /// Check an `if` used as an expression (RFC-0030): a two-branch boolean
    /// `match` in disguise. `else` is mandatory (totality); the condition is
    /// `Bool`; the two branches unify through the exact match-arm machinery
    /// (`unify_arm`), so validated-type coercion at the use boundary is inherited
    /// unchanged. An `else if` chain arrives as a nested `IfExpr` in
    /// `else_branch`, recursing here.
    #[allow(clippy::too_many_arguments)]
    fn check_if_expr(
        &self,
        cond: &Expr,
        then_branch: &Expr,
        else_branch: Option<&Expr>,
        line: usize,
        scope: &Vec<HashMap<String, Binding>>,
        expected: Option<&Type>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        // Totality: an expression must yield a value on every path.
        let Some(else_branch) = else_branch else {
            return Err(format!(
                "line {line}: `if` used as an expression needs an `else` (every branch \
                 must yield a value)"
            ));
        };
        let cty = self.expr(cond, scope, Some(&Type::Bool), fn_ret)?;
        if self.base(&cty) != Type::Bool && !matches!(cty, Type::Err) {
            return Err(format!(
                "line {line}: `if` condition must be Bool, found {cty}"
            ));
        }
        // Branches unify exactly like match arms: the first branch is checked
        // against the expected type, the second against the accumulated result.
        let mut result: Option<Type> = expected.cloned();
        let tty = self.expr(then_branch, scope, result.as_ref(), fn_ret)?;
        self.unify_arm(&mut result, tty, line)?;
        let ety = self.expr(else_branch, scope, result.as_ref(), fn_ret)?;
        self.unify_arm(&mut result, ety, line)?;
        result.ok_or_else(|| format!("line {line}: empty `if` expression"))
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
                Add | Sub | Mul | Div | Rem => Err(format!(
                    "line {line}: `{t}` needs a `Num` bound for arithmetic"
                )),
                Lt | LtEq | Gt | GtEq => Err(format!(
                    "line {line}: `{t}` needs an `Ord` bound to compare"
                )),
                Eq | NotEq => Err(format!("line {line}: `{t}` needs an `Eq` bound")),
                And | Or => Err(format!("line {line}: `&&`/`||` need Bool operands")),
                Match => Err(format!(
                    "line {line}: `=~` needs a String operand, not `{t}`"
                )),
                // Bitwise ops (RFC-0045) need a concrete integer type; there is
                // no protocol bound that grants them on a type parameter.
                BitAnd | BitOr | BitXor | Shl | Shr => Err(format!(
                    "line {line}: bitwise operators need a concrete integer type, not `{t}`"
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
            // `Code + Code` concatenates fragments (RFC-0054), origins carried.
            // Gen-only, so this is only reachable inside a `gen fn` anyway.
            Add if l == code && r == code => Ok(code.clone()),
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
                    Err(format!(
                        "line {line}: `%` needs matching integer operands, found {l} and {r}"
                    ))
                }
            }
            // Ordering: matching numeric operands, or two Strings (byte-wise
            // lexicographic — byte order, NOT locale collation, consistent with
            // `s.length` and `bytes(s)` counting bytes; RFC-0022).
            Lt | LtEq | Gt | GtEq => {
                if l == r && (numeric(&l) || l == Type::Str) {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: comparison needs matching numeric or String operands, \
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
                    Err(format!(
                        "line {line}: `&&`/`||` needs Bool operands, found {l} and {r}"
                    ))
                }
            }
            // `=~` matches a String against a regex literal → Bool (the literal
            // requirement and pattern validity are checked at the `Expr::Binary`
            // site, which has the syntax).
            // Bitwise ops (RFC-0045): both operands the same integer type (a
            // sized integer, or the literal `Int`) — no implicit widening, the
            // same discipline `+` uses. A shift amount is the same integer type
            // as the shifted value. Result is that integer type. NOT Bool (use
            // `&&`/`||`/`!`), NOT a float.
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let integral = |t: &Type| matches!(t, Type::Int | Type::IntN { .. });
                if l == r && integral(&l) {
                    Ok(l)
                } else if integral(&l) && integral(&r) {
                    Err(format!(
                        "line {line}: bitwise operators need matching integer operands, \
                         found {l} and {r}"
                    ))
                } else {
                    Err(format!(
                        "line {line}: bitwise operators need integer operands, found {l} and {r}"
                    ))
                }
            }
            Match => {
                if l == Type::Str && r == Type::Str {
                    Ok(Type::Bool)
                } else {
                    Err(format!(
                        "line {line}: `=~` needs a String and a pattern, found {l} and {r}"
                    ))
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
        // Calling a function value (RFC-0023/0037): `f(x)` where `f` is a
        // binding of function type — a v1 `fn`-typed parameter, or any stored
        // fn-typed `let`/`for`-var/field-bound local/module state (RFC-0037,
        // through a named alias too). Checked against the value's
        // `fn(..) -> R` signature before the builtins, so a binding always
        // shadows a same-named builtin.
        if let Some(binding) = self.lookup(scope, name) {
            if let Type::Fn(ptys, ret) = self.base(&binding.ty) {
                if ptys.len() != args.len() {
                    return Err(format!(
                        "line {line}: `{name}` is a function value taking {} argument(s), got {}",
                        ptys.len(),
                        args.len()
                    ));
                }
                for (i, (arg, pty)) in args.iter().zip(&ptys).enumerate() {
                    let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
                    if !self.coercible(&aty, pty) {
                        return Err(format!(
                            "line {line}: `{name}` argument {} expects {pty}, found {aty}",
                            i + 1
                        ));
                    }
                    self.prove_coercion(arg, pty, line)?;
                }
                // RFC-0037 effect collection: a call through a STORED value (any
                // fn-typed binding that is not a v1 parameter — the params frame
                // is index 1, above the globals frame 0) is dispatched over the
                // signature's collected sources, so record (caller, signature)
                // for the extended spawn/workers fixpoint. v1 parameter calls
                // keep their caller-site attribution untouched.
                let frame = scope.iter().rposition(|f| f.contains_key(name));
                if frame != Some(1) {
                    self.stored_calls
                        .borrow_mut()
                        .push((self.cur_fn.borrow().clone(), self.base(&binding.ty)));
                }
                return Ok((*ret).clone());
            }
        }
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
                    Type::Int
                        | Type::Float
                        | Type::Float32
                        | Type::IntN { .. }
                        | Type::Bool
                        | Type::Str
                )
            };
            if a != b || !equatable(&a) {
                return Err(format!(
                    "line {line}: `assertEq` needs two equal, equatable values, found {a} and {b}"
                ));
            }
            return Ok(Type::Unit);
        }

        // Benchmark builtin (RFC-0055): `blackBox<T>(v: T) -> T` — identity with an
        // optimizer-opacity guarantee, so the work producing `v` can't be deleted
        // and its result can't be constant-folded. Legal ONLY inside a `bench` or a
        // `test` body (same steering rule/wording style as `assert`).
        if name == "blackBox" {
            if !*self.in_test.borrow() && !*self.in_bench.borrow() {
                return Err(format!(
                    "line {line}: `blackBox` is only available inside a `bench` or `test` block — \
                     it exists to defeat the optimizer while measuring, not for ordinary code"
                ));
            }
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `blackBox` takes 1 argument, got {}",
                    args.len()
                ));
            }
            // Identity: the argument's own type flows straight out (generic in T).
            let t = self.expr(&args[0], scope, expected, fn_ret)?;
            return Ok(t);
        }

        // built-in: print(Int|Bool) -> Unit
        if name == "print" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: print expects 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, None, fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(
                t,
                Type::Int
                    | Type::Float
                    | Type::Float32
                    | Type::IntN { .. }
                    | Type::Bool
                    | Type::Str
            ) {
                return Err(format!(
                    "line {line}: print needs a number, Bool, or String, found {t}"
                ));
            }
            return Ok(Type::Unit);
        }

        // built-in: logger(String) -> Logger (RFC-0008).
        if name == "logger" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `logger` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!(
                    "line {line}: `logger` needs a String name, found {t}"
                ));
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
                return Err(format!(
                    "line {line}: `{name}` message must be a String, found {m}"
                ));
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
                return Err(format!(
                    "line {line}: `args` takes no arguments, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `readFile` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!(
                    "line {line}: `readFile` needs a String path, found {t}"
                ));
            }
            return Ok(Type::Result(Box::new(Type::Str), Box::new(Type::Str)));
        }
        // `listDir(path) -> Result<Array<String>, String>` (RFC-0021 family): the
        // entry names directly under `path` (no `.`/`..`, unsorted-by-OS order the
        // interpreter sorts for determinism). At generation time it is mediated
        // through the loader's resolver and scoped to the generator's path args;
        // at runtime it lists the real filesystem. Canonical error `cannot list
        // \`p\``.
        if name == "listDir" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `listDir` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!(
                    "line {line}: `listDir` needs a String path, found {t}"
                ));
            }
            return Ok(Type::Result(
                Box::new(Type::Array(Box::new(Type::Str))),
                Box::new(Type::Str),
            ));
        }
        // `moduleInterface(path) -> ModuleInterface` (RFC-0021): generation-time
        // reflection over a module's exported surface. A runtime call traps; the
        // type is available everywhere so a `gen fn` type-checks against it.
        if name == "moduleInterface" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `moduleInterface` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!(
                    "line {line}: `moduleInterface` needs a String path, found {t}"
                ));
            }
            return Ok(Type::Named("ModuleInterface".to_string()));
        }
        // RFC-0054 code quotes. All are generation-only; a use outside a `gen fn`
        // is a compile error (mirroring `Code`), which also keeps them out of
        // every backend. `@codeText`/`@codeSplice` are the internal desugar of a
        // `vyrn"…"` literal (never written by hand); `render`/`rawAt`/`raw`/`lex`
        // are the surface builtins.
        // The surface builtins (`render`/`rawAt`/`raw`/`lex`) are common words, so
        // they are NOT reserved: a user function or binding of the same name wins
        // (resolved below), and the builtin only applies when nothing shadows it.
        // The `@`-prefixed desugar names are unspellable, so always intercepted.
        let is_surface_builtin = matches!(name, "render" | "rawAt" | "raw" | "lex")
            && self.sigs.get(name).is_none()
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
                return Err(format!(
                    "line {line}: {surface} only available during generation"
                ));
            }
            let code = || Type::Named("Code".to_string());
            match name {
                // Internal: a literal skeleton fragment -> Code.
                "@codeText" => {
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(code());
                }
                // Internal: splice a value into a hole -> Code. The value must be a
                // scalar (String/number/Bool) or already `Code`; a String is data
                // and can never become code (it is escaped or validated at
                // generation time by the hole's context).
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
                        return Err(format!(
                            "line {line}: cannot splice {t} into a code quote \
                             (expected String, number, Bool, or Code)"
                        ));
                    }
                    return Ok(code());
                }
                // `render(Code) -> String`.
                "render" => {
                    if args.len() != 1 {
                        return Err(format!(
                            "line {line}: `render` takes 1 argument, got {}",
                            args.len()
                        ));
                    }
                    let t = self.base(&self.expr(&args[0], scope, Some(&code()), fn_ret)?);
                    if !matches!(t, Type::Err) && t != code() {
                        return Err(format!("line {line}: `render` needs a Code value, found {t}"));
                    }
                    return Ok(Type::Str);
                }
                // `rawAt(text, path, line, col) -> Code` — spliced user text with an
                // origin, so `render` maps diagnostics inside it back (RFC-0033).
                "rawAt" => {
                    if args.len() != 4 {
                        return Err(format!(
                            "line {line}: `rawAt` takes 4 arguments (text, path, line, col), got {}",
                            args.len()
                        ));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?;
                    self.expr(&args[2], scope, Some(&Type::Int), fn_ret)?;
                    self.expr(&args[3], scope, Some(&Type::Int), fn_ret)?;
                    return Ok(code());
                }
                // `raw(text) -> Code` — origin-less verbatim splice (a migration
                // escape hatch; new code should use `vyrn"…"` quotes instead).
                "raw" => {
                    if args.len() != 1 {
                        return Err(format!(
                            "line {line}: `raw` takes 1 argument, got {}",
                            args.len()
                        ));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(code());
                }
                // `lex(source) -> Array<Token>` — the compiler's real lexer.
                "lex" => {
                    if args.len() != 1 {
                        return Err(format!(
                            "line {line}: `lex` takes 1 argument, got {}",
                            args.len()
                        ));
                    }
                    self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?;
                    return Ok(Type::Array(Box::new(Type::Named("Token".to_string()))));
                }
                _ => unreachable!(),
            }
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
                    return Err(format!(
                        "line {line}: `writeFile` needs String arguments, found {t}"
                    ));
                }
            }
            return Ok(Type::Result(Box::new(Type::Bool), Box::new(Type::Str)));
        }
        // RFC-0044: atomically move `from` over `to` (the host primitive behind
        // `writeAtomic`). Same error shape as `writeFile` — `Result<Bool, String>`
        // with canonical `@.io.*` wording.
        if name == "renameFile" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `renameFile` takes 2 arguments (from, to), got {}",
                    args.len()
                ));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, Some(&Type::Str), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!(
                        "line {line}: `renameFile` needs String arguments, found {t}"
                    ));
                }
            }
            return Ok(Type::Result(Box::new(Type::Bool), Box::new(Type::Str)));
        }
        // RFC-0044: flush a file's contents to stable storage (the optional
        // power-durability upgrade over `writeAtomic`'s crash-consistency).
        if name == "fsyncFile" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `fsyncFile` takes 1 argument (a path), got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!(
                    "line {line}: `fsyncFile` needs a String path, found {t}"
                ));
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
                return Err(format!(
                    "line {line}: `readFileBytes` needs a String path, found {t}"
                ));
            }
            return Ok(Type::Result(
                Box::new(Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }))),
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
            let want = Type::Array(Box::new(Type::IntN {
                bits: 8,
                signed: false,
            }));
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
                return Err(format!(
                    "line {line}: `{name}` takes 2 arguments, got {}",
                    args.len()
                ));
            }
            for a in args {
                let t = self.base(&self.expr(a, scope, Some(&Type::Str), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if t != Type::Str {
                    return Err(format!(
                        "line {line}: `{name}` needs String arguments, found {t}"
                    ));
                }
            }
            return Ok(Type::Bool);
        }

        // built-in: slice(s, start, end) -> String (RFC-0046). A byte-range
        // substring — the primitive `std/strings` builds on. `start`/`end` are
        // byte offsets (Int64). O(1) validated at runtime: a cut inside a
        // multi-byte UTF-8 character traps (`error: slice splits a UTF-8
        // character`), an out-of-range offset traps like an array OOB
        // (`error: slice index out of range`).
        if name == "slice" {
            if args.len() != 3 {
                return Err(format!(
                    "line {line}: `slice` takes 3 arguments (s, start, end), got {}",
                    args.len()
                ));
            }
            let s = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(s, Type::Err) {
                return Ok(Type::Err);
            }
            if s != Type::Str {
                return Err(format!("line {line}: `slice` needs a String, found {s}"));
            }
            for a in &args[1..] {
                let t = self.base(&self.expr(a, scope, Some(&Type::Int), fn_ret)?);
                if matches!(t, Type::Err) {
                    return Ok(Type::Err);
                }
                if !matches!(t, Type::Int) {
                    return Err(format!(
                        "line {line}: `slice` needs Int64 offsets, found {t}"
                    ));
                }
            }
            return Ok(Type::Str);
        }

        // Text encodings. Encoders: String -> String. Decoders: String ->
        // Option<String> (None on malformed input or a non-UTF-8 result).
        if matches!(name, "hexEncode" | "base64Encode" | "urlEncode") {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `{name}` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `{name}` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `{name}` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let t = self.base(&self.expr(&args[0], scope, Some(&Type::Str), fn_ret)?);
            if matches!(t, Type::Err) {
                return Ok(Type::Err);
            }
            if t != Type::Str {
                return Err(format!("line {line}: `{name}` needs a String, found {t}"));
            }
            let elem = if name == "bytes" {
                Type::IntN {
                    bits: 8,
                    signed: false,
                }
            } else {
                Type::Int
            };
            return Ok(Type::Array(Box::new(elem)));
        }

        // Internal string concat (`a + b` on Strings, and interpolation): the
        // `@concat` spelling is produced by the desugarer / the `+` lowering,
        // never by user source. Heap-allocated result.
        if name == "@concat" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `@concat` takes 2 arguments, got {}",
                    args.len()
                ));
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
                other => {
                    return Err(format!(
                        "line {line}: `.join()` needs a Task, found {other}"
                    ))
                }
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
                Type::Int
                    | Type::IntN { .. }
                    | Type::Float
                    | Type::Float32
                    | Type::Bool
                    | Type::Str
            ) {
                return Err(format!(
                    "line {line}: `toString` renders a number, Bool, or String, found {t}"
                ));
            }
            return Ok(Type::Str);
        }
        if name == "parse" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `parse` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `cell` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `get` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `set` takes 2 arguments, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `release` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `array` takes no arguments, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `push` takes 2 arguments, got {}",
                    args.len()
                ));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            // `push` returns the SAME collection kind it received, so
            // `xs = xs.push(v)` keeps a `SmallArray<T, N>` binding (RFC-0056).
            let (elem, rebuild): (Type, Box<dyn Fn(Type) -> Type>) = match self.base(&at) {
                Type::Array(inner) => (
                    (*inner).clone(),
                    Box::new(|e| Type::Array(Box::new(e))),
                ),
                Type::SmallArray(inner, n) => (
                    (*inner).clone(),
                    Box::new(move |e| Type::SmallArray(Box::new(e), n)),
                ),
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
            return Ok(rebuild(elem));
        }
        if name == "at" {
            if args.len() != 2 {
                return Err(format!(
                    "line {line}: `at` takes 2 arguments, got {}",
                    args.len()
                ));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            // `m[k]` on a Map (RFC-0028): the key coerces to `String` and the
            // result is `Option<V>` (a missing key is `None`, never a trap).
            if let Type::Map(_, val) = self.base(&at) {
                let k = self.base(&self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?);
                if matches!(k, Type::Err) {
                    return Ok(Type::Err);
                }
                if crate::types::resolve(&k, self.types) != Type::Str {
                    return Err(format!(
                        "line {line}: a map key must be a String, found {k}"
                    ));
                }
                return Ok(Type::Option(val));
            }
            let elem = match self.base(&at) {
                Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                    (*inner).clone()
                }
                // `s[i]` on a String yields the byte at that index as a `UInt8`
                // (RFC-0022 — consistent with `bytes(s): Array<UInt8>` and
                // `s.length` counting bytes; mixed arithmetic needs an explicit
                // `Int64(s[i])`).
                Type::Str => Type::IntN {
                    bits: 8,
                    signed: false,
                },
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
                return Err(format!(
                    "line {line}: `at` index must be an Int64, found {i}"
                ));
            }
            return Ok(elem);
        }
        if name == "alen" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `alen` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let at = self.base(&at);
            if matches!(at, Type::Err) {
                return Ok(Type::Err);
            }
            if !matches!(at, Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..)) {
                return Err(format!("line {line}: `alen` needs an Array, found {at}"));
            }
            return Ok(Type::Int);
        }
        if name == "afree" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `afree` takes 1 argument, got {}",
                    args.len()
                ));
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
        // `xs.toArray()` (RFC-0056) — copy a `SmallArray<T, N>` out to a
        // growable `Array<T>`. Method-only (`@toArray`); the one explicit
        // conversion (no implicit coercion either direction). A plain `Array<T>`
        // receiver is also accepted (a defensive copy) so the method reads
        // uniformly, but its primary use is on a SmallArray.
        if name == "@toArray" {
            if args.len() != 1 {
                return Err(format!("line {line}: `toArray` takes no arguments"));
            }
            let at = self.expr(&args[0], scope, None, fn_ret)?;
            let elem = match self.base(&at) {
                Type::SmallArray(inner, _) | Type::Array(inner) => (*inner).clone(),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: `toArray` needs a SmallArray, found {other}"
                    ))
                }
            };
            return Ok(Type::Array(Box::new(elem)));
        }
        // Map methods (RFC-0028), all method-only (unspellable `@` names). `has`
        // and `keys` are read-only; `remove` mutates and requires a `mut` binding.
        if name == "@has" || name == "@remove" || name == "@keys" {
            let op = &name[1..];
            let mt = self.expr(&args[0], scope, None, fn_ret)?;
            let val = match self.base(&mt) {
                Type::Map(_, v) => (*v).clone(),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: `{op}` needs a Map as its receiver, found {other}"
                    ))
                }
            };
            let _ = val;
            if name == "@keys" {
                if args.len() != 1 {
                    return Err(format!("line {line}: `keys` takes no arguments"));
                }
                return Ok(Type::Array(Box::new(Type::Str)));
            }
            // `has(k)` / `remove(k)` take one String key.
            if args.len() != 2 {
                return Err(format!("line {line}: `{op}` takes 1 argument (a key)"));
            }
            if name == "@remove" {
                // Mutating: the receiver must be a plain `mut` Map binding.
                if let Expr::Var { name: recv, .. } = &args[0] {
                    let b = self.lookup(scope, recv).ok_or_else(|| {
                        format!("line {line}: `remove` on unknown variable `{recv}`")
                    })?;
                    if !b.mutable {
                        return Err(format!(
                            "line {line}: cannot `remove` from `{recv}` (declared without `mut`)"
                        ));
                    }
                } else {
                    return Err(format!(
                        "line {line}: `remove` needs a plain map variable as its receiver"
                    ));
                }
            }
            let k = self.base(&self.expr(&args[1], scope, Some(&Type::Str), fn_ret)?);
            if !matches!(k, Type::Err) && crate::types::resolve(&k, self.types) != Type::Str {
                return Err(format!(
                    "line {line}: a map key must be a String, found {k}"
                ));
            }
            return Ok(Type::Bool);
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
            if !matches!(
                src,
                Type::Int | Type::Float | Type::Float32 | Type::IntN { .. }
            ) {
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
                return Err(format!(
                    "line {line}: `value` takes 1 argument, got {}",
                    args.len()
                ));
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
                return Err(format!(
                    "line {line}: `@list` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let a = self.expr(&args[0], scope, None, fn_ret)?;
            match self.base(&a) {
                Type::ArrayN(inner, _) | Type::Array(inner) => return Ok(Type::Array(inner)),
                Type::Err => return Ok(Type::Err),
                other => {
                    return Err(format!(
                        "line {line}: `@list` needs an Array, found {other}"
                    ))
                }
            }
        }

        // built-in: Some(x) -> Option<typeof x>
        if name == "Some" {
            if args.len() != 1 {
                return Err(format!(
                    "line {line}: `Some` takes 1 argument, got {}",
                    args.len()
                ));
            }
            let inner_expected = match expected {
                Some(Type::Option(t)) => Some((**t).clone()),
                _ => None,
            };
            let aty = self.expr(&args[0], scope, inner_expected.as_ref(), fn_ret)?;
            if matches!(aty, Type::Option(_) | Type::Result(..)) {
                return Err(format!(
                    "line {line}: nested Option/Result is not supported in v0.1"
                ));
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
                return Err(format!(
                    "line {line}: `{name}` takes 1 argument, got {}",
                    args.len()
                ));
            }
            // Resolve a named alias (`type DeleteResult = Result<..>`) so the
            // expected `Result<T, E>` is visible for payload inference (RFC-0024).
            let expected_res = expected.map(|e| crate::types::resolve(e, self.types));
            let want = match &expected_res {
                Some(Type::Result(t, e)) => Some(
                    (name == "Ok")
                        .then(|| (**t).clone())
                        .unwrap_or_else(|| (**e).clone()),
                ),
                _ => None,
            };
            let aty = self.expr(&args[0], scope, want.as_ref(), fn_ret)?;
            if matches!(aty, Type::Option(_) | Type::Result(..)) {
                return Err(format!(
                    "line {line}: nested Option/Result is not supported in v0.1"
                ));
            }
            let (t, e) = match &expected_res {
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
            let mut atys: Vec<Type> = vec![Type::Err; args.len()];
            // Pass 1: the ordinary (non-`fn`) arguments bind the type parameters
            // that flow IN (e.g. `T` from `xs: Array<T>`). This must run before the
            // `fn`-typed arguments so a `map<T, U>(xs: Array<T>, f: fn(T) -> U)`
            // lambda sees a CONCRETE `T` for its parameter, and its body's type
            // then infers `U` (RFC-0023 — this ordering is what makes generic
            // higher-order functions monomorphize).
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                if matches!(pty, Type::Fn(..)) {
                    continue;
                }
                let aty = self.expr(arg, scope, None, fn_ret)?;
                self.unify(pty, &aty, &mut subst, line)?;
                atys[i] = aty;
            }
            // Pass 2: each `fn`-typed argument (a lambda, named function, or
            // pass-through parameter). The parameter's `fn(..)` type is substituted
            // with what pass 1 learned, so its own parameter types are concrete; the
            // function value's inferred return type binds the remaining parameter
            // (`U`).
            for (i, (arg, pty)) in args.iter().zip(params).enumerate() {
                if let Type::Fn(..) = pty {
                    let expected_fn = crate::types::substitute(pty, &subst);
                    self.check_fn_arg(name, i, arg, &expected_fn, scope, fn_ret, &mut subst, line)?;
                }
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
            // A `fn`-typed parameter (RFC-0023) takes a lambda, a named function,
            // or a pass-through `fn`-typed parameter — never an ordinary value —
            // and is checked/monomorphized by the shared helper.
            if let Type::Fn(..) = pty {
                let mut ignored: HashMap<String, Type> = HashMap::new();
                self.check_fn_arg(name, i, arg, pty, scope, fn_ret, &mut ignored, line)?;
                continue;
            }
            let aty = self.expr(arg, scope, Some(pty), fn_ret)?;
            if !self.coercible(&aty, pty) {
                return Err(format!(
                    "line {line}: `{name}` argument {} expects {pty}, found {aty}",
                    i + 1
                ));
            }
            self.prove_coercion(arg, pty, line)?;
            self.prove_string_interpolation(arg, pty, scope, fn_ret, line)?;
            // A `modify` parameter receives the caller's binding by reference —
            // full discipline checked in the shared helper.
            if caps.and_then(|c| c.get(i)) == Some(&Capability::Modify) {
                self.check_modify_arg(name, i, arg, &aty, pty, scope, line)?;
            }
        }
        Ok(ret.clone())
    }

    /// Check a `fn`-typed argument (RFC-0023): a lambda literal, a named
    /// top-level function, or a pass-through `fn`-typed parameter. `expected_fn`
    /// is the parameter's `fn(P..) -> R` type with its parameter types already
    /// substituted concrete; the function value's inferred return type is unified
    /// into `subst` against `R` (which drives generic higher-order inference).
    #[allow(clippy::too_many_arguments)]
    fn check_fn_arg(
        &self,
        callee: &str,
        i: usize,
        arg: &Expr,
        expected_fn: &Type,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
        subst: &mut HashMap<String, Type>,
        line: usize,
    ) -> Result<(), String> {
        let (ptys, ret) = match expected_fn {
            Type::Fn(ps, r) => (ps.clone(), (**r).clone()),
            _ => return Ok(()),
        };
        match arg {
            Expr::Lambda {
                params,
                body,
                line: lline,
            } => {
                if params.len() != ptys.len() {
                    return Err(format!(
                        "line {lline}: this lambda takes {} parameter(s), but `{callee}` \
                         argument {} expects {}",
                        params.len(),
                        i + 1,
                        ptys.len()
                    ));
                }
                // Bind the lambda's parameters (typed from the `fn` signature) in a
                // fresh frame over the enclosing scope — captures resolve through
                // the outer frames by read.
                let mut inner = scope.clone();
                inner.push(HashMap::new());
                for (pn, pty) in params.iter().zip(&ptys) {
                    inner.last_mut().unwrap().insert(
                        pn.clone(),
                        Binding {
                            ty: pty.clone(),
                            mutable: false,
                        },
                    );
                }
                // Enforce the capture rules: a lambda may not assign to, `drop`, or
                // `consume` a captured (outer) binding — it captures by read.
                let mut locals: HashSet<String> = params.iter().cloned().collect();
                self.check_lambda_body_captures(body, scope, &mut locals, *lline)?;
                // Type-check the body and infer its result type.
                let ret_known = !matches!(ret, Type::Param(_));
                let body_ty = match body {
                    LambdaBody::Expr(e) => {
                        let exp = if ret_known { Some(&ret) } else { None };
                        let t = self.expr(e, &inner, exp, fn_ret)?;
                        if ret == Type::Unit {
                            // Unit-returning lambda: the body value is discarded.
                            Type::Unit
                        } else {
                            t
                        }
                    }
                    LambdaBody::Block(b) => {
                        if !ret_known {
                            return Err(format!(
                                "line {lline}: cannot infer the return type of a \
                                 block-bodied lambda passed to a generic `fn` parameter; \
                                 use an expression body `|..| expr`"
                            ));
                        }
                        let returns = self.block(b, &ret, &mut inner);
                        if ret != Type::Unit && !returns {
                            return Err(format!(
                                "line {lline}: this lambda must return {ret} on all paths"
                            ));
                        }
                        ret.clone()
                    }
                };
                if ret == Type::Unit {
                    return Ok(());
                }
                if ret_known {
                    if !self.coercible(&body_ty, &ret) {
                        return Err(format!(
                            "line {lline}: this lambda returns {body_ty}, but `{callee}` \
                             expects it to return {ret}"
                        ));
                    }
                    if let LambdaBody::Expr(e) = body {
                        self.prove_coercion(e, &ret, *lline)?;
                    }
                } else {
                    // Infer the generic return parameter (`U`) from the body type.
                    self.unify(&ret, &body_ty, subst, *lline)?;
                }
                Ok(())
            }
            // A bare name: either a pass-through `fn`-typed parameter, or a named
            // top-level function used as a function value.
            Expr::Var { name: vn, .. } => {
                // Base-resolve so a stored value under a named fn-type alias
                // (RFC-0037, e.g. `Transform`) passes through too.
                if let Some(Type::Fn(vptys, vret)) =
                    self.lookup(scope, vn).map(|b| self.base(&b.ty))
                {
                    if vptys.len() != ptys.len() {
                        return Err(format!(
                            "line {line}: `{vn}` is a {}-argument function value, but \
                             `{callee}` argument {} expects {}",
                            vptys.len(),
                            i + 1,
                            ptys.len()
                        ));
                    }
                    for (a, b) in vptys.iter().zip(&ptys) {
                        if !self.assignable(a, b) && !self.assignable(b, a) {
                            return Err(format!(
                                "line {line}: `{vn}` has parameter type {a}, but `{callee}` \
                                 expects {b}"
                            ));
                        }
                    }
                    self.unify(&ret, &vret, subst, line)?;
                    return Ok(());
                }
                let sig = self.sigs.get(vn).ok_or_else(|| {
                    format!(
                        "line {line}: `{callee}` argument {} expects a function; `{vn}` is \
                         neither a lambda nor a known function",
                        i + 1
                    )
                })?;
                // A generic function cannot be passed as a monomorphic function
                // value in v1 (its type parameters have nothing to solve against).
                if self.generics.contains_key(vn.as_str()) {
                    return Err(format!(
                        "line {line}: `{vn}` is generic and cannot be passed as a function \
                         value in v1 (RFC-0023)"
                    ));
                }
                if sig.0.len() != ptys.len() {
                    return Err(format!(
                        "line {line}: `{vn}` takes {} argument(s), but `{callee}` argument \
                         {} expects a {}-argument function",
                        sig.0.len(),
                        i + 1,
                        ptys.len()
                    ));
                }
                for (a, b) in sig.0.iter().zip(&ptys) {
                    if !self.assignable(b, a) {
                        return Err(format!(
                            "line {line}: `{vn}` expects a {a} argument, but `{callee}` will \
                             pass it {b}"
                        ));
                    }
                }
                self.unify(&ret, &sig.1, subst, line)?;
                Ok(())
            }
            other => Err(format!(
                "line {}: `{callee}` argument {} must be a lambda `|..| ..` or a function \
                 name (RFC-0023)",
                other.line(),
                i + 1
            )),
        }
    }

    /// Check a lambda literal flowing into a STORED function value (RFC-0037):
    /// the expected type is concrete (`fn(P..) -> R`, possibly through a named
    /// alias), the capture discipline is RFC-0023's verbatim (captures are a
    /// by-value read-only snapshot at this evaluation site), and the source is
    /// recorded for defunctionalization (one enum variant per source) along
    /// with the effect summary the spawn/workers analyses need.
    fn stored_fn_lambda(
        &self,
        expr: &Expr,
        exp: &Type,
        scope: &Vec<HashMap<String, Binding>>,
        fn_ret: Option<&Type>,
    ) -> Result<Type, String> {
        let Expr::Lambda { params, body, line } = expr else {
            unreachable!()
        };
        let sig = self.base(exp);
        let Type::Fn(ptys, ret) = &sig else {
            unreachable!()
        };
        let ret = (**ret).clone();
        if params.len() != ptys.len() {
            return Err(format!(
                "line {line}: this lambda takes {} parameter(s), but the expected \
                 function type `{exp}` takes {}",
                params.len(),
                ptys.len()
            ));
        }
        // Bind the lambda's parameters (typed from the signature) in a fresh
        // frame over the enclosing scope — captures resolve through the outer
        // frames by read.
        let mut inner = scope.clone();
        inner.push(HashMap::new());
        for (pn, pty) in params.iter().zip(ptys) {
            inner.last_mut().unwrap().insert(
                pn.clone(),
                Binding {
                    ty: pty.clone(),
                    mutable: false,
                },
            );
        }
        // RFC-0023 capture rules verbatim: read-only, no nested lambda literal.
        let mut locals: HashSet<String> = params.iter().cloned().collect();
        self.check_lambda_body_captures(body, scope, &mut locals, *line)?;
        // Calls through OTHER stored fn values inside this body belong to this
        // lambda's own effect summary (the body runs wherever the value is
        // invoked, not here) — snapshot the recorder around the body check.
        let calls_before = self.stored_calls.borrow().len();
        match body {
            LambdaBody::Expr(e) => {
                let t = self.expr(e, &inner, Some(&ret), fn_ret)?;
                if ret != Type::Unit {
                    if !self.coercible(&t, &ret) {
                        return Err(format!(
                            "line {line}: this lambda returns {t}, but the expected \
                             function type `{exp}` returns {ret}"
                        ));
                    }
                    self.prove_coercion(e, &ret, *line)?;
                }
            }
            LambdaBody::Block(b) => {
                let returns = self.block(b, &ret, &mut inner);
                if ret != Type::Unit && !returns {
                    return Err(format!(
                        "line {line}: this lambda must return {ret} on all paths"
                    ));
                }
            }
        }
        let nested_sigs: Vec<Type> = self.stored_calls.borrow()[calls_before..]
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        // Effect summary for the extended spawn/workers fixpoint: the body's
        // call names, the first module-state binding it touches (if any), and
        // whether it performs a spawn-forbidden op.
        let mut calls: std::collections::HashSet<String> = Default::default();
        match body {
            LambdaBody::Expr(e) => calls_expr(e, &mut calls),
            LambdaBody::Block(b) => calls_block(b, &mut calls),
        }
        // Names that shadow module state at this point: the lambda's own
        // params/binders plus every enclosing LOCAL binding (scope frame 0 is
        // the globals frame itself).
        let mut local_names: HashSet<String> = params.iter().cloned().collect();
        if let LambdaBody::Block(b) = body {
            collect_binders_block(b, &mut local_names);
        }
        for frame in scope.iter().skip(1) {
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
        let forbidden = calls.iter().any(|c| SPAWN_FORBIDDEN.contains(&c.as_str()))
            || matches!(body, LambdaBody::Block(b) if contains_drop(b));
        self.stored_sources.borrow_mut().push(StoredSource {
            sig: sig.clone(),
            named: None,
            lambda: Some(StoredLambda {
                defined_in: self.cur_fn.borrow().clone(),
                line: *line,
                calls,
                touches_global,
                forbidden,
                nested_sigs,
            }),
        });
        Ok(exp.clone())
    }

    /// Check a bare function name flowing into a stored function value
    /// (RFC-0037): the named function's signature must match the expected
    /// `fn(P..) -> R`; generic, `extern`, and `gen` functions are rejected with
    /// named diagnostics. Records the source (an empty-payload enum variant).
    fn stored_fn_named(&self, name: &str, exp: &Type, line: usize) -> Result<Type, String> {
        let sig = self.base(exp);
        let Type::Fn(ptys, ret) = &sig else {
            unreachable!()
        };
        self.storable_named_fn(name, line)?;
        let (sptys, sret) = &self.sigs[name];
        if sptys.len() != ptys.len() {
            return Err(format!(
                "line {line}: `{name}` takes {} argument(s), but the expected \
                 function type `{exp}` takes {}",
                sptys.len(),
                ptys.len()
            ));
        }
        for (a, b) in sptys.iter().zip(ptys) {
            if !self.assignable(b, a) {
                return Err(format!(
                    "line {line}: `{name}` expects a {a} argument, but `{exp}` will \
                     pass it {b}"
                ));
            }
        }
        if **ret != Type::Unit && !self.coercible(sret, ret) {
            return Err(format!(
                "line {line}: `{name}` returns {sret}, but the expected function \
                 type `{exp}` returns {ret}"
            ));
        }
        self.stored_sources.borrow_mut().push(StoredSource {
            sig: sig.clone(),
            named: Some(name.to_string()),
            lambda: None,
        });
        Ok(exp.clone())
    }

    /// The RFC-0037 gate on using a named function as a value: generic,
    /// `extern`, and `gen` functions are rejected with named diagnostics.
    fn storable_named_fn(&self, name: &str, line: usize) -> Result<(), String> {
        if self.generics.contains_key(name) {
            return Err(format!(
                "line {line}: `{name}` is generic and cannot be used as a function \
                 value in v1 (RFC-0023)"
            ));
        }
        if self.extern_fns.contains(name) {
            return Err(format!(
                "line {line}: an `extern` function cannot be used as a function \
                 value — the host boundary dispatches by name (RFC-0037)"
            ));
        }
        if self.gen_fns.contains(name) {
            return Err(format!(
                "line {line}: a `gen fn` runs at generation time and cannot be \
                 used as a function value (RFC-0037)"
            ));
        }
        Ok(())
    }

    /// Enforce a lambda's capture discipline (RFC-0023): inside the body, a
    /// captured (outer) binding may be READ but never assigned, `drop`ped, or
    /// passed to a `consume` parameter. Names introduced inside the lambda
    /// (parameters, `let`s, `for`-vars) are tracked in `locals` and are exempt.
    fn check_lambda_body_captures(
        &self,
        body: &LambdaBody,
        outer: &Vec<HashMap<String, Binding>>,
        locals: &mut HashSet<String>,
        line: usize,
    ) -> Result<(), String> {
        match body {
            LambdaBody::Expr(e) => self.captures_expr(e, outer, locals),
            LambdaBody::Block(b) => self.captures_block(b, outer, &mut locals.clone()),
        }
        .map_err(|m| format!("line {line}: {m}"))
    }

    fn captures_block(
        &self,
        b: &Block,
        outer: &Vec<HashMap<String, Binding>>,
        locals: &mut HashSet<String>,
    ) -> Result<(), String> {
        for s in &b.stmts {
            self.captures_stmt(s, outer, locals)?;
        }
        Ok(())
    }

    fn captures_stmt(
        &self,
        s: &Stmt,
        outer: &Vec<HashMap<String, Binding>>,
        locals: &mut HashSet<String>,
    ) -> Result<(), String> {
        // A captured binding is one visible in the enclosing scope and NOT shadowed
        // by a name introduced inside the lambda.
        let is_capture = |n: &str, locals: &HashSet<String>| {
            !locals.contains(n) && self.lookup(outer, n).is_some()
        };
        match s {
            Stmt::Let { name, value, .. } => {
                self.captures_expr(value, outer, locals)?;
                locals.insert(name.clone());
                Ok(())
            }
            Stmt::Assign { name, value, line } => {
                if is_capture(name, locals) {
                    return Err(format!(
                        "a lambda captures by read; it cannot assign to the captured \
                         binding `{name}` (line {line})"
                    ));
                }
                self.captures_expr(value, outer, locals)
            }
            Stmt::SetField {
                name, value, line, ..
            } => {
                if is_capture(name, locals) {
                    return Err(format!(
                        "a lambda captures by read; it cannot mutate a field of the \
                         captured binding `{name}` (line {line})"
                    ));
                }
                self.captures_expr(value, outer, locals)
            }
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                if is_capture(name, locals) {
                    return Err(format!(
                        "a lambda captures by read; it cannot store into the captured \
                         binding `{name}` (line {line})"
                    ));
                }
                self.captures_expr(index, outer, locals)?;
                self.captures_expr(value, outer, locals)
            }
            Stmt::Drop { name, line } => {
                if is_capture(name, locals) {
                    return Err(format!(
                        "a lambda cannot `drop` the captured binding `{name}` (line {line})"
                    ));
                }
                Ok(())
            }
            Stmt::Return { value: Some(e), .. } => self.captures_expr(e, outer, locals),
            Stmt::Return { value: None, .. } => Ok(()),
            Stmt::Expr(e) => self.captures_expr(e, outer, locals),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.captures_expr(cond, outer, locals)?;
                self.captures_block(then_block, outer, &mut locals.clone())?;
                if let Some(eb) = else_block {
                    self.captures_block(eb, outer, &mut locals.clone())?;
                }
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                self.captures_expr(cond, outer, locals)?;
                self.captures_block(body, outer, &mut locals.clone())
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                self.captures_expr(iter, outer, locals)?;
                let mut inner = locals.clone();
                inner.insert(var.clone());
                self.captures_block(body, outer, &mut inner)
            }
            Stmt::Region { body, .. } => self.captures_block(body, outer, &mut locals.clone()),
        }
    }

    fn captures_expr(
        &self,
        e: &Expr,
        outer: &Vec<HashMap<String, Binding>>,
        locals: &HashSet<String>,
    ) -> Result<(), String> {
        let is_capture = |n: &str| !locals.contains(n) && self.lookup(outer, n).is_some();
        match e {
            Expr::Call { name, args, line } | Expr::Spawn { name, args, line } => {
                // Passing a captured binding to a `consume` parameter would move it
                // out of the enclosing scope from inside the lambda — forbidden.
                let caps = self.caps.get(name);
                for (k, a) in args.iter().enumerate() {
                    if caps.and_then(|c| c.get(k)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: vn, .. } = a {
                            if is_capture(vn) {
                                return Err(format!(
                                    "a lambda cannot consume the captured binding `{vn}` \
                                     (line {line})"
                                ));
                            }
                        }
                    }
                    self.captures_expr(a, outer, locals)?;
                }
                Ok(())
            }
            Expr::TryConstruct { args, .. } | Expr::ArrayLit { elems: args, .. } => {
                for a in args {
                    self.captures_expr(a, outer, locals)?;
                }
                Ok(())
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                self.captures_expr(expr, outer, locals)
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.captures_expr(lhs, outer, locals)?;
                self.captures_expr(rhs, outer, locals)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.captures_expr(scrutinee, outer, locals)?;
                for arm in arms {
                    let mut inner = locals.clone();
                    for b in crate::movecheck::pattern_bindings(&arm.pattern) {
                        inner.insert(b.to_string());
                    }
                    self.captures_expr(&arm.body, outer, &inner)?;
                }
                Ok(())
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.captures_expr(cond, outer, locals)?;
                self.captures_expr(then_branch, outer, locals)?;
                if let Some(eb) = else_branch {
                    self.captures_expr(eb, outer, locals)?;
                }
                Ok(())
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.captures_expr(v, outer, locals)?;
                }
                Ok(())
            }
            // A nested lambda literal is NOT permitted inside a lambda body in v1
            // (RFC-0023 nesting lock): it would compound monomorphization. A lambda
            // body MAY call functions that themselves take `fn` parameters — that is
            // an ordinary call, handled above.
            Expr::Lambda { line, .. } => Err(format!(
                "a lambda body may not contain another lambda literal in v1 \
                 (line {line})"
            )),
            // Scalar leaves and plain variable reads (captures by read) are fine.
            _ => Ok(()),
        }
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
            // A generic `Array<T>` / `Array<T, N>` binds `T` from the element type
            // (RFC-0023 relies on this so `map<T, U>(xs: Array<T>, ..)` infers `T`).
            // A transparent named alias to a collection resolves first.
            Type::Array(inner) => match crate::types::resolve(aty, self.types) {
                Type::Array(a) => self.unify(inner, &a, subst, line),
                _ => Err(format!("line {line}: expected {pty}, found {aty}")),
            },
            Type::ArrayN(inner, n) => match aty {
                Type::ArrayN(a, m) if m == n => self.unify(inner, a, subst, line),
                _ => Err(format!("line {line}: expected {pty}, found {aty}")),
            },
            // A generic `SmallArray<T, N>` binds `T` from the element type; `N`
            // must match exactly (RFC-0056 — integer arguments do not infer).
            Type::SmallArray(inner, n) => match crate::types::resolve(aty, self.types) {
                Type::SmallArray(a, m) if m == *n => self.unify(inner, &a, subst, line),
                _ => Err(format!("line {line}: expected {pty}, found {aty}")),
            },
            Type::Ref(inner) => match aty {
                Type::Ref(a) => self.unify(inner, a, subst, line),
                _ => Err(format!("line {line}: expected {pty}, found {aty}")),
            },
            // A generic `Map<String, V>` binds `V` from the value type. A
            // transparent named alias to a map resolves first.
            Type::Map(pk, pv) => match crate::types::resolve(aty, self.types) {
                Type::Map(ak, av) => {
                    self.unify(pk, &ak, subst, line)?;
                    self.unify(pv, &av, subst, line)
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
            // A `SmallArray<T, N>` (RFC-0056) can shrink like a growable Array.
            Type::SmallArray(inner, _) => Ok((*inner).clone()),
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
                UnOp::BitNot => format!("~{s}"),
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
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
            };
            format!("{} {o} {}", pred_summary(lhs), pred_summary(rhs))
        }
        // `at(s, i)` is the desugaring of indexing — render it back as `s[i]`.
        Expr::Call { name, args, .. } if name == "at" && args.len() == 2 => {
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
        Expr::Spawn { name, .. } => format!("spawn {name}(..)"),
        Expr::Lambda { params, .. } => format!("|{}| ..", params.join(", ")),
    }
}

/// Whether a type contains a directly nested `Option`/`Result` (the v0.1
/// prohibition), anywhere inside it.
fn has_nested_wrap(ty: &Type) -> bool {
    let wrapped = |t: &Type| matches!(t, Type::Option(_) | Type::Result(..));
    match ty {
        Type::Option(t) => wrapped(t) || has_nested_wrap(t),
        Type::Result(a, b) => wrapped(a) || wrapped(b) || has_nested_wrap(a) || has_nested_wrap(b),
        Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) | Type::Ref(t)
        | Type::Task(t) => has_nested_wrap(t),
        Type::Map(k, v) => has_nested_wrap(k) || has_nested_wrap(v),
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
    let max = if signed {
        i64::MAX >> (64 - u32::from(bits))
    } else {
        (1i64 << bits) - 1
    };
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
        let max: u64 = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        format!("0..={max}")
    }
}

/// Render a literal as the user wrote it: a negative `n` is a wrapped
/// u64-range literal, so show its unsigned value.
fn render_int_literal(n: i64) -> String {
    if n < 0 {
        (n as u64).to_string()
    } else {
        n.to_string()
    }
}

/// Builtins a concurrent task may not use: `print` (observable ordering),
/// `cell`/`set`/`release` (mutate the shared reference slab), `afree` (frees a
/// buffer the caller may still hold across the task boundary), and the log
/// methods. `get` is a read-only slab access and is allowed.
const SPAWN_FORBIDDEN: &[&str] = &[
    "print",
    "cell",
    "set",
    "release",
    "afree",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    // Input I/O effects (RFC-0014): observe/mutate the outside world (stdin
    // cursor, the filesystem), so they must not cross a task boundary. `listDir`
    // reads the filesystem too (RFC-0021).
    "args",
    "readLine",
    "readFile",
    "writeFile",
    "renameFile",
    "fsyncFile",
    "readFileBytes",
    "stringFromBytes",
    "listDir",
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
        Stmt::If {
            then_block,
            else_block,
            ..
        } => contains_drop(then_block) || else_block.as_ref().is_some_and(contains_drop),
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
        Expr::Call { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => args.iter().any(expr_contains_spawn),
        Expr::Match {
            scrutinee, arms, ..
        } => expr_contains_spawn(scrutinee) || arms.iter().any(|a| expr_contains_spawn(&a.body)),
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            expr_contains_spawn(cond)
                || expr_contains_spawn(then_branch)
                || else_branch.as_ref().is_some_and(|e| expr_contains_spawn(e))
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
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
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
    "writeFile",
    "renameFile",
    "fsyncFile",
    "readLine",
    "args",
    "readFileBytes",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
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
    let fn_map: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let extern_fns: std::collections::HashSet<&str> = program
        .functions
        .iter()
        .filter(|f| f.is_extern)
        .map(|f| f.name.as_str())
        .collect();
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
/// One source that flows into a stored function value (RFC-0037): a named
/// top-level function or a lambda literal — one defunctionalization enum
/// variant each, in collection (declaration/lift) order.
#[derive(Debug, Clone)]
pub struct StoredSource {
    /// The structural `fn(P..) -> R` signature (named aliases resolved).
    pub sig: Type,
    /// `Some(name)` when the source is a named top-level function.
    pub named: Option<String>,
    /// `Some` when the source is a lambda literal.
    pub lambda: Option<StoredLambda>,
}

/// A lambda source's effect summary (RFC-0037), computed where the literal is
/// checked — the spawn/workers analyses union these over a signature's sources.
#[derive(Debug, Clone)]
pub struct StoredLambda {
    /// The function whose body lexically contains the literal.
    pub defined_in: String,
    pub line: usize,
    /// Every call name in the body (functions, builtins, methods).
    pub calls: std::collections::HashSet<String>,
    /// The first module-state binding the body reads or writes, if any.
    pub touches_global: Option<String>,
    /// Whether the body performs a spawn-forbidden op (I/O, cells, `drop`, ...).
    pub forbidden: bool,
    /// Signatures of OTHER stored function values the body itself calls.
    pub nested_sigs: Vec<Type>,
}

/// Whole-program stored-function-value facts (RFC-0037).
#[derive(Debug, Clone, Default)]
pub struct StoredFnEffects {
    pub sources: Vec<StoredSource>,
    /// `(function, signature)` for each call through a stored fn value.
    pub calls: Vec<(String, Type)>,
}

/// Whether two collected fn signatures could describe the same stored value.
/// Structural equality, loosened so a generic `Type::Param` matches anything
/// (a stored fn type inside a generic function is collected pre-substitution;
/// matching loosely keeps the effect union conservative).
fn fn_sigs_match(a: &Type, b: &Type) -> bool {
    if matches!(a, Type::Param(_)) || matches!(b, Type::Param(_)) {
        return true;
    }
    match (a, b) {
        (Type::Fn(ap, ar), Type::Fn(bp, br)) => {
            ap.len() == bp.len()
                && ap.iter().zip(bp).all(|(x, y)| fn_sigs_match(x, y))
                && fn_sigs_match(ar, br)
        }
        (Type::Option(x), Type::Option(y))
        | (Type::Array(x), Type::Array(y))
        | (Type::Ref(x), Type::Ref(y))
        | (Type::Task(x), Type::Task(y)) => fn_sigs_match(x, y),
        (Type::Result(x1, x2), Type::Result(y1, y2)) | (Type::Map(x1, x2), Type::Map(y1, y2)) => {
            fn_sigs_match(x1, y1) && fn_sigs_match(x2, y2)
        }
        _ => a == b,
    }
}

/// The signatures whose stored values are NOT spawn-safe to call, under the
/// current safe-function assumption: a signature is unsafe when ANY collected
/// source is — a named source outside `safe`, or a lambda source that touches
/// module state, performs a forbidden op, calls an unsafe function, or calls
/// a stored value of an unsafe signature (iterated to fixpoint).
fn stored_unsafe_sigs(
    effects: &StoredFnEffects,
    safe: &std::collections::HashSet<String>,
    fn_names: &std::collections::HashSet<String>,
) -> Vec<Type> {
    let mut unsafe_sigs: Vec<Type> = Vec::new();
    loop {
        let mut changed = false;
        for src in &effects.sources {
            if unsafe_sigs.iter().any(|u| fn_sigs_match(u, &src.sig)) {
                continue;
            }
            let bad = if let Some(n) = &src.named {
                !safe.contains(n)
            } else if let Some(l) = &src.lambda {
                l.touches_global.is_some()
                    || l.forbidden
                    || l.calls
                        .iter()
                        .any(|c| fn_names.contains(c) && !safe.contains(c))
                    || l.nested_sigs
                        .iter()
                        .any(|s| unsafe_sigs.iter().any(|u| fn_sigs_match(u, s)))
            } else {
                false
            };
            if bad {
                unsafe_sigs.push(src.sig.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    unsafe_sigs
}

/// Extend the pre-check spawn-safety set with stored-function-value edges
/// (RFC-0037): a function that calls a stored value of an unsafe signature
/// becomes unsafe, and the ordinary call graph re-propagates until fixed.
fn extend_spawn_safe(
    program: &Program,
    pre: &std::collections::HashSet<String>,
    effects: &StoredFnEffects,
) -> std::collections::HashSet<String> {
    let fn_names: std::collections::HashSet<String> =
        program.functions.iter().map(|f| f.name.clone()).collect();
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
    let mut ext = pre.clone();
    loop {
        let mut changed = false;
        let unsafe_sigs = stored_unsafe_sigs(effects, &ext, &fn_names);
        for (f, sig) in &effects.calls {
            if ext.contains(f) && unsafe_sigs.iter().any(|u| fn_sigs_match(u, sig)) {
                ext.remove(f);
                changed = true;
            }
        }
        // Ordinary call-graph propagation over the shrunk set.
        let snapshot = ext.clone();
        for f in &program.functions {
            if snapshot.contains(&f.name) {
                let mut callees: std::collections::HashSet<String> = Default::default();
                for c in fn_calls(&f.body) {
                    if let Some(impls) = method_impls.get(&c) {
                        callees.extend(impls.iter().cloned());
                    }
                    callees.insert(c);
                }
                let ok = callees
                    .iter()
                    .filter(|c| fn_names.contains(*c))
                    .all(|c| snapshot.contains(c));
                if !ok && ext.remove(&f.name) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    ext
}

/// RFC-0025 (`vyrn serve --workers`): does `root` — transitively — read or
/// write module state? Each worker owns a fully independent interpreter, so
/// module state is the ONE thing a parallel `handle` cannot touch soundly
/// (it is shared by definition; per-worker copies would silently diverge).
/// Returns the shortest call chain `root -> .. -> offender` plus the name of
/// a touched global, or `None` when the whole call tree is module-state-free.
///
/// Deliberately narrower than spawn-safety: `print`, logging, and file I/O
/// are thread-compatible host effects (each access-log/output line stays
/// atomic) and do NOT gate workers — only shared mutable state does.
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
    // RFC-0037: a call through a stored function value dispatches over the
    // signature's collected sources. Each distinct signature becomes a pseudo
    // node (named for the chain) whose callees are the sources: a named source
    // is an edge to that function; a lambda source that touches module state is
    // an offender itself, and its calls/nested stored calls are further edges.
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
    // Surface method names expand to every registered impl, exactly like the
    // spawn-safety fixpoint — otherwise a global-touching impl reached through
    // a protocol method call would hide from the walk.
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
    // BFS from `root` with parent links, so the first hit yields the shortest
    // chain (deterministic: candidates visit in sorted order).
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
        // A stored-value pseudo node (RFC-0037): its "body" is the union of the
        // signature's sources. A module-state-touching lambda source is the
        // offender; everything else contributes edges.
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
        // RFC-0037: calls through stored values dispatch to the pseudo node.
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
        Stmt::Assign { name, value, .. } | Stmt::SetField { name, value, .. } => {
            is_global(name) || global_ref_expr(value, globals, local)
        }
        Stmt::IndexSet {
            name, index, value, ..
        } => {
            is_global(name)
                || global_ref_expr(index, globals, local)
                || global_ref_expr(value, globals, local)
        }
        Stmt::Return { value: Some(e), .. } => global_ref_expr(e, globals, local),
        Stmt::Return { value: None, .. } => false,
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            global_ref_expr(cond, globals, local)
                || global_ref_block(then_block, globals, local)
                || else_block
                    .as_ref()
                    .is_some_and(|eb| global_ref_block(eb, globals, local))
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
        Expr::Match {
            scrutinee, arms, ..
        } => {
            global_ref_expr(scrutinee, globals, local)
                || arms
                    .iter()
                    .any(|a| global_ref_expr(&a.body, globals, local))
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            global_ref_expr(cond, globals, local)
                || global_ref_expr(then_branch, globals, local)
                || else_branch
                    .as_ref()
                    .is_some_and(|e| global_ref_expr(e, globals, local))
        }
        Expr::StructLit { fields, .. } => fields
            .iter()
            .any(|(_, v)| global_ref_expr(v, globals, local)),
        Expr::ArrayLit { elems, .. } => elems.iter().any(|v| global_ref_expr(v, globals, local)),
        Expr::MapLit { entries, .. } => entries
            .iter()
            .any(|(k, v)| global_ref_expr(k, globals, local) || global_ref_expr(v, globals, local)),
        // A lambda body (RFC-0023) that reads module state makes the enclosing
        // call chain non-spawn-safe — the effect is attributed to the
        // instantiation site (this function).
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => global_ref_expr(e2, globals, local),
            LambdaBody::Block(b) => global_ref_block(b, globals, local),
        },
    }
}

/// Enforce a module-state initializer's restrictions (RFC-0013, RFC-0029): it
/// may not read a global declared later (or itself), and it may call only
/// literals/operators/built-ins, constructors, and functions IMPORTED from
/// another module (which initialize first). A same-module ordinary function,
/// any `extern`, a protocol method, or a `spawn` is rejected. Returns the first
/// violation.
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
) -> Result<(), String> {
    let recur = |e: &Expr| {
        init_restrictions(
            e,
            forbidden,
            fn_module,
            own_module,
            all_globals,
            ready,
            own_name,
            line,
        )
    };
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
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => recur(expr),
        Expr::Binary { lhs, rhs, .. } => {
            recur(lhs)?;
            recur(rhs)
        }
        Expr::Call { name, args, .. } => {
            // An `extern` or protocol method is never callable before `main`.
            let forbidden_here = forbidden.contains(name)
                // A SAME-MODULE ordinary function is forbidden too — only
                // imported modules are guaranteed initialized first (RFC-0029).
                || matches!(fn_module.get(name), Some(m) if m == own_module);
            if forbidden_here {
                return Err(format!(
                    "line {line}: initializer of `{own_name}` may not call `{name}` — a \
                     module-state initializer runs before `main`, so it may use only \
                     literals, operators, built-ins, and functions imported from another \
                     module (whose state initializes first)"
                ));
            }
            for a in args {
                recur(a)?;
            }
            Ok(())
        }
        Expr::Spawn { name, .. } => Err(format!(
            "line {line}: initializer of `{own_name}` may not `spawn {name}` — a \
             module-state initializer runs before `main` (no user calls)"
        )),
        Expr::TryConstruct { args, .. } => {
            for a in args {
                recur(a)?;
            }
            Ok(())
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            recur(scrutinee)?;
            for a in &arms.iter().map(|a| &a.body).collect::<Vec<_>>() {
                recur(a)?;
            }
            Ok(())
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            recur(cond)?;
            recur(then_branch)?;
            if let Some(eb) = else_branch {
                recur(eb)?;
            }
            Ok(())
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                recur(v)?;
            }
            Ok(())
        }
        Expr::ArrayLit { elems, .. } => {
            for v in elems {
                recur(v)?;
            }
            Ok(())
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                recur(k)?;
                recur(v)?;
            }
            Ok(())
        }
        // A lambda literal can never appear in a valid initializer (the checker's
        // position rule rejects it outside a call argument, and initializers make
        // no calls); recurse for completeness so the deeper diagnostic still fires.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => recur(e2),
            LambdaBody::Block(_) => Ok(()),
        },
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
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
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
        Expr::Match {
            scrutinee, arms, ..
        } => {
            calls_expr(scrutinee, out);
            for a in arms {
                calls_expr(&a.body, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            calls_expr(cond, out);
            calls_expr(then_branch, out);
            if let Some(eb) = else_branch {
                calls_expr(eb, out);
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
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                calls_expr(k, out);
                calls_expr(v, out);
            }
        }
        // Calls inside a lambda body (RFC-0023) are attributed to the enclosing
        // function — that is the monomorphization site, so a lambda that performs
        // I/O (or, via `global_ref_expr`, reads module state) makes the enclosing
        // function non-spawn-safe.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => calls_expr(e2, out),
            LambdaBody::Block(b) => calls_block(b, out),
        },
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

    // ---- module_state_use (RFC-0025, the `--workers` gate) ----------------

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

    // ---- stored function values: spawn / workers pins (RFC-0037) ---------

    #[test]
    fn spawning_through_a_stateful_stored_value_is_rejected() {
        // `work` looks pure to the pre-check fixpoint (its only impurity flows
        // through a stored function value whose source reads module state) —
        // the extended fixpoint must catch the spawn.
        let src = "let mut hits: Int64 = 0\n\
             fn stateful() -> Int64 { return hits }\n\
             fn make() -> fn() -> Int64 { return stateful }\n\
             fn work() -> Int64 { let f = make()  return f() }\n\
             fn main() -> Int64 { let t = spawn work()  return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("invokes a stored function value"), "{e}");
    }

    #[test]
    fn spawning_through_an_isolated_stored_value_is_fine() {
        let src = "fn pure() -> Int64 { return 7 }\n\
             fn make() -> fn() -> Int64 { return pure }\n\
             fn work() -> Int64 { let f = make()  return f() }\n\
             fn main() -> Int64 { let t = spawn work()  return t.join() }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn spawned_function_may_not_take_fn_parameters() {
        let src = "fn hof(f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n\
             fn main() -> Int64 { let t = spawn hof(|x| x)  return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("may not take function-value parameters"), "{e}");
    }

    #[test]
    fn workers_gate_walks_through_stored_values() {
        // The definer (`make`) never touches state itself; the offending global
        // is reached only THROUGH the stored value's source — the chain must
        // name the pseudo node and the source function.
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
        // RFC-0029: the `--workers` gate is module-agnostic — after linking, a
        // global owned by an imported module is an ordinary program global, so a
        // `handle` reaching it gates workers exactly as a root global would. We
        // simulate the post-link program by tagging the state and its accessor
        // with a foreign module; the gate must still fire and name the global.
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
        // Other functions may touch the global; only `handle`'s call tree counts.
        // `print` and file I/O do NOT gate workers — module state is THE gate.
        let src = "let mut hits: Int64 = 0\n\
                   fn other() -> Int64 { hits = hits + 1\n return hits }\n\
                   fn pure(n: Int64) -> Int64 { return n * 2 }\n\
                   fn handle(n: Int64) -> Int64 { print(n)\n let r = readFile(\"x\")\n \
                       return pure(n) }\n";
        let program = parse(lex(src).unwrap()).unwrap();
        assert!(module_state_use(&program, "handle", &Default::default()).is_none());
    }

    #[test]
    fn rejects_type_mismatch() {
        let e = check_src("fn main() -> Int64 { return true; }").unwrap_err();
        assert!(e.contains("return type mismatch"), "{e}");
    }

    // ---- codability of payload enums / Result (RFC-0024) ----------------

    #[test]
    fn payload_enum_is_codable() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit \
                   fn f(s: Shape) -> String { return toJson(s) } \
                   fn g(s: String) -> Validation<Shape> { return fromJson(Shape, s) } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn result_is_codable_and_named_aliases_work() {
        let src = "type R = Result<Bool, String> \
                   fn f(x: R) -> String { return toJson(x) } \
                   fn g(s: String) -> Validation<R> { return fromJson(R, s) } \
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
                       let v = fromJson(Issue, s) \
                       return toJson(v) } \
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("cannot encode `Validation`"), "{e}");
    }

    #[test]
    fn option_of_result_is_codable() {
        let src = "type Wrap = { r: Option<Result<Int64, String>> } \
                   fn f(w: Wrap) -> String { return toJson(w) } \
                   fn g(s: String) -> Validation<Wrap> { return fromJson(Wrap, s) } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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
        assert!(
            e.contains("not comptime-pure") && e.contains("spawn"),
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
        let e = check_src("fn main() -> Int64 { let r = writeFile(\"p\"); return 0 }").unwrap_err();
        assert!(e.contains("`writeFile` takes 2 arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let a = args(1); return 0 }").unwrap_err();
        assert!(e.contains("`args` takes no arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let l = readLine(\"x\"); return 0 }").unwrap_err();
        assert!(e.contains("`readLine` takes no arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let s = stringFromBytes(\"x\"); return 0 }")
            .unwrap_err();
        assert!(e.contains("`stringFromBytes` needs an Array<UInt8>"), "{e}");
    }

    #[test]
    fn slice_builtin_signature() {
        // slice(String, Int64, Int64) -> String (RFC-0046).
        assert!(check_src(
            "fn main() -> Int64 { let x: String = slice(\"hello\", 1, 3) return x.length }"
        )
        .is_ok());
        let e = check_src("fn main() -> Int64 { let x = slice(\"hi\") return 0 }").unwrap_err();
        assert!(e.contains("`slice` takes 3 arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let x = slice(42, 0, 1) return 0 }").unwrap_err();
        assert!(e.contains("`slice` needs a String"), "{e}");
        let e =
            check_src("fn main() -> Int64 { let x = slice(\"hi\", \"a\", 1) return 0 }").unwrap_err();
        assert!(e.contains("`slice` needs Int64 offsets"), "{e}");
    }

    #[test]
    fn io_builtins_are_spawn_forbidden() {
        // A function touching stdin/files/argv is an effect — never a task.
        for body in [
            "let l = readLine()",
            "let r = readFile(\"p\")",
            "let w = writeFile(\"p\", \"c\")",
            "let a = args()",
        ] {
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
        let e =
            check_src("fn main() -> Int64 { let b: Array<Int64> = bytes(\"hi\") return b.length }")
                .unwrap_err();
        assert!(e.contains("Array<UInt8>"), "{e}");
    }

    #[test]
    fn string_index_is_uint8() {
        // RFC-0022: `s[i]` is a UInt8 — it flows into a UInt8 slot, and returning
        // it as Int64 without an explicit `Int64(..)` is a type error.
        let ok = "fn main() -> Int64 { let s = \"hi\" let b: UInt8 = s[0] return Int64(b) }";
        assert!(check_src(ok).is_ok(), "{:?}", check_src(ok));
        let e = check_src("fn main() -> Int64 { let s = \"hi\" return s[0] }").unwrap_err();
        assert!(e.contains("expected Int64, found UInt8"), "{e}");
    }

    #[test]
    fn string_ordering_type_rule() {
        // RFC-0022: `< <= > >=` accept two Strings and yield Bool. Mixing a String
        // with a non-String is rejected with the numeric-or-String wording.
        for op in ["<", "<=", ">", ">="] {
            let ok =
                format!("fn main() -> Int64 {{ if \"a\" {op} \"b\" {{ return 1 }} return 0 }}");
            assert!(check_src(&ok).is_ok(), "{op}: {:?}", check_src(&ok));
        }
        let e = check_src("fn main() -> Int64 { if \"a\" < 3 { return 1 } return 0 }").unwrap_err();
        assert!(e.contains("numeric or String"), "{e}");
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
        let e = check_src(
            "fn noisy(n: Int64) -> Int64 { print(n); return n; } \
                           fn main() -> Int64 { let t = spawn noisy(5); return t.join(); }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn rejects_spawn_of_transitively_impure_function() {
        let e = check_src(
            "fn inner(n: Int64) -> Int64 { print(n); return n; } \
                           fn outer(n: Int64) -> Int64 { return inner(n); } \
                           fn main() -> Int64 { let t = spawn outer(5); return t.join(); }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    // ---- RFC-0044 storage host effects (renameFile / fsyncFile) ------------

    #[test]
    fn rfc0044_rename_and_fsync_have_the_io_signatures() {
        let src = "fn main() -> Int64 { \
                       let r: Result<Bool, String> = renameFile(\"a\", \"b\") \
                       let s: Result<Bool, String> = fsyncFile(\"a\") \
                       return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
        let e = check_src("fn main() -> Int64 { let r = renameFile(\"a\"); return 0 }").unwrap_err();
        assert!(e.contains("`renameFile` takes 2 arguments"), "{e}");
        let e = check_src("fn main() -> Int64 { let r = fsyncFile(1); return 0 }").unwrap_err();
        assert!(e.contains("`fsyncFile` needs a String path"), "{e}");
    }

    #[test]
    fn rfc0044_rename_and_fsync_are_rejected_in_a_generator() {
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
    fn rfc0044_rename_and_fsync_are_effects_not_tasks() {
        for io in ["renameFile(\"a\", \"b\")", "fsyncFile(\"a\")"] {
            let src = format!(
                "fn eff(n: Int64) -> Int64 {{ let w = {io} return n }} \
                 fn main() -> Int64 {{ let t = spawn eff(5); return t.join() }}"
            );
            let e = check_src(&src).unwrap_err();
            assert!(e.contains("isolated (pure)"), "{io}: {e}");
        }
    }

    #[test]
    fn rfc0044_load_result_prelude_enum_is_matchable() {
        // `load` is a call-site desugar; the injected `LoadResult<T>` enum lets a
        // caller match all three outcomes without importing anything.
        let src = "type Rec = { n: Int64 } \
                   fn describe(r: LoadResult<Rec>) -> Int64 { \
                       return match r { \
                           Missing => 1, Corrupt(iss) => 2, Loaded(x) => x.n } } \
                   fn main() -> Int64 { return describe(load(Rec, \"p\")) }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
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
        let e =
            check_src("test \"dup\" { assert(true) } test \"dup\" { assert(true) }").unwrap_err();
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

    // ---- RFC-0043 time/random effect pins --------------------------------
    // now()/monotonic()/randomSeed() are host-boundary externs, so the EXISTING
    // purity analysis (not new machinery) governs where they may appear: they
    // are host effects. The pure PRNG/formatting — ordinary arithmetic, no
    // extern — stays usable anywhere, including generators.

    #[test]
    fn rfc0043_host_clock_extern_is_rejected_in_a_generator() {
        // A gen fn reaching the clock extern (what `now()` wraps) is not
        // comptime-pure — pinned via the existing extern rule.
        let e = check_src(
            "extern fn hostNowMillis() -> Int64 \
             fn now() -> Int64 { return hostNowMillis() } \
             gen fn g() -> String { let t = now() return \"\" } \
             fn main() -> Int64 { return 0 }",
        )
        .unwrap_err();
        assert!(
            e.contains("not comptime-pure") && e.contains("extern"),
            "{e}"
        );
    }

    #[test]
    fn rfc0043_pure_prng_is_comptime_usable() {
        // The seeded PRNG is pure arithmetic (MINSTD): a gen fn may use it, so a
        // reproducible generator is fine — only the host SEED is an effect.
        let src = "gen fn g() -> String { \
                     let seed = 42 \
                     let next = (seed * 48271) % 2147483647 \
                     return \"\" \
                   } \
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn rfc0043_spawned_task_calling_the_clock_is_rejected() {
        // A task calling now()/randomSeed() does host I/O; like print/file I/O it
        // is not isolated, so `spawn` rejects it (consistent treatment — the RFC
        // prose's "allowed" is inaccurate: host I/O in a task is forbidden, as
        // `parallel.vyrn` documents).
        let e = check_src(
            "extern fn hostRandomSeed() -> Int64 \
             fn seed() -> Int64 { return hostRandomSeed() } \
             fn main() -> Int64 { let t = spawn seed(); return t.join() }",
        )
        .unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
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

    // ---- export extern (RFC-0012 M2) -------------------------------------

    #[test]
    fn export_extern_without_a_body_is_a_parse_error() {
        // The exported direction MUST supply an implementation; a body-less form
        // is an import, which is not how you write `export`.
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
        assert!(
            !e.is_empty(),
            "a type error in the body must be reported: {e}"
        );
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
        let e = check_src(
            "type P = { x: Int64 }; \
                           fn main() -> Int64 { let p = P { x: 1 }; p.x = 2; return p.x; }",
        )
        .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn rejects_field_mutation_wrong_type() {
        let e = check_src(
            "type P = { x: Int64 }; \
                           fn main() -> Int64 { let mut p = P { x: 1 }; p.x = \"s\"; return 0; }",
        )
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
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                           a = push(a, \"x\"); return 0; }",
        )
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
        let e =
            check_src("fn main() -> Int64 { let a: Array<Int64> = [1, 2]; a[0] = 9; return 0; }")
                .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn pop_requires_mut() {
        let e = check_src(
            "fn main() -> Int64 { let a: Array<Int64> = [1, 2]; let p = a.pop(); return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("without `mut`"), "{e}");
    }

    #[test]
    fn index_store_rejects_wrong_element_type() {
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2]; a[0] = \"x\"; return 0; }",
        )
        .unwrap_err();
        assert!(e.contains("holds Int64"), "{e}");
    }

    #[test]
    fn index_store_rejects_non_int_index() {
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2]; a[\"i\"] = 9; return 0; }",
        )
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
        let e = check_src(
            "fn main() -> Int64 { let mut a: Array<Int64> = [1]; let p = pop(a); return 0; }",
        )
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
            assert!(
                e.contains("does not satisfy `Age`"),
                "case: {src}\ngot: {e}"
            );
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
        let e =
            check_src("fn main() -> Int64 { let x = 9223372036854775808; return 0; }").unwrap_err();
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
        assert!(
            check_src(bad).unwrap_err().contains("UserId"),
            "raw string rejected"
        );
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
                "fn main() -> Int64 { let n = 5; return join(n); }",
                "`join(t)` was removed",
            ),
            (
                "fn main() -> Int64 { let s = toString(1); return 0; }",
                "`toString` is a method",
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
        assert!(
            check_src(bad).unwrap_err().contains("does not satisfy"),
            "port"
        );
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
        assert!(
            check_src(bad)
                .unwrap_err()
                .contains("does not satisfy `Name`"),
            "short"
        );
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
        // A non-literal pattern is rejected.
        let dyn_pat = "fn f(s: String, p: String) -> Bool { return s =~ p; } \
                       fn main() -> Int64 { return 0; }";
        assert!(check_src(dyn_pat)
            .unwrap_err()
            .contains("string-literal pattern"));
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
        assert!(
            e.contains("isolated") || e.contains("spawn") || e.contains("pure"),
            "{e}"
        );
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
        assert!(
            e.contains("isolated") || e.contains("spawn") || e.contains("pure"),
            "{e}"
        );
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

    // ---- RFC-0020 M1: finite string types & interpolation containment -------

    /// A common prelude: a finite key type, a finite section type, and a `t`
    /// consumer.
    const KEYS: &str = "type TransKey = String where value =~ \"(home\\\\.(title|subtitle)|nav\\\\.(home|about|settings)\\\\.label)\"\n\
         type Section = String where value =~ \"home|about|settings\"\n\
         fn t(key: TransKey) -> Int64 { return 0 }\n";

    #[test]
    fn proven_interpolation_into_finite_type_is_accepted() {
        // Every value of `"nav.\{s}.label"` (s: Section) is a TransKey.
        let src = format!(
            "{KEYS}fn main() -> Int64 {{ let s: Section = \"home\"  return t(\"nav.\\{{s}}.label\") }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn interpolation_witness_is_a_compile_error() {
        // A Section that includes `profile` produces `nav.profile.label`, which
        // TransKey does not contain — the witness must name it exactly.
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
        // A plain-String hole is not finite → no containment, no error (the
        // boundary keeps its ordinary runtime validation).
        let src = format!(
            "{KEYS}fn build(x: String) -> Int64 {{ return t(\"nav.\\{{x}}.label\") }}\n\
             fn main() -> Int64 {{ return build(\"home\") }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn finite_var_contained_is_accepted_and_uncontained_is_left_to_runtime() {
        // Section ⊆ TransKey? No — but a Section variable might still hold a
        // conforming value, so flowing it into TransKey is NOT an error (runtime
        // check stays). This must type-check.
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
        // The target mixes a length clause with a regex clause → not a pure
        // regex language → containment does not apply, no error even though the
        // interpolation could exceed the length at runtime.
        let src = "type Key = String where value =~ \"[a-z]+\" && value.length < 3\n\
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

    // ---- RFC-0023 function values ---------------------------------------

    const TWICE: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n";

    #[test]
    fn accepts_lambda_and_named_fn_argument() {
        let src = format!(
            "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{ let a = twice([1, 2], |x| x * 2)  let b = twice([1, 2], dbl)  return 0 }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn fn_types_are_storable() {
        // RFC-0037 lifts the v1 parameter-only restriction: `fn(..) -> ..` is
        // legal in returns, `let` annotations, record fields, array elements,
        // Option payloads, and module state — with lambda literals and named
        // functions accepted as sources.
        let ret = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn pick() -> fn(Int64) -> Int64 { return dbl }\n\
             fn main() -> Int64 { let f = pick()  return f(21) }";
        assert!(check_src(ret).is_ok(), "{:?}", check_src(ret));
        let letb = "fn main() -> Int64 { let g: fn(Int64) -> Int64 = |x| x * 2  return g(3) }";
        assert!(check_src(letb).is_ok(), "{:?}", check_src(letb));
        let rec = "type R = { f: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 { let r = R { f: |x| x + 1 }  let g = r.f  return g(1) }";
        assert!(check_src(rec).is_ok(), "{:?}", check_src(rec));
        let arr = "fn main() -> Int64 {\n\
             let mut xs: Array<fn(Int64) -> Int64> = []\n\
             xs.push(|x| x * 2)\n\
             let f = xs[0]\n\
             return f(4) }";
        assert!(check_src(arr).is_ok(), "{:?}", check_src(arr));
        let opt = "fn main() -> Int64 {\n\
             let o: Option<fn(Int64) -> Int64> = Some(|x| x - 1)\n\
             return match o { Some(f) => f(1), None => 0 } }";
        assert!(check_src(opt).is_ok(), "{:?}", check_src(opt));
        let state = "type Middleware = fn(Int64) -> Int64\n\
             let mut chain: Array<Middleware> = []\n\
             fn add() { chain.push(|x| x + 1) }\n\
             fn main() -> Int64 { add()  let m = chain[0]  return m(1) }";
        assert!(check_src(state).is_ok(), "{:?}", check_src(state));
        // Composition: value-to-value flow creates no new source and stays legal.
        let compose = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let g = dbl  let h = g  return h(5) }";
        assert!(check_src(compose).is_ok(), "{:?}", check_src(compose));
    }

    #[test]
    fn fn_types_still_rejected_where_illegal() {
        // `Ref` cannot hold a function value (RFC-0037 defers it).
        let refv = "fn main() -> Int64 { let c: Ref<fn(Int64) -> Int64> = cell(0)  return 0 }";
        assert!(
            check_src(refv)
                .unwrap_err()
                .contains("cannot hold a function value"),
            "{:?}",
            check_src(refv)
        );
        // No higher-order-of-higher-order: a stored fn type may not take or
        // return another function value.
        let hof = "fn main() -> Int64 { let g: fn(fn(Int64) -> Int64) -> Int64 = |x| 0  return 0 }";
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
             fn main() -> Int64 { let h = H { f: |x| x }  let s = toJson(h)  return 0 }";
        assert!(
            check_src(wire).unwrap_err().contains("cannot encode"),
            "{:?}",
            check_src(wire)
        );
    }

    #[test]
    fn lambda_still_needs_a_function_type_from_context() {
        let src = "fn main() -> Int64 { let g = |x| x * 2  return 0 }";
        assert!(
            check_src(src)
                .unwrap_err()
                .contains("needs a function type from context"),
            "{:?}",
            check_src(src)
        );
    }

    #[test]
    fn lambda_arity_mismatch_is_rejected() {
        let src =
            format!("{TWICE}fn main() -> Int64 {{ let a = twice([1], |x, y| x + y)  return 0 }}");
        assert!(
            check_src(&src).unwrap_err().contains("parameter"),
            "{:?}",
            check_src(&src)
        );
    }

    #[test]
    fn lambda_return_type_mismatch_is_rejected() {
        let src =
            format!("{TWICE}fn main() -> Int64 {{ let a = twice([1], |x| x > 0)  return 0 }}");
        let e = check_src(&src).unwrap_err();
        assert!(
            e.contains("returns Bool") || e.contains("expects it to return"),
            "{e}"
        );
    }

    #[test]
    fn cannot_assign_to_captured_binding() {
        let src = format!(
            "{TWICE}fn main() -> Int64 {{ let mut c = 0  let a = twice([1], |x| {{ c = c + x  return c }})  return 0 }}"
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
             fn main() -> Int64 { let name = \"hi\"  let r = apply(name, |x| { drop name  return x })  return 0 }";
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
             fn main() -> Int64 { let name = \"hi\"  let r = apply(name, |x| { let z = take(name)  return x })  return 0 }";
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
            "{TWICE}fn main() -> Int64 {{ let a = twice([1], |x| twice([x], |y| y + 1).length)  return 0 }}"
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
             fn main() -> Int64 {{ let a = outer([1, 2], |x| x + 1)  return 0 }}"
        );
        assert!(check_src(&src).is_ok(), "{:?}", check_src(&src));
    }

    #[test]
    fn generic_map_infers_return_type() {
        let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []\n\
             for x in xs { out.push(f(x)) }\n\
             return out }\n\
             fn main() -> Int64 { let ys: Array<Int64> = [1, 2]  let zs = map(ys, |x| x > 0)  return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn lambda_reading_module_state_poisons_spawn() {
        // A lambda that reads a global makes the enclosing function non-spawn-safe.
        let src = "let g: Int64 = 5\n\
             fn apply(x: Int64, f: fn(Int64) -> Int64) -> Int64 { return f(x) }\n\
             fn worker(x: Int64) -> Int64 { return apply(x, |y| y + g) }\n\
             fn main() -> Int64 { let t = spawn worker(1)  return t.join() }";
        assert!(
            check_src(src).unwrap_err().contains("not allowed"),
            "{:?}",
            check_src(src)
        );
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

    // ---- Map<String, V> (RFC-0028) -------------------------------------

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

    #[test]
    fn map_rejects_non_string_key() {
        let src = "fn f(m: Map<Int64, Int64>) -> Int64 { return 0 }\n\
                   fn main() -> Int64 { return 0 }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("Map` key must be `String`"), "{e}");
    }

    #[test]
    fn map_allows_validated_string_key() {
        // A validated string type resolves to `String`, so it is a legal key.
        let src = "type Name = String where value.length >= 1\n\
                   fn f(m: Map<Name, Int64>) -> Int64 { return 0 }\n\
                   fn main() -> Int64 { return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn map_lookup_is_option() {
        // `m[k]` is `Option<V>` — matching `Some/None` is required to read `V`.
        let src = "fn main() -> Int64 {\n\
             let mut m: Map<String, Int64> = [:]\n\
             m[\"a\"] = 1\n\
             return m[\"a\"] }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("Option"), "{e}");
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
    fn map_value_type_must_match() {
        let src = "fn main() -> Int64 {\n\
             let mut m: Map<String, Int64> = [:]\n\
             m[\"a\"] = true\n\
             return 0 }";
        assert!(check_src(src).is_err());
    }

    #[test]
    fn map_alias_is_codable_by_name() {
        let src = "type M = Map<String, Int64>\n\
             fn main() -> Int64 {\n\
             let v = fromJson(M, \"{}\")\n\
             print(jsonSchema(M))\n\
             return 0 }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    // ---- `if` as an expression (RFC-0030) --------------------------------

    #[test]
    fn if_expression_typechecks_in_let_init() {
        let src = "fn main() -> Int64 {\n\
             let x = if true { 1 } else { 2 }\n\
             return x }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    #[test]
    fn if_expression_needs_an_else() {
        // A missing `else` in expression position names the totality rule (the
        // statement form still allows a missing else — that path is untouched).
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
    fn if_expression_condition_must_be_bool() {
        let src = "fn main() -> Int64 {\n\
             let x = if 3 { 1 } else { 2 }\n\
             return x }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("condition must be Bool"), "{e}");
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
        // The statement form is untouched: no `else`, statements inside, no value.
        let src = "fn main() -> Int64 {\n\
             let mut x = 0\n\
             if true { x = 1 }\n\
             return x }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }

    // ---- SmallArray<T, N> (RFC-0056) --------------------------------------

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
        // Only `SmallArray` consumes an integer type argument in v1.
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
    fn smallarray_literal_longer_than_capacity_is_rejected() {
        // The capacity is known, so a literal exceeding it cannot fit inline.
        let src = "fn main() -> Int64 {\n\
             let xs: SmallArray<Int64, 2> = [1, 2, 3]\n\
             return xs.length }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("SmallArray<_, 2>"), "{e}");
    }

    #[test]
    fn smallarray_at_a_contract_boundary_is_a_named_error() {
        // `SmallArray` is not part of the JSON codec closure; using it where a
        // contract type is expected is a named checker error, not a silent hole.
        let src = "fn main() -> Int64 {\n\
             let xs: SmallArray<Int64, 4> = [1, 2]\n\
             let s = toJson(xs)\n\
             return xs.length }";
        let e = check_src(src).unwrap_err();
        assert!(
            e.contains("SmallArray") && e.contains("codable"),
            "{e}"
        );
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
}
