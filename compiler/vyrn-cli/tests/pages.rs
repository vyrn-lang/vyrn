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

    // Page modules are bound under per-route namespaces (RFC-0027): same-named
    // exports across pages coexist with no aliasing and no co-naming dummies.
    assert!(
        src.contains("import * as p0 from \"./pages/index\""),
        "namespace page import:\n{src}"
    );
    assert!(
        src.contains("p0.page()"),
        "namespaced static page call:\n{src}"
    );
    assert!(
        src.contains(".Params { "),
        "namespaced Params construction:\n{src}"
    );
    // RFC-0071: a `.vyrn` page's data is its declared `data` member, run through
    // the runner its return TYPE named — `p<idx>.load(p)` is gone with the name
    // match it came from.
    assert!(
        src.contains("runParamQuery("),
        "the declared query's runner:\n{src}"
    );
    assert!(src.contains(".data()"), "namespaced data call:\n{src}");
    assert!(
        !src.contains(".load(p)"),
        "no name-matched loader call:\n{src}"
    );
    assert!(
        src.contains(".page(p, d)"),
        "namespaced loader page call:\n{src}"
    );
    // The obsolete co-naming dummies are gone.
    assert!(!src.contains("fn page() -> Int64"), "no page dummy:\n{src}");
    assert!(
        !src.contains("type Params = Int64"),
        "no Params dummy:\n{src}"
    );

    // RoutePath — the regex-validated string of the whole route language, with an
    // Int64 param as its integer-spelling regex.
    assert!(
        src.contains("export type RoutePath = String where value =~ \"(")
            && src.contains("/users/(0|-?[1-9][0-9]*)"),
        "RoutePath finite regex:\n{src}"
    );

    // Typed-URL helpers: one per dynamic route, one per static route.
    assert!(
        src.contains("export fn hrefUsers(id: Int64) -> RoutePath"),
        "dynamic helper:\n{src}"
    );
    assert!(
        src.contains("export fn hrefItems(id: Int64) -> RoutePath"),
        "dynamic helper:\n{src}"
    );
    assert!(
        src.contains("export fn itemsPath() -> RoutePath"),
        "static helper:\n{src}"
    );
    assert!(
        src.contains("export fn rootPath() -> RoutePath"),
        "root helper:\n{src}"
    );

    // The dynamic segment is validated against the declared type before user code.
    assert!(
        src.contains("fromJson(UiRouteInt, segs["),
        "dynamic segment parse:\n{src}"
    );
    // The loader's Invalid arm renders a 422 error page.
    assert!(src.contains("status: 422"), "error-page status:\n{src}");
    // The exported entry point.
    assert!(
        src.contains("export fn route(req: Request) -> Response"),
        "route entry:\n{src}"
    );

    // RFC-0074: the tree is mountable, and enumerated. One `//@route` per page on
    // the same channel `std/rpc` uses — so `vyrn routes` prints a page without
    // knowing it is one — and a `routes()` group `mount` takes, in dispatch order
    // so first-match agrees with `route`'s own static-before-dynamic order.
    for want in [
        "//@route GET / index convention",
        "//@route GET /items items convention",
        "//@route GET /items/{id} items/[id] convention",
        "//@route GET /users/{id} users/[id] convention",
        "GET(httpRoute(\"/\", uiPageRun, \"index\")),",
        "GET(httpRoute(\"/users/{id}\", uiPageRun, \"users/[id]\")),",
    ] {
        assert!(src.contains(want), "missing `{want}`:\n{src}");
    }
}

/// A page group obeys `mount`'s ordering rules, which is the whole reason to make
/// one: before this a page tree was a `fn(Request) -> Response` mounted by hand
/// after everything else, and an API route that swallowed a page path was
/// invisible until somebody loaded the page.
///
/// It can be checked at all because a page DECLINES — `routes()` is one route per
/// pattern, not one catch-all. Only the tree's 404 always answers, and that stays
/// the composition root's fallback.
#[test]
fn a_page_shadowed_by_an_earlier_group_is_a_startup_error() {
    let dir = scratch("pageshadow");
    write(
        &dir.join("pages/users/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { id: Int64 }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import * as site from pages(\"./pages\")\n\
         import { mount, surface } from \"std/http\"\n\
         fn under(req: Request) -> Option<Response> { return None }\n\
         fn main() -> Int64 {\n\
         \x20   let req = Request { method: \"GET\", path: \"/\", headers: [:], body: \"\" }\n\
         \x20   match mount(req, [[surface(\"/users\", under)], site.routes()], [], []) {\n\
         \x20       Some(r) => print(\"answered\"),\n\
         \x20       None => print(\"none\"),\n\
         \x20   }\n\
         \x20   return 0\n\
         }\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(!out.status.success(), "a shadowed page must trap");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("is unreachable"), "{err}");
    assert!(
        err.contains("GET /users/{id}"),
        "the shadowed page is named:\n{err}"
    );
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a Params/segment mismatch must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("PAGES_PARAM_MISMATCH"),
        "mismatch diagnostic:\n{err}"
    );
    assert!(err.contains("users"), "diagnostic names the file:\n{err}");
}

/// RFC-0033 (second producer): a page whose view takes the wrong type passes
/// generation-time inspection, but the check error in the synthesized router's
/// dispatch glue is reported against the PAGE module — proving origin maps
/// aren't `.vyx`-shaped.
///
/// The mismatch is between what `data` produces and what `page` accepts. A
/// contract member's type parameters are OPEN (RFC-0071 M1), so `fn page(d: T)
/// -> Html` admits any parameter type and the contract check cannot object —
/// which is exactly the class of error that has to survive into the generated
/// glue to be caught at all. (Before RFC-0072 M2 this test used a wrong RETURN
/// type; `Page` now names `page`, so that one is caught at the declaration.)
#[test]
fn page_type_error_remaps_to_the_page_module() {
    let dir = scratch("uiremap");
    write(
        &dir.join("pages/index.vyrn"),
        "import { el, Html } from \"std/html\"\n\
         import { query, Query } from \"std/ui\"\n\
         export type Data = { n: Int64 }\n\
         fn fetch() -> Data {\n    return Data { n: 1 }\n}\n\
         export fn data() -> Query<Data> {\n    return query(fetch)\n}\n\
         export fn page(d: String) -> Html {\n    return el(\"main\", [], [])\n}\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("check");
    assert!(
        !out.status.success(),
        "a wrong view parameter type must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    // Reported against the page module (region-level, line 1), not the router.
    assert!(
        err.contains("pages/index.vyrn:1:1:"),
        "remapped to the page file:\n{err}"
    );
    assert!(
        err.contains("note: in generated code"),
        "keeps the generated note:\n{err}"
    );
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "an unsupported param type must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("PAGES_UNSUPPORTED_PARAM_TYPE"),
        "unsupported-type diagnostic:\n{err}"
    );
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
         return Response { status: 200, contentType: \"text/plain; charset=utf-8\", body: \"raw:\" + p.id, vary: \"\", headers: [:] }\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path.copy(), headers: [:], body: \"\" }) }\n\
         fn main() -> Int64 {\n\
         let a = h(\"/p/deadbeef\")\n\
         print(\"P:\\{a.status}:\\{a.body.byteLength}\")\n\
         let b = h(\"/raw/cafe\")\n\
         print(\"R:\\{b.status}:\\{b.contentType}:\\{b.body}\")\n\
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
        "String-segment + respond app must run:\n{combined}"
    );
    // The String segment binds "deadbeef" and renders an HTML document (200).
    assert!(
        combined.contains("P:200:"),
        "String segment page renders 200:\n{combined}"
    );
    // The respond page owns the content type and body verbatim.
    assert!(
        combined.contains("R:200:text/plain; charset=utf-8:raw:cafe"),
        "respond raw bytes:\n{combined}"
    );
}

/// A `.vyx` page (RFC-0039 §4) routes through `pagesThemed`: its `params {}`
/// block binds the bracket segment, its `data` query runs, its template classes are
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
         import { ParamQuery, paramQuery } from \"std/ui\"\n\
         params { id: Int64 }\n\
         export fn data() -> ParamQuery<Params, Validation<Data>> {\n\
         return paramQuery(fetch)\n\
         }\n\
         fn fetch(p: Params) -> Validation<Data> {\n\
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
        "import { contains } from \"std/strpred\"\n\
         import { pagesThemed } from \"std/ui\"\n\
         import { route } from pagesThemed(\"./pages\", \"./theme.json\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path.copy(), headers: [:], body: \"\" }) }\n\
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), ".vyx pages app must run:\n{combined}");
    assert!(
        combined.contains("home:200"),
        "static .vyx page:\n{combined}"
    );
    assert!(
        combined.contains("book:200:true:true"),
        "loader .vyx page binds segment + Data:\n{combined}"
    );
    assert!(
        combined.contains("badid:404"),
        "non-integer Int64 segment 404s:\n{combined}"
    );
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(!out.status.success(), "a route collision must fail to load");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("PAGES_ROUTE_COLLISION"),
        "collision diagnostic:\n{err}"
    );
    // Names both offending files.
    assert!(
        err.contains("id") && err.contains("slug"),
        "diagnostic names both files:\n{err}"
    );
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
         import { ParamQuery, paramQuery } from \"std/ui\"\n\
         import { Params, Data } from \"../../shared\"\n\
         export fn data() -> ParamQuery<Params, Validation<Data>> {\n\
             return paramQuery(fetch)\n\
         }\n\
         fn fetch(p: Params) -> Validation<Data> {\n\
             return Valid(Data { label: \"user\\{p.id}\" })\n\
         }\n\
         export fn page(p: Params, d: Data) -> Html {\n\
             return el(\"main\", [], [text(d.label.copy())])\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn main() -> Int64 {\n\
             let r = route(Request { method: \"GET\", path: \"/users/7\", headers: [:], body: \"\" })\n\
             print(\"\\{r.status}\")\n\
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
        "imported-Params page must load and run:\n{combined}"
    );
    assert!(
        combined.contains("200"),
        "the dynamic route renders (200):\n{combined}"
    );

    // The synthesized router reaches the foreign `Params` through an aliased
    // import from its declaring module, not through the page namespace.
    let eg = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&eg.stdout);
    assert!(
        src.contains("import { Params as uiParams0 } from \"./shared\""),
        "foreign Params import:\n{src}"
    );
    assert!(
        src.contains("uiParams0 { "),
        "foreign Params construction:\n{src}"
    );
}

// ---- RFC-0041: layouts, head, error pages ----------------------------------

/// A `routes/layout.vyx` wraps every page body (its `<slot/>`), a page/layout
/// A layout's `head { … }` block and a page's `head(d)` member thread
/// `<link>`/`<script>`/dynamic `<title>` into the
/// document head, a `load -> Result<Data, PageError>` failure renders the nearest
/// `error.vyx` at the carried status, a `Validation` failure folds into a 422
/// error page, and `layout="none"` opts a page out of the shell.
#[test]
fn layout_head_and_error_pages_route_end_to_end() {
    let dir = scratch("layout");
    write(
        &dir.join("theme.json"),
        "{ \"safelist\": [\"shell\", \"home\", \"book\", \"err\", \"solo\"] }\n",
    );
    // The layout: the shell (with a <slot/>) plus a head block (stylesheet + boot).
    write(
        &dir.join("pages/layout.vyx"),
        "<script>\nhead {\n    stylesheet \"/style.css\"\n    module \"/nav.js\"\n}\n</script>\n\
         <template>\n<div class=\"shell\"><nav>bin</nav><main><slot/></main></div>\n</template>\n",
    );
    write(
        &dir.join("pages/index.vyx"),
        "<template>\n<h1 class=\"home\">Home</h1>\n</template>\n",
    );
    // A Result loader: Ok renders with a dynamic head title, Err → the error page.
    write(
        &dir.join("pages/p/[id].vyx"),
        "<script>\n\
         import { Head, PageError, ParamQuery, noHead, withTitle, paramQuery, notFound } from \"std/ui\"\n\
         params { id: String }\n\
         export fn head(d: Data) -> Head {\n    return withTitle(noHead(), d.name)\n}\n\
         export fn data() -> ParamQuery<Params, Result<Data, PageError>> {\n\
         return paramQuery(fetch)\n}\n\
         fn fetch(p: Params) -> Result<Data, PageError> {\n\
         if p.id == \"good\" {\n    return Ok(Data { name: \"Good One\" })\n}\n\
         return Err(notFound(\"no id \" + p.id))\n}\n\
         type Data = { name: String }\n\
         </script>\n\
         <template>\n<article class=\"book\"><h1>{{ data.name }}</h1></article>\n</template>\n",
    );
    // A Validation loader → 422 folded into a PageError.
    write(
        &dir.join("pages/v/[id].vyx"),
        "<script>\nimport { ParamQuery, paramQuery } from \"std/ui\"\n\
         params { id: Int64 }\n\
         export fn data() -> ParamQuery<Params, Validation<Data>> {\n\
         return paramQuery(fetch)\n}\n\
         fn fetch(p: Params) -> Validation<Data> {\n\
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
        "import { contains } from \"std/strpred\"\n\
         import { pagesThemed } from \"std/ui\"\n\
         import { route } from pagesThemed(\"./pages\", \"./theme.json\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path.copy(), headers: [:], body: \"\" }) }\n\
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
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "layout/error app must run:\n{combined}"
    );
    // Home: wrapped in the layout, the layout head stylesheet threaded.
    assert!(
        combined.contains("home:200:true:true"),
        "layout wrap + head:\n{combined}"
    );
    // Result Ok: dynamic <title> from the page head block, still under the layout.
    assert!(
        combined.contains("good:200:true:true"),
        "dynamic head title under layout:\n{combined}"
    );
    // Result Err: the themed error page at the carried 404, wrapped in the layout.
    assert!(
        combined.contains("bad:404:true:true:true"),
        "Result error page:\n{combined}"
    );
    // Validation Invalid: folded into a 422 error page.
    assert!(
        combined.contains("val:422:true:true"),
        "Validation 422 error page:\n{combined}"
    );
    // layout="none": no shell.
    assert!(
        combined.contains("solo:200:false:true"),
        "layout opt-out:\n{combined}"
    );
}

/// A `layout.vyx` without a `<slot/>` is a named generation diagnostic.
#[test]
fn a_layout_without_a_slot_is_a_diagnostic() {
    let dir = scratch("noslot");
    write(
        &dir.join("pages/layout.vyx"),
        "<template>\n<div>no slot</div>\n</template>\n",
    );
    write(
        &dir.join("pages/index.vyx"),
        "<template>\n<h1>home</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a slot-less layout must fail to load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("VYX_LAYOUT_NO_SLOT"),
        "no-slot diagnostic:\n{err}"
    );
}

// ---- RFC-0071: the `Page` contract's declaration forms ---------------------

/// The `head`/`data` members of `std/ui:Page`, written as the declarations they
/// now are, routed end to end. There is no second form to route beside them:
/// RFC-0071 M2c deleted the block and the name-match.
#[test]
fn the_page_contract_members_route_end_to_end() {
    let dir = scratch("contractforms");
    // A page on the new form: `head()` returns a `Head`, `data()` a `Query<T>`.
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Head, Query, noHead, withTitle, withStylesheet, query } from \"std/ui\"\n\
         export fn head() -> Head {\n\
         return withStylesheet(withTitle(noHead(), \"Home\"), \"/style.css\")\n}\n\
         export fn data() -> Query<Array<String>> {\n\
         return query(names)\n}\n\
         fn names() -> Array<String> {\n    return [\"a\", \"b\"]\n}\n\
         </script>\n\
         <template>\n<h1>{{ data.length }}</h1>\n</template>\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { contains } from \"std/strpred\"\n\
         import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn h(path: String) -> Response { return route(Request { method: \"GET\", path: path.copy(), headers: [:], body: \"\" }) }\n\
         fn main() -> Int64 {\n\
         let a = h(\"/\")\n\
         print(\"new:\\{a.status}:\\{a.body.contains(\"<title>Home</title>\")}:\\{a.body.contains(\"/style.css\")}:\\{a.body.contains(\"<h1>2</h1>\")}\")\n\
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
        "contract-form app must run:\n{combined}"
    );
    // The declared `head()` supplies both the title and the stylesheet, and the
    // declared `data()` reaches the view as the `data` prop.
    assert!(
        combined.contains("new:200:true:true:true"),
        "declaration forms:\n{combined}"
    );
}

/// THE acceptance criterion (RFC-0071): a misspelled member is an ERROR, where it
/// used to be a page that compiled clean and silently rendered with no data.
///
/// `laod` is the RFC's own example and it lands in the *not close* row —
/// Damerau-Levenshtein `laod`→`data` is 3, and `load` is no longer a member for
/// it to be one transposition from. It is still reported, which is the whole
/// point: a closed contract has no silent path.
#[test]
fn a_misspelled_page_export_is_an_error() {
    let dir = scratch("laod");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Query, query } from \"std/ui\"\n\
         export fn laod() -> Query<Array<String>> {\n\
         return query(one)\n}\n\
         fn one() -> Array<String> {\n    return [\"a\"]\n}\n\
         </script>\n\
         <template>\n<h1>home</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a misspelled member must fail the load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        err.contains("PAGES_CONTRACT"),
        "contract diagnostic:\n{err}"
    );
    assert!(
        err.contains("contract_unknown"),
        "unknown-export class:\n{err}"
    );
    assert!(err.contains("laod"), "names the offending export:\n{err}");
}

/// A near-miss within the did-you-mean threshold names the member it meant.
#[test]
fn a_near_miss_page_export_names_the_member_it_meant() {
    let dir = scratch("dta");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Query, query } from \"std/ui\"\n\
         export fn dta() -> Query<Array<String>> {\n\
         return query(one)\n}\n\
         fn one() -> Array<String> {\n    return [\"a\"]\n}\n\
         </script>\n\
         <template>\n<h1>home</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    assert!(
        !out.status.success(),
        "a near-miss member must fail the load"
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("didYouMean"), "did-you-mean class:\n{err}");
    assert!(err.contains("dta"), "names the offending export:\n{err}");
}

/// A private helper is outside the contract — a page needs local helpers, and the
/// closed rule applies to its PUBLIC surface only.
#[test]
fn a_private_page_helper_is_outside_the_contract() {
    let dir = scratch("privhelper");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         fn shown() -> String {\n    return \"home\"\n}\n\
         </script>\n\
         <template>\n<h1>{{ shown() }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a private helper must not trip the contract:\n{combined}"
    );
}

// ---- the demo runs green ---------------------------------------------------

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/pagesdemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(
        combined.contains("5 passed, 0 failed"),
        "expected 5 green tests:\n{combined}"
    );
}

// ===========================================================================
// RFC-0071 M2b — multi-shape `head`, laziness in the type, and params in the
// query. These are the two capabilities M2 shipped WITHOUT, and the reason
// `bin/routes/p/[id].vyx` could not migrate: its <title> is its loaded data's
// title, and its data depends on its route parameters.
// ===========================================================================

/// A page whose `head` reads the LOADED DATA — the shape the `head { … }` block
/// could express and a zero-argument `fn head()` could not.
#[test]
fn head_can_take_the_pages_loaded_data() {
    let dir = scratch("headdata");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Head, Query, noHead, withTitle, query } from \"std/ui\"\n\
         export fn head(d: String) -> Head {\n    return withTitle(noHead(), d)\n}\n\
         export fn data() -> Query<String> {\n    return query(title)\n}\n\
         fn title() -> String {\n    return \"from the data\"\n}\n\
         </script>\n\
         <template>\n<h1>{{ data }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "must generate:\n{err}");
    // The wrapper's own signature is the router's, unchanged; what varies is
    // what it FORWARDS to the accessor.
    assert!(
        src.contains("return headHtml(uiPgHead(d))"),
        "head is handed the data:\n{src}"
    );
    assert!(
        src.contains("return headTitleOf(uiPgHead(d))"),
        "and so is headTitle:\n{src}"
    );
}

/// A page whose `head` takes BOTH the params and the data — the fourth shape.
#[test]
fn head_can_take_params_and_data_together() {
    let dir = scratch("headboth");
    write(
        &dir.join("pages/u/[id].vyx"),
        "<script>\n\
         import { Head, ParamQuery, noHead, withTitle, paramQuery } from \"std/ui\"\n\
         params { id: Int64 }\n\
         export fn head(p: Params, d: Int64) -> Head {\n    return withTitle(noHead(), d.toString())\n}\n\
         export fn data() -> ParamQuery<Params, Int64> {\n    return paramQuery(twice)\n}\n\
         fn twice(p: Params) -> Int64 {\n    return p.id * 2\n}\n\
         </script>\n\
         <template>\n<h1>{{ data }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "must generate:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        src.contains("return headHtml(uiPgHead(p, d))"),
        "both are forwarded:\n{src}"
    );
}

/// A `head` asking for what the page cannot give is an error, not an empty head.
#[test]
fn a_head_asking_for_data_a_dataless_page_lacks_is_reported() {
    let dir = scratch("headnodata");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Head, noHead, withTitle } from \"std/ui\"\n\
         export fn head(d: String) -> Head {\n    return withTitle(noHead(), d)\n}\n\
         </script>\n\
         <template>\n<h1>x</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "a head with nothing to read must be refused"
    );
    assert!(
        err.contains("VYX_HEAD_SIGNATURE"),
        "naming the offense:\n{err}"
    );
}

/// Laziness is read off the RETURN TYPE, not out of `data`'s body — the last
/// source scan in the page pipeline, deleted rather than renamed.
#[test]
fn laziness_comes_from_the_declared_type() {
    let dir = scratch("lazytype");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Lazy, PageData, query, lazy } from \"std/ui\"\n\
         export fn data() -> Lazy<Int64> {\n    return lazy(query(seven))\n}\n\
         fn seven() -> Int64 {\n    return 7\n}\n\
         fn shown(d: PageData<Int64>) -> String {\n\
         return match d {\n        Loading => \"...\",\n        Ready(n) => n.toString(),\n    }\n}\n\
         </script>\n\
         <template>\n<h1>{{ shown(data) }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "must generate:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        src.contains("runLazy(uiPgData())"),
        "the lazy runner:\n{src}"
    );
    // A lazy page's view is over `PageData<T>` and the server renders `Ready(d)`.
    assert!(
        src.contains("Ready(d)"),
        "the view is wrapped for SSR:\n{src}"
    );
}

/// The same shape declared `Query` is NOT lazy: nothing is read from the body,
/// so the two differ only in the declaration.
#[test]
fn a_query_return_is_not_lazy_however_its_body_is_written() {
    let dir = scratch("lazytypeno");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Query, query } from \"std/ui\"\n\
         export fn data() -> Query<Int64> {\n    return query(seven)\n}\n\
         fn seven() -> Int64 {\n    return 7\n}\n\
         </script>\n\
         <template>\n<h1>{{ data }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "must generate:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        src.contains("runQuery(uiPgData())"),
        "the blocking runner:\n{src}"
    );
    assert!(!src.contains("runLazy"), "and not the lazy one:\n{src}");
    assert!(
        !src.contains("PageData<"),
        "the view is over the raw type:\n{src}"
    );
}

/// A `data` whose deferred call takes the page's own `Params` routes exactly as
/// a `fn load(p: Params)` did — which is what makes migrating one free.
#[test]
fn a_param_query_routes_like_a_params_loader() {
    let dir = scratch("paramquery");
    write(
        &dir.join("pages/u/[id].vyx"),
        "<script>\n\
         import { ParamQuery, paramQuery } from \"std/ui\"\n\
         params { id: Int64 }\n\
         export fn data() -> ParamQuery<Params, Int64> {\n    return paramQuery(twice)\n}\n\
         fn twice(p: Params) -> Int64 {\n    return p.id * 2\n}\n\
         </script>\n\
         <template>\n<h1>{{ data }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("emit-gen")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("emit-gen");
    let src = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "must generate:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        src.contains("export fn load(p: Params) -> Int64"),
        "the wrapper takes the params:\n{src}"
    );
    assert!(
        src.contains("runParamQuery(uiPgData(), p)"),
        "and hands them over:\n{src}"
    );
    assert!(
        src.contains(".load(p)"),
        "so the router calls it exactly as before:\n{src}"
    );
}

/// `data` returning something that is not one of the four query types is named,
/// not compiled into a call to a runner that does not exist.
#[test]
fn a_data_returning_a_non_query_is_reported() {
    let dir = scratch("baddata");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         export fn data() -> Int64 {\n    return 7\n}\n\
         </script>\n\
         <template>\n<h1>x</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "must be refused");
    assert!(
        err.contains("VYX_DATA_RETURN"),
        "naming the offense:\n{err}"
    );
}

// ---- the old page forms are gone (RFC-0071 M2c) ---------------------------

/// `head { … }` in a PAGE is no longer a form. The scanner that lifted it out of
/// the `<script>` ran only on the page path and is gone, so the block reaches the
/// compiled body as source and fails there — loudly, which is the point.
///
/// Layouts and error pages keep their block: `Page` is a contract about pages,
/// and a layout has none to be a member of.
#[test]
fn a_head_block_in_a_page_is_no_longer_a_form() {
    let dir = scratch("nohreadblock");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         head {\n    title: \"Old\"\n}\n\
         </script>\n\
         <template>\n<h1>hi</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "a page head block must be refused:\n{err}"
    );
}

/// A layout's `head { … }` still works — the half of the scanner that has a
/// reason to live. This is what stops the deletion above from being a regression.
#[test]
fn a_layout_head_block_still_works() {
    let dir = scratch("layouthead");
    write(
        &dir.join("pages/layout.vyx"),
        "<script>\n\
         head {\n    title: \"Shell\"\n    stylesheet \"/theme.css\"\n}\n\
         </script>\n\
         <template>\n<div><slot /></div>\n</template>\n",
    );
    write(
        &dir.join("pages/index.vyx"),
        "<template>\n<h1>home</h1>\n</template>\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { contains } from \"std/strpred\"\n\
         import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./pages\")\n\
         fn main() -> Int64 {\n\
         let r = route(Request { method: \"GET\", path: \"/\", headers: [:], body: \"\" })\n\
         print(\"lay:\\{r.status}:\\{r.body.contains(\"<title>Shell</title>\")}:\\{r.body.contains(\"/theme.css\")}\")\n\
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
        "layout head must still build:\n{combined}"
    );
    assert!(
        combined.contains("lay:200:true:true"),
        "layout head threads through:\n{combined}"
    );
}

/// `export fn load` in a `.vyx` page is now an unknown export against a CLOSED
/// contract — not a deprecated form, and not a silent no-data page either. The
/// name-match that used to find it is gone.
#[test]
fn an_exported_load_in_a_vyx_page_is_an_unknown_export() {
    let dir = scratch("loadgone");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         export fn load() -> Int64 {\n    return 7\n}\n\
         </script>\n\
         <template>\n<h1>hi</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "must be refused:\n{err}");
    assert!(
        err.contains("PAGES_CONTRACT"),
        "as a contract issue naming `load`:\n{err}"
    );
    assert!(err.contains("load"), "naming the export:\n{err}");
}

/// Nothing warns any more. The warning CHANNEL survives (M2b Part A, exercised by
/// `tests/warnings.rs`); its deprecation producer does not.
#[test]
fn a_page_on_the_declaration_forms_is_silent() {
    let dir = scratch("nodep");
    write(
        &dir.join("pages/index.vyx"),
        "<script>\n\
         import { Head, Query, noHead, withTitle, query } from \"std/ui\"\n\
         export fn head() -> Head {\n    return withTitle(noHead(), \"New\")\n}\n\
         export fn data() -> Query<Int64> {\n    return query(seven)\n}\n\
         fn seven() -> Int64 {\n    return 7\n}\n\
         </script>\n\
         <template>\n<h1>{{ data }}</h1>\n</template>\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    assert!(out.status.success(), "must build:\n{err}");
    assert!(!err.contains("warning:"), "nothing to say:\n{err}");
}

// ---- a URL slug is not an identifier ---------------------------------------
//
// `pages/about-us.vyrn` used to produce an export literally named
// `hrefAbout-us` / `about-usPath`, and the router then failed to parse at the
// hyphen (`expected LParen, found Minus`). The conversion is now deliberate:
// every run of non-alphanumeric bytes is a word break, each later word is
// capitalised, and a digit-leading name takes a `_`.

/// The oracle is that the generated router PARSES and that the helpers come out
/// under their converted names — the app imports each one by name, so a changed
/// mapping is a failed import, not a silent pass.
#[test]
fn awkward_slugs_convert_to_identifiers_and_the_router_parses() {
    let dir = scratch("slugs");
    let page = "import { el, Html } from \"std/html\"\n\
                export fn page() -> Html { return el(\"main\", [], []) }\n";
    for stem in ["about-us", "sign-in", "2fa", "a.b", "return"] {
        write(&dir.join(format!("pages/{stem}.vyrn")), page);
    }
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route, aboutUsPath, signInPath, _2faPath, aBPath, returnPath } from pages(\"./pages\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("check");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the generated router must parse and check:\n{err}"
    );
}

/// Slug-to-identifier is many-to-one, so two routes can reach one helper name.
/// That is a diagnostic (RFC-0099), not a silently duplicated export.
#[test]
fn two_slugs_reaching_one_helper_name_are_a_diagnostic() {
    let dir = scratch("slugclash");
    let page = "import { el, Html } from \"std/html\"\n\
                export fn page() -> Html { return el(\"main\", [], []) }\n";
    write(&dir.join("pages/about-us.vyrn"), page);
    write(&dir.join("pages/aboutUs.vyrn"), page);
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("check");
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "one identifier for two routes must fail:\n{err}"
    );
    assert!(
        err.contains("aboutUsPath"),
        "the diagnostic names the helper:\n{err}"
    );
    assert!(
        err.contains("/about-us") && err.contains("/aboutUs"),
        "the diagnostic names both routes:\n{err}"
    );
}

/// `std/ui`'s own unit tests — the slug-to-identifier mapping is decided there,
/// and nothing else in this file would run them.
#[test]
fn std_ui_unit_tests_run_green() {
    let module = repo_file("std/ui.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std/ui unit tests failed:\n{combined}"
    );
    assert!(
        combined.contains("9 passed, 0 failed"),
        "expected 9 green tests:\n{combined}"
    );
}
