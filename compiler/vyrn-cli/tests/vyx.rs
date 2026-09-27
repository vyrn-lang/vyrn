//! Tests for the `.vyx` component compiler, the `std/vyx` generators,
//! through the `vyrn` binary. Generation runs with the cache disabled so a stale entry
//! never masks a regression.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_file(rel: &str) -> PathBuf {
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

/// A fresh scratch directory with an empty `comp/` for a test's `.vyx` fixtures.
fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_vyx_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("comp")).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// The one-line app that imports a view function from the generator over `./comp`.
const APP: &str = "import { components } from \"std/vyx\"\n\
     import { widget } from components(\"./comp\")\n\
     fn main() -> Int64 { return 0 }\n";

fn run_app(dir: &Path) -> (bool, String) {
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let combined =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    (out.status.success(), combined)
}

#[test]
fn emit_gen_shows_the_synthesized_component_module() {
    let demo = repo_file("examples/vyxdemo.vyrn");
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

    assert!(
        src.contains("export fn row(item: Item) -> Html"),
        "row signature:\n{src}"
    );
    assert!(
        src.contains("export fn listing(items: Array<Item>) -> Html"),
        "listing signature:\n{src}"
    );
    assert!(
        src.contains("export fn panel(title: String, children: consume Array<Html>) -> Html"),
        "panel signature:\n{src}"
    );

    // A relative script import is rebased so it resolves from the synthesized module.
    assert!(
        src.contains("from \"./vyxcomp/./models\""),
        "rebased import:\n{src}"
    );

    assert!(src.contains("for it in items {"), "for loop:\n{src}");
    assert!(
        src.contains("keyed((it.id).toString()"),
        "keyed push:\n{src}"
    );
    assert!(src.contains("row(it)"), "internal component call:\n{src}");

    assert!(
        src.contains("On(\"click\", \"removeRow\", (item.id).toString())"),
        "event lowering:\n{src}"
    );
    assert!(
        src.contains("On(\"input\", \"setQty\""),
        "input event:\n{src}"
    );
    assert!(src.contains("Cls(\"row\")"), "class -> Cls:\n{src}");
    assert!(
        src.contains("for vyxCh in consume children {"),
        "children splice:\n{src}"
    );
    assert!(src.contains("Raw("), "{{@raw}} -> Raw:\n{src}");
}

#[test]
fn unclosed_element_fails_naming_the_file_and_line() {
    let dir = scratch("unclosed");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li>oops\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "an unclosed element must fail to load");
    assert!(
        err.contains("is never closed"),
        "unclosed diagnostic:\n{err}"
    );
    assert!(
        err.contains("Widget.vyx:2:1"),
        "diagnostic is anchored in the file, at the line:\n{err}"
    );
}

#[test]
fn missing_for_key_fails_naming_the_file() {
    let dir = scratch("nokey");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<ul>\n<li v-for=\"x in xs\">{{ x }}</li>\n</ul>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a keyless {{#for}} must fail to load");
    assert!(
        err.contains("has no `:key`"),
        "missing-key diagnostic:\n{err}"
    );
    assert!(
        err.contains("Widget.vyx:3:1"),
        "diagnostic is anchored in the file, at the line:\n{err}"
    );
}

#[test]
fn unknown_component_fails_naming_the_tag() {
    let dir = scratch("unknowncomp");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<ul><Missing :x=\"1\"/></ul>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "an unknown component tag must fail to load");
    assert!(
        err.contains("names no component"),
        "unknown-component diagnostic:\n{err}"
    );
    assert!(err.contains("Missing"), "diagnostic names the tag:\n{err}");
}

#[test]
fn non_scalar_event_arg_fails() {
    let dir = scratch("nonscalar");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<button @click=\"go(a, b)\">x</button>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a multi-argument event handler must fail to load");
    assert!(
        err.contains("passes more than one argument"),
        "non-scalar diagnostic:\n{err}"
    );
}

#[test]
fn multiple_roots_fail() {
    let dir = scratch("roots");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li>a</li>\n<li>b</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a template with multiple roots must fail to load");
    assert!(
        err.contains("holds more than one root element"),
        "multiple-roots diagnostic:\n{err}"
    );
}

#[test]
fn malformed_props_fails() {
    let dir = scratch("props");
    // A props block missing its opening brace.
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nprops item: Item\n</script>\n<template><li>x</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a malformed props block must fail to load");
    assert!(err.contains("`props`"), "bad-props diagnostic:\n{err}");
}

#[test]
fn props_before_import_fails_naming_the_file_and_line() {
    let dir = scratch("importsfirst");
    // A `props` block ahead of the import violates the imports-first rule.
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nprops { x: Int64 }\nimport { t } from \"../s\"\n</script>\n<template><li>{{ x }}</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a props block before an import must fail to load");
    assert!(
        err.contains("an `import` sits after the `props` block"),
        "imports-first diagnostic:\n{err}"
    );
    assert!(
        err.contains("Widget.vyx:3:1"),
        "diagnostic is anchored in the file, at the import's line:\n{err}"
    );
}

#[test]
fn imports_before_props_loads_and_runs() {
    let dir = scratch("importsok");
    write(&dir.join("s.vyrn"), "export type T = { v: Int64 }\n");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nimport { T } from \"../s\"\nprops { x: T }\n</script>\n<template><li>{{ x.v }}</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(ok, "imports-first must load and run:\n{err}");
}

#[test]
fn missing_template_section_fails() {
    let dir = scratch("notemplate");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>props { x: Int64 }</script>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a .vyx with no <template> must fail to load");
    assert!(
        err.contains("has no `<template>` section"),
        "no-template diagnostic:\n{err}"
    );
}

/// The diagnostic lands on the `.vyx` at the expression's column, with the
/// generated location as a note.
#[test]
fn type_error_in_template_expression_remaps_to_the_vyx() {
    let dir = scratch("remap");
    // `Row` has `title`; the template mistypes it as `titel`. The interpolation
    // is on line 6 as `<li>{{ item.titel }}`; `<li>{{ ` is 7 chars, so `item`
    // begins at column 8.
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\ntype Row = { title: String }\nprops { item: Row }\n</script>\n<template>\n<li>{{ item.titel }}</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a template type error must fail to load");
    assert!(err.contains("Widget.vyx:6:8:"), "remapped location:\n{err}");
    assert!(err.contains("titel"), "carries the checker message:\n{err}");
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
    assert!(
        !err.contains("generated by components(\"./comp\") at app.vyrn:6:"),
        "not the banner:\n{err}"
    );
}

/// The diagnostic stays at the generated location with the malformed directive noted.
/// A hand-written generator delivers the directive as any third-party
/// generator would.
#[test]
fn malformed_origin_directive_never_loses_the_diagnostic() {
    let dir = scratch("malformed");
    write(
        &dir.join("gen.vyrn"),
        "export gen fn bad(x: String) -> String {\n\
         return \"//@origin not-a-position\\nexport fn f() -> Int64 { return true }\\n\"\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { bad } from \"./gen\"\n\
         import { f } from bad(\"x\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the type error must fail the load");
    assert!(
        err.contains("note: malformed `//@origin` directive"),
        "malformed note:\n{err}"
    );
    assert!(
        err.contains("generated by bad"),
        "kept at generated location:\n{err}"
    );
}

/// `std/vyx` copies a `<script>` body verbatim, so a multi-line string
/// literal puts author text at the start of a generated line. Only a line the lexer
/// calls a comment is a directive; data that looks like one is inert.
#[test]
fn a_directive_inside_a_vyx_string_literal_cannot_fail_the_build() {
    let dir = scratch("inject");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n  <div>{{ banner() }}</div>\n</template>\n\
         <script>\nfn banner() -> String {\n    return \"first\n\
         //@diag error ../../../../../../../../elsewhere.vyrn:1:1 injected by a string literal\n\
         last\"\n}\n</script>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, text) = run_app(&dir);
    assert!(ok, "a string literal must not fail the build:\n{text}");
    assert!(
        !text.contains("elsewhere.vyrn"),
        "no file outside the project is named:\n{text}"
    );
    assert!(
        !text.contains("injected by a string literal:") && !text.contains("error: injected"),
        "the injected text is data, not a diagnostic:\n{text}"
    );
}

/// The origin map maps generated source to user source, so a directive that climbs
/// out of the project is malformed. The diagnostic stays at its
/// generated location and says why.
#[test]
fn an_origin_pointing_outside_the_project_is_not_a_map() {
    let dir = scratch("escape");
    write(
        &dir.join("gen.vyrn"),
        "export gen fn bad(x: String) -> String {\n\
         return \"//@origin ../../../../../../../../outside.vyx:1:1\\nexport fn f() -> Int64 { return true }\\n\"\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { bad } from \"./gen\"\n\
         import { f } from bad(\"x\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the type error still fails the load");
    assert!(
        !err.contains("outside.vyx:1:1:"),
        "the diagnostic is not attributed to a file outside the project:\n{err}"
    );
    assert!(
        err.contains("note: malformed `//@origin` directive"),
        "the refusal says why:\n{err}"
    );
    assert!(
        err.contains("generated by bad"),
        "the diagnostic is never lost:\n{err}"
    );
}

/// A stray `\` in a template expression stops the lexer on the synthesized module.
/// The origin map is built before lexing, so the error still lands on the
/// `.vyx`, with the generated location as a note.
#[test]
fn lex_error_in_template_expression_remaps_to_the_vyx() {
    let dir = scratch("lexremap");
    // `<li>{{ ` is 7 chars, so the expression starts at column 8 of line 2.
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li>{{ oops(\\) }}</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(
        !ok,
        "a stray backslash in a template expression must fail the load"
    );
    assert!(
        err.contains("Widget.vyx:2:8:"),
        "remapped to the .vyx position:\n{err}"
    );
    assert!(
        err.contains("unexpected character"),
        "carries the lexer message:\n{err}"
    );
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
    assert!(
        !err.starts_with("generated by components"),
        "no longer reported at the banner alone:\n{err}"
    );
}

#[test]
fn parse_error_in_template_expression_remaps_to_the_vyx() {
    let dir = scratch("parseremap");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li>{{ 1 + + * 2 }}</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(
        !ok,
        "a syntactically broken template expression must fail the load"
    );
    assert!(
        err.contains("Widget.vyx:2:8:"),
        "remapped to the .vyx position:\n{err}"
    );
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
}

/// A lex error on a line no `//@origin` governs keeps its generated location:
/// a generated location beats a wrong one. The generator emits its
/// directive after the broken line, so that line is ungoverned.
#[test]
fn ungoverned_generated_glue_keeps_its_generated_location() {
    let dir = scratch("glue");
    write(
        &dir.join("gen.vyrn"),
        // Line 1 of the OUTPUT holds a character the lexer rejects (`#`) and is
        // governed by nothing; the `//@origin` directive comes after it.
        "export gen fn glue(x: String) -> String {\n\
         return \"export fn f() -> Int64 { return # }\\n//@origin ./a.vyx:1:1\\nexport fn g() -> Int64 { return 1 }\\n\"\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { glue } from \"./gen\"\n\
         import { f } from glue(\"x\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the lex error must fail the load");
    assert!(
        err.contains("unexpected character"),
        "the diagnostic survives:\n{err}"
    );
    assert!(
        err.contains("generated by glue"),
        "kept at the generated location:\n{err}"
    );
    assert!(
        !err.contains("a.vyx"),
        "not attributed to an unrelated origin:\n{err}"
    );
}

/// `APP` through the themed generator; `./theme.json` resolves relative to
/// the app, like `./comp`.
const THEMED_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
     import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
     fn main() -> Int64 { return 0 }\n";

/// A minimal theme: `flex` and `p-2` derive from it, and the safelist adds two bespoke
/// names that have no CSS rule.
const THEME_JSON: &str = "{ \"colors\": { \"brand\": \"#123456\" },\n\
     \"spacing\": { \"2\": \"0.5rem\" },\n\
     \"safelist\": [\"card\", \"book-row\"] }\n";

/// A static `class` literal is checked against `Tw` and gets its own column-exact
/// `//@origin`.
#[test]
fn themed_typo_class_remaps_to_the_vyx_column() {
    let dir = scratch("themed_typo");
    // `<li class="` is 11 chars, so `flx` starts at column 12. `flx` is neither a
    // derived utility nor safelisted.
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li class=\"flx\">x</li>\n</template>\n",
    );
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a typo'd utility class must fail the load");
    assert!(
        err.contains("Widget.vyx:2:12:"),
        "remapped to the class column:\n{err}"
    );
    assert!(err.contains("flx"), "carries the offending class:\n{err}");
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
}

/// The safelist adds `card` to the checked vocabulary; a bound `:class` coerces at
/// runtime.
#[test]
fn themed_safelist_and_utilities_check_and_run() {
    let dir = scratch("themed_ok");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>props { cls: String }</script>\n\
         <template>\n\
         <li class=\"card flex p-2\"><span :class=\"cls\">x</span></li>\n\
         </template>\n",
    );
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let (ok, err) = run_app(&dir);
    assert!(
        ok,
        "a safelisted + utility class mix must load and run:\n{err}"
    );
}

/// `vyxTheme.cls` returns `Cls(c)`, so the themed module runs the same as the bare one.
#[test]
fn themed_emit_gen_routes_class_through_vyx_theme() {
    let dir = scratch("themed_emit");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li class=\"card\">x</li>\n</template>\n",
    );
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("import * as vyxTheme from tw(\"./theme.json\")"),
        "themed import:\n{src}"
    );
    assert!(
        src.contains("vyxTheme.cls(\"card\")"),
        "class routed through vyxTheme.cls:\n{src}"
    );
    assert!(
        src.contains("//@origin"),
        "carries origin directives:\n{src}"
    );
}

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/vyxdemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(
        combined.contains("1 passed, 0 failed"),
        "expected 1 green test:\n{combined}"
    );
}

#[test]
fn audit_comment_mentioning_props_is_ignored() {
    let dir = scratch("audit_comment_props");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\n// the props for this widget's template are below\nprops { title: String }\n</script>\n<template><li>{{ title }}</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("export fn widget(title: String) -> Html"),
        "one prop only:\n{src}"
    );
}

#[test]
fn audit_helper_named_props_is_not_a_block() {
    let dir = scratch("audit_ident_props");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nfn f() -> Int64 {\nlet props = 5\nreturn props\n}\n</script>\n<template><li>{{ f() }}</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("export fn widget() -> Html"),
        "no phantom props:\n{src}"
    );
    assert!(
        src.contains("let props = 5"),
        "helper passes through:\n{src}"
    );
}

#[test]
fn audit_literal_brace_in_text_stays_literal() {
    let dir = scratch("audit_brace");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template><li>a { b } c</li></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(src.contains("a { b } c"), "braces stay literal:\n{src}");
}

#[test]
fn audit_html_comment_in_template_is_stripped() {
    let dir = scratch("audit_htmlcomment");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template><ul><!-- note --><li>x</li></ul></template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(ok, "an HTML comment must not break the template:\n{err}");
}

/// The template section is markup: a `"` in text is not a string start. Read as one,
/// it swallows the `</template>`.
#[test]
fn an_odd_double_quote_in_text_is_a_character() {
    let dir = scratch("oddquote");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li>a 6\" nail</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(
        ok,
        "an odd quote in text must not hide the template:\n{err}"
    );
    assert!(
        !err.contains("has no `<template>` section"),
        "the template is present:\n{err}"
    );
}

#[test]
fn a_single_quoted_attribute_may_hold_a_double_quote() {
    let dir = scratch("sqattr");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li title='2\"'>x</li>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(
        ok,
        "a single-quoted value holding a double quote must compile:\n{err}"
    );
}

#[test]
fn a_close_tag_inside_a_comment_does_not_truncate_the_template() {
    let dir = scratch("cmtclose");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<ul><!-- </template> --><li>kept</li></ul>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(ok, "a commented close tag must close nothing:\n{err}");

    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(
        src.contains("kept"),
        "content past the comment survives:\n{src}"
    );
}

#[test]
fn a_nested_template_element_does_not_end_the_section() {
    let dir = scratch("nested");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<ul><template><li>kept</li></template></ul>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(ok, "a nested template must not end the section:\n{err}");
}

#[test]
fn an_empty_template_is_found_not_missing() {
    let dir = scratch("empty");
    write(&dir.join("comp/Widget.vyx"), "<template></template>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "an empty template has no root element");
    assert!(
        !err.contains("has no `<template>` section"),
        "the section was found, so the diagnostic is not 'no template':\n{err}"
    );
}

/// The arity guard counts brackets, and `>` is a comparison: counted as a bracket,
/// it hides the comma and the generator emits an unparseable call.
#[test]
fn a_comparison_in_a_multi_arg_event_still_reports_the_arity() {
    let dir = scratch("cmpevent");
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<button @click=\"pick(a > b, c)\">x</button>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a multi-argument event handler must fail to load");
    assert!(
        err.contains("passes more than one argument"),
        "the arity diagnostic, not a parse error:\n{err}"
    );
    assert!(
        !err.contains("expected RParen"),
        "the generator must not emit unparseable Vyrn:\n{err}"
    );
}

#[test]
fn a_comparison_inside_a_single_event_argument_compiles() {
    let dir = scratch("cmpone");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nprops { a: Int64, b: Int64 }\n</script>\n\
         <template>\n<button @click=\"pick(a > b)\">x</button>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(ok, "one comparison is one argument:\n{err}");
}

/// `<script>` bodies whose first `</script>` sits in a string or a comment. An import
/// and a `props` block follow each, so a scanner that reaches them saw past the decoy.
const DECOY_SCRIPTS: &[(&str, &str)] = &[
    ("string", "fn tag() -> String { return \"</script>\" }"),
    ("line_comment", "// </script>"),
    (
        "escaped_quote",
        "fn tag() -> String { return \"a\\\"</script>b\" }",
    ),
];

/// Two implementations that cannot share code decide the `.vyx` section boundary:
/// `std/vyx`'s scanner, which compiles the component, and `vyrn_frontend::vyx`, which
/// the tools read it with. The test fails if either drifts.
#[test]
fn audit_hostile_sections_agree_with_the_generator() {
    for (tag, decoy) in DECOY_SCRIPTS {
        let dir = scratch(&format!("decoy_{tag}"));
        write(&dir.join("vyrn.json"), "{ \"main\": \"app.vyrn\" }\n");
        write(
            &dir.join("util.vyrn"),
            "export fn helper() -> String {\n    return \"h\"\n}\n",
        );
        write(
            &dir.join("comp/Widget.vyx"),
            &format!(
                "<script>\n{decoy}\nimport {{ helper }} from \"../util\"\nprops {{ n: Int64 }}\n\
                 </script>\n<template><li>{{{{ n }}}}{{{{ helper() }}}}</li></template>\n"
            ),
        );
        write(&dir.join("app.vyrn"), APP);

        // `std/vyx`'s answer: the props after the decoy became parameters.
        let gen = vyrn()
            .arg("emit-gen")
            .arg(dir.join("app.vyrn"))
            .output()
            .expect("emit-gen");
        assert!(
            gen.status.success(),
            "{tag}: the generator must compile the component:\n{}",
            String::from_utf8_lossy(&gen.stderr)
        );
        let src = String::from_utf8_lossy(&gen.stdout);
        assert!(
            src.contains("export fn widget(n: Int64) -> Html"),
            "{tag}: the generator truncated the section:\n{src}"
        );

        // The tools' answer: the import after the decoy is a project graph edge.
        let why = vyrn()
            .current_dir(&dir)
            .arg("why")
            .arg("util.vyrn")
            .output()
            .expect("why");
        let out = String::from_utf8_lossy(&why.stdout).replace('\\', "/");
        assert!(
            out.contains("comp/Widget.vyx -> util.vyrn"),
            "{tag}: `why` disagrees with the generator about the section:\n{out}"
        );
    }
}

/// A toy provider: a library `gen fn` of shape `(attrs, file, line, col)
/// -> String` whose module exports `provide() -> Html`. A tag resolves against what a
/// `<script>` imports and becomes a nested generation. The provider reports an unknown
/// glyph at the anchor it is given.
const PROVIDER: &str = r##"import { parseJson } from "std/jsonread"
import { Json } from "std/json"
import { fieldsOf, fieldAt } from "std/jsondec"
import { report, Severity } from "std/diag"

fn attrOf(attrs: String, key: String) -> String {
    let j = match parseJson(attrs) {
        Ok(v) => v,
        Err(e) => JNull,
    }
    return match fieldAt(fieldsOf(j), key) {
        JStr(s) => s,
        JNull => "",
        JBool(b) => "",
        JNum(n) => "",
        JArr(a) => "",
        JObj(f) => "",
    }
}

export gen fn Glyph(attrs: String, file: String, line: Int64, col: Int64) -> String {
    let name = attrOf(attrs, "name")
    let label = attrOf(attrs, "label")
    if name != "github" && name != "discord" {
        return report(Error, file, line, col, "no glyph `\{name}` here - nearest is `github`")
    }
    let body = if name == "github" { "M8 0 L16 8 L8 16 Z" } else { "M2 2 H14 V14 H2 Z" }
    let mut out = "import { el, text, Attr, Html } from \"std/html\"\n"
    out = out + "export fn provide() -> Html {\n"
    out = out + "    return el(\"svg\", [A(\"aria-label\", \"\{label}\")], [text(\"\{body}\")])\n"
    out = out + "}\n"
    return out
}
"##;

/// The app that prints the `Badge` view, so the spliced tree is observable.
const BADGE_APP: &str = "import { components } from \"std/vyx\"\n\
     import { badge } from components(\"./comp\")\n\
     import { toHtmlString } from \"std/html\"\n\
     fn main() -> Int64 { print(toHtmlString(badge())) return 0 }\n";

/// A scratch project with the toy provider and one `Badge.vyx` template body.
fn provider_project(tag: &str, template: &str) -> PathBuf {
    let dir = scratch(tag);
    write(&dir.join("provider.vyrn"), PROVIDER);
    write(
        &dir.join("comp/Badge.vyx"),
        &format!(
            "<script>\nimport {{ Glyph }} from \"../provider\"\n</script>\n\n<template>\n{template}\n</template>\n"
        ),
    );
    write(&dir.join("app.vyrn"), BADGE_APP);
    dir
}

fn run_named(dir: &Path, app: &str) -> (bool, String) {
    let out = vyrn().arg("run").arg(dir.join(app)).output().expect("run");
    let combined =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    (out.status.success(), combined)
}

#[test]
fn a_provider_tag_generates_and_splices_its_html() {
    let dir = provider_project(
        "provgen",
        "<span class=\"badge\">\n    <Glyph name=\"github\" label=\"GitHub\"/>\n    <Glyph name=\"discord\" label=\"Discord\"/>\n</span>",
    );
    let (ok, out) = run_named(&dir, "app.vyrn");
    assert!(ok, "a provider tag must load and run:\n{out}");
    assert!(
        out.contains("<span class=\"badge\"><svg aria-label=\"GitHub\">M8 0 L16 8 L8 16 Z</svg><svg aria-label=\"Discord\">M2 2 H14 V14 H2 Z</svg></span>"),
        "the provider's trees are not spliced at the tags:\n{out}"
    );

    // One nested generator import per tag: its static attributes as one JSON
    // constant, its file and line as the anchor, and a `provide()` call in its place.
    let gen = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&gen.stdout);
    assert!(
        src.contains(
            "from Glyph(\"{\\\"name\\\":\\\"github\\\",\\\"label\\\":\\\"GitHub\\\"}\", \"./comp/Badge.vyx\", 7, 1)"
        ),
        "the emitted provider import:\n{src}"
    );
    assert_eq!(
        src.matches("import * as vyxp").count(),
        2,
        "one generation per tag:\n{src}"
    );
    assert_eq!(
        src.matches(".provide())").count(),
        2,
        "the conventional entry point is called at each tag:\n{src}"
    );
}

#[test]
fn a_provider_diagnostic_lands_on_the_tag() {
    let dir = provider_project(
        "provtypo",
        "<span class=\"badge\">\n    <Glyph name=\"githup\" label=\"GitHub\"/>\n</span>",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "an unknown glyph must fail the load");
    // The provider never reads the `.vyx`; the anchor reaches it as arguments.
    assert!(
        err.contains("Badge.vyx:7:1: no glyph `githup` here - nearest is `github`"),
        "the provider's report is not anchored at the tag:\n{err}"
    );
}

/// One tag takes a different path than two. It keeps the entry point off the unreserved
/// surface builtins: named `render`, a lone tag fails the build, because
/// builtin shadowing is decided across the whole program.
#[test]
fn one_provider_tag_alone_in_a_page_builds() {
    let dir = provider_project(
        "provsolo",
        "<span class=\"badge\"><Glyph name=\"github\" label=\"GitHub\"/></span>",
    );
    let (ok, out) = run_named(&dir, "app.vyrn");
    assert!(
        ok,
        "a page with exactly one provider tag must build:\n{out}"
    );
    assert!(
        out.contains(
            "<span class=\"badge\"><svg aria-label=\"GitHub\">M8 0 L16 8 L8 16 Z</svg></span>"
        ),
        "the lone provider's tree is not spliced at the tag:\n{out}"
    );
}

#[test]
fn a_bound_attribute_on_a_provider_tag_is_refused() {
    let dir = provider_project(
        "provdyn",
        "<span class=\"badge\">\n    <Glyph :name=\"which\" label=\"GitHub\"/>\n</span>",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "a bound attribute on a provider tag must fail");
    assert!(
        err.contains("Badge.vyx:7:1: `<Glyph>` is a generation-time provider, and `:name` binds an expression"),
        "the structural refusal:\n{err}"
    );
    assert!(
        err.contains("a provider's attributes become constant arguments to a generator, so write `name=\"\u{2026}\"` as a static attribute, or wrap `<Glyph>` in a sibling `.vyx` component that computes it"),
        "the refusal says what to do instead:\n{err}"
    );
}

#[test]
fn an_event_and_children_on_a_provider_tag_are_refused() {
    let dir = provider_project(
        "provevt",
        "<span class=\"badge\">\n    <Glyph name=\"github\" @click=\"go\"/>\n</span>",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "an event on a provider tag must fail");
    assert!(
        err.contains("`<Glyph>` is a generation-time provider, and `@click` binds a handler \u{2014} a provider's attributes become constant arguments to a generator, so a provider tag takes static attributes only"),
        "the event refusal:\n{err}"
    );

    let dir = provider_project(
        "provkids",
        "<span class=\"badge\">\n    <Glyph name=\"github\">hi</Glyph>\n</span>",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "children on a provider tag must fail");
    assert!(
        err.contains("`<Glyph>` is given children, and it is a generation-time provider \u{2014} a provider's tree comes from its attributes alone, so it takes none"),
        "the children refusal:\n{err}"
    );
}

#[test]
fn an_unresolved_tag_says_what_the_two_resolution_paths_are() {
    let dir = provider_project(
        "provmiss",
        "<span class=\"badge\">\n    <Glyf name=\"github\"/>\n</span>",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "a tag naming neither path must fail");
    assert!(
        err.contains("`<Glyf>` names no component \u{2014} a component is a `.vyx` file in the same directory, or a generation-time provider a `<script>` imports"),
        "the tag-miss message:\n{err}"
    );
}

#[test]
fn each_provider_tag_gets_its_own_minted_namespace() {
    let dir = provider_project(
        "provns",
        "<span class=\"badge\">\n    <Glyph name=\"github\" label=\"GitHub\"/>\n    <Dot/>\n    <Glyph name=\"discord\" label=\"Discord\"/>\n</span>",
    );
    // `Other.vyx` imports nothing: the import namespace is flat across the set, so
    // its tag resolves through `Badge.vyx`'s import.
    write(
        &dir.join("comp/Dot.vyx"),
        "<template><i class=\"dot\">.</i></template>\n",
    );
    write(
        &dir.join("comp/Other.vyx"),
        "<template><b><Glyph name=\"github\" label=\"Sib\"/></b></template>\n",
    );
    let (ok, out) = run_named(&dir, "app.vyrn");
    assert!(
        ok,
        "provider tags beside a sibling component must run:\n{out}"
    );
    assert!(
        out.contains("<svg aria-label=\"GitHub\">M8 0 L16 8 L8 16 Z</svg><i class=\"dot\">.</i><svg aria-label=\"Discord\">"),
        "the sibling component and the two providers interleave:\n{out}"
    );

    let gen = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&gen.stdout);
    let mut aliases: Vec<&str> = src
        .lines()
        .filter(|l| l.starts_with("import * as vyxp"))
        .map(|l| l.split_whitespace().nth(3).unwrap())
        .collect();
    let total = aliases.len();
    aliases.sort();
    aliases.dedup();
    assert_eq!(total, 3, "one generation per tag, three tags:\n{src}");
    assert_eq!(
        aliases.len(),
        3,
        "two tags shared one minted namespace:\n{src}"
    );
    // Minted, never the author's alias `Glyph`.
    assert!(
        aliases.iter().all(|a| a.starts_with("vyxp_")),
        "an alias is not minted: {aliases:?}"
    );
}

#[test]
fn an_unchanged_rebuild_regenerates_no_provider() {
    let dir = provider_project(
        "provcache",
        "<span class=\"badge\">\n    <Glyph name=\"github\" label=\"GitHub\"/>\n    <Glyph name=\"discord\" label=\"Discord\"/>\n</span>",
    );
    // This test's OWN cache directory, so the count is not somebody else's work.
    let cache = dir.join("gen-cache");
    let build = || -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .env("VYRN_GEN_CACHE_DIR", &cache)
            .arg("run")
            .arg(dir.join("app.vyrn"))
            .output()
            .expect("run");
        assert!(
            out.status.success(),
            "cached build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    // Each generation entry's name and mtime, so a rewrite shows. Files only: the
    // `wasm/` directory is the compiled engine's store, not a generation.
    let stamp = |cache: &Path| -> Vec<(String, std::time::SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(cache)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.file_type().is_ok_and(|t| t.is_file()))
            })
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().to_string(),
                    e.metadata().unwrap().modified().unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };

    let first = build();
    let after_first = stamp(&cache);
    assert_eq!(
        after_first.len(),
        3,
        "one entry for the template and one per tag: {after_first:?}"
    );
    let second = build();
    assert_eq!(first, second, "the cached rebuild rendered differently");
    assert_eq!(
        after_first,
        stamp(&cache),
        "an unchanged rebuild rewrote a cache entry"
    );

    // The provider's source is one of its generation's inputs; the template's
    // output, one import line, does not change.
    write(
        &dir.join("provider.vyrn"),
        &PROVIDER.replace("M8 0 L16 8 L8 16 Z", "M9 9 L1 1 Z"),
    );
    let third = build();
    assert!(
        third.contains("M9 9 L1 1 Z"),
        "editing the provider was a stale cache hit:\n{third}"
    );
}

/// The only bare capitalized string literals (the shape of a component tag) that
/// `std/vyx.vyrn` may hold outside its `test` blocks. A built-in component needs its
/// name as such a literal, so it would have to declare itself here; components are
/// libraries.
///
/// `Html`, `Data` and `Params` are type spellings in generated code. The `Ui*` names
/// are the stems `std/ui` gives the component it compiles a route file into.
const ALLOWED_CAPITALIZED_LITERALS: &[&str] = &[
    "Data",
    "Html",
    "Params",
    "UiClientData",
    "UiErrorBody",
    "UiLayoutBody",
    "UiPageBody",
];

#[test]
fn std_vyx_names_no_component() {
    let src = std::fs::read_to_string(repo_file("std/vyx.vyrn")).unwrap();
    // The `test` blocks are fixtures that name components because they compile them.
    let cut = src
        .lines()
        .position(|l| l.starts_with("test \""))
        .expect("std/vyx.vyrn has test blocks");
    let code = src.lines().take(cut).collect::<Vec<_>>().join("\n");

    // Every string literal in the code region, skipping `//` comment lines.
    let cs: Vec<char> = code.chars().collect();
    let mut literals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '"' {
            let mut j = i + 1;
            let mut buf = String::new();
            while j < cs.len() {
                if cs[j] == '\\' {
                    j += 2;
                    // An escape cannot be part of a bare identifier; a placeholder
                    // keeps the literal from matching by accident.
                    buf.push('\u{0}');
                    continue;
                }
                if cs[j] == '"' {
                    break;
                }
                buf.push(cs[j]);
                j += 1;
            }
            literals.push(buf);
            i = j + 1;
            continue;
        }
        if cs[i] == '/' && i + 1 < cs.len() && cs[i + 1] == '/' {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            continue;
        }
        i += 1;
    }

    let tag_shaped = |s: &str| {
        let mut it = s.chars();
        matches!(it.next(), Some(c) if c.is_ascii_uppercase())
            && it.all(|c| c.is_ascii_alphanumeric())
    };
    let mut offenders: Vec<&str> = literals
        .iter()
        .map(|s| s.as_str())
        .filter(|s| tag_shaped(s) && !ALLOWED_CAPITALIZED_LITERALS.contains(s))
        .collect();
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "std/vyx.vyrn names a component: {offenders:?} - a component is a library. \
         If one of these is a type spelling or a synthetic stem, add it \
         to ALLOWED_CAPITALIZED_LITERALS with the reason."
    );
}

/// `std/vyx` cannot check a provider's shape: a generator reads only under its own
/// constant path arguments, and a template's provider is never one. The
/// emitted import carries the tag's `//@origin`, so the loader's diagnostic lands on
/// the tag.
#[test]
fn a_provider_that_is_not_a_generator_fails_at_the_tag() {
    let dir = provider_project(
        "provshape",
        "<span class=\"badge\">\n    <Glyph name=\"github\"/>\n</span>",
    );
    // A plain `fn` where the protocol wants a `gen fn`.
    write(
        &dir.join("provider.vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export fn Glyph(a: String) -> Html { return el(\"i\", [], [text(a)]) }\n",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "a non-generator provider must fail the load");
    assert!(
        err.contains("Badge.vyx:7:1: `Glyph` is not an imported `gen fn`"),
        "the loader's own diagnostic lands on the tag:\n{err}"
    );

    // The same for a `gen fn` of the wrong arity.
    write(
        &dir.join("provider.vyrn"),
        "export gen fn Glyph(a: String) -> String { return \"\" }\n",
    );
    let (ok, err) = run_named(&dir, "app.vyrn");
    assert!(!ok, "a wrong-arity provider must fail the load");
    assert!(
        err.contains("Badge.vyx:7:1: generator `Glyph` takes 1 argument(s), got 4"),
        "the arity diagnostic lands on the tag:\n{err}"
    );
}
