//! Integration tests for the pages generator (RFC-0026 M3) — the `std/ui`
//! `pages` generator driven through the real `vyrn` binary.
//!
//!   * `emit-gen` the demo and assert the synthesized router's shape (the
//!     aliased page imports, the co-naming dummies, `RoutePath` + typed-URL
//!     helpers, the segment splitter, per-route `try`/`render`, and `route`);
//!   * three generation-failure fixtures (built in tempdirs) each fail the load
//!     with a diagnostic naming the offending file: a Params/segment mismatch,
//!     an unsupported param type, and a route collision;
//!   * the demo runs green under `vyrn test`.
//!
//! Generation runs with the cache disabled so a stale entry never masks a
//! regression.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_file(rel: &str) -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh, empty scratch directory with an empty `pages/` for a test's fixtures.
fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_pages_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// The one-line app that imports `route` from the generator over `./pages`.
const APP: &str = "import { pages } from \"std/ui\"\n\
     import { route } from pages(\"./pages\")\n\
     fn main() -> Int64 { return 0 }\n";

// ---- emit-gen: the synthesized router's shape ------------------------------

#[test]
fn emit_gen_shows_the_synthesized_router() {
    let demo = repo_file("examples/pagesdemo.vyrn");
    let out = vyrn().arg("emit-gen").arg(&demo).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);

    // Page modules are bound under per-route namespaces (RFC-0027): same-named
    // exports across pages coexist with no aliasing and no co-naming dummies.
    assert!(src.contains("import * as p0 from \"./pages/index\""), "namespace page import:\n{src}");
    assert!(src.contains("p0.page()"), "namespaced static page call:\n{src}");
    assert!(src.contains(".Params { "), "namespaced Params construction:\n{src}");
    assert!(src.contains(".load(p)"), "namespaced load call:\n{src}");
    assert!(src.contains(".page(p, d)"), "namespaced loader page call:\n{src}");
    // The obsolete co-naming dummies are gone.
    assert!(!src.contains("fn page() -> Int64"), "no page dummy:\n{src}");
    assert!(!src.contains("type Params = Int64"), "no Params dummy:\n{src}");

    // RoutePath — the regex-validated string of the whole route language, with an
    // Int64 param as its integer-spelling regex.
    assert!(
        src.contains("export type RoutePath = String where value =~ \"(")
            && src.contains("/users/(0|-?[1-9][0-9]*)"),
        "RoutePath finite regex:\n{src}"
    );

    // Typed-URL helpers: one per dynamic route, one per static route.
    assert!(src.contains("export fn hrefUsers(id: Int64) -> RoutePath"), "dynamic helper:\n{src}");
    assert!(src.contains("export fn hrefItems(id: Int64) -> RoutePath"), "dynamic helper:\n{src}");
    assert!(src.contains("export fn itemsPath() -> RoutePath"), "static helper:\n{src}");
    assert!(src.contains("export fn rootPath() -> RoutePath"), "root helper:\n{src}");

    // The dynamic segment is validated against the declared type before user code.
    assert!(src.contains("fromJson(UiRouteInt, segs["), "dynamic segment parse:\n{src}");
    // The loader's Invalid arm renders a 422 error page.
    assert!(src.contains("status: 422"), "error-page status:\n{src}");
    // The exported entry point.
    assert!(src.contains("export fn route(req: Request) -> Response"), "route entry:\n{src}");
}

// ---- generation failures each name the offending file ----------------------

#[test]
fn params_segment_mismatch_fails_naming_the_file() {
    let dir = scratch("mismatch");
    // The `[id]` segment has no matching Params field (the field is `slug`).
    write(
        &dir.join("pages/users/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { slug: Int64 }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "a Params/segment mismatch must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("PAGES_PARAM_MISMATCH"), "mismatch diagnostic:\n{err}");
    assert!(err.contains("users"), "diagnostic names the file:\n{err}");
}

/// RFC-0033 (second producer): a page whose `page` returns the wrong type
/// passes generation-time inspection (which checks arity, not the return type),
/// but the check error in the synthesized router's dispatch glue is reported
/// against the PAGE module — proving origin maps aren't `.vyx`-shaped.
#[test]
fn page_type_error_remaps_to_the_page_module() {
    let dir = scratch("uiremap");
    // A static page whose `page()` returns `Int64` — `document(…, page())`
    // requires `Html`, so the router fails to type-check.
    write(&dir.join("pages/index.vyrn"), "export fn page() -> Int64 { return 0 }\n");
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("check");
    assert!(!out.status.success(), "a wrong page return type must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    // Reported against the page module (region-level, line 1), not the router.
    assert!(err.contains("pages/index.vyrn:1:1:"), "remapped to the page file:\n{err}");
    assert!(err.contains("expects Html"), "carries the checker message:\n{err}");
    assert!(err.contains("note: in generated code"), "keeps the generated note:\n{err}");
}

#[test]
fn unsupported_param_type_fails_naming_the_file() {
    let dir = scratch("badtype");
    // `Int64`/`String` are supported (RFC-0039 §5); `Float64` is not.
    write(
        &dir.join("pages/tag/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { id: Float64 }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "an unsupported param type must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("PAGES_UNSUPPORTED_PARAM_TYPE"), "unsupported-type diagnostic:\n{err}");
    assert!(err.contains("tag"), "diagnostic names the file:\n{err}");
}

/// A `String` dynamic segment (RFC-0039 §5) matches any non-empty, non-`/`
/// segment and binds it into `Params`; a raw-response page exports `respond`
/// for full content-type/status control. Both route through the generated
/// router, and a `Float64`-looking or empty segment is handled correctly.
#[test]
fn string_segment_and_respond_route_end_to_end() {
    let dir = scratch("stringseg");
    write(&dir.join("pages/index.vyrn"), "import { el, text, Html } from \"std/html\"\nexport fn page() -> Html { return el(\"h1\", [], [text(\"home\")]) }\n");
    write(
        &dir.join("pages/p/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { id: String }\n\
         export fn page(p: Params) -> Html { return el(\"h1\", [], [text(\"paste \" + p.id)]) }\n",
    );
    write(
        &dir.join("pages/raw/[id].vyrn"),
        "export type Params = { id: String }\n\
         export fn respond(p: Params) -> Response {\n\
         return Response { status: 200, contentType: \"text/plain; charset=utf-8\", body: \"raw:\" + p.id }\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path, body: \"\" }) }\n\
         fn main() -> Int64 {\n\
         let a = h(\"/p/deadbeef\")\n\
         print(\"P:\\{a.status}:\\{a.body.byteLength}\")\n\
         let b = h(\"/raw/cafe\")\n\
         print(\"R:\\{b.status}:\\{b.contentType}:\\{b.body}\")\n\
         return 0\n\
         }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let combined = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "String-segment + respond app must run:\n{combined}");
    // The String segment binds "deadbeef" and renders an HTML document (200).
    assert!(combined.contains("P:200:"), "String segment page renders 200:\n{combined}");
    // The respond page owns the content type and body verbatim.
    assert!(combined.contains("R:200:text/plain; charset=utf-8:raw:cafe"), "respond raw bytes:\n{combined}");
}

/// A `.vyx` page (RFC-0039 §4) routes through `pagesThemed`: its `params {}`
/// block binds the bracket segment, its `fn load` runs, its template classes are
/// theme-checked, and a non-integer `Int64` segment 404s before user code.
#[test]
fn vyx_page_with_loader_routes_through_pages_themed() {
    let dir = scratch("vyxpage");
    write(
        &dir.join("pages/index.vyx"),
        "<template>\n<main class=\"home\"><h1>home</h1></main>\n</template>\n",
    );
    write(
        &dir.join("pages/book/[id].vyx"),
        "<script>\n\
         params { id: Int64 }\n\
         fn load(p: Params) -> Validation<Data> {\n\
         return Valid(Data { title: \"Book #\" + p.id.toString() })\n\
         }\n\
         type Data = { title: String }\n\
         </script>\n\
         <template>\n\
         <article class=\"book\"><h1>{{ data.title }}</h1><p class=\"p-2\">id {{ id }}</p></article>\n\
         </template>\n",
    );
    write(
        &dir.join("theme.json"),
        "{ \"spacing\": { \"2\": \"0.5rem\" }, \"safelist\": [\"home\", \"book\"] }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pagesThemed } from \"std/ui\"\n\
         import { route } from pagesThemed(\"./pages\", \"./theme.json\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path, body: \"\" }) }\n\
         fn main() -> Int64 {\n\
         let a = h(\"/\")\n\
         print(\"home:\\{a.status}\")\n\
         let b = h(\"/book/42\")\n\
         print(\"book:\\{b.status}:\\{b.body.contains(\"Book #42\")}:\\{b.body.contains(\"id 42\")}\")\n\
         let c = h(\"/book/notint\")\n\
         print(\"badid:\\{c.status}\")\n\
         return 0\n\
         }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let combined = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), ".vyx pages app must run:\n{combined}");
    assert!(combined.contains("home:200"), "static .vyx page:\n{combined}");
    assert!(combined.contains("book:200:true:true"), "loader .vyx page binds segment + Data:\n{combined}");
    assert!(combined.contains("badid:404"), "non-integer Int64 segment 404s:\n{combined}");
}

#[test]
fn route_collision_fails_naming_both_files() {
    let dir = scratch("collision");
    // Two dynamic pages under the same directory claim the same route `/a/:`.
    write(
        &dir.join("pages/a/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { id: Int64 }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(
        &dir.join("pages/a/[slug].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { slug: Int64 }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "a route collision must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("PAGES_ROUTE_COLLISION"), "collision diagnostic:\n{err}");
    // Names both offending files.
    assert!(err.contains("id") && err.contains("slug"), "diagnostic names both files:\n{err}");
}

// ---- imported Params/Data (RFC-0031: the reachable type closure) -----------

#[test]
fn imported_params_type_works_via_the_closure() {
    let dir = scratch("importedparams");
    // The page's `Params`/`Data` live in a SHARED module the page imports —
    // before RFC-0031 `moduleInterface` saw only the page's own declarations, so
    // this failed with PAGES_MISSING_PARAMS_TYPE. The closure hands the generator
    // the imported declarations, and the router imports `Params` from its
    // declaring module (it is not reachable as `p0.Params` — namespaces reach a
    // module's own exports only).
    write(
        &dir.join("shared.vyrn"),
        "export type Params = { id: Int64 }\n\
         export type Data = { label: String }\n",
    );
    write(
        &dir.join("pages/users/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         import { Params, Data } from \"../../shared\"\n\
         export fn load(p: Params) -> Validation<Data> {\n\
             return Valid(Data { label: \"user\\{p.id}\" })\n\
         }\n\
         export fn page(p: Params, d: Data) -> Html {\n\
             return el(\"main\", [], [text(d.label)])\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn main() -> Int64 {\n\
             let r = route(Request { method: \"GET\", path: \"/users/7\", body: \"\" })\n\
             print(\"\\{r.status}\")\n\
             return 0\n\
         }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "imported-Params page must load and run:\n{combined}");
    assert!(combined.contains("200"), "the dynamic route renders (200):\n{combined}");

    // The synthesized router reaches the foreign `Params` through an aliased
    // import from its declaring module, not through the page namespace.
    let eg = vyrn().arg("emit-gen").arg(dir.join("app.vyrn")).output().expect("emit-gen");
    let src = String::from_utf8_lossy(&eg.stdout);
    assert!(
        src.contains("import { Params as uiParams0 } from \"./shared\""),
        "foreign Params import:\n{src}"
    );
    assert!(src.contains("uiParams0 { "), "foreign Params construction:\n{src}");
}

// ---- RFC-0041: layouts, head, error pages ----------------------------------

/// A `routes/layout.vyx` wraps every page body (its `<slot/>`), a page/layout
/// `head { … }` block threads `<link>`/`<script>`/dynamic `<title>` into the
/// document head, a `load -> Result<Data, PageError>` failure renders the nearest
/// `error.vyx` at the carried status, a `Validation` failure folds into a 422
/// error page, and `layout="none"` opts a page out of the shell.
#[test]
fn layout_head_and_error_pages_route_end_to_end() {
    let dir = scratch("layout");
    write(&dir.join("theme.json"), "{ \"safelist\": [\"shell\", \"home\", \"book\", \"err\", \"solo\"] }\n");
    // The layout: the shell (with a <slot/>) plus a head block (stylesheet + boot).
    write(
        &dir.join("pages/layout.vyx"),
        "<script>\nhead {\n    stylesheet \"/style.css\"\n    module \"/nav.js\"\n}\n</script>\n\
         <template>\n<div class=\"shell\"><nav>bin</nav><main><slot/></main></div>\n</template>\n",
    );
    write(&dir.join("pages/index.vyx"), "<template>\n<h1 class=\"home\">Home</h1>\n</template>\n");
    // A Result loader: Ok renders with a dynamic head title, Err → the error page.
    write(
        &dir.join("pages/p/[id].vyx"),
        "<script>\n\
         import { PageError, notFound } from \"std/ui\"\n\
         params { id: String }\n\
         head {\n    title: data.name\n}\n\
         fn load(p: Params) -> Result<Data, PageError> {\n\
         if p.id == \"good\" {\n    return Ok(Data { name: \"Good One\" })\n}\n\
         return Err(notFound(\"no id \" + p.id))\n}\n\
         type Data = { name: String }\n\
         </script>\n\
         <template>\n<article class=\"book\"><h1>{{ data.name }}</h1></article>\n</template>\n",
    );
    // A Validation loader → 422 folded into a PageError.
    write(
        &dir.join("pages/v/[id].vyx"),
        "<script>\nparams { id: Int64 }\n\
         fn load(p: Params) -> Validation<Data> {\n\
         if p.id > 0 {\n    return Valid(Data { n: p.id })\n}\n\
         return Invalid([Issue { key: \"id.pos\", path: \"id\", message: \"must be positive\" }])\n}\n\
         type Data = { n: Int64 }\n</script>\n\
         <template>\n<p class=\"book\">n {{ data.n }}</p>\n</template>\n",
    );
    // The themed error page: reads the injected `error` prop.
    write(
        &dir.join("pages/error.vyx"),
        "<template>\n<section class=\"err\"><h1>Oops {{ error.status }}</h1><p>{{ error.message }}</p></section>\n</template>\n",
    );
    // A page opting out of the layout entirely.
    write(
        &dir.join("pages/solo/index.vyx"),
        "<script>\nlayout=\"none\"\n</script>\n<template>\n<h1 class=\"solo\">Solo</h1>\n</template>\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pagesThemed } from \"std/ui\"\n\
         import { route } from pagesThemed(\"./pages\", \"./theme.json\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path, body: \"\" }) }\n\
         fn main() -> Int64 {\n\
         let a = h(\"/\")\n\
         print(\"home:\\{a.status}:\\{a.body.contains(\"class=\\\"shell\\\"\")}:\\{a.body.contains(\"/style.css\")}\")\n\
         let b = h(\"/p/good\")\n\
         print(\"good:\\{b.status}:\\{b.body.contains(\"<title>Good One</title>\")}:\\{b.body.contains(\"class=\\\"shell\\\"\")}\")\n\
         let c = h(\"/p/bad\")\n\
         print(\"bad:\\{c.status}:\\{c.body.contains(\"Oops 404\")}:\\{c.body.contains(\"no id bad\")}:\\{c.body.contains(\"class=\\\"shell\\\"\")}\")\n\
         let d = h(\"/v/-1\")\n\
         print(\"val:\\{d.status}:\\{d.body.contains(\"Oops 422\")}:\\{d.body.contains(\"must be positive\")}\")\n\
         let e = h(\"/solo\")\n\
         print(\"solo:\\{e.status}:\\{e.body.contains(\"class=\\\"shell\\\"\")}:\\{e.body.contains(\"Solo\")}\")\n\
         return 0\n\
         }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    let combined = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "layout/error app must run:\n{combined}");
    // Home: wrapped in the layout, the layout head stylesheet threaded.
    assert!(combined.contains("home:200:true:true"), "layout wrap + head:\n{combined}");
    // Result Ok: dynamic <title> from the page head block, still under the layout.
    assert!(combined.contains("good:200:true:true"), "dynamic head title under layout:\n{combined}");
    // Result Err: the themed error page at the carried 404, wrapped in the layout.
    assert!(combined.contains("bad:404:true:true:true"), "Result error page:\n{combined}");
    // Validation Invalid: folded into a 422 error page.
    assert!(combined.contains("val:422:true:true"), "Validation 422 error page:\n{combined}");
    // layout="none": no shell.
    assert!(combined.contains("solo:200:false:true"), "layout opt-out:\n{combined}");
}

/// A `layout.vyx` without a `<slot/>` is a named generation diagnostic.
#[test]
fn a_layout_without_a_slot_is_a_diagnostic() {
    let dir = scratch("noslot");
    write(&dir.join("pages/layout.vyx"), "<template>\n<div>no slot</div>\n</template>\n");
    write(&dir.join("pages/index.vyx"), "<template>\n<h1>home</h1>\n</template>\n");
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "a slot-less layout must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("VYX_LAYOUT_NO_SLOT"), "no-slot diagnostic:\n{err}");
}

// ---- the demo runs green ---------------------------------------------------

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/pagesdemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(combined.contains("5 passed, 0 failed"), "expected 5 green tests:\n{combined}");
}
