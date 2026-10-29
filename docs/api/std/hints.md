# std/hints

Policy for checking libraries: per-project rule levels and per-line waivers.

A rule is a `code`, a string this module never interprets; an input file is
text with lines. `std/vyx-hints` uses only this public surface.

A library's rules record [`Finding`](#finding)s as data; [`reports`](#reports) applies the
project's levels and waivers once and returns the reports:

```vyrn
import { Finding, noPolicy, reports } from "std/hints"
import { Severity } from "std/diag"

export gen fn myHints(path: String) -> Code {
    let src = match readFile(path) { Ok(t) => t, Err(e) => "" }
    let mut found: Array<Finding> = []
    // ... find something at line 4, column 9 ...
    found.push(Finding { code: "my/rule", dflt: Warning, line: 4, col: 9, message: "say what is wrong" })
    return reports(noPolicy(), src, path, found)
}
```

A project sets levels with a JSON object under the library's own top-level
key, in any JSON file passed to the generator as a constant path:

```json
{ "hints": { "a11y/img-alt": "error", "perf/img-size": "off" } }
```

[`policyOf`](#policyof) refuses a config that does not parse or names an unknown level:
a swallowed fault would report rules on while they are off.

`vyrn-ignore <code>` on the reported line, or on the line above it, drops
that one report. The marker is plain text inside the input's own comment:

```text
<!-- vyrn-ignore a11y/tabindex-positive: the tour sets the order -->
<div tabindex="2">Start here</div>
```

## HintPolicy

```vyrn
type HintPolicy = { codes: Array<String>, levels: Array<String> }
```

Per-code severity overrides, as parallel arrays (a code and its level word).

## Finding

```vyrn
type Finding = { code: String, dflt: Severity, line: Int64, col: Int64, message: String }
```

One rule's finding in an input file: the rule's `code`, its severity when
the project sets none, the 1-based `line` and `col` it anchors at, and what
is wrong.

## noPolicy

```vyrn
fn noPolicy() -> HintPolicy
```

The empty policy: no override, every rule at its default severity.

## policyOf

```vyrn
fn policyOf(configText: String, key: String) -> Result<HintPolicy, String>
```

Reads a policy from the top-level `key` of `configText`.

`Ok(noPolicy())` when the key is absent. `Err` when the document does not
parse, the key is not an object of strings, or a level is not `off`,
`warning` or `error`.

## levelOf

```vyrn
fn levelOf(p: HintPolicy, code: String, dflt: Severity) -> String
```

The level `code` runs at under `p`: its configured word, or the word for
`dflt` when the project said nothing about it.

## reports

```vyrn
fn reports(p: HintPolicy, src: String, file: String, findings: Array<Finding>) -> Code
```

The reports `findings` earn in `file` under `p` (`earned`). `src` is the
text of `file`.

## waived

```vyrn
fn waived(src: String, line: Int64, code: String) -> Bool
```

Whether `code` is waived at `line` of `src`: a `vyrn-ignore <code>` marker
on that line or on the one above it.

## unusedWaivers

```vyrn
fn unusedWaivers(p: HintPolicy, src: String, findings: Array<Finding>) -> Array<Finding>
```

A `hint/unused-waiver` finding for every `vyrn-ignore` marker in `src` that
waives nothing: a marker at line L is used when a finding of its code whose
rule `p` does not turn off sits at L or L + 1. [`reports`](#reports) judges these
findings like any other, so the rule is configured and waived by the same
policy.
