//! Tests for typed i18n, the `std/i18n` generator, through the `vyrn`
//! binary. Generation runs with the cache disabled so a stale entry never masks a
//! regression.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_file(rel: &str) -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh scratch directory with an empty `locales/`.
fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_i18n_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("locales")).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

#[test]
fn emit_gen_shows_the_synthesized_translation_module() {
    let demo = repo_file("examples/i18ndemo.vyrn");
    let out = vyrn()
        .arg("emit-gen")
        .arg(&demo)
        .output()
        .expect("emit-gen");
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);

    // Module state is the generator sandbox's carve-out.
    assert!(src.contains("export type Locale ="), "Locale enum:\n{src}");
    assert!(
        src.contains("| En") && src.contains("| Uk"),
        "locale variants:\n{src}"
    );
    assert!(
        src.contains("let mut currentLocale: Locale = En"),
        "module state:\n{src}"
    );
    assert!(
        src.contains("export fn setLocale(l: Locale)"),
        "setLocale:\n{src}"
    );

    assert!(
        src.contains("export type TransKey = String where value =~ \"(")
            && src.contains("home\\\\.title")
            && src.contains("cart\\\\.items"),
        "TransKey finite regex:\n{src}"
    );

    assert!(
        src.contains("export type TStatusState = String where value =~ \"(active|other|paused)\""),
        "select arg type:\n{src}"
    );

    assert!(
        src.contains("export fn tCartItems(count: Int64) -> String"),
        "plural fn:\n{src}"
    );
    assert!(
        src.contains("export fn tGreeting(name: String) -> String"),
        "string-arg fn:\n{src}"
    );
    assert!(
        src.contains("export fn tStatus(state: TStatusState) -> String"),
        "select fn:\n{src}"
    );

    assert!(src.contains("/// Welcome"), "arg-less doc:\n{src}");
    assert!(
        src.contains("/// {count, plural, one {# item} other {# items}}"),
        "plural doc:\n{src}"
    );

    // The CLDR operand is the absolute value: a negative count takes its
    // magnitude's branch.
    assert!(
        src.contains(
            "(if count < 0 { 0 - count } else { count }) % 10 == 1 && (if count < 0 { 0 - count } else { count }) % 100 != 11",
        ),
        "uk one rule:\n{src}"
    );

    assert!(
        src.contains("export fn t(key: TransKey) -> String"),
        "t():\n{src}"
    );

    // Every key also exports an un-prefixed alias for `import * as t`.
    assert!(
        src.contains("export fn cartItems(count: Int64) -> String"),
        "un-prefixed plural fn:\n{src}"
    );
    assert!(
        src.contains("export fn greeting(name: String) -> String"),
        "un-prefixed string-arg fn:\n{src}"
    );
}

#[test]
fn namespace_access_reads_unprefixed_keys() {
    let dir = scratch("nsaccess");
    write(
        &dir.join("locales/en.json"),
        "{ \"app\": { \"title\": \"Paste Bin\" }, \"greeting\": \"Hi {name}\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import * as t from i18n(\"./locales\")\n\
         fn main() -> Int64 {\n\
         print(t.appTitle())\n\
         print(t.greeting(\"Bob\"))\n\
         return 0\n\
         }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "namespace i18n access must run:\n{combined}"
    );
    assert!(
        combined.contains("Paste Bin"),
        "un-prefixed arg-less key:\n{combined}"
    );
    assert!(
        combined.contains("Hi Bob"),
        "un-prefixed arg key:\n{combined}"
    );
}

#[test]
fn drift_between_locales_is_a_readable_load_error() {
    let dir = scratch("drift");
    write(
        &dir.join("locales/en.json"),
        "{ \"home\": { \"title\": \"Hi\" }, \"bye\": \"Bye\" }",
    );
    write(
        &dir.join("locales/uk.json"),
        "{ \"home\": { \"title\": \"Pryvit\" }, \"extra\": \"X\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a drifting locale pair must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("locale `uk` is missing key `bye`"),
        "missing-key drift:\n{err}"
    );
    assert!(
        err.contains("locale `uk` defines key `extra`"),
        "extra-key drift:\n{err}"
    );
}

#[test]
fn unsupported_value_fails_generation() {
    let dir = scratch("badvalue");
    write(&dir.join("locales/en.json"), "{ \"count\": 5 }");
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "an unsupported value must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("locale `en` is not a usable locale file"),
        "parse-error diagnostic:\n{err}"
    );
}

#[test]
fn plural_in_a_locale_without_a_rule_fails() {
    let dir = scratch("noplural");
    // `xx` has no CLDR plural rule in the starter table.
    write(
        &dir.join("locales/xx.json"),
        "{ \"n\": \"{count, plural, one {# x} other {# xs}}\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a plural in a ruleless locale must fail"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("locale `xx` has no plural rule"),
        "no-plural-rule diagnostic:\n{err}"
    );
}

#[test]
fn a_lone_apostrophe_is_literal_and_keeps_the_placeholder() {
    let dir = scratch("apos");
    write(
        &dir.join("locales/en.json"),
        "{ \"greet\": \"It's {name}!\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { tGreet } from i18n(\"./locales\")\n\
         fn main() -> Int64 { print(tGreet(\"Bob\")) return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "apostrophe message must compile with the arg:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sout = String::from_utf8_lossy(&out.stdout);
    assert!(
        sout.contains("It's Bob!"),
        "expected `It's Bob!`, got:\n{sout}"
    );
}

#[test]
fn paired_and_quoting_apostrophes_render_per_icu() {
    let dir = scratch("apos2");
    // `''` -> one apostrophe; `'{'` -> a literal brace.
    write(
        &dir.join("locales/en.json"),
        "{ \"a\": \"b''c\", \"d\": \"x '{' y\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { tA, tD } from i18n(\"./locales\")\n\
         fn main() -> Int64 { print(tA()) print(tD()) return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sout = String::from_utf8_lossy(&out.stdout);
    assert!(sout.contains("b'c"), "paired '' -> one apostrophe:\n{sout}");
    assert!(sout.contains("x { y"), "'{{' -> literal brace:\n{sout}");
}

#[test]
fn an_unmatched_brace_fails_generation() {
    let dir = scratch("brace");
    write(
        &dir.join("locales/en.json"),
        "{ \"msg\": \"50% off { sale\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "an unmatched brace must fail generation"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("key `msg` has an unbalanced brace"),
        "brace diagnostic naming the key:\n{err}"
    );
}

#[test]
fn a_dotted_vs_nested_key_collision_is_named() {
    let dir = scratch("dupkey");
    // "a.b" and {"a":{"b":...}} flatten to the same entry.
    write(
        &dir.join("locales/en.json"),
        "{ \"a.b\": \"x\", \"a\": { \"b\": \"y\" } }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a duplicate flattened key must fail generation"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("key `a.b` is defined twice"),
        "dup-key diagnostic:\n{err}"
    );
    assert!(
        !err.contains("` defined twice"),
        "must not surface as the confusing duplicate-fn error:\n{err}"
    );
}

#[test]
fn a_fn_name_key_collision_is_named() {
    let dir = scratch("clashkey");
    // home.title and homeTitle both make tHomeTitle.
    write(
        &dir.join("locales/en.json"),
        "{ \"home.title\": \"A\", \"homeTitle\": \"B\" }",
    );
    write(
        &dir.join("app.vyrn"),
        "import { i18n } from \"std/i18n\"\n\
         import { t } from i18n(\"./locales\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a fn-name key collision must fail generation"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("keys `home.title` and `homeTitle` both name the function `tHomeTitle`"),
        "collision diagnostic listing both keys:\n{err}"
    );
    assert!(
        !err.contains("` defined twice"),
        "must not surface as the confusing duplicate-fn error:\n{err}"
    );
}

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/i18ndemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(
        combined.contains("4 passed, 0 failed"),
        "expected 4 green tests:\n{combined}"
    );
}
