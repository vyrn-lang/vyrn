//! RFC-0125 §3 M3 — the by-default sweep, over the programs TESTS write.
//!
//! The sweep that flipped the kernel on read every `.vyrn` file in the
//! checkout, 466 of them, and fixed the eleven it refused. It could not see a
//! program that exists only as a Rust string literal, and three times now that
//! is where the next one was: `parity.rs`'s `wrap<T>` (the twelfth), then
//! `fallible.rs`'s `pass(h: Http)` and `limits.rs`'s `[x, x]`, which turned CI
//! red on four platforms at once because a test binary was their only reader.
//!
//! This file is the same sweep with the same rule, over the other corpus. It
//! reads every `tests/*.rs`, lifts the Vyrn-looking string literals out, and
//! runs `vyrn check` on each one twice — once with the kernel and once with
//! `VYRN_NO_KERNEL=1`. A program only the first refuses is the finding, and
//! nothing else is.
//!
//! **Why that pair of runs is the whole filter.** Most of what comes out of a
//! test file is not a program: a `format!` template with `{PRELUDE}` still in
//! it, a fragment, a program written to BE refused. Every one of those answers
//! the same way twice — a parse error is a parse error with the kernel off —
//! so the comparison drops it without a list of exceptions to maintain. Only a
//! program the compiler accepted before the kernel and refuses after it can
//! reach the report, which is exactly the class that goes red in CI.
//!
//! Extraction is deliberately partial. A literal that will not reassemble is a
//! literal that will not parse, and an unparsed literal is skipped, so the
//! failure mode of a bad lift is a miss and never a false alarm. The count is
//! asserted so that a lift that silently stops finding programs is itself a
//! failure.
//!
//! **What it catches, measured against the four it was written for.** Put the
//! defects back and this reports three of them: `fallible.rs`'s two and
//! `limits.rs`'s `[x, x]`. The fourth it cannot see, for the reason the filter
//! is built on — `limits.rs`'s `polymorphic_recursion..` programs are refused
//! WITHOUT the kernel too, by the monomorphization limit they exist to test,
//! so no pair of runs disagrees about them. That one is caught where it should
//! be: its own assertion reads the message, and a kernel refusal is not the
//! needle it looks for.
//!
//! Ignored, like the other corpus walks: it spawns two processes per program.
//! `cargo test -p vyrn-cli --test testsweep -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Undo Rust's string escapes, and its line continuation: a `\` at the end of
/// a line eats the newline and the indentation after it, which is how these
/// files wrap a long program.
fn unescape(lit: &str) -> Option<String> {
    let mut out = String::with_capacity(lit.len());
    let mut it = lit.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '\\' => out.push('\\'),
            '\n' => {
                while it.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                    it.next();
                }
            }
            // `\u{..}` and anything else this lift does not model: give up on
            // the literal rather than guess at it.
            _ => return None,
        }
    }
    Some(out)
}

/// Every `"..."` literal in a Rust source, with `//` and `/* */` comments and
/// `'"'` character literals stepped over. Raw strings are skipped: no test
/// writes a Vyrn program as one.
fn literals(src: &str) -> Vec<String> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            // A char literal, so that `'"'` does not open a string.
            '\'' if b.get(i + 1) == Some(&'"') && b.get(i + 2) == Some(&'\'') => i += 3,
            'r' if b.get(i + 1) == Some(&'"') || b.get(i + 1) == Some(&'#') => {
                i += 1;
                while b.get(i) == Some(&'#') {
                    i += 1;
                }
                if b.get(i) == Some(&'"') {
                    i += 1;
                    while i < b.len() && b[i] != '"' {
                        i += 1;
                    }
                }
                i += 1;
            }
            '"' => {
                i += 1;
                let start = i;
                while i < b.len() {
                    if b[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == '"' {
                        break;
                    }
                    i += 1;
                }
                out.push(b[start..i.min(b.len())].iter().collect());
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// `const NAME: &str = "..."` in the same file, so a `format!("{PRELUDE}..")`
/// reassembles into the program the test actually runs. That is where
/// `fallible.rs`'s defect lived: the protocol is a `const` and the function
/// under test is a template around it.
fn consts(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, _) in src.match_indices("const ") {
        let rest = &src[i + 6..];
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            continue;
        }
        let Some(eq) = rest.find('=') else { continue };
        let Some(q) = rest[eq..].find('"') else {
            continue;
        };
        let lits = literals(&rest[eq + q..]);
        if let Some(v) = lits.first().and_then(|l| unescape(l)) {
            out.push((format!("{{{name}}}"), v));
        }
    }
    out
}

/// Whether a lifted literal is worth a `vyrn check`: it declares something and
/// spans lines. Anything else is a message, a path or a needle.
fn looks_like_a_program(s: &str) -> bool {
    s.contains('\n')
        && (s.contains("fn ") || s.contains("type ") || s.contains("export "))
        && s.len() > 40
}

fn check(path: &Path, no_kernel: bool) -> (bool, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.arg("check").arg(path);
    if no_kernel {
        c.env("VYRN_NO_KERNEL", "1");
    } else {
        c.env_remove("VYRN_NO_KERNEL");
    }
    let out = c.output().expect("vyrn check");
    let all = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    (out.status.success(), all)
}

#[test]
#[ignore = "spawns two `vyrn check` runs per lifted program; run explicitly: \
            cargo test -p vyrn-cli --test testsweep -- --ignored"]
fn no_program_a_test_writes_is_accepted_without_the_kernel_and_refused_with_it() {
    let dir = std::env::temp_dir().join("vyrn-testsweep");
    std::fs::create_dir_all(&dir).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(tests_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no test sources found");

    let mut programs = 0usize;
    let mut refused: Vec<String> = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let subs = consts(&src);
        for (i, lit) in literals(&src).into_iter().enumerate() {
            let Some(mut s) = unescape(&lit) else {
                continue;
            };
            if !looks_like_a_program(&s) {
                continue;
            }
            // A `format!` template doubles its braces; undo that, then put the
            // file's own `const`s back where their placeholders were.
            if s.contains("{{") {
                s = s.replace("{{", "{").replace("}}", "}");
            }
            for (name, value) in &subs {
                s = s.replace(name.as_str(), value);
            }
            let stem = f.file_stem().unwrap().to_string_lossy().to_string();
            let path = dir.join(format!("{stem}-{i}.vyrn"));
            std::fs::write(&path, &s).unwrap();
            let (without, _) = check(&path, true);
            if !without {
                // Not a program, or a program written to be refused. Either
                // way the kernel is not what decides it.
                continue;
            }
            programs += 1;
            let (with, msg) = check(&path, false);
            if !with {
                refused.push(format!("{stem} literal #{i}:\n{msg}\n--- source ---\n{s}"));
            }
        }
    }

    assert!(
        refused.is_empty(),
        "{} program(s) a test writes are accepted without the kernel and refused with it \
         (RFC-0125 §3 M3, the by-default sweep):\n\n{}",
        refused.len(),
        refused.join("\n\n")
    );
    // A lift that stops finding programs must fail rather than pass quietly.
    // The floor is under the count this ran at, not a target: 169 literals
    // across 71 test sources reassemble into something the compiler accepts.
    assert!(
        programs >= 150,
        "the lift found only {programs} runnable programs across {} test sources — \
         it stopped reassembling them",
        files.len()
    );
}
