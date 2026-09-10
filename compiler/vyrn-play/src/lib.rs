//! The Vyrn front end as a wasm module, for the playground on the website.
//!
//! Three questions, all answered by the compiler itself:
//!
//!   - [`play_tokens`] — where every token is and what kind it is, so the page
//!     colours code with the compiler's own lexer and not a second one written in
//!     JavaScript.
//!   - [`play_check`] — every diagnostic, structured, from the real loader.
//!   - [`play_compile`] — the program as a wasm module, from the direct backend:
//!     the same bytes `vyrn build --target wasm` writes, so what the page runs is
//!     what `vyrn run` runs.
//!
//! THE PAGE RUNS THE PROGRAM, NOT THIS MODULE. This crate cannot instantiate a
//! wasm module, because it IS one. So it answers the bytes and
//! `site/public/play-worker.js` hands them to `web/wasi-min.js`, which is the
//! runtime the `web/` demos have always used for a `vyrn build --target wasm`
//! module and which already answers `{ exitCode, stdout, stderr }`. Stdout,
//! stdin and the clock are the PAGE's now — a browser tab has all three, and
//! only a module that runs a program inside itself needed them faked.
//!
//! WHAT THIS CRATE ADDS is a calling convention and a JSON writer. Not one
//! language decision is made here. `vyrn-frontend` and `vyrn-codegen` compile
//! for `wasm32-unknown-unknown` unchanged.
//!
//! ONE FILE, PLUS `std/`. The standard library is embedded by `build.rs` — the
//! whole directory, walked rather than listed — because the guide book's run
//! links come here and twenty of its twenty-five programs import it. A RELATIVE
//! import still has nowhere to go, and reports the loader's own
//! `module not found: …`.
//!
//! THE CALLING CONVENTION. No bindgen, no dependencies. One input buffer and one
//! output buffer, both owned by the module:
//!
//!   1. `input_ptr(n)` reserves `n` bytes and returns where to write them.
//!   2. an entry point is called with the length; it returns the result length.
//!   3. `result_ptr()` says where the result is. It is UTF-8 JSON — except for
//!      [`play_compile`], which answers a wasm module when the program compiled.
//!      The caller tells the two apart by the first byte: a module begins with
//!      the four-byte wasm magic (a NUL, then `asm`) and JSON begins with `{`.
//!
//! `memory.buffer` is detached by a growth, so the page re-reads both pointers
//! after every call. `site/public/play-worker.js` and `site/public/play.js` are
//! the only callers.

use std::cell::RefCell;

use vyrn_frontend::diagnostics::{Diagnostic, Severity};
use vyrn_frontend::lexer::{self, Tok, Triv, TrivKind};
use vyrn_frontend::loader::{LoadOptions, MapResolver};

thread_local! {
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------------
// The wasm surface
// ---------------------------------------------------------------------------

/// Reserve `len` bytes of input and return where to write them.
#[no_mangle]
pub extern "C" fn input_ptr(len: usize) -> *mut u8 {
    INPUT.with(|i| {
        let mut b = i.borrow_mut();
        b.clear();
        b.resize(len, 0);
        b.as_mut_ptr()
    })
}

/// Where the last call's JSON result begins. Its length was that call's return
/// value.
#[no_mangle]
pub extern "C" fn result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

/// Token spans for `input[..src_len]`. See [`tokens_json`].
#[no_mangle]
pub extern "C" fn play_tokens(src_len: usize) -> usize {
    with_input(src_len, |src| tokens_json(src).into_bytes())
}

/// Every diagnostic for `input[..src_len]`. See [`check_json`].
#[no_mangle]
pub extern "C" fn play_check(src_len: usize) -> usize {
    with_input(src_len, |src| check_json(src).into_bytes())
}

/// Compile `input[..src_len]` and answer the module's bytes, or the JSON
/// diagnostics of a program that did not compile. See [`compile_result`].
#[no_mangle]
pub extern "C" fn play_compile(src_len: usize) -> usize {
    with_input(src_len, |src| compile_result(src))
}

/// Decode the input as UTF-8, hand it to `f`, and publish what comes back.
///
/// The page sends `TextEncoder` output, so the invalid case is unreachable in
/// practice; it still answers with a diagnostic rather than a panic, because a
/// panic on this target aborts the instance and says nothing. A `src_len`
/// past the reserved buffer gets the same treatment — the length is the
/// host's word, and slicing would panic.
fn with_input(src_len: usize, f: impl FnOnce(&str) -> Vec<u8>) -> usize {
    let too_long = INPUT.with(|i| src_len > i.borrow().len());
    if too_long {
        let d = Diagnostic::error(
            0,
            0,
            "host",
            format!("src_len {src_len} exceeds the input buffer"),
        );
        return publish_json(format!("{{\"diagnostics\":[{}]}}", diag_json(&d)));
    }
    let bytes = INPUT.with(|i| i.borrow()[..src_len].to_vec());
    let result = match std::str::from_utf8(&bytes) {
        Ok(src) => f(src),
        Err(_) => {
            let d = Diagnostic::error(0, 0, "lex", "the source is not valid UTF-8".to_string());
            format!("{{\"diagnostics\":[{}]}}", diag_json(&d)).into_bytes()
        }
    };
    publish(result)
}

/// Store the result and return its length — the calling convention's return
/// value, shared by every entry point.
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

// ---------------------------------------------------------------------------
// Highlighting: the compiler's lexer, and the site's five classes
// ---------------------------------------------------------------------------

/// The words the grammar reads as keywords in position and the lexer hands back
/// as identifiers.
///
/// The same list as `site/app/hl.vyrn`, which colours the snippets the rest of
/// the site shows, and for the same reason: `read`, `modify` and `consume` are
/// the language's whole ownership surface, and a page that invites you to type
/// them cannot render them as ordinary names.
const CONTEXTUAL: &[&str] = &[
    "read", "modify", "consume", "gen", "test", "bench", "panic", "from", "as",
];

/// The CSS class for one lexed item, or `""` for text that carries no colour.
///
/// The classes are the stylesheet's — `k`, `s`, `n`, `c`, `t` — so a snippet
/// typed into the playground is coloured exactly like the same snippet printed on
/// the landing page.
fn class_of(item: &Triv) -> &'static str {
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

/// One coloured run: `[start, length, class]`, in UTF-16 code units.
///
/// UTF-16 because the caller is JavaScript and that is what a JavaScript string
/// index means. Byte offsets would be right for every ASCII program and one
/// character out for the first accented letter in a comment.
fn tokens_json(src: &str) -> String {
    let items = match lexer::lex_with_trivia(src) {
        Ok(items) => items,
        // Mid-keystroke the source is often not lexable (an unclosed string, a
        // stray character). The page falls back to plain text for this frame
        // rather than showing a blank editor.
        Err(d) => return format!("{{\"error\":{}}}", json_str(&d.message)),
    };
    let mut out = String::from("{\"spans\":[");
    let mut byte_at = 0usize;
    let mut u16_at = 0usize;
    let mut first = true;
    for item in &items {
        if item.text.is_empty() {
            continue;
        }
        // Every item's `text` is its VERBATIM source slice (that is what makes
        // the formatter able to reprint a literal), and items arrive in source
        // order. So the extent of a string, a number or a comment is found by
        // walking forward — nothing here re-scans for a closing quote.
        let Some(found) = src[byte_at..].find(item.text.as_str()) else {
            continue;
        };
        let start = byte_at + found;
        u16_at += src[byte_at..start].encode_utf16().count();
        let len = item.text.encode_utf16().count();
        let cls = class_of(item);
        if !cls.is_empty() {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!("[{u16_at},{len},\"{cls}\"]"));
        }
        u16_at += len;
        byte_at = start + item.text.len();
    }
    out.push_str("]}");
    out
}

// ---------------------------------------------------------------------------
// Checking and running
// ---------------------------------------------------------------------------

/// The standard library, as `build.rs` walked it out of `std/`.
mod std_modules {
    include!(concat!(env!("OUT_DIR"), "/std_modules.rs"));
}

/// Load `src` as a one-file program, the way `vyrn run` loads a file.
///
/// The same entry point the CLI uses (`load_warned`), so the page cannot disagree
/// with the compiler about what compiles: imports resolve through a resolver,
/// generators run, the JSON encoders are synthesized, and the move checker sees
/// the finished program.
///
/// The resolver holds `std/` and nothing else. A relative import has nowhere to
/// go and says so; `std/` resolves against a root of `std`, which is what makes
/// the keys `build.rs` wrote the ones the loader asks for.
///
/// THE LOWERING IS INSTALLED HERE, and this is the one place both entry points
/// pass through. Without it the placer never runs, `core::BODIES` stays empty
/// and the emitter's AST dispatch is the whole compiler — a second, weaker
/// compiler on a shipping surface. A program `vyrn run` refuses, the page
/// refuses, in the same sentence. The call writes five slots and is idempotent,
/// so it costs a load nothing worth measuring.
fn load(
    src: &str,
) -> (
    Result<vyrn_frontend::ast::Program, Vec<Diagnostic>>,
    Vec<Diagnostic>,
) {
    vyrn_lower::install();
    let opts = LoadOptions {
        std_root: Some("std".into()),
        aliases: Default::default(),
        alias_base: String::new(),
        audience: None,
        artifacts: None,
    };
    let resolver = MapResolver(
        std_modules::STD
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    vyrn_frontend::load_warned(src, "play.vyrn", &opts, &resolver)
}

fn check_json(src: &str) -> String {
    let (result, warnings) = load(src);
    let mut all = match result {
        Ok(_) => Vec::new(),
        Err(diags) => diags,
    };
    all.extend(warnings);
    format!("{{\"diagnostics\":{}}}", diags_json(&all))
}

/// The program as a wasm module, or the diagnostics of one that did not compile.
///
/// The module is the direct backend's (RFC-0077), which is what `vyrn build
/// --target wasm` writes — no clang, no sysroot, and therefore the one backend
/// that compiles for this target. A warning is not answered here: the page's own
/// `check` reports warnings continuously, and a run does not change what the
/// checker said.
fn compile_result(src: &str) -> Vec<u8> {
    let (result, _warnings) = load(src);
    let program = match result {
        Ok(p) => p,
        Err(diags) => return format!("{{\"diagnostics\":{}}}", diags_json(&diags)).into_bytes(),
    };
    match vyrn_codegen::direct::compile(&program) {
        Ok(bytes) => bytes,
        // A backend refusal is a diagnostic with no position — the same channel
        // the page already renders, rather than a silent abort.
        Err(e) => {
            let d = Diagnostic::error(0, 0, "codegen", e);
            format!("{{\"diagnostics\":[{}]}}", diag_json(&d)).into_bytes()
        }
    }
}

// ---------------------------------------------------------------------------
// JSON, by hand. The crate has one dependency and it is the compiler.
// ---------------------------------------------------------------------------

/// `s` as a JSON string literal, quotes included.
///
/// The BODY is [`vyrn_frontend::codec::escape_into`], RFC-0018's canonical
/// table, which both wasm backends must produce byte for byte. This file used to
/// write the table out again — the third copy of it — and a copy of an escape
/// rule is how a page renders a diagnostic the compiler never wrote.
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

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A span list as `(start, len, class)` triples, parsed back out of the JSON
    /// so the test asserts on what the page receives.
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

    /// A span's own text, taken back out of the source by the offsets the page
    /// would use. If the extents drift, this is what catches it.
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
                ("Int64".to_string(), "t".to_string()),
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
        // Every guide chapter with a run link imports `std/`, so this is the test
        // that keeps those links working.
        let std_import = check_json(
            "import { joinWith } from \"std/strings\"\nfn main() -> Int64 { return 0 }\n",
        );
        assert_eq!(std_import, "{\"diagnostics\":[]}");
        // A relative import has nowhere to go, in the loader's own words.
        let local = check_json("import { f } from \"./other\"\nfn main() -> Int64 { return 0 }\n");
        assert!(local.contains("module not found"), "{local}");
    }

    /// The wasm magic, which is how the page tells a module from the JSON of a
    /// program that did not compile.
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

    /// The one thing this crate decides: which of the two answers comes back.
    /// WHAT the module then does is the compiled route's rule, and `vyrn-cli`'s
    /// parity suite is where it is stated.
    #[test]
    fn a_program_that_does_not_compile_answers_diagnostics_and_not_a_module() {
        let bytes = compile_result("fn main() -> Int64 {\n    return nope\n}\n");
        let json = String::from_utf8(bytes).expect("diagnostics are UTF-8 JSON");
        assert!(json.starts_with("{\"diagnostics\":["), "{json}");
        assert!(json.contains("\"line\":2"), "{json}");
    }

    /// The length is the host's word; a slice past the reserved buffer used to
    /// panic, and a panic on wasm aborts the instance silently. An out-of-range
    /// length must answer through the normal JSON channel instead.
    #[test]
    fn play_compile_length_past_the_buffer_answers_instead_of_aborting() {
        input_ptr(8); // reserve 8 zero bytes
        let n = play_compile(usize::MAX);
        let json = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result_ptr(), n) })
            .into_owned();
        assert!(json.contains("exceeds the input buffer"), "{json}");
    }

    #[test]
    fn play_tokens_length_past_the_buffer_answers_instead_of_aborting() {
        input_ptr(4);
        let n = play_tokens(usize::MAX);
        let json = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(result_ptr(), n) })
            .into_owned();
        assert!(json.contains("exceeds the input buffer"), "{json}");
    }

    /// The `main` `site/app/guidecode.vyrn` adds to every block, so what this
    /// compiles is what the run link carries.
    const MAIN_TAIL: &str = "\nfn main() -> Int64 {\n    print(demo())\n    return 0\n}\n";

    /// Every guide program, as the page hands it over: the file plus the `main`
    /// `guidecode.vyrn` appends.
    ///
    /// Skipped for the two reasons that file skips them (`guidePlayable`): a
    /// block that imports a SIBLING is not one program, and the playground
    /// takes one; a block with no `demo` is not what the appended `main` calls.
    /// The generator chapter writes one of each.
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

    /// The playground compiles every program the book offers to run.
    ///
    /// `site/app/guide.vyrn` already runs each of these while the site builds,
    /// but through `vyrn run`. This asserts the PLAYGROUND agrees, and the
    /// playground is a different process with a different compiler assembled in
    /// it — which is the whole reason the corpus is worth running twice.
    ///
    /// `VYRN_PLAY_DUMP` writes each answer to that directory, one file per
    /// program, so two runs can be compared byte for byte.
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

    /// FNV-1a over a module, so a dump names the bytes without holding them.
    fn sum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h: u64, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
        })
    }

    #[test]
    fn a_control_character_in_output_survives_the_json() {
        assert_eq!(json_str("a\u{1}b\n"), "\"a\\u0001b\\n\"");
    }
}
