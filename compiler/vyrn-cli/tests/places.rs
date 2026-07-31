//! RFC-0082 M1: a container mutation reaches through a *place* — a record
//! field, an array element, a chain of them — and not only a plain variable.
//!
//! The behaviour is pinned three ways by `examples/slottable.vyrn`. What
//! behaviour cannot pin is the thing the whole milestone turns on: the desugar
//! must MOVE the container's header out of the field and back, not copy the
//! container. A copying lowering would be just as correct and would make every
//! write O(N), so no output and no assertion can tell them apart — only the
//! emitted code can. These tests are that half, in the shape of RFC-0081's
//! `the_json_writer_does_not_copy_once_per_element`: a structural count, not a
//! duration, so a loaded machine cannot make them flaky.

mod common;
use common::*;

/// The body of `fn <name>` in `src`'s emitted LLVM IR.
fn body_of(src: &str, name: &str) -> String {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    // Two tests here both emit `fn bump`, and cargo runs them concurrently, so
    // the file name has to be unique per call and not per function.
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = dir.join(format!("{name}-{nth}.vyrn"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn().arg("emit-ir").arg(&file).output().expect("vyrn emit-ir");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ir = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let start = ir
        .find(&format!("@vyrn_{name}("))
        .unwrap_or_else(|| panic!("no `vyrn_{name}` in the emitted IR:\n{ir}"));
    let start = ir[..start].rfind("\ndefine ").expect("no `define` before it") + 1;
    ir[start..start + ir[start..].find("\n}\n").expect("unterminated body")].to_string()
}

/// Every `call` in a body, minus the trap path — which is unreachable on the
/// hot path and is `stderr`/`fprintf`/`exit`, never an allocation.
fn allocating_calls(body: &str) -> Vec<&str> {
    body.lines()
        .filter(|l| l.contains("call ") || l.contains("call("))
        .filter(|l| {
            !["@__vyrn_stderr", "@fprintf", "@exit"]
                .iter()
                .any(|f| l.contains(f))
        })
        .collect()
}

/// `s.xs[i] = 9` must lower to: load the `{ptr,len,cap}` header out of the
/// record, bounds-check, store the element, put the header back. Nothing is
/// allocated and nothing is copied elementwise, which is what makes it O(1) per
/// write instead of O(N).
#[test]
fn an_index_assign_through_a_record_field_allocates_nothing() {
    let body = body_of(
        "type Store = { xs: Array<Int64>, n: Int64 }\n\
         fn bump(s: modify Store, i: Int64) {\n\
         s.xs[i] = 9\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s = Store { xs: [1, 2, 3], n: 0 }\n\
         bump(s, 0)\n\
         print(s.xs[0])\n\
         return 0\n\
         }\n",
        "bump",
    );
    let calls = allocating_calls(&body);
    assert!(
        calls.is_empty(),
        "an index assignment through a field must not allocate or copy — a \
         copying desugar would be correct and quadratic:\n{}\nin:\n{body}",
        calls.join("\n")
    );
    // The store itself is still there (an empty body would also allocate
    // nothing), and there is exactly one — not one per element.
    assert_eq!(
        body.matches("store i64 9, ptr").count(),
        1,
        "expected exactly one element store:\n{body}"
    );
}

/// The same for `pop`, which mutates AND returns, so it is hoisted around the
/// whole statement rather than desugared inside the expression.
#[test]
fn a_pop_through_a_record_field_allocates_nothing() {
    let body = body_of(
        "type Store = { xs: Array<Int64>, n: Int64 }\n\
         fn take(s: modify Store) -> Int64 {\n\
         let x = s.xs.pop()\n\
         return x ?? -1\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s = Store { xs: [1, 2, 3], n: 0 }\n\
         print(take(s))\n\
         print(s.xs.length)\n\
         return 0\n\
         }\n",
        "take",
    );
    let calls = allocating_calls(&body);
    assert!(
        calls.is_empty(),
        "`pop` through a field must shrink the header in place, not rebuild the \
         array:\n{}\nin:\n{body}",
        calls.join("\n")
    );
}

/// The nested case: `o.i.xs[k] = v` moves the outer field out first and back
/// last. Records are values, so the outer move is a fixed-size header copy —
/// still independent of the array's length, which is the property that matters.
#[test]
fn a_nested_field_chain_allocates_nothing_either() {
    let body = body_of(
        "type Inner = { xs: Array<Int64> }\n\
         type Outer = { i: Inner, n: Int64 }\n\
         fn bump(o: modify Outer, k: Int64) {\n\
         o.i.xs[k] = 9\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut o = Outer { i: Inner { xs: [1, 2] }, n: 0 }\n\
         bump(o, 1)\n\
         print(o.i.xs[1])\n\
         return 0\n\
         }\n",
        "bump",
    );
    let calls = allocating_calls(&body);
    assert!(calls.is_empty(), "{}\nin:\n{body}", calls.join("\n"));
}

/// The receiver forms that have no place to write back to keep failing, and say
/// which ones do work. A call result is not a place: there is nowhere to put the
/// container back, so a desugar cannot rescue it and silently dropping the write
/// would be the worst outcome.
#[test]
fn a_call_result_is_still_not_an_assignable_place() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nonplace.vyrn");
    std::fs::write(&file, "fn main() -> Int64 { f()[0] = 9  return 0 }\n").unwrap();
    let out = vyrn().arg("check").arg(&file).output().expect("vyrn check");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "expected a refusal, got:\n{err}");
    assert!(
        err.contains("an array variable, a record field, or an array element"),
        "the refusal should name the forms that DO work:\n{err}"
    );
}
