//! Generation machinery: the splice rule, the
//! `Code` rope, its renderer and span table, the `lex` token literal, the span
//! `syntaxParser` answers, the sandbox's
//! path rule, `moduleInterface`'s reflection, and the seam the driver installs
//! the generation engine through. Nothing here walks a tree: each function
//! takes a resolver, a path or a source, and returns an `Expr` or a `String`.

use crate::ast::{Expr, Id, Program};
use crate::diagnostics::Diagnostic;
use std::collections::HashMap;
use std::sync::Arc;

/// A 1-based line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// Who wrote a span of generated code. A quote's text is written by the module
/// that holds the `vyrn"..."`, at the part's position there; a `String`, number
/// or `Bool` a hole turns into code is the call site's data. A generator cannot
/// name an author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    File { key: Arc<str>, at: Pos },
    CallSite,
}

/// Where a diagnostic inside a span shows: a path relative to the importing
/// module and a position (`at`, `codeAt`). It never changes the author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub path: Arc<str>,
    pub at: Pos,
}

/// What a span holds, beyond its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Code: a quote's text, which a hole re-indents, or the call site's data.
    Code,
    /// Input `codeAt` copied, byte for byte: never re-indented.
    Copied,
    /// One report `diag` wrote, its text `<severity> <anchor> <message>`: the
    /// loader reads it as a diagnostic, and only from a span of this kind. It
    /// writes no text into the module.
    Diag,
}

/// One span of a `Code` value: its text, its author, where its errors show,
/// and its kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub author: Author,
    pub shown: Option<Shown>,
    pub kind: Kind,
}

/// A `Code` value: a rope of spans. Concatenation is a node, so a loop of
/// `acc = acc + piece` is linear.
#[derive(Debug)]
pub enum Rope {
    Leaf(Vec<Span>),
    Cat(Arc<Rope>, Arc<Rope>),
}

impl Rope {
    /// The spans in order. The walk keeps its own stack: a loop's accumulator is
    /// a left spine as deep as the loop ran.
    pub fn spans(&self) -> Vec<&Span> {
        let mut out = Vec::new();
        let mut todo = vec![self];
        while let Some(r) = todo.pop() {
            match r {
                Rope::Leaf(ss) => out.extend(ss),
                Rope::Cat(a, b) => {
                    todo.push(b);
                    todo.push(a);
                }
            }
        }
        out
    }
}

/// One row of a generated module's span table: the byte offset in the module's
/// text where a span starts, its author, and where its errors show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRow {
    pub offset: usize,
    pub author: Author,
    pub shown: Option<Shown>,
}

/// One report a generator returned: the byte offset in the module's text it
/// stands at, and its [`Kind::Diag`] text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub offset: usize,
    pub line: String,
}

/// What a generator wrote: the module's text, its span table and its reports,
/// each sorted by offset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Written {
    pub source: String,
    pub spans: Vec<SpanRow>,
    pub reports: Vec<Report>,
}

impl Written {
    /// The text of `rope`'s code spans, one row per non-empty span, and its
    /// reports.
    pub fn code(rope: &Rope) -> Written {
        let mut w = Written::default();
        for s in rope.spans() {
            let offset = w.source.len();
            match s.kind {
                Kind::Diag => w.reports.push(Report {
                    offset,
                    line: s.text.clone(),
                }),
                _ if s.text.is_empty() => {}
                _ => {
                    w.spans.push(SpanRow {
                        offset,
                        author: s.author.clone(),
                        shown: s.shown.clone(),
                    });
                    w.source.push_str(&s.text);
                }
            }
        }
        w
    }
}

/// Renders a `Code` value to source text (`render`): the text of its code
/// spans. A report writes none.
pub fn render_code(rope: &Rope) -> String {
    let spans = rope.spans().into_iter();
    spans
        .filter(|s| s.kind != Kind::Diag)
        .map(|s| s.text.as_str())
        .collect()
}

/// A value in a code-quote hole, as the splice rule reads it.
///
/// The six cases are the six `vyrn_codegen::TAG_*` values, because a compiled
/// generator names them across the wall: a seventh on either side would be a
/// tag the other cannot handle.
#[derive(Debug, Clone)]
pub enum Spliced {
    Str(String),
    Code(Arc<Rope>),
    Bool(bool),
    /// Any integer width, sign-extended to 64 bits; `signed` picks the decimal
    /// rendering.
    Int {
        v: i64,
        signed: bool,
    },
    F64(f64),
    F32(f32),
}

/// Applies the splice rule to a value in a hole: a `Code` or `Name` splices
/// verbatim, keeping its authors, and a `String`, number or `Bool` becomes one
/// literal of the call site's. A quote span of a multi-line `Code` is
/// re-indented to `indent`, the leading whitespace of the hole's line; a span
/// `codeAt` copied never is.
///
/// `indent` ends in a comment prefix when the hole stands in a line or doc
/// comment (the parser's `continuation`). There every value is plain text of
/// the call site's: a `String` as written, a `Code` as its text, a number or
/// `Bool` as it prints, and each newline continues the comment with `indent`,
/// so the text never leaves it.
///
/// The wasm generation engine applies it host-side, so the escaping and float
/// formatting have one implementation.
pub fn gen_code_splice(val: &Spliced, indent: &str) -> Result<Arc<Rope>, String> {
    let text = |s: String| Ok(Arc::new(Rope::Leaf(vec![Span::data(s)])));
    if !indent.trim().is_empty() {
        let plain = match val {
            Spliced::Str(s) => s.clone(),
            Spliced::Code(rope) => rope.spans().iter().map(|s| s.text.as_str()).collect(),
            scalar => gen_code_splice(scalar, "")?
                .spans()
                .iter()
                .map(|s| s.text.as_str())
                .collect(),
        };
        let plain = plain.replace("\r\n", "\n").replace('\r', "\n");
        return text(plain.replace('\n', &format!("\n{indent}")));
    }
    match val {
        Spliced::Code(rope) if indent.is_empty() => Ok(rope.clone()),
        Spliced::Code(rope) => Ok(Arc::new(Rope::Leaf(reindented(rope, indent)))),
        Spliced::Str(s) => text(escape_string_literal(s)),
        Spliced::Int { v, signed } => text(if *signed {
            v.to_string()
        } else {
            (*v as u64).to_string()
        }),
        // `NaN` and `inf` have no Vyrn literal, so a non-finite value fails here,
        // not later as a module that cannot parse.
        Spliced::F64(f) if f.is_finite() => text(splice_float(format!("{f}"))),
        Spliced::F32(f) if f.is_finite() => text(splice_float(format!("{f}"))),
        Spliced::F64(f) => Err(format!(
            "cannot splice non-finite float {f} into a code quote"
        )),
        Spliced::F32(f) => Err(format!(
            "cannot splice non-finite float {f} into a code quote"
        )),
        Spliced::Bool(b) => text(b.to_string()),
    }
}

/// The spans of `rope` with `indent` after each newline of a quote span that
/// another line follows. A blank line stays blank.
fn reindented(rope: &Rope, indent: &str) -> Vec<Span> {
    let spans = rope.spans();
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for (i, s) in spans.iter().enumerate() {
        let quote = s.kind == Kind::Code && matches!(s.author, Author::File { .. });
        if !quote {
            out.push((*s).clone());
            continue;
        }
        // The first character after this span, which decides a final newline.
        let after = spans[i + 1..].iter().find_map(|n| n.text.chars().next());
        let mut text = String::with_capacity(s.text.len());
        let mut chars = s.text.chars().peekable();
        while let Some(c) = chars.next() {
            text.push(c);
            let next = chars.peek().copied().or(after);
            if c == '\n' && next.is_some_and(|n| n != '\n') {
                text.push_str(indent);
            }
        }
        out.push(Span {
            text,
            ..(*s).clone()
        });
    }
    out
}

/// Returns `s` as a `Name` span, or why it is not one: a `Name` is one
/// non-keyword Vyrn identifier.
pub fn name_span(s: &str) -> Result<Span, String> {
    match is_bare_identifier(s) {
        true => Ok(Span::data(s.to_string())),
        false => Err(format!("`name` needs one identifier, and {s:?} is not one")),
    }
}

/// Returns the `Diag` span of one report, `<severity> <anchor> <message>`. The
/// loader splits it at whitespace, and a report is one line.
pub fn diag_span(severity: &str, anchor: &str, message: &str) -> Result<Span, String> {
    let line = format!("{severity} {anchor} {message}");
    if line.contains(['\n', '\r']) {
        return Err(format!("a report is one line, and {line:?} is not"));
    }
    Ok(Span {
        kind: Kind::Diag,
        ..Span::data(line)
    })
}

/// Returns what `syntaxParser` answers for the grammar at `path`, whose `bytes` the
/// run read, `None` when it could not: the parser as code by module `key`, or a report
/// at the place the grammar goes wrong.
pub fn syntax_parser_span(key: &str, path: &str, bytes: Option<&[u8]>) -> Result<Span, String> {
    let report = |at: String, sentence: &str| diag_span("error", &at, &sentence.replace('\n', " "));
    let Some(bytes) = bytes else {
        return report(format!("{path}:1:1"), "cannot read this grammar");
    };
    let Ok(src) = std::str::from_utf8(bytes) else {
        return report(format!("{path}:1:1"), "a grammar is UTF-8 text");
    };
    match vyrn_syntax::load(src).and_then(|g| vyrn_syntax::parser(&g)) {
        Ok(text) => Ok(Span {
            text,
            author: Author::File {
                key: Arc::from(key),
                at: Pos { line: 1, col: 1 },
            },
            shown: None,
            kind: Kind::Code,
        }),
        Err(e) => report(format!("{path}:{}:{}", e.line, e.col), &e.sentence),
    }
}

/// Returns the span `codeAt` copies: `text`, which must stand at `line:col`
/// (1-based) of `bytes`, the input at resolver key `key` that the generator
/// read, written by that input. `path` is the generator's spelling, which its
/// errors show under.
pub fn copied_span(
    key: &str,
    path: &str,
    bytes: &[u8],
    (line, col): (usize, usize),
    text: &str,
) -> Result<Span, String> {
    let at = Pos { line, col };
    let held = String::from_utf8_lossy(bytes);
    let start = held
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let rest = &held[start.min(held.len())..];
    let from = rest
        .char_indices()
        .nth(col.saturating_sub(1))
        .map(|(i, _)| i);
    match from.is_some_and(|i| rest[i..].starts_with(text)) && line >= 1 && col >= 1 {
        true => Ok(Span {
            text: text.to_string(),
            author: Author::File {
                key: Arc::from(key),
                at,
            },
            shown: Some(Shown {
                path: Arc::from(path),
                at,
            }),
            kind: Kind::Copied,
        }),
        false => Err(format!(
            "`codeAt` was given text that `{path}` does not hold at {line}:{col}"
        )),
    }
}

/// `rope` with every span's errors shown at `path:line:col`: `at` moves where
/// an error shows, never the author.
pub fn shown_at(rope: &Rope, path: &str, (line, col): (usize, usize)) -> Rope {
    let shown = Shown {
        path: Arc::from(path),
        at: Pos { line, col },
    };
    Rope::Leaf(
        rope.spans()
            .into_iter()
            .map(|s| Span {
                shown: Some(shown.clone()),
                ..s.clone()
            })
            .collect(),
    )
}

/// Returns shortest-roundtrip float digits as plain decimal text. `Display`
/// never uses an exponent, which the lexer cannot read, and an integral value
/// gets `.0` so it lexes as a float.
fn splice_float(digits: String) -> String {
    if digits.contains('.') {
        digits
    } else {
        format!("{digits}.0")
    }
}

/// Escapes a `String` into a Vyrn string literal, quotes included, with the
/// escapes the lexer decodes (`\n \t \r \" \\`).
fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // `{` is an ordinary character: a backslash before it is already doubled,
            // so it cannot open a hole.
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Returns whether `s` is one non-keyword Vyrn identifier, decided by the
/// lexer.
fn is_bare_identifier(s: &str) -> bool {
    match crate::lexer::lex(s) {
        Ok(toks) => {
            matches!(toks.first().map(|t| &t.tok), Some(crate::lexer::Tok::Ident(n)) if n == s)
                && toks.len() == 2 // the identifier, then EOF
        }
        Err(_) => false,
    }
}

/// Returns the `lex` token list as an `Array<Token>` record literal, which the
/// wasm generation engine encodes for the guest.
pub fn gen_lex_tokens_lit(source: &str) -> Expr {
    Expr::ArrayLit {
        id: Id::NEW,
        elems: lexed(source)
            .into_iter()
            .map(|(kind, text, line, col)| Expr::StructLit {
                id: Id::NEW,
                name: "Token".to_string(),
                fields: vec![
                    ("kind".to_string(), Expr::str(kind)),
                    ("text".to_string(), Expr::str(text)),
                    ("line".to_string(), Expr::int(line)),
                    ("col".to_string(), Expr::int(col)),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// Returns the `codeRuns` list as an `Array<CodeRun>` record literal, which the
/// wasm generation engine encodes for the guest. A source that does not lex has
/// no runs, so it shows with no colour.
pub fn gen_code_runs_lit(source: &str) -> Expr {
    let runs = crate::classify::code_runs(source).unwrap_or_default();
    Expr::ArrayLit {
        id: Id::NEW,
        elems: (runs.into_iter())
            .map(|r| Expr::StructLit {
                id: Id::NEW,
                name: "CodeRun".to_string(),
                fields: vec![
                    ("start".to_string(), Expr::int(r.start as i64)),
                    ("stop".to_string(), Expr::int(r.stop as i64)),
                    ("class".to_string(), Expr::str(r.class.css())),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// Returns the `vyrnModule` or `vyrnType` answer as an `Array<VyrnNode>` record
/// literal, which the wasm generation engine encodes for the guest.
pub fn gen_vyrn_nodes_lit(source: &str, start: crate::recognizer::Start) -> Expr {
    let ints = |ix: Vec<usize>| Expr::ArrayLit {
        id: Id::NEW,
        elems: ix.into_iter().map(|i| Expr::int(i as i64)).collect(),
        line: 0,
    };
    Expr::ArrayLit {
        id: Id::NEW,
        elems: (crate::reader::read(source, start).into_iter())
            .map(|n| Expr::StructLit {
                id: Id::NEW,
                name: "VyrnNode".to_string(),
                fields: vec![
                    ("rule".to_string(), Expr::str(n.rule)),
                    ("start".to_string(), Expr::int(n.start as i64)),
                    ("stop".to_string(), Expr::int(n.stop as i64)),
                    ("kids".to_string(), ints(n.kids)),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// Runs the lexer over `source` as `(kind, text, line, col)` rows: the one
/// place the `lex` builtin's token list is decided.
pub(crate) fn lexed(source: &str) -> Vec<(String, String, i64, i64)> {
    match crate::lexer::lex(source) {
        Ok(toks) => toks
            .iter()
            .filter(|t| !matches!(t.tok, crate::lexer::Tok::Eof))
            .map(|t| {
                let (kind, text) = crate::lexer::token_name_and_text(&t.tok);
                (kind, text, t.line as i64, t.col as i64)
            })
            .collect(),
        Err(d) => {
            // Unlexable input is one `error` token at the diagnostic's position.
            vec![("error".to_string(), d.message, d.line as i64, d.col as i64)]
        }
    }
}

impl Span {
    /// Text the call site supplied as data, with no position.
    pub fn data(text: String) -> Span {
        Span {
            text,
            author: Author::CallSite,
            shown: None,
            kind: Kind::Code,
        }
    }
}

/// One generation read: the resolved key (a file, or a directory with a
/// trailing `/`) and its content, `None` when the read failed. A failure is
/// recorded too: "0 examples" stops being right when `examples/` appears.
pub type GenRead = (String, Option<Vec<u8>>);

/// A generator's result: what it wrote, and the inputs it read, which the
/// loader folds into the cache key.
pub struct GenOutput {
    pub written: Written,
    pub reads: Vec<GenRead>,
}

/// Everything a generation run needs from the loader.
pub struct GenInputs<'a> {
    /// The engine that runs the generator, and every generator its loads
    /// reach; `None` fails each run.
    pub engine: Option<&'a GenEngine>,
    pub resolver: &'a dyn crate::loader::ModuleResolver,
    /// The load options (std root, manifest aliases), so `moduleInterface` can
    /// link the reflected module's imports.
    pub opts: &'a crate::loader::LoadOptions,
    /// The key of the module that holds the generator.
    pub module: String,
    /// The importing module's directory: the base for relative paths.
    pub importer_dir: String,
    /// Resolved path prefixes the generator may read under: its constant path
    /// arguments. Empty means no filesystem access.
    pub allowed: Vec<String>,
    /// The constant path arguments that name a manifest dependency, with the
    /// module key the import map resolves each to, so a read reaches
    /// the pinned bytes. Each key is also in `allowed`, so an alias is a second
    /// spelling of a declared root, not a new root.
    pub aliased: Vec<(String, String)>,
    /// Step budget and output-size cap.
    pub fuel: u64,
    pub max_output: usize,
    /// A fingerprint of the generator's module closure (its keys and content
    /// hashes), or `None` when the closure holds a generated module no resolver
    /// can re-read. An engine that caches compiled generators keys on it instead
    /// of hashing the whole program.
    pub sources_fingerprint: Option<String>,
    /// The `TypeArg` literal a derived-code generator's one parameter
    /// receives; `None` for an import target.
    pub type_arg: Option<Expr>,
    /// Whether the program passed its check. An unchecked program runs only
    /// from a compiled module the engine already holds for
    /// `sources_fingerprint`; otherwise the engine answers
    /// [`GenError::Unchecked`].
    pub checked: bool,
}

/// Resolves a mediated path argument against the importer's directory and
/// checks that it stays under one of the generator's declared input roots.
/// Returns the resolver key, or the scoping trap message.
///
/// An argument in `aliased` resolves to the key the loader gave
/// that manifest dependency: a lock-pinned remote key or a path under the
/// manifest's directory, which path arithmetic cannot reach. The input-root
/// check still decides on the resolved key. The wasm generation engine
/// mediates its host imports with this same rule.
pub fn gen_scoped_path(
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    arg: &str,
) -> Result<String, String> {
    let resolved = match aliased.iter().find(|(spelling, _)| spelling == arg) {
        Some((_, key)) => key.clone(),
        None => {
            let joined = if importer_dir.is_empty() {
                arg.to_string()
            } else {
                format!("{importer_dir}/{arg}")
            };
            crate::loader::normalize(&joined)
        }
    };
    let ok = allowed
        .iter()
        .any(|root| resolved == *root || resolved.starts_with(&format!("{root}/")));
    if !ok {
        return Err(format!(
            "generator read `{arg}` escapes its declared inputs ({}) — a generator may only \
             read under its constant path arguments",
            allowed.join(", ")
        ));
    }
    Ok(resolved)
}

/// Serves `moduleInterface(path)` and `moduleCode(path)`, as `kind` picks:
/// reads the module, links it to follow its type closure, and builds its
/// literal. Every module the link touched is appended to `reads`, so editing
/// a closure type's defining file misses the generator cache. The wasm
/// generation engine serves both imports from here.
pub fn gen_module_lit(
    kind: crate::schema_reflect::Reflect,
    engine: Option<&GenEngine>,
    resolver: &dyn crate::loader::ModuleResolver,
    opts: &crate::loader::LoadOptions,
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    reads: &mut Vec<GenRead>,
    path: &str,
) -> Result<Expr, String> {
    // Resolve like a module specifier (`.vyrn` appended), scoped like
    // `readFile`. A manifest dependency keeps its spelling: its target already
    // carries the extension.
    let spec = if path.ends_with(".vyrn")
        || path.ends_with(".json")
        || aliased.iter().any(|(spelling, _)| spelling == path)
    {
        path.to_string()
    } else {
        format!("{path}.vyrn")
    };
    let resolved = gen_scoped_path(importer_dir, allowed, aliased, &spec)?;
    let source = resolver.read(&resolved).map_err(|e| {
        format!(
            "{} {}: {e}",
            kind.builtin(),
            crate::trap::io_at("readerr", &path)
        )
    })?;
    reads.push((resolved.clone(), Some(source.clone().into_bytes())));

    linked_module_lit(
        kind,
        engine,
        resolver,
        opts,
        importer_dir,
        path,
        &resolved,
        &source,
        reads,
    )
    .map_err(|diags| {
        let d = diags.first();
        let where_ = d
            .and_then(|d| d.file.clone())
            .map(|f| format!(" ({f})"))
            .unwrap_or_default();
        let msg = d
            .map(|d| d.message.clone())
            .unwrap_or_else(|| "load failed".to_string());
        format!("{} `{path}`{where_}: {msg}", kind.builtin())
    })
}

/// Links `source` (the module at `resolved`) to follow its type closure and
/// builds the literal `kind` names, as its builtin does. `path` is the
/// spelling the reflected module's own types keep. Every other module the
/// link read is appended to `reads`. A module that does not link yields the
/// loader's diagnostics.
pub fn linked_module_lit(
    kind: crate::schema_reflect::Reflect,
    engine: Option<&GenEngine>,
    resolver: &dyn crate::loader::ModuleResolver,
    opts: &crate::loader::LoadOptions,
    importer_dir: &str,
    path: &str,
    resolved: &str,
    source: &str,
    reads: &mut Vec<GenRead>,
) -> Result<Expr, Vec<Diagnostic>> {
    // Link the module so the closure walk sees types declared in its imports.
    // A recording resolver adds every file the link reads to the cache inputs.
    let rec = crate::loader::RecordingResolver::new(resolver);
    let program = crate::loader::load(source, resolved, opts, &rec, engine)?;
    // The link's reads, kept as text for the origin index: the AST has no name
    // columns, so the lexer supplies them (see `Origins`).
    let mut origin_src: Vec<(Option<String>, String, String)> =
        vec![(None, resolved.to_string(), source.to_string())];
    for (p, s) in rec.into_reads() {
        // The root module was recorded above.
        if p != resolved {
            origin_src.push((Some(p.clone()), p.clone(), s.clone()));
            reads.push((p, Some(s.into_bytes())));
        }
    }

    // One import specifier per declaring module, so a generator can
    // import a closure type from the module that declares it. The reflected
    // module's own types keep the generator's argument spelling.
    let mut specifiers: HashMap<Option<String>, String> = HashMap::new();
    specifiers.insert(None, path.to_string());
    for t in &program.type_decls {
        if let Some(key) = &t.module {
            specifiers.entry(Some(key.clone())).or_insert_with(|| {
                crate::loader::import_specifier(importer_dir, key, opts.std_root.as_deref())
            });
        }
    }
    let origins = crate::schema_reflect::Origins::new(
        origin_src
            .iter()
            .map(|(k, f, s)| (k.clone(), f.as_str(), s.as_str())),
    );
    Ok(crate::schema_reflect::module_lit(
        &program,
        &specifiers,
        &origins,
        kind,
    ))
}

/// Why a generation run failed.
pub enum GenError {
    /// The lowering's judgments refused the generator's own program; never
    /// empty.
    Refused(Vec<Diagnostic>),
    /// The run failed: a trap, a decline, or no engine.
    Failed(String),
    /// The engine holds no compiled module for an unchecked program; the caller
    /// checks it and runs again.
    Unchecked,
}

/// The generation engine. It judges the generator's type-checked program
/// with the lowering's judgments, then compiles and runs it. Both need the
/// lowering and codegen, which only the host has, so the host passes the
/// engine into each load and check. `None` means the engine declined the
/// generator.
pub type GenEngine = dyn Fn(
        &Program,
        &str,
        &[crate::consteval::ConstVal],
        &GenInputs<'_>,
    ) -> Option<Result<GenOutput, GenError>>
    + Send
    + Sync;

/// Identifies the running compiler build: the crate version, then the
/// executable's size and mtime. A persisted generator output or artifact is
/// reused only by the same build.
///
/// An executable written within [`crate::manifest::MTIME_SLACK`] of now is
/// named by the hash of its bytes instead: a second build of the same size in
/// that window can carry the same mtime. Later, no write can, so the stat
/// suffices.
pub fn compiler_identity() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        let exe_id = match exe.as_ref().and_then(|p| std::fs::metadata(p).ok()) {
            Some(m) => executable_id(m.len(), m.modified().ok(), || {
                std::fs::read(exe.as_ref()?).ok()
            }),
            // A build that cannot be identified must not reuse outputs across
            // processes. A page has no executable and no process id, and its
            // cache never leaves the page (`vyrn-play`).
            None if cfg!(target_os = "unknown") => "page".to_string(),
            None => format!("unknown-{}", std::process::id()),
        };
        format!("{}:{exe_id}", env!("CARGO_PKG_VERSION"))
    })
    .clone()
}

/// The executable part of [`compiler_identity`]: `len:mtime` once the mtime is
/// settled, else the hash of `bytes()`.
fn executable_id(
    len: u64,
    mtime: Option<std::time::SystemTime>,
    bytes: impl FnOnce() -> Option<Vec<u8>>,
) -> String {
    let settled = mtime
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= crate::manifest::MTIME_SLACK);
    let nanos = mtime.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    match (settled, nanos) {
        (true, Some(d)) => format!("{len}:{}", d.as_nanos()),
        _ => match bytes() {
            Some(b) => format!("sha256:{}", crate::hash::sha256_hex(&b)),
            None => format!("unknown-{}", std::process::id()),
        },
    }
}

/// Runs `fn_name` in `program` as a generation target on `inputs.engine`,
/// under the sandbox in `inputs`, with the compile-time constants `args`. Returns the
/// module source and the recorded reads, or why the run failed. `program`
/// has passed [`crate::check_and_synthesize`]; the engine judges the rest.
pub fn generate(
    program: &Program,
    fn_name: &str,
    args: &[crate::consteval::ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, GenError> {
    // Without an engine no `gen fn` can run, and the error says so.
    let Some(engine) = inputs.engine else {
        return Err(GenError::Failed(format!(
            "cannot run the generator `{fn_name}`: no generation engine is installed"
        )));
    };
    engine(program, fn_name, args, &inputs).unwrap_or_else(|| {
        Err(GenError::Failed(format!(
            "cannot run the generator `{fn_name}`: the installed generation engine declined it"
        )))
    })
}

/// The name `derive(g, x)` calls: the entry generator `g` writes for the type
/// of `x`, after [`derive`] prefixes every function `g` wrote.
pub fn derived_name(g: &str, ty: &crate::ast::Type) -> String {
    format!("derive${g}$t{}", crate::types::struct_key(ty))
}

/// What a derive generator wrote: its functions and type declarations, each
/// renamed under `derive$g$`, which no source can spell.
pub type Derived = (Vec<crate::ast::Function>, Vec<crate::ast::TypeDecl>);

/// A `derive` site: generator `g` writes an entry for `ty`, and the call at
/// `line` calls it as `entry`, a [`Type::Fn`](crate::ast::Type::Fn).
#[derive(Clone)]
pub struct Site {
    pub g: String,
    pub ty: crate::ast::Type,
    pub line: usize,
    pub entry: crate::ast::Type,
}

/// Runs the generator of every `derive(g, x)` site on `engine`, over the types
/// the checker gave `x`, and returns what they wrote.
///
/// `sites` are in source order. Each generator runs once per
/// program, over all of its types as one `TypeArg`: a run per type would write a
/// shared subtype's function twice. It runs through `linker` as an import
/// line's generator does ([`crate::loader::generation`]): its module loaded as
/// its own root, and its output cached under the `TypeArg`. With no `linker`,
/// every site fails. The output is not
/// checked here, except each entry's signature against its site; the caller
/// checks the program it joins. A generator's own program may call `derive` (a
/// `std/ui` generator reaches `toJson`), but not reach itself.
pub fn derive(
    program: &Program,
    sites: &[Site],
    engine: Option<&GenEngine>,
    linker: Option<crate::symbols::Linker<'_>>,
) -> Result<Derived, Diagnostic> {
    let unplaced = |e: String| Diagnostic::error(0, 0, "check", e);
    let mut out: Derived = (Vec::new(), Vec::new());
    if sites.is_empty() {
        return Ok(out);
    }
    let types = crate::types::decl_map(program);
    let mut gens: Vec<&str> = Vec::new();
    for s in sites {
        if !gens.contains(&s.g.as_str()) {
            gens.push(&s.g);
        }
    }
    for g in gens {
        let roots: Vec<crate::ast::Type> = sites
            .iter()
            .filter(|s| s.g == g)
            .map(|s| s.ty.clone())
            .collect();
        let (arg, placed) = crate::schema_reflect::type_arg_lit(&roots, &types);
        if let Some(i) = placed.iter().position(Option::is_none) {
            return Err(unplaced(format!(
                "`derive({g}, ..)` cannot reflect `{}`: it has no wire form or no source spelling",
                roots[i]
            )));
        }
        let Some((_, opts, resolver)) = linker else {
            return Err(unplaced(format!(
                "cannot run the generator `{g}`: no load links its module"
            )));
        };
        let (key, name) = declared(program, g);
        let identity = format!("{key}\u{0}{name}");
        if opts.nest.deriving.contains(&identity) {
            return Err(unplaced(format!(
                "generator `{g}` reaches `derive({g}, ..)`"
            )));
        }
        let mut inner = crate::loader::generator_opts(opts);
        inner.nest.deriving.push(identity);
        let mut read = HashMap::new();
        let root = program.spellings.root.as_str();
        let refused = |ds: Vec<Diagnostic>| -> String {
            ds.into_iter()
                .take(1)
                .map(|d| match d.file {
                    Some(_) => d,
                    None => crate::loader::in_module(d, key, root),
                })
                .map(|d| format!("generator `{g}` does not check: {}", d.render()))
                .collect()
        };
        // The cache key holds the `TypeArg` literal, so a change to a reflected
        // type misses and any other edit of the program hits.
        let call = crate::loader::GenCall {
            consts: &[],
            type_arg: Some(&arg),
            arg_repr: format!("{arg:?}"),
            importer_dir: String::new(),
            allowed: Vec::new(),
            aliased: Vec::new(),
        };
        let gen = crate::loader::generation;
        let written = gen(
            key,
            name,
            call,
            (true, false),
            &inner,
            resolver,
            engine,
            &mut read,
        );
        let written = written.map_err(|f| match f {
            crate::loader::GenFault::Load(ds) => unplaced(refused(ds)),
            crate::loader::GenFault::Reread(why) => {
                let rule = crate::rules::rule!(GenModuleReread, module = key, why);
                Diagnostic::refusal(0, 0, "check", rule)
            }
            crate::loader::GenFault::Run(GenError::Refused(ds)) => unplaced(refused(ds)),
            crate::loader::GenFault::Run(GenError::Failed(trap)) => unplaced(trap),
            crate::loader::GenFault::Run(GenError::Unchecked) => {
                unplaced("the engine asked to check a checked program".to_string())
            }
        })?;
        let (fns, decls) = adopt(&written.written.source, g).map_err(unplaced)?;
        for s in sites.iter().filter(|s| s.g == g) {
            let Some(f) = fns.iter().find(|f| f.name == derived_name(g, &s.ty)) else {
                return Err(unplaced(format!(
                    "generator `{g}` wrote no entry for `{}`: each root node's `name` must be a function",
                    s.ty
                )));
            };
            let mut got = crate::ast::Type::Fn(
                f.params.iter().map(|p| p.ty.clone()).collect(),
                Box::new(f.ret.clone()),
            );
            // Generated code spells a named union as written.
            crate::ctor::declared_unions(&mut got, &types);
            if got != s.entry {
                let rule = crate::rules::rule!(DeriveEntryMismatch, g, got, want = s.entry);
                return Err(Diagnostic::refusal(s.line, 0, "check", rule));
            }
        }
        out.0.extend(fns);
        out.1.extend(decls);
    }
    Ok(out)
}

/// The module key and the declared name of the generator `program` links as
/// `g`. A declaration the loader renamed apart, or one of an injected module,
/// is in [`Program::spellings`]; any other is declared as linked, in the
/// module its function names.
fn declared<'a>(program: &'a Program, g: &'a str) -> (&'a str, &'a str) {
    if let Some((written, key)) = program.spellings.decls.get(g) {
        return (key, written);
    }
    let module = (program.functions.iter())
        .find(|f| f.name == g)
        .and_then(|f| f.module.as_deref());
    (module.unwrap_or(&program.spellings.root), g)
}

/// Parses what generator `g` wrote, `src`, and renames each function and type
/// it defines to `derive$g$<name>`.
fn adopt(src: &str, g: &str) -> Result<Derived, String> {
    let tokens = crate::lexer::lex(src).map_err(|d| {
        format!(
            "generator `{g}` wrote text that does not lex: {}\n{src}",
            d.message
        )
    })?;
    let (mut written, errors) = crate::parser::parse_bare(tokens);
    if let Some(d) = errors.first() {
        return Err(format!(
            "generator `{g}` wrote text that does not parse: {}\n{src}",
            d.message
        ));
    }
    let names = written.functions.iter().map(|f| &f.name);
    let mut map: HashMap<String, String> = names
        .chain(written.type_decls.iter().map(|t| &t.name))
        .map(|n| (n.clone(), format!("derive${g}${n}")))
        .collect();
    for (ph, fold) in crate::schema_reflect::PLACEHOLDERS {
        for (i, _) in src.match_indices(ph) {
            let name: String = src[i..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let real = format!("{fold}{}", &name[ph.len()..]);
            map.insert(name, real);
        }
    }
    // The banner a diagnostic in the written code names as its file.
    let banner = format!("generated by derive({g}, ..)");
    for f in &mut written.functions {
        f.name = map[&f.name].clone();
        f.module = Some(banner.clone());
    }
    for t in &mut written.type_decls {
        t.name = map[&t.name].clone();
        t.module = Some(banner.clone());
    }
    crate::loader::rewrite_names(&mut written, &map);
    Ok((written.functions, written.type_decls))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two builds of one size written in one mtime tick differ by their bytes;
    /// a settled mtime is trusted.
    #[test]
    fn a_fresh_executable_is_named_by_its_bytes() {
        let now = Some(std::time::SystemTime::now());
        let a = executable_id(4, now, || Some(b"bldA".to_vec()));
        let b = executable_id(4, now, || Some(b"bldB".to_vec()));
        assert_ne!(a, b);
        let old = Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000));
        assert_eq!(executable_id(4, old, || None), "4:1000000000000000");
    }
}
