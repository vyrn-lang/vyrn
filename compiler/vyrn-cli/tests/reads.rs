//! The World's read relation ([`vyrn_lower::World::readers`]) across an edit
//! to one declaration of a two-module program.

use std::sync::Arc;

use vyrn_frontend::ast::{DeclId, DeclKind, FnId, Key, Program, ScopeId};
use vyrn_frontend::loader::DiskResolver;
use vyrn_lower::World;

mod common;

const LIB: &str = "export fn twice(x: Int64) -> Int64 { return x * 2 }\n";

/// The root before the edit: `c` calls `half`, which no module declares.
const ROOT: &str = "
import { twice } from \"./lib.vyrn\"
fn a() -> Int64 { return twice(1) }
fn b() -> Int64 { return 3 }
fn c() -> Int64 { return half(4) }
fn main() -> Int64 {
    print(a() + b() + c())
    return 0
}
";

/// The edit: the root declares `half`.
const HALF: &str = "fn half(x: Int64) -> Int64 { return x / 2 }\n";

/// The World of the linked program. The load links and does not check, so
/// the root before the edit, which the checker refuses, has a World too.
fn world(root: &str) -> (Arc<World>, Program) {
    let dir = common::scratch("reads");
    std::fs::write(dir.join("lib.vyrn"), LIB).expect("write the module");
    let path = dir.join("main.vyrn");
    std::fs::write(&path, root).expect("write the root");
    let path = path.to_string_lossy().replace('\\', "/");
    let p = vyrn_frontend::loader::load(root, &path, &Default::default(), &DiskResolver, None)
        .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let w = vyrn_lower::analyze(&p);
    w.check();
    (w, p)
}

fn decl(p: &Program, name: &str) -> Key {
    let i = p.functions.iter().position(|f| f.name == name).unwrap();
    Key::Decl(DeclId::nth(DeclKind::Fn, i))
}

fn ids(w: &World, names: &[&str]) -> Vec<FnId> {
    names.iter().map(|n| w.fn_id(n).unwrap()).collect()
}

#[test]
fn a_declaration_in_another_module_is_read_by_its_callers_alone() {
    let (w, p) = world(&format!("{ROOT}{HALF}"));
    assert_eq!(w.readers(&decl(&p, "twice")), ids(&w, &["a"]));
}

#[test]
fn a_miss_is_a_read_before_the_edit_and_a_hit_after_it() {
    let (w, _) = world(ROOT);
    let miss = Key::Miss(ScopeId { module: None }, "half".to_string());
    assert_eq!(w.readers(&miss), ids(&w, &["c"]));
    let (w, p) = world(&format!("{ROOT}{HALF}"));
    assert_eq!(w.readers(&decl(&p, "half")), ids(&w, &["c"]));
}
