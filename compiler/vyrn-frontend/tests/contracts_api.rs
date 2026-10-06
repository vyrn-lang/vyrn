//! The editor's contract queries over the real `std/ui:Page` and
//! `std/vyx:Component` declarations: role resolution, completion, hover,
//! definition positions and did-you-mean. The LSP is a pure adapter over these
//! functions.

mod common;

use vyrn_frontend::contracts::{
    contract_completions, contract_fixes, contract_member_hover, edit_distance, load_contract,
    role_for, roles_from_manifest, synthesized_members, RoleScope,
};
use vyrn_frontend::loader::{DiskResolver, LoadOptions};

fn repo(rel: &str) -> String {
    let p = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join(rel)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{rel}: {e}"));
    p.to_string_lossy().replace('\\', "/").replace("//?/", "")
}

fn opts() -> LoadOptions {
    LoadOptions {
        std_root: Some(repo("std")),
        ..Default::default()
    }
}

/// [`opts`] with shared expansions, as a compile loads.
fn shared_opts() -> LoadOptions {
    LoadOptions {
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..opts()
    }
}

/// `std/ui:Page`, resolved the way the LSP resolves it.
fn page() -> vyrn_frontend::contracts::ContractView {
    load_contract(
        "std/ui",
        "Page",
        &repo("examples/bin/server.vyrn"),
        &opts(),
        &DiskResolver,
    )
    .expect("std/ui declares contract Page")
}

#[test]
fn resolves_the_real_page_contract() {
    let v = page();
    assert_eq!(v.name, "Page");
    assert_eq!(v.module, "std/ui");
    assert!(
        v.file.ends_with("std/ui.vyrn"),
        "declaring file: {}",
        v.file
    );
    let names: Vec<&str> = v.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["head", "data", "page", "respond"],
        "members in declaration order"
    );

    // Four shapes, all offered, though `fn head(d: T)` and `fn head(p: P)` are
    // one signature.
    let head = v.member("head").expect("head");
    assert_eq!(head.shapes.len(), 4, "head's declared shapes");
    let spellings: Vec<&str> = head.shapes.iter().map(|s| s.spelling.as_str()).collect();
    assert_eq!(
        spellings,
        vec![
            "fn() -> Head",
            "fn(T) -> Head",
            "fn(P) -> Head",
            "fn(P, T) -> Head"
        ]
    );
    assert!(head.optional, "head has a default (`= noHead()`)");
    assert!(
        head.doc
            .as_deref()
            .unwrap_or("")
            .contains("head takes what the view takes"),
        "the member's own /// doc: {:?}",
        head.doc
    );

    let data = v.member("data").expect("data");
    let spellings: Vec<&str> = data.shapes.iter().map(|s| s.spelling.as_str()).collect();
    assert_eq!(
        spellings,
        vec![
            "fn() -> Query<T>",
            "fn() -> Lazy<T>",
            "fn() -> ParamQuery<P, T>",
            "fn() -> ParamLazy<P, T>",
        ],
        "the four data types"
    );

    // A closed contract makes typo detection total.
    assert!(v.open_rule.is_none(), "Page has no open rule");
}

#[test]
fn resolves_the_open_component_contract() {
    let v = load_contract(
        "std/vyx",
        "Component",
        &repo("examples/bin/server.vyrn"),
        &opts(),
        &DiskResolver,
    )
    .expect("std/vyx declares contract Component");
    assert!(v.members.is_empty(), "an open contract names nothing");
    let rule = v.open_rule.clone().expect("the open rule");
    assert_eq!(rule.spelling, "fn(..) -> Html");
    assert!(rule.variadic);
    // Nothing to complete: the open slot's names are the application's.
    assert!(contract_completions(&v, &[]).is_empty());
}

/// Go-to-definition lands on the member's name, not the line start.
#[test]
fn members_carry_their_declaration_position() {
    let v = page();
    let src = std::fs::read_to_string(&v.file).unwrap();
    for m in &v.members {
        assert!(m.line > 0, "{} has a line", m.name);
        assert!(m.col > 0, "{} has a name column", m.name);
        let line = src.lines().nth(m.line - 1).expect("the declaration line");
        let got: String = line
            .chars()
            .skip(m.col - 1)
            .take(m.end_col - m.col)
            .collect();
        assert_eq!(got, m.name, "the column span covers the name on {line:?}");
    }
}

/// `roles_from_manifest` takes a parsed document: a parse failure is not its
/// business.
fn roles_from_text(json: &str) -> Vec<vyrn_frontend::contracts::Role> {
    roles_from_manifest(&vyrn_frontend::schema::parse_json(json).unwrap())
}

#[test]
fn roles_come_from_the_manifest() {
    let roles = roles_from_text(
        r#"{ "name": "app", "roles": { "routes": "std/ui:Page", "widgets": "std/vyx:Component" } }"#,
    );
    assert_eq!(roles.len(), 2);
    let page = roles.iter().find(|r| r.contract == "Page").unwrap();
    assert_eq!(page.scope, RoleScope::Segment("routes".into()));
    assert_eq!(page.module, "std/ui");
    // The chrome default: a layout is not a page.
    assert!(page.except.iter().any(|e| e == "layout"));

    let r = role_for("/app/routes/index.vyx", &roles).expect("a page is in the role");
    assert_eq!(r.contract, "Page");
    assert!(
        role_for("/app/routes/layout.vyx", &roles).is_none(),
        "a layout has no contract to be a member of"
    );
    assert!(
        role_for("/app/routes/error.vyx", &roles).is_none(),
        "nor does an error page"
    );
    assert!(
        role_for("/app/store.vyrn", &roles).is_none(),
        "a module outside every role is governed by nothing"
    );
    assert_eq!(
        role_for("/app/widgets/CreateForm.vyx", &roles).map(|r| r.contract.as_str()),
        Some("Component")
    );
}

/// A role scope may be a run of segments, so the audience and role axes compose
/// in one scope instead of one silently winning.
#[test]
fn a_role_scope_may_span_the_audience_segment() {
    let roles = roles_from_text(
        r#"{ "roles": { "server/api": "std/rpc:Api", "client/api": "std/ui:Page" } }"#,
    );
    assert_eq!(
        role_for("/app/server/api/pastes.vyrn", &roles).map(|r| r.contract.as_str()),
        Some("Api")
    );
    assert!(
        role_for("/app/server/api/pastes.http.vyrn", &roles).is_none(),
        "a dotted stem is a projection over the modules beside it, not one of them — \
         the same question `std/rpc`'s own scan asks"
    );
    assert_eq!(
        role_for("/app/client/api/other.vyrn", &roles).map(|r| r.contract.as_str()),
        Some("Page"),
        "the same inner segment under a different audience is a different role"
    );
    assert!(
        role_for("/app/api/loose.vyrn", &roles).is_none(),
        "a run matches consecutively or not at all"
    );
    // A feature-outer layout matches too.
    assert_eq!(
        role_for("/app/src/pastes/server/api/x.vyrn", &roles).map(|r| r.contract.as_str()),
        Some("Api")
    );
}

/// The rule `crate::audience` applies to audience segments, so the two axes
/// agree on "more specific".
#[test]
fn the_nearest_scope_wins() {
    let roles = roles_from_text(
        r#"{ "roles": { "routes": "std/ui:Page", "widgets": "std/vyx:Component" } }"#,
    );
    assert_eq!(
        role_for("/app/routes/admin/widgets/Panel.vyx", &roles).map(|r| r.contract.as_str()),
        Some("Component"),
        "the widgets directory is nearer the file than the routes directory above it"
    );
    assert_eq!(
        role_for("/app/widgets/admin/routes/Page.vyx", &roles).map(|r| r.contract.as_str()),
        Some("Page")
    );
}

#[test]
fn a_role_may_declare_its_own_exceptions() {
    let roles = roles_from_text(
        r#"{ "roles": { "routes": { "contract": "std/ui:Page", "except": ["_shell"] } } }"#,
    );
    assert!(role_for("/app/routes/_shell.vyx", &roles).is_none());
    assert!(
        role_for("/app/routes/layout.vyx", &roles).is_some(),
        "an explicit `except` replaces the default, it does not extend it"
    );
}

/// The snippet is the full declaration, so the type is right before the user
/// types.
#[test]
fn completion_offers_every_shape_with_a_full_declaration() {
    let v = page();
    let items = contract_completions(&v, &[]);
    assert_eq!(items.len(), 16, "four members, four shapes each");
    let head0 = items
        .iter()
        .find(|i| i.snippet.starts_with("export fn head()"))
        .unwrap();
    assert_eq!(head0.label, "head");
    assert_eq!(
        head0.snippet, "export fn head() -> Head {\n    return $0\n}",
        "the zero-argument shape is complete as-is"
    );
    assert!(
        head0.detail.contains("contract `Page` (std/ui)"),
        "{}",
        head0.detail
    );
    assert!(head0.doc.is_some(), "the member's /// doc rides along");

    // The type is a tabstop too: the contract's `T` is open, so only the page
    // knows it.
    let head2 = items
        .iter()
        .find(|i| i.snippet.starts_with("export fn head(${1:"))
        .expect("a one-parameter shape");
    assert_eq!(
        head2.snippet,
        "export fn head(${1:t}: ${2:T}) -> Head {\n    return $0\n}"
    );

    let data_lazy = items
        .iter()
        .find(|i| i.snippet.contains("-> Lazy<T>"))
        .expect("the lazy shape");
    assert_eq!(data_lazy.label, "data");
    assert_eq!(
        data_lazy.snippet,
        "export fn data() -> Lazy<T> {\n    return $0\n}"
    );
}

#[test]
fn completion_drops_what_the_page_already_wrote() {
    let v = page();
    let items = contract_completions(&v, &["data".to_string(), "page".to_string()]);
    assert!(
        items
            .iter()
            .all(|i| i.label == "head" || i.label == "respond"),
        "data and page are written"
    );
    assert_eq!(items.len(), 8);
}

/// Uses its own contract because `Page` has no required member.
#[test]
fn required_members_sort_first() {
    let dir = scratch("order");
    write(
        &dir.join("c.vyrn"),
        "export contract Both {\n\
         /// optional\n\
         fn a() -> Int64 = zero()\n\
         /// required\n\
         fn b() -> Int64\n\
         }\n\
         fn zero() -> Int64 { return 0 }\n",
    );
    let root = dir.join("c.vyrn").to_string_lossy().replace('\\', "/");
    let v = load_contract("./c", "Both", &root, &opts(), &DiskResolver).expect("the contract");
    let items = contract_completions(&v, &[]);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["b", "a"],
        "required first, declaration order within"
    );
    assert!(items[0].required);
    assert!(!items[1].required);
    assert!(items[0].sort < items[1].sort, "sortText carries the order");
}

#[test]
fn hover_names_the_shape_the_doc_and_the_contract() {
    let v = page();
    let h = contract_member_hover(&v, "head").expect("head is a member");
    assert!(h.contains("fn head: fn() -> Head"), "the shape:\n{h}");
    assert!(h.contains("fn head: fn(P, T) -> Head"), "every shape:\n{h}");
    assert!(h.contains("Document head contributions"), "the doc:\n{h}");
    assert!(
        h.contains("member of contract `Page` (std/ui)"),
        "the contract:\n{h}"
    );
    assert!(
        contract_member_hover(&v, "helper").is_none(),
        "a helper is not a member"
    );
}

#[test]
fn a_near_miss_export_yields_a_rename() {
    let v = page();
    let src = "import { Query } from \"std/ui\"\n\
               export fn dta() -> Query<Int64> {\n    return q()\n}\n";
    let fixes = contract_fixes(&v, src);
    assert_eq!(fixes.len(), 1, "{fixes:?}");
    assert_eq!(fixes[0].from, "dta");
    assert_eq!(fixes[0].to, "data");
    assert_eq!(fixes[0].line, 2);
    // The edit spans the name only.
    let line = src.lines().nth(1).unwrap();
    let got: String = line
        .chars()
        .skip(fixes[0].col - 1)
        .take(fixes[0].end_col - fixes[0].col)
        .collect();
    assert_eq!(got, "dta");
}

#[test]
fn a_private_helper_yields_no_fix() {
    let v = page();
    assert!(contract_fixes(&v, "fn dta() -> Int64 {\n    return 1\n}\n").is_empty());
}

/// `laod` is 3 edits from `data`, past the threshold. `checkContract` still
/// reports the error; there is only no suggestion.
#[test]
fn a_far_miss_yields_no_suggestion() {
    let v = page();
    assert!(contract_fixes(&v, "export fn laod() -> Int64 {\n    return 1\n}\n").is_empty());
}

/// The Rust `edit_distance` and `std/strings:editDistance` implement one
/// function. The Vyrn program returns how many pairs disagree with the Rust
/// answers baked into it.
#[test]
fn edit_distance_matches_the_vyrn_one() {
    const PAIRS: &[(&str, &str)] = &[
        ("", ""),
        ("data", "data"),
        ("dta", "data"),
        ("dat", "data"),
        ("adta", "data"),
        ("laod", "data"),
        ("load", "data"),
        ("ab", "ba"),
        ("head", ""),
        ("heaad", "head"),
        ("Haed", "Head"),
        ("component", "contract"),
        ("tïtlë", "title"),
    ];
    let mut body = String::from(
        "import { editDistance } from \"std/strings\"\nfn main() -> Int64 {\n    let mut bad = 0\n",
    );
    for (a, b) in PAIRS {
        body.push_str(&format!(
            "    if editDistance(\"{a}\", \"{b}\") != {} {{\n        bad = bad + 1\n    }}\n",
            edit_distance(a, b)
        ));
    }
    body.push_str("    return bad\n}\n");
    let dir = scratch("editdist");
    let root = dir.join("m.vyrn");
    write(&root, &body);
    let path = root.to_string_lossy().replace('\\', "/");
    let program = vyrn_lower::load(&body, &path, &shared_opts(), &DiskResolver, None)
        .unwrap_or_else(|d| panic!("the cross-check program must compile: {d:?}"));
    let disagreements = common::run_compiled(&program).expect("the cross-check program must run");
    assert_eq!(
        disagreements, 0,
        "std/strings:editDistance and contracts::edit_distance disagree on {disagreements} of {} pairs",
        PAIRS.len()
    );
}

/// `Component` names no members, so there is no absence to report. The rule reads
/// the contract's declarations, never a table of generator names.
#[test]
fn a_component_vyx_has_no_members_to_synthesize() {
    let v = load_contract(
        "std/vyx",
        "Component",
        &repo("examples/bin/server.vyrn"),
        &opts(),
        &DiskResolver,
    )
    .unwrap();
    let path = repo("examples/bin/app/widgets/CreateForm.vyx");
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(synthesized_members(&v, &path, &src).is_empty());
}

/// A `.vyx` still being written has no view, and the report must not invent one.
#[test]
fn a_templateless_vyx_synthesizes_nothing() {
    let v = page();
    assert!(synthesized_members(&v, "/app/routes/half.vyx", "<script>\n</script>\n").is_empty());
    assert_eq!(
        synthesized_members(
            &v,
            "/app/routes/half.vyx",
            "<template>\n<p></p>\n</template>\n"
        ),
        vec!["page".to_string()],
        "a template with no script at all is still a view"
    );
    assert!(
        synthesized_members(
            &v,
            "/app/routes/half.vyx",
            "<script>\nlet s = \"<template>\"\n</script>\n"
        )
        .is_empty(),
        "a template tag inside the script is a string, not a section"
    );
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vyrn_contracts_api_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(path: &std::path::Path, text: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).expect("parent dir");
    }
    std::fs::write(path, text).expect("write fixture");
}
