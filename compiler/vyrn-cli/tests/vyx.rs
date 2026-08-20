//! Integration tests for the `.vyx` component compiler (RFC-0026 M4, RFC-0039 v2)
//! — the `std/vyx` `components` generator driven through the real `vyrn` binary.
//!
//!   * `emit-gen` the demo and assert the synthesized module's shape (one view
//!     function per component, the keyed `v-for`, the `On` event ABI, `Cls`
//!     classes, the `<slot/>` splice, the `v-html` passthrough, the rebased
//!     relative import);
//!   * generation-diagnostic fixtures (built in tempdirs) each fail the load with
//!     a diagnostic naming the offending `.vyx` file and line: an unclosed
//!     element, a missing `v-for` `:key`, an unknown component tag, a non-scalar
//!     event argument, multiple roots, a malformed props block, and a missing
//!     `<template>` section;
//!   * the demo runs green under `vyrn test`.
//!
//! Generation runs with the cache disabled so a stale entry never masks a
//! regression.

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

// ---- emit-gen: the synthesized module's shape ------------------------------

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

    // One exported pure view function per component, props as parameters.
    assert!(
        src.contains("export fn row(item: Item) -> Html"),
        "row signature:\n{src}"
    );
    assert!(
        src.contains("export fn listing(items: Array<Item>) -> Html"),
        "listing signature:\n{src}"
    );
    // The `{children}`-using component carries the trailing children parameter.
    assert!(
        src.contains("export fn panel(title: String, children: consume Array<Html>) -> Html"),
        "panel signature:\n{src}"
    );

    // A relative script import is rebased so it resolves from the synthesized module.
    assert!(
        src.contains("from \"./vyxcomp/./models\""),
        "rebased import:\n{src}"
    );

    // The keyed {#for} lowers to a loop + keyed pushes; the sibling <Row/> resolves
    // to an internal call.
    assert!(src.contains("for it in items {"), "for loop:\n{src}");
    assert!(
        src.contains("keyed((it.id).toString()"),
        "keyed push:\n{src}"
    );
    assert!(src.contains("row(it)"), "internal component call:\n{src}");

    // The event ABI, a class attr, the {children} splice, and the {@raw} passthrough.
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

// ---- generation diagnostics each name the offending file + line ------------

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
    // Imports ahead of the props block is the required order — it loads and runs.
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

// ---- RFC-0033: origin remapping into the `.vyx` buffer ---------------------

/// A type error inside a template `{expr}` is reported against the `.vyx` file
/// at the exact source column of the expression (not the synthesized module),
/// with the generated location preserved as a note.
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
    // The diagnostic points at the `.vyx` file, the interpolation's line/column.
    assert!(err.contains("Widget.vyx:6:8:"), "remapped location:\n{err}");
    assert!(err.contains("titel"), "carries the checker message:\n{err}");
    // The generated location survives as a note (the `emit-gen` breadcrumb).
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
    // It must NOT be reported against the raw synthesized banner alone.
    assert!(
        !err.contains("generated by components(\"./comp\") at app.vyrn:6:"),
        "not the banner:\n{err}"
    );
}

/// A malformed `//@origin` directive never LOSES the diagnostic: it surfaces at
/// the generated location with the malformed directive noted (RFC-0033
/// guardrail). Driven through a tiny hand-written generator so the malformed
/// directive reaches the frontend exactly as any third-party generator's would.
#[test]
fn malformed_origin_directive_never_loses_the_diagnostic() {
    let dir = scratch("malformed");
    // The generator emits a malformed directive governing a type-erroring line.
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
    // The diagnostic is not dropped and notes the malformed directive.
    assert!(
        err.contains("note: malformed `//@origin` directive"),
        "malformed note:\n{err}"
    );
    // It stays at the generated location (the banner), never silently vanishing.
    assert!(
        err.contains("generated by bad"),
        "kept at generated location:\n{err}"
    );
}

/// Generated text is DATA until the compiler decides otherwise.
///
/// `std/vyx` copies a `<script>` body through verbatim (RFC-0048 §1), so a
/// multi-line string literal in a component puts author-controlled text at the
/// start of a generated line. That text used to be scanned for `//@diag` with no
/// lexical context, so a component copied from a gallery could fail the build at
/// a path outside the project with wording the compiler never wrote. The lexer
/// says which lines are comments; data that merely looks like a directive is
/// inert.
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

/// An anchor resolves; it does not roam (RFC-0099, Containment). RFC-0033 maps
/// generated source to USER source, so a directive that climbs out of the
/// project is refused as malformed — and the never-lose rule keeps the
/// diagnostic at its generated location, saying why.
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

// ---- RFC-0053: lex/parse/load errors in generated code remap too ----------

/// A stray `\` inside a template expression is copied VERBATIM into the
/// synthesized module, where it stops the LEXER — so the module never parses and
/// (before RFC-0053) had no origin map at all, producing a dead-end message that
/// named only a banner key. The map is now built from the synthesized text before
/// it is lexed, so the lex error lands on the `.vyx` line/column the expression
/// starts at, with the generated location kept as a note.
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
    // The dead-end form — a bare banner key with no file the user can open.
    assert!(
        !err.starts_with("generated by components"),
        "no longer reported at the banner alone:\n{err}"
    );
}

/// The same for a PARSE error (the text lexes but does not parse): a template
/// expression that is a syntactic fragment reports at the `.vyx`.
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

/// The never-lose guarantee (RFC-0053, unchanged from RFC-0033): a lex error in
/// generator GLUE — a line no `//@origin` directive governs — keeps its generated
/// location. Better a precise-but-generated location than a wrong one. Driven
/// through a hand-written generator that emits a directive AFTER the broken glue
/// line, so the failing line is genuinely ungoverned.
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
    // Line 1 precedes every directive ⇒ ungoverned ⇒ stays at the banner.
    assert!(
        err.contains("generated by glue"),
        "kept at the generated location:\n{err}"
    );
    assert!(
        !err.contains("a.vyx"),
        "not attributed to an unrelated origin:\n{err}"
    );
}

// ---- RFC-0036: componentsThemed — compile-checked classes against a theme --

/// The app that imports a view function from the THEMED generator over `./comp`,
/// threading `./theme.json` (resolved relative to the app, exactly like `./comp`).
const THEMED_APP: &str = "import { componentsThemed } from \"std/vyx\"\n\
     import { widget } from componentsThemed(\"./comp\", \"./theme.json\")\n\
     fn main() -> Int64 { return 0 }\n";

/// A minimal theme: enough to derive `flex`/`p-2` utilities, plus a `safelist`
/// carrying two bespoke names so `class=\"card …\"` checks with no CSS rule.
const THEME_JSON: &str = "{ \"colors\": { \"brand\": \"#123456\" },\n\
     \"spacing\": { \"2\": \"0.5rem\" },\n\
     \"safelist\": [\"card\", \"book-row\"] }\n";

/// A themed build compile-checks a STATIC `class` literal against `Tw`: a typo'd
/// utility (`flx`) is a load error reported against the `.vyx` at the exact column
/// of the class string (the RFC-0036 origin-fidelity upgrade — a static class gets
/// its own column-exact `//@origin`, not a region-level one).
#[test]
fn themed_typo_class_remaps_to_the_vyx_column() {
    let dir = scratch("themed_typo");
    // `<li class="flx">` on line 2; `<li class="` is 11 chars, so `flx` starts at
    // column 12. `flx` is neither a derived utility nor safelisted ⇒ a `Tw` error.
    write(
        &dir.join("comp/Widget.vyx"),
        "<template>\n<li class=\"flx\">x</li>\n</template>\n",
    );
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a typo'd utility class must fail the load");
    // The diagnostic lands column-exactly on the class string inside the `.vyx`.
    assert!(
        err.contains("Widget.vyx:2:12:"),
        "remapped to the class column:\n{err}"
    );
    assert!(err.contains("flx"), "carries the offending class:\n{err}");
    // The generated location survives as an `emit-gen` breadcrumb note.
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
}

/// A themed build accepts a mix of a safelisted bespoke name and derived utilities
/// (`card flex p-2`) — the safelist folds `card` into the checked vocabulary, and
/// the dynamic `class={cls}` coerces at runtime. The app loads and runs green.
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

/// The themed emission is byte-identical at runtime to the bare one: `vyxTheme.cls`
/// returns `Cls(c)`, so `emit-gen` shows the class routed through the checked
/// bridge while the module imports the theme namespaced.
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

// ---- the demo runs green ---------------------------------------------------

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

// ---- std/vyx's own unit suite runs in CI -----------------------------------

/// The `std/vyx` module carries an inline `test` suite pinning the scanner
/// (including all six RFC-0039 audit reproducers, now driven by the RFC-0054 M4b
/// `lex()`-based keyword finder) and the emitters. Run it through the real binary
/// so a scanner/emitter regression fails `cargo test`, not only a manual
/// `vyrn test std/vyx.vyrn`.
#[test]
fn std_vyx_unit_tests_run_green() {
    let module = repo_file("std/vyx.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std/vyx unit tests failed:\n{combined}"
    );
    assert!(
        combined.contains("48 passed, 0 failed"),
        "expected 48 green tests:\n{combined}"
    );
}

// ---- RFC-0054 M4b: the six audit reproducers, end-to-end through the binary --
// The `lex()`-based script scanner makes each historical miscompile structurally
// impossible. These drive the FULL `components` pipeline (not just the inline
// unit fns) so a regression in the real generator — the interpreter running
// `lex()` over a script section — is caught.

/// A comment (or string) mentioning `props` before the real block never conjures
/// a phantom props block: the emitted view fn takes exactly the declared prop.
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

/// A helper identifier named `props` (`let props = …`) is not a props block —
/// the view fn takes no parameters and the helper passes through verbatim.
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

/// A `</script>` inside a helper string does not truncate the section — the
/// helper (and everything after the string) reaches the synthesized module.
#[test]
fn audit_close_script_in_string_does_not_truncate() {
    let dir = scratch("audit_closescript");
    write(
        &dir.join("comp/Widget.vyx"),
        "<script>\nfn tag() -> String { return \"</script>\" }\nprops { n: Int64 }\n</script>\n<template><li>{{ n }}{{ tag() }}</li></template>\n",
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
    // The `props` AFTER the string was reached (section not truncated at the string).
    assert!(
        src.contains("export fn widget(n: Int64) -> Html"),
        "section not truncated:\n{src}"
    );
    assert!(
        src.contains("fn tag() -> String"),
        "helper survived:\n{src}"
    );
}

/// A literal `{ a }` in template TEXT stays literal (it is not a `{{ … }}`
/// interpolation) — the text node carries the braces verbatim.
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

/// An HTML comment inside the template is stripped, not parsed as a bad tag.
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

// ---- the template section is markup, and is scanned by markup's rules -------
//
// Each fixture below is well-formed markup that the code-mode scanner misread:
// the oracle is that the generated module PARSES and runs, not that the
// generator survived.

/// A bare `"` in text content is a quotation mark. Read as the start of a string
/// literal it swallowed the real `</template>`, and the component came back with
/// a no-`<template>` diagnostic for a template that was right there.
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

/// An attribute value may be `'`-quoted (the documented Vue convention), and then
/// it may carry a `"`.
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

/// An HTML comment holds no tags: a `</template>` inside one closed the section
/// early and truncated everything after it.
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

    // And the content past the comment really is in the module.
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

/// A nested `<template>` element is closed by its own tag, so the section's
/// extent reaches the outer one.
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

/// An empty template is found — and then fails on its own terms (no root), never
/// as a missing section.
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

// ---- the event-argument arity guard counts brackets, not comparisons --------

/// `pick(a > b, c)` is two arguments. Counting `>` as a closing bracket drove the
/// depth negative, hid the comma, and emitted a two-argument call into a
/// one-argument call site — `expected RParen, found Comma`.
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

/// And one argument stays one argument, comparison or not.
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

// ---- the section boundary: one rule, two implementations -------------------

/// Hostile `<script>` bodies whose FIRST `</script>` is not the section's: it
/// sits inside a string or a comment. Each is followed by an import and a
/// `props` block, so anything that reaches those saw past the decoy.
const DECOY_SCRIPTS: &[(&str, &str)] = &[
    ("string", "fn tag() -> String { return \"</script>\" }"),
    ("line_comment", "// </script>"),
    (
        "escaped_quote",
        "fn tag() -> String { return \"a\\\"</script>b\" }",
    ),
];

/// The `.vyx` section boundary is decided by two implementations that cannot
/// share code — `std/vyx`'s scanner, which compiles the component, and
/// `vyrn_frontend::vyx`, which the tools read a `.vyx` with — so this asserts
/// they answer the same on every hostile body: the generator compiles the props
/// that follow the decoy, and `vyrn why` reports the import that follows it.
///
/// It fails if EITHER drifts. The tool half was a naive `find("</script>")`
/// before `vyrn_frontend::vyx` existed, and `why` denied an import the program
/// makes.
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

        // `std/vyx`'s answer: the props AFTER the decoy became parameters, so
        // its scanner ran past the decoy to the real close tag.
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

        // The tools' answer, over the same file: the import AFTER the decoy is
        // an edge of the project graph.
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

// ---- RFC-0107 M1: generation-time components (the provider protocol) -------
//
// A provider is an ordinary library module exporting a `gen fn` of the
// conventional shape `(attrs, file, line, col) -> String`, whose generated module
// exports `provide() -> Html`. `std/vyx` names no provider: the tag resolves
// against what a `<script>` imported, and the tag becomes a NESTED generation.

/// The toy provider: ordinary Vyrn that knows nothing about `.vyx`. It reads the
/// attribute object `std/vyx` handed it with the shared JSON reader, and reports
/// an unknown glyph at the anchor it was given rather than in its own source.
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
    // Both tags reached the provider with their own attributes, and both trees
    // were spliced where the tags stood.
    assert!(
        out.contains("<span class=\"badge\"><svg aria-label=\"GitHub\">M8 0 L16 8 L8 16 Z</svg><svg aria-label=\"Discord\">M2 2 H14 V14 H2 Z</svg></span>"),
        "the provider's trees are not spliced at the tags:\n{out}"
    );

    // The emitted module shows the mechanism: one nested generator import per
    // tag, the tag's static attributes as ONE JSON constant, the tag's own file
    // and line as the anchor, and a `provide()` call where the tag stood.
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
    // The provider never read the `.vyx`; the anchor travelled to it as
    // arguments, and `std/diag` put its report on the tag's line.
    assert!(
        err.contains("Badge.vyx:7:1: no glyph `githup` here - nearest is `github`"),
        "the provider's report is not anchored at the tag:\n{err}"
    );
}

/// ONE provider tag, which is the case RFC-0107 M1 never ran: every green row it
/// wrote had two, and two generated modules exporting the same name clash into a
/// different code path than one does. With the entry point still called `render`
/// a single tag failed the build in `std/vyx` itself —
/// "function `render` is defined in `generated by Glyph(…)` but not imported
/// here" — because `render` is one of RFC-0054's surface builtins, those are
/// deliberately not reserved, and the shadowing is decided across the whole
/// program rather than per module. M3 renamed the entry point to `provide`; this
/// row is what keeps it renamed.
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
    // The message RFC-0107 M0 caught over-promising is now true of both paths.
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
    // A sibling component, and a second sibling whose OWN `<script>` imports
    // nothing - the import namespace is flat across the set, so its tag resolves
    // through `Badge.vyx`'s import exactly as the synthesized module does.
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
    // Minted, never the author's: the alias the `<script>` wrote is `Glyph`.
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
    // Every cache entry's name and modification time, so a REWRITE is visible and
    // not just a new key.
    let stamp = |cache: &Path| -> Vec<(String, std::time::SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(cache)
            .unwrap()
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
    // The template's own generation plus one per provider tag.
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

    // The provider's own source is one of ITS recorded inputs, so editing it
    // invalidates the provider's generation - and not the template's, whose
    // output is one import line that did not change.
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

// ---- the negative gate: `std/vyx` names no component -----------------------

/// The rule, stated precisely enough to test: **outside its `test` blocks,
/// `std/vyx.vyrn` contains no string literal that is a bare capitalized
/// identifier - the shape of a component tag - except the names below.**
///
/// A privileged built-in component cannot be added without one: whether it is
/// compared against a tag, seeded into the registry, or emitted as the callee at
/// a tag site, its name has to appear as such a literal. So this list is the
/// place a hardwired `<Icon>` would have to declare itself, in the open, and the
/// RFC-0107 line - "directives are the language; components are libraries" - is
/// asserted rather than hoped for.
///
/// The allowed names are of two kinds, neither a component this compiler
/// provides:
///
///   * `Html`, `Data`, `Params` - TYPE spellings written into generated code.
///   * `UiPageBody`, `UiLayoutBody`, `UiErrorBody`, `UiClientData` - the stems
///     `std/ui` gives the synthetic component it compiles a route file INTO. They
///     name the user's own page, not a widget any template can import.
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
    // The `test` blocks are fixtures, not the compiler; they name components
    // (`Item`, `Btn`, ...) because they compile them. The rule is about the code.
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
        "std/vyx.vyrn names a component: {offenders:?} - a component is a library \
         (RFC-0107). If one of these is a type spelling or a synthetic stem, add it \
         to ALLOWED_CAPITALIZED_LITERALS with the reason."
    );
}

/// `std/vyx` does not check a provider's shape before emitting the call, and
/// cannot: a generator may only read under its own constant path arguments, and a
/// provider named by a TEMPLATE is never one of them (RFC-0107 M0, P1a/P1b). The
/// decision costs nothing here, because the emitted import's own diagnostic is
/// already specific AND already anchored at the tag - the import carries the
/// tag's `//@origin`.
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
