//! The World's call relation ([`vyrn_lower::World::callees`] and
//! [`vyrn_lower::World::callers`]) over small programs.

use std::sync::Arc;

use vyrn_frontend::ast::{FnId, Program};
use vyrn_lower::World;

fn world(src: &str) -> (Arc<World>, Program) {
    let mut p = vyrn_frontend::parser::parse(vyrn_frontend::lexer::lex(src).unwrap()).unwrap();
    p.expansions = vyrn_frontend::project::Expansions::shared();
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

/// [`World::state_path`] from `handle`, by row name, with the global it ends at.
fn state_path(src: &str) -> Option<(Vec<String>, Option<String>)> {
    let (w, _) = world(src);
    let (path, global) = w.state_path(w.fn_id("handle").unwrap())?;
    let names = path.iter().map(|f| w.fn_rows()[f.index()].name.clone());
    Some((names.collect(), global.map(str::to_string)))
}

fn path(names: &[&str], global: &str) -> Option<(Vec<String>, Option<String>)> {
    let names = names.iter().map(|n| n.to_string()).collect();
    Some((names, Some(global.to_string())))
}

#[test]
fn the_state_path_is_the_shortest_call_chain_to_the_global() {
    let src = "
let mut hits: Int64 = 0
fn bump() -> Int64 { hits = hits + 1
 return hits }
fn respond() -> Int64 { return bump() }
fn far() -> Int64 { return respond() }
fn handle(n: Int64) -> Int64 { return far() + respond() }
fn main() -> Int64 { return handle(1) }
";
    assert_eq!(
        state_path(src),
        path(&["handle", "respond", "bump"], "hits")
    );
}

#[test]
fn the_state_path_follows_a_function_handed_to_a_fn_parameter() {
    let src = "
let mut hits: Int64 = 0
fn bump(x: Int64) -> Int64 { hits = hits + x
 return hits }
fn apply(f: fn(Int64) -> Int64, x: Int64) -> Int64 { return f(x) }
fn handle(n: Int64) -> Int64 { return apply(bump, n) }
fn main() -> Int64 { return handle(1) }
";
    assert_eq!(state_path(src), path(&["handle", "apply", "bump"], "hits"));
}

#[test]
fn the_state_path_follows_a_stored_function_value() {
    let src = "
let mut hits: Int64 = 0
fn stateful(x: Int64) -> Int64 { return x + hits }
fn make() -> fn(Int64) -> Int64 { return stateful }
fn handle(n: Int64) -> Int64 { let m = make()
 return m(n) }
fn main() -> Int64 { return handle(1) }
";
    assert_eq!(state_path(src), path(&["handle", "stateful"], "hits"));
}

#[test]
fn the_state_path_takes_the_impl_a_method_call_dispatches_to() {
    let src = "
let mut hits: Int64 = 0
type P = { x: Int64 }
type Q = { y: Int64 }
protocol Pa { fn describe(self) -> Int64 }
protocol Qa { fn describe(self) -> Int64 }
impl Pa for P { fn describe(self) -> Int64 { return self.x } }
impl Qa for Q { fn describe(self) -> Int64 { return self.y + hits } }
fn handle(n: Int64) -> Int64 { let p = P { x: n }
 return p.describe() }
fn main() -> Int64 { let q = Q { y: 1 }
 return handle(q.describe()) }
";
    assert_eq!(state_path(src), None);
    let reaches_q = src.replace("P { x: n }", "Q { y: n }");
    assert_eq!(
        state_path(&reaches_q),
        path(&["handle", "Qa$Q$describe"], "hits")
    );
}

#[test]
fn output_and_file_io_reach_no_module_state() {
    let src = "
let mut hits: Int64 = 0
fn other() -> Int64 { hits = hits + 1
 return hits }
fn handle(n: Int64) -> Int64 { print(n)
 let r = readFile(\"x\")
 return n * 2 }
fn main() -> Int64 { return handle(other()) }
";
    assert_eq!(state_path(src), None);
}
