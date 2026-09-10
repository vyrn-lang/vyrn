//! RFC-0125 §3 M3, the column slice: every refusal a reader gets in the editor
//! is pinned to a token, and the kernel's are no exception.
//!
//! `vyrn-frontend` cannot reach a kernel refusal — the memory judgment lives
//! above it, behind `own.rs`'s installed slot — so `tests/diagnostics_api.rs`
//! and `tests/symbols_api.rs` lost their ownership pins when the last rule left
//! `movecheck.rs` (RFC-0125 §3 M3, `track-dt`'s track gates). This crate links
//! both halves, so the pin lives here: `vyrn_lower::install()` and then
//! `vyrn_frontend::symbols::analyze`, which is the call `vyrn-lsp` makes.
//!
//! What it asserts is the two halves of one sentence. A refusal keeps its LINE,
//! which is the whole-stderr licence every other census reads. And it gains a
//! COLUMN: a 1-based span over a real token of that line, never `0` (which the
//! LSP squiggles as the whole line, leading spaces and `return` included).

use std::path::{Path, PathBuf};

fn dirs() -> Vec<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    vec![base.join("refusals"), base.join("unlicensed")]
}

/// Every `.vyrn` under the two refusal corpora, sorted.
fn programs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The first diagnostic `analyze` gives for `src`, with the lowering installed.
fn first(src: &str) -> Option<vyrn_frontend::diagnostics::Diagnostic> {
    vyrn_lower::install();
    vyrn_frontend::symbols::analyze(src)
        .diagnostics
        .into_iter()
        .next()
}

/// Every program of the two refusal corpora is refused with a line AND a
/// column, and the column names a span on that line of the source.
#[test]
fn every_refusal_the_editor_shows_is_pinned_to_a_token() {
    let mut bad: Vec<String> = Vec::new();
    for p in programs() {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&p).expect("read the program");
        let Some(d) = first(&src) else {
            bad.push(format!("{name}: the editor shows nothing"));
            continue;
        };
        if d.line == 0 {
            bad.push(format!("{name}: no line"));
            continue;
        }
        if d.col == 0 || d.end_col <= d.col {
            bad.push(format!(
                "{name}: line {} has no column ({}:{}) — {}",
                d.line,
                d.col,
                d.end_col,
                d.message.lines().next().unwrap_or("")
            ));
            continue;
        }
        // The span must land on the line it names.
        let line = src.replace("\r\n", "\n");
        let Some(text) = line.lines().nth(d.line - 1) else {
            bad.push(format!("{name}: line {} is past the end", d.line));
            continue;
        };
        let width = text.chars().count();
        if d.col > width || d.end_col > width + 1 {
            bad.push(format!(
                "{name}: {}:{}..{} is off the end of a {width}-character line",
                d.line, d.col, d.end_col
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "a refusal a reader gets has lost its column:\n  {}",
        bad.join("\n  ")
    );
}

/// The table for RFC-0125 §3 M3, printed from the corpus:
/// `cargo test -p vyrn-cli --test columns -- --ignored --nocapture`.
#[test]
#[ignore]
fn the_pinned_columns_of_the_refusal_corpora() {
    for p in programs() {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&p).expect("read the program");
        match first(&src) {
            Some(d) => println!(
                "{name} {}:{}:{} [{}] {}",
                d.line,
                d.col,
                d.end_col,
                d.stage,
                d.message.lines().next().unwrap_or("")
            ),
            None => println!("{name} — nothing"),
        }
    }
}
