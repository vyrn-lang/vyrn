//! `vyrn doc` integration tests (RFC-0065): the byte-pinned golden output and
//! the `--verify` drift gate. Interpreter-agnostic (no clang), so these run in
//! the default suite.
//!
//! The fixture exercises every documented shape at once: a detached file header,
//! `fn`/`type`/`protocol` exports, a ` ```mermaid ` fence (passed through
//! verbatim), an UNCLOSED fence (the tool never eats content), a `///` block
//! detached from its declaration by a blank line (must NOT attach), a private
//! declaration (omitted), and a `test` block (omitted).

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-doc-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture module. lowerCamelCase, no semicolons, `///` markdown docs.
const FIXTURE: &str = r#"/// A tiny fixture module for `vyrn doc` golden output. It exercises the
/// file header, fenced diagrams, and the detached-block rule.

import { slice } from "std/strings"

/// Routes a request through the middleware chain.
///
/// ```mermaid
/// flowchart LR
///   req --> handler
/// ```
export fn route(req: Request) -> Response {
    return handle(req)
}

/// A rectangle the renderer knows how to draw.
///
/// ```text
/// unclosed fence on purpose
export type Shape = { width: Int64, height: Int64 }

/// Things that can render themselves to a `String`.
export protocol Show {
    fn show(self) -> String
}

/// This block is DETACHED from the declaration below by a blank line.

export fn area(s: Shape) -> Int64 {
    return s.width * s.height
}

/// A private helper — omitted from the docs (not exported).
fn handle(req: Request) -> Response {
    return Response { ok: true }
}

export type Request = { path: String }

export type Response = { ok: Bool }

test "area multiplies" {
    assertEq(area(Shape { width: 2, height: 3 }), 6)
}
"#;

/// The pinned page for the fixture module (a flush-left raw string so the
/// mermaid indentation survives verbatim). Note: `area` carries NO doc (its
/// `///` block was detached), `handle` is absent (private), and the unclosed
/// ` ```text ` fence is emitted verbatim.
const EXPECTED_PAGE: &str = r"# widgets

A tiny fixture module for `vyrn doc` golden output. It exercises the
file header, fenced diagrams, and the detached-block rule.

## route

```vyrn
fn route(req: Request) -> Response
```

Routes a request through the middleware chain.

```mermaid
flowchart LR
  req --> handler
```

## Shape

```vyrn
type Shape = { width: Int64, height: Int64 }
```

A rectangle the renderer knows how to draw.

```text
unclosed fence on purpose

## Show

```vyrn
protocol Show { fn show(self) -> String }
```

Things that can render themselves to a `String`.

## area

```vyrn
fn area(s: Shape) -> Int64
```

## Request

```vyrn
type Request = { path: String }
```

## Response

```vyrn
type Response = { ok: Bool }
```
";

const EXPECTED_INDEX: &str = r"# API Reference

- [widgets](widgets.md) — A tiny fixture module for `vyrn doc` golden output. It exercises the
";

/// Write the fixture into its own directory and document it (directory mode, so
/// no import resolution is needed — the `import` line is parsed and ignored).
fn generate(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("widgets.vyrn"), FIXTURE).unwrap();
    let out = dir.join("out");
    let status = vyrn()
        .arg("doc")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(
        status.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    out
}

#[test]
fn golden_module_page_is_byte_pinned() {
    let out = generate("golden-page");
    let page = std::fs::read_to_string(out.join("widgets.md")).unwrap();
    assert_eq!(page, EXPECTED_PAGE, "generated page drifted from the golden");
}

#[test]
fn golden_index_is_byte_pinned() {
    let out = generate("golden-index");
    let index = std::fs::read_to_string(out.join("index.md")).unwrap();
    assert_eq!(index, EXPECTED_INDEX, "generated index drifted from the golden");
}

#[test]
fn output_is_deterministic_across_two_runs() {
    let first = std::fs::read_to_string(generate("determinism-a").join("widgets.md")).unwrap();
    let second = std::fs::read_to_string(generate("determinism-b").join("widgets.md")).unwrap();
    assert_eq!(first, second, "two runs produced different bytes");
}

#[test]
fn verify_passes_on_freshly_generated_docs() {
    let dir = scratch("verify-clean");
    std::fs::write(dir.join("widgets.vyrn"), FIXTURE).unwrap();
    let out = dir.join("out");
    assert_eq!(
        vyrn().arg("doc").arg(&dir).arg("-o").arg(&out).output().unwrap().status.code(),
        Some(0)
    );
    // A --verify immediately after a generate must be a no-op success.
    let verified = vyrn()
        .arg("doc")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .arg("--verify")
        .output()
        .unwrap();
    assert_eq!(
        verified.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn verify_flags_out_of_date_docs() {
    let dir = scratch("verify-drift");
    std::fs::write(dir.join("widgets.vyrn"), FIXTURE).unwrap();
    let out = dir.join("out");
    vyrn().arg("doc").arg(&dir).arg("-o").arg(&out).output().unwrap();
    // Corrupt a generated page: --verify must exit 1.
    std::fs::write(out.join("widgets.md"), "stale\n").unwrap();
    let verified = vyrn()
        .arg("doc")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .arg("--verify")
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&verified.stderr).contains("drift"),
        "expected a drift message"
    );
}

#[test]
fn verify_flags_a_stale_extra_page() {
    let dir = scratch("verify-stale");
    std::fs::write(dir.join("widgets.vyrn"), FIXTURE).unwrap();
    let out = dir.join("out");
    vyrn().arg("doc").arg(&dir).arg("-o").arg(&out).output().unwrap();
    // A page no longer generated (e.g. a removed module) is drift too.
    std::fs::write(out.join("orphan.md"), "# gone\n").unwrap();
    let verified = vyrn()
        .arg("doc")
        .arg(&dir)
        .arg("-o")
        .arg(&out)
        .arg("--verify")
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(1));
}
