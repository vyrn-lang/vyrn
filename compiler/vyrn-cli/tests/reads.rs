//! The World's read relation ([`vyrn_lower::World::readers`]) across an edit
//! to one declaration of a two-module program.

use std::sync::Arc;

use vyrn_frontend::ast::{DeclId, DeclKind, FnId, Key, Program, ScopeId};
use vyrn_frontend::loader::DiskResolver;
use vyrn_lower::World;

mod common;

const LIB: &str = "
export fn twice(x: Int64) -> Int64 { return x * 2 }
export type Point = { x: Int64 }
";

/// The edit to the module: `Point` gains a field.
const LIB_Y: &str = "
export fn twice(x: Int64) -> Int64 { return x * 2 }
export type Point = { x: Int64, y: Int64 }
";

/// The root before the edit: `c` calls `half`, which no module declares.
const ROOT: &str = "
import { twice, Point } from \"./lib.vyrn\"
fn a() -> Int64 { return twice(1) }
fn b() -> Int64 { return 3 }
fn c() -> Int64 { return half(4) }
fn d(p: Point) -> Int64 { return p.x }
fn main() -> Int64 {
    print(a() + b() + c())
    return 0
}
test \"a doubles\" { assertEq(a(), 2) }
";

/// The edit to the root: it declares `half`.
const HALF: &str = "fn half(x: Int64) -> Int64 { return x / 2 }\n";

/// The World of the linked program. The load links and does not check, so
/// the root before the edit, which the checker refuses, has a World too.
fn world(lib: &str, root: &str) -> (Arc<World>, Program) {
    vyrn_frontend::checker::record_reads();
    let dir = common::scratch("reads");
    std::fs::write(dir.join("lib.vyrn"), lib).expect("write the module");
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
    let (w, p) = world(LIB, &format!("{ROOT}{HALF}"));
    assert_eq!(w.readers(&decl(&p, "twice")), ids(&w, &["a"]));
}

#[test]
fn a_miss_is_a_read_before_the_edit_and_a_hit_after_it() {
    let (w, _) = world(LIB, ROOT);
    let miss = Key::Miss(ScopeId { module: None }, "half".to_string());
    assert_eq!(w.readers(&miss), ids(&w, &["c"]));
    let (w, p) = world(LIB, &format!("{ROOT}{HALF}"));
    assert_eq!(w.readers(&decl(&p, "half")), ids(&w, &["c"]));
}

#[test]
fn a_test_block_reads_the_function_it_calls() {
    let (w, p) = world(LIB, &format!("{ROOT}{HALF}"));
    assert_eq!(w.readers(&decl(&p, "a")), ids(&w, &["main", "test@0"]));
}

#[test]
fn a_type_declaration_is_read_by_the_bodies_that_name_it() {
    for lib in [LIB, LIB_Y] {
        let (w, p) = world(lib, &format!("{ROOT}{HALF}"));
        let i = p.type_decls.iter().position(|t| t.name == "Point").unwrap();
        let point = Key::Decl(DeclId::nth(DeclKind::Type, i));
        assert_eq!(w.readers(&point), ids(&w, &["d"]));
    }
}
