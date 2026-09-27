//! The `std/ui` `pages` generator driven through the `vyrn` binary.
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

/// A fresh scratch directory with an empty `pages/`.
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

    // Per-route namespaces let same-named exports across pages coexist.
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
    // A page's data is its declared `data` member, run through the runner its
    // return type names, never a name-matched loader.
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
    // No co-naming dummies.
    assert!(!src.contains("fn page() -> Int64"), "no page dummy:\n{src}");
    assert!(
        !src.contains("type Params = Int64"),
        "no Params dummy:\n{src}"
    );

    // RoutePath is a string validated by a regex of the whole route language; an
    // Int64 param is its integer-spelling regex.
    assert!(
        src.contains("export type RoutePath = String where value =~ \"(")
            && src.contains("/users/(0|-?[1-9][0-9]*)"),
        "RoutePath finite regex:\n{src}"
    );

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

    // A dynamic segment is validated against the declared type before user code.
    assert!(
        src.contains("fromJson<UiRouteInt>(segs["),
        "dynamic segment parse:\n{src}"
    );
    // The loader's Invalid arm renders a 422 error page.
    assert!(src.contains("status: 422"), "error-page status:\n{src}");
    assert!(
        src.contains("export fn route(req: Request) -> Response"),
        "route entry:\n{src}"
    );

    // The tree is mountable: one `//@route` per page on the channel
    // `std/rpc` uses, so `vyrn routes` prints pages too, and a `routes()` group in
    // dispatch order, so first-match agrees with `route`'s static-before-dynamic.
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

/// A page group obeys `mount`'s ordering rules, so an API route that swallows a
/// page path fails at startup. This is checkable because `routes()` is one route
/// per pattern, not a catch-all; only the tree's 404 always answers.
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
        err.contains("`Params` declares `slug`"),
        "mismatch diagnostic:\n{err}"
    );
    assert!(err.contains("users"), "diagnostic names the file:\n{err}");
}

/// A check error in the router's generated glue is reported against the page
/// module, so origin maps are not `.vyx`-specific.
///
/// A contract member's type parameters are open, so the contract check
/// admits `page(d: String)` against a `Query<Data>`; only the generated glue can
/// catch the mismatch.
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
    // `Int64`/`String` are supported; `Float64` is not.
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
        err.contains("which a URL segment cannot carry"),
        "unsupported-type diagnostic:\n{err}"
    );
    assert!(err.contains("tag"), "diagnostic names the file:\n{err}");
}

/// A `String` dynamic segment matches any non-empty segment without
/// `/`; a page that exports `respond` controls the content type and status.
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
    assert!(
        combined.contains("P:200:"),
        "String segment page renders 200:\n{combined}"
    );
    assert!(
        combined.contains("R:200:text/plain; charset=utf-8:raw:cafe"),
        "respond raw bytes:\n{combined}"
    );
}

/// `href -> route -> Params` is the identity for every value. Each awkward value
/// breaks a different way: a space and `/` are not segment bytes, `?` and `#` end
/// the path, `%` starts an escape, `+` is not a space here, an astral character is
/// four bytes, and an already-encoded value catches double encoding or decoding.
#[test]
fn a_string_segment_round_trips_through_the_url_boundary() {
    let dir = scratch("urlboundary");
    write(
        &dir.join("pages/t/[v].vyrn"),
        "export type Params = { v: String }\n\
         export fn respond(p: Params) -> Response {\n\
         return Response { status: 200, contentType: \"text/plain\", body: \"[\" + p.v + \"]\", vary: \"\", headers: [:] }\n\
         }\n",
    );
    write(
        &dir.join("app.vyrn"),
        "import { pages } from \"std/ui\"\n\
         import { route, hrefT } from pages(\"./pages\")\n\
         fn hitAt(path: String) -> Response { return route(Request { method: \"GET\", path: path.copy(), headers: [:], body: \"\" }) }\n\
         fn trip(label: String, v: String) {\n\
         let h = hrefT(v)\n\
         let r = hitAt(h.copy())\n\
         print(\"\\{label}|\\{h}|\\{r.status}|\\{r.body}\")\n\
         }\n\
         fn wire(label: String, path: String) { print(\"\\{label}|\\{hitAt(path).status}\") }\n\
         fn main() -> Int64 {\n\
         trip(\"space\", \"a b\")\n\
         trip(\"slash\", \"a/b\")\n\
         trip(\"question\", \"a?b\")\n\
         trip(\"hash\", \"a#b\")\n\
         trip(\"percent\", \"a%b\")\n\
         trip(\"plus\", \"a+b\")\n\
         trip(\"astral\", \"a\\u{1D11E}b\")\n\
         trip(\"encoded\", \"a%20b\")\n\
         trip(\"plain\", \"ab\")\n\
         wire(\"nul\", \"/t/a%00b\")\n\
         wire(\"badhex\", \"/t/a%zzb\")\n\
         wire(\"truncated\", \"/t/a%4\")\n\
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
    assert!(out.status.success(), "round-trip app must run:\n{combined}");

    // label | href | status | what user code saw. The href holds only unreserved
    // bytes and `%XX`, and the value comes back byte-identical.
    for want in [
        "space|/t/a%20b|200|[a b]",
        "slash|/t/a%2Fb|200|[a/b]",
        "question|/t/a%3Fb|200|[a?b]",
        "hash|/t/a%23b|200|[a#b]",
        "percent|/t/a%25b|200|[a%b]",
        "plus|/t/a%2Bb|200|[a+b]",
        "astral|/t/a%F0%9D%84%9Eb|200|[a\u{1D11E}b]",
        // The value's `%` is itself escaped, so the decoder returns the value.
        "encoded|/t/a%2520b|200|[a%20b]",
        // An already-legal value is untouched, so no existing href moves.
        "plain|/t/ab|200|[ab]",
        // A segment that is not valid percent-encoded text is a 404: user code
        // never sees wire bytes, and nothing aborts.
        "nul|404",
        "badhex|404",
        "truncated|404",
    ] {
        assert!(combined.contains(want), "missing `{want}`:\n{combined}");
    }
}

/// A `.vyx` page: `params {}` binds the bracket segment, `data` runs,
/// classes are theme-checked, and a non-integer `Int64` segment 404s.
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
        err.contains("both claim route"),
        "collision diagnostic:\n{err}"
    );
    assert!(
        err.contains("id") && err.contains("slug"),
        "diagnostic names both files:\n{err}"
    );
}

#[test]
fn imported_params_type_works_via_the_closure() {
    let dir = scratch("importedparams");
    // `Params` and `Data` live in a module the page imports. The reachable type
    // closure hands them to the generator, and the router imports
    // `Params` from its declaring module: a namespace reaches only a module's own
    // exports.
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

/// Layouts, head and error pages: a `layout.vyx` wraps every page at
/// its `<slot/>`, a layout's `head { .. }` and a page's `head(d)` fill the
/// document head, a `PageError` renders the nearest `error.vyx` at its status, a
/// `Validation` failure becomes a 422, and `layout="none"` opts out of the shell.
#[test]
fn layout_head_and_error_pages_route_end_to_end() {
    let dir = scratch("layout");
    write(
        &dir.join("theme.json"),
        "{ \"safelist\": [\"shell\", \"home\", \"book\", \"err\", \"solo\"] }\n",
    );
    write(
        &dir.join("pages/layout.vyx"),
        "<script>\nhead {\n    stylesheet \"/style.css\"\n    module \"/nav.js\"\n}\n</script>\n\
         <template>\n<div class=\"shell\"><nav>bin</nav><main><slot/></main></div>\n</template>\n",
    );
    write(
        &dir.join("pages/index.vyx"),
        "<template>\n<h1 class=\"home\">Home</h1>\n</template>\n",
    );
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
    // Reads the injected `error` prop.
    write(
        &dir.join("pages/error.vyx"),
        "<template>\n<section class=\"err\"><h1>Oops {{ error.status }}</h1><p>{{ error.message }}</p></section>\n</template>\n",
    );
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
    assert!(
        combined.contains("home:200:true:true"),
        "layout wrap + head:\n{combined}"
    );
    assert!(
        combined.contains("good:200:true:true"),
        "dynamic head title under layout:\n{combined}"
    );
    assert!(
        combined.contains("bad:404:true:true:true"),
        "Result error page:\n{combined}"
    );
    assert!(
        combined.contains("val:422:true:true"),
        "Validation 422 error page:\n{combined}"
    );
    assert!(
        combined.contains("solo:200:false:true"),
        "layout opt-out:\n{combined}"
    );
}

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
        err.contains("has no `<slot/>`"),
        "no-slot diagnostic:\n{err}"
    );
}

/// The `head` and `data` members of `std/ui:Page`, routed end to end.
#[test]
fn the_page_contract_members_route_end_to_end() {
    let dir = scratch("contractforms");
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
    // `head()` supplies the title and the stylesheet; `data()` reaches the view
    // as the `data` prop.
    assert!(
        combined.contains("new:200:true:true:true"),
        "declaration forms:\n{combined}"
    );
}

/// A closed contract has no silent path: a misspelled member would
/// otherwise render a page with no data. `laod` is 3 edits from `data`, too far
/// for a suggestion, and is still reported.
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
        err.contains("unknown export"),
        "contract diagnostic:\n{err}"
    );
    assert!(
        err.contains("contract `Page`"),
        "naming the contract it broke:\n{err}"
    );
    assert!(err.contains("laod"), "names the offending export:\n{err}");
}

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
    assert!(
        err.contains("did you mean `data`?"),
        "did-you-mean class:\n{err}"
    );
    assert!(err.contains("dta"), "names the offending export:\n{err}");
}

/// The closed rule applies to a page's public surface only.
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

/// A page's `head` can read its loaded data, as a title taken from the data does.
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
    // The wrapper's signature is fixed by the router; what it forwards varies.
    assert!(
        src.contains("return headHtml(uiPgHead(d))"),
        "head is handed the data:\n{src}"
    );
    assert!(
        src.contains("return headTitleOf(uiPgHead(d))"),
        "and so is headTitle:\n{src}"
    );
}

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
        err.contains("which is none of the signatures the router calls it with"),
        "naming the offense:\n{err}"
    );
}

/// Laziness is read off the return type, never scanned out of `data`'s body.
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
        src.contains("Ready(consume d)"),
        "the view is wrapped for SSR:\n{src}"
    );
}

/// The same body declared `Query` is not lazy: only the declaration differs.
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

/// A `ParamQuery` generates the same `load(p: Params)` wrapper the router calls.
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

/// Otherwise the router would call a runner that does not exist.
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
        err.contains("a page's `data` returns `Query<T>`"),
        "naming the offense:\n{err}"
    );
}

/// A page's `head { .. }` block reaches the compiled body as source and fails
/// there. Layouts and error pages keep the block: they belong to no contract.
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

/// The control for the test above.
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

/// `load` is not a member of the closed `Page` contract.
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
        err.contains("unknown export `load`"),
        "as a contract issue naming `load`:\n{err}"
    );
    assert!(err.contains("load"), "naming the export:\n{err}");
}

/// The warning channel itself is tested in `tests/warnings.rs`.
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

/// A URL slug is not an identifier: each run of non-alphanumeric bytes is a word
/// break, later words are capitalised, and a digit-leading name takes a `_`. The
/// app imports each helper by name, so a changed mapping fails the import.
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
