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
    InlayHint, InlayHintLabel, InlayHintTooltip, MarkupContent, MarkupKind, Position, Range,
};
use vyrn_frontend::core::Span;
use vyrn_frontend::{Analysis, CostItem, CostLine, FnCost};
use vyrn_lower::lastrun::{self, Found, Sites};

use crate::{char_col_to_utf16, line_of_text};

/// The verb of a check row `why --cost` keeps. A run counts no blocks for it.
const KEPT: &str = "check kept";

/// The verbs of the proof glyph: a check the compiler proved and a call the emitter erases.
/// Its hover's proof is resolved on demand ([`resolve`]).
const PROOFS: [&str; 2] = ["check proved", "call erased"];

/// The glyph of [`PROOFS`].
const PROVED: &str = "⇒";

/// Whether a run counts blocks for a row of `verb`.
fn blocks(verb: &str) -> bool {
    !matches!(verb, KEPT | "check refused" | "call kept") && !PROOFS.contains(&verb)
}

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
const GLYPHS: [(&str, &str, &str); 9] = [
    ("copies", "⧉", "copies"),
    ("allocates", "⊞", "allocates"),
    ("enters", "↘", "enters an allocating function"),
    ("grows", "↗", "may grow"),
    (KEPT, "?", "check kept"),
    ("check refused", "✗", "check refused"),
    ("call kept", "↺", "call kept"),
    (PROOFS[0], PROVED, "check proved"),
    (PROOFS[1], PROVED, "call erased"),
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

/// The link that ends each compact hover. The extension's `vyrn.whyCost` command runs the
/// report on the active file.
pub const WHY_COST: &str = "[vyrn why --cost](command:vyrn.whyCost)";

/// One cost row of the document in the compact style: a verb's glyph on a line, or the blocks
/// the last run made there. [`groups`], the expression hover and `vyrn/costRows` read it.
pub struct Row {
    /// 1-based line.
    pub line: usize,
    /// The glyph with its count in superscript.
    pub glyph: String,
    pub count: usize,
    /// Where each fact of the row stands; empty for the last run's blocks.
    pub spans: Vec<Range>,
    /// A bold title with the glyph and count, a line per noun with its count and reason, the
    /// loops around it and a fix where one is plain. [`WHY_COST`] is not in it.
    pub hover: String,
    /// Whether the row is a proof glyph, whose proofs [`resolve`] adds.
    pub proof: bool,
    /// The bare glyph and the title words; for a verb [`GLYPHS`] lacks and for the last run's
    /// blocks, the row's label and no title.
    mark: String,
    title: &'static str,
    /// Each noun with the range of each of its sites, `None` where no expression spells one;
    /// empty for a row [`GLYPHS`] lacks.
    items: Vec<(CostItem, Vec<Option<Range>>)>,
    depth: u32,
    fix: Option<&'static str>,
}

/// `span` as an LSP range of `src`; `None` for [`Span::NONE`].
fn range(src: &str, span: &Span) -> Option<Range> {
    let at = |(line, col): (usize, usize)| Position {
        line: (line - 1) as u32,
        character: char_col_to_utf16(line_of_text(src, line - 1), col - 1),
    };
    (span.start.1 > 0 && span.end.1 > 0).then(|| Range {
        start: at(span.start),
        end: at(span.end),
    })
}

/// One line of a hover: the count of the item's sites it covers, when more than one, then the
/// item in words.
fn item_line(item: &CostItem, n: usize) -> String {
    match n {
        1 => format!("\n- {}", item.text(1)),
        n => format!("\n- {n}\u{d7} {}", item.text(1)),
    }
}

/// The note on the loops around a fact and the fixes that close a hover.
fn tail(depth: u32, fixes: &[&str]) -> String {
    let mut out = match depth {
        0 => String::new(),
        1 => "\n\nIn a loop.".to_string(),
        d => format!("\n\nIn {d} nested loops."),
    };
    for fix in fixes {
        out += &format!("\n\nFix: {fix}");
    }
    out
}

/// One row as a compact glyph: its glyph and count, and a hover with a bold title, a line per
/// noun with its count and reason, the loops around it and a fix where one is plain. A verb
/// [`GLYPHS`] lacks shows its words.
fn compact(row: &CostLine, src: &str) -> Row {
    let items: Vec<(CostItem, Vec<Option<Range>>)> = (row.items.iter())
        .map(|i| (i.clone(), i.spans.iter().map(|s| range(src, s)).collect()))
        .collect();
    let spans: Vec<Range> = (items.iter())
        .flat_map(|(_, r)| r.iter().flatten().copied())
        .collect();
    let proof = PROOFS.contains(&row.verb);
    let Some(&(_, glyph, title)) = GLYPHS.iter().find(|g| g.0 == row.verb) else {
        let (mark, hover) = (row.label.clone(), row.text.clone());
        return Row {
            line: row.line,
            glyph: mark.clone(),
            count: row.count,
            spans,
            hover,
            proof,
            mark,
            title: "",
            items: Vec::new(),
            depth: 0,
            fix: None,
        };
    };
    let fix =
        (row.verb == "grows" && row.depth > 0).then_some("`reserve` the capacity before the loop.");
    let lines: String = (items.iter()).map(|(i, r)| item_line(i, r.len())).collect();
    let hover = format!(
        "**{glyph} {title} {}**\n{lines}{}",
        row.count,
        tail(row.depth, fix.as_slice())
    );
    Row {
        line: row.line,
        glyph: format!("{glyph}{}", superscript(row.count)),
        count: row.count,
        spans,
        hover,
        proof,
        mark: glyph.to_string(),
        title,
        items,
        depth: row.depth,
        fix,
    }
}

/// The facts of one line whose expressions end at one position, drawn as one hint there, or at
/// the line's end for facts no expression spells.
pub struct Group {
    pub at: Position,
    /// Each verb's glyph with the count of its facts here in superscript.
    pub label: String,
    /// Per verb a bold title with its count and a line per noun, then the loops around them
    /// and the fixes once. [`WHY_COST`] is not in it.
    pub hover: String,
    pub proof: bool,
}

/// The groups of `rows`, line by line: one after each expression a fact is about, where the
/// facts whose expressions end there share it, and one at the line's end for facts no
/// expression spells and rows with no facts.
pub fn groups(rows: &[Row], src: &str) -> Vec<Group> {
    let key = |p: Position| (p.line, p.character);
    let mut out = Vec::new();
    for line in rows.chunk_by(|a, b| a.line == b.line) {
        let mut ends: Vec<Position> = line
            .iter()
            .flat_map(|r| r.spans.iter().map(|s| s.end))
            .collect();
        ends.sort_by_key(|p| key(*p));
        ends.dedup();
        let places = ends.into_iter().map(Some).chain([None]);
        for place in places {
            let here = |r: &Option<Range>| r.map(|r| r.end) == place;
            let (mut label, mut parts, mut proof) = (String::new(), Vec::new(), false);
            let (mut depth, mut fixes) = (0, Vec::new());
            for row in line {
                if row.items.is_empty() {
                    if place.is_none() {
                        label += &row.mark;
                        parts.push(row.hover.clone());
                        proof |= row.proof;
                    }
                    continue;
                }
                let mut count = 0;
                let mut lines = String::new();
                for (item, ranges) in &row.items {
                    let n = ranges.iter().filter(|r| here(r)).count();
                    if n > 0 {
                        count += n;
                        lines += &item_line(item, n);
                    }
                }
                if count == 0 {
                    continue;
                }
                label += &format!("{}{}", row.mark, superscript(count));
                parts.push(format!("**{} {} {count}**\n{lines}", row.mark, row.title));
                proof |= row.proof;
                depth = depth.max(row.depth);
                fixes.extend(row.fix.filter(|f| !fixes.contains(f)));
            }
            if parts.is_empty() {
                continue;
            }
            out.push(Group {
                at: place.unwrap_or_else(|| line_end(src, line[0].line)),
                label,
                hover: format!("{}{}", parts.join("\n\n"), tail(depth, &fixes)),
                proof,
            });
        }
    }
    out
}

/// The compact rows of one line's rows of `f`: one per row, then, after a fresh run, the
/// blocks it made on the line.
fn compact_rows(f: &FnCost, rows: &[&CostLine], run: Option<&Sites>, src: &str) -> Vec<Row> {
    let mut out: Vec<Row> = rows.iter().map(|r| compact(r, src)).collect();
    // A check or a kept call makes no block, so a run counts none for it.
    let blocking: Vec<&&CostLine> = rows.iter().filter(|r| blocks(r.verb)).collect();
    if let Some(run) = run.filter(|_| !blocking.is_empty()) {
        let [blocks, bytes] = (blocking.iter())
            .map(|r| ran(run, f, r))
            .fold([0, 0], |a, b| [a[0] + b[0], a[1] + b[1]]);
        let mark = format!("{LAST_RUN}{blocks}");
        out.push(Row {
            line: rows[0].line,
            glyph: mark.clone(),
            count: blocks as usize,
            spans: Vec::new(),
            hover: format!(
                "**{LAST_RUN} last run {blocks}**\n\n{blocks} blocks, {bytes} bytes on this line \
                 in the last `vyrn run --profile`."
            ),
            proof: false,
            mark,
            title: "",
            items: Vec::new(),
            depth: 0,
            fix: None,
        });
    }
    out
}

/// The compact rows of lines `from..=to` (1-based) of the document at `path`, whose buffer is
/// `src`: by function, then by line.
pub fn rows(
    analysis: &Analysis,
    path: Option<&str>,
    src: &str,
    from: usize,
    to: usize,
) -> Vec<Row> {
    let mut out = Vec::new();
    let mut run = None;
    for f in &analysis.cost {
        let rows: Vec<&CostLine> = (f.lines.iter())
            .filter(|r| (from..=to).contains(&r.line))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let fresh = match run.get_or_insert_with(|| last_run(path, src, analysis)) {
            Found::Fresh(sites) => Some(&*sites),
            _ => None,
        };
        for line_rows in rows.chunk_by(|a, b| a.line == b.line) {
            out.extend(compact_rows(f, line_rows, fresh, src));
        }
    }
    out
}

/// The position just past the last character of 1-based `line` of `src`.
fn line_end(src: &str, line: usize) -> Position {
    let text = line_of_text(src, line - 1);
    Position {
        line: (line - 1) as u32,
        character: char_col_to_utf16(text, text.chars().count()),
    }
}

/// The hints for lines `from..=to` (1-based) that allocate, copy, grow a container, enter an
/// allocating function, keep a check, prove one or erase a call; a line with none of them has
/// no hint. In the compact style a hint stands after each [`Group`], the outermost expression
/// that holds its facts, with the group's glyphs and hover, then [`WHY_COST`]: an editor hovers
/// a hint as its own element. A hint with a proof glyph carries the document's `uri` and its
/// line as `data`, for [`resolve`]. The full style shows only costs, in words, at the end of
/// each line.
pub fn hints(
    analysis: &Analysis,
    uri: &str,
    path: Option<&str>,
    src: &str,
    from: usize,
    to: usize,
) -> Vec<InlayHint> {
    let hint = |position: Position, label: String, tip: String, data| InlayHint {
        position,
        label: InlayHintLabel::String(label),
        kind: None,
        text_edits: None,
        tooltip: Some(InlayHintTooltip::MarkupContent(markdown(tip))),
        padding_left: Some(true),
        padding_right: None,
        data,
    };
    if crate::compact_hints() {
        let rows = rows(analysis, path, src, from, to);
        return (groups(&rows, src).into_iter())
            .map(|g| {
                let data = (g.proof)
                    .then(|| serde_json::json!({ "uri": uri, "line": g.at.line as usize + 1 }));
                hint(g.at, g.label, format!("{}\n\n{WHY_COST}", g.hover), data)
            })
            .collect();
    }
    let mut out = Vec::new();
    let mut run = None;
    for f in &analysis.cost {
        let rows: Vec<&CostLine> = (f.lines.iter())
            .filter(|r| (from..=to).contains(&r.line) && !PROOFS.contains(&r.verb))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let fresh = match run.get_or_insert_with(|| last_run(path, src, analysis)) {
            Found::Fresh(sites) => Some(&*sites),
            _ => None,
        };
        for line_rows in rows.chunk_by(|a, b| a.line == b.line) {
            let labels: Vec<String> = line_rows.iter().map(|r| label(f, r, fresh)).collect();
            let tips: Vec<String> = line_rows.iter().map(|r| tooltip(f, r, fresh)).collect();
            let at = line_rows[0].line;
            out.push(hint(
                line_end(src, at),
                labels.join(", "),
                tips.join("\n\n"),
                None,
            ));
        }
    }
    out
}

/// The proof glyph's hint `hint` with the proofs of its line added to its hover: each removed
/// check's goal and premises, as `vyrn why --proofs` prints them. `proofs` is the traced
/// analysis's ([`Analysis::proofs`]), which the server runs only for this request.
pub fn resolve(mut hint: InlayHint, line: usize, proofs: &[(usize, String)]) -> InlayHint {
    let text: Vec<&str> = (proofs.iter())
        .filter(|(l, _)| *l == line)
        .map(|(_, t)| t.as_str())
        .collect();
    let proof = match text.is_empty() {
        true => "No check on this line rests on a proof the traced walk found.".to_string(),
        false => format!("```\n{}\n```", text.join("\n")),
    };
    if let Some(InlayHintTooltip::MarkupContent(m)) = &mut hint.tooltip {
        m.value = format!("{proof}\n\n{}", m.value);
    }
    hint
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
                    .filter(|r| r.verb != KEPT && !PROOFS.contains(&r.verb))
                    .map(|r| ran(&sites, f, r)[0])
                    .sum();
                let allocates =
                    (f.lines.iter()).any(|r| r.verb != KEPT && !PROOFS.contains(&r.verb));
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
