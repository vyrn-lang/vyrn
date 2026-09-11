//! The release PLAN over one body: which bindings a frame reclaims, and with
//! what — RFC-0125 §3 M3, the container slice.
//!
//! These assertions were `own.rs`'s unit tests. They asked a table that pass
//! built for itself (`Ownership::droppable`), and that table is deleted: the
//! emitters read the core's placed rows, and the rows are the placer's. So
//! the same questions are asked here, one milestone down the pipeline, where
//! a lowering can be installed and the placer can run.
//!
//! Each helper is the old one with its source swapped: `drop_count` counts
//! the DISTINCT bindings `Ownership::releases` names for a function,
//! `drop_kinds` reads the kind off each row, `kepts` lines the rows up
//! against the `let`s in source order, and `reclaims` asks about one name.

use std::collections::HashMap;

use vyrn_frontend::ast::{Block, EnumVariant, Field, Program, Stmt, Type};
use vyrn_frontend::own::{analyze, DropKind, Ownership};

/// Parse, then analyse with the placer installed — the rows are the core's.
fn analyze_src(src: &str) -> (Ownership, Program) {
    vyrn_lower::install();
    let p = vyrn_frontend::parser::parse(vyrn_frontend::lexer::lex(src).unwrap()).unwrap();
    let _memo = vyrn_frontend::project::Memo::open();
    let o = analyze(&p);
    (o, p)
}

/// The kind each binding of `which` is released with, by binding key. A
/// binding released at several exits carries one kind, so the map is the
/// per-binding view of the per-exit rows.
fn placed(o: &Ownership, which: &str) -> HashMap<usize, DropKind> {
    o.releases
        .get(which)
        .map(|rows| rows.iter().map(|r| (r.binding, r.kind.clone())).collect())
        .unwrap_or_default()
}

/// How many bindings in function `which` the frame reclaims.
fn drop_count(src: &str, which: &str) -> usize {
    let (o, _) = analyze_src(src);
    placed(&o, which).len()
}

fn drop_kinds(src: &str, which: &str) -> Vec<DropKind> {
    let (o, _) = analyze_src(src);
    placed(&o, which).into_values().collect()
}

/// What the plan decided for every `let` in `which`, in source order: the
/// release the frame reclaims with, or `None` where the frame reclaims
/// nothing.
fn kepts(src: &str, which: &str) -> Vec<Option<DropKind>> {
    let (o, p) = analyze_src(src);
    let f = p.functions.iter().find(|f| f.name == which).unwrap();
    let d = placed(&o, which);
    let mut lets = Vec::new();
    let_stmts(&f.body, &mut lets);
    lets.iter()
        .map(|s| {
            let k = *s as *const Stmt as usize;
            d.get(&k).cloned()
        })
        .collect()
}

/// `Option<T>`'s release row, as `release_kind` records it.
fn opt_row(t: Type) -> DropKind {
    DropKind::Deep(Type::Enum(vec![
        EnumVariant {
            name: "None".to_string(),
            payload: Vec::new(),
        },
        EnumVariant {
            name: "Some".to_string(),
            payload: vec![t],
        },
    ]))
}

/// Whether this frame reclaims the binding named `name` at all.
fn reclaims(src: &str, name: &str) -> bool {
    let (o, p) = analyze_src(src);
    let f = p.functions.iter().find(|f| f.name == "main").unwrap();
    let mut lets = Vec::new();
    let_stmts(&f.body, &mut lets);
    let s = lets
        .iter()
        .find(|s| matches!(s, Stmt::Let { name: n, .. } if n == name))
        .unwrap();
    let k = *s as *const Stmt as usize;
    placed(&o, "main").contains_key(&k)
}

// ---- RFC-0093 M2: a take leaves a hole, and the walk skips it --------

/// Every `let` in `body`, in source order, nested blocks included.
fn let_stmts<'a>(body: &'a Block, out: &mut Vec<&'a Stmt>) {
    for s in &body.stmts {
        if matches!(s, Stmt::Let { .. }) {
            out.push(s);
        }
        match s {
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => {
                let_stmts(then_block, out);
                if let Some(e) = else_block {
                    let_stmts(e, out);
                }
            }
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                let_stmts(body, out)
            }
            _ => {}
        }
    }
}

#[test]
fn frees_non_escaping_temporary() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let s = a + b; let n = s.byteLength; return n; }";
    assert_eq!(drop_count(src, "main"), 1);
}

/// Round fifty-seven: `Some(x)` types as `Option<type_of(x)>`, so the
/// binding carries a release row — `let root = Some(insert(..))` typed
/// as bare `Option` and its payload box had no owner (tree's last block).
#[test]
fn a_some_binding_types_as_the_option_it_is() {
    let src = "fn mk() -> String { return \"a\" + \"b\" }\n\
               fn main() -> Int64 {\n\
                   let o = Some(mk())\n\
                   return 0\n\
               }";
    let fs = kepts(src, "main");
    assert_eq!(fs.len(), 1);
    assert!(fs[0].is_some(), "`o` is reclaimed, not {:?}", fs[0]);
}

/// Round fifty-seven: a lambda's capture is a deep snapshot (both
/// compiling backends duplicate heap captures into the block), so the
/// captured binding still reclaims at block exit.
#[test]
fn a_lambda_capture_reclaims() {
    let src = "type Sink = fn(Int64)\n\
               let mut pending: Array<Sink> = []\n\
               fn keep(cb: consume Sink) {\n\
                   pending.push(cb)\n\
               }\n\
               fn main() -> Int64 {\n\
                   let tag = \"t\" + \"g\"\n\
                   keep(n -> print(\"\\{tag}:\\{n}\"))\n\
                   return 0\n\
               }";
    let fs = kepts(src, "main");
    assert_eq!(fs.len(), 1, "one binding: tag");
    assert!(
        fs[0].is_some(),
        "a lambda-captured binding reclaims, not {:?}",
        fs[0]
    );
}

/// RFC-0089 rule 1, Phase 4c. `let t = s` MOVES: the new name owns the
/// buffer and the old one no longer does, so the block still frees it once.
/// Before the rules were enforced this pass could not tell an alias from a
/// move and left both to leak.
#[test]
fn an_alias_moves_the_owner_rather_than_duplicating_it() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let s = a + b; let t = s; return t.byteLength; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

#[test]
fn concat_argument_is_a_safe_read() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let s = a + b; let u = s + b; return u.byteLength; }";
    assert_eq!(drop_count(src, "main"), 2);
}

#[test]
fn a_value_stored_into_an_outer_container_escapes() {
    // `xs.push(s)` moves `s` into a container that outlives the inner block,
    // so `s` must NOT stay droppable — freeing it would leave the array
    // holding a dangling buffer, and the array releases the element itself
    // since RFC-0092 M2. Only the array is released here.
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let mut xs: Array<String> = []; \
               if true { let s = a + b; xs.push(s); } \
               return xs.length; }";
    assert_eq!(
        drop_kinds(src, "main"),
        vec![DropKind::Deep(Type::Array(Box::new(Type::Str)))]
    );
}

/// A `region` is not asked here. This walk used to skip a `String` bound
/// inside one, on the argument that the arena owned it — which claimed for
/// the arena every block the frame minted at that depth, a callee's
/// included. The ownership test is the block header and `free` states it
/// once: an arena block carries a class word of 0 and is refused in
/// silence. So the binding is droppable like any other and the arena keeps
/// the ones that are its (RFC-0125 §3 M4, the region triage).
#[test]
fn a_binding_inside_a_region_is_droppable_like_any_other() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; let mut n = 0; \
               region { let s = a + b; n = s.byteLength } return n; }";
    assert_eq!(drop_count(src, "main"), 1);
}

/// Census §2c, closed in Phase 4c. `mut` used to mean "who owns the old value
/// after a reassignment is unclear", so a `mut` String was left to leak. Rule
/// 1 governs reassignment now, so the binding owns whatever it holds last and
/// the block frees that.
#[test]
fn a_mutable_string_is_reclaimed() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let mut s = a + b; return s.byteLength; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

#[test]
fn an_inner_let_shadowing_a_string_is_not_a_string() {
    // `s + 1` under `let s = 1` is an integer add. Reading the outer `s`
    // here made `t` droppable, and the backend freed the integer 2.
    let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
               if true { let s = 1; let t = s + 1; print(t); } return 0; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_loop_binder_shadowing_a_string_is_not_a_string() {
    let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
               let ns: Array<Int64> = []; \
               for s in ns { let t = s + 1; print(t); } return 0; }";
    // The array, and only the array: `s` is a literal, and `t` is the
    // integer add this test is about. A String `t` here freed an integer.
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn a_pattern_binder_shadowing_a_string_is_not_a_string() {
    let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
               let o: Option<Int64> = Some(1); \
               if let Some(s) = o { let t = s + 1; print(t); } return 0; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_lambda_parameter_shadowing_a_string_is_not_a_string() {
    let src = "fn apply(f: fn(Int64) -> Int64, x: Int64) -> Int64 { return f(x); } \
               fn main() -> Int64 { let s = \"x\"; print(s); \
               return apply(s -> { let t = s + 1; print(t); return t + 1; }, 2); }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_shadowed_string_is_still_freed() {
    // The mirror of the four above. Over-correcting into "an inner binding
    // is never a string" would turn the miscompile into a leak: both
    // concatenations are fresh Strings and both must be reclaimed.
    let src = "fn main() -> Int64 { let a = \"x\"; \
               let s = a + \"y\"; print(s); \
               if true { let s = a + \"z\"; print(s); } return 0; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
}

#[test]
fn an_inner_let_shadowing_a_non_string_is_a_string() {
    // The other direction of the same lookup: the innermost binding answers,
    // so an inner String under an outer integer concatenates.
    let src = "fn main() -> Int64 { let s = 1; print(s); \
               if true { let s = \"a\"; let t = s + \"b\"; print(t); } return 0; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

/// RFC-0089 M1b. `copy` is a producer, so the copy is the caller's to
/// release — and the receiver stays a live owner, so both are freed once.
#[test]
fn copy_transfers_and_leaves_the_receiver_owned() {
    let src = "fn main() -> Int64 { let a = \"x\" + \"y\"; let b = a.copy(); \
               print(a); print(b); return 0; }";
    assert_eq!(drop_count(src, "main"), 2);
}

/// A `read` parameter is a promise that the caller keeps the value (rule 2),
/// so passing a local to one takes nothing: the caller still frees its own
/// String, and the callee's result is a second one it owns. Two frees, two
/// buffers. Before Phase 4c every call was treated as a possible retention
/// and both leaked.
#[test]
fn passing_to_a_read_parameter_keeps_the_caller_the_owner() {
    let src = "fn tail(s: String) -> String { return s + \"!\"; } \
               fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let y = tail(s); return y.byteLength; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
}

/// The other half of one rule. An arm that hands its payload out gives the
/// SCRUTINEE up, so the binding the payload flowed into is the only owner
/// there is — releasing both is the double free that aborted every native
/// build of `let s = match o { Some(v) => v, None => "" }`.
#[test]
fn a_payload_that_leaves_its_arm_leaves_the_scrutinee_unreclaimed() {
    let src = "fn maybe(n: Int64) -> Option<String> { return Some(\"x\") } \
               fn main() -> Int64 { let o = maybe(1) \
               let s = match o { Some(v) => v, None => \"\", } \
               return Int64(s.byteLength) }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

/// A declared `release` is a user function, and a function cannot be told to
/// leave one field alone.
#[test]
fn a_declared_release_keeps_leaking_its_hole() {
    let src = "protocol Owned { fn release(self) } \
               type Box = { name: String, n: Int64 } \
               impl Owned for Box { fn release(self) { print(1) } } \
               fn mk(a: String) -> Box { return Box { name: a + \"n\", n: 1 } } \
               fn main() -> Int64 { let b = mk(\"x\"); let t = consume b.name; \
               return Int64(t.byteLength) + b.n; }";
    assert!(!reclaims(src, "b"), "{:?}", kepts(src, "main"));
}

#[test]
fn mut_array_with_self_update_is_auto_freed() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
               let mut i = 0; while i < 3 { a.push(i); i = i + 1; } \
               return a[0]; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn explicitly_dropped_array_is_not_auto_freed() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
               a.push(1); let v = a[0]; drop a; return v; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn returned_array_is_not_auto_freed() {
    let src = "fn build() -> Array<Int64> { let mut a: Array<Int64> = []; \
               a.push(1); return a; } fn main() -> Int64 { return 0; }";
    // `a` is moved out by the return, so it is not freed inside `build`.
    assert_eq!(drop_count(src, "build"), 0);
}

#[test]
fn an_annotated_array_literal_is_released() {
    // The defect the RFC was written from: `Expr::ArrayLit` was absent from
    // the expression list, so this leaked on every engine while the identical
    // `array()` call did not. Nothing forced the two to agree, because the
    // list was what decided.
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
               a.push(1); return a[0]; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn an_unannotated_array_literal_is_not_released() {
    // The other half, and the one that costs a heap if it is got wrong.
    // `[1, 2, 3]` with no annotation is a FIXED array held inline, so the
    // literal cannot say what it is — only the annotation can. Answering
    // `Array` for every literal freed a stack address and corrupted the heap.
    let src = "fn main() -> Int64 { let a = [1, 2, 3]; return a[0]; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_self_referring_array_element_is_not_walked() {
    // `type L = Array<L>` has no bottom to a structural walk, and both
    // compiling backends emitted one until they ran out of stack — the same
    // crash `copy` met in Phase 4b. The row answers the buffer alone, so the
    // elements leak, which is the answer this file gives wherever it cannot
    // prove otherwise.
    let src = "type L = Array<L>; fn main() -> Int64 { let xs: L = []; return xs.length; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn a_fresh_key_snapshot_is_released() {
    // `m.keys()` copies the keys into a new buffer (RFC-0028, and the KEYS
    // themselves since RFC-0092 M2) and was absent from the same list.
    let src = "fn main() -> Int64 { let m: Map<String, Int64> = [\"a\": 1]; \
               let ks = m.keys(); return ks.length; }";
    // The map and the snapshot, in whichever order the map iterates. Both
    // are `Deep`: the snapshot's elements are Strings with a release row
    // (U4, M2), and since M3 so are the map's own keys.
    let mut kinds = drop_kinds(src, "main");
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(
        kinds,
        vec![
            DropKind::Deep(Type::Array(Box::new(Type::Str))),
            DropKind::Deep(Type::Map(Box::new(Type::Str), Box::new(Type::Int))),
        ]
    );
}

/// RFC-0095 M3, and RFC-0092 M5's row one keyword over.
///
/// `for x in consume xs` takes the container, and the LOOP gives it back
/// where it ends. That release is the core's own statement and no row's —
/// the take is what the kernel judges at the loop — so the plan places
/// nothing here and `Facts::loop_gives_back` names the loop (RFC-0125 §3
/// M3, the consuming-loop slice and the container slice).
#[test]
fn a_consuming_loop_gives_its_container_back_at_the_loop() {
    let src = "fn make() -> Array<String> { let mut o: Array<String> = [];                o.push(\"a\"); return o; }                fn main() -> Int64 { let xs = make(); let mut n = 0;                for x in consume xs { n = n + Int64(x.byteLength); } return n; }";
    let (o, _) = analyze_src(src);
    assert!(
        placed(&o, "main").is_empty(),
        "the `let` is moved into the loop, so no exit row names it"
    );
    let facts = vyrn_lower::core::facts().expect("the placer folded the core's answers");
    assert_eq!(
        facts.loop_gives_back.len(),
        1,
        "the core names the one loop that gives its container back"
    );
    assert!(
        facts.loop_buffer_only.is_empty(),
        "nothing left through the loop variable, so the release walks the whole container"
    );
}

/// Round sixteen's other half: the `elem_only` attribution is exactly what
/// keeps the downgrade off a buffer somebody else owns. A snapshot a LENDER
/// handed back is the recorded round-fourteen trap — freeing its buffer is
/// a use-after-free, and the poison in `movecheck::facts` must win over the
/// element handover the body also recorded.
#[test]
fn a_lent_snapshot_keeps_its_buffer_even_when_an_element_leaves() {
    let src = "fn pick(xs: Array<String>) -> String \
               { for x in xs { return if true { x } else { \"\" } } return \"\" } \
               fn h(a: Array<String>) -> Array<String> { return [pick(a)] } \
               fn main() -> Int64 { let arr: Array<String> = [\"a\" + \"b\"] \
               let mut out: Array<String> = [] \
               for x in h(arr) { out.push(x) } \
               return out.length }";
    assert!(
        !drop_kinds(src, "main").contains(&DropKind::FreeArr),
        "a lender's result is not the loop's to free"
    );
}

#[test]
fn a_bare_file_with_no_imports_still_frees_its_string() {
    // The bootstrap answer. `vyrn run` on a bare file has no resolver and
    // therefore no `std/`, so the built-in rows are seeded by the compiler and
    // this program — which imports nothing and declares no protocol — still
    // gets a `free`. RFC-0080 M3 refused `?` through a std protocol for this
    // exact reason; the decision that frees memory may not be weaker.
    let src = "fn main() -> Int64 { let a = \"x\"; let s = a + \"y\"; \
               return s.byteLength; }";
    assert!(!src.contains("import"));
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

#[test]
fn a_user_type_declares_how_it_is_released() {
    // The design's own test, in miniature: nothing in the compiler knows the
    // name `Ring`. The row comes out of the program.
    let src = "protocol Owned { fn release(self) } \
               type Ring = { slots: Array<Int64> } \
               impl Owned for Ring { fn release(self) { print(1) } } \
               fn make() -> Ring { return Ring { slots: [] } } \
               fn main() -> Int64 { let r = make(); return 0; }";
    let (o, _) = analyze_src(src);
    assert_eq!(
        o.proto.release_kind(&Type::Named("Ring".into())),
        Some(DropKind::Release(
            "Owned__Ring__release".to_string(),
            Type::Named("Ring".into())
        ))
    );
    assert_eq!(
        drop_kinds(src, "main"),
        vec![DropKind::Release(
            "Owned__Ring__release".to_string(),
            Type::Named("Ring".into())
        )]
    );
}

/// RFC-0092 M3: a record releases its places. Phase 5 measured that it could
/// not, and [`Owned::release_kind`] records the three parity failures a row
/// produced then — a record hands its insides out as projections, and rule 3
/// recorded a returned projection as a lend rather than refusing it. M1
/// refuses all three spellings, so the row is sayable.
#[test]
fn a_record_releases_its_places() {
    let src = "type Ring = { slots: Array<Int64> } \
               fn make() -> Ring { return Ring { slots: [] } } \
               fn main() -> Int64 { let r = make(); return 0; }";
    let (o, _) = analyze_src(src);
    let want = DropKind::Deep(Type::Record(vec![Field {
        name: "slots".into(),
        ty: Type::Array(Box::new(Type::Int)),
    }]));
    assert_eq!(
        o.proto.release_kind(&Type::Named("Ring".into())),
        Some(want.clone())
    );
    assert_eq!(drop_kinds(src, "main"), vec![want]);
}

/// And a record of scalars is not, which keeps the rule about heap.
#[test]
fn a_record_of_scalars_is_reclaimed_by_nothing() {
    let src = "type Point = { x: Int64, y: Int64 } \
               fn make() -> Point { return Point { x: 1, y: 2 } } \
               fn main() -> Int64 { let p = make(); return 0; }";
    let (o, _) = analyze_src(src);
    assert_eq!(o.proto.release_kind(&Type::Named("Point".into())), None);
    assert_eq!(drop_count(src, "main"), 0);
}

/// The guard the `Array` row already carried, one shape over. `type Node =
/// { kids: Array<Node> }` is ordinary Vyrn, and a structural release walk of
/// it has no bottom — the crash `copy` met in Phase 4b, met a third time.
/// It answers nothing and its places leak, which is what this file does
/// wherever it cannot prove otherwise.
#[test]
fn a_self_referring_record_is_not_walked() {
    let src = "type Node = { name: String, kids: Array<Node> } \
               fn main() -> Int64 { let n = Node { name: \"a\", kids: [] }; \
               return n.kids.length; }";
    assert_eq!(drop_count(src, "main"), 0);
}

/// RFC-0096. The same shape with a DECLARATION on the cycle is walked
/// again: the release emits a call at `Node`, and a call is the bottom the
/// structural walk lacked. The record above it — which only REACHES the
/// self-referring type — gets its row back with it, which is 63 corpus
/// bindings on two `impl`s.
#[test]
fn a_declaration_on_the_cycle_gives_the_types_above_it_their_row_back() {
    let decl = "type Node = { name: String, kids: Array<Node> } \
                type Doc = { root: Node, title: String } \
                impl Owned for Node { fn release(consume self) { \
                let name = consume self.name; drop name; \
                let kids = consume self.kids; drop kids; } } ";
    let src = format!(
        "{decl} fn main() -> Int64 {{ \
         let d = Doc {{ root: Node {{ name: \"a\", kids: [] }}, title: \"t\" }}; \
         return d.root.kids.length; }}"
    );
    let (o, _) = analyze_src(&src);
    assert_eq!(
        o.proto.release_kind(&Type::Named("Node".into())),
        Some(DropKind::Release(
            "Owned__Node__release".to_string(),
            Type::Named("Node".into())
        ))
    );
    // The container and the record above the declaration walk again.
    assert!(matches!(
        o.proto
            .release_kind(&Type::Array(Box::new(Type::Named("Node".into())))),
        Some(DropKind::Deep(_))
    ));
    assert!(matches!(
        o.proto.release_kind(&Type::Named("Doc".into())),
        Some(DropKind::Deep(_))
    ));
    assert_eq!(drop_count(&src, "main"), 1);
}

/// RFC-0093's hole. `consume d.title` takes one field and leaves the record
/// behind. M1 left the whole binding unreclaimed rather than free the field
/// twice; M2 carries the hole set to the walk, so the record is reclaimed
/// MINUS the place the take gave away.
#[test]
fn a_record_with_a_taken_field_is_reclaimed_minus_the_hole() {
    let src = "type Doc = { title: String, body: String } \
               fn main() -> Int64 { let d = Doc { title: \"a\" + \"b\", body: \"c\" }; \
               let t = consume d.title; return t.byteLength; }";
    // `t` is the String the record gave away, and `d` is the rest of it.
    // WHICH place the walk skips is the core's answer since the
    // input-circle slice (`tests/coretables.rs`); what this pass still
    // says is that the record is reclaimed at all.
    assert_eq!(
        kepts(src, "main"),
        vec![
            Some(DropKind::Deep(Type::Record(vec![
                Field {
                    name: "title".into(),
                    ty: Type::Str,
                },
                Field {
                    name: "body".into(),
                    ty: Type::Str,
                },
            ]))),
            Some(DropKind::FreeStr),
        ]
    );
}

/// Census §14, Phase 5. An `Option` and a `Result` DO own their payload:
/// the recommended way to write a fallible function was also the leaking one.
#[test]
fn a_sum_owns_its_payload() {
    let src = "fn pick(a: String, b: String) -> Option<String> { return Some(a + b); } \
               fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let o = pick(a, b); return 0; }";
    let (o, _) = analyze_src(src);
    let want = opt_row(Type::Str);
    assert_eq!(
        o.proto.release_kind(&Type::option(Type::Str)),
        Some(want.clone())
    );
    assert_eq!(drop_kinds(src, "main"), vec![want]);
}

/// And an `Option` of a scalar is not, which keeps the rule about heap.
#[test]
fn a_sum_of_scalars_is_reclaimed_by_nothing() {
    let src = "fn pick(n: Int64) -> Option<Int64> { return Some(n); } \
               fn main() -> Int64 { let o = pick(1); return 0; }";
    let (o, _) = analyze_src(src);
    assert_eq!(o.proto.release_kind(&Type::option(Type::Int)), None);
    assert_eq!(drop_count(src, "main"), 0);
}
