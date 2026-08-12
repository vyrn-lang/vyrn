# std/diag

std/diag — a generator reports a diagnostic (RFC-0099).

A `gen fn` returns Vyrn source text. A line of that text may also be a
REPORT: a severity, an anchor in a file the generator read, and a message.
The loader lifts it out and the toolchain shows it like any other
diagnostic — `vyrn check`, `vyrn build`, `vyrn run` and the editor alike.

```vyrn
import { report, Severity } from "std/diag"

export gen fn table(path: String) -> String {
    let mut out = ""
    out = out + report(Warning, path, 9, 3, "column `email` has no length limit")
    out = out + "export fn columnCount() -> Int64 { return 3 }\n"
    return out
}
```

A `Warning` rides a build that SUCCEEDED — it changes no exit code and no
byte of program output, unless the build asked for zero warnings with
`--deny-warnings`. An `Error` fails the load, at the anchor, with the
generator's own wording.

The anchor names a file the generator READ, at a 1-based line and column it
computed while reading it — the same notation `//@origin` uses (RFC-0033),
resolved against the module that imported the generator. That is the only
anchor a generator can give correctly by construction; anchoring at a file it
never opened is possible and is nobody's check but the generator's own.

The compiler knows nothing about what is being checked. Rules are ordinary
Vyrn in ordinary libraries, and a third-party generator — including one that
targets another language entirely — uses this surface exactly as std does.

## Severity

```vyrn
type Severity = Warning | Error
```

How serious a report is. `Warning`: this compiled, but. `Error`: this does
not compile.

## report

```vyrn
fn report(severity: Severity, file: String, line: Int64, col: Int64, message: String) -> String
```

A report anchored in an input file, at a 1-based `line` and `col`.

## reportHere

```vyrn
fn reportHere(severity: Severity, message: String) -> String
```

A report with no position to give — shown at the generated line it sits on.
Use it when the fault is in the generator's inputs as a whole (a missing
file, a contradiction between two of them) rather than at one place in one.
