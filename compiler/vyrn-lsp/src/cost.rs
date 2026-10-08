//! What each line of the open document costs, as inlay hints and code lenses.
//!
//! The rows are `vyrn why --cost`'s, read from [`Analysis::cost`], so the words are its
//! words. When the document's last `vyrn run --profile` matches its source
//! ([`vyrn_lower::lastrun`]), each allocating row also shows the blocks that run made there.

use lsp_types::{InlayHint, InlayHintLabel, InlayHintTooltip, Position};
use vyrn_frontend::{Analysis, CostLine, FnCost};
use vyrn_lower::lastrun::{self, Found, Sites};

use crate::{char_col_to_utf16, line_of_text};

/// The verb of a check row `why --cost` keeps. A run counts no blocks for it.
const KEPT: &str = "check kept";

/// The last run of the document at `path`, if its stamp is the buffer's `src` and the modules
/// `analysis` linked.
fn last_run(path: Option<&str>, src: &str, analysis: &Analysis) -> Found {
    match path {
        Some(p) => lastrun::load(p, &lastrun::stamp(src, &analysis.module_hashes)),
        None => Found::Never,
    }
}

/// `[blocks, bytes]` the run made at `row` of `f`.
fn ran(run: &Sites, f: &FnCost, row: &CostLine) -> [u64; 2] {
    let at = (f.name.clone(), row.line as u32, row.verb.to_string());
    run.get(&at).map_or([0, 0], |c| [c[0], c[1]])
}

/// One row as an end-of-line label: the verb, its count, `(implicit)` for a copy, and after a
/// middle dot the blocks of the last run.
fn label(f: &FnCost, row: &CostLine, run: Option<&Sites>) -> String {
    let mut out = row.verb.to_string();
    if row.count > 1 {
        out += &format!(" {}", row.count);
    }
    if row.implicit {
        out += " (implicit)";
    }
    match run {
        Some(run) if row.verb != KEPT => out + &format!(" \u{b7} {} blocks", ran(run, f, row)[0]),
        _ => out,
    }
}

/// One row as tooltip text: `why --cost`'s row, with the loop it sits in and the run's counts.
fn tooltip(f: &FnCost, row: &CostLine, run: Option<&Sites>) -> String {
    let mut out = format!("{} {}", row.verb, row.text);
    if row.depth > 0 {
        out += &format!(" (loop {})", row.depth);
    }
    match run {
        Some(run) if row.verb != KEPT => {
            let [blocks, bytes] = ran(run, f, row);
            out + &format!("\nlast run: {blocks} blocks, {bytes} bytes")
        }
        _ => out,
    }
}

/// The hints for lines `from..=to` (1-based): one at the end of each line that allocates,
/// copies, grows a container, enters an allocating function or keeps a check. A line with none of
/// them has no hint.
pub fn hints(
    analysis: &Analysis,
    path: Option<&str>,
    src: &str,
    from: usize,
    to: usize,
) -> Vec<InlayHint> {
    let mut out = Vec::new();
    let mut run = None;
    for f in &analysis.cost {
        let rows: Vec<&CostLine> = (f.lines.iter())
            .filter(|r| (from..=to).contains(&r.line))
            .collect();
        if rows.is_empty() {
            continue;
        }
        if run.is_none() {
            run = Some(last_run(path, src, analysis));
        }
        let fresh = match &run {
            Some(Found::Fresh(sites)) => Some(sites),
            _ => None,
        };
        for line_rows in rows.chunk_by(|a, b| a.line == b.line) {
            let line = line_rows[0].line;
            let text = line_of_text(src, line - 1);
            let labels: Vec<String> = (line_rows.iter()).map(|r| label(f, r, fresh)).collect();
            let tips: Vec<String> = (line_rows.iter()).map(|r| tooltip(f, r, fresh)).collect();
            out.push(InlayHint {
                position: Position {
                    line: (line - 1) as u32,
                    character: char_col_to_utf16(text, text.chars().count()),
                },
                label: InlayHintLabel::String(labels.join(", ")),
                kind: None,
                text_edits: None,
                tooltip: Some(InlayHintTooltip::String(tips.join("\n"))),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }
    }
    out
}

/// One lens per function that costs anything: `allocates 3, grows 1, copies 2, checks kept 1`,
/// and the blocks of the last run when it is fresh. A stale profile adds one lens saying so, above
/// the first such function. Each lens is `{ line, title }`, `line` 0-based.
pub fn lenses(analysis: &Analysis, path: Option<&str>, src: &str) -> Vec<serde_json::Value> {
    let run = last_run(path, src, analysis);
    let mut out = Vec::new();
    for f in &analysis.cost {
        let total = |verbs: &[&str]| -> usize {
            (f.lines.iter())
                .filter(|r| verbs.contains(&r.verb))
                .map(|r| r.count)
                .sum()
        };
        let mut parts: Vec<String> = [
            ("allocates", total(&["allocates", "enters"])),
            ("grows", total(&["grows"])),
            ("copies", total(&["copies"])),
            ("checks kept", total(&[KEPT])),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(what, n)| format!("{what} {n}"))
        .collect();
        if let Found::Fresh(sites) = &run {
            let blocks: u64 = (f.lines.iter())
                .filter(|r| r.verb != KEPT)
                .map(|r| ran(sites, f, r)[0])
                .sum();
            parts.push(format!("last run: {blocks} blocks"));
        }
        out.push(
            serde_json::json!({ "line": f.line.saturating_sub(1), "title": parts.join(", ") }),
        );
    }
    if let (Found::Stale, Some(first)) = (&run, analysis.cost.first()) {
        out.insert(
            0,
            serde_json::json!({
                "line": first.line.saturating_sub(1),
                "title": "profile stale: the source changed since the last `vyrn run --profile`",
            }),
        );
    }
    out
}
