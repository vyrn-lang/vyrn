# std/grammar.editor

Editor support for the `.syntax` files `std/grammar` reads.

No program links this module, so it costs a program nothing. A tool finds it
beside `std/grammar` by its name.

## vyrnCheckGrammar

```vyrn
fn vyrnCheckGrammar(root: String, path: String) -> Code
```

What the grammar at `path` refuses: the report `syntax` gives where it does not
load or does not lower, at its line and column. `root` is the same path.

## vyrnDefinitionGrammar

```vyrn
fn vyrnDefinitionGrammar(root: String, path: String, line: Int64, col: Int64) -> String
```

The head of the rule a reference at 1-based `line` and `col` of the grammar at
`path` names, as a `path:line:col` anchor, or "" where the name is no rule.

## vyrnHoverGrammar

```vyrn
fn vyrnHoverGrammar(root: String, path: String, line: Int64, col: Int64) -> String
```

The line that defines the rule a reference at 1-based `line` and `col` of the
grammar at `path` names, or "".

## vyrnCompleteGrammar

```vyrn
fn vyrnCompleteGrammar(root: String, path: String, line: Int64, col: Int64) -> String
```

Completion items at 1-based `line` and `col` of the grammar at `path`: the
rules it defines, then the loader's built-ins. Returns a JSON array of
`{label, kind, detail, from}`.
