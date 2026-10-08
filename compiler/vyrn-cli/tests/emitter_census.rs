//! The structural census of `compiler/vyrn-codegen/src/direct.rs`, the emitter
//! by the method of `checker_census.rs`.
//!
//! A section is one anchor item plus every item after it up to the next
//! anchor. Its span runs from the anchor's doc comment to the line before the
//! next anchor's, so every line belongs to one section and the counts add up
//! to the file. The table records anchor and kind; the test computes spans.
//!
//! Beside lines, each section counts its `Instruction::` sites: the wasm it
//! writes by hand. A family that leaves the emitter moves both numbers; one
//! that only gets shorter moves lines alone, which catches a deletion of prose
//! rather than of emission.

mod common;

use std::path::{Path, PathBuf};

/// What a section of `direct.rs` is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// The mapping section 2.3 allows: a `prim` row to its instruction, `load`
    /// and `store` to typed memory access, `drop` to a call, `trap` to a call
    /// with a table index, control flow to wasm's blocks.
    Mapping,
    /// A decision section 2.3 forbids: placing, an unrequested bound check,
    /// deciding validation, optimizing, re-typing, or a rewrite that belongs
    /// in an earlier pass. The deletion candidates.
    Decision,
    /// The runtime written by hand: the declaration table, its index and a few
    /// inline sequences; the functions live in `std/runtime.vyrn`.
    Runtime,
    /// One hand-written block per builtin name; a row-driven mapping over the
    /// builtin's signature and the core's rows replaces one.
    Builtin,
    /// The wasm format: import tables, ABI rules, custom sections, memory
    /// arguments.
    Encoding,
    /// The driver, the monomorphisation queue, contexts, frames, scratch
    /// locals, name lookup.
    Shared,
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Mapping => "the mapping §2.3 names",
            Kind::Decision => "a decision §2.3 says it must not make",
            Kind::Runtime => "the runtime it emits by hand",
            Kind::Builtin => "one block per builtin name",
            Kind::Encoding => "the wasm format",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// What a section reads to decide what it emits.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Reads {
    /// The section decides nothing about a source form.
    Neither,
    Core,
    /// The source form, and the core has a row that says the same thing: a
    /// second statement of a rule, a switch to take.
    Twice,
    /// The source form, and the core states no row for it.
    Source,
    /// The core for a release, a take or a type; the source form for what to
    /// emit.
    Both,
}

impl Reads {
    fn label(self) -> &'static str {
        match self {
            Reads::Neither => "neither",
            Reads::Core => "the core's rows",
            Reads::Twice => "the source, and the core says it too",
            Reads::Source => "the source, and the core has no row",
            Reads::Both => "both, for two questions",
        }
    }
}

/// `at` is the head of the item that starts the section.
struct Section {
    at: &'static str,
    kind: Kind,
    reads: Reads,
}

const fn sec(at: &'static str, kind: Kind, reads: Reads) -> Section {
    Section { at, kind, reads }
}

/// The sections in file order; the first starts at line 1.
fn sections() -> Vec<Section> {
    use Kind::*;
    #[allow(clippy::enum_glob_use)]
    use Reads::{Both, Core, Neither, Source};
    vec![
        sec("fn unsupported", Shared, Neither),
        sec("struct Wasi", Encoding, Neither),
        sec("struct Ext", Encoding, Neither),
        sec("fn compile", Shared, Neither),
        sec("fn compile_inner", Shared, Core),
        sec("fn abi_kind", Encoding, Neither),
        sec("enum MapKey", Shared, Neither),
        sec("struct Sig", Shared, Neither),
        sec("struct Cx", Shared, Neither),
        sec("fn loop_buffer_only", Mapping, Core),
        sec("fn sub", Shared, Core),
        sec("fn wasm_sig", Encoding, Neither),
        sec("enum Place", Shared, Neither),
        sec("impl Place", Mapping, Neither),
        sec("const LAMBDA", Shared, Neither),
        sec("struct Fn_", Shared, Neither),
        sec("fn lower_globals_init", Shared, Both),
        sec("fn lower_body", Mapping, Core),
        sec("fn frame_fits", Decision, Neither),
        sec("fn lower_fnval_copy", Shared, Neither),
        sec("fn scratch", Shared, Neither),
        sec("fn register_rel", Mapping, Core),
        sec("fn rel_for", Mapping, Neither),
        sec("fn addr_local", Mapping, Neither),
        sec("fn region_enter", Mapping, Neither),
        sec("fn lookup", Shared, Neither),
        sec("fn place_for", Mapping, Neither),
        sec("fn coerce", Mapping, Neither),
        sec("fn emit_validation", Mapping, Neither),
        sec("fn regex_dfa", Builtin, Neither),
        sec("fn str_bin", Mapping, Neither),
        sec("fn host", Builtin, Neither),
        sec("fn is_extern", Mapping, Neither),
        sec("fn print_value", Builtin, Neither),
        sec("fn core_mem", Mapping, Neither),
        sec("fn log_write", Builtin, Neither),
        sec("fn out_ptr", Builtin, Neither),
        sec("fn emit_call_with", Mapping, Source),
        sec("fn length_of", Builtin, Neither),
        sec("struct Walk", Builtin, Source),
        sec("fn walk", Mapping, Neither),
        sec("fn trap_row", Mapping, Neither),
        sec("fn bounds_check", Mapping, Neither),
        sec("fn load_elem", Mapping, Neither),
        sec("fn fixed_elems", Builtin, Neither),
        sec("type Sum", Shared, Source),
        sec("fn owns_heap", Decision, Neither),
        sec("fn copy_word", Mapping, Neither),
        sec("fn try_construct", Decision, Neither),
        sec("fn tag_test", Mapping, Neither),
        sec("fn map_into", Builtin, Neither),
        sec("fn sa_parts", Builtin, Neither),
        sec("fn at", Encoding, Neither),
        sec("struct Num", Mapping, Neither),
        sec("vyrn_frontend::body_scope_descent!", Shared, Source),
        sec("type Spill", Mapping, Neither),
        sec("macro_rules! runtime_fns", Runtime, Neither),
        sec("struct Rt", Runtime, Neither),
        sec("const SHDR", Runtime, Neither),
        sec("fn runtime", Runtime, Neither),
        sec("const RIGHT_FD_WRITE", Shared, Neither),
        sec("fn builtin_spec", Mapping, Both),
        sec("#[cfg(test)]", Tests, Neither),
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn emitter() -> Vec<String> {
    let p = repo_root().join("compiler/vyrn-codegen/src/direct.rs");
    std::fs::read_to_string(&p)
        .expect("read direct.rs")
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

fn spans(lines: &[String]) -> Vec<(usize, usize, usize)> {
    common::census_spans("direct.rs", lines, sections().iter().map(|s| s.at))
}

fn instructions(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .map(|l| l.matches("Instruction::").count())
        .sum()
}

/// Counts the source forms a span names in code. `Expr::line` is an accessor,
/// not a form.
fn forms(lines: &[String], a: usize, b: usize) -> usize {
    lines[a - 1..b]
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| {
            ["Expr::", "Stmt::", "Pattern::", "ArmBody::"]
                .iter()
                .map(|p| l.matches(p).count())
                .sum::<usize>()
                - l.matches("Expr::line").count()
        })
        .sum()
}

/// Counts the core reads in a span: the [`Cx`] queries that reach
/// `vyrn_lower::core`, the record's two type answers and the
/// core's statements. A query added to the emitter must be added here before a
/// section can be classified as reading it.
fn rows(lines: &[String], a: usize, b: usize) -> usize {
    const Q: [&str; 18] = [
        "body_of",
        "St::",
        "Rhs::",
        "Lit::",
        "Callee::",
        ".direct()",
        "receiver_row",
        "store_row",
        "store_fact",
        "discarded_row",
        "loop_gives_back",
        "arg_drop_row",
        "loop_buffer_only",
        "arm_row",
        "owns_scrutinee",
        "frees_boxes",
        "node_ty",
        "join_ty",
    ];
    lines[a - 1..b]
        .iter()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| Q.iter().map(|q| l.matches(q).count()).sum::<usize>())
        .sum()
}

#[test]
fn the_structural_census_covers_the_file() {
    let lines = emitter();
    let spans = spans(&lines);
    let mut next = 1;
    for (_, a, b) in &spans {
        assert_eq!(*a, next, "a gap or an overlap at line {a} of direct.rs");
        next = b + 1;
    }
    assert_eq!(
        next - 1,
        lines.len(),
        "the last section does not reach the end of direct.rs"
    );
}

/// Pins lines and hand-emitted instructions per kind in
/// `tests/pins/emitter-census.tsv`; `VYRN_PIN=write` rewrites it.
#[test]
fn the_emitter_census_matches_its_pin() {
    let lines = emitter();
    let secs = sections();
    let mut by_kind = std::collections::BTreeMap::new();
    let mut ins_by_kind = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        *by_kind.entry(secs[i].kind as usize).or_insert(0usize) += b - a + 1;
        *ins_by_kind.entry(secs[i].kind as usize).or_insert(0usize) += instructions(&lines, a, b);
    }
    let got: Vec<(&'static str, usize, usize)> = [
        Kind::Mapping,
        Kind::Decision,
        Kind::Runtime,
        Kind::Builtin,
        Kind::Encoding,
        Kind::Shared,
        Kind::Tests,
    ]
    .iter()
    .map(|k| {
        (
            k.label(),
            by_kind.get(&(*k as usize)).copied().unwrap_or(0),
            ins_by_kind.get(&(*k as usize)).copied().unwrap_or(0),
        )
    })
    .collect();
    common::pin(
        "emitter-census",
        "kind\tlines\twasm",
        got.iter().map(|(k, n, w)| format!("{k}\t{n}\t{w}")),
    );
    assert_eq!(
        got.iter().map(|(_, n, _)| n).sum::<usize>(),
        lines.len(),
        "the kinds do not add up to the file"
    );
    assert_eq!(
        got.iter().map(|(_, _, n)| n).sum::<usize>(),
        instructions(&lines, 1, lines.len()),
        "the instruction counts do not add up to the file"
    );
}

/// Pins what each class reads in `tests/pins/emitter-reads.tsv`, and checks
/// each section's class against its counts. No count tells `Twice` from
/// `Source`; that stays the reader's call, as `Kind` is.
#[test]
fn what_the_emitter_reads_matches_its_pin() {
    let lines = emitter();
    let secs = sections();
    let mut by_class = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        let (f, r) = (forms(&lines, a, b), rows(&lines, a, b));
        let at = secs[i].at;
        match secs[i].reads {
            Reads::Neither => assert!(
                f == 0 && r == 0,
                "`{at}` is filed as reading neither and names {f} source forms and {r} core rows"
            ),
            Reads::Core => assert!(
                f == 0 && r > 0,
                "`{at}` is filed as reading the core and names {f} source forms and {r} core rows"
            ),
            Reads::Twice | Reads::Source => assert!(
                f > 0 && r == 0,
                "`{at}` is filed as reading the source and names {f} source forms and {r} core rows"
            ),
            Reads::Both => assert!(
                f > 0 && r > 0,
                "`{at}` is filed as reading both and names {f} source forms and {r} core rows"
            ),
        }
        let e = by_class
            .entry(secs[i].reads as usize)
            .or_insert((0usize, 0usize, 0usize, 0usize));
        e.0 += 1;
        e.1 += b - a + 1;
        e.2 += f;
        e.3 += r;
    }
    let got: Vec<(&'static str, usize, usize, usize, usize)> = [
        Reads::Neither,
        Reads::Core,
        Reads::Twice,
        Reads::Source,
        Reads::Both,
    ]
    .iter()
    .map(|c| {
        let (n, l, f, r) = by_class
            .get(&(*c as usize))
            .copied()
            .unwrap_or((0, 0, 0, 0));
        (c.label(), n, l, f, r)
    })
    .collect();
    common::pin(
        "emitter-reads",
        "class\tsections\tlines\tforms\trows",
        got.iter()
            .map(|(c, n, l, f, r)| format!("{c}\t{n}\t{l}\t{f}\t{r}")),
    );
    assert_eq!(
        got.iter().map(|(_, n, ..)| n).sum::<usize>(),
        secs.len(),
        "the classes do not add up to the sections"
    );
    assert_eq!(
        got.iter().map(|(_, _, l, ..)| l).sum::<usize>(),
        lines.len(),
        "the classes do not add up to the file"
    );
}
