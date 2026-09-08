//! RFC-0125 §3 M3, the store slice — the rule a store releases by.
//!
//! `own.rs` used to fold this out of `movecheck`'s event stream, twice: once
//! over the store events for a name, once over `place_stores` for a field or
//! an element. The core states it now, so these rules moved here with the
//! answer. Each one names the defect it was written for; the wording of each
//! claim is the wording it had in `own.rs`.
//!
//! A store releases what its place holds:
//!
//!   - unless the value HANDS THE PLACE BACK (`xs = xs.push(v)`), which the
//!     core reads off the statement;
//!   - unless the place owns no heap, which the core reads off the type;
//!   - at a name, exactly while the path has not made it `Gone`;
//!   - at a field or element, over the alias table's root: module state, a
//!     `modify` parameter, or a root this frame owns and still holds.
//!
//! The corpus half of the same rule is `coretables` (every store in every
//! example) and `residue` (what a wrong answer costs in blocks).

use vyrn_frontend::loader::DiskResolver;

mod common;

use vyrn_frontend::ast::{Block, Stmt};

/// Load one source string, run the analysis with the placer installed, and
/// hand back the program beside the core's answers.
fn analyze(src: &str) -> (vyrn_frontend::ast::Program, vyrn_frontend::own::Ownership) {
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
    let program = vyrn_frontend::load(src, &root, &opts, &DiskResolver)
        .unwrap_or_else(|d| panic!("{}", d.first().map(|d| d.render()).unwrap_or_default()));
    let _lowered = vyrn_lower::lower(&program);
    let own = vyrn_frontend::own::analyze(&program);
    (program, own)
}

/// Does the core release what this store displaces? Asked through the plan's
/// key, which is what both compiled backends ask through.
fn releases(own: &vyrn_frontend::own::Ownership, at: usize) -> bool {
    let facts = vyrn_lower::core::facts().expect("the placer fills the core's facts");
    facts
        .stores
        .get(&own.plan.key_of(at))
        .copied()
        .unwrap_or(false)
}

/// Every statement of `b`, and of every block under it, that `want` accepts.
fn collect(b: &Block, want: &dyn Fn(&Stmt) -> bool, out: &mut Vec<usize>) {
    for s in &b.stmts {
        if want(s) {
            out.push(s as *const Stmt as usize);
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

fn stores_in(b: &Block, want: impl Fn(&Stmt) -> bool) -> Vec<usize> {
    let mut out = Vec::new();
    collect(b, &want, &mut out);
    out
}

/// §26 steps 3–4: a field store into a binding this function releases owns
/// the value it displaces; the same store into module state owns by rule 4;
/// and round forty-six's third — a mention of the target that only COPIES out
/// of it (`b.s + "!"`) does not stand the store down.
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
    let (p, o) = analyze(src);
    let sets = stores_in(&p.functions[0].body, |s| matches!(s, Stmt::SetField { .. }));
    assert_eq!(sets.len(), 3);
    assert!(releases(&o, sets[0]), "a droppable local owns");
    assert!(releases(&o, sets[1]), "module state owns by rule");
    assert!(
        releases(&o, sets[2]),
        "a copying mention of the target does not stand the store down"
    );
}

/// Exit-residue round fifty-six, the loop-local pairing: a take sharing a
/// loop with a store refuses the store, UNLESS the binding's own `let` sits
/// inside that loop. Declared inside, the back edge re-initializes the
/// binding, so the conditional reassignment frees the copy it displaces
/// (`std/graphql`'s alias path leaked one per alias).
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
    let (p, o) = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "name"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(&o, assigns[0]),
        "the loop-local reassignment owns the copy it displaces"
    );
}

/// Round fifty-six, the escape screen: `tail` reads its parameter through
/// `slice` but returns only copies and concats, so `out = tail(out)` releases
/// what it displaces — the mention screen clears it (`store_fresh`) and the
/// judgment finds the name held.
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
    let (p, o) = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "out"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(&o, assigns[0]),
        "a lender read consumed by a copy does not stand the store down"
    );
}

/// Round fifty-seven: an early `return` of the binding does not block a later
/// field store into it. The take is on a path that LEFT, so where the store
/// runs the record is still the frame's — `httpApply`'s early `return
/// answered` blocked the 304 branch's `answered.body = ""` from freeing the
/// body it displaces, one representation per conditional GET.
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
    let (p, o) = analyze(src);
    let sets = stores_in(&p.functions[1].body, |s| matches!(s, Stmt::SetField { .. }));
    assert_eq!(sets.len(), 1);
    assert!(
        releases(&o, sets[0]),
        "the early exiting take does not block the later field store"
    );
}

/// Round eighteen, moved with the answer (RFC-0125 §3 M3, the fresh-store
/// slice): `dec = halve2(dec)` in a loop — the value mentions the place, but
/// only as a read argument to a declared non-lender, so the store releases
/// what it replaces. The bare mention (`x = x + ..` aside), a builtin (`a =
/// @push(a, i)` hands its own buffer back), and a lender callee all stand the
/// store down.
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
    let (p, o) = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "dec"),
    );
    assert_eq!(assigns.len(), 1);
    assert!(
        releases(&o, assigns[0]),
        "exactly the `dec = halve2(dec)` store"
    );
}

/// Round twenty-two, moved with the answer: the mention reading walks STRUCT
/// LITERALS and scalar projections — `f = Frag { start: f.start, holes: [h] }`
/// reads one heap-free scalar out of the value it replaces, and `holes:
/// joinH(f.holes, ..)` reads a projection through a screened callee. Both
/// stores release the old record's buffers (std/regex's frag merges leaked one
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
    let (p, o) = analyze(src);
    let assigns = stores_in(
        &p.functions[1].body,
        |s| matches!(s, Stmt::Assign { name, .. } if name == "f"),
    );
    assert_eq!(assigns.len(), 2);
    assert!(
        assigns.iter().all(|at| releases(&o, *at)),
        "both frag stores release what they replace"
    );
}

// The other two screens `store_is_fresh` reads keep their witnesses where the
// closures are computed (`movecheck`'s
// `a_lender_forwarded_through_an_aggregate_is_still_marked_lending` and
// `a_lambda_at_a_consume_parameter_escapes`). Neither has a store-side witness
// that reaches this file: a lender's own program is one the KERNEL refuses (a
// returned element), and `blackBox` — the launderer round nineteen was written
// for — is refused outside a `bench` or a `test` block. Both were unit tests of
// the closure, not of an emitted program, and the closure is still the
// checker's.
