//! The World's call relation ([`vyrn_lower::World::callees`] and
//! [`vyrn_lower::World::callers`]) over small programs.

use std::sync::Arc;

use vyrn_frontend::ast::{FnId, Program};
use vyrn_lower::World;

fn world(src: &str) -> (Arc<World>, Program) {
    let (p, _memo) = vyrn_frontend::project::Memo::load(|| {
        vyrn_frontend::parser::parse(vyrn_frontend::lexer::lex(src).unwrap())
    })
    .unwrap();
    let w = vyrn_lower::analyze(&p);
    w.check();
    (w, p)
}

fn ids(w: &World, names: &[&str]) -> Vec<FnId> {
    names.iter().map(|n| w.fn_id(n).unwrap()).collect()
}

const CHAIN: &str = "
fn b(x: Int64) -> Int64 { return x + 1 }
fn a(x: Int64) -> Int64 { return b(x) * b(x) }
fn id<T>(x: T) -> T { return x }
fn main() -> Int64 {
    let y = a(1)
    let z = id(2)
    let t = id(true)
    if t { print(y + z) }
    return 0
}
";

#[test]
fn a_function_lists_its_callees_once_in_source_order() {
    let (w, _) = world(CHAIN);
    let main = w.fn_id("main").unwrap();
    assert_eq!(w.callees(main), ids(&w, &["a", "id"]));
    assert_eq!(w.callees(w.fn_id("a").unwrap()), ids(&w, &["b"]));
}

#[test]
fn every_instance_of_a_generic_calls_under_the_generic() {
    let (w, _) = world(CHAIN);
    assert_eq!(w.callers(w.fn_id("id").unwrap()), ids(&w, &["main"]));
}

#[test]
fn a_lambda_calls_under_the_function_that_holds_it() {
    let (w, _) = world(
        "
fn b(x: Int64) -> Int64 { return x + 1 }
fn apply(f: fn(Int64) -> Int64, x: Int64) -> Int64 { return f(x) }
fn main() -> Int64 { return apply(x -> b(x), 1) }
",
    );
    assert_eq!(w.callers(w.fn_id("b").unwrap()), ids(&w, &["main"]));
}
