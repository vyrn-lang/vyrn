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

#[test]
fn unsupported_param_type_fails_naming_the_file() {
    let dir = scratch("badtype");
    // A `String` param is unsupported in v1 (Int64 only).
    write(
        &dir.join("pages/tag/[id].vyrn"),
        "import { el, text, Html } from \"std/html\"\n\
         export type Params = { id: String }\n\
         export fn page(p: Params) -> Html { return el(\"main\", [], []) }\n",
    );
    write(&dir.join("app.vyrn"), APP);
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "an unsupported param type must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("PAGES_UNSUPPORTED_PARAM_TYPE"), "unsupported-type diagnostic:\n{err}");
    assert!(err.contains("tag"), "diagnostic names the file:\n{err}");
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
