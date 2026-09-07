//! The form census — RFC-0127, and the rest of RFC-0125 §1.1's `surface`.
//!
//! RFC-0126 priced the `types` factor: 33 constructors of `ast::Type`, one row
//! each, pinned by `tests/surface.rs`. This is the same question asked of
//! everything else the language spells — every statement, expression and pattern
//! form, every declaration, and every keyword and contextual word.
//!
//! # What this pins, and why a test rather than prose
//!
//! `tests/surface.rs` says it for the types and the reason carries: a table in
//! prose beside a table in code is two tables, and the one in prose is the one
//! that drifts. `tests/boundaries.rs`, `tests/lowered.rs` and `tests/refusals.rs`
//! each hold their table the same way.
//!
//! Seven facts are checked:
//!
//! 1. [`the_census_covers_every_form`] reads `Stmt`, `Expr` and `Pattern` out of
//!    `ast.rs` and asserts RFC-0127's two tables list exactly those, in the
//!    declaration order. A form added to the surface therefore fails this test
//!    until it has a cost row and a verdict.
//! 2. [`the_form_census_is_what_the_rfc_records`] recomputes every count in the
//!    form table by the method RFC-0127 §2 states.
//! 3. [`the_declaration_census_is_what_the_rfc_records`] does the same for the
//!    declaration table, whose rows are read out of `Program`'s `Vec` fields —
//!    so a tenth declaration form fails here too.
//! 4. [`the_keyword_census_is_what_the_rfc_records`] reads the lexer's
//!    `keyword_or_ident` map and asserts the RFC's keyword table lists the same
//!    spellings against the same tokens, with the same counts. This is the third
//!    reader of that map: `editor/vscode/test/grammar.test.mjs` is the second.
//! 5. [`the_contextual_words_are_what_the_rfc_records`] does the same for the
//!    words the lexer hands back as identifiers, and asserts the playground's
//!    `CONTEXTUAL` list is a subset of the RFC's rows.
//! 6. [`the_formatter_and_the_lsp_do_not_name_a_form`] pins the two zero
//!    measurements §3.3 reports, because a zero in prose is a claim and a zero
//!    in a test is a fact.
//! 7. [`the_verdicts_are_from_the_closed_set`] holds §4's vocabulary to three
//!    words and asserts the sentence that tallies them.
//!
//! # The method (RFC-0127 §2), stated once and applied here
//!
//! RFC-0126 §2's rule, unchanged: a line whose trimmed text starts with `//` is
//! not code; an item annotated `#[cfg(test)]` is skipped whole; in what is left,
//! the needle counts where the next character is not a letter, digit or
//! underscore. Only the needle differs per table, and each table says which.

use std::path::{Path, PathBuf};

/// The nine files a FORM is stated in, in the RFC's order. A column may be more
/// than one file when one pass is written in two (`vyrn-lower`).
///
/// The `shared` column was `native` until RFC-0125 §3 M4's fourth slice: it read
/// `vyrn-codegen/src/lib.rs`, which was the text-IR emitter AND the lowering both
/// emitters call. The route went; the file stayed, minus the emitter, and so did
/// the column — under the name of what is left in it.
const FORM_COLUMNS: &[(&str, &[&str])] = &[
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("movecheck", &["vyrn-frontend/src/movecheck.rs"]),
    ("own", &["vyrn-frontend/src/own.rs"]),
    (
        "lower",
        &[
            "vyrn-lower/src/lib.rs",
            "vyrn-lower/src/core.rs",
            // The must-use judgment reads the tree a reader wrote, and it
            // moved here with the rule (RFC-0125 §3 M3, the obligation
            // slice). A form it walks is a form this column states.
            "vyrn-lower/src/typed.rs",
        ],
    ),
    ("shared", &["vyrn-codegen/src/lib.rs"]),
    ("wasm", &["vyrn-codegen/src/direct.rs"]),
    ("editor", &["vyrn-frontend/src/symbols.rs"]),
];

/// The seven files a DECLARATION is stated in. Not the same set: a declaration
/// is linked by the loader and selected by the CLI, and three of the nine above
/// never see one.
const DECL_COLUMNS: &[(&str, &[&str])] = &[
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("loader", &["vyrn-frontend/src/loader.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("project", &["vyrn-frontend/src/project.rs"]),
    ("shared", &["vyrn-codegen/src/lib.rs"]),
    ("editor", &["vyrn-frontend/src/symbols.rs"]),
    ("cli", &["vyrn-cli/src/main.rs"]),
];

/// The three files a KEYWORD is stated in. No pass below the parser can see one.
const KEYWORD_COLUMNS: &[(&str, &[&str])] = &[
    ("lexer", &["vyrn-frontend/src/lexer.rs"]),
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("fmt", &["vyrn-frontend/src/fmt.rs"]),
];

/// The four files a CONTEXTUAL word is stated in — the three above and the
/// checker, which is where two of them carry a rule.
const CONTEXTUAL_COLUMNS: &[(&str, &[&str])] = &[
    ("lexer", &["vyrn-frontend/src/lexer.rs"]),
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("fmt", &["vyrn-frontend/src/fmt.rs"]),
];

/// The words the lexer hands back as identifiers and the parser reads by
/// position. The playground's `CONTEXTUAL` list is checked to be a subset, so a
/// word added there without a row here fails; the five it does not carry are
/// named in RFC-0127 §3.4 and are here because the parser or the checker reads
/// them.
const CONTEXTUAL_WORDS: &[&str] = &[
    "read", "modify", "consume", "share", "gen", "test", "bench", "panic", "from", "as", "extern",
    "lazy", "place", "logging", "contract",
];

/// The verdict vocabulary of RFC-0127 §4, which is RFC-0126 §4's. Three words,
/// and no fourth.
const VERDICTS: &[&str] = &["stays", "desugar", "decide"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn compiler_file(rel: &str) -> String {
    let p = repo_root().join("compiler").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn rfc_text() -> String {
    let p = repo_root()
        .join("rfcs")
        .join("RFC-0127-a-form-is-an-arm-in-every-walk.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The file with its comment lines and its `#[cfg(test)]` items removed.
///
/// RFC-0126 §2's rule verbatim. A doc comment that names a form is not a case,
/// and a fixture in a test module is not a pass.
fn code_only(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with("#[cfg(test)]") {
            let mut depth = 0i32;
            let mut open = false;
            let mut j = i;
            while j < lines.len() {
                for ch in lines[j].chars() {
                    if ch == '{' {
                        depth += 1;
                        open = true;
                    } else if ch == '}' {
                        depth -= 1;
                    }
                }
                if open && depth <= 0 {
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if !t.starts_with("//") {
            out.push(lines[i]);
        }
        i += 1;
    }
    out.join("\n")
}

/// How many times `code` names `needle` as itself.
///
/// The trailing character must not continue the identifier, or `Stmt::If` would
/// count every `Stmt::IfLet` and `.tests` every `.tests_run`.
fn mentions(code: &str, needle: &str) -> usize {
    let bytes = code.as_bytes();
    let mut n = 0usize;
    let mut from = 0usize;
    while let Some(at) = code[from..].find(needle) {
        let end = from + at + needle.len();
        let ok = match bytes.get(end) {
            None => true,
            Some(c) => !(c.is_ascii_alphanumeric() || *c == b'_'),
        };
        if ok {
            n += 1;
        }
        from = end;
    }
    n
}

/// One column's code: every file it names, comments and test items removed.
fn column(files: &[&str]) -> String {
    files
        .iter()
        .map(|f| code_only(&compiler_file(f)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn columns(spec: &[(&str, &[&str])]) -> Vec<String> {
    spec.iter().map(|(_, files)| column(files)).collect()
}

/// A count as the RFC's prose writes one: thousands separated by a comma.
fn grouped(n: usize) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The variants of `pub enum <name>` in `ast.rs`, in declaration order.
///
/// The names are the four-space-indented capitalised identifiers of the enum
/// body — the shape every variant in these enums has.
fn variants_of(src: &str, name: &str) -> Vec<String> {
    let head = format!("pub enum {name} {{");
    let start = src
        .find(&head)
        .unwrap_or_else(|| panic!("`{head}` is gone — this test needs a new anchor"));
    let body = &src[start..];
    let end = body
        .find("\n}\n")
        .unwrap_or_else(|| panic!("the end of `pub enum {name}`"));
    let mut out = Vec::new();
    for line in body[..end].lines() {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || rest.starts_with("//") || rest.starts_with('#') {
            continue;
        }
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !ident.is_empty() && ident.starts_with(|c: char| c.is_ascii_uppercase()) {
            out.push(ident);
        }
    }
    assert!(out.len() > 2, "only {} variants of {name}", out.len());
    out
}

/// Every form the census prices: `Stmt`, then `Expr`, then `Pattern`, each in
/// its declaration order and spelled as the code spells it.
fn forms() -> Vec<String> {
    let src = compiler_file("vyrn-frontend/src/ast.rs");
    let mut out = Vec::new();
    for owner in ["Stmt", "Expr", "Pattern"] {
        for v in variants_of(&src, owner) {
            out.push(format!("{owner}::{v}"));
        }
    }
    out
}

/// Every declaration form, read as `Program`'s `Vec` fields in declaration
/// order — which is how a pass reaches one, and so is the needle.
///
/// `Program`'s other three fields are not declarations: two carry the `logging`
/// block's settings and one is the loader's shadow set (RFC-0127 §3.2).
fn declarations() -> Vec<String> {
    let src = compiler_file("vyrn-frontend/src/ast.rs");
    let start = src
        .find("pub struct Program {")
        .expect("`pub struct Program`");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("the end of `pub struct Program`");
    let mut out = Vec::new();
    for line in body[..end].lines() {
        let Some(rest) = line.strip_prefix("    pub ") else {
            continue;
        };
        let Some((name, ty)) = rest.split_once(':') else {
            continue;
        };
        if ty.trim_start().starts_with("Vec<") {
            out.push(name.to_string());
        }
    }
    assert!(
        out.len() > 5,
        "only {} declaration fields on Program",
        out.len()
    );
    out
}

/// Every `"word" => Tok::Name` arm of the lexer's `keyword_or_ident`, in order.
///
/// The same anchor `editor/vscode/test/grammar.test.mjs` reads. A keyword added
/// to the language and not to RFC-0127 fails here.
fn keywords() -> Vec<(String, String)> {
    let src = compiler_file("vyrn-frontend/src/lexer.rs");
    let at = src
        .find("fn keyword_or_ident(")
        .expect("`keyword_or_ident` is gone from lexer.rs — this test needs a new anchor");
    let body = &src[at..src[at..].find("\n}").map(|e| at + e).unwrap_or(src.len())];
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix('"') else {
            continue;
        };
        let Some((word, tail)) = rest.split_once('"') else {
            continue;
        };
        let Some(tok) = tail.split("Tok::").nth(1) else {
            continue;
        };
        let tok: String = tok
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !tok.is_empty() {
            out.push((word.to_string(), tok));
        }
    }
    assert!(out.len() > 15, "only {} keyword arms found", out.len());
    out
}

/// The rows of a markdown table, found by its header line and read to the first
/// line that is not a row. Each row is its trimmed cells.
fn table(text: &str, header: &str) -> Vec<Vec<String>> {
    let start = text
        .find(header)
        .unwrap_or_else(|| panic!("RFC-0127 has no table with header {header:?}"));
    let mut rows = Vec::new();
    for line in text[start..].lines().skip(2) {
        if !line.starts_with("| `") {
            break;
        }
        rows.push(
            line.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect(),
        );
    }
    rows
}

const FORM_HEADER: &str =
    "| form | parser | checker | movecheck | own | lower | shared | wasm | editor | all eight |";
const DECL_HEADER: &str =
    "| declaration | parser | loader | checker | project | shared | editor | cli | all seven |";
const KEYWORD_HEADER: &str = "| keyword | token | lexer | parser | fmt | all three |";
const CONTEXTUAL_HEADER: &str = "| word | lexer | parser | checker | fmt | all four |";
const VERDICT_HEADER: &str = "| form | what it is | RFC | verdict | the desugar, or the reason |";

/// One table's counts against the code, by the method of §2.
///
/// Returns the grand total so the caller can check the prose sentence.
fn check_counts(
    rows: &[Vec<String>],
    spec: &[(&str, &[&str])],
    needle_of: &dyn Fn(&str) -> String,
    label_cells: usize,
) -> usize {
    let code = columns(spec);
    let mut total = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for row in rows {
        assert_eq!(
            row.len(),
            label_cells + spec.len() + 1,
            "a row has {} cells: {row:?}",
            row.len()
        );
        let label = row[0].trim_matches('`');
        let needle = needle_of(label);
        let mut sum = 0usize;
        for (k, (col, _)) in spec.iter().enumerate() {
            let got = mentions(&code[k], &needle);
            let want: usize = row[label_cells + k].parse().expect("a count");
            if got != want {
                wrong.push(format!(
                    "{needle} in {col}: code says {got}, RFC says {want}"
                ));
            }
            sum += got;
        }
        let want_sum: usize = row[label_cells + spec.len()]
            .parse()
            .expect("the row total");
        if sum != want_sum {
            wrong.push(format!(
                "{needle}: the row sums to {sum}, RFC says {want_sum}"
            ));
        }
        total += sum;
    }
    assert!(
        wrong.is_empty(),
        "the census has moved:\n  {}",
        wrong.join("\n  ")
    );
    total
}

/// Both tables list every form `ast.rs` declares, in that order.
///
/// The anti-drift half: a form added to the surface has no cost row and no
/// verdict until somebody writes one, and this test says so by name.
#[test]
fn the_census_covers_every_form() {
    let text = rfc_text();
    let want: Vec<String> = forms().iter().map(|f| format!("`{f}`")).collect();
    let got: Vec<String> = table(&text, FORM_HEADER)
        .iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(
        got, want,
        "RFC-0127's cost table and `ast.rs` list different forms"
    );

    // The verdict table carries the forms and then the declarations, which is
    // the order §4 states them in.
    let mut want_verdicts = want.clone();
    want_verdicts.extend(declarations().iter().map(|d| format!("`{d}`")));
    let got_verdicts: Vec<String> = table(&text, VERDICT_HEADER)
        .iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(
        got_verdicts, want_verdicts,
        "RFC-0127's verdicts and the surface list different rows"
    );

    assert!(
        text.contains(&format!("{} forms", want.len())),
        "the prose should say {} forms",
        want.len()
    );
}

/// Every number in the form table is what the code says today.
#[test]
fn the_form_census_is_what_the_rfc_records() {
    let text = rfc_text();
    let rows = table(&text, FORM_HEADER);
    let total = check_counts(&rows, FORM_COLUMNS, &|l| l.to_string(), 1);
    let sentence = format!("{} mentions in eight files", grouped(total));
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}

/// Every number in the declaration table is what the code says today, and the
/// rows are `Program`'s own `Vec` fields.
#[test]
fn the_declaration_census_is_what_the_rfc_records() {
    let text = rfc_text();
    let rows = table(&text, DECL_HEADER);
    let got: Vec<String> = rows
        .iter()
        .map(|r| r[0].trim_matches('`').to_string())
        .collect();
    assert_eq!(
        got,
        declarations(),
        "RFC-0127's declaration table and `Program` list different fields"
    );
    let total = check_counts(&rows, DECL_COLUMNS, &|l| format!(".{l}"), 1);
    let sentence = format!("{total} mentions in seven files");
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}

/// The keyword table's spellings and tokens are the lexer's own map, and its
/// counts are what the code says today.
#[test]
fn the_keyword_census_is_what_the_rfc_records() {
    let text = rfc_text();
    let rows = table(&text, KEYWORD_HEADER);
    let got: Vec<(String, String)> = rows
        .iter()
        .map(|r| {
            (
                r[0].trim_matches('`').to_string(),
                r[1].trim_matches('`')
                    .trim_start_matches("Tok::")
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        got,
        keywords(),
        "RFC-0127's keyword table and `keyword_or_ident` disagree"
    );
    let kw = keywords();
    let total = check_counts(
        &rows,
        KEYWORD_COLUMNS,
        &|l| {
            let tok = &kw.iter().find(|(w, _)| w == l).expect("a keyword row").1;
            format!("Tok::{tok}")
        },
        2,
    );
    let sentence = format!("{total} mentions in three files");
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}

/// The contextual table lists every word the parser reads by position, and the
/// playground's list is inside it.
#[test]
fn the_contextual_words_are_what_the_rfc_records() {
    let text = rfc_text();
    let rows = table(&text, CONTEXTUAL_HEADER);
    let got: Vec<String> = rows
        .iter()
        .map(|r| r[0].trim_matches('`').to_string())
        .collect();
    assert_eq!(
        got,
        CONTEXTUAL_WORDS
            .iter()
            .map(|w| w.to_string())
            .collect::<Vec<_>>(),
        "RFC-0127's contextual table and this test's list disagree"
    );

    // The playground colours a subset of these, and `tests/contextual_words.rs`
    // holds it equal to the site's. A word added to both without a row here is
    // a word the census cannot see.
    let play = std::fs::read_to_string(repo_root().join("compiler/vyrn-play/src/lib.rs"))
        .expect("the playground crate");
    let anchor = "const CONTEXTUAL: &[&str] = &[";
    let at = play
        .find(anchor)
        .expect("the playground's CONTEXTUAL list is gone — this test needs a new anchor");
    let body = &play[at + anchor.len()..];
    let body = &body[..body.find(']').expect("the list's bracket")];
    for word in body.split('"').skip(1).step_by(2) {
        assert!(
            CONTEXTUAL_WORDS.contains(&word),
            "the playground colours `{word}` and RFC-0127 has no row for it"
        );
    }

    let total = check_counts(&rows, CONTEXTUAL_COLUMNS, &|l| format!("\"{l}\""), 1);
    let sentence = format!("{total} mentions in four files");
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}

/// §3.3's two zeros, as facts rather than as claims.
///
/// The formatter names no form because RFC-0017 formats a token stream, and the
/// LSP binary names one — `Expr::Str`, once, about a module path — because
/// RFC-0006 made it an adapter over the frontend's own answers.
#[test]
fn the_formatter_and_the_lsp_do_not_name_a_form() {
    let all = forms();
    let fmt = code_only(&compiler_file("vyrn-frontend/src/fmt.rs"));
    for f in &all {
        assert_eq!(
            mentions(&fmt, f),
            0,
            "`vyrn fmt` names {f}; RFC-0127 §3.3 says it names no form"
        );
    }

    let lsp_dir = repo_root().join("compiler/vyrn-lsp/src");
    let mut lsp = String::new();
    for e in std::fs::read_dir(&lsp_dir)
        .expect("read vyrn-lsp/src")
        .flatten()
    {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "rs") {
            lsp.push_str(&code_only(
                &std::fs::read_to_string(&p).expect("an lsp file"),
            ));
            lsp.push('\n');
        }
    }
    let named: Vec<(String, usize)> = all
        .iter()
        .map(|f| (f.clone(), mentions(&lsp, f)))
        .filter(|(_, n)| *n > 0)
        .collect();
    assert_eq!(
        named,
        vec![("Expr::Str".to_string(), 1)],
        "RFC-0127 §3.3 says the LSP names `Expr::Str` once and no other form"
    );
}

/// §4's verdict column is three words, and the tally sentence counts them.
#[test]
fn the_verdicts_are_from_the_closed_set() {
    let text = rfc_text();
    let rows = table(&text, VERDICT_HEADER);
    let mut tally = [0usize; 3];
    for row in &rows {
        assert_eq!(
            row.len(),
            5,
            "a verdict row has {} cells: {row:?}",
            row.len()
        );
        let at = VERDICTS
            .iter()
            .position(|v| *v == row[3])
            .unwrap_or_else(|| {
                panic!(
                    "{}: verdict {:?} is not one of {VERDICTS:?}",
                    row[0], row[3]
                )
            });
        tally[at] += 1;
        assert!(
            !row[4].is_empty(),
            "{}: a verdict without a reason is a guess",
            row[0]
        );
    }
    let sentence = format!(
        "Of the {} rows: `desugar` {}, `decide` {}, `stays` {}.",
        rows.len(),
        tally[1],
        tally[2],
        tally[0]
    );
    assert!(
        text.contains(&sentence),
        "the prose should say {sentence:?}"
    );
}

/// The four tables for RFC-0127 §3, printed from the code:
/// `cargo test -p vyrn-cli --test forms -- --ignored --nocapture
/// the_form_census_as_a_table`.
#[test]
#[ignore]
fn the_form_census_as_a_table() {
    let print = |header: &str,
                 labels: &[String],
                 spec: &[(&str, &[&str])],
                 needle_of: &dyn Fn(&str) -> String,
                 extra: &dyn Fn(&str) -> String| {
        let code = columns(spec);
        println!("\n{header}");
        println!("|---{}|", "|---".repeat(spec.len() + 1));
        let mut total = 0usize;
        for l in labels {
            let counts: Vec<usize> = (0..spec.len())
                .map(|k| mentions(&code[k], &needle_of(l)))
                .collect();
            let sum: usize = counts.iter().sum();
            total += sum;
            let cells: Vec<String> = counts.iter().map(|c| c.to_string()).collect();
            println!("| `{l}` |{} {} | {sum} |", extra(l), cells.join(" | "));
        }
        println!("\n{total} mentions in {} files", spec.len());
    };
    let none = |_: &str| String::new();
    print(
        FORM_HEADER,
        &forms(),
        FORM_COLUMNS,
        &|l| l.to_string(),
        &none,
    );
    print(
        DECL_HEADER,
        &declarations(),
        DECL_COLUMNS,
        &|l| format!(".{l}"),
        &none,
    );
    let kw = keywords();
    let words: Vec<String> = kw.iter().map(|(w, _)| w.clone()).collect();
    let tok_of = |l: &str| {
        let t = kw
            .iter()
            .find(|(w, _)| w == l)
            .expect("a keyword")
            .1
            .clone();
        format!("Tok::{t}")
    };
    print(KEYWORD_HEADER, &words, KEYWORD_COLUMNS, &tok_of, &|l| {
        format!(" `{}` |", tok_of(l))
    });
    let ctx: Vec<String> = CONTEXTUAL_WORDS.iter().map(|w| w.to_string()).collect();
    print(
        CONTEXTUAL_HEADER,
        &ctx,
        CONTEXTUAL_COLUMNS,
        &|l| format!("\"{l}\""),
        &none,
    );
}
