//! Integration tests for the checking libraries (RFC-0100), driven through the
//! real `vyrn` binary.
//!
//! RFC-0099 gave a generator a way to SAY something. This is the first consumer:
//! `std/hints` (policy — configuration and waivers) and `std/vyx-hints` (rules
//! for `.vyx` components). Neither is a compiler feature, and the test that
//! matters most here is the last one — a hint library written outside `std`,
//! over a file format that is not `.vyx` and not Vyrn, reaching for exactly the
//! same two imports and getting exactly the same configuration and waivers. If
//! that test needed anything `std/vyx-hints` has and a third party does not, the
//! mechanism would be a `vyx-hints`-shaped hole rather than a mechanism.

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
    c.env_remove("VYRN_DENY_WARNINGS");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_hints_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_in(dir: &Path, cmd: &str, root: &str) -> Run {
    let out = vyrn()
        .arg(cmd)
        .arg(dir.join(root))
        .output()
        .expect("run vyrn");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        stderr: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    }
}

// ---- the two libraries' own unit suites ------------------------------------

/// Run one std module's inline `test` blocks and assert the green count.
fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_dir(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(
        combined.contains(expected),
        "expected `{expected}`:\n{combined}"
    );
}

#[test]
fn std_hints_unit_tests_run_green() {
    unit_tests_green("std/hints.vyrn", "11 passed, 0 failed");
}

#[test]
fn std_vyx_hints_unit_tests_run_green() {
    // One row per rule: the fixture that fires it, and the near miss that must
    // not. The near miss is the half that catches an over-eager rule.
    unit_tests_green("std/vyx-hints.vyrn", "27 passed, 0 failed");
}

// ---- `.vyx` rules, end to end ----------------------------------------------

/// A widget with two faults a rule can prove: an `<img>` with no `alt` and no
/// intrinsic size, and a `@click` on a `<div>`.
const WIDGET: &str = r#"<script>
props { url: String, caption: String }
</script>

<template>
<div class="card">
    <img :src="url"/>
    <div @click="open()">{{ caption }}</div>
</div>
</template>
"#;

/// The same widget with every fault fixed — the near miss, at the level of a
/// whole build: a clean corpus must produce a clean build.
const CLEAN_WIDGET: &str = r#"<script>
props { url: String, caption: String }
</script>

<template>
<div class="card">
    <img :src="url" alt="" width="320" height="180"/>
    <button @click="open()">{{ caption }}</button>
</div>
</template>
"#;

const APP: &str = r#"import { vyxHints } from "std/vyx-hints"
import * as hints from vyxHints("./widgets")

fn main() -> Int64 {
    print(hints.vyxHintsChecked())
    return 0
}
"#;

const APP_CONFIGURED: &str = r#"import { vyxHintsConfigured } from "std/vyx-hints"
import * as hints from vyxHintsConfigured("./widgets", "./vyrn.json")

fn main() -> Int64 {
    print(hints.vyxHintsChecked())
    return 0
}
"#;

/// A project: one widget, and the root that runs the checks over it.
fn project(tag: &str, widget: &str, app: &str, manifest: Option<&str>) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("widgets")).unwrap();
    std::fs::write(dir.join("widgets/Card.vyx"), widget).unwrap();
    std::fs::write(dir.join("app.vyrn"), app).unwrap();
    if let Some(m) = manifest {
        std::fs::write(dir.join("vyrn.json"), m).unwrap();
    }
    dir
}

#[test]
fn a_rule_reports_at_the_line_and_column_of_the_vyx_file() {
    let dir = project("anchor", WIDGET, APP, None);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_eq!(
        r.code, 0,
        "advice rides a build that succeeded:\n{}",
        r.stderr
    );
    assert_eq!(r.stdout, "1\n", "the program ran, and said what it checked");
    // `widgets/Card.vyx` line 7 is the `<img>`; column 16 is its first attribute
    // value. The author never sees the generated module, so a report against it
    // would be unactionable.
    assert!(
        r.stderr
            .contains("widgets/Card.vyx:7:16: warning: a11y/img-alt:"),
        "anchored in the .vyx, at the attribute:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("perf/img-size:") && r.stderr.contains("a11y/click-target:"),
        "every rule that fired is reported:\n{}",
        r.stderr
    );
}

#[test]
fn a_clean_component_earns_a_silent_build() {
    let dir = project("clean", CLEAN_WIDGET, APP, None);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout, "1\n");
    assert_eq!(r.stderr, "", "nothing to say about correct markup");
}

#[test]
fn hints_change_neither_the_exit_code_nor_a_byte_of_program_output() {
    // The invariant inherited from RFC-0099, restated for a real rule set: the
    // advice is advice.
    let warned = run_in(
        &project("same_warned", WIDGET, APP, None),
        "run",
        "app.vyrn",
    );
    let clean = run_in(
        &project("same_clean", CLEAN_WIDGET, APP, None),
        "run",
        "app.vyrn",
    );
    assert_eq!(warned.code, clean.code);
    assert_eq!(warned.stdout, clean.stdout);
    assert!(!warned.stderr.is_empty() && clean.stderr.is_empty());
}

#[test]
fn deny_warnings_is_how_a_project_refuses_them() {
    let dir = project("deny", WIDGET, APP, None);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .arg("--deny-warnings")
        .output()
        .expect("run vyrn");
    assert!(!out.status.success(), "refused");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "never ran");
}

#[test]
fn configuration_turns_a_rule_off_and_moves_another_to_error() {
    // The requirement that makes this a library rather than a fixed list: a
    // project changes the rules without editing the library. The manifest is
    // the home because it ignores keys it does not know, so this is one file
    // and no new one.
    let manifest = r#"{
  "hints": {
    "perf/img-size": "off",
    "a11y/click-target": "off",
    "a11y/img-alt": "error"
  }
}
"#;
    let dir = project("config", WIDGET, APP_CONFIGURED, Some(manifest));
    let r = run_in(&dir, "run", "app.vyrn");
    assert_ne!(r.code, 0, "the raised rule failed the build:\n{}", r.stderr);
    assert!(
        r.stderr.contains("widgets/Card.vyx:7:16: a11y/img-alt:"),
        "at error severity, with no marker:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("perf/img-size") && !r.stderr.contains("a11y/click-target"),
        "the disabled rules said nothing:\n{}",
        r.stderr
    );
}

#[test]
fn a_broken_hints_config_is_a_refusal_not_a_silent_no_op() {
    // `find_manifest`'s discipline, in a library: an unreadable policy is not
    // the empty policy. Every one of these must FAIL, because each one would
    // otherwise report "0 problems" over a project that never got checked.
    let cases = [
        (
            "trailing comma",
            "{\n  \"hints\": {\n    \"a11y/img-alt\": \"error\",\n  }\n}\n",
        ),
        (
            "unknown level",
            "{ \"hints\": { \"a11y/img-alt\": \"shout\" } }",
        ),
        ("wrong shape", "{ \"hints\": [\"a11y/img-alt\"] }"),
    ];
    for (what, manifest) in cases {
        let dir = project("badconfig", WIDGET, APP_CONFIGURED, Some(manifest));
        let r = run_in(&dir, "run", "app.vyrn");
        assert_ne!(r.code, 0, "{what} refused:\n{}", r.stderr);
        // Either reader may be the one that catches it: the CLI reads the same
        // manifest for its own keys and refuses a document that does not parse
        // before the generator ever runs. Both refuse; neither shrugs.
        assert!(
            r.stderr.contains("is not a usable hints config")
                || r.stderr.contains("is not valid JSON"),
            "{what} says why:\n{}",
            r.stderr
        );
    }
    // And a config that is simply not there is the same answer.
    let dir = project("noconfig", WIDGET, APP_CONFIGURED, None);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_ne!(r.code, 0, "a missing config is refused:\n{}", r.stderr);
    assert!(
        r.stderr.contains("cannot read the hints config"),
        "{}",
        r.stderr
    );
}

#[test]
fn an_inline_handler_fails_the_build_with_no_flag() {
    // The one place the library judges the OUTPUT broken rather than improvable:
    // the template already spells this `@click`, and `onclick` needs
    // `script-src 'unsafe-inline'` to run at all.
    let widget = "<script>\nprops { caption: String }\n</script>\n\n<template>\n<div onclick=\"open()\">{{ caption }}</div>\n</template>\n";
    let dir = project("inline", widget, APP, None);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_ne!(r.code, 0, "refused:\n{}", r.stderr);
    assert_eq!(r.stdout, "", "the program never ran");
    assert!(
        r.stderr
            .contains("widgets/Card.vyx:6:15: sec/inline-handler:"),
        "in the library's own words, at the attribute:\n{}",
        r.stderr
    );
}

#[test]
fn a_waiver_in_the_component_drops_one_report_and_only_that_one() {
    let widget = "<script>\nprops { body: String, other: String }\n</script>\n\n<template>\n<div>\n    <!-- vyrn-ignore sec/raw-html: rendered from the repo's own markdown -->\n    <p v-html=\"body\"></p>\n    <p v-html=\"other\"></p>\n</div>\n</template>\n";
    let dir = project("waiver", widget, APP, None);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        !r.stderr.contains("Card.vyx:8:"),
        "the waived line says nothing:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("Card.vyx:9:") && r.stderr.contains("sec/raw-html:"),
        "the next one still does:\n{}",
        r.stderr
    );
}

// ---- a third-party hint library, over a format that is not `.vyx` ----------

/// A hint library nobody in `std` wrote, for a file format nobody in `std`
/// knows. It imports `std/diag` and `std/hints` and NOTHING else that
/// `std/vyx-hints` imports — no `std/vyx`, no template parser, no privilege.
/// Its rules, its codes, its severities and its config key are its own.
const SQL_HINTS: &str = r#"import { reportHere, Severity } from "std/diag"
import { hint, noPolicy, policyOf, HintPolicy } from "std/hints"
import { contains, slice } from "std/strpred"
import { toLower } from "std/strings"

/// A parsed policy, or the refusal that explains why there is none.
type SqlPolicy = { policy: HintPolicy, err: String }

/// `sqlHints(path, config)` — two rules over a `.sql` file, one per line.
export gen fn sqlHints(path: String, config: String) -> String {
    let src = match readFile(path) {
        Ok(t) => t,
        Err(e) => "",
    }
    let cfgText = match readFile(config) {
        Ok(t) => t,
        Err(e) => "",
    }
    let cfg = match policyOf(cfgText, "sqlHints") {
        Ok(v) => SqlPolicy { policy: v, err: "" },
        Err(e) => SqlPolicy { policy: noPolicy(), err: e.copy() },
    }
    if cfg.err != "" {
        return reportHere(Error, "sql-hints: `\{config}` is not a usable config — \{cfg.err}")
    }
    let pol = cfg.policy
    let mut out = ""
    let mut line = 1
    let mut start = 0
    let mut i = 0
    let n = src.byteLength
    while i <= n {
        // A line ends at a newline, or at the end of a file that does not end
        // with one. The empty run after a trailing newline is not a line.
        if (i == n && i > start) || (i < n && src[i] == '\n') {
            let text = toLower(sqlSlice(src, start, i))
            if contains(text, "select *") {
                out = out
                + hint(pol, "sql/select-star", Warning, src, path, line, 1,
                    "`SELECT *` ships every column, including the ones added later")
            }
            if contains(text, "delete from") && contains(text, "where") == false {
                out = out
                + hint(pol, "sql/unbounded-delete", Error, src, path, line, 1,
                    "`DELETE` with no `WHERE` empties the table")
            }
            line = line + 1
            start = i + 1
        }
        i = i + 1
    }
    return out + "export fn sqlChecked() -> Int64 { return \{(line - 1).toString()} }\n"
}

fn sqlSlice(s: String, from: Int64, to: Int64) -> String {
    return match slice(s, from, to) {
        Ok(v) => v,
        Err(e) => "",
    }
}
"#;

const SQL_APP: &str = r#"import { sqlHints } from "./sql_hints"
import { sqlChecked } from sqlHints("./schema.sql", "./vyrn.json")

fn main() -> Int64 {
    print(sqlChecked())
    return 0
}
"#;

fn sql_project(tag: &str, sql: &str, manifest: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::write(dir.join("sql_hints.vyrn"), SQL_HINTS).unwrap();
    std::fs::write(dir.join("schema.sql"), sql).unwrap();
    std::fs::write(dir.join("app.vyrn"), SQL_APP).unwrap();
    std::fs::write(dir.join("vyrn.json"), manifest).unwrap();
    dir
}

#[test]
fn a_third_party_hint_library_needs_nothing_std_has() {
    // Its warning: a build that succeeded, anchored in a file that is neither
    // Vyrn nor `.vyx`, at the line the library computed while reading it.
    let advised = sql_project("thirdparty", "select 1\nselect * from books\n", "{}");
    let r = run_in(&advised, "run", "app.vyrn");
    assert_eq!(r.code, 0, "advice rides the build:\n{}", r.stderr);
    assert_eq!(r.stdout, "2\n");
    assert!(
        r.stderr
            .contains("schema.sql:2:1: warning: sql/select-star:"),
        "{}",
        r.stderr
    );

    // Its refusal: the same directive, the severity its author chose, no flag.
    // A failing load carries no advice, so this is a second project.
    let refused = sql_project("thirdparty_err", "delete from books\n", "{}");
    let r = run_in(&refused, "run", "app.vyrn");
    assert_ne!(r.code, 0, "its error-severity rule bit:\n{}", r.stderr);
    assert_eq!(r.stdout, "", "the program never ran");
    assert!(
        r.stderr
            .contains("schema.sql:1:1: sql/unbounded-delete: `DELETE` with no `WHERE`"),
        "in its own words:\n{}",
        r.stderr
    );
}

#[test]
fn a_third_party_library_gets_the_same_configuration_and_waivers() {
    // Its own key in the same manifest, so two hint libraries in one project do
    // not collide — and the waiver marker rides a `--` SQL comment without
    // `std/hints` knowing that SQL has comments.
    let sql = "-- vyrn-ignore sql/select-star\nselect * from books\nselect * from tags\n";
    let manifest = "{ \"sqlHints\": { \"sql/unbounded-delete\": \"off\" } }";
    let dir = sql_project("thirdparty_cfg", sql, manifest);
    let r = run_in(&dir, "run", "app.vyrn");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout, "3\n");
    assert!(
        !r.stderr.contains("schema.sql:2:"),
        "the waived line says nothing:\n{}",
        r.stderr
    );
    assert!(
        r.stderr
            .contains("schema.sql:3:1: warning: sql/select-star:"),
        "the next one still does:\n{}",
        r.stderr
    );

    // And its config is refused on the same terms as `std/vyx-hints`'s.
    let bad = sql_project(
        "thirdparty_bad",
        "select 1\n",
        "{ \"sqlHints\": { \"sql/select-star\": \"loud\" } }",
    );
    let r = run_in(&bad, "run", "app.vyrn");
    assert_ne!(r.code, 0, "refused:\n{}", r.stderr);
    assert!(r.stderr.contains("is not a usable config"), "{}", r.stderr);
}
