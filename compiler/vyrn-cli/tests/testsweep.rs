//! The kernel sweep over programs that exist only as string literals in test
//! sources, which the `.vyrn` corpus sweep cannot see.
//!
//! Each Vyrn-looking literal runs `vyrn check` twice, with the kernel and with
//! `VYRN_NO_KERNEL=1`. A program only the first refuses is the finding. A
//! fragment, a template or a program written to be refused answers the same both
//! times, so the pair of runs needs no exception list.
//!
//! Extraction is partial on purpose: a literal that does not reassemble does not
//! parse and is skipped, so a bad lift is a miss, never a false alarm. The count
//! has a floor so a lift that stops finding programs fails. A program the
//! monomorphization limit refuses without the kernel too is invisible here; its
//! own test reads the message.
//!
//! Ignored because it spawns two processes per program.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the sweep reads test sources from.
///
/// `vyrn-frontend` does not link the kernel, and `vyrn-lsp` installs it only in
/// `main`, which its unit tests do not call. A test there that asserts a program
/// is clean asserts only that the checker had nothing to say, so their programs
/// are lifted here, where the kernel runs.
fn test_sources() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        "tests",
        "../vyrn-frontend/tests",
        "../vyrn-frontend/src",
        "../vyrn-lsp/tests",
        "../vyrn-lsp/src",
    ]
    .iter()
    .map(|d| root.join(d))
    .collect()
}

/// Undoes Rust's string escapes, including the line continuation: a `\` at the
/// end of a line eats the newline and the indentation after it.
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
            // `\u{..}` and anything else unmodelled: skip the literal, never guess.
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
/// reassembles into the program the test runs.
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

/// Refusal sentences only the kernel states, one needle each, with the census row
/// that licensed moving the rule out of `movecheck.rs`.
///
/// With the kernel off, the checker does not refuse these, so the two runs are
/// meant to disagree about them. The exemption is per rule, not per program; add
/// an entry only with its census row.
const LEFT_THE_CHECKER: &[(&str, &str)] = &[
    ("was taken out of", "row 04, a whole read after a hole"),
    ("still reads out of it", "row 05, a write ends an alias"),
    ("is used here but was already consumed by", "row 06, rule 1"),
    ("an element is not a place a take reaches", "row 08"),
    ("has nothing to take", "row 09"),
    ("may not be passed to a `consume` parameter via", "row 12"),
    ("may not be returned from a closure", "row 28"),
    (
        "may not be returned — it is",
        "rows 15, 16 and 18, rule 3 at the return",
    ),
    ("may not be returned from an exported function", "row 17"),
    (
        "may not be stored into",
        "rows 01, 02, 03, 27 and 34, rule 2 at a store",
    ),
    ("is dropped here but was already consumed by", "row 20"),
    ("may not be dropped — it is", "row 21"),
    ("was moved here into", "row 07, rule 1's move"),
    ("may not be put into", "row 19, a borrow into a constructor"),
    (
        "inside a loop, so it would be used again",
        "row 25, rule 1 across a back edge",
    ),
    (
        "must be a value of its own",
        "row 26, a rebuilding builtin takes its receiver",
    ),
    (
        "the `for .. in consume` loop",
        "rows 10, 11 and 29, the take a loop writes",
    ),
    (
        "may not be dropped — `",
        "row 22, a `drop` of a binding a take left a hole in",
    ),
    (
        "may not be captured by a closure that outlives this call",
        "row 24, a borrow captured by a closure that outlives the call",
    ),
    // A rule the checker never had: the core mints `@borrow`, so a refusal that
    // quotes it is the kernel's alone.
    // `tests/check/pass_a_lender_forwarded_through_an_aggregate.vyrn` pins its
    // one program.
    (
        "`@borrow` may not be returned",
        "row 17's other half, a borrow an arm yields",
    ),
    // A rule the checker never had: a declared `release` reads every payload.
    // `refusals.rs`'s `a_payload_of_a_type_that_declares_release_is_not_handed_on`
    // pins it.
    (
        "may not be handed to a `consume` parameter: ",
        "m7-hole, a declared release reads every payload",
    ),
];

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
    let mut files: Vec<PathBuf> = test_sources()
        .into_iter()
        .flat_map(|d| {
            std::fs::read_dir(&d)
                .unwrap_or_else(|e| panic!("read {}: {e}", d.display()))
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "rs"))
                .collect::<Vec<_>>()
        })
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
            // The crate name disambiguates: two directories hold a `contracts.rs`.
            let crate_of = f
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let stem = format!("{crate_of}/{}", f.file_stem().unwrap().to_string_lossy());
            let path = dir.join(format!("{}-{i}.vyrn", stem.replace('/', "-")));
            std::fs::write(&path, &s).unwrap();
            let (without, _) = check(&path, true);
            if !without {
                // Not a program, or one written to be refused: not the kernel's call.
                continue;
            }
            programs += 1;
            let (with, msg) = check(&path, false);
            if !with && !LEFT_THE_CHECKER.iter().any(|(n, _)| msg.contains(n)) {
                refused.push(format!("{stem} literal #{i}:\n{msg}\n--- source ---\n{s}"));
            }
        }
    }

    assert!(
        refused.is_empty(),
        "{} program(s) a test writes are accepted without the kernel and refused with it:\n\n{}",
        refused.len(),
        refused.join("\n\n")
    );
    println!(
        "{programs} programs lifted from {} test sources",
        files.len()
    );
    // A floor under the measured count, not a target: it moves when test sources
    // are added or deleted, never to hide a lift that stopped working.
    assert!(
        programs >= 300,
        "the lift found only {programs} runnable programs across {} test sources — \
         it stopped reassembling them",
        files.len()
    );
}
