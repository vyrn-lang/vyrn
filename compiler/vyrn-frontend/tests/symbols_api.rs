//! The symbol-query API the LSP consumes: `analyze`, `resolve`, `completions`.

use std::collections::HashSet;

use vyrn_frontend::{
    analyze, completions, member_completions, member_context, resolve, scope_completions,
    string_context, SymbolKind,
};

/// The real `examples/enum.vyrn`, not an inline copy that could go stale.
fn enum_vyrn() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/enum.vyrn");
    std::fs::read_to_string(path).expect("examples/enum.vyrn should exist")
}

fn names(a: &vyrn_frontend::Analysis) -> HashSet<String> {
    a.symbols.iter().map(|s| s.name.clone()).collect()
}

/// The parser-injected `Value` enum and its variants have no source position,
/// so they are filtered out.
#[test]
fn indexes_enum_example_symbols() {
    let a = analyze(&enum_vyrn());
    assert!(
        a.diagnostics.is_empty(),
        "enum.vyrn should be clean: {:?}",
        a.diagnostics
    );
    let n = names(&a);
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(n.contains(expected), "missing symbol {expected}: {:?}", n);
    }
    for injected in ["Value", "IntVal", "StrVal", "BoolVal"] {
        assert!(
            !n.contains(injected),
            "injected {injected} should be filtered: {:?}",
            n
        );
    }
}

/// Name columns come from the token stream, not the AST's line-only positions,
/// so go-to-definition lands on the name.
#[test]
fn symbols_have_precise_name_columns() {
    let a = analyze(&enum_vyrn());
    let by_name: std::collections::HashMap<&str, &vyrn_frontend::Symbol> =
        a.symbols.iter().map(|s| (s.name.as_str(), s)).collect();
    // `type Shape = enum {` on line 1: "type" cols 1-4, space 5, "Shape" cols 6-10.
    let shape = by_name["Shape"];
    assert_eq!(shape.kind, SymbolKind::Type);
    assert_eq!(shape.line, 1);
    assert_eq!(shape.col, 6);
    // `    Circle(Int64),` on line 2: "Circle" cols 5-10.
    let circle = by_name["Circle"];
    assert_eq!(circle.kind, SymbolKind::Variant);
    assert_eq!(circle.line, 2);
    assert_eq!(circle.col, 5);
    // `fn area(...)` on line 6: "fn " then "area" cols 4-7.
    let area = by_name["area"];
    assert_eq!(area.kind, SymbolKind::Function);
    assert_eq!(area.line, 6);
    assert_eq!(area.col, 4);
}

#[test]
fn resolve_variant_at_call_site() {
    let a = analyze(&enum_vyrn());
    // Line 15: `    let a = area(Circle(2));`, `Circle` cols 18-23.
    let r = resolve(&a, 15, 18).expect("Circle at (15,18) should resolve");
    assert_eq!(r.kind, SymbolKind::Variant);
    assert_eq!(r.name, "Circle");
    assert_eq!(r.target_line, 2);
    assert_eq!(r.target_col, 5);
    assert_eq!(r.hover, "variant of Shape: Circle(Int64)");
}

#[test]
fn resolve_function_at_call_site() {
    let a = analyze(&enum_vyrn());
    // Line 15: `area` cols 13-16.
    let r = resolve(&a, 15, 13).expect("area at (15,13) should resolve");
    assert_eq!(r.kind, SymbolKind::Function);
    assert_eq!(r.target_line, 6);
    assert_eq!(r.target_col, 4);
    assert_eq!(r.hover, "fn area(s: Shape) -> Int64");
}

#[test]
fn resolve_type_at_annotation() {
    let a = analyze(&enum_vyrn());
    // Line 6: `fn area(s: Shape) -> Int {`, `Shape` cols 12-16.
    let r = resolve(&a, 6, 12).expect("Shape at (6,12) should resolve");
    assert_eq!(r.kind, SymbolKind::Type);
    assert_eq!(r.target_line, 1);
    assert_eq!(r.target_col, 6);
    assert_eq!(
        r.hover,
        "type Shape = enum { Circle(Int64), Rect(Int64, Int64), Unit }"
    );
}

#[test]
fn resolve_returns_none_off_identifier() {
    let a = analyze(&enum_vyrn());
    // Line 6, col 1 is the `f` of the keyword `fn`; no `TokenInfo` covers it.
    assert!(resolve(&a, 6, 1).is_none());
}

/// The client filters by prefix.
#[test]
fn completions_list_top_level() {
    let a = analyze(&enum_vyrn());
    let labels: HashSet<String> = completions(&a).into_iter().map(|c| c.label).collect();
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(
            labels.contains(expected),
            "completion missing {expected}: {:?}",
            labels
        );
    }
    for injected in ["Value", "IntVal", "StrVal", "BoolVal"] {
        assert!(
            !labels.contains(injected),
            "injected {injected} leaked into completions"
        );
    }
}

#[test]
fn completions_offer_builtin_functions_by_name() {
    let a = analyze("fn main() -> Int64 { return 0 }");
    let print = completions(&a).into_iter().find(|c| c.label == "print");
    assert!(print.is_some_and(|c| c.detail.starts_with("fn print")));
}

#[test]
fn a_refinement_completes_with_its_rule() {
    let a = analyze("type Age = Int64 where value >= 18");
    let age = completions(&a).into_iter().find(|c| c.label == "Age");
    assert_eq!(
        age.map(|c| c.detail).as_deref(),
        Some("type Age = Int64 where value >= 18")
    );
}

/// Inline-refinement types (`User.age` from a field-level `where`) are
/// desugaring artifacts.
#[test]
fn synthetic_field_refinement_types_are_filtered() {
    let src = "type User = { age: Int64 where value >= 18 }\n\
               fn main() -> Int64 { let u = User { age: 21 } return u.age }\n";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean source: {:?}",
        a.diagnostics
    );
    let n = names(&a);
    assert!(n.contains("User"), "the record is indexed: {n:?}");
    assert!(
        !n.iter().any(|s| s.contains('.')),
        "no synthetic types leak: {n:?}"
    );
    let labels: HashSet<String> = completions(&a).into_iter().map(|c| c.label).collect();
    assert!(
        !labels.iter().any(|s| s.contains('.')),
        "completions clean: {labels:?}"
    );
    // Hover renders the refinement as written; the synthetic name never leaks.
    let user = a.symbols.iter().find(|s| s.name == "User").unwrap();
    assert_eq!(
        user.detail, "type User = { age: Int64 where value >= 18 }",
        "{}",
        user.detail
    );
}

/// Statement-level recovery keeps the function, its tokens and the
/// good statements' locals indexed. Downstream checks are skipped, so the parse
/// error is the only diagnostic.
#[test]
fn parse_error_still_indexes_symbols() {
    let src = "fn main() -> Int64 {\n\
                   let good = 1\n\
                   let x = ;\n\
                   return good\n\
               }";
    let a = analyze(src);
    assert!(
        a.symbols.iter().any(|s| s.name == "main"),
        "fn symbol indexed: {:?}",
        a.symbols
    );
    assert!(
        !a.tokens.is_empty(),
        "identifier tokens cached for cursor mapping"
    );
    assert!(
        a.locals.iter().any(|l| l.name == "good"),
        "the good local survives the bad statement"
    );
    assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
    assert_eq!(a.diagnostics[0].stage, "parse");
}

/// `Diagnostic` is not `PartialEq`, so the test compares counts and rendered
/// messages.
#[test]
fn diagnostics_delegate_matches_analyze() {
    let a = analyze(&enum_vyrn());
    assert_eq!(
        a.diagnostics.len(),
        vyrn_frontend::diagnostics(&enum_vyrn()).len()
    );
    assert!(a.diagnostics.is_empty());

    let bad = "fn main() -> Int64 { let x = ; return x; }";
    let ab = analyze(bad);
    let db = vyrn_frontend::diagnostics(bad);
    assert_eq!(ab.diagnostics.len(), db.len());
    assert_eq!(ab.diagnostics.len(), 1);
    assert_eq!(ab.diagnostics[0].render(), db[0].render());
}

/// `examples/foreach.vyrn`: an annotated `let`, a mutable unannotated `let`, and
/// `for`-in loop variables.
fn foreach_vyrn() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/foreach.vyrn");
    std::fs::read_to_string(path).expect("examples/foreach.vyrn should exist")
}

/// Go-to-definition lands on the param name, not the function name.
#[test]
fn resolve_param_at_use_site() {
    let a = analyze(&enum_vyrn());
    // Line 7: `    return match s {`, `s` at col 18.
    let r = resolve(&a, 7, 18).expect("param s at (7,18) should resolve");
    assert_eq!(r.kind, SymbolKind::Param);
    assert_eq!(r.name, "s");
    // `fn area(s: Shape)` is on line 6; `s` at col 9.
    assert_eq!(r.target_line, 6);
    assert_eq!(r.target_col, 9);
    // A value of a user type also shows that type's shape.
    assert_eq!(
        r.hover,
        "s: Shape

```vyrn
type Shape = enum { Circle(Int64), Rect(Int64, Int64), Unit }
```"
    );
}

#[test]
fn resolve_annotated_let() {
    let a = analyze(&foreach_vyrn());
    assert!(
        a.diagnostics.is_empty(),
        "foreach.vyrn should be clean: {:?}",
        a.diagnostics
    );
    // Line 4: `    for s in squares {`, `squares` at col 14.
    let r = resolve(&a, 4, 14).expect("squares at (4,14) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "squares");
    // `let squares: Array<Int, 5> = ..` is on line 2.
    assert_eq!(r.target_line, 2);
    assert_eq!(r.hover, "let squares: Array<Int64, 5>");
}

#[test]
fn resolve_mutable_unannotated_let() {
    let a = analyze(&foreach_vyrn());
    // Line 5: `        total = total + s;`, the first `total` at col 9.
    let r = resolve(&a, 5, 9).expect("total at (5,9) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "total");
    // `let mut total = 0;` is on line 3.
    assert_eq!(r.target_line, 3);
    assert_eq!(r.hover, "let mut total: Int64");
}

#[test]
fn resolve_for_var() {
    let a = analyze(&foreach_vyrn());
    // Line 5: `        total = total + s;`, the loop var `s` at col 25.
    let r = resolve(&a, 5, 25).expect("for-var s at (5,25) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "s");
    // `for s in squares {` is on line 4; `s` is at col 9.
    assert_eq!(r.target_line, 4);
    assert_eq!(r.hover, "for s: Int64");
}

#[test]
fn unannotated_let_infers_str() {
    let src = "\
fn main() -> Int64 {
    let s = \"hi\";
    print(s);
    return 0;
}
";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean source: {:?}",
        a.diagnostics
    );
    // Line 2: `    let s = "hi";`, `s` at col 9.
    let r = resolve(&a, 2, 9).expect("s at (2,9) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "s");
    assert_eq!(r.target_line, 2);
    // The memory line is the core's, and `analyze` judges no ownership, so
    // the hover carries the type alone; `vyrn-lsp`'s suite pins the memory line.
    assert_eq!(r.hover, "let s: String");
}

#[test]
fn unknown_type_pinned_to_ident() {
    let src = "\
fn f(x: Foo) -> Int64 {
    return 0;
}
fn main() -> Int64 { return 0; }
";
    let a = analyze(src);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.message.contains("unknown type"))
        .expect("an unknown-type diagnostic");
    // Line 1: `fn f(x: Foo) -> Int {`, `Foo` at cols 9-11.
    assert_eq!(d.line, 1);
    assert_eq!(
        d.col, 9,
        "pinned to the `Foo` identifier, not col 0 (whole line)"
    );
    assert_eq!(d.end_col, 12);
}

#[test]
fn binding_not_visible_before_its_line() {
    let src = "fn main() -> Int64 {\n    return x;\n    let x = 5;\n    return x;\n}\n";
    let a = analyze(src);
    // Line 2: `x` at col 12, used before its binding on line 3.
    assert!(
        resolve(&a, 2, 12).is_none(),
        "use before binding should not resolve"
    );
    let r = resolve(&a, 4, 12).expect("x at (4,12) should resolve to the let");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.target_line, 3);
}

/// `definition: false` tells the LSP's go-to-definition handler there is no
/// source declaration to jump to.
#[test]
fn builtin_method_hover_is_definition_false() {
    let src = "\
fn main() -> Int64 {
    let mut a: Array<Int64> = [];
    a.push(1);
    return a.length;
}
";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean source: {:?}",
        a.diagnostics
    );
    // Line 3: `    a.push(1);`: `a` 5, `.` 6, `push` 7-11.
    let r = resolve(&a, 3, 7).expect("push at (3,7) should resolve to a builtin");
    assert_eq!(r.kind, SymbolKind::Method);
    assert_eq!(r.name, "push");
    assert!(!r.definition, "a built-in method has no source declaration");
    assert!(r.hover.contains("array.push(value)"), "hover: {}", r.hover);
}

#[test]
fn builtin_logger_method_hover() {
    let src = "\
fn main() -> Int64 {
    let log = logger(\"app\");
    log.info(\"hi\");
    return 0;
}
";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean source: {:?}",
        a.diagnostics
    );
    // Line 3: `    log.info("hi");`: `log` 5-8, `.` 9, `info` 10-14.
    let r = resolve(&a, 3, 10).expect("info at (3,10) should resolve to a builtin");
    assert_eq!(r.name, "info");
    assert!(!r.definition);
    assert!(
        r.hover.contains("info(logger, message)"),
        "hover: {}",
        r.hover
    );
}

/// `resolve` checks locals before the builtin fallback.
#[test]
fn local_shadows_builtin_method_name() {
    let src = "\
fn main() -> Int64 {
    let push = 5;
    return push;
}
";
    let a = analyze(src);
    let r = resolve(&a, 3, 12).expect("push at (3,12) should resolve to the local");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "push");
    assert!(r.definition, "a local has a real definition");
    assert_eq!(r.hover, "let push: Int64");
}

#[test]
fn member_completions_for_array() {
    let src = "\
fn main() -> Int64 {
    let mut a: Array<Int64> = [];
    a.push(1);
    return a.length;
}
";
    let a = analyze(src);
    // Line 3: `    a.push(1);`, cursor just after the dot (col 7).
    let labels: HashSet<String> = member_completions(&a, 3, 7)
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in ["push", "at", "pop", "length"] {
        assert!(
            labels.contains(expected),
            "array member {expected} missing: {:?}",
            labels
        );
    }
    // `afree` and `alen` were removed; completion must not offer them.
    assert!(!labels.contains("afree"), "afree is gone: {labels:?}");
    assert!(!labels.contains("alen"), "alen is gone: {labels:?}");
    assert!(
        !labels.contains("info"),
        "info is a Logger method, not an array's"
    );
}

#[test]
fn member_completions_for_logger() {
    let src = "\
fn main() -> Int64 {
    let log = logger(\"app\");
    log.info(\"hi\");
    return 0;
}
";
    let a = analyze(src);
    // Line 3: `    log.info("hi");`, cursor just after the dot (col 10).
    let labels: HashSet<String> = member_completions(&a, 3, 10)
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in ["trace", "debug", "info", "warn", "error"] {
        assert!(
            labels.contains(expected),
            "logger member {expected} missing: {:?}",
            labels
        );
    }
    assert!(
        !labels.contains("push"),
        "push is an array method, not a Logger's"
    );
}

#[test]
fn member_completions_empty_without_receiver_type() {
    let src = "fn main() -> Int64 { let a: Array<Int64> = []; return a.length; }\n";
    let a = analyze(src);
    assert!(
        member_completions(&a, 1, 1).is_empty(),
        "no dot → no members"
    );
}

#[test]
fn analyze_linked_resolves_imports() {
    let root =
        "import { double } from \"./lib\"\n\nfn main() -> Int64 {\n    return double(21)\n}\n";
    let lib = "export fn double(x: Int64) -> Int64 {\n    return x * 2\n}\n";

    assert!(
        analyze(root)
            .diagnostics
            .iter()
            .any(|d| d.message.contains("double")),
        "plain analyze should flag the imported name"
    );

    let resolver = vyrn_frontend::loader::MapResolver(
        [("lib.vyrn".to_string(), lib.to_string())]
            .into_iter()
            .collect(),
    );
    let opts = vyrn_frontend::loader::LoadOptions::default();
    let a = vyrn_frontend::analyze_linked(root, "main.vyrn", &opts, &resolver, None);
    assert!(
        a.diagnostics.is_empty(),
        "linked analyze should be clean: {:?}",
        a.diagnostics
    );
    let syms = names(&a);
    assert!(syms.contains("main"));

    let d = a
        .symbols
        .iter()
        .find(|s| s.name == "double")
        .expect("imported symbol indexed");
    assert_eq!(d.file.as_deref(), Some("lib.vyrn"));
    assert_eq!(d.line, 1, "declaration line in the imported file");
    assert_eq!(d.col, 0, "foreign columns are unknown (whole-line)");

    // Line 4: `    return double(21)`, col 13 is inside `double`.
    let r = resolve(&a, 4, 13).expect("call site resolves");
    assert_eq!(r.name, "double");
    assert_eq!(r.target_file.as_deref(), Some("lib.vyrn"));
    assert_eq!(r.target_line, 1);
    assert!(
        r.definition,
        "an imported symbol has a real declaration to jump to"
    );
    assert!(
        r.hover.contains("double"),
        "hover shows the signature: {}",
        r.hover
    );

    let labels: HashSet<String> = completions(&a).into_iter().map(|c| c.label).collect();
    assert!(
        labels.contains("double"),
        "imported names complete: {labels:?}"
    );
}

/// Top-level names are unique program-wide, so the collision is a link error.
#[test]
fn import_collision_errors_but_root_index_survives() {
    let root = "import { helper } from \"./lib\"\n\nfn helper(x: Int64) -> Int64 {\n    return x\n}\n\nfn main() -> Int64 {\n    return helper(1)\n}\n";
    let lib = "export fn helper(x: Int64) -> Int64 {\n    return x + 1\n}\n";
    let resolver = vyrn_frontend::loader::MapResolver(
        [("lib.vyrn".to_string(), lib.to_string())]
            .into_iter()
            .collect(),
    );
    let opts = vyrn_frontend::loader::LoadOptions::default();
    let a = vyrn_frontend::analyze_linked(root, "main.vyrn", &opts, &resolver, None);
    assert!(
        !a.diagnostics.is_empty(),
        "the name collision must be reported"
    );
    // Line 8: `    return helper(1)`, cursor inside `helper`.
    let r = resolve(&a, 8, 13).expect("call site still resolves");
    assert_eq!(r.target_file, None, "resolves to the root declaration");
    assert_eq!(r.target_line, 3, "jumps to the local `fn helper`");
}

#[test]
fn analyze_linked_adopts_foreign_errors() {
    let root = "import { bad } from \"./lib\"\n\nfn main() -> Int64 {\n    return bad(1)\n}\n";
    let lib = "export fn bad(x: Int64) -> Int64 {\n    let a = None\n    return x\n}\n";
    let resolver = vyrn_frontend::loader::MapResolver(
        [("lib.vyrn".to_string(), lib.to_string())]
            .into_iter()
            .collect(),
    );
    let opts = vyrn_frontend::loader::LoadOptions::default();
    let a = vyrn_frontend::analyze_linked(root, "main.vyrn", &opts, &resolver, None);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.message.starts_with("in lib.vyrn:"))
        .expect("foreign error should be adopted with an `in <file>:` prefix");
    assert_eq!(d.line, 0, "foreign diagnostics anchor at line 0");
}

#[test]
fn analyze_linked_missing_module_still_indexes_root() {
    let root = "import { f } from \"./gone\"\n\nfn main() -> Int64 {\n    return 0\n}\n";
    let resolver = vyrn_frontend::loader::MapResolver(Default::default());
    let opts = vyrn_frontend::loader::LoadOptions::default();
    let a = vyrn_frontend::analyze_linked(root, "main.vyrn", &opts, &resolver, None);
    assert!(
        !a.diagnostics.is_empty(),
        "unresolvable import must be reported"
    );
    assert!(
        names(&a).contains("main"),
        "root symbols survive a load failure"
    );
}

/// Member completion offers the matching `impl P for T` methods for a concrete
/// receiver and the bound protocol's methods for a bounded generic.
#[test]
fn member_completion_offers_protocol_methods() {
    let src = "protocol Show {\n    fn show(self) -> String\n}\n\nimpl Show for Int64 {\n    fn show(self) -> String { return self.toString() }\n}\n\nfn describe<T: Show>(x: T) -> String {\n    return x.show()\n}\n\nfn main() -> Int64 {\n    let n: Int64 = 5\n    let s = n.show()\n    return 0\n}\n";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean program: {:?}",
        a.diagnostics
    );

    // Line 15 `    let s = n.show()`: the dot is at col 14.
    let labels: HashSet<String> = member_completions(&a, 15, 15)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        labels.contains("show"),
        "impl method offered for Int64 receiver: {labels:?}"
    );

    // Line 10 `    return x.show()` in `describe<T: Show>`.
    let labels: HashSet<String> = member_completions(&a, 10, 14)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        labels.contains("show"),
        "protocol method offered for bounded T: {labels:?}"
    );

    // The impl method is a user symbol, so go-to-definition works.
    let r = resolve(&a, 15, 15).expect("method name resolves");
    assert_eq!(r.name, "show");
    assert!(r.definition, "user impl method has a source declaration");
    assert!(
        r.hover.contains("show"),
        "hover shows the signature: {}",
        r.hover
    );
}

#[test]
fn member_completion_offers_record_fields() {
    let src = "type User = { age: Int64 where value >= 18, name: String }\nfn main() -> Int64 {\n    let u: User = User { age: 21, name: \"max\" }\n    let a = u.age\n    return 0\n}\n";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean program: {:?}",
        a.diagnostics
    );
    // Line 4 `    let a = u.age`: the dot is at col 14.
    let items = member_completions(&a, 4, 15);
    let by_label: std::collections::HashMap<String, String> =
        items.into_iter().map(|c| (c.label, c.detail)).collect();
    assert!(
        by_label.contains_key("name"),
        "plain field offered: {by_label:?}"
    );
    assert_eq!(
        by_label.get("age").map(String::as_str),
        Some("age: Int64 where value >= 18"),
        "refined field renders as written: {by_label:?}"
    );
}

/// A hole is re-lexed as its own source, so its positions count from the hole.
/// `Parser::binder_pos` answers `(0, 0)` there and such a binder is not
/// indexed, or the editor would point at an unrelated token.
#[test]
fn a_binder_inside_an_interpolation_is_not_a_local() {
    let src = "fn twice(f: fn(Int64) -> Int64, x: Int64) -> Int64 {
    return f(f(x))
}
fn main() -> Int64 {
    let s = \"x=\\{twice(q -> { let t = q + 1
        return t }, 3)}\"
    print(s)
    return 0
}
";
    let a = analyze(src);
    assert!(
        a.diagnostics.is_empty(),
        "clean program: {:?}",
        a.diagnostics
    );
    let names: Vec<&str> = a.locals.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        ["f", "x", "s"],
        "the hole's `q` and `t` are not locals"
    );
    // Without the rule above, the hole's `let t` lands at line 1, column 18,
    // inside `twice`'s signature.
    let lines: Vec<&str> = src.lines().collect();
    for b in &a.locals {
        let at = &lines[b.line - 1][b.col - 1..];
        assert!(
            at.starts_with(b.name.as_str()),
            "{b:?} is not spelled at its position: {at}"
        );
    }
}

/// Every `.vyrn` of the corpus, sorted, with the repository root.
fn corpus() -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["examples", "site", "std", "compiler/vyrn-cli/tests"] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    (root, files)
}

/// Walks the corpus on a 256 MB stack: `site/app/chart.vyrn` is 35 KB of one
/// expression tree, too deep for a debug build on the harness's stack.
fn over_the_corpus(each: fn(&str, &str)) {
    let (root, files) = corpus();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            for p in files {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                let Ok(src) = std::fs::read_to_string(&p) else {
                    continue;
                };
                each(&rel, &src);
            }
        })
        .expect("spawn")
        .join()
        .expect("the corpus scan");
}

/// Every local binding `analyze` gives over the corpus, printed.
///
/// The pin for the editor's locals: a local is not a declaration, so no
/// `vyrn check` byte moves when one is lost. Compare every byte before and after.
#[test]
#[ignore]
fn the_pinned_binders_over_the_corpus() {
    over_the_corpus(|rel, src| {
        let a = analyze(src);
        println!("===== {rel} ({} locals)", a.locals.len());
        for b in &a.locals {
            println!(
                "{rel}:{}:{}:{} {} {:?} fn@{} | {}",
                b.line,
                b.col,
                b.end_col,
                b.name,
                b.kind,
                b.fn_line,
                b.ty.as_ref().map(ToString::to_string).unwrap_or_default()
            );
        }
    });
}

/// Every diagnostic position `analyze` gives over the corpus, printed.
///
/// The pin for `analyze`'s columns: `vyrn check` does not go through this path,
/// so a change to the keyword-column map moves no byte of its stderr. Compare
/// the whole output before and after.
#[test]
#[ignore]
fn the_pinned_columns_over_the_corpus() {
    over_the_corpus(|rel, src| {
        let a = analyze(src);
        println!("===== {rel} ({} diagnostics)", a.diagnostics.len());
        for d in &a.diagnostics {
            println!(
                "{}:{}:{}:{} {} | {}",
                rel, d.line, d.col, d.end_col, d.stage, d.message
            );
        }
    });
}

#[test]
fn member_context_survives_a_non_ascii_receiver() {
    assert!(member_context("caf\u{e9}.", 1, 6));
    assert!(member_context("caf\u{e9}.pu", 1, 8));
    assert!(!member_context("caf\u{e9}", 1, 5));
    assert!(member_context("arr.pu", 1, 7));
    assert!(!member_context("let x = 1", 1, 10));
}

#[test]
fn string_context_counts_unescaped_quotes_before_the_cursor() {
    assert!(string_context("from \"std/", 1, 11));
    assert!(!string_context("let s = \"a\\\"b\" + x", 1, 18));
    assert!(!string_context("let s = 1", 1, 9));
}

#[test]
fn scope_completions_offer_the_locals_declared_above_the_cursor() {
    let src = "\
fn f(n: Int64) -> Int64 {
    let xs = [1, 2]
    let later = 3
    return n
}
";
    let a = analyze(src);
    let labels: HashSet<String> = scope_completions(&a, 2)
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in ["n", "xs", "f"] {
        assert!(labels.contains(expected), "{expected} missing: {labels:?}");
    }
    assert!(
        !labels.contains("later"),
        "a local below the cursor: {labels:?}"
    );
}

#[test]
fn member_completions_on_a_fixed_size_array_offer_only_what_it_accepts() {
    let src = "\
fn main() -> Int64 {
    let xs = [1, 2]
    let n = xs.length
    return n
}
";
    let a = analyze(src);
    let labels: HashSet<String> = member_completions(&a, 3, 16)
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in ["at", "length", "copy"] {
        assert!(labels.contains(expected), "{expected} missing: {labels:?}");
    }
    for refused in ["push", "pop", "swapRemove", "reserve"] {
        assert!(!labels.contains(refused), "{refused} offered: {labels:?}");
    }
}

#[test]
fn hover_on_a_builtin_function_shows_its_contract() {
    let src = "\
fn main() -> Int64 {
    print(1)
    return 0
}
";
    let r = resolve(&analyze(src), 2, 6).expect("print resolves");
    assert_eq!(r.kind, SymbolKind::Function);
    assert!(!r.definition);
    assert!(
        r.signature
            .as_deref()
            .is_some_and(|s| s.starts_with("fn print")),
        "{r:?}"
    );
}

#[test]
fn a_doc_link_to_a_known_name_takes_the_renderers_target() {
    let out = vyrn_frontend::doc_links("see [`b`]", |n| Some(format!("<{n}>")));
    assert_eq!(out, "see <b>");
}

#[test]
fn a_doc_link_to_an_unknown_name_reads_as_code() {
    assert_eq!(vyrn_frontend::doc_links("see [`b`].", |_| None), "see `b`.");
}

#[test]
fn a_doc_link_with_its_own_target_stays_as_written() {
    let doc = "see [`b`](https://example.com) and [a] list";
    assert_eq!(vyrn_frontend::doc_links(doc, |_| Some(String::new())), doc);
}
