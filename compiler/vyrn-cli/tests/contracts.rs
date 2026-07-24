//! Integration tests for module contracts (RFC-0071 M1), driven through the real
//! `vyrn` binary.
//!
//! The point of the RFC is that a convention becomes a *declaration*, and that
//! checking it is ordinary library code — so the test exercises the whole path
//! end to end rather than any one layer:
//!
//!   `contract` declaration -> `contractOf` reflection -> `moduleInterface`
//!   reflection -> `std/contract:checkContract` -> diagnostics
//!
//! One generator declares a CLOSED contract (`Page`) and an OPEN one (`Api`) and
//! emits, for each checked module, a function returning the issues it found. The
//! app prints them, so the assertions are on real end-user text produced by a
//! real generator run — not on an in-process API.
//!
//! Generation runs with the cache disabled so a stale entry never masks a change.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    // Windows `canonicalize` returns a `\\?\` verbatim path, which the loader's
    // own path joining cannot parse (see 84b78d8) — strip it.
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c.env("VYRN_STD", repo_dir("std"));
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_contract_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The generator: a closed `Page` contract, an open `Api` contract, and a
/// `gen fn` per contract that reflects a module and bakes `checkContract`'s
/// issues into a function returning them.
const GEN: &str = r#"import { checkContract, suppliesMember } from "std/contract"

/// What a page module may export.
export contract Page {
    /// This page's data, resolved before render.
    fn data() -> Array<T>
    /// The page's title.
    fn title() -> String
    /// Document head contributions for this page.
    let head: String = ""
    /// The page's slug.
    fn slug() -> String
}

/// Every export is a procedure: one String in, one String out. Names here are
/// the application's vocabulary, so there is nothing to enumerate.
export contract Api {
    /// A procedure.
    fn *(input: String) -> String
}

/// A contract whose members are all optional, exercising the M2 `fn` default.
export contract Widget {
    /// The widget's label.
    fn label() -> String = "untitled"
}

/// An open contract that constrains the RETURN type only — views legitimately
/// differ in arity, so enumerating one would say nothing true.
export contract Views {
    /// A view.
    fn *(..) -> String
}

fn report(name: String, issues: Array<Issue>) -> String {
    let mut out = "export fn " + name + "() -> Array<String> {\n"
    out = out + "    let mut out: Array<String> = []\n"
    for i in issues {
        out = out + "    out.push(\"" + i.key + " | " + i.message + "\")\n"
    }
    out = out + "    return out\n}\n"
    return out
}

export gen fn pageReport(path: String) -> String {
    let iface = moduleInterface(path)
    return report("pageIssues", checkContract(iface, contractOf(Page)))
}

export gen fn apiReport(path: String) -> String {
    let iface = moduleInterface(path)
    return report("apiIssues", checkContract(iface, contractOf(Api)))
}

export gen fn widgetReport(path: String) -> String {
    let iface = moduleInterface(path)
    let mut out = report("widgetIssues", checkContract(iface, contractOf(Widget)))
    out = out + report("viewIssues", checkContract(iface, contractOf(Views)))
    let has = suppliesMember(iface, contractOf(Widget), "label")
    return out + "export fn widgetHasLabel() -> Bool {\n    return " + has.toString() + "\n}\n"
}
"#;

const APP: &str = r#"import { pageReport, apiReport } from "./gen"
import { pageIssues } from pageReport("./page")
import { apiIssues } from apiReport("./api")

fn main() -> Int64 {
    for m in pageIssues() {
        print(m)
    }
    for m in apiIssues() {
        print(m)
    }
    return 0
}
"#;

/// Build the fixture tree and run it; returns stdout.
fn run_fixture(tag: &str, page: &str, api: &str) -> String {
    let dir = scratch(tag);
    std::fs::write(dir.join("gen.vyrn"), GEN).unwrap();
    std::fs::write(dir.join("app.vyrn"), APP).unwrap();
    std::fs::write(dir.join("page.vyrn"), page).unwrap();
    std::fs::write(dir.join("api.vyrn"), api).unwrap();
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    assert!(
        out.status.success(),
        "run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// A module that satisfies both contracts. `head` is omitted — it has a default,
/// so it is optional and its absence is NOT a problem.
const CLEAN_PAGE: &str = r#"export fn data() -> Array<Int64> {
    return [1]
}
export fn title() -> String {
    return "t"
}
export fn slug() -> String {
    return "s"
}
fn helper() -> String {
    return ""
}
"#;

const CLEAN_API: &str = r#"export fn ping(input: String) -> String {
    return input
}
export fn echo(input: String) -> String {
    return input
}
"#;

#[test]
fn a_conforming_module_produces_no_issues() {
    let out = run_fixture("clean", CLEAN_PAGE, CLEAN_API);
    assert_eq!(out.trim(), "", "expected no issues, got:\n{out}");
}

#[test]
fn a_private_helper_is_outside_the_contract() {
    // `helper` in CLEAN_PAGE is module-private. A closed contract constrains the
    // module's PUBLIC surface only — a page still needs local helpers.
    let out = run_fixture("private", CLEAN_PAGE, CLEAN_API);
    assert!(!out.contains("helper"), "{out}");
}

#[test]
fn every_diagnostic_class_in_the_rfcs_table_is_produced() {
    // One module hitting all five rows at once, so their relative order is
    // pinned too: members in declaration order, then unknown exports.
    let page = r#"export fn data() -> Array<Int64> {
    return [1]
}
export fn title() -> Int64 {
    return 0
}
export fn dta() -> String {
    return ""
}
export fn helper() -> String {
    return ""
}
"#;
    let api = r#"export fn ping(input: String) -> String {
    return input
}
export fn sync(n: Int64) -> String {
    return ""
}
"#;
    let out = run_fixture("all", page, api);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 5, "{out}");

    // `data() -> Array<Int64>` satisfies `fn data() -> Array<T>`: a member's
    // type parameters are open, so any instantiation is admitted. (`data` still
    // appears below, as the did-you-mean *suggestion* for `dta`.)
    assert!(
        !lines.iter().any(|l| l.contains("| `data`")),
        "`data` should have matched:\n{out}"
    );

    // Type mismatch.
    assert_eq!(
        lines[0],
        "contract.type | `title` must be `fn() -> String`, found `fn() -> Int64` \
         (contract `Page`, ./gen)"
    );
    // Required member absent (`head` is optional and is silent; `slug` is not).
    assert_eq!(
        lines[1],
        "contract.missing | module must export `slug`: `fn() -> String` (contract `Page`, ./gen)"
    );
    // Unknown export within Damerau-Levenshtein distance 2 of a member.
    assert_eq!(
        lines[2],
        "contract.unknown.didYouMean | unknown export `dta` — did you mean `data`? \
         (contract `Page`, ./gen)"
    );
    // Unknown export that is close to nothing. THE row that matters most: an
    // unrecognized export is reported, never ignored.
    assert_eq!(
        lines[3],
        "contract.unknown | unknown export `helper` (contract `Page`, ./gen is closed)"
    );
    // Open-rule shape mismatch: the name is free, the shape is not.
    assert_eq!(
        lines[4],
        "contract.open | `sync` must match the open rule `fn(String) -> String`, \
         found `fn(Int64) -> String` (contract `Api`, ./gen)"
    );
}

#[test]
fn an_open_contract_admits_any_name_of_the_right_shape() {
    // `laod` would be a typo under a closed contract; under `Api` it is simply a
    // procedure named `laod`, which is correct — procedure names are the
    // application's vocabulary and cannot be enumerated.
    let api = r#"export fn laod(input: String) -> String {
    return input
}
"#;
    let out = run_fixture("open", CLEAN_PAGE, api);
    assert_eq!(out.trim(), "", "{out}");
}

#[test]
fn std_contract_unit_tests_run_green() {
    // `std/contract`'s own inline suite: the type-spelling matcher (where the
    // per-member type-parameter openness actually lives) and the signature
    // notation both sides of a mismatch message are built with.
    let module = repo_dir("std").join("contract.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std/contract unit tests failed:\n{combined}"
    );
    assert!(
        combined.contains("3 passed, 0 failed"),
        "expected 3 green tests:\n{combined}"
    );
}

// ---- M2: optional `fn` members and the variadic open rule ------------------

/// Build the M2 fixture tree and run it; returns stdout.
fn run_widget_fixture(tag: &str, module: &str) -> String {
    let dir = scratch(tag);
    std::fs::write(dir.join("gen.vyrn"), GEN).unwrap();
    std::fs::write(dir.join("page.vyrn"), CLEAN_PAGE).unwrap();
    std::fs::write(dir.join("api.vyrn"), CLEAN_API).unwrap();
    std::fs::write(dir.join("mod.vyrn"), module).unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        r#"import { widgetReport } from "./gen"
import { widgetIssues, viewIssues, widgetHasLabel } from widgetReport("./mod")

fn main() -> Int64 {
    print("supplies=\{widgetHasLabel()}")
    for m in widgetIssues() {
        print("widget " + m)
    }
    for m in viewIssues() {
        print("view " + m)
    }
    return 0
}
"#,
    )
    .unwrap();
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    assert!(
        out.status.success(),
        "run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

#[test]
fn an_fn_member_with_a_default_is_optional() {
    // `fn label() -> String = "untitled"` — the module omits it, and its absence
    // is silent, exactly as a `let` member's default already made it. Without the
    // default the same module would be told it must export `label`.
    let out = run_widget_fixture("optfn", "export fn view() -> String {\n    return \"v\"\n}\n");
    assert!(!out.contains("widget contract.missing"), "{out}");
    assert!(out.contains("supplies=false"), "{out}");
}

#[test]
fn supplies_member_answers_the_question_a_name_hunt_used_to() {
    let out = run_widget_fixture(
        "supplies",
        "export fn label() -> String {\n    return \"L\"\n}\n",
    );
    assert!(out.contains("supplies=true"), "{out}");
}

#[test]
fn a_variadic_open_rule_constrains_the_return_type_only() {
    // Views legitimately differ in arity — zero props, four props — so the open
    // rule says the one thing that is true of all of them.
    let out = run_widget_fixture(
        "variadic",
        "export fn a() -> String {\n    return \"a\"\n}\n\
         export fn b(x: Int64, y: Bool) -> String {\n    return \"b\"\n}\n\
         export fn c() -> Int64 {\n    return 0\n}\n",
    );
    let views: Vec<&str> = out.lines().filter(|l| l.starts_with("view ")).collect();
    assert_eq!(views.len(), 1, "only `c` should fail:\n{out}");
    assert_eq!(
        views[0],
        "view contract.open | `c` must match the open rule `fn(..) -> String`, \
         found `fn() -> Int64` (contract `Views`, ./gen)"
    );
}

#[test]
fn a_named_member_may_not_leave_its_parameters_open() {
    // `(..)` is the open rule's alone: a named member's arity is part of what the
    // name promises, so accepting it there would weaken the very thing that makes
    // a closed contract's typo detection total.
    let dir = scratch("namedvariadic");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P { fn head(..) -> String }\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("only the open rule"), "{all}");
}

#[test]
fn the_open_rule_may_not_have_a_default() {
    let dir = scratch("opendefault");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P { fn *(a: String) -> String = \"\" }\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("open rule cannot have a default"), "{all}");
}

#[test]
fn contract_of_is_comptime_only_and_has_no_native_lowering() {
    // Nothing about a contract survives into an emitted module (RFC-0071), so
    // reaching `contractOf` at runtime is a clear compile error rather than a
    // link failure — the same rule `moduleInterface` already follows.
    let dir = scratch("nolower");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P { let head: String = \"\" }\n\
         fn main() -> Int64 {\n\
         let c = contractOf(P)\n\
         print(c.name)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("emit-ir")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("compile-time reflection (RFC-0071)"),
        "expected a comptime-only refusal, got:\n{all}"
    );
}

// ===========================================================================
// RFC-0071 M2b — alternative signatures.
//
// A member NAME may be declared more than once; the repeats are alternatives,
// and a module satisfies the member by matching any ONE of them. This is what
// `head` needed: a page's shape genuinely varies (data, params, both, neither),
// and one signature per member left a real page unable to write it at all.
// ===========================================================================

/// A generator whose `Shaped` contract declares `render` at three shapes and
/// reports both the issues AND which alternative each module matched.
const ALT_GEN: &str = r#"import { checkContract, matchedMember } from "std/contract"

/// One name, three shapes.
export contract Shaped {
    /// Render this thing, taking whatever it needs.
    fn render() -> String = ""
    fn render(a: T) -> String
    fn render(a: T, b: R) -> String
}

fn report(name: String, issues: Array<Issue>) -> String {
    let mut out = "export fn " + name + "() -> Array<String> {\n    let mut xs: Array<String> = []\n"
    for iss in issues {
        out = out + "    xs.push(\"" + iss.key + ": " + iss.message + "\")\n"
    }
    return out + "    return xs\n}\n"
}

export gen fn shapedReport(path: String) -> String {
    let iface = moduleInterface(path)
    let mut out = report("shapedIssues", checkContract(iface, contractOf(Shaped)))
    let m = matchedMember(iface, contractOf(Shaped), "render")
    return out + "export fn shapedMatch() -> Int64 {\n    return " + m.toString() + "\n}\n"
}
"#;

const ALT_APP: &str = r#"import { shapedReport } from "./gen"
import { shapedIssues, shapedMatch } from shapedReport("./mod")

fn main() -> Int64 {
    for m in shapedIssues() {
        print(m)
    }
    print("matched=" + shapedMatch().toString())
    return 0
}
"#;

fn run_alt(tag: &str, module: &str) -> String {
    let dir = scratch(tag);
    std::fs::write(dir.join("gen.vyrn"), ALT_GEN).unwrap();
    std::fs::write(dir.join("app.vyrn"), ALT_APP).unwrap();
    std::fs::write(dir.join("mod.vyrn"), module).unwrap();
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
    )
}

#[test]
fn any_one_alternative_satisfies_the_member() {
    for (arity, module) in [
        (0, "export fn render() -> String {\n    return \"\"\n}\n"),
        (1, "export fn render(a: Int64) -> String {\n    return \"\"\n}\n"),
        (2, "export fn render(a: Int64, b: String) -> String {\n    return \"\"\n}\n"),
    ] {
        let out = run_alt(&format!("alt{arity}"), module);
        assert!(!out.contains("contract."), "shape {arity} must satisfy `render`:\n{out}");
        assert!(
            out.contains(&format!("matched={arity}")),
            "the generator learns WHICH shape it got (expected {arity}):\n{out}"
        );
    }
}

#[test]
fn an_export_matching_no_alternative_names_all_of_them() {
    // Three shapes, none of them three-argument: one issue, not three.
    let out = run_alt(
        "altbad",
        "export fn render(a: Int64, b: String, c: Bool) -> String {\n    return \"\"\n}\n",
    );
    let hits = out.matches("contract.type").count();
    assert_eq!(hits, 1, "one issue for one member, not one per alternative:\n{out}");
    assert!(out.contains("must be one of"), "the wording admits several shapes:\n{out}");
    assert!(
        out.contains("fn() -> String") && out.contains("fn(T, R) -> String"),
        "every alternative is named:\n{out}"
    );
}

#[test]
fn a_default_on_any_alternative_makes_the_name_optional() {
    // `render`'s FIRST alternative carries a default, so omitting the export
    // entirely is legal — optionality is a property of the name, because an
    // absent export is absent at every shape.
    let out = run_alt("altmissing", "fn helper() -> String {\n    return \"\"\n}\n");
    assert!(!out.contains("contract.missing"), "the member is optional:\n{out}");
    assert!(out.contains("matched=-1"), "and reported as not supplied:\n{out}");
}

#[test]
fn a_name_cannot_change_member_form_between_alternatives() {
    // Alternatives are alternatives, not a change of kind: a `let` and a `fn` of
    // one name is a contradiction and is refused where it is written.
    let dir = scratch("altform");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P {\n    fn head() -> String\n    let head: String\n}\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must be refused:\n{err}");
    assert!(err.contains("both a value and a function"), "naming the reason:\n{err}");
}

#[test]
fn a_contract_still_has_at_most_one_open_rule() {
    let dir = scratch("altopen");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P {\n    fn *(..) -> String\n    fn *(..) -> Int64\n}\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("run vyrn");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must be refused:\n{err}");
    assert!(err.contains("at most one"), "naming the reason:\n{err}");
}
