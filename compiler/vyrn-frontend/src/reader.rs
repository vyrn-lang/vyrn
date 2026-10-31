//! What a generator reads of Vyrn source: the nodes of the one parser's grammar, each
//! with its byte range of the text. `vyrnModule` and `vyrnType` answer these.
//!
//! A node is a rule of `vyrn.syntax` that the recognizer's `Hook` names. A declaration
//! that does not parse has no node, a token no declaration starts with is an `Item` with
//! no kids, and text that does not lex has no nodes at all. A generator copies such text,
//! and the compile of what it wrote refuses it at its own line and column.

use crate::lexer::{self, TrivKind};
use crate::recognizer::{nodes, Spanned, Start};

/// One node. `kids` index the nodes of the answer, in source order. A rule's node
/// follows its kids, so the last node of a non-empty answer is the root.
#[derive(Debug, PartialEq, Eq)]
pub struct Node {
    pub rule: String,
    pub start: usize,
    pub stop: usize,
    pub kids: Vec<usize>,
}

/// Returns the nodes of `src` read from `start`.
pub fn read(src: &str, start: Start) -> Vec<Node> {
    let Ok(scan) = lexer::scan(src) else {
        return Vec::new();
    };
    let spans: Vec<_> = (scan.items.iter())
        .filter(|i| !matches!(i.kind, TrivKind::Comment))
        .map(|i| i.span.clone())
        .collect();
    let toks = lexer::tokens(scan);
    let mut out: Vec<Node> = Vec::new();
    // The nodes with no parent yet.
    let mut roots: Vec<usize> = Vec::new();
    for n in nodes(&toks, start) {
        let Spanned {
            hook,
            from,
            to,
            split,
            ..
        } = n;
        // A rule that matched no token, as a receiver with no capability does.
        if from == to && !split {
            continue;
        }
        let (from, to) = (from as usize, to as usize);
        let start = spans[from].start;
        let stop = if split {
            spans[to].start + 1
        } else {
            spans[to - 1].end
        };
        let at = roots.partition_point(|&r| out[r].start < start);
        let kids = roots.split_off(at);
        roots.push(out.len());
        out.push(Node {
            rule: format!("{hook:?}"),
            start,
            stop,
            kids,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(src: &str, start: Start) -> Vec<String> {
        (read(src, start).iter())
            .map(|n| format!("{} `{}` {:?}", n.rule, &src[n.start..n.stop], n.kids))
            .collect()
    }

    #[test]
    fn a_generic_type_splits_its_close() {
        assert_eq!(
            rows("Map<String, Array<T>>", Start::Type),
            [
                "Type `String` []",
                "TypeName `T` []",
                "Type `T` [1]",
                "Type `Array<T>` [2]",
                "Type `Map<String, Array<T>>` [0, 3]",
            ]
        );
    }

    #[test]
    fn a_hole_has_no_nodes_outside_a_quote() {
        for src in ["\0", "fn \0() {}", "fn f(\0) {}", "type T = { \0 }"] {
            assert_eq!(rows(src, Start::Module), Vec::<String>::new(), "{src:?}");
        }
        assert_eq!(rows("\0", Start::Type), Vec::<String>::new());
        assert_eq!(rows("{ \0 }", Start::Type), Vec::<String>::new());
    }

    #[test]
    fn a_type_stops_before_its_where() {
        assert_eq!(
            rows("Int64 where value > 0", Start::Type),
            ["Type `Int64` []"]
        );
    }

    #[test]
    fn a_record_holds_its_fields() {
        assert_eq!(
            rows("{ a: Int64 }", Start::Type),
            [
                "Type `Int64` []",
                "Field `a: Int64` [0]",
                "Type `{ a: Int64 }` [1]",
            ]
        );
    }

    #[test]
    fn nothing_and_a_mistake_have_no_nodes() {
        assert_eq!(rows("", Start::Module), Vec::<String>::new());
        assert_eq!(rows("", Start::Type), Vec::<String>::new());
        assert_eq!(rows("\"open", Start::Module), Vec::<String>::new());
    }

    #[test]
    fn a_type_that_stops_short_has_no_nodes() {
        assert_eq!(rows("{ a: Stri;ng }", Start::Type), Vec::<String>::new());
        assert_eq!(rows("Array<Int64", Start::Type), Vec::<String>::new());
    }

    #[test]
    fn a_stray_token_is_an_item_with_no_kids() {
        let src =
            "import { a as b } from \"./c\"\nprops\nexport fn f(p: Int64) -> Bool { oops( }\n";
        assert_eq!(
            rows(src, Start::Module),
            [
                "ImportName `a as b` []",
                "Source `\"./c\"` []",
                "Import `import { a as b } from \"./c\"` [0, 1]",
                "Item `import { a as b } from \"./c\"` [2]",
                "Item `props` []",
                "DeclName `f` []",
                "Type `Int64` []",
                "Param `p: Int64` [6]",
                "Params `(p: Int64)` [7]",
                "Type `Bool` []",
                "Ret `-> Bool` [9]",
                "SkimBody `{ oops( }` []",
                "FnDecl `fn f(p: Int64) -> Bool { oops( }` [5, 8, 10, 11]",
                "Export `export fn f(p: Int64) -> Bool { oops( }` [12]",
                "Item `export fn f(p: Int64) -> Bool { oops( }` [13]",
                "Module `import { a as b } from \"./c\"\nprops\nexport fn f(p: Int64) -> Bool { oops( }` [3, 4, 14]",
            ]
        );
    }
}
