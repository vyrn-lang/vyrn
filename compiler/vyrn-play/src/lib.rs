//! The Vyrn front end as a wasm module, for the playground on the website.
//!
//! Three questions, all answered by the compiler itself:
//!
//!   - [`play_tokens`] — where every token is and what kind it is, so the page
//!     colours code with the compiler's own lexer and not a second one written in
//!     JavaScript.
//!   - [`play_check`] — every diagnostic, structured, from the real loader.
//!   - [`play_run`] — the tree-walking interpreter, which is the reference
//!     semantics: what the page prints is what `vyrn run` prints.
//!
//! WHAT THIS CRATE ADDS is a calling convention and a JSON writer. Not one
//! language decision is made here. `vyrn-frontend` compiles for
//! `wasm32-unknown-unknown` unchanged; the only edits it needed were three
//! `cfg` switches at the host boundary (output, input, the clock — see
//! `vyrn_frontend::playhost`), because a browser tab has no stdout, no stdin and
//! no clock a module may call.
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
//!   2. an entry point is called with the lengths; it returns the result length.
//!   3. `result_ptr()` says where the result is. It is UTF-8 JSON.
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
    with_input(src_len, |src| tokens_json(src))
}

/// Every diagnostic for `input[..src_len]`. See [`check_json`].
#[no_mangle]
pub extern "C" fn play_check(src_len: usize) -> usize {
    with_input(src_len, |src| check_json(src))
}

/// Run `input[..src_len]`, with `input[src_len..src_len + stdin_len]` as stdin
/// and `now_ms` as the wall clock. See [`run_json`].
#[no_mangle]
pub extern "C" fn play_run(src_len: usize, stdin_len: usize, now_ms: f64) -> usize {
    let stdin = INPUT.with(|i| i.borrow()[src_len..src_len + stdin_len].to_vec());
    with_input(src_len, |src| run_json(src, &stdin, now_ms as i64))
}

/// Decode the input as UTF-8, hand it to `f`, and publish what comes back.
///
/// The page sends `TextEncoder` output, so the invalid case is unreachable in
/// practice; it still answers with a diagnostic rather than a panic, because a
/// panic on this target aborts the instance and says nothing.
fn with_input(src_len: usize, f: impl FnOnce(&str) -> String) -> usize {
    let bytes = INPUT.with(|i| i.borrow()[..src_len].to_vec());
    let json = match std::str::from_utf8(&bytes) {
        Ok(src) => f(src),
        Err(_) => {
            let d = Diagnostic::error(0, 0, "lex", "the source is not valid UTF-8".to_string());
            format!("{{\"diagnostics\":[{}]}}", diag_json(&d))
        }
    };
    RESULT.with(|r| {
        let mut b = r.borrow_mut();
        *b = json.into_bytes();
        b.len()
    })
}

// ---------------------------------------------------------------------------
// Highlighting: the compiler's lexer, and the site's five classes
// ---------------------------------------------------------------------------

/// The words the grammar reads as keywords in position and the lexer hands back
/// as identifiers.
///
/// The same list as `site/app/hl.vyrn`, which colours the snippets the rest of
/// the site shows, and for the same reason: `read`, `modify`, `consume` and
/// `share` are the language's whole ownership surface, and a page that invites
/// you to type them cannot render them as ordinary names.
const CONTEXTUAL: &[&str] = &[
    "read", "modify", "consume", "share", "gen", "test", "bench", "panic", "from", "as",
];

/// The CSS class for one lexed item, or `""` for text that carries no colour.
///
/// The classes are the stylesheet's — `k`, `s`, `n`, `c`, `t` — so a snippet
/// typed into the playground is coloured exactly like the same snippet printed on
/// the landing page.
fn class_of(item: &Triv) -> &'static str {
    match &item.kind {
        TrivKind::Comment | TrivKind::Doc => "c",
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
fn load(
    src: &str,
) -> (
    Result<vyrn_frontend::ast::Program, Vec<Diagnostic>>,
    Vec<Diagnostic>,
) {
    let opts = LoadOptions {
        std_root: Some("std".into()),
        aliases: Default::default(),
        alias_base: String::new(),
        audience: None,
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

fn run_json(src: &str, stdin: &[u8], now_ms: i64) -> String {
    let (result, warnings) = load(src);
    let program = match result {
        Ok(p) => p,
        Err(diags) => return format!("{{\"diagnostics\":{}}}", diags_json(&diags)),
    };
    arm_host(stdin, now_ms);
    let outcome = vyrn_frontend::interp::run_with_args(&program, &[]);
    let (stdout, mut stderr) = drain_host();
    // `vyrn run`'s own shape: a trap is `error: <message>` on stderr and a
    // failing exit code; `main`'s return value is the exit code otherwise, low
    // byte only, as a process exit code is.
    let exit = match outcome {
        Ok(code) => code & 0xff,
        Err(e) => {
            stderr.push_str(&format!("error: {e}\n"));
            1
        }
    };
    format!(
        "{{\"stdout\":{},\"stderr\":{},\"exitCode\":{},\"diagnostics\":{}}}",
        json_str(&stdout),
        json_str(&stderr),
        exit,
        diags_json(&warnings)
    )
}

/// Point the interpreter's host boundary at the page's stdin and clock.
///
/// A no-op off `wasm32-unknown-unknown`: there the interpreter reads the real
/// stdin and the real clock, which is what the host-side tests below exercise.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn arm_host(stdin: &[u8], now_ms: i64) {
    vyrn_frontend::playhost::arm(stdin, now_ms);
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn arm_host(_stdin: &[u8], _now_ms: i64) {}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn drain_host() -> (String, String) {
    vyrn_frontend::playhost::drain()
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn drain_host() -> (String, String) {
    (String::new(), String::new())
}

// ---------------------------------------------------------------------------
// JSON, by hand. The crate has one dependency and it is the compiler.
// ---------------------------------------------------------------------------

/// `s` as a JSON string literal, quotes included.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Every other control character, and nothing else: a JSON string may
            // carry any other codepoint literally, and escaping them all would
            // quadruple a page of program output.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
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

    #[test]
    fn a_program_that_calls_the_standard_library_runs() {
        let json = run_json(
            "import { joinWith } from \"std/strings\"\n\
             fn main() -> Int64 {\n    print(joinWith([\"a\", \"b\"], \"-\"))\n    return 0\n}\n",
            b"",
            0,
        );
        assert!(json.contains("\"exitCode\":0"), "{json}");
    }

    #[test]
    fn a_run_reports_the_exit_code_main_returned() {
        // Output goes to the real stdout on the host, so this asserts the shape
        // and the code. The browser check is what proves the buffers.
        let json = run_json("fn main() -> Int64 {\n    return 7\n}\n", b"", 0);
        assert!(json.contains("\"exitCode\":7"), "{json}");
    }

    #[test]
    fn a_trap_becomes_error_on_stderr_and_a_failing_code() {
        let json = run_json(
            "fn main() -> Int64 {\n    panic(\"nope\")\n    return 0\n}\n",
            b"",
            0,
        );
        assert!(json.contains("\"exitCode\":1"), "{json}");
        assert!(json.contains("error: nope"), "{json}");
    }

    #[test]
    fn a_program_too_deep_stops_with_the_limit_every_engine_shares() {
        let src = "fn down(n: Int64) -> Int64 {\n    if n <= 0 { return 0 }\n    return down(n - 1)\n}\nfn main() -> Int64 {\n    return down(5000)\n}\n";
        let json = run_json(src, b"", 0);
        assert!(json.contains(&vyrn_frontend::trap::call_depth()), "{json}");
    }

    #[test]
    fn a_control_character_in_output_survives_the_json() {
        assert_eq!(json_str("a\u{1}b\n"), "\"a\\u0001b\\n\"");
    }
}
