//! `std/html` refusals: the two ways a view tree can be malformed rather than
//! merely ugly. Interpreter-only (no clang), so these run in the default suite.
//!
//! The attribute-name refusal is also `examples/htmlrefuse.vyrn`, so the parity
//! harness pins its wording on three backends; the cases here are the siblings
//! a trap cannot share a program with — a trap ends the run, so one refusal is
//! one program.
//!
//! Why a refusal and not an escape: a name lands OUTSIDE the quotes, where the
//! escaping that protects a value means nothing and a single space starts a
//! second attribute. `<img src" onload="alert(1)="x">` was real output.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-html-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run one program and return `(stdout, stderr, exit code)`.
fn run(name: &str, body: &str) -> (String, String, Option<i32>) {
    let dir = scratch(name);
    let src = dir.join("app.vyrn");
    let prog = format!(
        "import {{ el, text, attr, on, toHtmlString }} from \"std/html\"\n\n\
         fn main() -> Int64 {{\n{body}    return 0\n}}\n"
    );
    std::fs::write(&src, prog).unwrap();
    let out = vyrn().arg("run").arg(&src).output().expect("run");
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code(),
    )
}

#[test]
fn a_tag_name_carrying_markup_is_refused() {
    let (_, err, code) = run(
        "tag",
        "    print(toHtmlString(el(\"div onmouseover=alert(1)\", [], [text(\"hi\")])))\n",
    );
    assert_eq!(code, Some(1), "the refusal must end the run:\n{err}");
    assert!(
        err.contains("`div onmouseover=alert(1)` is not a usable tag name"),
        "the trap must quote the name it refused:\n{err}"
    );
}

#[test]
fn an_event_name_carrying_markup_is_refused() {
    let (_, err, code) = run(
        "event",
        "    print(toHtmlString(el(\"button\", [on(\"click\\\" onload=\\\"alert(1)\", \"h\", \"1\")], [])))\n",
    );
    assert_eq!(code, Some(1), "the refusal must end the run:\n{err}");
    assert!(
        err.contains("is not a usable event name"),
        "an event name forms part of the attribute name, so it is checked too:\n{err}"
    );
}

/// The empty name would have rendered `<>`, which is text, not an element.
#[test]
fn an_empty_name_is_refused() {
    let (_, err, code) = run("empty", "    print(toHtmlString(el(\"\", [], [])))\n");
    assert_eq!(code, Some(1), "the refusal must end the run:\n{err}");
    assert!(
        err.contains("`` is not a usable tag name"),
        "an empty tag name is refused like any other unusable one:\n{err}"
    );
}

/// The second defect: children of a void element used to vanish without a word.
#[test]
fn children_of_a_void_element_are_refused_not_dropped() {
    let (out, err, code) = run(
        "void",
        "    print(toHtmlString(el(\"p\", [], [text(\"before\")])))\n\
         \x20   print(toHtmlString(el(\"img\", [attr(\"src\", \"/a.png\")], [text(\"lost\")])))\n",
    );
    assert_eq!(
        out, "<p>before</p>\n",
        "the good node still renders:\n{out}"
    );
    assert_eq!(code, Some(1), "the refusal must end the run:\n{err}");
    assert!(
        err.contains("<img> is a void element and takes no children — 1 given"),
        "the trap must name the element and the count:\n{err}"
    );
}

/// The refusals must not cost a legitimate view anything: a custom element, a
/// namespaced attribute, an underscore, and a value full of markup all render.
#[test]
fn legitimate_names_and_hostile_values_still_render() {
    let (out, err, code) = run(
        "ok",
        "    print(toHtmlString(el(\"my-widget\", [attr(\"xml:lang\", \"en\"), attr(\"data_x\", \"1\"), on(\"click\", \"h\", \"7\")], [text(\"hi\")])))\n\
         \x20   print(toHtmlString(el(\"p\", [attr(\"title\", \"\\\" onload=\\\"alert(1)\")], [])))\n",
    );
    assert_eq!(code, Some(0), "no refusal was due here:\n{err}");
    assert_eq!(
        out,
        "<my-widget xml:lang=\"en\" data_x=\"1\" data-on-click=\"h\" data-arg-click=\"7\">hi</my-widget>\n\
         <p title=\"&quot; onload=&quot;alert(1)\"></p>\n"
    );
}
