//! Every refusal the editor shows is pinned to a token, the kernel's included. The
//! ownership judgments sit above `vyrn-frontend`, so the pin lives in this crate:
//! `vyrn_frontend::analyze_judged` with `vyrn_lower::JUDGE`, the call `vyrn-lsp`
//! makes. A refusal keeps its line and gains a 1-based column span over a real
//! token, never `0`, which the LSP squiggles as the whole line.

use std::path::{Path, PathBuf};

fn dirs() -> Vec<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    vec![base.join("refusals"), base.join("unlicensed")]
}

fn programs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The first diagnostic the editor gives for `src`.
fn first(src: &str) -> Option<vyrn_frontend::diagnostics::Diagnostic> {
    vyrn_frontend::analyze_judged(src, None, &vyrn_lower::JUDGE)
        .diagnostics
        .into_iter()
        .next()
}

#[test]
fn every_refusal_the_editor_shows_is_pinned_to_a_token() {
    let mut bad: Vec<String> = Vec::new();
    for p in programs() {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&p).expect("read the program");
        let Some(d) = first(&src) else {
            bad.push(format!("{name}: the editor shows nothing"));
            continue;
        };
        if d.line == 0 {
            bad.push(format!("{name}: no line"));
            continue;
        }
        if d.col == 0 || d.end_col <= d.col {
            bad.push(format!(
                "{name}: line {} has no column ({}:{}) — {}",
                d.line,
                d.col,
                d.end_col,
                d.message.lines().next().unwrap_or("")
            ));
            continue;
        }
        let line = src.replace("\r\n", "\n");
        let Some(text) = line.lines().nth(d.line - 1) else {
            bad.push(format!("{name}: line {} is past the end", d.line));
            continue;
        };
        let width = text.chars().count();
        if d.col > width || d.end_col > width + 1 {
            bad.push(format!(
                "{name}: {}:{}..{} is off the end of a {width}-character line",
                d.line, d.col, d.end_col
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "a refusal a reader gets has lost its column:\n  {}",
        bad.join("\n  ")
    );
}

/// The pinner's identifier path.
#[test]
fn unknown_variable_pinned_to_ident() {
    let src = "\
fn main() -> Int64 {
    return x;
}
";
    let d = first(src).expect("an unknown-variable diagnostic");
    // `    return x;`: `x` at col 12.
    assert_eq!(d.line, 2);
    assert_eq!(
        d.col, 12,
        "pinned to the `x` identifier, not col 0 (whole line)"
    );
    assert_eq!(d.end_col, 13);
}

/// The pinner's keyword path: the message's first backticked token is `if`, a reserved
/// word the keyword column map resolves.
#[test]
fn if_condition_pinned_to_if_keyword() {
    let src = "\
fn main() -> Int64 {
    if 5 {
        print(1);
    }
    return 0;
}
";
    let d = first(src).expect("an if-condition diagnostic");
    // `    if 5 {`: `if` at cols 5-6.
    assert_eq!(d.line, 2);
    assert_eq!(
        d.col, 5,
        "pinned to the `if` keyword, not col 0 (whole line)"
    );
    assert_eq!(d.end_col, 7);
}

/// The core states the rule where it builds the switch, and the pinner reads the
/// keyword off the message.
#[test]
fn match_exhaustiveness_pinned_to_match_keyword() {
    let src = "type T = | A(Int64) | B
fn f(x: T) -> Int64 {
    let r = match x {
        A(n) => n,
    }
    return r
}
fn main() -> Int64 { return 0 }
";
    let d = first(src).expect("a non-exhaustive-match diagnostic");
    assert!(d.message.contains("missing variant"), "{}", d.message);
    // `    let r = match x {`: `match` at cols 13-17.
    assert_eq!(d.line, 3);
    assert_eq!(
        d.col, 13,
        "pinned to the `match` keyword, not col 0 (whole line)"
    );
    assert_eq!(d.end_col, 18);
}

/// Prints the pinned columns of the corpus:
/// `cargo test -p vyrn-cli --test columns -- --ignored --nocapture`.
#[test]
#[ignore]
fn the_pinned_columns_of_the_refusal_corpora() {
    for p in programs() {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&p).expect("read the program");
        match first(&src) {
            Some(d) => println!(
                "{name} {}:{}:{} [{}] {}",
                d.line,
                d.col,
                d.end_col,
                d.stage,
                d.message.lines().next().unwrap_or("")
            ),
            None => println!("{name} — nothing"),
        }
    }
}
