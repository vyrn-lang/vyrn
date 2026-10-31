//! The provenance analysis (`vyrn_lower::provenance`) over the 46 functions
//! the analytic report of 2026-10-21 classified by hand, pinned in
//! `tests/pins/provenance.tsv`: each function's class, its hand label, and
//! the reason the analysis gives where it cannot say. A row whose class
//! differs from its hand label is a disagreement; the commit that moves one
//! explains it.

mod common;
use common::*;

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The census: the file that declares each function, its name and its hand
/// label.
const CENSUS: &[(&str, &str, &str)] = &[
    ("std/arrays.vyrn", "map", "identity pointwise"),
    ("std/arrays.vyrn", "filter", "subsequence"),
    ("std/arrays.vyrn", "sortWith", "permutation"),
    ("std/arrays.vyrn", "sortBy", "permutation"),
    ("std/bench.vyrn", "sortedCopy", "permutation"),
    ("std/graphql.vyrn", "copyGqlErrs", "identity"),
    ("std/graphql.vyrn", "gqlStep", "identity+fresh(end)"),
    ("std/graphql.vyrn", "gqlArgsBesides", "subsequence"),
    ("std/html.vyrn", "copyHtmlArray", "identity"),
    ("std/html.vyrn", "withKey", "identity+fresh(end)"),
    ("std/html.vyrn", "childPath", "identity+fresh(end)"),
    ("std/html.vyrn", "listInsert", "identity+fresh(inside)"),
    ("std/html.vyrn", "listMove", "gather"),
    ("std/http.vyrn", "httpNamesOf", "fresh"),
    ("std/json.vyrn", "copyJsonArray", "identity"),
    ("std/json.vyrn", "copyJsonFields", "identity"),
    ("std/json.vyrn", "sortKeysArray", "identity pointwise"),
    ("std/json.vyrn", "sortKeysFields", "gather"),
    ("std/openapi.vyrn", "oaSorted", "unknown"),
    ("std/regex.vyrn", "joinHoles", "concat"),
    ("std/ui.vyrn", "uiAppendStr", "identity+fresh(end)"),
    ("std/ui.vyrn", "uiDynNamesOf", "subsequence pointwise"),
    ("std/ui.vyrn", "uiHeadJoin2", "concat"),
    ("std/ui.vyrn", "uiDynArgs", "fresh"),
    ("std/ui.vyrn", "uiDispatchOrder", "fresh"),
    ("std/von.vyrn", "copyVonArray", "identity"),
    ("std/vyx.vyrn", "vyxAppend", "identity+fresh(end)"),
    ("std/vyx.vyrn", "vyxStripDirectives", "subsequence"),
    ("std/vyx.vyrn", "vyxElementRoots", "subsequence"),
    ("std/vyx.vyrn", "vyxCompNames", "fresh"),
    ("std/vyx.vyrn", "vyxImportedNames", "fresh"),
    ("std/stream.vyrn", "merge", "shuffle"),
    ("examples/capturefn.vyrn", "applyAll", "identity pointwise"),
    ("examples/clearkeep.vyrn", "fill", "identity+fresh(end)"),
    ("examples/consumeloop.vyrn", "spliced", "concat"),
    ("examples/consumeloop.vyrn", "lengthsOf", "identity"),
    ("examples/fannkuch.vyrn", "flip", "permutation"),
    ("examples/map.vyrn", "put", "identity+fresh(end)"),
    ("examples/matchown.vyrn", "takeWords", "identity"),
    ("examples/part_of_a_temporary.vyrn", "pass", "identity"),
    ("examples/permute.vyrn", "apply", "gather"),
    ("examples/permute.vyrn", "inverse", "fresh"),
    (
        "examples/revcomp.vyrn",
        "reverseComplement",
        "reverse pointwise",
    ),
    ("examples/recordorder.vyrn", "arrays", "identity"),
    ("examples/copy.vyrn", "twice", "stutter"),
];

/// The root a function is checked under: its own file, or for a generic
/// function the first root that instantiates it.
fn root_of(file: &'static str, name: &str) -> &'static str {
    match (file, name) {
        ("std/arrays.vyrn", "map") => "examples/lambdas.vyrn",
        ("std/arrays.vyrn", "filter") => "examples/lambdas.vyrn",
        ("std/arrays.vyrn", "sortWith") => "examples/knucleotide.vyrn",
        ("std/arrays.vyrn", "sortBy") => "examples/closures2.vyrn",
        _ => file,
    }
}

/// The pinned row of each census function of `root`: its label, or `not an
/// array result` where it returns none, and the reason an unknown gives.
fn rows_of(root: &str) -> Vec<(usize, String)> {
    let path = repo_root().join(root);
    let src = std::fs::read_to_string(&path).expect("the root reads");
    let key = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let engine = vyrn_genwasm::engine();
    let (program, world) = vyrn_lower::load_warned(
        &src,
        &key,
        &opts,
        &vyrn_frontend::loader::DiskResolver,
        Some(&*engine),
    )
    .0
    .unwrap_or_else(|_| panic!("{root} checks"));
    let rows = world.fn_rows();
    let mut out = Vec::new();
    for (i, (file, name, _)) in CENSUS.iter().enumerate() {
        let r = root_of(file, name);
        if r != root {
            continue;
        }
        let found = (0..rows.len()).find_map(|k| {
            let id = vyrn_frontend::ast::FnId::nth(k);
            let source = rows[k].generic.as_ref().map_or(id, |g| g.0);
            // A std module's declaration takes its reserved spelling (`json$`), and a
            // root's is renamed apart from a std one (`fill__from0`).
            let f = &program.functions.get(source.index())?.name;
            let f = f.rsplit_once('$').map_or(f.as_str(), |(_, f)| f);
            if f != *name
                && !f
                    .strip_prefix(*name)
                    .is_some_and(|r| r.starts_with("__from"))
            {
                return None;
            }
            let body = world.body_at(id)?;
            let here = match &body.file {
                None => *file == r,
                Some(f) => f.ends_with(file),
            };
            (here && !body.name.contains("@lambda")).then_some(body)
        });
        let (label, why) = match found.map(vyrn_lower::provenance::of) {
            None => ("no core body".to_string(), String::new()),
            Some(None) => ("not an array result".to_string(), String::new()),
            Some(Some(p @ vyrn_lower::provenance::Provenance::Unknown(why))) => {
                (p.label(), why.to_string())
            }
            Some(Some(p)) => (p.label(), String::new()),
        };
        out.push((i, format!("{label}\t{why}")));
    }
    out
}

/// Each census function's provenance, against its hand label.
#[test]
fn the_hand_census_holds_against_real_core_bodies() {
    let mut roots: Vec<&str> = CENSUS.iter().map(|r| root_of(r.0, r.1)).collect();
    roots.sort_unstable();
    roots.dedup();
    let mut rows: Vec<(usize, String)> = std::thread::scope(|s| {
        // Four threads: each load holds a program and its World.
        let runs: Vec<_> = (roots.chunks(roots.len().div_ceil(4)))
            .map(|rs| {
                std::thread::Builder::new()
                    .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
                    .spawn_scoped(s, move || {
                        rs.iter().flat_map(|r| rows_of(r)).collect::<Vec<_>>()
                    })
                    .expect("a census thread starts")
            })
            .collect();
        runs.into_iter()
            .flat_map(|h| h.join().expect("a root's census"))
            .collect()
    });
    rows.sort_by_key(|r| r.0);
    let lines = rows.into_iter().map(|(i, row)| {
        let (file, name, hand) = CENSUS[i];
        let file = file.trim_end_matches(".vyrn");
        let got = row.split('\t').next().unwrap_or_default();
        let verdict = if got == hand { "agrees" } else { "differs" };
        format!("{file} {name}\t{row}\t{hand}\t{verdict}")
    });
    pin(
        "provenance",
        "function\tprovenance\tunknown because\thand label\tverdict",
        lines,
    );
}

/// `vyrn why --provenance` prints each array result under its function's
/// signature, with each source's class where two sources meet.
#[test]
fn why_prints_each_array_result_under_its_function() {
    let out = vyrn()
        .current_dir(repo_root())
        .args(["why", "--provenance", "examples/consumeloop.vyrn"])
        .output()
        .expect("vyrn why --provenance");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let text = norm(&out.stdout);
    let spliced = "fn spliced(a: Array<String>, b: Array<String>) -> Array<String>\n  \
                   result: concat, from `a` (identity), `b` (identity)\n";
    assert!(text.contains(spliced), "{text}");
}
