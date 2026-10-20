//! The Vyrn front end as a wasm module for the website's playground:
//! [`play_tokens`] (highlighting with the compiler's lexer), [`play_check`]
//! (diagnostics from the real loader) and [`play_compile`] (the bytes
//! `vyrn build --target wasm` writes). [`play_complete`], [`play_hover`] and
//! [`play_signature`] answer the editor from the analysis `vyrn-lsp` serves,
//! at a 1-based line and char column. `site/public/play-worker.js` runs the
//! module with `web/wasi-min.js`; this crate cannot, because it is one. `std/`
//! is embedded by `build.rs`; a relative import reports `module not found`.
//!
//! Calling convention: `input_ptr(n)` reserves `n` bytes to write; an entry
//! point takes the length and returns the result length; `result_ptr()` says
//! where the result is. The result is UTF-8 JSON, or from [`play_compile`] a
//! wasm module, told apart by the first byte (`\0asm` against `{`). A memory
//! growth detaches `memory.buffer`, so the page re-reads both pointers after
//! every call.

use std::cell::RefCell;

use vyrn_frontend::diagnostics::{Diagnostic, Severity};
use vyrn_frontend::lexer::{self, Tok, Triv, TrivKind};
use vyrn_frontend::loader::{LoadOptions, MapResolver};
use vyrn_frontend::rules::{IntoHole, Rule};
use vyrn_frontend::{
    analyze_linked, member_completions, member_context, resolve, scope_completions, string_context,
    string_literal_completions, Analysis, Completion, Resolution, SymbolKind,
};

thread_local! {
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Reserves `len` bytes of input and returns where to write them.
#[no_mangle]
pub extern "C" fn input_ptr(len: usize) -> *mut u8 {
    INPUT.with(|i| {
        let mut b = i.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

/// Where the last call's result begins; its length was that call's return value.
#[no_mangle]
pub extern "C" fn result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn play_tokens(src_len: usize) -> usize {
    with_input(src_len, |src| tokens_json(src).into_bytes())
}

#[no_mangle]
pub extern "C" fn play_check(src_len: usize) -> usize {
    with_input(src_len, |src| check_json(src).into_bytes())
}

/// Compiles `input[..src_len]` to module bytes, or the JSON diagnostics of a
/// program that did not compile.
#[no_mangle]
pub extern "C" fn play_compile(src_len: usize) -> usize {
    with_input(src_len, |src| compile_result(src))
}

/// Decodes the input as UTF-8, hands it to `f`, and publishes the result.
///
/// Invalid UTF-8 and a `src_len` past the reserved buffer answer a diagnostic,
/// because a panic on this target aborts the instance silently.
fn with_input(src_len: usize, f: impl FnOnce(&str) -> Vec<u8>) -> usize {
    let too_long = INPUT.with(|i| src_len > i.borrow().len());
    if too_long {
        let d = Diagnostic::refusal(
            0,
            0,
            "host",
            Rule::SourceTooLong {
                src_len: src_len.hole(),
            },
        );
        return publish_json(format!("{{\"diagnostics\":[{}]}}", diag_json(&d)));
    }
    let bytes = INPUT.with(|i| i.borrow()[..src_len].to_vec());
    let result = match std::str::from_utf8(&bytes) {
        Ok(src) => f(src),
        Err(_) => {
            let d = Diagnostic::refusal(0, 0, "lex", Rule::NotUtf8 {});
            format!("{{\"diagnostics\":[{}]}}", diag_json(&d)).into_bytes()
        }
    };
    publish(result)
}

fn publish(bytes: Vec<u8>) -> usize {
    RESULT.with(|r| {
        let mut b = r.borrow_mut();
        *b = bytes;
        b.len()
    })
}

fn publish_json(json: String) -> usize {
    publish(json.into_bytes())
}

/// Words the grammar reads as keywords in position and the lexer returns as
/// identifiers. The same list as `site/app/hl.vyrn`.
const CONTEXTUAL: &[&str] = &[
    "read", "modify", "consume", "gen", "test", "bench", "panic", "from", "as",
];

/// The stylesheet's CSS class for one lexed item (`k`, `s`, `n`, `c`, `t`,
/// `f`), or `""` for text with no colour. `callee` says a `(` follows the item
/// or `fn` precedes it, as `site/app/hl.vyrn` decides.
fn class_of(item: &Triv, callee: bool) -> &'static str {
    match &item.kind {
        TrivKind::Comment | TrivKind::Doc(_) => "c",
        TrivKind::Tok(tok) => match tok {
            Tok::Str(_) | Tok::TemplateStr { .. } => "s",
            Tok::Int(_) | Tok::Byte(_) | Tok::Float(_) => "n",
            Tok::Doc(_) => "c",
            Tok::Ident(name) => {
                if CONTEXTUAL.contains(&name.as_str()) {
                    "k"
                } else if name.starts_with(char::is_uppercase) {
                    "t"
                } else if callee {
                    "f"
                } else {
                    ""
                }
            }
            other => {
                if lexer::token_name_and_text(other).0 == "keyword" {
                    "k"
                } else {
                    ""
                }
            }
        },
    }
}

fn tok_of(item: &Triv) -> Option<&Tok> {
    match &item.kind {
        TrivKind::Tok(t) => Some(t),
        _ => None,
    }
}

/// One coloured run: `[start, length, class]`, in UTF-16 code units, because
/// that is what a JavaScript string index counts.
fn tokens_json(src: &str) -> String {
    let mut spans = Vec::new();
    match paint(src, 0, &mut spans) {
        Ok(()) => format!("{{\"spans\":[{}]}}", spans.join(",")),
        // Mid-keystroke the source is often not lexable; the page shows plain
        // text for this frame.
        Err(d) => format!("{{\"error\":{}}}", json_str(&d.message)),
    }
}

/// Pushes the runs of `src`, which starts `base` UTF-16 units into the buffer.
fn paint(src: &str, base: usize, out: &mut Vec<String>) -> Result<(), Diagnostic> {
    let items = lexer::lex_with_trivia(src)?;
    let mut byte_at = 0usize;
    let mut u16_at = base;
    let mut prev: Option<&Tok> = None;
    for (i, item) in items.iter().enumerate() {
        let callee = matches!(items[i + 1..].iter().find_map(tok_of), Some(Tok::LParen))
            || matches!(prev, Some(Tok::Fn));
        if let Some(t) = tok_of(item) {
            prev = Some(t);
        }
        if item.text.is_empty() {
            continue;
        }
        // Each item's `text` is its verbatim source slice, in source order, so
        // a forward search finds its extent.
        let Some(found) = src[byte_at..].find(item.text.as_str()) else {
            continue;
        };
        let start = byte_at + found;
        u16_at += src[byte_at..start].encode_utf16().count();
        let len = item.text.encode_utf16().count();
        if let TrivKind::Tok(Tok::TemplateStr { exprs, .. }) = &item.kind {
            paint_template(&item.text, exprs, u16_at, out)?;
        } else {
            let cls = class_of(item, callee);
            if !cls.is_empty() {
                out.push(format!("[{u16_at},{len},\"{cls}\"]"));
            }
        }
        u16_at += len;
        byte_at = start + item.text.len();
    }
    Ok(())
}

/// Pushes the runs of the interpolated string `text`, which starts at UTF-16
/// offset `at`: each hole is painted as code, the rest as string. A hole's
/// source is shorter than `text`, so the recursion ends.
fn paint_template(
    text: &str,
    holes: &[lexer::Hole],
    mut at: usize,
    out: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    let mut from = 0usize;
    for hole in holes {
        let Some(found) = text[from..].find(&format!("\\{{{}", hole.src)) else {
            break;
        };
        let n = text[from..from + found].encode_utf16().count();
        if n > 0 {
            out.push(format!("[{at},{n},\"s\"]"));
        }
        at += n;
        out.push(format!("[{at},2,\"i\"]"));
        paint(&hole.src, at + 2, out)?;
        at += 2 + hole.src.encode_utf16().count();
        // The lexer ends a hole at its closing `}`, one byte.
        out.push(format!("[{at},1,\"i\"]"));
        at += 1;
        from += found + 2 + hole.src.len() + 1;
    }
    out.push(format!(
        "[{at},{},\"s\"]",
        text[from..].encode_utf16().count()
    ));
    Ok(())
}

/// The standard library, as `build.rs` walked it out of `std/`.
mod std_modules {
    include!(concat!(env!("OUT_DIR"), "/std_modules.rs"));
}

thread_local! {
    /// The page's answer to [`run_generator`]: the module's stdout, or why it
    /// did not run.
    static GEN_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Reserves `len` bytes for the page's answer to `run_generator` and returns
/// where to write them.
#[no_mangle]
pub extern "C" fn gen_output_ptr(len: usize) -> *mut u8 {
    GEN_OUT.with(|o| {
        let mut b = o.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "vyrn_play")]
extern "C" {
    /// Runs a generator module in the page. The request is [`gen_request`]'s
    /// bytes; the page writes stdout, or the error, through [`gen_output_ptr`]
    /// and answers 0 for stdout, 1 for an error.
    #[link_name = "run_generator"]
    fn page_run_generator(req: *const u8, len: usize) -> u32;
}

/// Runs a generator module in the page and answers its stdout.
fn run_generator(
    module: &[u8],
    argv: &[String],
    atoms: &[vyrn_genwasm::Atom],
) -> Result<Vec<u8>, String> {
    let status = page_run(&gen_request(module, argv, atoms))?;
    let out = GEN_OUT.with(|o| std::mem::take(&mut *o.borrow_mut()));
    match status {
        0 => Ok(out),
        _ => Err(String::from_utf8_lossy(&out).into_owned()),
    }
}

#[cfg(target_arch = "wasm32")]
fn page_run(req: &[u8]) -> Result<u32, String> {
    // SAFETY: the page reads `len` bytes at `req`, which stay borrowed for the
    // call, and writes only through `gen_output_ptr`.
    Ok(unsafe { page_run_generator(req.as_ptr(), req.len()) })
}

/// On the host (`cargo test`) there is no page to run a module.
#[cfg(not(target_arch = "wasm32"))]
fn page_run(_: &[u8]) -> Result<u32, String> {
    Err("a generator runs only in the page".to_string())
}

/// The request `run_generator` hands the page, little-endian: the module
/// (`u32` length, bytes), argv (`u32` count, then `u32` length and bytes
/// each) and the atoms (`u32` count, then a tag byte each: 0 and an `i64`, or
/// 1 and a `u32` length and bytes).
fn gen_request(module: &[u8], argv: &[String], atoms: &[vyrn_genwasm::Atom]) -> Vec<u8> {
    let mut out = Vec::new();
    let bytes = |out: &mut Vec<u8>, b: &[u8]| {
        out.extend((b.len() as u32).to_le_bytes());
        out.extend(b);
    };
    bytes(&mut out, module);
    out.extend((argv.len() as u32).to_le_bytes());
    for a in argv {
        bytes(&mut out, a.as_bytes());
    }
    out.extend((atoms.len() as u32).to_le_bytes());
    for a in atoms {
        match a {
            vyrn_genwasm::Atom::Int(n) => {
                out.push(0);
                out.extend(n.to_le_bytes());
            }
            vyrn_genwasm::Atom::Str(s) => {
                out.push(1);
                bytes(&mut out, s);
            }
        }
    }
    out
}

/// The playground's generation engine: each generator runs in the page.
fn engine() -> Box<vyrn_frontend::gen::GenEngine> {
    vyrn_lower::gen_engine(|p, f, a, i| vyrn_genwasm::run_pure(p, f, a, i, run_generator))
}

fn options(expansions: std::sync::Arc<vyrn_frontend::project::Expansions>) -> LoadOptions {
    LoadOptions {
        std_root: Some("std".into()),
        aliases: Default::default(),
        alias_base: String::new(),
        audience: None,
        artifacts: None,
        expansions,
        session: None,
        nest: Default::default(),
    }
}

/// A resolver that holds `std/` alone, so a relative import reports `module not found`.
fn std_resolver() -> MapResolver {
    MapResolver(
        std_modules::STD
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

/// Loads `src` as a one-file program through `load_warned`, the CLI's entry
/// point.
fn load(
    src: &str,
    expansions: std::sync::Arc<vyrn_frontend::project::Expansions>,
) -> (
    Result<
        (
            vyrn_frontend::ast::Program,
            std::sync::Arc<vyrn_lower::World>,
        ),
        Vec<Diagnostic>,
    >,
    Vec<Diagnostic>,
) {
    let engine = engine();
    vyrn_lower::load_warned(
        src,
        "play.vyrn",
        &options(expansions),
        &std_resolver(),
        Some(&*engine),
    )
}

fn check_json(src: &str) -> String {
    let (result, warnings) = load(src, Default::default());
    let mut all = match result {
        Ok(_) => Vec::new(),
        Err(diags) => diags,
    };
    all.extend(warnings);
    format!("{{\"diagnostics\":{}}}", diags_json(&all))
}

/// The program as a wasm module from the direct backend, or the diagnostics of
/// one that did not compile. Warnings are left to `play_check`.
fn compile_result(src: &str) -> Vec<u8> {
    // The load and the backend must walk one expansion of each desugared site
    // (`schemaOf<T>()`, a user container's `a[i]`), or the core's rows, keyed
    // by the load's nodes, miss the backend's.
    let (program, world) = match load(src, vyrn_frontend::project::Expansions::shared()).0 {
        Ok(p) => p,
        Err(diags) => return format!("{{\"diagnostics\":{}}}", diags_json(&diags)).into_bytes(),
    };
    match vyrn_codegen::direct::compile(&program, world) {
        Ok(bytes) => bytes,
        // A backend refusal is a diagnostic with no position.
        Err(e) => {
            let d = Diagnostic::error(0, 0, "codegen", e);
            format!("{{\"diagnostics\":[{}]}}", diag_json(&d)).into_bytes()
        }
    }
}

/// Completions at the 1-based `(line, col)` cursor, `col` in chars:
/// `{"items":[{"label","kind","detail","doc"}]}`, unfiltered; the page
/// filters by the typed prefix.
#[no_mangle]
pub extern "C" fn play_complete(src_len: usize, line: usize, col: usize) -> usize {
    with_input(src_len, |src| complete_json(src, line, col).into_bytes())
}

/// The hover at the cursor, as `play_complete` places it:
/// `{"signature":...,"doc":...}`, or `{}` where nothing resolves.
#[no_mangle]
pub extern "C" fn play_hover(src_len: usize, line: usize, col: usize) -> usize {
    with_input(src_len, |src| {
        let found = resolve(&analysis(src), line, col);
        hover_json(found, None).into_bytes()
    })
}

/// The signature of the innermost call whose parentheses hold the cursor:
/// the hover of its callee plus `"arg"`, the 0-based argument the cursor is
/// in, or `{}` outside a call.
#[no_mangle]
pub extern "C" fn play_signature(src_len: usize, line: usize, col: usize) -> usize {
    with_input(src_len, |src| signature_json(src, line, col).into_bytes())
}

fn signature_json(src: &str, line: usize, col: usize) -> String {
    let Some((l, c, arg)) = call_at(src, line, col) else {
        return "{}".to_string();
    };
    let found =
        resolve(&analysis(src), l, c).or_else(|| resolve(&analysis(&closed_call(src, l, c)), l, c));
    hover_json(found, Some(arg))
}

/// The analysis `vyrn-lsp` answers an editor from, linked against the
/// embedded `std/`.
fn analysis(src: &str) -> Analysis {
    let engine = engine();
    analyze_linked(
        src,
        "play.vyrn",
        &options(Default::default()),
        &std_resolver(),
        Some(&*engine),
    )
}

/// Routes the cursor as `vyrn-lsp` routes it: a string offers std module
/// names after `from "std/` and the keys of a finite string type, a member
/// position the receiver's members, anywhere else the names in scope and the
/// keywords.
fn complete_json(src: &str, line: usize, col: usize) -> String {
    let mut out = Vec::new();
    if string_context(src, line, col) {
        if let Some(prefix) = std_import_prefix(src, line, col) {
            for (key, _) in std_modules::STD {
                let name = key.trim_end_matches(".vyrn");
                if let Some(rest) = name.strip_prefix(prefix) {
                    out.push(item_json(rest, "module", name, None));
                }
            }
        } else {
            for c in string_literal_completions(&analysis(src), src, line, col) {
                out.push(completion_json(&c));
            }
        }
    } else if member_context(src, line, col) {
        for c in member_completions(&analysis(&with_member_name(src, line, col)), line, col) {
            out.push(completion_json(&c));
        }
    } else {
        for c in scope_completions(&analysis(src), line) {
            out.push(completion_json(&c));
        }
        for k in lexer::KEYWORDS.iter().chain(CONTEXTUAL) {
            out.push(item_json(k, "keyword", "keyword", None));
        }
    }
    format!("{{\"items\":[{}]}}", out.join(","))
}

/// The directory part of a `from "std/...` specifier the cursor is typing,
/// up to its last `/`, or `None` when the string is not such a specifier.
fn std_import_prefix(src: &str, line: usize, col: usize) -> Option<&str> {
    let text = src.lines().nth(line.checked_sub(1)?)?;
    let head = &text[..col_byte(text, col)];
    let quote = head.rfind('"')?;
    let spec = &head[quote + 1..];
    if !head[..quote].trim_end().ends_with("from") || !spec.starts_with("std/") {
        return None;
    }
    Some(&spec[..=spec.rfind('/')?])
}

/// `src` with a placeholder member name at a cursor right after `.`, so a
/// line still being typed parses and its receiver is typed.
fn with_member_name(src: &str, line: usize, col: usize) -> String {
    let mut out = String::with_capacity(src.len() + 1);
    for (i, text) in src.split_inclusive('\n').enumerate() {
        let at = col_byte(text, col);
        if i + 1 == line && text[..at].ends_with('.') {
            out.push_str(&text[..at]);
            out.push('x');
            out.push_str(&text[at..]);
        } else {
            out.push_str(text);
        }
    }
    out
}

/// `src` with line `line` cut to `()` after the callee at `col`, so a call
/// still being typed parses and an imported callee resolves.
fn closed_call(src: &str, line: usize, col: usize) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, text) in src.split_inclusive('\n').enumerate() {
        let from = col_byte(text, col);
        match text[from..].find('(') {
            Some(p) if i + 1 == line => {
                out.push_str(&text[..from + p]);
                out.push_str("()\n");
            }
            _ => out.push_str(text),
        }
    }
    out
}

/// The byte offset of 1-based char column `col` in `text`, clamped to its end.
fn col_byte(text: &str, col: usize) -> usize {
    (text.char_indices().nth(col.saturating_sub(1))).map_or(text.len(), |(i, _)| i)
}

/// The callee token's `(line, col)` and the 0-based argument index of the
/// innermost unclosed `(` before the cursor, when an identifier names it.
fn call_at(src: &str, line: usize, col: usize) -> Option<(usize, usize, usize)> {
    let toks = lexer::lex(src).ok()?;
    // Each open bracket: its callee when it is a call's `(`, and its commas.
    let mut open: Vec<(Option<(usize, usize)>, usize)> = Vec::new();
    let mut prev: Option<&lexer::Token> = None;
    for t in toks.iter().take_while(|t| (t.line, t.col) < (line, col)) {
        match &t.tok {
            Tok::LParen => open.push((
                prev.filter(|p| matches!(p.tok, Tok::Ident(_)))
                    .map(|p| (p.line, p.col)),
                0,
            )),
            Tok::LBracket | Tok::LBrace => open.push((None, 0)),
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                open.pop();
            }
            Tok::Comma => {
                if let Some(top) = open.last_mut() {
                    top.1 += 1;
                }
            }
            _ => {}
        }
        prev = Some(t);
    }
    let (callee, arg) = open.pop()?;
    let (l, c) = callee?;
    Some((l, c, arg))
}

fn kind_name(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "function",
        SymbolKind::Type => "type",
        SymbolKind::Variant => "variant",
        SymbolKind::Method => "method",
        SymbolKind::Field => "field",
        SymbolKind::Param => "param",
        SymbolKind::Local => "local",
        SymbolKind::Global => "global",
    }
}

fn completion_json(c: &Completion) -> String {
    item_json(&c.label, kind_name(c.kind), &c.detail, c.doc.as_deref())
}

fn item_json(label: &str, kind: &str, detail: &str, doc: Option<&str>) -> String {
    format!(
        "{{\"label\":{},\"kind\":\"{kind}\",\"detail\":{},\"doc\":{}}}",
        json_str(label),
        json_str(detail),
        doc.map_or("null".to_string(), json_str)
    )
}

fn hover_json(r: Option<Resolution>, arg: Option<usize>) -> String {
    let Some(r) = r else {
        return "{}".to_string();
    };
    let arg = arg.map_or(String::new(), |a| format!(",\"arg\":{a}"));
    format!(
        "{{\"signature\":{},\"doc\":{}{arg}}}",
        r.signature.as_deref().map_or("null".to_string(), json_str),
        json_str(&r.doc)
    )
}

/// `s` as a JSON string literal, quotes included, escaped by the canonical
/// table in [`vyrn_frontend::codec::escape_into`].
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    vyrn_frontend::codec::escape_into(s, &mut out);
    out.push('"');
    out
}

fn diag_json(d: &Diagnostic) -> String {
    let note = match &d.note {
        Some(n) => json_str(n),
        None => "null".to_string(),
    };
    format!(
        "{{\"line\":{},\"col\":{},\"endCol\":{},\"severity\":\"{}\",\"stage\":\"{}\",\"message\":{},\"note\":{}}}",
        d.line,
        d.col,
        d.end_col,
        match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        d.stage,
        json_str(&d.message),
        note
    )
}

fn diags_json(ds: &[Diagnostic]) -> String {
    let mut out = String::from("[");
    for (i, d) in ds.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&diag_json(d));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span list as `(start, len, class)` triples, parsed back out of the JSON.
    fn spans(src: &str) -> Vec<(usize, usize, String)> {
        let json = tokens_json(src);
        let body = json
            .strip_prefix("{\"spans\":[")
            .and_then(|s| s.strip_suffix("]}"))
            .unwrap_or_else(|| panic!("not a span list: {json}"));
        if body.is_empty() {
            return Vec::new();
        }
        body.split("],[")
            .map(|t| {
                let t = t.trim_start_matches('[').trim_end_matches(']');
                let f: Vec<&str> = t.split(',').collect();
                (
                    f[0].parse().unwrap(),
                    f[1].parse().unwrap(),
                    f[2].trim_matches('"').to_string(),
                )
            })
            .collect()
    }

    /// A span's text, taken out of the source by the page's offsets.
    fn slice16(src: &str, start: usize, len: usize) -> String {
        let units: Vec<u16> = src.encode_utf16().collect();
        String::from_utf16(&units[start..start + len]).unwrap()
    }

    #[test]
    fn a_span_covers_exactly_its_source_text() {
        let src = "fn main() -> Int64 {\n    print(\"hi\") // go\n    return 0\n}\n";
        let got: Vec<(String, String)> = spans(src)
            .into_iter()
            .map(|(s, l, c)| (slice16(src, s, l), c))
            .collect();
        assert_eq!(
            got,
            vec![
                ("fn".to_string(), "k".to_string()),
                ("main".to_string(), "f".to_string()),
                ("Int64".to_string(), "t".to_string()),
                ("print".to_string(), "f".to_string()),
                ("\"hi\"".to_string(), "s".to_string()),
                ("// go".to_string(), "c".to_string()),
                ("return".to_string(), "k".to_string()),
                ("0".to_string(), "n".to_string()),
            ]
        );
    }

    #[test]
    fn offsets_are_utf16_code_units_not_bytes() {
        // The emoji is one character, two UTF-16 units and four bytes, so a byte
        // offset would put every span after the comment two units early.
        let src = "// 🌊\ntype T = Int64\n";
        let last = spans(src).pop().expect("the type name is coloured");
        assert_eq!(slice16(src, last.0, last.1), "Int64");
    }

    #[test]
    fn a_capability_word_is_a_keyword_and_an_ordinary_name_is_not() {
        let classes = |src: &str| -> Vec<String> { spans(src).into_iter().map(|s| s.2).collect() };
        assert_eq!(classes("let x = consume\n"), vec!["k", "k"]);
        assert_eq!(classes("let consumer = 1\n"), vec!["k", "n"]);
    }

    #[test]
    fn an_unlexable_source_says_so_instead_of_going_blank() {
        let json = tokens_json("let s = \"unterminated\n");
        assert!(json.starts_with("{\"error\":"), "{json}");
    }

    #[test]
    fn a_program_that_compiles_reports_nothing() {
        assert_eq!(
            check_json("fn main() -> Int64 {\n    return 0\n}\n"),
            "{\"diagnostics\":[]}"
        );
    }

    #[test]
    fn a_diagnostic_arrives_with_its_position_and_stage() {
        let json = check_json("fn main() -> Int64 {\n    return nope\n}\n");
        assert!(json.contains("\"line\":2"), "{json}");
        assert!(json.contains("\"stage\":\"check\""), "{json}");
        assert!(json.contains("\"severity\":\"error\""), "{json}");
    }

    #[test]
    fn the_standard_library_is_here_and_a_second_file_is_not() {
        let std_import = check_json(
            "import { joinWith } from \"std/strings\"\nfn main() -> Int64 { return 0 }\n",
        );
        assert_eq!(std_import, "{\"diagnostics\":[]}");
        // A relative import has nowhere to go, in the loader's own words.
        let local = check_json("import { f } from \"./other\"\nfn main() -> Int64 { return 0 }\n");
        assert!(local.contains("module not found"), "{local}");
    }

    /// How the page tells a module from JSON diagnostics.
    const MAGIC: &[u8] = b"\0asm";

    #[test]
    fn a_program_that_calls_the_standard_library_compiles_to_a_module() {
        let bytes = compile_result(
            "import { joinWith } from \"std/strings\"\n\
             fn main() -> Int64 {\n    print(joinWith([\"a\", \"b\"], \"-\"))\n    return 0\n}\n",
        );
        assert_eq!(
            &bytes[..4],
            MAGIC,
            "not a module: {:?}",
            &bytes[..8.min(bytes.len())]
        );
    }

    #[test]
    fn a_program_that_does_not_compile_answers_diagnostics_and_not_a_module() {
        let bytes = compile_result("fn main() -> Int64 {\n    return nope\n}\n");
        let json = String::from_utf8(bytes).expect("diagnostics are UTF-8 JSON");
        assert!(json.starts_with("{\"diagnostics\":["), "{json}");
        assert!(json.contains("\"line\":2"), "{json}");
    }

    /// A panic on wasm aborts the instance silently, so an out-of-range length
    /// answers through the JSON channel.
    #[test]
    fn play_compile_length_past_the_buffer_answers_instead_of_aborting() {
        input_ptr(8); // reserve 8 zero bytes
        let n = play_compile(usize::MAX);
        let json = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result_ptr(), n) })
            .into_owned();
        assert!(json.contains("exceeds the input buffer"), "{json}");
    }

    #[test]
    fn an_interpolation_hole_is_painted_as_code() {
        assert_eq!(
            tokens_json(r#"x("a\{f(1)}b")"#),
            r#"{"spans":[[0,1,"f"],[2,2,"s"],[4,2,"i"],[6,1,"f"],[8,1,"n"],[10,1,"i"],[11,2,"s"]]}"#
        );
    }

    #[test]
    fn play_tokens_length_past_the_buffer_answers_instead_of_aborting() {
        input_ptr(4);
        let n = play_tokens(usize::MAX);
        let json = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result_ptr(), n) })
            .into_owned();
        assert!(json.contains("exceeds the input buffer"), "{json}");
    }

    /// The `main` that `site/app/guidecode.vyrn` appends to every block.
    const MAIN_TAIL: &str = "\nfn main() -> Int64 {\n    print(demo())\n    return 0\n}\n";

    /// Every guide program with [`MAIN_TAIL`] appended, skipping what
    /// `guidePlayable` skips: a block importing a sibling, and one with no `demo`.
    fn guide_programs() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site/guide");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).expect("read site/guide") {
            let p = e.expect("a guide entry").path();
            if p.extension().is_none_or(|x| x != "vyrn") {
                continue;
            }
            let src = std::fs::read_to_string(&p).expect("read a guide program");
            if src.contains("from \"./") || !src.contains("fn demo(") {
                continue;
            }
            let id = p.file_stem().expect("a stem").to_string_lossy().to_string();
            out.push((id, format!("{src}{MAIN_TAIL}")));
        }
        out.sort();
        out
    }

    /// `site/app/guide.vyrn` runs these through `vyrn run`; this checks the
    /// playground, a different compiler assembly, agrees.
    ///
    /// `VYRN_PLAY_DUMP` writes each answer to that directory, one file per program.
    #[test]
    fn every_runnable_guide_program_compiles_in_the_playground() {
        let dump = std::env::var("VYRN_PLAY_DUMP").ok();
        if let Some(d) = &dump {
            std::fs::create_dir_all(d).expect("the dump directory");
        }
        let mut refused = Vec::new();
        for (id, src) in guide_programs() {
            let checked = check_json(&src);
            let bytes = compile_result(&src);
            let is_module = bytes.starts_with(b"\0asm");
            if let Some(d) = &dump {
                let body = if is_module {
                    format!("module {} bytes, {:016x}", bytes.len(), sum(&bytes))
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                std::fs::write(
                    std::path::Path::new(d).join(format!("{id}.txt")),
                    format!("check: {checked}\ncompile: {body}\n"),
                )
                .expect("write a dump");
            }
            if !is_module {
                refused.push(format!("{id}: {}", String::from_utf8_lossy(&bytes)));
            }
        }
        assert!(refused.is_empty(), "{}", refused.join("\n"));
    }

    /// FNV-1a, so a dump names the module's bytes without holding them.
    fn sum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h: u64, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
        })
    }

    #[test]
    fn a_control_character_in_output_survives_the_json() {
        assert_eq!(json_str("a\u{1}b\n"), "\"a\\u0001b\\n\"");
    }

    const SPLIT: &str = "import { split } from \"std/strings\"\n\nfn main() -> Int64 {\n    let parts = split(\"a,b\", \",\")\n    return 0\n}\n";

    #[test]
    fn a_dot_still_being_typed_offers_the_receivers_members() {
        let src = "fn main() -> Int64 {\n    let xs = [1, 2]; xs.\n    return 0\n}\n";
        let json = complete_json(src, 2, 25);
        for m in ["at", "length"] {
            assert!(json.contains(&format!("\"label\":\"{m}\"")), "{json}");
        }
        assert!(
            !json.contains("\"label\":\"push\""),
            "a fixed-size array cannot grow: {json}"
        );
    }

    #[test]
    fn a_name_offers_locals_imports_and_keywords() {
        let json = complete_json(SPLIT, 5, 5);
        for m in ["parts", "split", "main", "return"] {
            assert!(json.contains(&format!("\"label\":\"{m}\"")), "{json}");
        }
    }

    #[test]
    fn a_std_specifier_offers_the_embedded_modules() {
        let json = complete_json("import { split } from \"std/st", 1, 30);
        assert!(json.contains("\"label\":\"strings\""), "{json}");
    }

    #[test]
    fn hover_on_a_std_function_shows_its_signature_and_doc() {
        let json = hover_json(resolve(&analysis(SPLIT), 4, 17), None);
        assert!(json.contains("fn split(s: String, sep: String)"), "{json}");
        assert!(json.contains("Splits `s` on every occurrence"), "{json}");
    }

    #[test]
    fn signature_help_names_the_call_and_the_argument_under_the_cursor() {
        assert_eq!(call_at(SPLIT, 4, 30), Some((4, 17, 1)));
        assert_eq!(call_at(SPLIT, 4, 10), None);
        assert_eq!(call_at("f(g(1), [2,", 1, 12), None);
    }

    #[test]
    fn signature_help_resolves_an_imported_callee_while_its_call_is_open() {
        let open = SPLIT.replace("split(\"a,b\", \",\")", "split(\"a,b\", ");
        let json = signature_json(&open, 4, 30);
        assert!(json.contains("fn split(s: String, sep: String)"), "{json}");
        assert!(json.contains("\"arg\":1"), "{json}");
    }
}
