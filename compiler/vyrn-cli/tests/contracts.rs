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
const GEN: &str = r#"import { checkContract } from "std/contract"

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
