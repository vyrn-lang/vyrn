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
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap();
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
    write(dir, "shared/wire.vyrn", "export type Note = { id: Int64, text: String }\n");
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
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success(), "expected the widening import to fail the build");
    let err = String::from_utf8_lossy(&out.stderr);
    // BOTH ends of the edge, named as a reader of the project would type them.
    assert!(err.contains("`app/routes/index.vyrn` is universal"), "{err}");
    assert!(err.contains("cannot import `server/store.vyrn`, which is server-only"), "{err}");
    // The `vyrn.json` key that decided it — the answer to "says who?".
    assert!(err.contains("declared by vyrn.json:audience.server"), "{err}");
    // And what to do instead.
    assert!(err.contains("client(\"./server/api\")"), "{err}");
}

#[test]
fn the_same_project_without_an_audience_key_compiles() {
    let dir = scratch("optout");
    widening_project(&dir, MANIFEST_WITHOUT);
    let out = vyrn().arg("run").arg(dir.join("main.vyrn")).output().unwrap();
    // `main` returns the note's id, so the exit code IS the evidence it ran.
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_server_module_may_import_a_universal_one() {
    let dir = scratch("legal");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(&dir, "shared/wire.vyrn", "export fn seven() -> Int64 {\n    return 7\n}\n");
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
    let out = vyrn().arg("run").arg(dir.join("main.vyrn")).output().unwrap();
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_server_module_may_not_reach_a_client_one() {
    let dir = scratch("crosswise");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(&dir, "client/boot.vyrn", "export fn boot() -> Int64 {\n    return 1\n}\n");
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
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`server/store.vyrn` is server-only"), "{err}");
    assert!(err.contains("`client/boot.vyrn`, which is client-only"), "{err}");
}

#[test]
fn nearest_segment_wins_so_feature_outer_layouts_work() {
    let dir = scratch("feature");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(&dir, "src/notes/server/api/notes.vyrn", "export fn list() -> Int64 {\n    return 1\n}\n");
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
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success(), "a feature-outer layout must be checked the same way");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`src/notes/app/view.vyrn` is universal"), "{err}");
    assert!(err.contains("`src/notes/server/api/notes.vyrn`, which is server-only"), "{err}");
}

#[test]
fn a_page_generated_from_a_vyx_inherits_the_pages_own_audience() {
    // The generated server/client modules of a `.vyx` page have no path of their
    // own. Their audience is the page's, which is the only answer that could be
    // right — and it is what makes the rule reach a page written as a template.
    let dir = scratch("vyx");
    write(&dir, "vyrn.json", MANIFEST_WITH_AUDIENCE);
    write(&dir, "server/store.vyrn", "export fn secret() -> String {\n    return \"s\"\n}\n");
    write(
        &dir,
        "app/routes/index.vyx",
        "<template>\n  <div>{ secret() }</div>\n</template>\n\
         <script>\nimport { secret } from \"../../server/store\"\n</script>\n",
    );
    write(
        &dir,
        "main.vyrn",
        "import { pages } from \"std/ui\"\n\
         import { route } from pages(\"./app/routes\")\n\
         fn main() -> Int64 {\n    return 0\n}\n",
    );
    let out = vyrn().arg("check").arg(dir.join("main.vyrn")).output().unwrap();
    assert!(!out.status.success(), "a .vyx page must not reach a server module either");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("app/routes/index.vyx"), "{err}");
    assert!(err.contains("server/store.vyrn"), "{err}");
}

#[test]
fn why_prints_the_audience_the_deciding_segment_and_the_chains() {
    let dir = scratch("why");
    widening_project(&dir, MANIFEST_WITH_AUDIENCE);
    let out = vyrn().arg("why").arg(dir.join("server/store.vyrn")).output().unwrap();
    assert!(out.status.success(), "`why` reports; it does not gate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("audience: server-only"), "{text}");
    assert!(text.contains("path segment `server` (vyrn.json audience.server)"), "{text}");
    assert!(text.contains("main.vyrn -> app/routes/index.vyrn -> server/store.vyrn"), "{text}");
}

#[test]
fn why_says_so_when_the_project_declared_no_audience() {
    let dir = scratch("whynone");
    widening_project(&dir, MANIFEST_WITHOUT);
    let out = vyrn().arg("why").arg(dir.join("server/store.vyrn")).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("declares no `audience` in vyrn.json"), "{text}");
}
