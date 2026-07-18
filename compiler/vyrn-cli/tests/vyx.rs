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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
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
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let combined = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    (out.status.success(), combined)
}

// ---- emit-gen: the synthesized module's shape ------------------------------

#[test]
fn emit_gen_shows_the_synthesized_component_module() {
    let demo = repo_file("examples/vyxdemo.vyrn");
    let out = vyrn().arg("emit-gen").arg(&demo).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);

    // One exported pure view function per component, props as parameters.
    assert!(src.contains("export fn row(item: Item) -> Html"), "row signature:\n{src}");
    assert!(src.contains("export fn listing(items: Array<Item>) -> Html"), "listing signature:\n{src}");
    // The `{children}`-using component carries the trailing children parameter.
    assert!(src.contains("export fn panel(title: String, children: Array<Html>) -> Html"), "panel signature:\n{src}");

    // A relative script import is rebased so it resolves from the synthesized module.
    assert!(src.contains("from \"./vyxcomp/./models\""), "rebased import:\n{src}");

    // The keyed {#for} lowers to a loop + keyed pushes; the sibling <Row/> resolves
    // to an internal call.
    assert!(src.contains("for it in items {"), "for loop:\n{src}");
    assert!(src.contains("keyed((it.id).toString()"), "keyed push:\n{src}");
    assert!(src.contains("row(it)"), "internal component call:\n{src}");

    // The event ABI, a class attr, the {children} splice, and the {@raw} passthrough.
    assert!(src.contains("On(\"click\", \"removeRow\", (item.id).toString())"), "event lowering:\n{src}");
    assert!(src.contains("On(\"input\", \"setQty\""), "input event:\n{src}");
    assert!(src.contains("Cls(\"row\")"), "class -> Cls:\n{src}");
    assert!(src.contains("for vyxCh in children {"), "children splice:\n{src}");
    assert!(src.contains("Raw("), "{{@raw}} -> Raw:\n{src}");
}

// ---- generation diagnostics each name the offending file + line ------------

#[test]
fn unclosed_element_fails_naming_the_file_and_line() {
    let dir = scratch("unclosed");
    write(&dir.join("comp/Widget.vyx"), "<template>\n<li>oops\n</template>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "an unclosed element must fail to load");
    assert!(err.contains("VYX_UNCLOSED_ELEMENT"), "unclosed diagnostic:\n{err}");
    assert!(err.contains("Widget_vyx"), "diagnostic names the file:\n{err}");
    assert!(err.contains("line_"), "diagnostic carries a line:\n{err}");
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
    assert!(err.contains("VYX_MISSING_FOR_KEY"), "missing-key diagnostic:\n{err}");
    assert!(err.contains("Widget_vyx"), "diagnostic names the file:\n{err}");
}

#[test]
fn unknown_component_fails_naming_the_tag() {
    let dir = scratch("unknowncomp");
    write(&dir.join("comp/Widget.vyx"), "<template>\n<ul><Missing :x=\"1\"/></ul>\n</template>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "an unknown component tag must fail to load");
    assert!(err.contains("VYX_UNKNOWN_COMPONENT"), "unknown-component diagnostic:\n{err}");
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
    assert!(err.contains("VYX_NON_SCALAR_EVENT_ARG"), "non-scalar diagnostic:\n{err}");
}

#[test]
fn multiple_roots_fail() {
    let dir = scratch("roots");
    write(&dir.join("comp/Widget.vyx"), "<template>\n<li>a</li>\n<li>b</li>\n</template>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a template with multiple roots must fail to load");
    assert!(err.contains("VYX_MULTIPLE_ROOTS"), "multiple-roots diagnostic:\n{err}");
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
    assert!(err.contains("VYX_BAD_PROPS"), "bad-props diagnostic:\n{err}");
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
    assert!(err.contains("VYX_IMPORTS_FIRST"), "imports-first diagnostic:\n{err}");
    assert!(err.contains("Widget_vyx"), "diagnostic names the file:\n{err}");
    assert!(err.contains("line_3"), "diagnostic carries the import's line:\n{err}");
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
    write(&dir.join("comp/Widget.vyx"), "<script>props { x: Int64 }</script>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a .vyx with no <template> must fail to load");
    assert!(err.contains("VYX_NO_TEMPLATE"), "no-template diagnostic:\n{err}");
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
    assert!(err.contains("note: in generated code"), "keeps the generated note:\n{err}");
    // It must NOT be reported against the raw synthesized banner alone.
    assert!(!err.contains("generated by components(\"./comp\") at app.vyrn:6:"), "not the banner:\n{err}");
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
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the type error must fail the load");
    // The diagnostic is not dropped and notes the malformed directive.
    assert!(err.contains("note: malformed `//@origin` directive"), "malformed note:\n{err}");
    // It stays at the generated location (the banner), never silently vanishing.
    assert!(err.contains("generated by bad"), "kept at generated location:\n{err}");
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
    write(&dir.join("comp/Widget.vyx"), "<template>\n<li class=\"flx\">x</li>\n</template>\n");
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a typo'd utility class must fail the load");
    // The diagnostic lands column-exactly on the class string inside the `.vyx`.
    assert!(err.contains("Widget.vyx:2:12:"), "remapped to the class column:\n{err}");
    assert!(err.contains("flx"), "carries the offending class:\n{err}");
    // The generated location survives as an `emit-gen` breadcrumb note.
    assert!(err.contains("note: in generated code"), "keeps the generated note:\n{err}");
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
    assert!(ok, "a safelisted + utility class mix must load and run:\n{err}");
}

/// The themed emission is byte-identical at runtime to the bare one: `vyxTheme.cls`
/// returns `Cls(c)`, so `emit-gen` shows the class routed through the checked
/// bridge while the module imports the theme namespaced.
#[test]
fn themed_emit_gen_routes_class_through_vyx_theme() {
    let dir = scratch("themed_emit");
    write(&dir.join("comp/Widget.vyx"), "<template>\n<li class=\"card\">x</li>\n</template>\n");
    write(&dir.join("theme.json"), THEME_JSON);
    write(&dir.join("app.vyrn"), THEMED_APP);
    let out = vyrn().arg("emit-gen").arg(dir.join("app.vyrn")).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);
    assert!(src.contains("import * as vyxTheme from tw(\"./theme.json\")"), "themed import:\n{src}");
    assert!(src.contains("vyxTheme.cls(\"card\")"), "class routed through vyxTheme.cls:\n{src}");
    assert!(src.contains("//@origin"), "carries origin directives:\n{src}");
}

// ---- the demo runs green ---------------------------------------------------

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/vyxdemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(combined.contains("1 passed, 0 failed"), "expected 1 green test:\n{combined}");
}
