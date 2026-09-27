//! Corpus-wide guarantees for the formatter: over `examples/`,
//! `examples/lib/` and `std/`, `fmt` is the identity, idempotent, and keeps the
//! token sequence modulo `Semi`.

use std::path::PathBuf;

use vyrn_frontend::lexer::{lex, Tok};

fn corpus() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["examples", "examples/lib", "std"] {
        let d = root.join(dir);
        let rd = std::fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("corpus dir {} unreadable: {e}", d.display()));
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(
        files.len() >= 45,
        "expected the full corpus, found {}",
        files.len()
    );
    files
}

/// Reads a corpus file as LF: a Windows checkout may convert line endings, and
/// the formatter is defined over LF.
fn read_lf(p: &PathBuf) -> String {
    std::fs::read_to_string(p)
        .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        .replace("\r\n", "\n")
}

fn tokens_modulo_semi(src: &str) -> Vec<Tok> {
    lex(src)
        .expect("corpus file lexes")
        .into_iter()
        .map(|t| t.tok)
        .filter(|t| !matches!(t, Tok::Semi | Tok::Eof))
        .collect()
}

#[test]
fn corpus_is_canonical() {
    let mut drifted = Vec::new();
    for f in corpus() {
        let src = read_lf(&f);
        let formatted = vyrn_frontend::fmt(&src)
            .unwrap_or_else(|e| panic!("fmt {} failed: {}", f.display(), e.render()));
        if formatted != src {
            drifted.push(f.display().to_string());
        }
    }
    assert!(
        drifted.is_empty(),
        "these corpus files are not canonical (run `vyrn fmt`):\n  {}",
        drifted.join("\n  ")
    );
}

#[test]
fn corpus_fmt_is_idempotent() {
    for f in corpus() {
        let src = read_lf(&f);
        let once = vyrn_frontend::fmt(&src).expect("fmt");
        let twice = vyrn_frontend::fmt(&once).expect("fmt again");
        assert_eq!(once, twice, "fmt is not idempotent on {}", f.display());
    }
}

#[test]
fn corpus_fmt_preserves_tokens_modulo_semi() {
    for f in corpus() {
        let src = read_lf(&f);
        let formatted = vyrn_frontend::fmt(&src).expect("fmt");
        assert_eq!(
            tokens_modulo_semi(&src),
            tokens_modulo_semi(&formatted),
            "fmt changed the token sequence of {}",
            f.display()
        );
    }
}

/// Formatting needs only lexing, so format-on-save works on a half-typed buffer.
#[test]
fn parse_error_file_still_formats() {
    let src = "fn main() -> Int64 {\nlet x =\nreturn 0\n}\n";
    let out = vyrn_frontend::fmt(src).expect("lexable input formats even if unparseable");
    assert_eq!(out, "fn main() -> Int64 {\n    let x =\n    return 0\n}\n");
}
