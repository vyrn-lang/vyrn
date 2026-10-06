//! Tests module contracts end to end through the `vyrn` binary: a
//! `contract` declaration, `contractOf` and `moduleInterface` reflection,
//! `std/contract:checkContract`, and the diagnostics a generator bakes into the
//! app, which prints them. The generator cache is off so a stale entry never
//! masks a change.

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
    // path joining cannot parse.
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

/// Each `gen fn` reflects a module and bakes `checkContract`'s issues into a
/// function returning them.
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

/// Builds the fixture tree, runs it, and returns stdout.
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

/// Satisfies both contracts. `head` is omitted: it has a default, so it is
/// optional.
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
fn every_contract_diagnostic_class_is_produced() {
    // All five rows at once, so their order is pinned too: members in
    // declaration order, then unknown exports.
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

    // A member's type parameters are open, so `Array<Int64>` satisfies
    // `Array<T>`. `data` still appears below, as the suggestion for `dta`.
    assert!(
        !lines.iter().any(|l| l.contains("| `data`")),
        "`data` should have matched:\n{out}"
    );

    assert_eq!(
        lines[0],
        "contract.type | `title` must be `fn() -> String`, found `fn() -> Int64` \
         (contract `Page`, ./gen)"
    );
    // `head` is optional and silent; `slug` is required.
    assert_eq!(
        lines[1],
        "contract.missing | module must export `slug`: `fn() -> String` (contract `Page`, ./gen)"
    );
    // Within Damerau-Levenshtein distance 2 of a member.
    assert_eq!(
        lines[2],
        "contract.unknown.didYouMean | unknown export `dta` — did you mean `data`? \
         (contract `Page`, ./gen)"
    );
    // Close to nothing, and still reported: a closed contract ignores no export.
    assert_eq!(
        lines[3],
        "contract.unknown | unknown export `helper` (contract `Page`, ./gen is closed)"
    );
    // Under the open rule the name is free, the shape is not.
    assert_eq!(
        lines[4],
        "contract.open | `sync` must match the open rule `fn(String) -> String`, \
         found `fn(Int64) -> String` (contract `Api`, ./gen)"
    );
}

#[test]
fn an_open_contract_admits_any_name_of_the_right_shape() {
    // `laod` would be a typo under a closed contract; under `Api` it is a
    // procedure name.
    let api = r#"export fn laod(input: String) -> String {
    return input
}
"#;
    let out = run_fixture("open", CLEAN_PAGE, api);
    assert_eq!(out.trim(), "", "{out}");
}

/// Builds the `Widget` and `Views` fixture tree, runs it, and returns stdout.
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

#[test]
fn an_fn_member_with_a_default_is_optional() {
    let out = run_widget_fixture(
        "optfn",
        "export fn view() -> String {\n    return \"v\"\n}\n",
    );
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
    // A named member's arity is part of what the name promises, and a closed
    // contract's typo detection depends on it.
    let dir = scratch("namedvariadic");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P { fn head(..) -> String }\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("open rule cannot have a default"), "{all}");
}

#[test]
fn contract_of_is_comptime_only_and_has_no_native_lowering() {
    // Nothing about a contract survives into an emitted module, so the checker
    // refuses `contractOf` at runtime, and every command prints its sentence.
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
        .arg("emit-wat")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("`contractOf` is only available during generation"),
        "expected a comptime-only refusal, got:\n{all}"
    );
}

/// Alternative signatures: a member name declared more than once is satisfied
/// by any one shape. The generator reports the issues and which shape matched.
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
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
        (
            1,
            "export fn render(a: Int64) -> String {\n    return \"\"\n}\n",
        ),
        (
            2,
            "export fn render(a: Int64, b: String) -> String {\n    return \"\"\n}\n",
        ),
    ] {
        let out = run_alt(&format!("alt{arity}"), module);
        assert!(
            !out.contains("contract."),
            "shape {arity} must satisfy `render`:\n{out}"
        );
        assert!(
            out.contains(&format!("matched={arity}")),
            "the generator learns WHICH shape it got (expected {arity}):\n{out}"
        );
    }
}

#[test]
fn an_export_matching_no_alternative_names_all_of_them() {
    let out = run_alt(
        "altbad",
        "export fn render(a: Int64, b: String, c: Bool) -> String {\n    return \"\"\n}\n",
    );
    let hits = out.matches("contract.type").count();
    assert_eq!(
        hits, 1,
        "one issue for one member, not one per alternative:\n{out}"
    );
    assert!(
        out.contains("must be one of"),
        "the wording admits several shapes:\n{out}"
    );
    assert!(
        out.contains("fn() -> String") && out.contains("fn(T, R) -> String"),
        "every alternative is named:\n{out}"
    );
}

#[test]
fn a_default_on_any_alternative_makes_the_name_optional() {
    // Only the first alternative carries a default. Optionality belongs to the
    // name, because an absent export is absent at every shape.
    let out = run_alt(
        "altmissing",
        "fn helper() -> String {\n    return \"\"\n}\n",
    );
    assert!(
        !out.contains("contract.missing"),
        "the member is optional:\n{out}"
    );
    assert!(
        out.contains("matched=-1"),
        "and reported as not supplied:\n{out}"
    );
}

#[test]
fn a_name_cannot_change_member_form_between_alternatives() {
    let dir = scratch("altform");
    std::fs::write(
        dir.join("app.vyrn"),
        "contract P {\n    fn head() -> String\n    let head: String\n}\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must be refused:\n{err}");
    assert!(
        err.contains("both a value and a function"),
        "naming the reason:\n{err}"
    );
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
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must be refused:\n{err}");
    assert!(err.contains("at most one"), "naming the reason:\n{err}");
}

/// `vyrn why --contract` on the repo's own app. The `routes/` role comes from the
/// generator call site: nothing in `examples/bin` declares a `roles` key.
#[test]
fn why_contract_reports_the_matched_shape_of_a_real_page() {
    let page = repo_dir("examples/bin/app/routes/index.vyx");
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(&page)
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "why must answer:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("role: directory") && text.contains("examples/bin/app/routes"),
        "the role, discovered from the generator call site:\n{text}"
    );
    assert!(
        text.contains("contract: Page (std/ui)"),
        "the resolved contract:\n{text}"
    );
    assert!(
        text.contains("std/ui.vyrn"),
        "where it is declared:\n{text}"
    );
    // Laziness is a type, so the report reads `data`'s shape off the declaration.
    assert!(
        text.contains("ok        head: shape 1 of 4"),
        "head's shape:\n{text}"
    );
    assert!(
        text.contains("ok        data: shape 2 of 4"),
        "data's shape:\n{text}"
    );
    assert!(
        !text.contains("isLoading"),
        "a helper is not part of the surface:\n{text}"
    );
    // A `.vyx`'s `<template>` is its page, though the `<script>` never names it.
    assert!(
        text.contains("ok        page: the `<template>` compiles to it"),
        "the page the template writes:\n{text}"
    );
    assert!(
        !text.contains("page: absent"),
        "and it is never absent in a file whose form guarantees it:\n{text}"
    );
    // `respond` is the alternative to `page`, which the template supplies.
    assert!(
        text.contains("default   respond: absent, optional"),
        "{text}"
    );
}

/// A `.http` projection sits in the api directory but is not a procedure module:
/// `std/rpc`'s scan skips a dotted stem, so it is in no role.
#[test]
fn why_contract_refuses_to_grade_a_projection() {
    let proj = repo_dir("examples/bin/server/api/pastes.http.vyrn");
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(&proj)
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("no contract: this file is in no role"),
        "{text}"
    );
    assert!(
        text.contains("its stem is dotted"),
        "naming the reason:\n{text}"
    );
    assert!(
        !text.contains("matches the open rule"),
        "and never grades `routes()`/`feeds()` against `Api`:\n{text}"
    );
    assert!(
        !out.status.success(),
        "no contract governs it, which is not the question asked"
    );

    // The rule is the dot, not the directory.
    let procs = repo_dir("examples/bin/server/api/pastes.vyrn");
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(&procs)
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}");
    assert!(text.contains("contract: Api (std/rpc)"), "{text}");
    assert!(
        text.contains("ok        recent: matches the open rule"),
        "{text}"
    );
}

#[test]
fn why_contract_says_a_layout_is_in_no_role() {
    let layout = repo_dir("examples/bin/app/routes/layout.vyx");
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(&layout)
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("no contract: this file is in no role"),
        "{text}"
    );
    assert!(
        !out.status.success(),
        "and that is not the answer that was asked for"
    );
}

#[test]
fn why_contract_reports_every_status_class() {
    let dir = scratch("why");
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"why\", \"main\": \"app.vyrn\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pages/index.vyx"),
        "<script>\n\
         import { Query, query } from \"std/ui\"\n\
         export fn data() -> Query<Int64> {\n    return query(one)\n}\n\
         export fn dta() -> Int64 {\n    return 1\n}\n\
         export fn helper() -> Int64 {\n    return 2\n}\n\
         fn one() -> Int64 {\n    return 1\n}\n\
         </script>\n\
         <template>\n<h1>hi</h1>\n</template>\n",
    )
    .unwrap();

    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("pages/index.vyx"))
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("ok        data: shape 1 of 4"),
        "satisfied:\n{text}"
    );
    assert!(
        text.contains("default   head: absent, optional"),
        "defaulted:\n{text}"
    );
    assert!(
        text.contains("ok        page: the `<template>` compiles to it"),
        "synthesized by the form, which is a fifth status class:\n{text}"
    );
    assert!(
        text.contains("UNKNOWN   dta: unknown export `dta` — did you mean `data`?"),
        "the did-you-mean:\n{text}"
    );
    assert!(
        text.contains(
            "UNKNOWN   helper: unknown export `helper` (contract `Page`, std/ui is closed)"
        ),
        "the far miss, still reported and never silent:\n{text}"
    );
    assert!(
        text.contains("objection(s); the generator that consumes this module is the gate"),
        "`why` reports; the generator refuses:\n{text}"
    );
}

#[test]
fn why_contract_resolves_the_open_component_contract() {
    let widget = repo_dir("examples/bin/app/widgets/CreateForm.vyx");
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(&widget)
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}");
    assert!(text.contains("contract: Component (std/vyx)"), "{text}");
}

/// `why --contract` reports what `std/contract` reports, so it agrees with the
/// generator. An export of the wrong arity is a mismatch, not shape 1.
#[test]
fn why_contract_reports_what_the_generator_reports() {
    let dir = scratch("whygate");
    std::fs::create_dir_all(dir.join("screens")).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export contract Screen {\n\
         \x20   fn title() -> String = untitled()\n\
         \x20   fn title(n: Int64) -> String\n\
         \x20   fn budget() -> Int64\n\
         }\n\
         \n\
         export contract Panel {\n\
         \x20   fn *(..) -> String\n\
         }\n\
         \n\
         fn untitled() -> String {\n    return \"untitled\"\n}\n\
         \n\
         export gen fn screens(dir: String) -> String {\n\
         \x20   return \"export fn mounted() -> Int64 {\n    return 1\n}\n\"\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"why\", \"main\": \"app.vyrn\", \"roles\": { \"screens\": \"./gen:Screen\", \"panels\": \"./gen:Panel\" } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "fn main() -> Int64 {\n    return 0\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("screens/home.vyrn"),
        "export fn title(a: Int64, b: Int64) -> String {\n    return \"home\"\n}\n",
    )
    .unwrap();
    let why = |file: &str| {
        let out = vyrn()
            .arg("why")
            .arg("--contract")
            .arg(dir.join(file))
            .output()
            .expect("why");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        text
    };
    let text = why("screens/home.vyrn");
    assert!(
        text.contains("MISMATCH  title: `title` must be one of `fn() -> String` or `fn(Int64) -> String`, found `fn(Int64, Int64) -> String`"),
        "the arity counts:\n{text}"
    );
    assert!(
        text.contains("MISSING   budget: module must export `budget`"),
        "{text}"
    );

    std::fs::create_dir_all(dir.join("panels")).unwrap();
    std::fs::write(
        dir.join("panels/side.vyrn"),
        "export fn label() -> String {\n    return \"side\"\n}\n\n\
         export fn width() -> Int64 {\n    return 1\n}\n",
    )
    .unwrap();
    let text = why("panels/side.vyrn");
    assert!(
        text.contains("ok        label: matches the open rule — fn(..) -> String"),
        "{text}"
    );
    assert!(
        text.contains("MISMATCH  width: `width` must match the open rule `fn(..) -> String`, found `fn() -> Int64`"),
        "{text}"
    );
}

/// Writes a project whose `screens` role is `Screen`, with `types.vyrn`
/// exporting `Item` and a `screens/` page body of the caller's choosing.
fn linked_project(tag: &str, page: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("screens")).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export contract Screen {\n    fn item() -> Item\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("types.vyrn"), "export type Item = { id: Int64 }\n").unwrap();
    std::fs::write(
        dir.join("vyrn.json"),
        "{ \"name\": \"why\", \"main\": \"app.vyrn\", \"roles\": { \"screens\": \"./gen:Screen\" } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "fn main() -> Int64 {\n    return 0\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("screens/home.vyrn"), page).unwrap();
    dir
}

/// `why --contract` judges the linked module, as `moduleInterface` does: a
/// page returning `m.Item` through `import * as m` is `Item`, not a mismatch.
#[test]
fn why_contract_reads_the_linked_module() {
    let dir = linked_project(
        "whylink",
        "import * as m from \"../types\"\n\n\
         export fn item() -> m.Item {\n    return m.Item { id: 1 }\n}\n",
    );
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("screens/home.vyrn"))
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("ok        item: shape 1 of 1"), "{text}");
    assert!(!text.contains("MISMATCH"), "{text}");
}

/// A page that does not link prints the loader's diagnostics and exits 1, with
/// no contract report.
#[test]
fn why_contract_prints_the_diagnostics_of_a_page_that_does_not_compile() {
    let dir = linked_project(
        "whybroken",
        "import * as m from \"../absent\"\n\n\
         export fn item() -> m.Item {\n    return m.Item { id: 1 }\n}\n",
    );
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("screens/home.vyrn"))
        .output()
        .expect("why");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("home.vyrn:0:0: cannot load"), "{err}");
}

/// Without a `vyrn.json`, `why --contract` finds the app root the editor finds:
/// the nearest directory holding a generator root, not the file's directory.
#[test]
fn why_contract_finds_the_editors_app_root() {
    let dir = scratch("whyroot");
    std::fs::create_dir_all(dir.join("screens")).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export contract Screen {\n    fn title() -> String\n}\n\n\
         export gen fn pages(dir: String) -> String {\n    return \"\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "import { pages } from \"./gen\"\n\
         import { mounted } from pages(\"./screens\")\n\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("screens/home.vyrn"),
        "export fn title() -> String {\n    return \"home\"\n}\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("why")
        .arg("--contract")
        .arg(dir.join("screens/home.vyrn"))
        .output()
        .expect("why");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("contract: Screen"), "{text}");
}
