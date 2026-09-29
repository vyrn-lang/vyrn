//! The release plan over one body: which bindings a frame reclaims, and with
//! what. The rows are the placer's, so these tests live here, where a lowering
//! can be installed and the placer can run.
//!
//! `drop_count` counts the distinct bindings `Ownership::releases` names for a
//! function, `drop_kinds` reads the kind off each row, `kepts` lines the rows
//! up against the `let`s in source order, and `reclaims` asks about one name.

use std::collections::HashMap;

use vyrn_frontend::ast::{Block, EnumVariant, Field, NodeId, Program, Stmt, Type};
use vyrn_frontend::own::{analyze, DropKind, Ownership};

/// Parses, then analyses with the placer installed, so the rows are the core's.
fn analyze_src(src: &str) -> (Ownership, Program) {
    vyrn_lower::install();
    let (p, _memo) = vyrn_frontend::project::Memo::load(|| {
        vyrn_frontend::parser::parse(vyrn_frontend::lexer::lex(src).unwrap())
    })
    .unwrap();
    let o = analyze(&p);
    (o, p)
}

/// The kind each binding of `which` is released with, by binding key. A
/// binding released at several exits carries one kind.
fn placed(o: &Ownership, which: &str) -> HashMap<NodeId, DropKind> {
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

/// The release for every `let` in `which`, in source order; `None` where the
/// frame reclaims nothing.
fn kepts(src: &str, which: &str) -> Vec<Option<DropKind>> {
    let (o, p) = analyze_src(src);
    let f = p.functions.iter().find(|f| f.name == which).unwrap();
    let d = placed(&o, which);
    let mut lets = Vec::new();
    let_stmts(&f.body, &mut lets);
    lets.iter().map(|s| d.get(&s.id()).cloned()).collect()
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

/// Whether `main`'s frame reclaims the binding named `name` at all.
fn reclaims(src: &str, name: &str) -> bool {
    let (o, p) = analyze_src(src);
    let f = p.functions.iter().find(|f| f.name == "main").unwrap();
    let mut lets = Vec::new();
    let_stmts(&f.body, &mut lets);
    let s = lets
        .iter()
        .find(|s| matches!(s, Stmt::Let { name: n, .. } if n == name))
        .unwrap();
    placed(&o, "main").contains_key(&s.id())
}

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

/// `Some(x)` types as `Option<type_of(x)>`; typed as bare `Option`, the
/// payload box has no owner.
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

/// A lambda's capture is a deep snapshot (the backends duplicate heap
/// captures into the block), so the captured binding still reclaims.
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

/// `let t = s` moves: the new name owns the buffer, so the
/// block frees it once.
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
    // `xs.push(s)` moves `s` into a container that outlives the inner block.
    // Freeing `s` would leave the array holding a dangling buffer; the array
    // releases the element itself.
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let mut xs: Array<String> = []; \
               if true { let s = a + b; xs.push(s); } \
               return xs.length; }";
    assert_eq!(
        drop_kinds(src, "main"),
        vec![DropKind::Deep(Type::Array(Box::new(Type::Str)))]
    );
}

/// The ownership test is the block header, stated once in `free`: an arena
/// block carries a class word of 0 and is refused in silence. Skipping the
/// binding would claim for the arena every block minted at that depth, a
/// callee's included.
#[test]
fn a_binding_inside_a_region_is_droppable_like_any_other() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; let mut n = 0; \
               region { let s = a + b; n = s.byteLength } return n; }";
    assert_eq!(drop_count(src, "main"), 1);
}

/// Rule 1 governs reassignment, so the binding owns whatever it holds last and
/// the block frees that.
#[test]
fn a_mutable_string_is_reclaimed() {
    let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
               let mut s = a + b; return s.byteLength; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

#[test]
fn an_inner_let_shadowing_a_string_is_not_a_string() {
    // `s + 1` under `let s = 1` is an integer add. Reading the outer `s` makes
    // `t` droppable, and the backend frees the integer 2.
    let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
               if true { let s = 1; let t = s + 1; print(t); } return 0; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_loop_binder_shadowing_a_string_is_not_a_string() {
    let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
               let ns: Array<Int64> = []; \
               for s in ns { let t = s + 1; print(t); } return 0; }";
    // The array only: `s` is a literal, and `t` is an integer add.
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
    // The mirror of the four above: "an inner binding is never a string" would
    // turn the miscompile into a leak.
    let src = "fn main() -> Int64 { let a = \"x\"; \
               let s = a + \"y\"; print(s); \
               if true { let s = a + \"z\"; print(s); } return 0; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
}

#[test]
fn an_inner_let_shadowing_a_non_string_is_a_string() {
    // The innermost binding answers, so an inner String under an outer
    // integer concatenates.
    let src = "fn main() -> Int64 { let s = 1; print(s); \
               if true { let s = \"a\"; let t = s + \"b\"; print(t); } return 0; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

/// `copy` is a producer, so the copy is the caller's to release, and the
/// receiver stays a live owner.
#[test]
fn copy_transfers_and_leaves_the_receiver_owned() {
    let src = "fn main() -> Int64 { let a = \"x\" + \"y\"; let b = a.copy(); \
               print(a); print(b); return 0; }";
    assert_eq!(drop_count(src, "main"), 2);
}

/// A `read` parameter promises the caller keeps the value (rule 2): the caller
/// frees its own String, and the callee's result is a second one it owns.
#[test]
fn passing_to_a_read_parameter_keeps_the_caller_the_owner() {
    let src = "fn tail(s: String) -> String { return s + \"!\"; } \
               fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let y = tail(s); return y.byteLength; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
}

/// An arm that hands its payload out gives the scrutinee up, so the binding
/// the payload flowed into is the only owner. Releasing both is a double free.
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
    assert_eq!(drop_count(src, "build"), 0);
}

#[test]
fn an_annotated_array_literal_is_released() {
    let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
               a.push(1); return a[0]; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn an_unannotated_array_literal_is_not_released() {
    // `[1, 2, 3]` with no annotation is a fixed array held inline; only the
    // annotation can say what a literal is. Answering `Array` here frees a
    // stack address and corrupts the heap.
    let src = "fn main() -> Int64 { let a = [1, 2, 3]; return a[0]; }";
    assert_eq!(drop_count(src, "main"), 0);
}

#[test]
fn a_self_referring_array_element_is_not_walked() {
    // `type L = Array<L>` has no bottom to a structural walk. The row answers
    // the buffer alone, so the elements leak: where the plan cannot prove a
    // release, it leaks.
    let src = "type L = Array<L>; fn main() -> Int64 { let xs: L = []; return xs.length; }";
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
}

#[test]
fn a_fresh_key_snapshot_is_released() {
    // `m.keys()` copies the key Strings themselves into a fresh buffer.
    let src = "fn main() -> Int64 { let m: Map<String, Int64> = [\"a\": 1]; \
               let ks = m.keys(); return ks.length; }";
    // The map and the snapshot, in either order. Both are `Deep`: the
    // snapshot's elements and the map's keys are Strings with a release row.
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

/// `for x in consume xs` takes the container, and the loop gives it back where
/// it ends. That release is the core's own statement, not a row, so the plan
/// places nothing and `Facts::loop_gives_back` names the loop.
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

/// The `elem_only` attribution keeps the downgrade off a buffer somebody else
/// owns. Freeing a lender's snapshot is a use-after-free, so the poison in
/// `movecheck::facts` wins over the element handover the body also records.
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
    // `vyrn run` on a bare file has no resolver and no `std/`, so the compiler
    // seeds the built-in rows and this program still gets a `free`.
    let src = "fn main() -> Int64 { let a = \"x\"; let s = a + \"y\"; \
               return s.byteLength; }";
    assert!(!src.contains("import"));
    assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
}

#[test]
fn a_user_type_declares_how_it_is_released() {
    // Nothing in the compiler knows the name `Ring`; the row comes out of the
    // program.
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

/// A record releases its places. The row is sound because a returned
/// projection is refused, not recorded as a lend (rule 3).
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

/// The rule is about heap.
#[test]
fn a_record_of_scalars_is_reclaimed_by_nothing() {
    let src = "type Point = { x: Int64, y: Int64 } \
               fn make() -> Point { return Point { x: 1, y: 2 } } \
               fn main() -> Int64 { let p = make(); return 0; }";
    let (o, _) = analyze_src(src);
    assert_eq!(o.proto.release_kind(&Type::Named("Point".into())), None);
    assert_eq!(drop_count(src, "main"), 0);
}

/// The `Array` row's guard, one shape over: a structural release walk of
/// `type Node = { kids: Array<Node> }` has no bottom, so it answers nothing and
/// its places leak.
#[test]
fn a_self_referring_record_is_not_walked() {
    let src = "type Node = { name: String, kids: Array<Node> } \
               fn main() -> Int64 { let n = Node { name: \"a\", kids: [] }; \
               return n.kids.length; }";
    assert_eq!(drop_count(src, "main"), 0);
}

/// A declaration on the cycle gives the walk its bottom: the release
/// emits a call at `Node`. A record that only reaches the self-referring type
/// gets its row back with it.
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

/// `consume d.title` takes one field and leaves a hole; the record is
/// reclaimed minus the place the take gave away.
#[test]
fn a_record_with_a_taken_field_is_reclaimed_minus_the_hole() {
    let src = "type Doc = { title: String, body: String } \
               fn main() -> Int64 { let d = Doc { title: \"a\" + \"b\", body: \"c\" }; \
               let t = consume d.title; return t.byteLength; }";
    // Which place the walk skips is the core's answer (`tests/coretables.rs`);
    // this test says only that the record is reclaimed at all.
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

/// An `Option` and a `Result` own their payload.
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

/// The rule is about heap.
#[test]
fn a_sum_of_scalars_is_reclaimed_by_nothing() {
    let src = "fn pick(n: Int64) -> Option<Int64> { return Some(n); } \
               fn main() -> Int64 { let o = pick(1); return 0; }";
    let (o, _) = analyze_src(src);
    assert_eq!(o.proto.release_kind(&Type::option(Type::Int)), None);
    assert_eq!(drop_count(src, "main"), 0);
}
