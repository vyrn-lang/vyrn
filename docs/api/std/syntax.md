# std/syntax

A parser written from a `.syntax` grammar, as ordinary Vyrn code.

`syntax(path)` is a generator import. It writes one `parse<Start>` for each rule
no other rule reads, one type per rule a reader can read, and a `Row` enum of the
rows the grammar names. The parser runs at generation and at run time alike, and
no interpreter of a grammar ships.

```vyrn
import { syntax, SyntaxError } from "std/syntax"
import { parseFile, File, Row } from syntax("./json.syntax")

fn valid(text: String) -> Bool {
    return match parseFile(text) {
        Ok(file) => true,
        Err(errors) => false,
    }
}
```

A grammar states shape only, so the parser holds no sentence. A row is a name and
its holes, and the reader says what each row means. A label is a field of the
tree, and every record holds its byte span. `syntaxReport(path)` writes, from the
same grammar, what the editor says about a file of it: a report at each refusal,
in the sentence `<stem>.errors/en.json` beside the grammar holds for its row. A
program that only parses imports `syntax(path)` alone and links no report.

## SyntaxError

```vyrn
type SyntaxError = { row: R, line: Int64, col: Int64, end: Int64 }
```

One refusal of a grammar whose rows are `R`: its row, a 1-based line and column,
and the byte offset past the refused token. A parser answers with its own `Row`,
so a refusal of one grammar is no refusal of another.

## syntax

```vyrn
fn syntax(path: String) -> Code
```

The parser the grammar at `path` writes. A grammar that does not load is a report
at its line and column, and the path must be one the generator's caller passed.

## syntaxReport

```vyrn
fn syntaxReport(path: String) -> Code
```

The reports of the grammar at `path`: a module over its parser that exports one
`gen fn report<Start>(path) -> Code` per start rule, a report at each refusal of
the text at `path` in its catalogue's sentence. A catalogue that is no JSON
object of strings is a report at its file.
