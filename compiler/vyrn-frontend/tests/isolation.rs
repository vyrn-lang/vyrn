//! The spawn-isolation rule of RFC-0004 §Q4, over the whole pipeline —
//! RFC-0125 §3 M6, the isolation slice.
//!
//! These tests were unit tests in `checker.rs`, because the rule was two
//! fixpoints over the AST call graph and `checker::check` answered them. The
//! rule is the effect judgment's now (`vyrn_lower::effects::spawn_refusals`),
//! and the judgment reads the named core, which does not exist while the
//! checker is still deciding a node's type. So the refusal is stated after the
//! check, by `vyrn_frontend::check_and_synthesize`, and a test of it has to run
//! the pipeline that installs the judgment. That is why they are HERE: an
//! integration test may dev-depend on `vyrn-lower`, and a unit test inside
//! `vyrn-frontend` may not.
//!
//! Every claim is the one the unit test made. The words moved once, where the
//! record says so: the refusal names the effects it found rather than saying
//! "does I/O or touches shared mutable state" about all of them.

use vyrn_frontend::{ast::Program, checker, lexer::lex, parser::parse};

/// A source, checked the way a driver checks it: the generation engine and the
/// judgments installed, then `check_and_synthesize`, which is where the
/// isolation rule is stated. `Err` is the first diagnostic, rendered.
fn check_src(s: &str) -> Result<(), String> {
    vyrn_genwasm::install();
    vyrn_lower::install();
    let mut program: Program =
        parse(lex(s).map_err(|e| format!("{e:?}"))?).map_err(|d| d.render())?;
    match vyrn_frontend::check_and_synthesize(&mut program).first() {
        Some(d) => Err(d.render()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A task may release what it owns — RFC-0125 §3 M6, the release slice.
    ///
    /// The spawn rule refused this for the reason its comment gave: "`drop`
    /// reclaims storage the spawning frame may still name". It does not. `a` is
    /// born in this body, no caller ever named it, and the release is the
    /// body's own. The hazard the comment described is a release of something
    /// the frame does NOT own, and RFC-0089 rule 2 refuses that in its own
    /// words — `xs` (line 1) is released although the body does not own it —
    /// with or without a `spawn` anywhere near it.
    #[test]
    fn a_task_may_release_what_it_owns() {
        assert!(check_src(
            "fn work(n: Int64) -> Int64 { let mut a: Array<Int64> = [] \
             a.push(n) let v = a[0] drop a return v } \
             fn main() -> Int64 { let t = spawn work(1); return t.join(); }",
        )
        .is_ok());
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
    fn lambda_reading_module_state_poisons_spawn() {
        // A lambda that reads a global makes the enclosing function non-spawn-safe.
        let src = "let g: Int64 = 5\n\
             fn apply(x: Int64, f: fn(Int64) -> Int64) -> Int64 { return f(x) }\n\
             fn worker(x: Int64) -> Int64 { return apply(x, y -> y + g) }\n\
             fn main() -> Int64 { let t = spawn worker(1)  return t.join() }";
        assert!(
            check_src(src).unwrap_err().contains("not allowed"),
            "{:?}",
            check_src(src)
        );
    }

    #[test]
    fn spawning_through_a_stateful_stored_value_is_rejected() {
        // `work`'s only impurity flows through a stored function value whose
        // source reads module state (RFC-0037). That needed a SECOND fixpoint
        // when the rule was the checker's — the pre-check could not see a call
        // through a value — and it needs nothing now: the judgment resolves a
        // callee that is a name of the body by its TYPE, over the closed set of
        // sources, and the module-state row travels the same edge as any other.
        // So the refusal is the one sentence, not a second one.
        let src = "let mut hits: Int64 = 0\n\
             fn stateful() -> Int64 { return hits }\n\
             fn make() -> fn() -> Int64 { return stateful }\n\
             fn work() -> Int64 { let f = make()  return f() }\n\
             fn main() -> Int64 { let t = spawn work()  return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("does `module-state`"), "{e}");
    }

    /// From `tests/semantics.rs`, where `run` does not install the judgment.
    #[test]
    fn logging_is_forbidden_in_spawned_tasks() {
        let src = "fn work(n: Int64) -> Int64 { let l = logger(\"w\") l.info(\"hi\") return n }                    fn main() -> Int64 { let t = spawn work(1) return t.join() }";
        let e = check_src(src).unwrap_err();
        assert!(e.contains("isolated (pure)"), "{e}");
    }

    #[test]
    fn spawning_through_an_isolated_stored_value_is_fine() {
        let src = "fn pure() -> Int64 { return 7 }\n\
             fn make() -> fn() -> Int64 { return pure }\n\
             fn work() -> Int64 { let f = make()  return f() }\n\
             fn main() -> Int64 { let t = spawn work()  return t.join() }";
        assert!(check_src(src).is_ok(), "{:?}", check_src(src));
    }
}
