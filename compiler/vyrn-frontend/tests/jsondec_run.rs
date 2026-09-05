//! What `fromJson` decodes, stated by RUNNING the program (RFC-0125 §3 M5, the
//! `library-run` row).
//!
//! The decoder's other tests read the source it generates and stay in
//! `src/jsondec.rs`. This one runs a program, and a program runs on the compiled
//! route now, which a unit test in this crate cannot reach — see
//! `tests/loader_run.rs` for why the dependency has to be an integration test's.

mod common;

use vyrn_frontend::loader::{LoadOptions, MapResolver};

/// Run a single-source program that uses `fromJson`, with every runtime
/// module the walk injects reachable — the same mapping the interpreter
/// tests use, so nothing here can drift from what ships.
fn run_json(src: &str) -> Result<i64, String> {
    let files = MapResolver(
        [
            (
                "std/json.vyrn".to_string(),
                include_str!("../../../std/json.vyrn").to_string(),
            ),
            (
                "std/codecs.vyrn".to_string(),
                include_str!("../../../std/codecs.vyrn").to_string(),
            ),
            (
                "std/text.vyrn".to_string(),
                include_str!("../../../std/text.vyrn").to_string(),
            ),
            (
                "std/strpred.vyrn".to_string(),
                include_str!("../../../std/strpred.vyrn").to_string(),
            ),
            (
                "std/jsondec.vyrn".to_string(),
                include_str!("../../../std/jsondec.vyrn").to_string(),
            ),
            (
                "std/jsonread.vyrn".to_string(),
                include_str!("../../../std/jsonread.vyrn").to_string(),
            ),
            (
                "std/num.vyrn".to_string(),
                include_str!("../../../std/num.vyrn").to_string(),
            ),
            (
                "std/hash.vyrn".to_string(),
                include_str!("../../../std/hash.vyrn").to_string(),
            ),
            // The compiled route needs these two and the interpreter did not: a
            // builtin the tree-walker answered in Rust is a call here, and
            // `std/runtime` is where the body is (RFC-0078).
            (
                "std/runtime.vyrn".to_string(),
                include_str!("../../../std/runtime.vyrn").to_string(),
            ),
            (
                "std/mem.vyrn".to_string(),
                include_str!("../../../std/mem.vyrn").to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let opts = LoadOptions {
        std_root: Some("std".into()),
        ..Default::default()
    };
    let program = vyrn_frontend::load(src, "main.vyrn", &opts, &files)
        .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
    common::run_compiled(&program)
}

/// A tuple payload whose members are ALL `Option` used to decode a short
/// wire array as all-`None`: `elemAt` answers `JNull` past the end and
/// `JNull` is a legal `None`, so `{"P":[]}` built a value the encoder can
/// never produce. The wire arity is now enforced up front: short AND long
/// arrays come back `Invalid` with an issue, while the exact arity still
/// decodes.
#[test]
fn a_tuple_payload_off_the_wire_arity_is_refused_even_all_option() {
    let src = "type E = | P(Option<Int64>, Option<Int64>) \
               fn issues(s: String) -> Int64 { \
                   return match fromJson(E, s) { \
                       Valid(_) => 0, \
                       Invalid(is) => is.length, \
                   }; } \
               fn main() -> Int64 { \
                   let ok = issues(\"{\\\"P\\\":[null,null]}\") \
                   if ok != 0 { return 0 - 1 } \
                   let short = issues(\"{\\\"P\\\":[]}\") \
                   let long = issues(\"{\\\"P\\\":[null,null,null]}\") \
                   if short == 0 { return 0 - 2 } \
                   if long == 0 { return 0 - 3 } \
                   return 1 }";
    assert_eq!(run_json(src).unwrap(), 1);
}
