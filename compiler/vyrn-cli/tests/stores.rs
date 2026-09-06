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

mod common;

use vyrn_frontend::ast::{Block, Stmt};

struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

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
    let program = vyrn_frontend::load(src, &root, &opts, &Fs)
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
    assert!(
        o.plan.store_fresh_at(assigns[0]),
        "the mention screen clears `out = tail(out)`"
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
