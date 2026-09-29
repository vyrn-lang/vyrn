//! The rule a store releases by, as the core states it.
//!
//! A store releases what its place holds, unless the value hands the place back
//! (`xs = xs.push(v)`) or the place owns no heap. At a name it releases while
//! the path has not made the name `Gone`; at a field or element, over the alias
//! table's root: module state, a `modify` parameter, or a root this frame still
//! holds. The corpus half is `coretables` and `residue`.

use vyrn_frontend::loader::DiskResolver;

mod common;

use vyrn_frontend::ast::{Block, NodeId, Stmt};

fn analyze(src: &str) -> vyrn_frontend::ast::Program {
    vyrn_lower::install();
    let dir = common::scratch("stores");
    let path = dir.join("m.vyrn");
    std::fs::write(&path, src).expect("write the source");
    let root = path.to_string_lossy().replace('\\', "/");
    let mut std_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std_root.pop();
    std_root.pop();
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(std_root.join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    let program = vyrn_lower::load(src, &root, &opts, &DiskResolver)
        .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let _lowered = vyrn_lower::lower(&program);
    vyrn_frontend::own::analyze(&program);
    program
}

fn releases(at: NodeId) -> bool {
    let facts = vyrn_lower::core::facts().expect("the placer fills the core's facts");
    facts.stores.get(&at).copied().unwrap_or(false)
}

/// Every statement of `b`, and of every block under it, that `want` accepts.
fn collect(b: &Block, want: &dyn Fn(&Stmt) -> bool, out: &mut Vec<NodeId>) {
    for s in &b.stmts {
        if want(s) {
            out.push(s.id());
        }
        match s {
            Stmt::While { body, .. } => collect(body, want, out),
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect(then_block, want, out);
                if let Some(eb) = else_block {
                    collect(eb, want, out);
                }
            }
            _ => {}
        }
    }
}

fn stores_in(b: &Block, want: impl Fn(&Stmt) -> bool) -> Vec<NodeId> {
    let mut out = Vec::new();
    collect(b, &want, &mut out);
    out
}

/// A field store into a local or module state owns what it displaces, and a
/// mention that only copies out of the target (`b.s + "!"`) does not stand it
/// down.
#[test]
fn a_place_store_owns_what_it_displaces() {
    let src = "type B = { s: String }\n\
               let mut g = B { s: \"m\" }\n\
               fn main() -> Int64 {\n\
                   let mut b = B { s: \"x\" + \"y\" }\n\
                   b.s = \"p\" + \"q\"\n\
                   g.s = \"r\" + \"t\"\n\
                   b.s = b.s + \"!\"\n\
                   return b.s.byteLength\n\
               }";
    let p = analyze(src);
    let sets = stores_in(&p.functions[0].body, |s| matches!(s, Stmt::SetField { .. }));
    assert_eq!(sets.len(), 3);
    assert!(releases(sets[0]), "a droppable local owns");
    assert!(releases(sets[1]), "module state owns by rule");
    assert!(
        releases(sets[2]),
        "a copying mention of the target does not stand the store down"
    );
}

/// A take sharing a loop with a store refuses the store unless the binding's
/// `let` is inside that loop: the back edge re-initializes it (`std/graphql`'s
/// alias path leaked one copy per alias).
#[test]
fn a_loop_local_store_owns_past_a_same_loop_take() {
    let src = "type P = { k: String, n: String }\n\
               fn tok(i: Int64) -> String { return \"t\" + \"x\" }\n\
               fn main() -> Int64 {\n\
                   let mut out: Array<P> = []\n\
                   let mut i = 0\n\
                   while i < 4 {\n\
                       let first = tok(i)\n\
                       let mut name = first.copy()\n\
                       if i > 1 {\n\
                           name = tok(i)\n\
                       }\n\
                       out.push(P { k: first, n: name })\n\
                       i = i + 1\n\
                   }\n\
                   return out.length\n\
               }";
    let p = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "name"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(assigns[0]),
        "the loop-local reassignment owns the copy it displaces"
    );
}

/// `tail` reads its parameter through `slice` but returns only copies, so the
/// mention screen (`store_fresh`) clears `out = tail(out)`.
#[test]
fn a_lender_read_consumed_by_a_copy_does_not_stand_the_store_down() {
    let src = "import { slice } from \"std/strpred\"\n\
               fn tail(s: String) -> String {\n\
                   let piece = slice(s, 0, 1) ?? panic(\"cut\")\n\
                   return \"\" + piece\n\
               }\n\
               fn go() -> String {\n\
                   let mut out = \"x\" + \"y\"\n\
                   let mut i = 0\n\
                   while i < 3 {\n\
                       out = tail(out)\n\
                       i = i + 1\n\
                   }\n\
                   return out\n\
               }\n\
               fn main() -> Int64 { return go().byteLength }";
    let p = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "out"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(assigns[0]),
        "a lender read consumed by a copy does not stand the store down"
    );
}

/// The take is on a path that left, so where the store runs the record is still
/// the frame's (`httpApply`'s 304 branch leaked one body per conditional GET).
#[test]
fn an_early_exiting_take_does_not_block_a_later_field_store() {
    let src = "type R = { body: String }\n\
               fn mk() -> R { return R { body: \"x\" + \"y\" } }\n\
               fn go(c: Bool) -> R {\n\
                   let mut answered = mk()\n\
                   if c {\n\
                       return answered\n\
                   }\n\
                   answered.body = \"\"\n\
                   return answered\n\
               }\n\
               fn main() -> Int64 { return go(true).body.byteLength }";
    let p = analyze(src);
    let sets = stores_in(&p.functions[1].body, |s| matches!(s, Stmt::SetField { .. }));
    assert_eq!(sets.len(), 1);
    assert!(
        releases(sets[0]),
        "the early exiting take does not block the later field store"
    );
}

/// `dec = halve2(dec)` mentions the place only as a read argument to a declared
/// non-lender, so the store releases what it replaces. A bare mention, a
/// builtin that hands its buffer back and a lender callee stand it down.
#[test]
fn a_read_call_mention_lets_the_store_release_what_it_replaces() {
    let src = "type D = { d: Array<Int64> }\n\
               fn halve2(x: D) -> D {\n\
                   let mut o: Array<Int64> = []\n\
                   let mut i = 0\n\
                   while i < x.d.length { o.push(x.d[i] / 2) i = i + 1 }\n\
                   return D { d: o }\n\
               }\n\
               fn go() -> Int64 {\n\
                   let mut dec = D { d: [8, 4] }\n\
                   let mut k = 0\n\
                   while k < 3 { dec = halve2(dec) k = k + 1 }\n\
                   return dec.d.length\n\
               }\n\
               fn main() -> Int64 { return go() }";
    let p = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "dec"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(assigns[0]),
        "exactly the `dec = halve2(dec)` store"
    );
}

/// A struct literal that reads a heap-free scalar or a screened projection of
/// the value it replaces still releases it (`std/regex`'s frag merges leaked one
/// holes-buffer per merge).
#[test]
fn a_struct_literal_store_with_scalar_mentions_releases_what_it_replaces() {
    let src = "type Frag = { start: Int64, holes: Array<Int64> }\n\
               fn joinH(a: Array<Int64>, b: Array<Int64>) -> Array<Int64> {\n\
                   let mut o: Array<Int64> = []\n\
                   for x in a { o.push(x) }\n\
                   for x in b { o.push(x) }\n\
                   return o\n\
               }\n\
               fn go() -> Int64 {\n\
                   let mut f = Frag { start: 0, holes: [1, 2] }\n\
                   let mut i = 0\n\
                   while i < 3 {\n\
                       f = Frag { start: f.start, holes: [i] }\n\
                       f = Frag { start: 9, holes: joinH(f.holes, [7]) }\n\
                       i = i + 1\n\
                   }\n\
                   return f.holes.length\n\
               }\n\
               fn main() -> Int64 { return go() }";
    let p = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "f"),
    );
    assert_eq!(assigns.len(), 2);
    assert!(
        assigns.iter().all(|at| releases(*at)),
        "both frag stores release what they replace"
    );
}

// `store_is_fresh`'s one remaining screen, the escape closure, is witnessed in
// `movecheck`'s `a_lambda_at_a_consume_parameter_escapes`; `blackBox`, the
// laundering shape, is refused outside a `bench` or `test` block. The lending
// and retention shapes are refused by the kernel (`refusals.rs`).
