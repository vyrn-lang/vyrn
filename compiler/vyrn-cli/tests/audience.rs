//! Integration tests for RFC-0072 M1 — audience, driven through the real `vyrn`
//! binary over real project trees.
//!
//! The claim the RFC makes is that "what runs where" stops being a bundler
//! convention and becomes a checker rule with a diagnostic, decided before
//! anything is built. So every test here writes a project, runs `vyrn check` or
//! `vyrn why`, and asserts on the text a user would see — not on an in-process
//! API that could agree with itself while the binary disagrees.
//!
//! The compatibility claim gets the same treatment: the SAME tree with the
//! `audience` key removed compiles clean.

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

/// A project in the RFC's audience-outer layout, whose page reaches straight
/// into the server module. The only variable across the tests below is whether
/// the manifest declares an `audience` key.
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
    // The `vyrn.json` key that decided it — the answer to "says who?".
    assert!(
        err.contains("declared by vyrn.json:audience.server"),
        "{err}"
    );
    // And what to do instead.
    assert!(err.contains("client(\"./server/api\")"), "{err}");
}

/// Absent and unreadable are different states, and the difference is the whole
/// boundary.
///
/// A `vyrn.json` that failed to parse was treated as no `vyrn.json` at all, so
/// every rule it carries evaporated with it. The one that matters is this one:
/// the mechanism that keeps server-only code out of a client bundle switched off
/// on a trailing comma, and the build printed `ok` and ran the program.
///
/// Asserted structurally — the exit codes and whether the program's output
/// escaped — because a downgrade produces no message to grep for.
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
    // it (RFC-0072 M5). The half that reaches a browser is the one the rule is
    // about: a component whose VIEW calls a server module is rejected when the
    // client root bundles it, naming both ends.
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
    // The other side of the same rule, and the reason M5's move is possible at
    // all: a page's `data()` runs on the server, `vyxPageClient` strips it out of
    // the client bundle, and the SSR module is compiled for the server root. A
    // loader reaching `server/api` is therefore not a widening import — it is
    // what server-side rendering IS.
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

/// A GENERATOR import is an import, and the rule decides it the same way.
///
/// A `.vyx` that lives under `server/` lends its module the server's audience
/// (RFC-0072 M1 — as landed), and mounting it from the client root is the widest
/// edge in the language: the generated module goes into the client bundle
/// carrying whatever the page reached for. The edge from the caller to the
/// generated module used to be checked by nothing at all, so `check` printed
/// `ok` and the client build printed the secret.
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

    // …and the tool a developer would ask agrees with the checker: the client
    // root reaches the page, through the generator call that mounts it.
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

/// The other half of the same rule, which the edge check must not break: ONE
/// universal page compiles to two modules that go to opposite sides of the wire
/// (RFC-0072 M5), and each is legal from the root that mounts it — the SSR half
/// reaching the server is what server-side rendering IS.
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
}

/// Audience is a property of a FILE. A second spelling of one path — a different
/// case on Windows — used to lose the audience with no diagnostic, and a file
/// with no audience is importable from anywhere.
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
