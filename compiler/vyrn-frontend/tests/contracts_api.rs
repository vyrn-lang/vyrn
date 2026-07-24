//! RFC-0071 M4 — the editor's contract queries, over the REAL `std/ui:Page` and
//! `std/vyx:Component` declarations.
//!
//! These are the frontend half of the milestone: role → contract resolution,
//! completion, hover, go-to-definition positions, did-you-mean, and the status
//! report `vyrn why --contract` prints. The LSP is a pure adapter over exactly
//! these functions, so a behaviour that is wrong here is wrong in the editor and
//! a behaviour that is right here only has wiring left to get wrong.

use vyrn_frontend::contracts::{
    contract_completions, contract_fixes, contract_member_hover, contract_status, discovered_roles,
    edit_distance, load_contract, role_for, roles_from_manifest, MemberStatus, RoleScope,
};
use vyrn_frontend::loader::{LoadOptions, ModuleResolver};

fn repo(rel: &str) -> String {
    let p = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join(rel)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{rel}: {e}"));
    p.to_string_lossy().replace('\\', "/").replace("//?/", "")
}

/// A plain read-only resolver — the editor's, minus the caches.
struct Disk;
impl ModuleResolver for Disk {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
    fn list(&self, resolved: &str) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = std::fs::read_dir(resolved)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }
}

fn opts() -> LoadOptions {
    LoadOptions {
        std_root: Some(repo("std")),
        ..Default::default()
    }
}

/// `std/ui:Page`, resolved the way the LSP resolves it.
fn page() -> vyrn_frontend::contracts::ContractView {
    load_contract("std/ui", "Page", &repo("examples/bin/server.vyrn"), &opts(), &Disk)
        .expect("std/ui declares contract Page")
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The real declaration resolves, with its module, its file, both members, and
/// every shape RFC-0071 M2b declares them at.
#[test]
fn resolves_the_real_page_contract() {
    let v = page();
    assert_eq!(v.name, "Page");
    assert_eq!(v.module, "std/ui");
    assert!(v.file.ends_with("std/ui.vyrn"), "declaring file: {}", v.file);
    let names: Vec<&str> = v.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["head", "data", "page", "respond"],
        "members in declaration order (RFC-0072 M2 added the two router entry points)"
    );

    // `head` is declared at four shapes; three of them are distinct signatures
    // (RFC-0071 M2b records why `fn head(d: T)` and `fn head(p: P)` are the same
    // one), and all four are OFFERED — the contract's own text is the statement.
    let head = v.member("head").expect("head");
    assert_eq!(head.shapes.len(), 4, "head's declared shapes");
    let spellings: Vec<&str> = head.shapes.iter().map(|s| s.spelling.as_str()).collect();
    assert_eq!(
        spellings,
        vec!["fn() -> Head", "fn(T) -> Head", "fn(P) -> Head", "fn(P, T) -> Head"]
    );
    assert!(head.optional, "head has a default (`= noHead()`)");
    assert!(
        head.doc.as_deref().unwrap_or("").contains("head takes what the view takes"),
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
        "the four data types RFC-0071 M2b settled on"
    );

    // `Page` is CLOSED — that is what makes typo detection total.
    assert!(v.open_rule.is_none(), "Page has no open rule");
}

/// `std/vyx:Component` is the OPEN one, and resolves as such.
#[test]
fn resolves_the_open_component_contract() {
    let v = load_contract(
        "std/vyx",
        "Component",
        &repo("examples/bin/server.vyrn"),
        &opts(),
        &Disk,
    )
    .expect("std/vyx declares contract Component");
    assert!(v.members.is_empty(), "an open contract names nothing");
    let rule = v.open_rule.clone().expect("the open rule");
    assert_eq!(rule.spelling, "fn(..) -> Html");
    assert!(rule.variadic);
    // Nothing to complete: the open slot's names are the application's.
    assert!(contract_completions(&v, &[]).is_empty());
}

/// Go-to-definition lands on the member's NAME in the contract, not the line
/// start — the same precision every other declaration jump has.
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

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// The `vyrn.json` form RFC-0071 specifies (and RFC-0072 inherits).
#[test]
fn roles_come_from_the_manifest() {
    let roles = roles_from_manifest(
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

/// RFC-0072 M2: a role scope may be a RUN of segments, so the audience axis and
/// the role axis compose in one scope instead of one of them silently winning.
#[test]
fn a_role_scope_may_span_the_audience_segment() {
    let roles = roles_from_manifest(
        r#"{ "roles": { "server/api": "std/rpc:Api", "client/api": "std/ui:Page" } }"#,
    );
    assert_eq!(
        role_for("/app/server/api/pastes.vyrn", &roles).map(|r| r.contract.as_str()),
        Some("Api")
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
    // Feature-outer layouts work for the same reason audience does.
    assert_eq!(
        role_for("/app/src/pastes/server/api/x.vyrn", &roles).map(|r| r.contract.as_str()),
        Some("Api")
    );
}

/// Nearest wins — the rule `crate::audience` applies to audience segments,
/// applied to role scopes so the two axes agree about "more specific".
#[test]
fn the_nearest_scope_wins() {
    let roles = roles_from_manifest(
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

/// A project may override the chrome stems for its own layout.
#[test]
fn a_role_may_declare_its_own_exceptions() {
    let roles = roles_from_manifest(
        r#"{ "roles": { "routes": { "contract": "std/ui:Page", "except": ["_shell"] } } }"#,
    );
    assert!(role_for("/app/routes/_shell.vyx", &roles).is_none());
    assert!(
        role_for("/app/routes/layout.vyx", &roles).is_some(),
        "an explicit `except` replaces the default, it does not extend it"
    );
}

/// With no `roles` key — which is every project in this repo today — the role
/// is discovered from the generator call the app already writes. No blessed
/// directory names: the directory comes from the call, the contract from the
/// module the generator was imported from.
#[test]
fn roles_fall_back_to_the_generator_call_site() {
    let roots: Vec<(String, String)> = ["examples/bin/server.vyrn", "examples/bin/client.vyrn"]
        .iter()
        .map(|p| {
            let path = repo(p);
            let src = std::fs::read_to_string(&path).unwrap();
            (path, src)
        })
        .collect();
    let roles = discovered_roles(&roots, &opts(), &Disk);
    assert!(
        roles.iter().any(|r| r.contract == "Page" && r.module == "std/ui"),
        "the pages generator's contract: {roles:?}"
    );
    assert!(
        roles.iter().any(|r| r.contract == "Component" && r.module == "std/vyx"),
        "the components generator's contract: {roles:?}"
    );
    let page = roles.iter().find(|r| r.contract == "Page").unwrap();
    match &page.scope {
        RoleScope::Dir(d) => assert!(d.ends_with("examples/bin/routes"), "resolved dir: {d}"),
        s => panic!("expected a resolved directory, got {s:?}"),
    }
    // And the real page in that directory is governed by it, while the real
    // layout beside it is not.
    assert_eq!(
        role_for(&repo("examples/bin/routes/index.vyx"), &roles).map(|r| r.contract.as_str()),
        Some("Page")
    );
    assert!(role_for(&repo("examples/bin/routes/layout.vyx"), &roles).is_none());
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

/// Every member, every shape, with its doc and a snippet that is the full
/// declaration — the RFC's "so the type is right before the user types".
#[test]
fn completion_offers_every_shape_with_a_full_declaration() {
    let v = page();
    let items = contract_completions(&v, &[]);
    assert_eq!(items.len(), 16, "four members, four shapes each");
    let head0 = items.iter().find(|i| i.snippet.starts_with("export fn head()")).unwrap();
    assert_eq!(head0.label, "head");
    assert_eq!(
        head0.snippet,
        "export fn head() -> Head {\n    return $0\n}",
        "the zero-argument shape is complete as-is"
    );
    assert!(head0.detail.contains("contract `Page` (std/ui)"), "{}", head0.detail);
    assert!(head0.doc.is_some(), "the member's /// doc rides along");

    // A shape with parameters makes both the name and the type a tabstop: the
    // contract's `T` is open, so only the page knows what it really is.
    let head2 = items
        .iter()
        .find(|i| i.snippet.starts_with("export fn head(${1:"))
        .expect("a one-parameter shape");
    assert_eq!(head2.snippet, "export fn head(${1:t}: ${2:T}) -> Head {\n    return $0\n}");

    let data_lazy = items
        .iter()
        .find(|i| i.snippet.contains("-> Lazy<T>"))
        .expect("the lazy shape");
    assert_eq!(data_lazy.label, "data");
    assert_eq!(data_lazy.snippet, "export fn data() -> Lazy<T> {\n    return $0\n}");
}

/// A member the page already exports is not offered again.
#[test]
fn completion_drops_what_the_page_already_wrote() {
    let v = page();
    let items = contract_completions(&v, &["data".to_string(), "page".to_string()]);
    assert!(items.iter().all(|i| i.label == "head" || i.label == "respond"), "data and page are written");
    assert_eq!(items.len(), 8);
}

/// Required members sort before optional ones. `Page`'s two are both optional,
/// so this is proved on a contract that has one of each.
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
    let v = load_contract("./c", "Both", &root, &opts(), &Disk).expect("the contract");
    let items = contract_completions(&v, &[]);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["b", "a"], "required first, declaration order within");
    assert!(items[0].required);
    assert!(!items[1].required);
    assert!(items[0].sort < items[1].sort, "sortText carries the order");
}

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

/// Hover names the type, the doc, and the contract — RFC-0071's three.
#[test]
fn hover_names_the_shape_the_doc_and_the_contract() {
    let v = page();
    let h = contract_member_hover(&v, "head").expect("head is a member");
    assert!(h.contains("fn head: fn() -> Head"), "the shape:\n{h}");
    assert!(h.contains("fn head: fn(P, T) -> Head"), "every shape:\n{h}");
    assert!(h.contains("Document head contributions"), "the doc:\n{h}");
    assert!(h.contains("member of contract `Page` (std/ui)"), "the contract:\n{h}");
    assert!(contract_member_hover(&v, "helper").is_none(), "a helper is not a member");
}

// ---------------------------------------------------------------------------
// Did-you-mean
// ---------------------------------------------------------------------------

/// The near-miss the RFC's whole argument rests on: `dta` is one edit from
/// `data`, and the editor can offer the rename.
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
    // The edit spans the NAME, so applying it renames `dta` and nothing else.
    let line = src.lines().nth(1).unwrap();
    let got: String = line
        .chars()
        .skip(fixes[0].col - 1)
        .take(fixes[0].end_col - fixes[0].col)
        .collect();
    assert_eq!(got, "dta");
}

/// A private helper is outside the contract entirely — no fix, no noise.
#[test]
fn a_private_helper_yields_no_fix() {
    let v = page();
    assert!(contract_fixes(&v, "fn dta() -> Int64 {\n    return 1\n}\n").is_empty());
}

/// `laod` lands outside the threshold (RFC-0071 M2 records this: the distance to
/// `data` is 3 and `load` is no longer a member). It is still an ERROR — that
/// comes from `checkContract` — but there is nothing honest to suggest.
#[test]
fn a_far_miss_yields_no_suggestion() {
    let v = page();
    assert!(contract_fixes(&v, "export fn laod() -> Int64 {\n    return 1\n}\n").is_empty());
}

/// The Rust `edit_distance` and `std/strings:editDistance` are two
/// implementations of one function, so they are pinned together — by RUNNING the
/// Vyrn one and comparing, not by asserting numbers twice.
///
/// The Vyrn program returns the number of pairs where the two disagree, with the
/// Rust answers baked in, so a divergence in either direction fails here.
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
    ];
    let mut body = String::from("import { editDistance } from \"std/strings\"\nfn main() -> Int64 {\n    let mut bad = 0\n");
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
    let program = vyrn_frontend::load(&body, &path, &opts(), &Disk)
        .unwrap_or_else(|d| panic!("the cross-check program must compile: {d:?}"));
    let disagreements =
        vyrn_frontend::interp::run(&program).expect("the cross-check program must run");
    assert_eq!(
        disagreements, 0,
        "std/strings:editDistance and contracts::edit_distance disagree on {disagreements} of {} pairs",
        PAIRS.len()
    );
}

// ---------------------------------------------------------------------------
// Status (`vyrn why --contract`)
// ---------------------------------------------------------------------------

/// The real `examples/bin/routes/index.vyx` page satisfies `Page`, and the
/// report says WHICH shape each member matched — the fact the generator reads
/// off a declaration instead of scanning a body.
#[test]
fn status_reports_the_matched_shape() {
    let v = page();
    let vyx = std::fs::read_to_string(repo("examples/bin/routes/index.vyx")).unwrap();
    let script = script_of(&vyx);
    let st = contract_status(&v, &script);
    let head = st.iter().find(|e| e.name == "head").unwrap();
    assert_eq!(
        head.status,
        MemberStatus::Satisfied { shape: 0 },
        "index.vyx writes `fn head() -> Head` — shape 0"
    );
    let data = st.iter().find(|e| e.name == "data").unwrap();
    assert_eq!(
        data.status,
        MemberStatus::Satisfied { shape: 1 },
        "`Lazy<Array<Paste>>` is the second `data` shape — laziness is a TYPE"
    );
    // Private helpers never appear: the closed rule is about the public surface.
    assert!(st.iter().all(|e| e.name != "isLoading"));
}

/// Absent-and-optional, absent-and-required, wrong shape, and unknown — the
/// four statuses `vyrn why` has to be able to print.
#[test]
fn status_covers_every_class() {
    let v = page();
    let st = contract_status(&v, "");
    assert_eq!(st[0].status, MemberStatus::Defaulted, "head defaults to noHead()");
    assert_eq!(st[1].status, MemberStatus::Defaulted, "data defaults to noQuery()");

    let st = contract_status(&v, "export fn head() -> Int64 {\n    return 1\n}\n");
    assert_eq!(
        st[0].status,
        MemberStatus::Mismatched {
            found: "fn() -> Int64".into()
        }
    );

    let st = contract_status(&v, "export fn dta() -> Int64 {\n    return 1\n}\n");
    let unknown = st.iter().find(|e| e.name == "dta").unwrap();
    assert_eq!(
        unknown.status,
        MemberStatus::Unknown {
            did_you_mean: Some("data".into())
        }
    );
}

/// An open contract admits any NAME but still constrains SHAPE.
#[test]
fn status_checks_the_open_rule() {
    let v = load_contract(
        "std/vyx",
        "Component",
        &repo("examples/bin/server.vyrn"),
        &opts(),
        &Disk,
    )
    .unwrap();
    let st = contract_status(
        &v,
        "import { Html } from \"std/html\"\n\
         export fn anythingAtAll() -> Html {\n    return h()\n}\n\
         export fn wrong() -> Int64 {\n    return 1\n}\n",
    );
    assert_eq!(st[0].name, "anythingAtAll");
    assert_eq!(st[0].status, MemberStatus::OpenMatched);
    assert_eq!(
        st[1].status,
        MemberStatus::OpenMismatched {
            found: "fn() -> Int64".into()
        }
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The `<script>` body of a `.vyx`, which is ordinary Vyrn.
fn script_of(vyx: &str) -> String {
    let open = vyx.find("<script>").expect("a script block") + "<script>".len();
    let close = vyx.find("</script>").expect("a closed script block");
    vyx[open..close].to_string()
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
