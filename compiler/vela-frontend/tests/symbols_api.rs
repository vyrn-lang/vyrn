//! Integration tests for the symbol-query API (`vela_frontend::analyze` /
//! `resolve` / `completions`) — the core layer the LSP consumes for hover,
//! go-to-definition, and completion.
//!
//! They guard: the symbol index covers top-level functions/types/variants (and
//! excludes the parser-injected `Value` enum), each symbol has a precise name
//! column derived from the token stream, and a cursor resolves to the right
//! declaration with the right hover text.

use std::collections::HashSet;

use vela_frontend::{analyze, completions, member_completions, resolve, SymbolKind};

/// The real `examples/enum.vela`, located relative to this crate's manifest dir
/// (so the test tracks the actual example, not a stale inline copy).
fn enum_vela() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/enum.vela");
    std::fs::read_to_string(path).expect("examples/enum.vela should exist")
}

fn names(a: &vela_frontend::Analysis) -> HashSet<String> {
    a.symbols.iter().map(|s| s.name.clone()).collect()
}

/// The index covers the user types, every variant, and the functions; the
/// parser-injected built-in `Value` enum (and its `VInt`/`VBool`/`VStr` variants)
/// is filtered out — it has no real source position.
#[test]
fn indexes_enum_example_symbols() {
    let a = analyze(&enum_vela());
    assert!(a.diagnostics.is_empty(), "enum.vela should be clean: {:?}", a.diagnostics);
    let n = names(&a);
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(n.contains(expected), "missing symbol {expected}: {:?}", n);
    }
    for injected in ["Value", "VInt", "VBool", "VStr"] {
        assert!(!n.contains(injected), "injected {injected} should be filtered: {:?}", n);
    }
}

/// Each symbol has a precise (non-zero) name column, derived from the token
/// stream rather than the AST's line-only positions — this is what lets
/// go-to-definition land on the exact name, not the whole line.
#[test]
fn symbols_have_precise_name_columns() {
    let a = analyze(&enum_vela());
    let by_name: std::collections::HashMap<&str, &vela_frontend::Symbol> =
        a.symbols.iter().map(|s| (s.name.as_str(), s)).collect();
    // `type Shape =` on line 4: "type" cols 1-4, space 5, "Shape" cols 6-10.
    let shape = by_name["Shape"];
    assert_eq!(shape.kind, SymbolKind::Type);
    assert_eq!(shape.line, 4);
    assert_eq!(shape.col, 6);
    // `| Circle(Int)` on line 5: "| " then "Circle" cols 7-12.
    let circle = by_name["Circle"];
    assert_eq!(circle.kind, SymbolKind::Variant);
    assert_eq!(circle.line, 5);
    assert_eq!(circle.col, 7);
    // `fn area(...)` on line 10: "fn " then "area" cols 4-7.
    let area = by_name["area"];
    assert_eq!(area.kind, SymbolKind::Function);
    assert_eq!(area.line, 10);
    assert_eq!(area.col, 4);
}

/// Hovering the `Circle` constructor at its call site resolves to the variant
/// declaration (line 5) with a useful detail string.
#[test]
fn resolve_variant_at_call_site() {
    let a = analyze(&enum_vela());
    // Line 19: `    let a = area(Circle(2));` — "Circle" cols 18-23.
    let r = resolve(&a, 19, 18).expect("Circle at (19,18) should resolve");
    assert_eq!(r.kind, SymbolKind::Variant);
    assert_eq!(r.name, "Circle");
    assert_eq!(r.target_line, 5);
    assert_eq!(r.target_col, 7);
    assert_eq!(r.hover, "variant of Shape: Circle(Int64)");
}

/// Hovering `area` at its call site resolves to the function declaration with a
/// full signature (params with names + types, return type).
#[test]
fn resolve_function_at_call_site() {
    let a = analyze(&enum_vela());
    // Line 19: "area" cols 13-16.
    let r = resolve(&a, 19, 13).expect("area at (19,13) should resolve");
    assert_eq!(r.kind, SymbolKind::Function);
    assert_eq!(r.target_line, 10);
    assert_eq!(r.target_col, 4);
    assert_eq!(r.hover, "fn area(s: Shape) -> Int64");
}

/// Hovering a type reference (`s: Shape`) resolves to the type declaration with
/// a rendering of its enum arms.
#[test]
fn resolve_type_at_annotation() {
    let a = analyze(&enum_vela());
    // Line 10: `fn area(s: Shape) -> Int {` — "Shape" cols 12-16.
    let r = resolve(&a, 10, 12).expect("Shape at (10,12) should resolve");
    assert_eq!(r.kind, SymbolKind::Type);
    assert_eq!(r.target_line, 4);
    assert_eq!(r.target_col, 6);
    assert_eq!(r.hover, "type Shape = Circle(Int64) | Rect(Int64, Int64) | Unit");
}

/// A cursor not on an identifier resolves to nothing.
#[test]
fn resolve_returns_none_off_identifier() {
    let a = analyze(&enum_vela());
    // Line 10, col 1 is the `f` of `fn` — a keyword token, not an Ident, so no
    // TokenInfo covers it.
    assert!(resolve(&a, 10, 1).is_none());
}

/// Completion lists every top-level symbol (the client filters by prefix); the
/// injected `Value` family is absent.
#[test]
fn completions_list_top_level() {
    let a = analyze(&enum_vela());
    let labels: HashSet<String> = completions(&a).into_iter().map(|c| c.label).collect();
    for expected in ["Shape", "Circle", "Rect", "Unit", "area", "main"] {
        assert!(labels.contains(expected), "completion missing {expected}: {:?}", labels);
    }
    for injected in ["Value", "VInt", "VBool", "VStr"] {
        assert!(!labels.contains(injected), "injected {injected} leaked into completions");
    }
}

/// A parse error yields no symbols (the parser does not recover) and a single
/// blocking diagnostic — matching `diagnostics()`'s contract.
#[test]
fn parse_error_yields_no_symbols() {
    let src = "fn main() -> Int64 { let x = ; return x; }";
    let a = analyze(src);
    assert!(a.symbols.is_empty());
    assert!(a.tokens.is_empty());
    assert_eq!(a.diagnostics.len(), 1);
    assert_eq!(a.diagnostics[0].stage, "parse");
}

/// `diagnostics()` delegates to `analyze()`, so for a clean file both report
/// no diagnostics. (`Diagnostic` is not `PartialEq`, so we compare counts and
/// the rendered first message rather than the vecs directly.)
#[test]
fn diagnostics_delegate_matches_analyze() {
    let a = analyze(&enum_vela());
    assert_eq!(a.diagnostics.len(), vela_frontend::diagnostics(&enum_vela()).len());
    assert!(a.diagnostics.is_empty());

    // A broken file: both report one parse diagnostic with the same message.
    let bad = "fn main() -> Int64 { let x = ; return x; }";
    let ab = analyze(bad);
    let db = vela_frontend::diagnostics(bad);
    assert_eq!(ab.diagnostics.len(), db.len());
    assert_eq!(ab.diagnostics.len(), 1);
    assert_eq!(ab.diagnostics[0].render(), db[0].render());
}

// ---------------------------------------------------------------------------
// locals: params, lets, for-in vars (hover + go-to-definition on variables)
// ---------------------------------------------------------------------------

/// The real `examples/foreach.vela` — a clean file with an annotated `let`, a
/// mutable unannotated `let`, and `for`-in loop variables.
fn foreach_vela() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/foreach.vela");
    std::fs::read_to_string(path).expect("examples/foreach.vela should exist")
}

/// Hovering a parameter at a use site resolves to the param, with its declared
/// type, and go-to-definition lands on the param name (not the function name).
#[test]
fn resolve_param_at_use_site() {
    let a = analyze(&enum_vela());
    // Line 11: `    return match s {` — `s` is at col 18 (1-based).
    let r = resolve(&a, 11, 18).expect("param s at (11,18) should resolve");
    assert_eq!(r.kind, SymbolKind::Param);
    assert_eq!(r.name, "s");
    // `fn area(s: Shape)` is on line 10; `s` is at col 9.
    assert_eq!(r.target_line, 10);
    assert_eq!(r.target_col, 9);
    assert_eq!(r.hover, "s: Shape");
}

/// Hovering an annotated `let` at a use site resolves to the binding with its
/// declared type.
#[test]
fn resolve_annotated_let() {
    let a = analyze(&foreach_vela());
    assert!(a.diagnostics.is_empty(), "foreach.vela should be clean: {:?}", a.diagnostics);
    // Line 13: `    for s in squares {` — `squares` is at col 14.
    let r = resolve(&a, 13, 14).expect("squares at (13,14) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "squares");
    // `let squares: Array<Int, 5> = ..` is on line 11; name at col 9.
    assert_eq!(r.target_line, 11);
    assert_eq!(r.hover, "let squares: Array<Int64, 5>");
}

/// A mutable unannotated `let` resolves with `let mut <name>: <type>` — the
/// checker's inferred type is retained now, so the hover carries it. (Go-to-def
/// worked before; this just adds the type.)
#[test]
fn resolve_mutable_unannotated_let() {
    let a = analyze(&foreach_vela());
    // Line 14: `        total = total + s;` — the first `total` is at col 9.
    let r = resolve(&a, 14, 9).expect("total at (14,9) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "total");
    // `let mut total = 0;` is on line 12; name at col 13. `0` infers to Int.
    assert_eq!(r.target_line, 12);
    assert_eq!(r.hover, "let mut total: Int64");
}

/// A `for`-in loop variable resolves to its binding line, now with the inferred
/// element type (`for s: Int`).
#[test]
fn resolve_for_var() {
    let a = analyze(&foreach_vela());
    // Line 14: `        total = total + s;` — the loop var `s` is at col 25.
    let r = resolve(&a, 14, 25).expect("for-var s at (14,25) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "s");
    // `for s in squares {` is on line 13; `s` is at col 9. `squares` is
    // `Array<Int, 5>`, so the loop variable is Int.
    assert_eq!(r.target_line, 13);
    assert_eq!(r.hover, "for s: Int64");
}

/// An unannotated `let` whose initializer is a string literal hovers with the
/// inferred `String` type — the checker's retained type, not just the name. Guards
/// the inference path for a non-`Int` scalar (the mutable/`for`-var cases above
/// both happened to infer `Int`).
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
    assert!(a.diagnostics.is_empty(), "clean source: {:?}", a.diagnostics);
    // Line 2: `    let s = "hi";` — `s` is at col 9.
    let r = resolve(&a, 2, 9).expect("s at (2,9) should resolve");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "s");
    assert_eq!(r.target_line, 2);
    assert_eq!(r.hover, "let s: String");
}

/// A local shadows a same-named top-level symbol: the `area` *call* on line 19
/// resolves to the top-level function (no local `area`), confirming the
/// local-then-top-level fallback still reaches top-level names.
#[test]
fn local_falls_back_to_top_level_symbol() {
    let a = analyze(&enum_vela());
    // Line 19: `    let a = area(Circle(2));` — `area` is at col 13.
    let r = resolve(&a, 19, 13).expect("area at (19,13) should resolve");
    assert_eq!(r.kind, SymbolKind::Function);
    assert_eq!(r.target_line, 10);
}

/// A non-exhaustive `match` diagnostic is pinned to the `match` *keyword*
/// column — not left line-only (`col == 0`, which the LSP would squiggle as the
/// whole line including leading spaces and `return`). The squiggle should land
/// on exactly `match`.
#[test]
fn match_exhaustiveness_pinned_to_match_keyword() {
    let src = "\
type T = | A(Int64) | B;
fn f(x: T) -> Int64 {
    let r = match x {
        A(n) => n,
    };
    return r;
}
fn main() -> Int64 { return 0; }
";
    let a = analyze(src);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.message.contains("missing variant"))
        .expect("a non-exhaustive-match diagnostic");
    // `match` is on line 3: `    let r = match x {` — "match" at 1-based cols
    // 13-17, so col=13, end_col=18.
    assert_eq!(d.line, 3);
    assert_eq!(d.col, 13, "pinned to the `match` keyword, not col 0 (whole line)");
    assert_eq!(d.end_col, 18);
}

/// A non-Bool `if` condition diagnostic is pinned to the `if` **keyword**, not
/// left whole-line. The first backtick-quoted token in the message is `if`,
/// which is a reserved word (never a `Tok::Ident`), so the keyword column map
/// resolves it. Guards the generalized pinner's keyword path.
#[test]
fn if_condition_pinned_to_if_keyword() {
    let src = "\
fn main() -> Int64 {
    if 5 {
        print(1);
    }
    return 0;
}
";
    let a = analyze(src);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.message.contains("`if` condition must be Bool"))
        .expect("an if-condition diagnostic");
    // Line 2: `    if 5 {` — 4 spaces, then `if` at 1-based cols 5-6.
    assert_eq!(d.line, 2);
    assert_eq!(d.col, 5, "pinned to the `if` keyword, not col 0 (whole line)");
    assert_eq!(d.end_col, 7);
}

/// An `unknown variable` diagnostic is pinned to the variable **identifier** on
/// the error's line. Guards the generalized pinner's identifier path (a user
/// name, the offending use being on the line).
#[test]
fn unknown_variable_pinned_to_ident() {
    let src = "\
fn main() -> Int64 {
    return x;
}
";
    let a = analyze(src);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.message.contains("unknown variable"))
        .expect("an unknown-variable diagnostic");
    // Line 2: `    return x;` — `x` at 1-based col 12 (4 spaces + "return" +
    // space + x).
    assert_eq!(d.line, 2);
    assert_eq!(d.col, 12, "pinned to the `x` identifier, not col 0 (whole line)");
    assert_eq!(d.end_col, 13);
}

/// A movecheck use-after-consume diagnostic is pinned to the consumed
/// **identifier** on the error's line (the movecheck message backtick-quotes the
/// variable name). Guards that the pinner covers movecheck, not just checker.
#[test]
fn movecheck_use_after_consume_pinned_to_ident() {
    let src = "\
fn main() -> Int64 {
    let x = 5;
    drop x;
    return x;
}
";
    let a = analyze(src);
    let d = a
        .diagnostics
        .iter()
        .find(|d| d.stage == "movecheck" && d.message.contains("already consumed"))
        .expect("a movecheck use-after-consume diagnostic");
    // Line 4: `    return x;` — the offending use `x` is at col 12.
    assert_eq!(d.line, 4);
    assert_eq!(d.col, 12, "pinned to the `x` use, not col 0 (whole line)");
    assert_eq!(d.end_col, 13);
}

/// An `unknown type` diagnostic (a type reference that doesn't resolve) is
/// pinned to the type **identifier** in the annotation. Guards the identifier
/// path for type names (distinct kind from variables, but same mechanism).
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
    // Line 1: `fn f(x: Foo) -> Int {` — `Foo` at 1-based cols 9-11.
    assert_eq!(d.line, 1);
    assert_eq!(d.col, 9, "pinned to the `Foo` identifier, not col 0 (whole line)");
    assert_eq!(d.end_col, 12);
}

/// A local defined AFTER the cursor is not yet in scope — the cursor on the
/// `squares` *binding* name (line 11) must not resolve to itself via a later
/// binding, and a use before the binding resolves to nothing (or to an earlier
/// same-named binding). Here we check the binding's own name resolves (line 11)
/// and that hovering the RHS array literal position (not an ident) is None.
#[test]
fn binding_not_visible_before_its_line() {
    let src = "fn main() -> Int64 {\n    return x;\n    let x = 5;\n    return x;\n}\n";
    let a = analyze(src);
    // Line 2: `    return x;` — `x` used before its binding on line 3. No local
    // with line <= 2 named `x`, and no top-level `x` → resolve returns None.
    // (x at col 12 on line 2: 4 spaces + "return" (5-10) + space (11) + x (12).)
    assert!(resolve(&a, 2, 12).is_none(), "use before binding should not resolve");
    // Line 4: `    return x;` — now the binding on line 3 is in scope.
    let r = resolve(&a, 4, 12).expect("x at (4,12) should resolve to the let");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.target_line, 3);
}

// ---------------------------------------------------------------------------
// method-call resolution + `.foo` member completion (built-in methods)
// ---------------------------------------------------------------------------

/// Hovering a built-in method name (`push` in `a.push(1)`) resolves to a
/// synthesized method resolution: it has hover text but `definition: false` (no
/// source declaration to jump to). Guards the builtin fallback in `resolve` and
/// the `definition` flag the LSP go-to-definition handler checks.
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
    assert!(a.diagnostics.is_empty(), "clean source: {:?}", a.diagnostics);
    // Line 3: `    a.push(1);` — 4 spaces, `a` (5), `.` (6), `push` cols 7-11.
    let r = resolve(&a, 3, 7).expect("push at (3,7) should resolve to a builtin");
    assert_eq!(r.kind, SymbolKind::Method);
    assert_eq!(r.name, "push");
    assert!(!r.definition, "a built-in method has no source declaration");
    assert!(r.hover.contains("push(array, value)"), "hover: {}", r.hover);
}

/// A built-in method called on a Logger (`log.info(..)`) hovers with the log
/// signature — guards a different receiver-type group than the array case.
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
    assert!(a.diagnostics.is_empty(), "clean source: {:?}", a.diagnostics);
    // Line 3: `    log.info("hi");` — 4 spaces, `log` 5-8, `.` 9, `info` 10-14.
    let r = resolve(&a, 3, 10).expect("info at (3,10) should resolve to a builtin");
    assert_eq!(r.name, "info");
    assert!(!r.definition);
    assert!(r.hover.contains("info(logger, message)"), "hover: {}", r.hover);
}

/// A user-defined local of the same name as a built-in wins over the built-in
/// (shadowing) — `resolve` checks locals before the builtin fallback.
#[test]
fn local_shadows_builtin_method_name() {
    let src = "\
fn main() -> Int64 {
    let push = 5;
    return push;
}
";
    let a = analyze(src);
    // `push` here is a local Int, not the array built-in.
    let r = resolve(&a, 3, 12).expect("push at (3,12) should resolve to the local");
    assert_eq!(r.kind, SymbolKind::Local);
    assert_eq!(r.name, "push");
    assert!(r.definition, "a local has a real definition");
    assert_eq!(r.hover, "let push: Int64");
}

/// `member_completions` after `arr.` lists the array methods + `length`, keyed
/// off the receiver's inferred `Array<Int>` type. Guards the receiver-type
/// resolution + builtin-by-type dispatch for arrays.
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
    // Line 3: `    a.push(1);` — cursor just after the dot (col 7).
    let labels: HashSet<String> =
        member_completions(&a, 3, 7).into_iter().map(|c| c.label).collect();
    for expected in ["push", "at", "alen", "afree", "length"] {
        assert!(labels.contains(expected), "array member {expected} missing: {:?}", labels);
    }
    // A Logger method must NOT appear for an array receiver.
    assert!(!labels.contains("info"), "info is a Logger method, not an array's");
}

/// `member_completions` after `log.` lists the five log levels, keyed off the
/// receiver's `Logger` type — guards a non-array receiver-type group.
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
    // Line 3: `    log.info("hi");` — cursor just after the dot (col 10).
    let labels: HashSet<String> =
        member_completions(&a, 3, 10).into_iter().map(|c| c.label).collect();
    for expected in ["trace", "debug", "info", "warn", "error"] {
        assert!(labels.contains(expected), "logger member {expected} missing: {:?}", labels);
    }
    // An array method must NOT appear for a Logger receiver.
    assert!(!labels.contains("push"), "push is an array method, not a Logger's");
}

/// `member_completions` returns nothing when the receiver's type can't be
/// resolved — here the cursor is on a line with no preceding dot — and when
/// the receiver is a top-level function (not a local). Guards the fallbacks.
#[test]
fn member_completions_empty_without_receiver_type() {
    let src = "fn main() -> Int64 { let a: Array<Int64> = []; return a.length; }\n";
    let a = analyze(src);
    // Line 1, col 1 — no dot at/before the cursor → no member context.
    assert!(member_completions(&a, 1, 1).is_empty(), "no dot → no members");
}