# std/code

The `Code` helpers a generator writes in Vyrn: list forms over code quotes.

A quote holds holes only, and logic lives in ordinary functions that return
`Code`. `join` is the one list form: a hole takes no `Array<Code>`.

```vyrn
import { join } from "std/code"

export gen fn sum(names: Array<String>) -> Code {
    let mut terms: Array<Code> = []
    for n in names {
        terms.push(vyrn"\{name(n)}()")
    }
    return vyrn"export fn total() -> Int64 { return \{join(terms, vyrn" + ")} }"
}
```

## join

```vyrn
fn join(xs: Array<Code>, sep: Code) -> Code
```

Joins `xs` with `sep` between each pair; no element gives empty `Code`.
