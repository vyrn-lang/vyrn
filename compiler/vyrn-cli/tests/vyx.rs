//! Integration tests for the `.vyx` component compiler (RFC-0026 M4) — the
//! `std/vyx` `components` generator driven through the real `vyrn` binary.
//!
//!   * `emit-gen` the demo and assert the synthesized module's shape (one view
//!     function per component, the keyed `{#for}`, the `On` event ABI, `Cls`
//!     classes, the `{children}` splice, the `{@raw}` passthrough, the rebased
//!     relative import);
//!   * generation-diagnostic fixtures (built in tempdirs) each fail the load with
//!     a diagnostic naming the offending `.vyx` file and line: an unclosed
//!     element, a missing `{#for}` key, an unknown component tag, a non-scalar
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
        "<template>\n<ul>\n{#for x in xs}<li>{x}</li>{/for}\n</ul>\n</template>\n",
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
    write(&dir.join("comp/Widget.vyx"), "<template>\n<ul><Missing x={1}/></ul>\n</template>\n");
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
fn missing_template_section_fails() {
    let dir = scratch("notemplate");
    write(&dir.join("comp/Widget.vyx"), "<script>props { x: Int64 }</script>\n");
    write(&dir.join("app.vyrn"), APP);
    let (ok, err) = run_app(&dir);
    assert!(!ok, "a .vyx with no <template> must fail to load");
    assert!(err.contains("VYX_NO_TEMPLATE"), "no-template diagnostic:\n{err}");
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
