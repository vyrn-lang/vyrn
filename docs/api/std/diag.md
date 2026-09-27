# std/diag

std/diag: a generator reports a diagnostic.

A line of a `gen fn`'s output may be a report: a severity, an anchor in a
file the generator read, and a message. The loader lifts it out, and every
tool shows it like any other diagnostic.

```vyrn
import { report, Severity } from "std/diag"

export gen fn table(path: String) -> String {
    let mut out = ""
    out = out + report(Warning, path, 9, 3, "column `email` has no length limit")
    out = out + "export fn columnCount() -> Int64 { return 3 }\n"
    return out
}
```

A `Warning` changes no exit code and no output byte, unless the build passes
`--deny-warnings`. An `Error` fails the load at the anchor. The anchor is a
1-based line and column, in `//@origin` notation, resolved
against the module that imported the generator.

## Severity

```vyrn
type Severity = Warning | Error
```

`Warning`: this compiled, but. `Error`: this does not compile.

## report

```vyrn
fn report(severity: Severity, file: String, line: Int64, col: Int64, message: String) -> String
```

A report anchored in an input file, at a 1-based `line` and `col`.

## reportHere

```vyrn
fn reportHere(severity: Severity, message: String) -> String
```

A report with no position, shown at the generated line it sits on: for a
fault in the generator's inputs as a whole, such as a missing file.
