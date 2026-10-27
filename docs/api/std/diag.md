# std/diag

A generator reports a diagnostic, anchored in a file it read.

A report is `Code`: a generator returns it with the module it writes, and
the loader shows it like any other diagnostic. Only a report is read as one,
so text a generator copies cannot forge a diagnostic.

```vyrn
import { report, Severity } from "std/diag"

export gen fn table(path: String) -> Code {
    let warn = report(Warning, path, 9, 3, "column `email` has no length limit")
    return warn + vyrn"export fn columnCount() -> Int64 { return 3 }"
}
```

A `Warning` changes no exit code and no output byte, unless the build passes
`--deny-warnings`. An `Error` fails the load at the anchor. The anchor is a
1-based line and column, resolved against the module that imported the
generator. A generator that returns a `String` writes `render(report(..))`,
a `//@diag` line, until the last `String` generator is gone.

## Severity

```vyrn
type Severity = enum { Warning, Error }
```

`Warning`: this compiled, but. `Error`: this does not compile.

## report

```vyrn
fn report(severity: Severity, file: String, line: Int64, col: Int64, message: String) -> Code
```

A report anchored in an input file, at a 1-based `line` and `col`.

## reportHere

```vyrn
fn reportHere(severity: Severity, message: String) -> Code
```

A report with no position, shown at the generated line it sits on: for a
fault in the generator's inputs as a whole, such as a missing file.
