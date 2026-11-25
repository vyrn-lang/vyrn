//! What each line of the open document costs, as inlay hints, and what the last profile counted,
//! as code lenses.
//!
//! The rows are `vyrn why --cost`'s, read from [`Analysis::cost`], so the words are its
//! words: the label is the row's few plain words, the tooltip its full text in Markdown. When the
//! document's last `vyrn run --profile` matches its source ([`vyrn_lower::lastrun`]), each
//! allocating row also shows the blocks that run made there, and each function the blocks it made.
//! In the compact style ([`crate::compact_hints`]) a row is one glyph of [`GLYPHS`] with its
//! count, and its text moves to the glyph's hover.

use lsp_types::{
    InlayHint, InlayHintLabel, InlayHintLabelPart, InlayHintLabelPartTooltip, InlayHintTooltip,
    MarkupContent, MarkupKind, Position,
};
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

/// One row as an end-of-line label: its few plain words, and after a middle dot the blocks of
/// the last run.
fn label(f: &FnCost, row: &CostLine, run: Option<&Sites>) -> String {
    match run {
        Some(run) if row.verb != KEPT => {
            format!("{} \u{b7} {} blocks", row.label, ran(run, f, row)[0])
        }
        _ => row.label.clone(),
    }
}

/// One row as tooltip Markdown: `why --cost`'s text, the loops it sits in and the run's counts.
fn tooltip(f: &FnCost, row: &CostLine, run: Option<&Sites>) -> String {
    let mut out = row.text.clone();
    match row.depth {
        0 => {}
        1 => out += " (in a loop)",
        d => out += &format!(" (in {d} nested loops)"),
    }
    match run {
        Some(run) if row.verb != KEPT => {
            let [blocks, bytes] = ran(run, f, row);
            out + &format!("\n\nLast run: {blocks} blocks, {bytes} bytes.")
        }
        _ => out,
    }
}

/// Each verb's compact glyph and hover title, keyed by [`CostLine::verb`]: the one table of
/// the editor's cost glyphs. [`LAST_RUN`] marks the blocks of the last profile.
const GLYPHS: [(&str, &str, &str); 7] = [
    ("copies", "⧉", "copies"),
    ("allocates", "⊞", "allocates"),
    ("enters", "↘", "enters an allocating function"),
    ("grows", "↗", "may grow"),
    (KEPT, "?", "check kept"),
    ("check refused", "✗", "check refused"),
    ("call kept", "↺", "call kept"),
];

/// The glyph of the blocks the last run made on a line.
const LAST_RUN: &str = "×";

/// `n` in superscript digits; empty for 1, as a noun of one fact has no `x1`.
fn superscript(n: usize) -> String {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    match n {
        1 => String::new(),
        n => (n.to_string().bytes())
            .map(|d| DIGITS[usize::from(d - b'0')])
            .collect(),
    }
}

fn markdown(value: String) -> MarkupContent {
    MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    }
}

/// One label part: `value`, with the Markdown `tip` as its hover.
fn part(value: String, tip: String) -> InlayHintLabelPart {
    InlayHintLabelPart {
        value,
        tooltip: Some(InlayHintLabelPartTooltip::MarkupContent(markdown(tip))),
        location: None,
        command: None,
    }
}

/// The link that ends each compact hover. The extension's `vyrn.whyCost` command runs the
/// report on the active file.
const WHY_COST: &str = "[vyrn why --cost](command:vyrn.whyCost)";

/// One row as a compact part: its glyph and count, and a hover with a bold title, one line per
/// noun with its reason, the loops around it, a fix where one is plain, and the report's link.
/// A verb [`GLYPHS`] lacks shows its words.
fn compact(row: &CostLine) -> InlayHintLabelPart {
    let Some(&(_, glyph, title)) = GLYPHS.iter().find(|g| g.0 == row.verb) else {
        return part(row.label.clone(), row.text.clone());
    };
    let mut tip = format!("**{glyph} {title} {}**\n", row.count);
    for item in &row.items {
        tip += &format!("\n- {item}");
    }
    match row.depth {
        0 => {}
        1 => tip += "\n\nIn a loop.",
        d => tip += &format!("\n\nIn {d} nested loops."),
    }
    if row.verb == "grows" && row.depth > 0 {
        tip += "\n\nFix: `reserve` the capacity before the loop.";
    }
    part(
        format!("{glyph}{}", superscript(row.count)),
        format!("{tip}\n\n{WHY_COST}"),
    )
}

/// The compact label of one line's rows of `f`: a part per row, then, after a fresh run, the
/// blocks it made on the line.
fn compact_label(f: &FnCost, rows: &[&CostLine], run: Option<&Sites>) -> InlayHintLabel {
    let mut parts: Vec<InlayHintLabelPart> = rows.iter().map(|r| compact(r)).collect();
    // A check or a kept call makes no block, so a run counts none for it.
    let blocking: Vec<&&CostLine> = (rows.iter())
        .filter(|r| !matches!(r.verb, KEPT | "check refused" | "call kept"))
        .collect();
    if let Some(run) = run.filter(|_| !blocking.is_empty()) {
        let [blocks, bytes] = (blocking.iter())
            .map(|r| ran(run, f, r))
            .fold([0, 0], |a, b| [a[0] + b[0], a[1] + b[1]]);
        let tip = format!(
            "**{LAST_RUN} last run {blocks}**\n\n{blocks} blocks, {bytes} bytes on this line in \
             the last `vyrn run --profile`.\n\n{WHY_COST}"
        );
        parts.push(part(format!("{LAST_RUN}{blocks}"), tip));
    }
    for p in parts.iter_mut().skip(1) {
        p.value.insert(0, ' ');
    }
    InlayHintLabel::LabelParts(parts)
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
            let (label, tooltip) = match crate::compact_hints() {
                true => (compact_label(f, line_rows, fresh), None),
                false => {
                    let labels: Vec<String> =
                        line_rows.iter().map(|r| label(f, r, fresh)).collect();
                    let tips: Vec<String> =
                        line_rows.iter().map(|r| tooltip(f, r, fresh)).collect();
                    let tip = InlayHintTooltip::MarkupContent(markdown(tips.join("\n\n")));
                    (InlayHintLabel::String(labels.join(", ")), Some(tip))
                }
            };
            out.push(InlayHint {
                position: Position {
                    line: (line - 1) as u32,
                    character: char_col_to_utf16(text, text.chars().count()),
                },
                label,
                kind: None,
                text_edits: None,
                tooltip,
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }
    }
    out
}

/// The lenses of the last profile. A fresh one puts `last run: N blocks` above each function
/// with a row that makes blocks; a stale one puts one lens saying so above the first function with a
/// row. With no profile, no lens: the line hints say what each line costs. Each lens is
/// `{ line, title }`, `line` 0-based.
pub fn lenses(analysis: &Analysis, path: Option<&str>, src: &str) -> Vec<serde_json::Value> {
    let lens = |f: &FnCost, title: String| serde_json::json!({ "line": f.line.saturating_sub(1), "title": title });
    match last_run(path, src, analysis) {
        Found::Fresh(sites) => (analysis.cost.iter())
            .filter_map(|f| {
                let blocks: u64 = (f.lines.iter())
                    .filter(|r| r.verb != KEPT)
                    .map(|r| ran(&sites, f, r)[0])
                    .sum();
                let allocates = f.lines.iter().any(|r| r.verb != KEPT);
                allocates.then(|| lens(f, format!("last run: {blocks} blocks")))
            })
            .collect(),
        Found::Stale => (analysis.cost.first())
            .map(|f| {
                lens(
                    f,
                    "profile stale: the source changed since the last `vyrn run --profile`".into(),
                )
            })
            .into_iter()
            .collect(),
        Found::Never => Vec::new(),
    }
}
