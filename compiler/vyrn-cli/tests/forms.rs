//! The form census: every statement, expression and pattern form,
//! every declaration, and every keyword and contextual word, priced in compiler
//! lines and pinned in `tests/pins/`. `tests/surface.rs` does the same for the
//! types.
//!
//! The method: a line whose trimmed text starts with `//`
//! is not code; an item annotated `#[cfg(test)]` is skipped whole; in what is
//! left, a needle counts where the next character is not a letter, digit or
//! underscore. Each table names its needle.

mod common;

use std::path::{Path, PathBuf};

/// The files a form is stated in, in the RFC's order. A column is several files
/// when one pass is written in several.
const FORM_COLUMNS: &[(&str, &[&str])] = &[
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("movecheck", &["vyrn-frontend/src/movecheck.rs"]),
    (
        "own",
        &[
            "vyrn-frontend/src/own.rs",
            // The `Owned` type table and the free type predicates.
            "vyrn-frontend/src/declared.rs",
        ],
    ),
    (
        "lower",
        &[
            "vyrn-lower/src/lib.rs",
            "vyrn-lower/src/core.rs",
            // The must-use judgment; a form it walks is a form this column
            // states.
            "vyrn-lower/src/typed.rs",
            // The String accumulator whitelist the builder and the emitter ask.
            "vyrn-lower/src/append.rs",
        ],
    ),
    ("shared", &["vyrn-codegen/src/lib.rs"]),
    ("wasm", &["vyrn-codegen/src/direct.rs"]),
    ("editor", &["vyrn-frontend/src/symbols.rs"]),
];

/// The files a declaration is stated in: the loader links one and the CLI
/// selects one, and some form columns never see one.
const DECL_COLUMNS: &[(&str, &[&str])] = &[
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("loader", &["vyrn-frontend/src/loader.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("project", &["vyrn-frontend/src/project.rs"]),
    ("shared", &["vyrn-codegen/src/lib.rs"]),
    ("editor", &["vyrn-frontend/src/symbols.rs"]),
    ("cli", &["vyrn-cli/src/main.rs"]),
];

/// The files a keyword is stated in. No pass below the parser sees one.
const KEYWORD_COLUMNS: &[(&str, &[&str])] = &[
    ("lexer", &["vyrn-frontend/src/lexer.rs"]),
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("fmt", &["vyrn-frontend/src/fmt.rs"]),
];

/// The keyword files plus the checker, where two contextual words carry a rule.
const CONTEXTUAL_COLUMNS: &[(&str, &[&str])] = &[
    ("lexer", &["vyrn-frontend/src/lexer.rs"]),
    ("parser", &["vyrn-frontend/src/parser.rs"]),
    ("checker", &["vyrn-frontend/src/checker.rs"]),
    ("fmt", &["vyrn-frontend/src/fmt.rs"]),
];

/// The words the lexer hands back as identifiers and the parser reads by
/// position. The playground's `CONTEXTUAL` list must be a subset.
const CONTEXTUAL_WORDS: &[&str] = &[
    "read", "modify", "consume", "gen", "test", "bench", "panic", "from", "as", "extern", "lazy",
    "logging", "contract",
];

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

/// The file with its comment lines and its `#[cfg(test)]` items removed: a doc
/// that names a form is not a case, and a test fixture is not a pass.
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

/// How many times `code` names `needle` as itself: the next character must not
/// continue the identifier, or `Stmt::If` would count every `Stmt::IfLet`.
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

/// The variants of `pub enum <name>` in `ast.rs`, in declaration order: the
/// four-space-indented capitalised identifiers of the enum body.
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

/// `Stmt`, then `Expr`, then `Pattern` variants, in declaration order.
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

/// `Program`'s `Vec` fields, in declaration order: a pass reaches a
/// declaration through one, so the field is the needle. The other fields are
/// not declarations.
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

/// Every `"word" => Tok::Name` row of the lexer's `keywords` table, in order.
/// `editor/vscode/test/grammar.test.mjs` reads the same anchor.
fn keywords() -> Vec<(String, String)> {
    let src = compiler_file("vyrn-frontend/src/lexer.rs");
    let at = src
        .find("\n    keywords {\n")
        .expect("the `keywords` table is gone from lexer.rs — this test needs a new anchor");
    let body = &src[at..src[at..]
        .find("\n    }")
        .map(|e| at + e)
        .unwrap_or(src.len())];
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

/// Pins `tests/pins/<name>.tsv`: a row per label with a count per column and
/// their sum, then a total row. Each label is (leading cells, needle).
fn pin_counts(name: &str, head: &str, labels: &[(String, String)], spec: &[(&str, &[&str])]) {
    let code = columns(spec);
    let mut totals = vec![0usize; spec.len() + 1];
    let mut rows: Vec<String> = Vec::new();
    for (cells, needle) in labels {
        let mut counts: Vec<usize> = code.iter().map(|c| mentions(c, needle)).collect();
        counts.push(counts.iter().sum());
        for (t, n) in totals.iter_mut().zip(&counts) {
            *t += n;
        }
        rows.push(common::pin_row(cells, &counts));
    }
    let mut all = "all".to_string();
    all.push_str(&"\t".repeat(head.matches('\t').count()));
    rows.push(common::pin_row(&all, &totals));
    let cols: Vec<&str> = spec.iter().map(|(c, _)| *c).collect();
    common::pin(name, &format!("{head}\t{}\tall", cols.join("\t")), rows);
}

#[test]
fn the_form_census_matches_its_pin() {
    let labels: Vec<(String, String)> = forms().into_iter().map(|f| (f.clone(), f)).collect();
    pin_counts("forms", "form", &labels, FORM_COLUMNS);
}

#[test]
fn the_declaration_census_matches_its_pin() {
    let labels: Vec<(String, String)> = declarations()
        .into_iter()
        .map(|d| (d.clone(), format!(".{d}")))
        .collect();
    pin_counts("declarations", "declaration", &labels, DECL_COLUMNS);
}

#[test]
fn the_keyword_census_matches_its_pin() {
    let labels: Vec<(String, String)> = keywords()
        .into_iter()
        .map(|(w, t)| (format!("{w}\tTok::{t}"), format!("Tok::{t}")))
        .collect();
    pin_counts("keywords", "keyword\ttoken", &labels, KEYWORD_COLUMNS);
}

#[test]
fn the_contextual_words_match_their_pin() {
    // `tests/contextual_words.rs` holds the playground's list equal to the
    // site's; a word added to both without a row here is invisible to the census.
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
            "the playground colours `{word}` and the census has no row for it"
        );
    }

    let labels: Vec<(String, String)> = CONTEXTUAL_WORDS
        .iter()
        .map(|w| (w.to_string(), format!("\"{w}\"")))
        .collect();
    pin_counts("contextual", "word", &labels, CONTEXTUAL_COLUMNS);
}

/// The formatter names no form because it formats a token stream.
/// The LSP names `Expr::Str` once, about a module path, because it is an
/// adapter over the frontend's answers.
#[test]
fn the_formatter_and_the_lsp_do_not_name_a_form() {
    let all = forms();
    let fmt = code_only(&compiler_file("vyrn-frontend/src/fmt.rs"));
    for f in &all {
        assert_eq!(
            mentions(&fmt, f),
            0,
            "`vyrn fmt` names {f}; it formats tokens and names no form"
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
        "the LSP names `Expr::Str` once and no other form"
    );
}

// What the corpus writes. The census above prices a form in compiler lines; a
// form nobody writes costs that price for nothing. The count is by lexing and
// parsing, never by grepping: a code quote, a comment and a doc are not forms.

/// The corpus buckets, in print order. A fourth bucket, the fenced Vyrn in the
/// committed docs, is built separately.
const CORPUS: &[(&str, &[&str])] = &[
    ("std", &["std"]),
    ("examples+site", &["examples", "site"]),
    ("tests", &["compiler/vyrn-cli/tests"]),
];

fn vyrn_files(dirs: &[&str]) -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    for d in dirs {
        let mut stack = vec![root.join(d)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Every fenced Vyrn block in the committed docs, as one source string each.
fn doc_fences() -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![repo_root().join("docs")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                files.push(p);
            }
        }
    }
    files.sort();
    let open = "```vyrn\n";
    let close = "\n```";
    let mut out = Vec::new();
    for f in files {
        let Ok(src) = std::fs::read_to_string(&f) else {
            continue;
        };
        let mut rest = src.as_str();
        while let Some(a) = rest.find(open) {
            let body = &rest[a + open.len()..];
            let Some(b) = body.find(close) else { break };
            out.push(body[..b].to_string());
            rest = &body[b + close.len()..];
        }
    }
    out
}

/// A constructor's own name, without a match over every constructor.
///
/// `Debug` writes the variant's name first, and this writer refuses the byte
/// after it, which aborts the formatting there. `format!("{e:?}")` would format
/// the whole subtree at every node, and the corpus holds a 35 KB expression tree.
struct Head(String);

impl std::fmt::Write for Head {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            if c.is_ascii_alphanumeric() {
                self.0.push(c);
            } else {
                return Err(std::fmt::Error);
            }
        }
        Ok(())
    }
}

fn head(v: &dyn std::fmt::Debug) -> String {
    let mut h = Head(String::new());
    let _ = std::fmt::write(&mut h, format_args!("{v:?}"));
    h.0
}

vyrn_frontend::body_scope_descent!(FormUse, form_block, form_stmt, form_expr);

struct Counter<'c>(&'c mut std::collections::BTreeMap<String, usize>);

impl Counter<'_> {
    fn bump(&mut self, owner: &str, node: &dyn std::fmt::Debug) {
        let h = head(node);
        if !h.is_empty() {
            *self.0.entry(format!("{owner}::{h}")).or_insert(0) += 1;
        }
    }
}

impl<'a> FormUse<'a> for Counter<'_> {
    fn stmt(&mut self, s: &'a vyrn_frontend::ast::Stmt, _: &std::collections::HashSet<String>) {
        self.bump("Stmt", s);
    }

    fn expr(
        &mut self,
        e: &'a vyrn_frontend::ast::Expr,
        _: &std::collections::HashSet<String>,
    ) -> bool {
        self.bump("Expr", e);
        true
    }

    fn arm_pattern(
        &mut self,
        p: &'a vyrn_frontend::ast::Pattern,
        _: usize,
        _: &std::collections::HashSet<String>,
    ) {
        self.bump("Pattern", p);
    }
}

/// Every keyword, operator and surface desugar one source file writes, counted
/// into `into` off the token stream.
fn count_tokens(tokens: &[vyrn_frontend::lexer::Token], into: &mut Uses) {
    use vyrn_frontend::lexer::{token_name_and_text, Tok};
    for (i, t) in tokens.iter().enumerate() {
        let (kind, text) = token_name_and_text(&t.tok);
        if kind == "keyword" || kind == "punct" {
            *into.entry(format!("tok {text}")).or_insert(0) += 1;
        }
        let next = tokens.get(i + 1);
        let after = next.map(|n| &n.tok);
        let same_line = next.map(|n| n.line == t.line).unwrap_or(false);
        let mut surface = |what: &str| *into.entry(format!("surface {what}")).or_insert(0) += 1;
        match (&t.tok, after) {
            // A tag is an identifier with a string literal against it on the
            // same line: `primary`'s own test.
            (Tok::Ident(n), Some(Tok::Str(_) | Tok::TemplateStr { .. })) if same_line => {
                surface(if n == "vyrn" {
                    "a code quote"
                } else {
                    "a tagged template"
                });
            }
            (Tok::Else, Some(Tok::If)) => surface("else if"),
            (Tok::While, Some(Tok::Let)) => surface("while let"),
            (Tok::Let | Tok::Mut, Some(Tok::Ident(_))) => {
                if matches!(tokens.get(i + 2).map(|n| &n.tok), Some(Tok::LParen)) {
                    surface("a refutable let");
                }
            }
            _ => {}
        }
        let tagged =
            i > 0 && tokens[i - 1].line == t.line && matches!(tokens[i - 1].tok, Tok::Ident(_));
        if matches!(t.tok, Tok::TemplateStr { .. }) && !tagged {
            surface("an interpolated string");
        }
        if let Tok::Ident(w) = &t.tok {
            // Only in the position the parser reads it: a binding named `read`
            // is a name, not the capability.
            let contextual = match w.as_str() {
                "read" | "modify" | "consume" => {
                    matches!(after, Some(Tok::Ident(_) | Tok::Vself | Tok::Fn))
                }
                "gen" | "extern" => matches!(after, Some(Tok::Fn)),
                "test" | "bench" => matches!(after, Some(Tok::Str(_))),
                "contract" => {
                    matches!(after, Some(Tok::Ident(_)))
                        && matches!(tokens.get(i + 2).map(|n| &n.tok), Some(Tok::LBrace))
                }
                "logging" => matches!(after, Some(Tok::LBrace)),
                "lazy" => matches!(after, Some(Tok::Ident(_))),
                "from" => i > 0 && matches!(after, Some(Tok::Str(_) | Tok::Ident(_))),
                "as" => i > 0 && matches!(after, Some(Tok::Ident(_))),
                "panic" => matches!(after, Some(Tok::LParen)),
                _ => false,
            };
            if contextual {
                *into.entry(format!("word {w}")).or_insert(0) += 1;
            }
        }
    }
}

/// Counts one source file. Returns `false`, counting nothing of the forms, when
/// the file does not lex or parse.
///
/// `impls` is not walked: `parse_accum` flattens every impl method into
/// `functions`, so walking both would count each body twice. A declaration on
/// `line` 0 is injected by the parser (`loader::is_injected`'s rule).
fn count_source(src: &str, into: &mut Uses) -> bool {
    let Ok(tokens) = vyrn_frontend::lexer::lex(src) else {
        return false;
    };
    count_tokens(&tokens, into);
    let (program, errors) = vyrn_frontend::parser::parse_accum(tokens);
    if !errors.is_empty() {
        return false;
    }
    let written: Vec<(&str, usize)> = vec![
        ("imports", program.imports.len()),
        (
            "type_decls",
            program.type_decls.iter().filter(|t| t.line != 0).count(),
        ),
        ("functions", program.functions.len()),
        ("protocols", program.protocols.len()),
        ("contracts", program.contracts.len()),
        ("impls", program.impls.len()),
        ("globals", program.globals.len()),
        ("tests", program.tests.len()),
        ("benches", program.benches.len()),
    ];
    for (field, n) in written {
        if n > 0 {
            *into.entry(format!("decl {field}")).or_insert(0) += n;
        }
    }
    let mut c = Counter(into);
    let mut locals = std::collections::HashSet::new();
    for f in &program.functions {
        form_block(&f.body, &mut locals, &mut c);
    }
    for t in &program.tests {
        form_block(&t.body, &mut locals, &mut c);
    }
    for b in &program.benches {
        form_block(&b.body, &mut locals, &mut c);
    }
    for g in &program.globals {
        form_expr(&g.init, &locals, &mut c);
    }
    for t in &program.type_decls {
        if t.line != 0 {
            if let Some(p) = &t.predicate {
                form_expr(p, &locals, &mut c);
            }
        }
    }
    true
}

/// Label -> how many times the corpus writes it.
type Uses = std::collections::BTreeMap<String, usize>;

/// One `Uses` per bucket, and (files parsed, files seen) per bucket.
fn corpus_uses() -> (Vec<(&'static str, Uses)>, Vec<(usize, usize)>) {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for (label, dirs) in CORPUS {
        let mut uses = Uses::new();
        let files = vyrn_files(dirs);
        let mut ok = 0usize;
        for p in &files {
            let Ok(src) = std::fs::read_to_string(p) else {
                continue;
            };
            if count_source(&src, &mut uses) {
                ok += 1;
            }
        }
        seen.push((ok, files.len()));
        out.push((*label, uses));
    }
    let fences = doc_fences();
    let mut uses = Uses::new();
    let mut ok = 0usize;
    for f in &fences {
        // A fence is usually a declaration, but the API docs also show a bare
        // statement, which needs a body.
        if count_source(f, &mut uses) || count_source(&format!("fn __d() {{\n{f}\n}}"), &mut uses) {
            ok += 1;
        }
    }
    seen.push((ok, fences.len()));
    out.push(("docs", uses));
    (out, seen)
}

fn use_labels() -> Vec<String> {
    let mut out = forms();
    out.extend(declarations().iter().map(|d| format!("decl {d}")));
    out.extend(keywords().iter().map(|(w, _)| format!("tok {w}")));
    let mut puncts: Vec<String> = vyrn_frontend::lexer::PUNCT_SPELLINGS
        .iter()
        .map(|p| format!("tok {p}"))
        .collect();
    puncts.sort();
    puncts.dedup();
    out.extend(puncts);
    out.extend(CONTEXTUAL_WORDS.iter().map(|w| format!("word {w}")));
    out.extend(SURFACE_DESUGARS.iter().map(|w| format!("surface {w}")));
    out
}

/// The surface forms the parser rewrites into something else, so only the
/// token stream can count them. Spelled as `count_tokens` labels them.
const SURFACE_DESUGARS: &[&str] = &[
    "an interpolated string",
    "a tagged template",
    "a code quote",
    "else if",
    "while let",
    "a refutable let",
];

/// A form in the first set costs its price for nobody; a form in the second is
/// kept alive by the suite that tests it. Both sets are pinned so a row that
/// gains its first use moves the record with it.
#[test]
fn nothing_in_the_corpus_writes_these() {
    let (uses, seen) = corpus_uses();
    // The docs bucket is exempt: `vyrn doc` prints signatures, and a `fn` with
    // no body does not parse, so a docs-only row is a weak claim.
    for (i, (ok, all)) in seen.iter().take(CORPUS.len()).enumerate() {
        assert_eq!(ok, all, "a file in the {} bucket did not parse", uses[i].0);
    }
    let total = |label: &str| -> usize {
        uses.iter()
            .map(|(_, u)| u.get(label).copied().unwrap_or(0))
            .sum()
    };
    let outside_tests = |label: &str| -> usize {
        uses.iter()
            .filter(|(b, _)| *b != "tests")
            .map(|(_, u)| u.get(label).copied().unwrap_or(0))
            .sum()
    };
    let mut zero = Vec::new();
    let mut tests_only = Vec::new();
    for l in use_labels() {
        if total(&l) == 0 {
            zero.push(l);
        } else if outside_tests(&l) == 0 {
            tests_only.push(l);
        }
    }
    assert_eq!(
        (zero.clone(), tests_only.clone()),
        (
            ZERO_IN_THE_CORPUS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            ONLY_A_TEST_WRITES_THESE
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        ),
        "the corpus's use of the surface has moved; update the list with it"
    );
}

/// Nothing in `std/`, `examples/`, `site/`, the CLI's fixtures or the docs
/// writes these. Empty: everything the language spells is written.
const ZERO_IN_THE_CORPUS: &[&str] = &[];

/// Only a compiler test writes these. Empty: no form is kept alive by its own
/// fixture.
const ONLY_A_TEST_WRITES_THESE: &[&str] = &[];
