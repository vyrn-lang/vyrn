//! What the loader does, stated by RUNNING the program it linked (RFC-0125 §3
//! M5, the `library-run` row).
//!
//! A link is right when the program it produced answers 42. These tests used to
//! say that inside `src/loader.rs`, through the tree-walking interpreter, which
//! is the one engine that lives in that crate. M5 retires the interpreter, so
//! the run moves to the route every command takes — the direct backend's module
//! in the driver's WASI host.
//!
//! That route is why the tests are HERE and not there. `vyrn-codegen` and
//! `vyrn-cli` depend on `vyrn-frontend`, so a unit test inside `vyrn-frontend`
//! that reaches back for them compiles a second copy of its own crate, and the
//! two `Program` types are different types. An integration test links the real
//! one, so the cycle resolves. The tests that read the loader's insides — the
//! cache entry's format, the nesting counter, the two generator budgets — stay
//! where the insides are.
//!
//! Everything else is unchanged: the same files, the same `MapResolver` standing
//! in for the network, the same claims.

use std::collections::HashMap;
use vyrn_frontend::loader::*;

mod common;
use common::run_compiled;

/// Every runtime module a builtin can inject (RFC-0078), plus the modules those
/// import, plus the two the COMPILED route needs: a builtin the tree-walker
/// answered in Rust is a call on this route, and `std/runtime` is where the body
/// is. Added to every resolver rather than per test: injection is conditional on
/// the mention, so a program that never says `fromJson` links none of them, and a
/// test that does say it should not have to know which files that implies.
const RT_FILES: &[(&str, &str)] = &[
    ("std/json.vyrn", include_str!("../../../std/json.vyrn")),
    (
        "std/jsonread.vyrn",
        include_str!("../../../std/jsonread.vyrn"),
    ),
    (
        "std/jsondec.vyrn",
        include_str!("../../../std/jsondec.vyrn"),
    ),
    ("std/num.vyrn", include_str!("../../../std/num.vyrn")),
    ("std/codecs.vyrn", include_str!("../../../std/codecs.vyrn")),
    ("std/text.vyrn", include_str!("../../../std/text.vyrn")),
    (
        "std/strpred.vyrn",
        include_str!("../../../std/strpred.vyrn"),
    ),
    ("std/hash.vyrn", include_str!("../../../std/hash.vyrn")),
    (
        "std/runtime.vyrn",
        include_str!("../../../std/runtime.vyrn"),
    ),
    ("std/mem.vyrn", include_str!("../../../std/mem.vyrn")),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// [`RT_MODULES`] writes each route's reserved name out in full so
    /// [`routed_builtin`] can return a `&'static str` without composing one on a
    /// path every call expression takes. That redundancy has to be checked, or a
    /// row could name a spelling the decl rename never produces — a call to a
    /// function nobody defines, which every engine would then refuse at the worst
    /// possible moment. Also: no builtin may be claimed by two modules.
    #[test]
    fn every_route_is_spelled_with_its_modules_prefix() {
        let mut seen: Vec<&str> = Vec::new();
        for rt in RT_MODULES {
            assert!(rt.prefix.ends_with('$'), "`{}` must end in `$`", rt.prefix);
            for (builtin, reserved) in rt.routes {
                assert_eq!(
                    *reserved,
                    format!("{}{}", rt.prefix, reserved.trim_start_matches(rt.prefix)),
                    "`{builtin}` names `{reserved}`, which is not `{}`-prefixed",
                    rt.prefix
                );
                assert!(
                    reserved.starts_with(rt.prefix),
                    "`{reserved}` vs `{}`",
                    rt.prefix
                );
                assert!(!seen.contains(builtin), "`{builtin}` is routed twice");
                seen.push(builtin);
                assert_eq!(routed_builtin(builtin), Some(*reserved));
            }
            for b in rt.desugared {
                assert!(
                    routed_builtin(b).is_none(),
                    "`{b}` is a desugar, not a route"
                );
            }
        }
        // A route is matched on the CALL NAME, before any type is known, so it
        // only means the builtin while no declaration may carry the name. This
        // is the check `movecheck::every_view_and_sink_name_is_reserved` makes
        // for its list and `parser::every_method_builtin_is_reserved_or_shadowable`
        // makes for its own — the same hazard, a third pass. A route spelled
        // with the method-form `@` prefix is unspellable and needs no guard.
        for rt in RT_MODULES {
            for (builtin, _) in rt.routes {
                assert!(
                    builtin.starts_with('@') || vyrn_frontend::checker::RESERVED.contains(builtin),
                    "`{builtin}` is routed to a std function but is not reserved, so a \
                     user declaration of that name would be silently unreachable"
                );
            }
        }
        assert!(routed_builtin("print").is_none());
        assert!(
            routed_builtin("lineAt").is_none(),
            "`lineAt` keeps its interpreter cache"
        );
        // RFC-0094 M2: a routed builtin with a FREE spelling needs no route — an
        // import does the same work and costs the compiler nothing. Eleven rows
        // left this table; `@charCount` is what remains, because a method-only
        // name has no spelling an import can bring into scope.
        for gone in vyrn_frontend::checker::MOVED_TO_STD {
            assert!(
                routed_builtin(gone.0).is_none(),
                "`{}` moved to `{}`; a route for it would shadow the import",
                gone.0,
                gone.1
            );
        }
        let routes: Vec<&str> = RT_MODULES
            .iter()
            .flat_map(|rt| rt.routes)
            .map(|(b, _)| *b)
            .collect();
        assert_eq!(routes, vec!["@charCount"]);
    }

    /// RFC-0081 M2: [`F64_STR`] is a name two backends emit a call to, so the
    /// module it names has to be in the table and its prefix has to be the one it
    /// is spelled with. Neither backend can check that for itself — a mismatch is
    /// a link error in a program that formats a float, which is most of them.
    #[test]
    fn the_float_formatter_is_std_nums() {
        let num = RT_MODULES
            .iter()
            .find(|rt| rt.spec == "std/num")
            .expect("std/num is linked");
        assert!(
            F64_STR.starts_with(num.prefix),
            "`{F64_STR}` vs `{}`",
            num.prefix
        );
        // Both spellings that reach it, and neither is a route: the float case is
        // one case of a type-directed builtin.
        assert!(num.desugared.contains(&"@str") && num.desugared.contains(&"print"));
        assert!(routed_builtin(F64_STR).is_none());
    }

    /// RFC-0125 §3 M6 (the third judgment's fifth slice): [`STRING_FAULT`] is the
    /// same shape and needs the same guard — three engines emit a call to it, and
    /// a mismatch between the name and the module's prefix is a link error in
    /// every program that makes a `String` from bytes.
    #[test]
    fn the_string_check_is_std_texts() {
        let text = RT_MODULES
            .iter()
            .find(|rt| rt.spec == "std/text")
            .expect("std/text is linked");
        assert!(
            STRING_FAULT.starts_with(text.prefix),
            "`{STRING_FAULT}` vs `{}`",
            text.prefix
        );
        // The mention that links the module, and it is a desugar rather than a
        // route: only the check moved, the build stayed with each engine.
        assert!(text.desugared.contains(&"stringFromBytes"));
        assert!(routed_builtin("stringFromBytes").is_none());
    }

    pub(super) fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(
            entries
                .iter()
                .chain(RT_FILES.iter())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    pub(super) fn opts() -> LoadOptions {
        LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        }
    }

    fn run_multi(root: &str, files: &[(&str, &str)]) -> Result<i64, String> {
        let files: Vec<(&str, &str)> = files
            .iter()
            .copied()
            .chain(RT_FILES.iter().copied())
            .collect();
        let files = &files[..];
        let mut program = load(root, "main.vyrn", &opts(), &map(files)).map_err(|ds| {
            ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
                "
",
            )
        })?;
        // `check_and_synthesize` rather than a bare check: since RFC-0078 M2b/M3 a
        // linked program is not runnable until the JSON builtins' generated Vyrn is
        // in it, and `loader::load` deliberately stops at the link. A test that ran
        // the bare check would fail at the call site with "no decoder", which is a
        // true statement about a program nobody finished assembling.
        let diags = vyrn_frontend::check_and_synthesize(&mut program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        run_compiled(&program)
    }

    fn load_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn imports_functions_and_types_across_modules() {
        let lib = "export fn double(x: Int64) -> Int64 { return x * 2 } \
                   export type Age = Int64 where value >= 18 \
                   fn hidden() -> Int64 { return 0 }";
        let root = "import { double, Age } from \"./lib\" \
                    fn main() -> Int64 { let a: Age = 21 return double(a) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 42);
    }

    #[test]
    fn import_alias_resolves_to_the_original_decl() {
        // RFC-0022: `getUser as fetchUser` — the alias is the local name and
        // resolves to the original function/type in the flat namespace.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 10 } \
                   export type Age = Int64 where value >= 0";
        let root = "import { getUser as fetchUser, Age as Years } from \"./lib\" \
                    fn main() -> Int64 { let y: Years = 3 return fetchUser(y) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 30);
    }

    #[test]
    fn import_alias_hides_the_original_name() {
        // The original name is not brought into scope by an aliased import.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id }";
        let root = "import { getUser as fetchUser } from \"./lib\" \
                    fn main() -> Int64 { return getUser(1) }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("getUser"), "{e}");
    }

    #[test]
    fn import_alias_clashing_with_a_local_decl_is_an_error() {
        let lib = "export fn getUser(id: Int64) -> Int64 { return id }";
        let root = "import { getUser as fetchUser } from \"./lib\" \
                    fn fetchUser() -> Int64 { return 0 } \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("clashes with a top-level declaration"), "{e}");
    }

    #[test]
    fn import_alias_lets_a_stub_share_the_real_name() {
        // The co-naming (RPC stub) pattern: the importing module defines its own
        // `getUser`, importing the real one under an alias it forwards to.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 100 }";
        let root = "import { getUser as getUserReal } from \"./lib\" \
                    fn getUser(id: Int64) -> Int64 { return getUserReal(id) + 1 } \
                    fn main() -> Int64 { return getUser(2) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 201);
    }

    #[test]
    fn aliased_enum_import_brings_variants_under_own_names() {
        // Importing an enum under an alias still brings its variants by their
        // own (unaliased) names (RFC-0022).
        let lib = "export type Color = | Red | Green | Blue";
        let root = "import { Color as Hue } from \"./lib\" \
                    fn pick(h: Hue) -> Int64 { return match h { Red => 1, Green => 2, Blue => 3 } } \
                    fn main() -> Int64 { let c: Hue = Green return pick(c) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 2);
    }

    #[test]
    fn validated_type_auto_validates_across_modules() {
        let lib = "export type Age = Int64 where value >= 18";
        let root = "import { Age } from \"./lib\" \
                    fn mk(n: Int64) -> Age { return n } \
                    fn main() -> Int64 { let a = mk(5) return 0 }";
        let e = run_multi(root, &[("lib.vyrn", lib)]).unwrap_err();
        assert!(e.contains("validation failed for `Age`"), "{e}");
    }

    #[test]
    fn importing_a_private_name_is_an_error() {
        let lib = "fn secret() -> Int64 { return 1 }";
        let root = "import { secret } from \"./lib\" \
                    fn main() -> Int64 { return secret() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not exported"), "{e}");
    }

    #[test]
    fn importing_a_missing_name_is_an_error() {
        let root = "import { nope } from \"./lib\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("lib.vyrn", "export fn f() -> Int64 { return 1 }")]);
        assert!(e.contains("does not define `nope`"), "{e}");
    }

    #[test]
    fn using_a_foreign_name_without_importing_it_is_an_error() {
        // `helper` exists (exported, even) in lib, but main never imported it.
        let lib = "export fn helper() -> Int64 { return 1 } \
                   export fn wanted() -> Int64 { return 2 }";
        let root = "import { wanted } from \"./lib\" \
                    fn main() -> Int64 { return wanted() + helper() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn std_result_and_option_imports_are_validated_noops() {
        // RFC-0062: importing the ambient builtins by name from `std/result` /
        // `std/option` is a no-op — no file is loaded, the names keep resolving
        // to the builtins, and the program runs exactly as it would ambiently.
        let root = "import { Result, Ok, Err } from \"std/result\" \
                    import { Option, Some, None } from \"std/option\" \
                    fn find(x: Int64) -> Result<Int64, String> { \
                        if x > 0 { return Ok(x) } return Err(\"neg\") } \
                    fn opt(x: Bool) -> Option<Int64> { \
                        if x { return Some(7) } return None } \
                    fn main() -> Int64 { \
                        let r = match find(5) { Ok(v) => v, Err(e) => 0 } \
                        let o = match opt(true) { Some(n) => n, None => 0 } \
                        return r + o }";
        assert_eq!(run_multi(root, &[]).unwrap(), 12);
    }

    #[test]
    fn std_result_ambient_use_without_the_import_still_works() {
        // The import is opt-in style, not a requirement: the same program runs
        // without importing the names (they were always ambient).
        let root = "fn find(x: Int64) -> Result<Int64, String> { \
                        if x > 0 { return Ok(x) } return Err(\"neg\") } \
                    fn main() -> Int64 { return match find(5) { Ok(v) => v, Err(e) => 0 } }";
        assert_eq!(run_multi(root, &[]).unwrap(), 5);
    }

    #[test]
    fn std_result_unknown_export_is_rejected() {
        let root = "import { Result, Foo } from \"std/result\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("std/result has no export `Foo`"), "{e}");
    }

    #[test]
    fn std_option_rejects_a_result_only_export() {
        // Each module's export list is fixed and distinct — `Result` is not an
        // export of `std/option`.
        let root = "import { Option, Result } from \"std/option\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("std/option has no export `Result`"), "{e}");
    }

    #[test]
    fn std_result_namespace_import_is_rejected() {
        // `import * as r from "std/result"` would create a second spelling
        // (`r.Ok`) for a builtin — rejected.
        let root = "import * as r from \"std/result\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[]);
        assert!(e.contains("cannot be imported as a namespace"), "{e}");
    }

    #[test]
    fn import_cycles_are_errors() {
        let a = "import { b } from \"./b\" export fn a() -> Int64 { return 1 }";
        let b = "import { a } from \"./a\" export fn b() -> Int64 { return 2 }";
        let root = "import { a } from \"./a\" fn main() -> Int64 { return a() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("import cycle"), "{e}");
    }

    #[test]
    fn cross_module_name_collisions_are_errors() {
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import { f } from \"./b\" \
                    fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(
            e.contains("`f` is declared by both `a.vyrn` and `b.vyrn`"),
            "{e}"
        );
    }

    #[test]
    fn a_module_pair_collision_is_one_error_that_names_the_fix() {
        // Two modules sharing top-level names is ONE mistake. It used to be
        // reported once per shared name — including names the user never wrote —
        // at the foreign declaration's line, against the root file, and then a
        // fourth time as "`f` is not defined in `b.vyrn`", which is false.
        let a = "export fn f() -> Int64 { return 1 } \
                 export fn g() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 } \
                 export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \n\
                    import { f } from \"./b\" \n\
                    fn main() -> Int64 { return f() }";
        let ds = match load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("a.vyrn", a), ("b.vyrn", b)]),
        ) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds,
        };
        let all = ds
            .iter()
            .map(|d| format!("{:?} {} {}", d.file, d.line, d.message))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(ds.len(), 1, "{all}");
        let d = &ds[0];
        // The user's own file, at the import they wrote — never a line borrowed
        // from the module the name collided with.
        assert_eq!(d.file, None, "{all}");
        assert_eq!(d.line, 2, "{all}");
        assert!(d.message.contains("`f` is declared by both"), "{all}");
        let note = d.note.as_deref().unwrap_or("");
        assert!(note.contains("import * as b from \"./b\""), "{note}");
        // `g` collides too, but the user never wrote it: a note, not an error.
        assert!(note.contains("`g` collides the same way"), "{note}");
        assert!(!all.contains("is not defined in"), "{all}");
        assert!(!all.contains("imported twice"), "{all}");
    }

    #[test]
    fn the_same_module_imported_twice_still_says_so() {
        // The suppression above is only for the same name from DIFFERENT modules
        // (which the pair collision covers). A genuine double binding still errors.
        let a = "export fn f() -> Int64 { return 1 } export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import { f } from \"./a\" \
                    fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("a.vyrn", a)]);
        assert!(e.contains("`f` is imported twice"), "{e}");
        let root = "import { f as x } from \"./a\" \
                    import { g as x } from \"./a\" \
                    fn main() -> Int64 { return x() }";
        let e = load_err(root, &[("a.vyrn", a)]);
        assert!(e.contains("`x` is imported twice"), "{e}");
    }

    #[test]
    fn a_collision_the_user_did_not_import_is_still_one_error() {
        // Neither name is imported from both modules, so nothing is "imported
        // twice" — but the flat namespace still cannot hold two `g`s, and the
        // user has to hear about it exactly once, with the fix.
        let a = "export fn f() -> Int64 { return 1 } \
                 export fn g() -> Int64 { return 1 }";
        let b = "export fn h() -> Int64 { return 2 } \
                 export fn g() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \n\
                    import { h } from \"./b\" \n\
                    fn main() -> Int64 { return f() + h() }";
        let ds = match load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("a.vyrn", a), ("b.vyrn", b)]),
        ) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds,
        };
        assert_eq!(ds.len(), 1, "{ds:?}");
        assert!(ds[0].message.contains("`g` is declared by both"), "{ds:?}");
    }

    #[test]
    fn a_namespace_call_is_not_a_bare_use_of_an_aliased_original() {
        // An aliased import hides the original name, and using it directly is an
        // error. `bt.routes()` is not that use: it is a namespace call to another
        // module entirely, which the method sugar parses as `routes(bt)` — and the
        // check counted that member name as a bare reference. The advice it gave
        // (`use pageRoutes`) would have produced `bt.pageRoutes()`, which names
        // nothing.
        let a = "export fn route() -> Int64 { return 1 } \
                 export fn routes() -> Int64 { return 2 }";
        let b = "export fn routes() -> Int64 { return 3 }";
        let root = "import { route, routes as pageRoutes } from \"./a\" \
                    import * as bt from \"./b\" \
                    fn main() -> Int64 { return route() + pageRoutes() + bt.routes() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]), Ok(6));
    }

    #[test]
    fn a_real_bare_use_of_an_aliased_original_is_still_reported() {
        // The other direction: the namespace call must not SATISFY the check
        // either. `routes()` here is the hidden name, written bare, and it is
        // still an error however many namespace calls share its spelling.
        let a = "export fn routes() -> Int64 { return 2 }";
        let b = "export fn routes() -> Int64 { return 3 }";
        let root = "import { routes as pageRoutes } from \"./a\" \
                    import * as bt from \"./b\" \
                    fn main() -> Int64 { return routes() + bt.routes() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(
            e.contains("`routes` is not in scope — it was imported as `pageRoutes`"),
            "{e}"
        );
        // One cause, one error: the collision diagnostics next door do not pile on.
        assert!(!e.contains("is declared by both"), "{e}");
    }

    #[test]
    fn a_namespace_import_resolves_the_collision() {
        // The fix the diagnostic names has to actually work.
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn f() -> Int64 { return 2 }";
        let root = "import { f } from \"./a\" \
                    import * as b from \"./b\" \
                    fn main() -> Int64 { return f() + b.f() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 3);
    }

    #[test]
    fn importing_an_enum_brings_its_variants() {
        let lib = "export type Shape = | Circle(Int64) | Dot \
                   export fn area(s: Shape) -> Int64 { \
                       return match s { Circle(r) => 3 * r * r, Dot => 0 } }";
        let root = "import { Shape, area } from \"./lib\" \
                    fn main() -> Int64 { return area(Circle(2)) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 12);
    }

    #[test]
    fn importing_a_protocol_brings_its_methods() {
        let lib = "export protocol Loud { fn shout(self) -> Int64 } \
                   impl Loud for Int64 { fn shout(self) -> Int64 { return self * 10 } }";
        let root = "import { Loud } from \"./lib\" \
                    fn main() -> Int64 { return 4.shout() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 40);
    }

    #[test]
    fn std_prefix_resolves_against_the_std_root() {
        let m = "export fn twice(x: Int64) -> Int64 { return x + x }";
        let root = "import { twice } from \"std/math\" \
                    fn main() -> Int64 { return twice(21) }";
        assert_eq!(run_multi(root, &[("std/math.vyrn", m)]).unwrap(), 42);
    }

    #[test]
    fn transitive_imports_load_once() {
        // Both a and b import shared; the diamond loads it once (no collision
        // with itself).
        let shared = "export fn one() -> Int64 { return 1 }";
        let a = "import { one } from \"./shared\" export fn a() -> Int64 { return one() + 10 }";
        let b = "import { one } from \"./shared\" export fn b() -> Int64 { return one() + 20 }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        assert_eq!(
            run_multi(
                root,
                &[("shared.vyrn", shared), ("a.vyrn", a), ("b.vyrn", b)]
            )
            .unwrap(),
            32
        );
    }

    #[test]
    fn non_root_logging_config_is_an_error() {
        let lib = "logging { level: trace } export fn f() -> Int64 { return 1 }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(
            e.contains("only the root module may configure `logging"),
            "{e}"
        );
    }

    #[test]
    fn non_root_module_state_is_legal_via_accessors() {
        // RFC-0029: a top-level `let` is legal in any module; cross-module
        // access goes through exported accessor functions. The imported module
        // owns `count`; the root reads it through `f`.
        let lib = "let mut count = 7 export fn f() -> Int64 { return count }";
        let root = "import { f } from \"./lib\" fn main() -> Int64 { return f() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 7);
    }

    #[test]
    fn diamond_imports_share_one_state_instance() {
        // RFC-0029: `left` and `right` both import the same `store`; the loader
        // resolves them to ONE module identity, so both mutate the single shared
        // `count`. The root observes 2 — a single instance across the diamond.
        let store = "let mut count: Int64 = 0 \
                     export fn tally() -> Int64 { return count } \
                     export fn bump() { count = count + 1 }";
        let left = "import { bump } from \"./store\" export fn l() { bump() }";
        let right = "import { bump } from \"./store\" export fn r() { bump() }";
        let root = "import { tally } from \"./store\" \
                    import { l } from \"./left\" import { r } from \"./right\" \
                    fn main() -> Int64 { l() r() return tally() }";
        assert_eq!(
            run_multi(
                root,
                &[
                    ("store.vyrn", store),
                    ("left.vyrn", left),
                    ("right.vyrn", right)
                ]
            )
            .unwrap(),
            2
        );
    }

    #[test]
    fn initializer_may_read_imported_module_state() {
        // RFC-0029: an initializer may call an imported accessor — the imported
        // module initializes first (post-order), so its state is already set.
        let store = "let seed: Int64 = 41 export fn seedVal() -> Int64 { return seed }";
        let root = "import { seedVal } from \"./store\" \
                    let snapshot: Int64 = seedVal() + 1 \
                    fn main() -> Int64 { return snapshot }";
        assert_eq!(run_multi(root, &[("store.vyrn", store)]).unwrap(), 42);
    }

    #[test]
    fn spawning_a_cross_module_stateful_fn_is_refused() {
        // RFC-0029 keeps RFC-0013's spawn isolation module-agnostic: a function
        // reaching ANY module's state (here the imported store's) is not
        // spawn-safe, so spawning it is refused.
        let store = "let mut count: Int64 = 0 \
                     export fn bump() -> Int64 { count = count + 1 return count }";
        let root = "import { bump } from \"./store\" \
                    fn worker() -> Int64 { return bump() } \
                    fn main() -> Int64 { let h = spawn worker() return h.join() }";
        let e = run_multi(root, &[("store.vyrn", store)]).unwrap_err();
        assert!(
            e.contains("is not allowed") && e.contains("isolated"),
            "{e}"
        );
    }

    #[test]
    fn two_modules_with_a_private_same_named_helper_link_cleanly() {
        // RFC-0046 §3: a non-exported decl is invisible outside its module, so
        // two modules may each carry a private `helper` without colliding — the
        // linker auto-renames the non-exported decls.
        let a = "fn helper() -> Int64 { return 1 } \
                 export fn aVal() -> Int64 { return helper() }";
        let b = "fn helper() -> Int64 { return 2 } \
                 export fn bVal() -> Int64 { return helper() }";
        let root = "import { aVal } from \"./a\" \
                    import { bVal } from \"./b\" \
                    fn main() -> Int64 { return aVal() + bVal() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 3);
    }

    #[test]
    fn a_program_that_imports_a_program_keeps_its_entry_point() {
        // Every file in `examples/` is a program, so importing one — the website
        // imports `examples/herofield.vyrn` to hash what it prints — put a
        // SECOND `main` in the link. `main` is not exported, so the name-privacy
        // rename above minted a fresh symbol for both of them, and the program
        // was left with no `main` at all: `call to unknown function \`main\``,
        // naming no file and no line. The root's entry keeps its spelling; the
        // imported one, which nothing can reach, is the one that renames.
        let lib = "export fn libValue() -> Int64 { return 5 } \
                   fn main() -> Int64 { return 99 }";
        let root = "import { libValue } from \"./lib\" \
                    fn main() -> Int64 { return libValue() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 5);
    }

    #[test]
    fn local_may_shadow_a_private_std_internal_name() {
        // RFC-0046 §3 (the vlog `pad2` bug): `std/time`'s private `pad2` forced a
        // consumer to rename its own `pad2`. A non-exported foreign name no longer
        // consumes the consumer's namespace — the local `pad2` compiles unchanged,
        // and each module's `pad2` resolves to its own.
        let lib = "fn pad2(n: Int64) -> Int64 { return n } \
                   export fn tick() -> Int64 { return pad2(7) }";
        let root = "import { tick } from \"./lib\" \
                    fn pad2(n: Int64) -> Int64 { return n + 100 } \
                    fn main() -> Int64 { return tick() + pad2(0) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 7 + 100);
    }

    #[test]
    fn module_state_assignment_survives_a_never_imported_foreign_namesake() {
        // The shelf `filter` bug: `arrays` exports `filter`, `ui` imports only
        // `includes` from it, so `arrays::filter` is LINKED but never imported
        // here — enough to force the name-privacy rename of this module's
        // same-named state. The rename has to reach the assignment TARGETS, not
        // just the reads, or the write side names a decl that no longer exists.
        let arrays = "export fn filter() -> Int64 { return 99 } \
                      export fn includes() -> Int64 { return 1 }";
        let ui = "import { includes } from \"./arrays\" \
                  export fn tag() -> Int64 { return includes() }";
        let root = "import { tag } from \"./ui\" \
                    let mut filter: Int64 = 0 \
                    let mut includes: Array<Int64> = [0] \
                    fn main() -> Int64 { \
                        filter = 7 \
                        includes[0] = 2 \
                        return filter + includes[0] + tag() \
                    }";
        assert_eq!(
            run_multi(root, &[("arrays.vyrn", arrays), ("ui.vyrn", ui)]).unwrap(),
            7 + 2 + 1
        );
    }

    #[test]
    fn global_name_collides_with_a_function() {
        // A global may not share a name with any other top-level declaration.
        let lib = "export fn tally() -> Int64 { return 1 }";
        let root = "import { tally } from \"./lib\" \
                    let tally = 0 \
                    fn main() -> Int64 { return tally }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("`tally` is declared by both"), "{e}");
    }

    // ---- flat-namespace local shadowing (dogfood BUG 2) ------------------
    // A local/param/loop/lambda/match binding whose name equals ANOTHER linked
    // module's export must never be mis-resolved as an un-imported foreign
    // reference. The visibility scan is scope-aware; locals bind before imports.

    #[test]
    fn local_let_shadows_a_foreign_export_of_the_same_name() {
        // The shelf shape: module `ui` has a local `t`; module `strings` exports
        // `t`. Both are linked (root imports from each). `ui`'s local `t` is NOT
        // a reference to `strings`'s `t`.
        let strings = "export fn t() -> Int64 { return 99 } \
                       export fn label() -> Int64 { return 1 }";
        let ui = "import { label } from \"./strings\" \
                  export fn render() -> Int64 { let t = 5 return t + label() }";
        let root = "import { render } from \"./ui\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { return render() + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("ui.vyrn", ui)]).unwrap(),
            5 + 1 + 99
        );
    }

    #[test]
    fn param_shadows_a_foreign_global_of_the_same_name() {
        // The shelf `loc` shape: a generated/library fn's PARAM `loc` shadows the
        // root's module-state global `loc`.
        let lib = "export fn greet(loc: Int64) -> Int64 { return loc + 1 }";
        let root = "import { greet } from \"./lib\" \
                    let mut loc = 10 \
                    fn main() -> Int64 { return greet(loc) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 11);
    }

    #[test]
    fn for_loop_var_shadows_a_foreign_export() {
        // The `std/ui` loop-var `t` shape.
        let strings = "export fn t() -> Int64 { return 7 }";
        let lib = "export fn total(xs: Array<Int64>) -> Int64 { \
                       let mut sum = 0 for t in xs { sum = sum + t } return sum }";
        let root = "import { total } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { let xs: Array<Int64> = [1, 2, 3] return total(xs) + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("lib.vyrn", lib)]).unwrap(),
            6 + 7
        );
    }

    #[test]
    fn match_bind_shadows_a_foreign_export() {
        let lib = "export fn why() -> Int64 { return 100 }";
        let root = "import { why } from \"./lib\" \
                    fn pick(x: Result<Int64, Int64>) -> Int64 { \
                        return match x { Ok(why) => why, Err(e) => e } } \
                    fn main() -> Int64 { return pick(Ok(3)) + why() }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 3 + 100);
    }

    #[test]
    fn a_genuinely_unimported_foreign_name_still_errors() {
        // Guard against over-fixing: a bare use that is NOT shadowed by any local
        // must still be flagged.
        let lib = "export fn helper() -> Int64 { return 1 } \
                   export fn wanted() -> Int64 { return 2 }";
        let root = "import { wanted } from \"./lib\" \
                    fn main() -> Int64 { return wanted() + helper() }";
        let e = load_err(root, &[("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn a_local_shadow_does_not_hide_a_later_genuine_reference() {
        // `t` is a local only inside the `for`; a use of `t` OUTSIDE that scope is
        // still a genuine foreign reference — and here `lib` never imported it, so
        // it must error even though a same-named local exists elsewhere in the fn.
        let strings = "export fn t() -> Int64 { return 7 }";
        let lib = "export fn f(xs: Array<Int64>) -> Int64 { \
                       let mut s = 0 for t in xs { s = s + t } return s + t() }";
        let root = "import { f } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { let xs: Array<Int64> = [1] return f(xs) + t() }";
        let e = load_err(root, &[("strings.vyrn", strings), ("lib.vyrn", lib)]);
        assert!(e.contains("not imported here"), "{e}");
    }

    #[test]
    fn namespaced_module_local_shadows_another_modules_export() {
        // Interaction with RFC-0027: a namespaced module `ui` has a local `t`
        // while `strings` (also linked) exports `t`.
        let strings = "export fn t() -> Int64 { return 40 }";
        let ui = "export fn render() -> Int64 { let t = 2 return t }";
        let root = "import * as ui from \"./ui\" \
                    import { t } from \"./strings\" \
                    fn main() -> Int64 { return ui.render() + t() }";
        assert_eq!(
            run_multi(root, &[("strings.vyrn", strings), ("ui.vyrn", ui)]).unwrap(),
            2 + 40
        );
    }

    #[test]
    fn co_named_stub_with_a_local_shadowing_another_export() {
        // Interaction with RFC-0022 co-naming AND local shadowing at once: the
        // root stubs `getUser` (co-naming) and also has a local `t` shadowing
        // `strings`'s exported `t`.
        let lib = "export fn getUser(id: Int64) -> Int64 { return id * 100 }";
        let strings = "export fn t() -> Int64 { return 5 }";
        let root = "import { getUser as getUserReal } from \"./lib\" \
                    import { t } from \"./strings\" \
                    fn getUser(id: Int64) -> Int64 { let t = 1 return getUserReal(id) + t } \
                    fn main() -> Int64 { return getUser(2) + t() }";
        assert_eq!(
            run_multi(root, &[("lib.vyrn", lib), ("strings.vyrn", strings)]).unwrap(),
            201 + 5
        );
    }

    #[test]
    fn generated_module_param_shadows_a_foreign_export() {
        // The `.vyx` `cls` shape: a generator-synthesized module has a fn whose
        // PARAM `cls` shadows `std/html`'s exported `cls`, both linked together.
        let html = "export fn cls(s: String) -> String { return s.copy() }";
        let gen = "export gen fn widgets(dir: String) -> String { \
                       return \"export fn item(cls: String) -> String { return cls.copy() }\" }";
        let root = "import { cls } from \"./html\" \
                    import { widgets } from \"./gen\" \
                    import { item } from widgets(\"./w\") \
                    fn main() -> Int64 { let a = cls(\"x\") let b = item(\"y\") return 0 }";
        // Links html (exports `cls`) + the synthesized module (param `cls`) with
        // no false \"cls not imported\" error.
        assert_eq!(
            run_multi(root, &[("html.vyrn", html), ("gen.vyrn", gen)]).unwrap(),
            0
        );
    }

    #[test]
    fn a_local_shadow_survives_a_name_privacy_rename() {
        // The rewrite of a module's OWN references after a name-privacy rename
        // (RFC-0046 §3) was scope-unaware: a local `let flag` kept its name while
        // every READ and WRITE of `flag` after it was rewritten to the renamed
        // GLOBAL — in plain statements and inside lambda bodies alike.
        let lib = "let mut flag = 9 \
                   export fn flip() -> Int64 { let flag = 1 return flag } \
                   export fn lam() -> Int64 { let g: fn(Int64) -> Int64 = flag -> flag + 1 return g(10) } \
                   export fn peek() -> Int64 { return flag }";
        let root = "import { flip, lam, peek } from \"./lib\" \
                    let mut flag = 7 \
                    fn main() -> Int64 { let f = flip() return f + lam() + peek() + flag }";
        // flip reads its LOCAL (1), lam's parameter is untouched by the global's
        // rename (11), peek reads lib's own state and main reads root's (9 + 7).
        assert_eq!(
            run_multi(root, &[("lib.vyrn", lib)]).unwrap(),
            1 + 11 + 9 + 7
        );
    }

    #[test]
    fn a_shadowed_use_of_an_aliased_original_is_not_reported() {
        // The hidden-original check (RFC-0022) scanned references unscoped: a
        // LOCAL named like an aliased import's original satisfied it and failed
        // the load though nothing referenced the hidden foreign name.
        let ui = "export fn render() -> Int64 { return 1 }";
        let root = "import { render as draw } from \"./ui\" \
                    fn helper() -> Int64 { let render = 2 return render } \
                    fn main() -> Int64 { return helper() }";
        assert_eq!(run_multi(root, &[("ui.vyrn", ui)]).unwrap(), 2);
    }

    // KNOWN LIMITATION (documented, not forgotten): with two same-named methods
    // on two linked protocols, the loader accepts and the checker picks by the
    // receiver's impl table, but the lowered symbol still mangles from a
    // last-writer protocol name — `Q__A__area` vs the registered `P__A__area`.
    // Closing it needs one protocol choice carried into symbol mangling.
    #[test]
    #[ignore = "same-named methods on two linked protocols: checker picks P, symbol mangling still says Q"]
    fn a_shared_method_name_resolves_to_the_imported_protocol() {
        // Two linked protocols may declare the same method name. Last-writer-wins
        // made the LATER-loaded module own `render`, so a call resolving to the
        // EARLIER (imported) protocol was rejected as unimported — purely because
        // of import order. The loader accepts when the receiver's own module is
        // imported; the checker picks between the candidates by impl table.
        let a = "export protocol P { fn area(self) -> Int64 } \
                 export type A = { v: Int64 } \
                 impl P for A { fn area(self) -> Int64 { return self.v } }";
        let b = "export protocol Q { fn area(self) -> Int64 } \
                 export type B = { v: Int64 } \
                 impl Q for B { fn area(self) -> Int64 { return self.v + 1 } }";
        let root = "import { A } from \"./a\" \
                    import { B } from \"./b\" \
                    fn main() -> Int64 { let x = A { v: 4 } return x.area() }";
        assert_eq!(run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(), 4);
    }

    #[test]
    fn an_unimported_shared_method_name_lists_every_candidate_module() {
        // The refusal keeps its shape when the receiver's own module is NOT
        // imported — reached through a third module that links the protocols
        // and provides the impl, so every candidate module is named instead of
        // guessing one.
        let a = "export protocol P { fn area(self) -> Int64 }";
        let b = "export protocol Q { fn area(self) -> Int64 }";
        let c = "import { P } from \"./a\" \
                 import { Q } from \"./b\" \
                 export type C = { v: Int64 } \
                 impl P for C { fn area(self) -> Int64 { return self.v } }";
        let root = "import { C } from \"./c\" \
                    fn main() -> Int64 { let x = C { v: 1 } return x.area() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b), ("c.vyrn", c)]);
        assert!(e.contains("`area` is defined in"), "{e}");
        assert!(e.contains("a.vyrn") && e.contains("b.vyrn"), "{e}");
    }

    #[test]
    fn generated_importer_survives_an_at_in_the_path() {
        // The banner used to split on the last " at ", so an importer whose path
        // contained " at " was truncated mid-directory and everything derived
        // from it — relative imports, audience, panic sites — resolved wrong.
        assert_eq!(
            generated_importer("generated by mk(\"./w\")\u{1f}N:/work/at acme/app/main.vyrn"),
            Some("N:/work/at acme/app/main.vyrn")
        );
        // A nested banner unwraps to the real on-disk file.
        assert_eq!(
            generated_importer(
                "generated by components(\"./w\")\u{1f}generated by i18n(\"./s\")\u{1f}proj/site.vyrn"
            ),
            Some("proj/site.vyrn")
        );
        // Banners written before the separator existed still parse the old way —
        // including its blind spot, which only such legacy keys can reach.
        assert_eq!(
            generated_importer("generated by mk(\"./w\") at N:/work at acme/main.vyrn"),
            Some("acme/main.vyrn")
        );
        assert_eq!(generated_importer("main.vyrn"), None);
    }

    // ---- RFC-0027: namespaced imports ------------------------------------

    #[test]
    fn namespace_calls_and_type_positions() {
        let api = "export type User = { id: Int64 } \
                   export fn getUser(id: Int64) -> User { return User { id: id } }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        let u: api.User = api.getUser(7) \
                        return u.id }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 7);
    }

    #[test]
    fn namespace_record_construction() {
        let api = "export type Req = { id: Int64 } \
                   export fn take(r: Req) -> Int64 { return r.id }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { return api.take(api.Req { id: 41 }) + 1 }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 42);
    }

    #[test]
    fn namespace_enum_variant_construction_and_match() {
        let lib = "export type Color = | Red | Green | Blue";
        let root = "import * as c from \"./lib\" \
                    fn rank(x: c.Color) -> Int64 { \
                        return match x { c.Color.Red => 1, c.Color.Green => 2, c.Color.Blue => 3 } } \
                    fn main() -> Int64 { return rank(c.Color.Green) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 2);
    }

    #[test]
    fn namespace_enum_variant_with_payload() {
        let lib = "export type Shape = | Circle(Int64) | Dot \
                   export fn area(s: Shape) -> Int64 { return match s { Circle(r) => r * r, Dot => 0 } }";
        let root = "import * as g from \"./lib\" \
                    fn main() -> Int64 { return g.area(g.Shape.Circle(6)) }";
        assert_eq!(run_multi(root, &[("lib.vyrn", lib)]).unwrap(), 36);
    }

    #[test]
    fn two_namespaced_modules_share_an_export_name() {
        // The whole point: two modules both export `render`, coexisting under
        // distinct namespaces without a flat-namespace collision.
        let a = "export fn render() -> Int64 { return 1 }";
        let b = "export fn render() -> Int64 { return 20 }";
        let root = "import * as a from \"./a\" \
                    import * as b from \"./b\" \
                    fn main() -> Int64 { return a.render() + b.render() }";
        assert_eq!(
            run_multi(root, &[("a.vyrn", a), ("b.vyrn", b)]).unwrap(),
            21
        );
    }

    #[test]
    fn namespace_composes_with_selective_import() {
        // A module may both selectively import and namespace the same module;
        // they resolve to the same decls.
        let api = "export fn getUser(id: Int64) -> Int64 { return id * 10 }";
        let root = "import { getUser } from \"./api\" \
                    import * as api from \"./api\" \
                    fn main() -> Int64 { return getUser(2) + api.getUser(3) }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 50);
    }

    #[test]
    fn namespace_type_name_argument() {
        // `fromJson(ns.User, s)` / `jsonSchema(ns.User)` — type-name arguments.
        let api = "export type User = { id: Int64, name: String }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        return match fromJson(api.User, \"{\\\"id\\\":5,\\\"name\\\":\\\"a\\\"}\") { \
                            Valid(u) => u.id, Invalid(iss) => 0 } }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 5);
    }

    #[test]
    fn local_binding_shadows_a_namespace() {
        // A local `api` shadows the namespace; `api.field` is then field access on
        // the local record, not a qualified reference.
        let api = "export type T = { field: Int64 } export fn mk() -> T { return T { field: 9 } }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { \
                        let rec = api.mk() \
                        let api = rec \
                        return api.field }";
        assert_eq!(run_multi(root, &[("api.vyrn", api)]).unwrap(), 9);
    }

    #[test]
    fn namespace_used_as_a_value_is_an_error() {
        let api = "export fn f() -> Int64 { return 1 }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { let x = api return 0 }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("namespace `api` is not a value"), "{e}");
    }

    #[test]
    fn namespace_member_must_be_exported() {
        let api = "fn secret() -> Int64 { return 1 } export fn ok() -> Int64 { return 2 }";
        let root = "import * as api from \"./api\" \
                    fn main() -> Int64 { return api.secret() }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("no exported member `secret`"), "{e}");
    }

    #[test]
    fn namespaces_are_one_level_deep() {
        // `./a` namespaces `./b`; a root namespace of `./a` cannot reach `b.thing`.
        let b = "export fn thing() -> Int64 { return 7 }";
        let a = "import * as b from \"./b\" export fn viaA() -> Int64 { return b.thing() }";
        let root = "import * as a from \"./a\" \
                    fn main() -> Int64 { return a.b.thing() }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("no exported member `b`"), "{e}");
    }

    #[test]
    fn namespace_name_colliding_with_a_decl_is_an_error() {
        let api = "export fn f() -> Int64 { return 1 }";
        let root = "import * as api from \"./api\" \
                    fn api() -> Int64 { return 0 } \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("api.vyrn", api)]);
        assert!(e.contains("collides with a top-level declaration"), "{e}");
    }

    #[test]
    fn duplicate_namespace_name_is_an_error() {
        let a = "export fn f() -> Int64 { return 1 }";
        let b = "export fn g() -> Int64 { return 2 }";
        let root = "import * as x from \"./a\" \
                    import * as x from \"./b\" \
                    fn main() -> Int64 { return 0 }";
        let e = load_err(root, &[("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("bound twice"), "{e}");
    }
    // ---- round-two regressions --------------------------------------------

    #[test]
    fn bare_expression_statement_diagnoses_at_its_own_line() {
        // A side-effect statement's references used to be seeded with line 0,
        // so an un-imported foreign call reported at the top of the module
        // instead of at the call site.
        let lib = "export fn other() -> Int64 { return 1 }";
        let root = "import { helper } from \"./lib\"\n\
                    fn main() -> Int64 {\n\
                        helper()\n\
                        other()\n\
                        return 0\n\
                    }";
        let ds = load(root, "main.vyrn", &opts(), &map(&[("lib.vyrn", lib)]))
            .expect_err("expected a load error");
        let hit = ds
            .iter()
            .find(|d| d.message.contains("`other`") && d.message.contains("not imported"));
        let d = hit.expect("the un-imported reference must be diagnosed");
        assert_eq!(d.line, 4, "diagnostic must sit on the call site: {d:?}");
    }

    #[test]
    fn shared_private_externs_keep_their_host_abi_spelling() {
        // Two stub modules restating the same private `extern fn` are one
        // host-ABI contract, not a collision: neither may be renamed (the
        // backends emit the import under the SOURCE spelling and the JS host
        // supplies it by that name), the merged program keeps a single copy,
        // and each stub calls its own without an import.
        let rpc_a = "extern fn vyrnRpcCall(x: Int64) -> Int64 \
                     export fn pingA(x: Int64) -> Int64 { return vyrnRpcCall(x) }";
        let rpc_b = "extern fn vyrnRpcCall(x: Int64) -> Int64 \
                     export fn pingB(x: Int64) -> Int64 { return vyrnRpcCall(x) }";
        let root = "import { pingA } from \"./rpc_a\" \
                    import { pingB } from \"./rpc_b\" \
                    fn main() -> Int64 { return 0 }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("rpc_a.vyrn", rpc_a), ("rpc_b.vyrn", rpc_b)]),
        )
        .unwrap();
        let externs: Vec<&str> = program
            .functions
            .iter()
            .filter(|f| f.is_extern)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(externs, vec!["vyrnRpcCall"], "one copy, source spelling");
    }

    #[test]
    fn aliased_import_does_not_reject_a_protocol_method_call() {
        // `widget.render()` arrives as the call `render(widget)` — exactly the
        // shape of a forbidden direct use of an aliased import's original.
        // When `render` is also a protocol-method surface name, that call
        // dispatches to impls BEFORE any free function and can never reach the
        // imported decl, so the hidden-original check must not fire.
        let ui = "export fn render(w: Int64) -> Int64 { return w }";
        let gfx = "export protocol P { fn render(self) -> Int64 } \
                   export type G = { v: Int64 } \
                   impl P for G { fn render(self) -> Int64 { return self.v } }";
        let root = "import { render as draw } from \"./ui\" \
                    import { G } from \"./gfx\" \
                    fn main() -> Int64 { let g: G = G { v: 3 } return g.render() }";
        let loaded = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("ui.vyrn", ui), ("gfx.vyrn", gfx)]),
        );
        assert!(
            loaded.is_ok(),
            "a method-sugar call must not read as a direct use: {:?}",
            loaded
                .err()
                .map(|ds| ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
    }

    #[test]
    fn hand_import_of_a_non_enum_decl_keeps_own_variant_spellings() {
        // Importing ANY decl of an injected module used to spray ALL of its
        // variant renames over the importer — corrupting a legal private enum
        // whose variant happens to share a spelling (`JStr`). The renames ride
        // on importing THE ENUM itself; importing only `emit` leaves the
        // consumer's own `JStr` alone.
        let root = "import { emit } from \"std/json\" \
                    type T = | JStr(Int64) | JEnd \
                    fn main() -> Int64 { \
                        let t: T = JStr(41) \
                        return match t { JStr(n) => n, JEnd => 0 } \
                    }";
        assert_eq!(run_multi(root, RT_FILES).unwrap(), 41);
    }

    #[test]
    fn importing_the_injected_enum_still_folds_its_variants() {
        // The other half of the gate: an importer of the enum itself follows
        // the variant renames, so its own same-spelled variant is NOT created
        // and the folded constructor runs std/json's code.
        let root = "import { Json, emit } from \"std/json\" \
                    fn main() -> Int64 { \
                        let j: Json = JStr(\"hi\") \
                        return if emit(j).byteLength == 4 { 1 } else { 0 } \
                    }";
        assert_eq!(run_multi(root, RT_FILES).unwrap(), 1);
    }
}

#[cfg(test)]
mod remote_tests {
    use super::tests::{map, opts};
    use super::*;

    fn load_err_at(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(_) => panic!("expected a load error"),
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn remote_specifiers_are_their_own_keys() {
        // A MapResolver keyed by the remote key stands in for the network —
        // exactly what the CLI's cache does.
        let lib = "export fn pad(n: Int64) -> Int64 { return n + 1 }";
        let root = "import { pad } from \"github:acme/strings@v1/src/pad\" \
                    fn main() -> Int64 { return pad(41) }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[("github:acme/strings@v1/src/pad.vyrn", lib)]),
        )
        .unwrap();
        assert_eq!(run_compiled(&program).unwrap(), 42);
    }

    #[test]
    fn relative_imports_inside_a_remote_stay_in_its_base() {
        let a = "import { b } from \"./b\" export fn a() -> Int64 { return b() }";
        let b = "export fn b() -> Int64 { return 7 }";
        let root = "import { a } from \"github:acme/x@abc/src/a\" \
                    fn main() -> Int64 { return a() }";
        let program = load(
            root,
            "main.vyrn",
            &opts(),
            &map(&[
                ("github:acme/x@abc/src/a.vyrn", a),
                ("github:acme/x@abc/src/b.vyrn", b),
            ]),
        )
        .unwrap();
        assert_eq!(run_compiled(&program).unwrap(), 7);
    }

    #[test]
    fn remote_relative_escapes_are_rejected() {
        let a = "import { x } from \"../../../etc/passwd\" \
                 export fn a() -> Int64 { return 0 }";
        let root = "import { a } from \"github:acme/x@abc/src/a\" \
                    fn main() -> Int64 { return a() }";
        let e = load_err_at(root, &[("github:acme/x@abc/src/a.vyrn", a)]);
        assert!(e.contains("escapes its remote module's base"), "{e}");
    }

    #[test]
    fn bare_specifiers_inside_remote_modules_are_rejected() {
        let a = "import { x } from \"money\" export fn a() -> Int64 { return 0 }";
        let root = "import { a } from \"gist:demko/abc123/a\" \
                    fn main() -> Int64 { return a() }";
        let mut o = opts();
        o.aliases.insert("money".into(), "./money".into());
        let e = match load(
            root,
            "main.vyrn",
            &o,
            &map(&[("gist:demko/abc123/a.vyrn", a)]),
        ) {
            Ok(_) => panic!("expected error"),
            Err(ds) => ds[0].message.clone(),
        };
        assert!(e.contains("cannot resolve import `money`"), "{e}");
    }

    #[test]
    fn http_imports_are_rejected() {
        let root = "import { x } from \"http://x.dev/y\" fn main() -> Int64 { return 0 }";
        let e = load_err_at(root, &[]);
        assert!(e.contains("insecure `http:`"), "{e}");
    }
}

#[cfg(test)]
mod gen_tests {
    use super::tests::opts;
    use super::*;
    use std::cell::RefCell;

    /// A resolver over an in-memory map that ALSO persists the generator cache in
    /// memory — so a second load in the same test observes cache hits.
    struct CachingResolver {
        files: HashMap<String, String>,
        cache: RefCell<HashMap<String, String>>,
    }
    impl CachingResolver {
        fn new(entries: &[(&str, &str)]) -> CachingResolver {
            CachingResolver {
                files: entries
                    .iter()
                    .chain(RT_FILES.iter())
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                cache: RefCell::new(HashMap::new()),
            }
        }
    }
    impl ModuleResolver for CachingResolver {
        fn read(&self, resolved: &str) -> Result<String, String> {
            self.files
                .get(resolved)
                .cloned()
                .ok_or_else(|| format!("not found: {resolved}"))
        }
        fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
            let prefix = format!("{}/", resolved.trim_end_matches('/'));
            let mut names: std::collections::BTreeSet<String> = Default::default();
            let mut any = false;
            for k in self.files.keys() {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    any = true;
                    if let Some(seg) = rest.split('/').next() {
                        if !seg.is_empty() {
                            names.insert(seg.to_string());
                        }
                    }
                }
            }
            if any {
                Ok(names.into_iter().collect())
            } else {
                Err(vyrn_frontend::trap::io_at("listerr", resolved))
            }
        }
        fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
            let prefix = format!("{}/", resolved.trim_end_matches('/'));
            let mut names: std::collections::BTreeSet<String> = Default::default();
            let mut any = false;
            for k in self.files.keys() {
                if let Some(rest) = k.strip_prefix(&prefix) {
                    any = true;
                    match rest.split_once('/') {
                        Some((seg, _)) if !seg.is_empty() => {
                            names.insert(format!("{seg}/"));
                        }
                        None if !rest.is_empty() => {
                            names.insert(rest.to_string());
                        }
                        _ => {}
                    }
                }
            }
            if any {
                Ok(names.into_iter().collect())
            } else {
                Err(vyrn_frontend::trap::io_at("listerr", resolved))
            }
        }
        fn gen_cache_get(&self, key: &str) -> Option<String> {
            self.cache.borrow().get(key).cloned()
        }
        fn gen_cache_put(&self, key: &str, value: &str) {
            self.cache
                .borrow_mut()
                .insert(key.to_string(), value.to_string());
        }
    }

    fn run_with(root: &str, r: &dyn ModuleResolver) -> Result<i64, String> {
        let program = load(root, "main.vyrn", &opts(), r)
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
        let diags = vyrn_frontend::checker::check_accum(&program);
        if let Some(d) = diags.first() {
            return Err(d.render());
        }
        run_compiled(&program)
    }

    fn map(entries: &[(&str, &str)]) -> MapResolver {
        MapResolver(
            entries
                .iter()
                .chain(RT_FILES.iter())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
    fn run(root: &str, files: &[(&str, &str)]) -> Result<i64, String> {
        run_with(root, &map(files))
    }
    fn gen_err(root: &str, files: &[(&str, &str)]) -> String {
        match load(root, "main.vyrn", &opts(), &map(files)) {
            Ok(p) => match vyrn_frontend::checker::check_accum(&p).first() {
                Some(d) => d.message.clone(),
                None => panic!("expected an error, load+check succeeded"),
            },
            Err(ds) => ds
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn generator_output_links_and_runs() {
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"export fn magic() -> Int64 { return 42 }\" }";
        let root = "import { mk } from \"./gen\" \
                    import { magic } from mk(\"./data\") \
                    fn main() -> Int64 { return magic() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 42);
    }

    #[test]
    fn generator_reads_a_scoped_file() {
        // The generator reads a data file (mediated) and emits it as a constant.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(\"./data/n.txt\") { \
                           Ok(s) => \"export fn n() -> String { return \\\"\" + s + \"\\\" }\", \
                           Err(e) => e } }";
        let root = "import { consts } from \"./gen\" \
                    import { n } from consts(\"./data\") \
                    fn main() -> Int64 { print(n()) return 0 }";
        let files = &[("gen.vyrn", gen), ("data/n.txt", "hello")];
        // Links + runs (the emitted `n` returns the file content).
        assert_eq!(run(root, files).unwrap(), 0);
    }

    #[test]
    fn generator_readfile_escape_is_rejected() {
        let gen = "export gen fn g(dir: String) -> String { \
                       return match readFile(\"./secret.txt\") { Ok(s) => s, Err(e) => e } }";
        let root = "import { g } from \"./gen\" \
                    import { x } from g(\"./data\") \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen), ("secret.txt", "top secret")]);
        assert!(e.contains("escapes its declared inputs"), "{e}");
    }

    #[test]
    fn generator_listdir_is_scoped_and_works() {
        // Emit a function returning the number of files under the data dir.
        let gen = "export gen fn count(dir: String) -> String { \
                       return match listDir(dir) { \
                           Ok(names) => \"export fn n() -> Int64 { return \" + names.length.toString() + \" }\", \
                           Err(e) => e } }";
        let root = "import { count } from \"./gen\" \
                    import { n } from count(\"./data\") \
                    fn main() -> Int64 { return n() }";
        let files = &[
            ("gen.vyrn", gen),
            ("data/a.txt", "1"),
            ("data/b.txt", "2"),
            ("data/c.txt", "3"),
        ];
        assert_eq!(run(root, files).unwrap(), 3);
    }

    #[test]
    fn distinct_args_make_distinct_modules_same_args_dedup() {
        // Two calls with different args ⇒ two modules with different names.
        let gen = "export gen fn mk(tag: String) -> String { \
                       return \"export fn tag\" + tag + \"() -> Int64 { return \" + tag + \" }\" }";
        let root = "import { mk } from \"./gen\" \
                    import { tag1 } from mk(\"1\") \
                    import { tag2 } from mk(\"2\") \
                    fn main() -> Int64 { return tag1() + tag2() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 3);
    }

    #[test]
    fn same_resolved_path_different_spellings_share_one_stateful_module() {
        // RFC-0040 §1: two importers call the same generator with path args that
        // RESOLVE identically but are spelled differently (`./data` vs the rebased
        // `./x/../data`). They must synthesize ONE module — so its module state (`n`)
        // exists once and both importers mutate the SAME instance. Without the
        // resolved-inputs identity, two modules each define `n`/`bump` and collide.
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"let mut n: Int64 = 0\\n\
                                export fn bump() -> Int64 { n = n + 1\\nreturn n }\\n\" }";
        let a = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./data\") \
                 export fn a() -> Int64 { return bump() }";
        let b = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./x/../data\") \
                 export fn b() -> Int64 { return bump() }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        // One shared `n`: a() = 1, b() = 2, sum = 3.
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("a.vyrn", a), ("b.vyrn", b)]).unwrap(),
            3,
        );
    }

    #[test]
    fn different_resolved_paths_stay_distinct_modules() {
        // The flip side of §1: two calls that resolve to DIFFERENT targets are
        // still two modules. Each emits `bump`, so the flat namespace collides —
        // proof the identity did not over-merge distinct targets.
        let gen = "export gen fn mk(dir: String) -> String { \
                       return \"export fn bump() -> Int64 { return 1 }\\n\" }";
        let a = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./data\") \
                 export fn a() -> Int64 { return bump() }";
        let b = "import { mk } from \"./gen\" \
                 import { bump } from mk(\"./other\") \
                 export fn b() -> Int64 { return bump() }";
        let root = "import { a } from \"./a\" \
                    import { b } from \"./b\" \
                    fn main() -> Int64 { return a() + b() }";
        let e = gen_err(root, &[("gen.vyrn", gen), ("a.vyrn", a), ("b.vyrn", b)]);
        assert!(e.contains("`bump` is declared by both"), "{e}");
    }

    #[test]
    fn generator_trap_becomes_a_load_diagnostic() {
        let gen = "export gen fn bad(x: Int64) -> String { \
                       let q = 1 / x \
                       return \"\" }";
        let root = "import { bad } from \"./gen\" \
                    import { z } from bad(0) \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("generator `bad") && e.contains("failed"), "{e}");
    }

    #[test]
    fn generated_name_collision_is_a_load_error() {
        let gen = "export gen fn mk(d: String) -> String { \
                       return \"export fn dup() -> Int64 { return 1 }\" }";
        // The root already defines `dup`, so the generated `dup` collides.
        let root = "import { mk } from \"./gen\" \
                    import { dup } from mk(\"./x\") \
                    fn dup() -> Int64 { return 2 } \
                    fn main() -> Int64 { return dup() }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("`dup` is declared by both"), "{e}");
    }

    #[test]
    fn non_constant_generator_argument_is_rejected() {
        let gen = "export gen fn mk(d: String) -> String { return \"\" }";
        let root = "import { mk } from \"./gen\" \
                    import { x } from mk(readLine()) \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("compile-time-constant"), "{e}");
    }

    #[test]
    fn missing_generator_is_a_clear_error() {
        let root = "import { x } from nope(\"./d\") fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[]);
        assert!(e.contains("not an imported `gen fn`"), "{e}");
    }

    #[test]
    fn module_interface_reflects_exported_surface() {
        // The generator emits a doc string listing the contract's exported fns.
        let contract = "export type Id = Int64 where value >= 1 \
                        export fn ping(id: Id) -> String { return \"pong\" }";
        let gen = "export gen fn doc(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut body = \"export fn names() -> String { return \\\"\" \
                       for f in iface.functions { body = body + f.name + \";\" } \
                       body = body + \"\\\" }\" \
                       return body }";
        let root = "import { doc } from \"./gen\" \
                    import { names } from doc(\"./contract\") \
                    fn main() -> Int64 { print(names()) return 0 }";
        let files = &[("gen.vyrn", gen), ("contract.vyrn", contract)];
        // Runs; `names()` returns "ping;" (the one exported fn).
        assert_eq!(run(root, files).unwrap(), 0);
    }

    #[test]
    fn module_interface_closure_reaches_imported_types() {
        // RFC-0031: the contract NAMES only `Req` in its signature and declares no
        // types of its own; `Req`/`Book`/`Id` live in `wire`. `moduleInterface`
        // must reach the whole closure, so the generator counting `iface.types`
        // sees all three.
        let wire = "export type Id = Int64 where value >= 1 \
                    export type Book = { id: Id } \
                    export type Req = { book: Book }";
        let contract = "import { Req } from \"./wire\" \
                        export fn make(r: Req) -> Req { return r }";
        let gen = "export gen fn cnt(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut n = 0 \
                       for t in iface.types { n = n + 1 } \
                       return \"export fn n() -> Int64 { return \" + \"\\{n}\" + \" }\\n\" }";
        let root = "import { cnt } from \"./gen\" \
                    import { n } from cnt(\"./contract\") \
                    fn main() -> Int64 { return n() }";
        assert_eq!(
            run(
                root,
                &[
                    ("gen.vyrn", gen),
                    ("contract.vyrn", contract),
                    ("wire.vyrn", wire)
                ]
            )
            .unwrap(),
            3,
            "closure = Req + Book + Id"
        );
    }

    #[test]
    fn closure_type_file_edit_invalidates_the_cache_unrelated_edit_hits() {
        // RFC-0031 cache soundness: a closure type's defining FILE (`wire.vyrn`)
        // is never a generator ARGUMENT (the arg is `./contract`), yet editing it
        // must miss the cache. It joins the recorded inputs through the reflection
        // read, so the content hash changes on edit; an unrelated file does not.
        let wire = "export type T = { a: Int64 } export fn seed(t: T) -> T { return t }";
        let contract = "import { T } from \"./wire\" export fn f(x: T) -> T { return x }";
        // The generator's output embeds the closure's field spelling, so a real
        // edit to `wire.vyrn` produces FRESH output, not just a re-run.
        let gen = "export gen fn refl(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut src = \"\" \
                       for t in iface.types { src = src + t.source } \
                       return \"export fn shape() -> Int64 { return \" + \"\\{src.byteLength}\" + \" }\\n\" }";
        let root = "import { refl } from \"./gen\" \
                    import { shape } from refl(\"./contract\") \
                    fn main() -> Int64 { return shape() }";
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("contract.vyrn", contract),
            ("wire.vyrn", wire),
            ("noise.vyrn", "export fn unused() -> Int64 { return 0 }"),
        ]);

        let before = gen_run_count();
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // An unrelated edit (a file the closure never reads) still hits.
        r.files.insert(
            "noise.vyrn".to_string(),
            "export fn unused() -> Int64 { return 1 }".to_string(),
        );
        run_with(root, &r).unwrap();
        assert_eq!(
            gen_run_count(),
            before + 1,
            "unrelated edit: still a cache hit"
        );

        // Editing the foreign closure type's file misses → re-run + fresh output.
        r.files.insert(
            "wire.vyrn".to_string(),
            "export type T = { a: Int64, b: Int64 } export fn seed(t: T) -> T { return t }"
                .to_string(),
        );
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 2, "closure type edited: re-run");
    }

    /// The generator's OWN sources — its module and everything that module
    /// imports — must invalidate its cache entry.
    ///
    /// They used to be hashed into the lookup key, which meant discovering the
    /// closure (a full parse-walk) on every hit just to find the entry. They are
    /// now recorded among the entry's inputs and re-hashed on lookup instead, so
    /// this is the test that the move kept the guarantee: edit the generator, or
    /// anything it imports, and the next load must RE-RUN rather than serve a
    /// stale expansion.
    #[test]
    fn editing_the_generator_or_its_imports_invalidates_the_cache() {
        let helper = r#"export fn tag() -> String { return "one" }"#;
        let gen = r#"import { tag } from "./helper"
export gen fn emit(x: String) -> String { return "export fn shape() -> String { return \"" + tag() + "\" }" }"#;
        let root = r#"import { emit } from "./gen"
import { shape } from emit("./seed")
fn main() -> Int64 { return shape().byteLength }"#;
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("helper.vyrn", helper),
            ("seed.vyrn", "export fn seed() -> Int64 { return 0 }"),
        ]);

        let before = gen_run_count();
        assert_eq!(run_with(root, &r).unwrap(), 3, "cold: `one`");
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        assert_eq!(run_with(root, &r).unwrap(), 3);
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // Edit a module the GENERATOR imports — never an argument, never read by
        // the sandbox, reachable only through the generator's own module graph.
        r.files.insert(
            "helper.vyrn".to_string(),
            r#"export fn tag() -> String { return "three" }"#.to_string(),
        );
        assert_eq!(
            run_with(root, &r).unwrap(),
            5,
            "generator's import edited: fresh output, not the stale `one`"
        );
        assert_eq!(
            gen_run_count(),
            before + 2,
            "generator's import edited: re-run"
        );

        // Edit the generator module itself.
        r.files.insert(
            "gen.vyrn".to_string(),
            gen.replace("tag()", "tag() + \"!\""),
        );
        assert_eq!(
            run_with(root, &r).unwrap(),
            6,
            "generator edited: fresh output (`three` + `!`)"
        );
        assert_eq!(gen_run_count(), before + 3, "generator edited: re-run");
    }

    #[test]
    fn co_naming_rename_leaves_namespace_member_calls_alone() {
        // RFC-0031 found this: `mid` delegates `store.get()` via a namespace
        // (RFC-0027) while ANOTHER module co-names `get` (aliased import + a local
        // stub of the same name, RFC-0022). The co-naming rename frees `mid`'s
        // `get` for the stub — but must NOT rewrite `store.get()` (method-sugar
        // call name) into `store.get__from0`; that member belongs to the
        // namespace resolver.
        let store = "let mut n = 41 \
                     export fn fetch() -> Int64 { n = n + 1 return n }";
        let mid = "import * as store from \"./store\" \
                   export fn fetch() -> Int64 { return store.fetch() }";
        let root = "import { fetch as fetch__real } from \"./mid\" \
                    fn fetch() -> Int64 { return fetch__real() } \
                    fn main() -> Int64 { return fetch() }";
        assert_eq!(
            run(root, &[("store.vyrn", store), ("mid.vyrn", mid)]).unwrap(),
            42
        );
    }

    #[test]
    fn closure_name_collision_is_a_load_diagnostic_naming_both_modules() {
        // RFC-0031: if the closure would hold two DISTINCT `T` decls (one per
        // module), reflection fails with a load diagnostic naming BOTH modules —
        // a wire format with two `T`s has no honest JSON spelling.
        let wire_a = "export type T = { a: Int64 } export type A = { t: T }";
        let wire_b = "export type T = { b: Int64 } export type B = { t: T }";
        let contract = "import { A } from \"./wireA\" \
                        import { B } from \"./wireB\" \
                        export fn f(a: A) -> B { return B { t: T { b: 0 } } }";
        let gen = "export gen fn cnt(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       return \"export fn z() -> Int64 { return 0 }\\n\" }";
        let root = "import { cnt } from \"./gen\" \
                    import { z } from cnt(\"./contract\") \
                    fn main() -> Int64 { return z() }";
        let e = gen_err(
            root,
            &[
                ("gen.vyrn", gen),
                ("contract.vyrn", contract),
                ("wireA.vyrn", wire_a),
                ("wireB.vyrn", wire_b),
            ],
        );
        assert!(
            e.contains("wireA.vyrn") && e.contains("wireB.vyrn"),
            "names both modules: {e}"
        );
        assert!(e.contains('T'), "names the colliding type: {e}");
    }

    #[test]
    fn generated_module_imports_a_sibling() {
        // A synthesized module (its key is a banner, not a path) must resolve its
        // own relative imports against the real importer's directory (RFC-0021 —
        // the first `moduleInterface` consumer, RPC, needs this).
        let contract = "export fn calc() -> Int64 { return 21 }";
        let gen = "export gen fn wrap(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       return \"import { calc } from \\\"\" + path + \"\\\"\\n\" \
                            + \"export fn go() -> Int64 { return calc() + calc() }\\n\" }";
        let root = "import { wrap } from \"./gen\" \
                    import { go } from wrap(\"./contract\") \
                    fn main() -> Int64 { return go() }";
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("contract.vyrn", contract)]).unwrap(),
            42
        );
    }

    #[test]
    fn nested_generator_resolves_paths_against_the_real_importer() {
        // RFC-0029 wave: a generator imported BY a generated module (a nested
        // generator — e.g. `i18n(..)` inside a `.vyx` script that `components(..)`
        // synthesized) must resolve its path arguments against the REAL importing
        // file's directory, not the synthetic banner key. `outer` emits a module
        // that imports `inner("./sub/data")`; `inner` reflects that module — which
        // it can only read if the path resolves to `sub/data.vyrn`.
        let gen = "export gen fn inner(path: String) -> String { \
                       let iface = moduleInterface(path) \
                       let mut n = 0 \
                       for f in iface.functions { n = n + 1 } \
                       return \"export fn cnt() -> Int64 { return \" + \"\\{n}\" + \" }\\n\" } \
                   export gen fn outer(dummy: String) -> String { \
                       return \"import { inner } from \\\"./gen\\\"\\n\" \
                            + \"import { cnt } from inner(\\\"./sub/data\\\")\\n\" \
                            + \"export fn go() -> Int64 { return cnt() }\\n\" }";
        let data = "export fn a() -> Int64 { return 1 } export fn b() -> Int64 { return 2 }";
        let root = "import { outer } from \"./gen\" \
                    import { go } from outer(\"x\") \
                    fn main() -> Int64 { return go() }";
        // `sub/data` has two exported functions, so `cnt()` — hence `go()` — is 2.
        assert_eq!(
            run(root, &[("gen.vyrn", gen), ("sub/data.vyrn", data)]).unwrap(),
            2
        );
    }

    #[test]
    fn generated_module_may_declare_module_state() {
        // Module state is legal in a generated module (RFC-0021's carve-out, now
        // the general RFC-0029 rule — see `non_root_module_state_is_legal_via_accessors`).
        // The generated `currentLocale`-style global initializes before `main` and
        // persists across handler calls made from the root (the setLocale/locale + t() shape).
        let gen = "export gen fn mk(tag: String) -> String { \
                       return \"let mut cur = 10\\n\" \
                            + \"export fn bump() { cur = cur + 1 }\\n\" \
                            + \"export fn peek() -> Int64 { return cur }\\n\" }";
        let root = "import { mk } from \"./gen\" \
                    import { bump, peek } from mk(\"x\") \
                    fn main() -> Int64 { bump() bump() return peek() }";
        // 10 (init) + 1 + 1 = 12; state persists across the two `bump()` calls.
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 12);
    }

    #[test]
    fn generated_module_calls_back_into_its_importer() {
        // The RPC dispatcher pattern: a generated module invokes a plain function
        // defined in the module that imported it (the callback convention). Names
        // owned by the importer are visible to generated code without an import.
        let gen = "export gen fn cb(tag: String) -> String { \
                       return \"export fn dispatch() -> Int64 { return onEvent() + 1 }\\n\" }";
        let root = "import { cb } from \"./gen\" \
                    import { dispatch } from cb(\"x\") \
                    fn onEvent() -> Int64 { return 41 } \
                    fn main() -> Int64 { return dispatch() }";
        assert_eq!(run(root, &[("gen.vyrn", gen)]).unwrap(), 42);
    }

    #[test]
    fn two_generators_same_args_do_not_share_a_cache_entry() {
        // One module may export several `gen fn`s; distinct generators over the
        // same arguments must not collide in the content-addressed cache (the
        // cache key includes the generator name).
        let gen = "export gen fn a(p: String) -> String { \
                       return \"export fn which() -> Int64 { return 1 }\" } \
                   export gen fn b(p: String) -> String { \
                       return \"export fn which() -> Int64 { return 2 }\" }";
        let root_a = "import { a } from \"./gen\" \
                      import { which } from a(\"./x\") \
                      fn main() -> Int64 { return which() }";
        let root_b = "import { b } from \"./gen\" \
                      import { which } from b(\"./x\") \
                      fn main() -> Int64 { return which() }";
        let r = CachingResolver::new(&[("gen.vyrn", gen)]);
        assert_eq!(run_with(root_a, &r).unwrap(), 1, "generator `a` output");
        assert_eq!(
            run_with(root_b, &r).unwrap(),
            2,
            "generator `b` must not reuse `a`'s cache"
        );
    }

    #[test]
    fn cache_hit_skips_the_second_run_and_input_change_invalidates() {
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(\"./data/n.txt\") { \
                           Ok(s) => \"export fn n() -> String { return \\\"\" + s + \"\\\" }\", \
                           Err(e) => e } }";
        let root = "import { consts } from \"./gen\" \
                    import { n } from consts(\"./data\") \
                    fn main() -> Int64 { return 0 }";
        let mut r = CachingResolver::new(&[("gen.vyrn", gen), ("data/n.txt", "one")]);

        let before = gen_run_count();
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");

        // Change the input file — the recorded input hash no longer matches.
        r.files.insert("data/n.txt".to_string(), "two".to_string());
        run_with(root, &r).unwrap();
        assert_eq!(gen_run_count(), before + 2, "input changed: re-run");
    }

    /// A generator that COUNTS the entries of a listed directory: the site's
    /// `repo.vyrn` in miniature. `-1` when the directory is not there at all.
    const COUNTER: &str = "export gen fn count(dir: String) -> String { \
                               return match listDir(dir) { \
                                   Ok(names) => \"export fn n() -> Int64 { return \" \
                                       + names.length.toString() + \" }\", \
                                   Err(e) => \"export fn n() -> Int64 { return 0 - 1 }\", \
                               } }";
    const COUNTER_ROOT: &str = "import { count } from \"./gen\" \
                                import { n } from count(\"./data\") \
                                fn main() -> Int64 { return n() }";

    #[test]
    fn a_file_added_to_a_listed_directory_re_runs_the_generator() {
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.insert("data/b.txt".to_string(), "2".to_string());
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            2,
            "counts the new file"
        );
        assert_eq!(gen_run_count(), before + 2, "listing changed: re-run");
    }

    #[test]
    fn a_file_removed_from_a_listed_directory_re_runs_the_generator() {
        let mut r = CachingResolver::new(&[
            ("gen.vyrn", COUNTER),
            ("data/a.txt", "1"),
            ("data/b.txt", "2"),
        ]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 2);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.remove("data/b.txt");
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "counts what is left"
        );
        assert_eq!(gen_run_count(), before + 2, "listing changed: re-run");
    }

    #[test]
    fn a_directory_that_appears_re_runs_the_generator() {
        // The site bug: the first build found no `examples/`, published "0
        // examples", and kept publishing it. A listing that FAILED is an input
        // too — the directory being absent is what the generator saw.
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER)]);
        let before = gen_run_count();
        // 255 rather than -1: `main`'s answer is a process exit code on this
        // route, and a byte is what a process reports (`vyrn run` masks the
        // interpreter's the same way).
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 255, "no directory yet");
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files.insert("data/a.txt".to_string(), "1".to_string());
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "the directory appeared: the cached miss is stale"
        );
        assert_eq!(gen_run_count(), before + 2, "directory appeared: re-run");
    }

    #[test]
    fn an_unrelated_file_does_not_invalidate_a_listing() {
        // The over-invalidation direction. An entry records what the generation
        // actually read — this listing and nothing else — so a file elsewhere in
        // the tree leaves the cache hit intact. A cache that never hits would
        // undo RFC-0076's keystroke budget.
        let mut r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");

        r.files
            .insert("elsewhere/z.txt".to_string(), "irrelevant".to_string());
        r.files
            .insert("data.txt".to_string(), "a near miss".to_string());
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(
            gen_run_count(),
            before + 1,
            "unrelated files: the cache must still hit"
        );
    }

    /// End to end through the loader: a poisoned entry sitting at the right key
    /// does not reach the program, and the generator runs instead.
    #[test]
    fn a_poisoned_entry_does_not_reach_the_program() {
        let r = CachingResolver::new(&[("gen.vyrn", COUNTER), ("data/a.txt", "1")]);
        let before = gen_run_count();
        assert_eq!(run_with(COUNTER_ROOT, &r).unwrap(), 1);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        let keys: Vec<String> = r.cache.borrow().keys().cloned().collect();
        assert_eq!(keys.len(), 1, "one generation, one entry");

        // What an attacker with write access to the cache directory writes.
        r.gen_cache_put(
            &keys[0],
            "v2 0\nexport fn count() -> Int64 { return 999 }\n",
        );
        assert_eq!(
            run_with(COUNTER_ROOT, &r).unwrap(),
            1,
            "the generator's own answer, not the entry's"
        );
        assert_eq!(gen_run_count(), before + 2, "the refused entry re-ran it");
    }

    #[test]
    fn generator_purity_violation_is_reported() {
        // A `gen fn` that writes a file fails the comptime-purity check.
        let gen = "export gen fn bad(d: String) -> String { \
                       let w = writeFile(\"x\", \"y\") return \"\" }";
        let root = "import { bad } from \"./gen\" \
                    import { z } from bad(\"./d\") \
                    fn main() -> Int64 { return 0 }";
        let e = gen_err(root, &[("gen.vyrn", gen)]);
        assert!(e.contains("not comptime-pure"), "{e}");
    }

    #[test]
    fn same_relative_arg_in_different_dirs_does_not_collide_in_the_cache() {
        // dogfood BUG 1: two modules in DIFFERENT directories both call the same
        // generator with the SAME relative arg (`consts("./data")`), but each
        // `./data` resolves to a different file. The content-addressed cache must
        // NOT serve the first importer's output to the second — the key now folds
        // in the RESOLVED inputs, so the two never share an entry.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(dir + \"/n.txt\") { \
                           Ok(s) => \"export fn val() -> Int64 { return \" + s + \" }\", \
                           Err(e) => e } }";
        let a = "import { consts } from \"../gen\" \
                 import * as g from consts(\"./data\") \
                 export fn na() -> Int64 { return g.val() }";
        let b = "import { consts } from \"../gen\" \
                 import * as g from consts(\"./data\") \
                 export fn nb() -> Int64 { return g.val() }";
        let root = "import { na } from \"./a/client\" \
                    import { nb } from \"./b/client\" \
                    fn main() -> Int64 { return na() * 10 + nb() }";
        let r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("a/client.vyrn", a),
            ("a/data/n.txt", "1"),
            ("b/client.vyrn", b),
            ("b/data/n.txt", "2"),
        ]);
        // Warm cache from `a`'s generation must not leak into `b`'s: 1*10 + 2 = 12
        // (a pre-fix collision served `b` the value `1`, giving 11).
        assert_eq!(run_with(root, &r).unwrap(), 12);
    }

    #[test]
    fn identical_importer_and_arg_still_hits_the_cache() {
        // The other half of BUG 1's fix: same importer + same arg must STILL be a
        // cache hit on re-load (no needless re-run). Two loads of the same root;
        // the generation runs once, then the warm cache short-circuits it.
        let gen = "export gen fn consts(dir: String) -> String { \
                       return match readFile(dir + \"/n.txt\") { \
                           Ok(s) => \"export fn val() -> Int64 { return \" + s + \" }\", \
                           Err(e) => e } }";
        let client = "import { consts } from \"../gen\" \
                      import { val } from consts(\"./data\") \
                      export fn na() -> Int64 { return val() }";
        let root = "import { na } from \"./a/client\" fn main() -> Int64 { return na() }";
        let r = CachingResolver::new(&[
            ("gen.vyrn", gen),
            ("a/client.vyrn", client),
            ("a/data/n.txt", "7"),
        ]);
        let before = gen_run_count();
        assert_eq!(run_with(root, &r).unwrap(), 7);
        assert_eq!(gen_run_count(), before + 1, "cold: one run");
        assert_eq!(run_with(root, &r).unwrap(), 7);
        assert_eq!(gen_run_count(), before + 1, "warm: cache hit, no re-run");
    }
}
