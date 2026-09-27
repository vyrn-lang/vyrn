//! Tests what `fromJson` decodes by running the program. It is an integration
//! test because a unit test in this crate cannot reach the compiled route (see
//! `tests/loader_run.rs`); the decoder's source-reading tests stay in
//! `src/jsondec.rs`.

mod common;

use vyrn_frontend::loader::{LoadOptions, MapResolver};

/// Runs a single-source program with every runtime module the loader injects
/// for `fromJson` reachable, read from the shipped `std/` files.
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
            // The compiled route calls builtins whose bodies live here.
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
    let (program, memo) =
        vyrn_frontend::project::Memo::load(|| vyrn_frontend::load(src, "main.vyrn", &opts, &files))
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))?;
    common::run_compiled(&program, &memo)
}

/// `elemAt` answers `JNull` past the end and `JNull` is a legal `None`, so
/// without an arity check `{"P":[]}` decodes to a value the encoder never makes.
#[test]
fn a_tuple_payload_off_the_wire_arity_is_refused_even_all_option() {
    let src = "type E = | P(Option<Int64>, Option<Int64>) \
               fn issues(s: String) -> Int64 { \
                   return match fromJson<E>(s) { \
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
