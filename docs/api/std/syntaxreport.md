# std/syntaxreport

A refusal's sentence, for the modules `syntaxReport` (`std/syntax`) writes.
A program that only parses never loads it.

## syntaxSentence

```vyrn
fn syntaxSentence(row: SyntaxRow, catalogue: String) -> String
```

What a refusal with `row` says, on one line. `catalogue` holds one
`Row=sentence` line per entry of the grammar's `<stem>.errors/en.json`, as the
parser's writer read it. In a sentence `{$name}` is a hole and a backslash
escapes the next character. A row with no line reads as the row and its holes.
