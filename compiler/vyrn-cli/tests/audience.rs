//! The audience rule, driven through the real `vyrn` binary over real project trees.
//! "What runs where" is a checker rule with a diagnostic, decided before anything is
//! built, so each test writes a project, runs `vyrn check` or `vyrn why`, and asserts on
//! the text a user sees, not on an in-process API that could disagree with the binary.
//! The same tree without the `audience` key compiles clean.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c.env("VYRN_STD", repo_dir("std"));
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_audience_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const MANIFEST_WITH_AUDIENCE: &str = r#"{
  "name": "aud",
  "main": "main.vyrn",
  "audience": { "server": ["server"], "client": ["client"], "universal": ["app", "shared"] }
}
"#;

const MANIFEST_WITHOUT: &str = r#"{ "name": "aud", "main": "main.vyrn" }
"#;

/// A project in the RFC's audience-outer layout, whose page reaches straight into the
/// server module. The tests vary only whether the manifest declares an `audience` key.
fn widening_project(dir: &Path, manifest: &str) {
    write(dir, "vyrn.json", manifest);
    write(
        dir,
        "shared/wire.vyrn",
        "export type Note = { id: Int64, text: String }\n",
    );
    write(
        dir,
        "server/store.vyrn",
        "import { Note } from \"../shared/wire\"\n\
         export fn getNote() -> Note {\n    return Note { id: 7, text: \"secret\" }\n}\n",
    );
    write(
        dir,
        "app/routes/index.vyrn",
        "import * as store from \"../../server/store\"\n\
         export fn page() -> Int64 {\n    return store.getNote().id\n}\n",
    );
    write(
        dir,
        "main.vyrn",
        "import { page } from \"./app/routes/index\"\nfn main() -> Int64 {\n    return page()\n}\n",
    );
}

#[test]
fn a_universal_page_importing_a_server_module_is_an_error_naming_both_files() {
    let dir = scratch("widen");
    widening_project(&dir, MANIFEST_WITH_AUDIENCE);
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected the widening import to fail the build"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // BOTH ends of the edge, named as a reader of the project would type them.
    assert!(
        err.contains("`app/routes/index.vyrn` is universal"),
        "{err}"
    );
    assert!(
        err.contains("cannot import `server/store.vyrn`, which is server-only"),
        "{err}"
    );
    // The `vyrn.json` key that decided it: the answer to "says who?".
    assert!(
        err.contains("declared by vyrn.json:audience.server"),
        "{err}"
    );
    // And what to do instead: the concrete crossing, naming the module the edge reached
    // from the module that imports it.
    assert!(err.contains("connect(\"../../server/store\")"), "{err}");
    assert!(!err.contains("server/api"), "{err}");
}

/// Absent and unreadable are different states. A `vyrn.json` that fails to parse must
/// not be treated as absent, or a trailing comma switches off the rule that keeps
/// server-only code out of a client bundle.
///
/// Asserted on exit codes and on whether the program's output escaped, because a
/// downgrade produces no message to grep for.
#[test]
fn a_manifest_that_does_not_parse_never_downgrades_to_no_rules() {
    let dir = scratch("badmanifest");
    widening_project(&dir, MANIFEST_WITH_AUDIENCE);
    // The control: with the manifest readable, the widening import is refused.
    let ok = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert_eq!(
        ok.status.code(),
        Some(1),
        "the boundary holds when readable"
    );

    // One trailing comma, which is the whole attack.
    let broken = MANIFEST_WITH_AUDIENCE.replace("\"shared\"] }", "\"shared\"], }");
    assert_ne!(broken, MANIFEST_WITH_AUDIENCE, "the edit must land");
    write(&dir, "vyrn.json", &broken);

    for cmd in ["check", "run"] {
        let out = vyrn().arg(cmd).arg(dir.join("main.vyrn")).output().unwrap();
        assert_ne!(
            out.status.code(),
            Some(0),
            "`vyrn {cmd}` reported success with rules it could not read"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "`vyrn {cmd}` produced output from a program it should not have run"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("vyrn.json"),
            "names the file it could not read"
        );
    }
}

#[test]
fn the_same_project_without_an_audience_key_compiles() {
    let dir = scratch("optout");
    widening_project(&dir, MANIFEST_WITHOUT);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    // `main` returns the note's id, so the exit code IS the evidence it ran.
    assert_eq!(
        out.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_server_module_may_import_a_universal_one() {
    let dir = scratch("legal");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(
        &dir,
        "shared/wire.vyrn",
        "export fn seven() -> Int64 {\n    return 7\n}\n",
    );
    write(
        &dir,
        "server/store.vyrn",
        "import { seven } from \"../shared/wire\"\nexport fn go() -> Int64 {\n    return seven()\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { go } from \"./server/store\"\nfn main() -> Int64 {\n    return go()\n}\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_server_module_may_not_reach_a_client_one() {
    let dir = scratch("crosswise");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(
        &dir,
        "client/boot.vyrn",
        "export fn boot() -> Int64 {\n    return 1\n}\n",
    );
    write(
        &dir,
        "server/store.vyrn",
        "import { boot } from \"../client/boot\"\nexport fn go() -> Int64 {\n    return boot()\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { go } from \"./server/store\"\nfn main() -> Int64 {\n    return go()\n}\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`server/store.vyrn` is server-only"), "{err}");
    assert!(
        err.contains("`client/boot.vyrn`, which is client-only"),
        "{err}"
    );
    // The other direction's remedy names the module too: a file the reader
    // can open, not a `(shared/)` hint.
    assert!(
        err.contains("move the shared part of `client/boot.vyrn` into a universal module"),
        "{err}"
    );
}

#[test]
fn nearest_segment_wins_so_feature_outer_layouts_work() {
    let dir = scratch("feature");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(
        &dir,
        "src/notes/server/api/notes.vyrn",
        "export fn list() -> Int64 {\n    return 1\n}\n",
    );
    write(
        &dir,
        "src/notes/app/view.vyrn",
        "import { list } from \"../server/api/notes\"\nexport fn v() -> Int64 {\n    return list()\n}\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { v } from \"./src/notes/app/view\"\nfn main() -> Int64 {\n    return v()\n}\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a feature-outer layout must be checked the same way"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`src/notes/app/view.vyrn` is universal"),
        "{err}"
    );
    assert!(
        err.contains("`src/notes/server/api/notes.vyrn`, which is server-only"),
        "{err}"
    );
}

const MANIFEST_TWO_ROOTS: &str = r#"{
  "name": "aud",
  "server": "server.vyrn",
  "client": "client/boot.vyrn",
  "audience": { "server": ["server"], "client": ["client"], "universal": ["app", "shared"] }
}
"#;

#[test]
fn a_vyx_reaching_a_server_module_is_an_error_in_the_half_that_ships() {
    // A `.vyx` compiles to TWO modules on opposite sides of the wire, and neither
    // has a path of its own, so each takes the audience of the root that mounts
    // it. The rule is about the half that reaches a browser: a component
    // whose view calls a server module is refused when the client root bundles it,
    // naming both ends.
    let dir = scratch("vyx");
    write(&dir, "vyrn.json", MANIFEST_TWO_ROOTS);
    write(
        &dir,
        "server/store.vyrn",
        "export fn secret() -> String {\n    return \"s\"\n}\n",
    );
    write(
        &dir,
        "app/widgets/Leak.vyx",
        "<template>\n  <div>{{ secret() }}</div>\n</template>\n\
         <script>\nimport { secret } from \"../../server/store\"\n</script>\n",
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { components } from \"std/vyx\"\n\
         import { leak } from components(\"../app/widgets\")\n\
         import { toHtmlString } from \"std/html\"\n\
         export extern fn v() -> String {\n    return toHtmlString(leak())\n}\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("client/boot.vyrn"))
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a .vyx in the client bundle must not reach a server module"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("app/widgets/Leak.vyx"), "{err}");
    assert!(err.contains("`client/boot.vyrn` is client-only"), "{err}");
    assert!(
        err.contains("`server/store.vyrn`, which is server-only"),
        "{err}"
    );
}

#[test]
fn a_pages_ssr_half_may_load_from_the_server_that_mounts_it() {
    // The other side of the rule: a page's `data()` runs on the server,
    // `vyxPageClient` strips it from the client bundle, and the SSR module is compiled
    // for the server root, so a loader reaching `server/api` is not a widening import.
    let dir = scratch("ssr");
    write(&dir, "vyrn.json", MANIFEST_TWO_ROOTS);
    write(
        &dir,
        "server/api/notes.vyrn",
        "import { Note } from \"../../shared/wire\"\n\
         /// The one note.\nexport fn one() -> Note {\n    return Note { n: 7 }\n}\n",
    );
    write(
        &dir,
        "shared/wire.vyrn",
        "export type Note = { n: Int64 }\n",
    );
    write(
        &dir,
        "app/routes/index.vyx",
        "<script>\nimport { one } from \"../../server/api/notes\"\n\
         import { Note } from \"../../shared/wire\"\n\
         import { Query, query } from \"std/ui\"\n\
         export fn data() -> Query<Note> {\n    return query(one)\n}\n</script>\n\n\
         <template>\n<main><p>{{ data.n }}</p></main>\n</template>\n",
    );
    write(
        &dir,
        "server.vyrn",
        r#"import { pages } from "std/ui"
import { route } from pages("./app/routes")
fn main() -> Int64 {
    let r = route(Request { method: "GET", path: "/", headers: [:], body: "" })
    print("\{r.status}")
    return 0
}
"#,
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { pagesClient } from \"std/ui\"\n\
         import { renderPage } from pagesClient(\"../app/routes\")\n\
         export extern fn vyrnRenderPage(p: String) -> String {\n    return renderPage(p)\n}\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("server.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "200");
    // And the client bundle, which is where a leak would matter, still checks.
    let out = vyrn()
        .arg("check")
        .arg(dir.join("client/boot.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A generator import is an import, and the rule decides it the same way. A `.vyx` under
/// `server/` lends its module the server's audience, so mounting it from the
/// client root puts whatever the page reached for into the client bundle.
#[test]
fn a_server_page_mounted_by_the_client_root_is_refused() {
    let dir = scratch("genedge");
    write(&dir, "vyrn.json", MANIFEST_TWO_ROOTS);
    write(
        &dir,
        "server/store.vyrn",
        "export fn secret() -> String {\n    return \"TOP-SECRET\"\n}\n",
    );
    write(
        &dir,
        "server/pages/Leak.vyx",
        "<template>\n  <main><p>{{ secret() }}</p></main>\n</template>\n\
         <script>\nimport { secret } from \"../store\"\n</script>\n",
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { vyxPage } from \"std/vyx\"\n\
         import { page } from vyxPage(\"../server/pages/Leak.vyx\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 {\n    print(toHtmlString(page()))\n    return 0\n}\n",
    );
    let out = vyrn()
        .arg("run")
        .arg(dir.join("client/boot.vyrn"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the client build must not compile a server-only page: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("TOP-SECRET"),
        "the secret must never reach the output"
    );
    // The objection names both ends of the edge it is about.
    assert!(err.contains("`client/boot.vyrn` is client-only"), "{err}");
    assert!(
        err.contains("`server/pages/Leak.vyx`, which is server-only"),
        "{err}"
    );

    // ...and `vyrn why` agrees with the checker: the client root reaches the page
    // through the generator call that mounts it.
    let out = vyrn()
        .arg("why")
        .arg(dir.join("server/pages/Leak.vyx"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("audience: server-only"), "{text}");
    assert!(
        text.contains("client/boot.vyrn -> server/pages/Leak.vyx"),
        "`why` must not deny an edge the checker enforces:\n{text}"
    );
}

/// The other half of the rule, which the edge check must not break: one universal page
/// compiles to two modules on opposite sides of the wire, each legal from
/// the root that mounts it; the SSR half reaching the server is server-side rendering.
#[test]
fn both_halves_of_a_universal_page_mount_from_their_own_root() {
    let dir = scratch("halves");
    write(&dir, "vyrn.json", MANIFEST_TWO_ROOTS);
    write(
        &dir,
        "shared/wire.vyrn",
        "export type Note = { n: Int64 }\n",
    );
    write(
        &dir,
        "server/api/notes.vyrn",
        "import { Note } from \"../../shared/wire\"\n\
         export fn one() -> Note {\n    return Note { n: 7 }\n}\n",
    );
    write(
        &dir,
        "app/routes/index.vyx",
        "<script>\nimport { one } from \"../../server/api/notes\"\n\
         import { Note } from \"../../shared/wire\"\n\
         import { Query, query } from \"std/ui\"\n\
         export fn data() -> Query<Note> {\n    return query(one)\n}\n</script>\n\n\
         <template>\n<main><p>{{ data.n }}</p></main>\n</template>\n",
    );
    write(
        &dir,
        "server.vyrn",
        "import { vyxPage } from \"std/vyx\"\nimport { Note } from \"./shared/wire\"\n\
         import { page } from vyxPage(\"./app/routes/index.vyx\")\n\
         import { toHtmlString } from \"std/html\"\n\
         fn main() -> Int64 {\n    print(toHtmlString(page(Note { n: 7 })))\n    return 0\n}\n",
    );
    write(
        &dir,
        "client/boot.vyrn",
        "import { vyxPageClient } from \"std/vyx\"\nimport { Note } from \"../shared/wire\"\n\
         import { page } from vyxPageClient(\"../app/routes/index.vyx\")\n\
         import { toHtmlString } from \"std/html\"\n\
         export extern fn v() -> String {\n    return toHtmlString(page(Note { n: 7 }))\n}\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    for root in ["server.vyrn", "client/boot.vyrn"] {
        let out = vyrn().arg("check").arg(dir.join(root)).output().unwrap();
        assert!(
            out.status.success(),
            "{root} must still mount its own half: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Checking is not rendering: the test runs the SSR half and reads its HTML, so a
    // build that passes while the page renders nothing fails here.
    let out = vyrn()
        .arg("run")
        .arg(dir.join("server.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the SSR half must render: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = String::from_utf8_lossy(&out.stdout);
    assert!(
        html.contains("<main><p>7</p></main>"),
        "the server-rendered page carries the server's own value:\n{html}"
    );
}

/// Audience is a property of a file: a second spelling of one path (a different case on
/// Windows) keeps it. A file with no audience is importable from anywhere.
#[test]
#[cfg(windows)]
fn a_second_spelling_of_one_path_is_the_same_module() {
    let dir = scratch("spelling");
    write(&dir, "vyrn.json", MANIFEST_TWO_ROOTS);
    write(
        &dir,
        "server/store.vyrn",
        "export fn secret() -> Int64 {\n    return 7\n}\n",
    );
    for (name, spelling) in [
        ("as-written", "../server/store"),
        ("as-typed", "../Server/store"),
    ] {
        write(
            &dir,
            &format!("client/{name}.vyrn"),
            &format!("import {{ secret }} from \"{spelling}\"\nfn main() -> Int64 {{\n    return secret()\n}}\n"),
        );
        let out = vyrn()
            .arg("check")
            .arg(dir.join(format!("client/{name}.vyrn")))
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{name} was accepted: {err}");
        assert!(
            err.contains("`server/store.vyrn`, which is server-only"),
            "{name} names the file it really imported:\n{err}"
        );
    }

    // And the report reaches the same file by either spelling: a chain keyed on
    // the spelling would deny an edge the checker had just refused.
    let out = vyrn()
        .arg("why")
        .arg(dir.join("server/store.vyrn"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for name in ["as-written", "as-typed"] {
        assert!(
            text.contains(&format!("client/{name}.vyrn -> server/store.vyrn")),
            "`why` must reach the file by either spelling:\n{text}"
        );
    }
}

#[test]
fn why_prints_the_audience_the_deciding_segment_and_the_chains() {
    let dir = scratch("why");
    widening_project(&dir, MANIFEST_WITH_AUDIENCE);
    let out = vyrn()
        .arg("why")
        .arg(dir.join("server/store.vyrn"))
        .output()
        .unwrap();
    assert!(out.status.success(), "`why` reports; it does not gate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("audience: server-only"), "{text}");
    assert!(
        text.contains("path segment `server` (vyrn.json audience.server)"),
        "{text}"
    );
    assert!(
        text.contains("main.vyrn -> app/routes/index.vyrn -> server/store.vyrn"),
        "{text}"
    );
}

#[test]
fn why_says_so_when_the_project_declared_no_audience() {
    let dir = scratch("whynone");
    widening_project(&dir, MANIFEST_WITHOUT);
    let out = vyrn()
        .arg("why")
        .arg(dir.join("server/store.vyrn"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("declares no `audience` in vyrn.json"),
        "{text}"
    );
}

/// The fence the compiler declares (PLAN-0125-runtime): `std/mem` is in the audience of
/// `std/runtime` alone, and `std/runtime` is in nobody's, whatever `vyrn.json` says.
#[test]
fn a_user_import_of_std_mem_is_refused_with_or_without_a_declared_audience() {
    for (tag, manifest) in [
        ("fence", MANIFEST_WITH_AUDIENCE),
        ("fencenone", MANIFEST_WITHOUT),
    ] {
        let dir = scratch(tag);
        write(&dir, "vyrn.json", manifest);
        write(
            &dir,
            "main.vyrn",
            "import { load8 } from \"std/mem\"\nfn main() -> Int64 {\n    return Int64(load8(0))\n}\n",
        );
        let out = vyrn()
            .arg("check")
            .arg(dir.join("main.vyrn"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{tag}: the primitives leaked");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("cannot import `std/mem`, whose audience is the runtime"),
            "{tag}:\n{err}"
        );
        assert!(err.contains("declared by the compiler"), "{tag}:\n{err}");
    }
}

#[test]
fn a_user_import_of_std_runtime_is_refused_in_the_namespace_form_too() {
    let dir = scratch("fencert");
    write(&dir, "vyrn.json", MANIFEST_WITHOUT);
    write(
        &dir,
        "main.vyrn",
        "import * as rt from \"std/runtime\"\nfn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn()
        .arg("check")
        .arg(dir.join("main.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cannot import `std/runtime`, whose audience is the runtime"),
        "{err}"
    );
}

#[test]
fn why_names_the_compiler_as_the_declarer_of_std_mems_audience() {
    let out = vyrn()
        .arg("why")
        .arg(repo_dir("std").join("mem.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("audience: `std/runtime`, declared by the compiler"),
        "{text}"
    );
}

/// `vyrn why` answers "imported by" about a project that does not compile.
///
/// This is why `main.rs`'s `project_imports` exists beside `loader::module_graph`: that
/// walk is root-driven, runs every generator import, and returns `Err` at the first spec
/// it cannot resolve. `why` needs every file under the project directory, a generator's
/// input rather than its output, and an answer per file. Two projects witness it: one
/// whose target has a type error, one that imports a missing file.
#[test]
fn why_answers_imported_by_about_a_project_that_does_not_compile() {
    for (tag, bad, importer) in [
        (
            "whytypeerror",
            "export fn helper() -> Int64 {\n    return \"not an Int64\"\n}\n",
            "import { helper } from \"./bad\"\nfn main() -> Int64 {\n    return helper()\n}\n",
        ),
        (
            "whybadspec",
            "import { nope } from \"./nowhere\"\nexport fn helper() -> Int64 {\n    return 1\n}\n",
            "import { helper } from \"./bad\"\nfn main() -> Int64 {\n    return helper()\n}\n",
        ),
    ] {
        let dir = scratch(tag);
        write(&dir, "vyrn.json", MANIFEST_WITHOUT);
        write(&dir, "bad.vyrn", bad);
        write(&dir, "main.vyrn", importer);

        let out = vyrn()
            .arg("check")
            .arg(dir.join("main.vyrn"))
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{tag}: the project is supposed to be broken"
        );

        let out = vyrn()
            .arg("why")
            .arg(dir.join("bad.vyrn"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{tag}: `why` reports; it does not gate"
        );
        assert!(
            text.contains("main.vyrn -> bad.vyrn"),
            "{tag}: `why` lost the edge:\n{text}"
        );
    }
}
