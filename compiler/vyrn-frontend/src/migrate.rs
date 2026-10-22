//! Rewrites a removed spelling into its replacement, for `vyrn fix`.
//!
//! The rewrite reads tokens only, because a removed spelling does not parse.
//! It keeps every line where it was, so a diagnostic, a trap line and a wasm
//! module of the rewritten file match the original's. Delete a rewrite once no
//! maintained source holds its spelling.

use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok, Triv, TrivKind};

/// Rewrites each leading-bar enum declaration, `type T = | A(Int64) | B`, into
/// the braced form, `type T = enum { A(Int64), B }`, and returns the new source
/// with the line of each rewritten declaration.
///
/// The variants keep their lines: `enum {` joins the `=`, each `|` goes, a
/// comma follows every variant but the last, and ` }` closes the last one's
/// line. Comments stay where they were. Returns a lex error verbatim.
pub fn brace_enums(source: &str) -> Result<(String, Vec<usize>), Diagnostic> {
    let items = lexer::lex_with_trivia(source)?;
    let toks: Vec<&Triv> = (items.iter())
        .filter(|t| matches!(t.kind, TrivKind::Tok(_)))
        .collect();
    let tok = |k: usize| match toks.get(k).map(|t| &t.kind) {
        Some(TrivKind::Tok(t)) => Some(t),
        _ => None,
    };
    // The char index each line starts at; a token's index is its line's plus its column.
    let starts: Vec<usize> = std::iter::once(0)
        .chain((source.chars().enumerate()).filter_map(|(i, c)| (c == '\n').then_some(i + 1)))
        .collect();
    let at = |t: &Triv| starts[t.start_line - 1] + t.col - 1;
    let end = |t: &Triv| at(t) + t.text(source).chars().count();
    let chars: Vec<char> = source.chars().collect();

    // Each edit replaces the chars in `from..to` with `with`; they never overlap.
    let mut edits: Vec<(usize, usize, &str)> = Vec::new();
    let mut lines = Vec::new();
    let mut k = 0;
    while k < toks.len() {
        let Some(eq) = declaration_eq(&tok, k) else {
            k += 1;
            continue;
        };
        // Each variant as its `|` and its last token. A `|` without a name after it
        // ends the list and stays, so the file is refused where it was.
        let mut bars: Vec<(usize, usize)> = Vec::new();
        let mut bar = eq + 1;
        while tok(bar) == Some(&Tok::Pipe) && matches!(tok(bar + 1), Some(Tok::Ident(_))) {
            let last = variant_end(&tok, bar + 1);
            bars.push((bar, last));
            bar = last + 1;
        }
        k = bar;
        let Some(&(_, final_tok)) = bars.last() else {
            continue;
        };
        lines.push(toks[eq].start_line);
        edits.push((end(toks[eq]), end(toks[eq]), " enum {"));
        for &(bar, last) in &bars {
            // The `|` and the blanks after it, up to the variant's name.
            let from = at(toks[bar]);
            let mut to = from + 1;
            while matches!(chars.get(to), Some(' ' | '\t')) {
                to += 1;
            }
            edits.push((from, to, ""));
            let after = if last == final_tok { " }" } else { "," };
            edits.push((end(toks[last]), end(toks[last]), after));
        }
    }

    let mut out = chars;
    for (from, to, with) in edits.into_iter().rev() {
        out.splice(from..to, with.chars());
    }
    Ok((out.into_iter().collect(), lines))
}

/// The index of the `=` of a `type Name<..> =` that starts at token `k`.
fn declaration_eq<'a>(tok: &impl Fn(usize) -> Option<&'a Tok>, k: usize) -> Option<usize> {
    if tok(k) != Some(&Tok::Type) || !matches!(tok(k + 1), Some(Tok::Ident(_))) {
        return None;
    }
    let mut i = k + 2;
    if tok(i) == Some(&Tok::Lt) {
        while !matches!(tok(i), Some(Tok::Gt) | None) {
            i += 1;
        }
        i += 1;
    }
    (tok(i) == Some(&Tok::Eq)).then_some(i)
}

/// The index of the last token of the variant whose name is token `name`: the
/// name, or the `)` that closes its payload.
fn variant_end<'a>(tok: &impl Fn(usize) -> Option<&'a Tok>, name: usize) -> usize {
    if tok(name + 1) != Some(&Tok::LParen) {
        return name;
    }
    let mut depth = 0usize;
    let mut i = name + 1;
    // Each turn moves `i` one token on; `None` past the end stops it.
    loop {
        match tok(i) {
            Some(Tok::LParen) => depth += 1,
            Some(Tok::RParen) => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            None => return i - 1,
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::brace_enums;

    fn braced(src: &str) -> String {
        brace_enums(src).expect("lexes").0
    }

    #[test]
    fn a_one_line_enum_closes_on_its_line() {
        assert_eq!(
            braced("type L = | A | B(Int64, Array<X>) where value > 0\n"),
            "type L = enum { A, B(Int64, Array<X>) } where value > 0\n"
        );
    }

    #[test]
    fn a_variant_per_line_keeps_its_line_and_its_comment() {
        let src = "/// doc\nexport type S<T> =\n    | C(fn(T) -> T) // c\n    | P\n\nfn f() {}\n";
        let want =
            "/// doc\nexport type S<T> = enum {\n    C(fn(T) -> T), // c\n    P }\n\nfn f() {}\n";
        assert_eq!(braced(src), want);
        assert_eq!(brace_enums(src).unwrap().1, vec![2]);
    }

    #[test]
    fn other_declarations_and_the_braced_form_are_left_alone() {
        let src = "import type { A } from \"./a\"\ntype R = { a: Int64 }\ntype E = enum { A }\nlet x = a | b\n";
        assert_eq!(braced(src), src);
    }
}
