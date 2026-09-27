# std/hints

Policy for checking libraries: per-project rule levels and per-line waivers.

A rule is a `code`, a string this module never interprets; an input file is
text with lines. `std/vyx-hints` uses only this public surface.

```vyrn
import { hint, noPolicy } from "std/hints"
import { Severity } from "std/diag"

export gen fn myHints(path: String) -> String {
    let src = match readFile(path) { Ok(t) => t, Err(e) => "" }
    let mut out = ""
    // ... find something at line 4, column 9 ...
    out = out + hint(noPolicy(), "my/rule", Warning, src, path, 4, 9, "say what is wrong")
    return out
}
```

A project sets levels with a JSON object under the library's own top-level
key, in any JSON file passed to the generator as a constant path:

```json
{ "hints": { "a11y/img-alt": "error", "perf/img-size": "off" } }
```

[`policyOf`] refuses a config that does not parse or names an unknown level:
a swallowed fault would report rules on while they are off.

`vyrn-ignore <code>` on the reported line, or on the line above it, drops
that one report. The marker is plain text inside the input's own comment:

```text
<!-- vyrn-ignore sec/raw-html: the summary is sanitized upstream -->
<div v-html="summary"></div>
```

## HintPolicy

```vyrn
type HintPolicy = { codes: Array<String>, levels: Array<String>, strict: Bool }
```

Per-code severity overrides, as parallel arrays (a code and its level word).
`strict` ignores waivers; only [`strictly`] sets it, for [`unusedWaivers`].

## noPolicy

```vyrn
fn noPolicy() -> HintPolicy
```

The empty policy: no override, every rule at its default severity.

## strictly

```vyrn
fn strictly(p: HintPolicy) -> HintPolicy
```

`p` with waivers ignored: what a rule would report. Builds the `raw`
input of [`unusedWaivers`].

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

## hint

```vyrn
fn hint(p: HintPolicy, code: String, dflt: Severity, src: String, file: String, line: Int64, col: Int64, message: String) -> String
```

One `//@diag` report line, or `""` when `code` is off or waived at `line`.
`src` is the text of `file`; the waiver marker is read from it.

## waived

```vyrn
fn waived(src: String, line: Int64, code: String) -> Bool
```

Whether `code` is waived at `line` of `src`: a `vyrn-ignore <code>` marker
on that line or on the one above it.

## unusedWaivers

```vyrn
fn unusedWaivers(p: HintPolicy, src: String, file: String, raw: String) -> String
```

Reports every `vyrn-ignore` marker in `src` that waives nothing. `raw` is
the same report built under [`strictly`]; a marker at line L is used when
`raw` reports its code at L or L + 1.

Reports go through [`hint`] with `p`, so `hint/unused-waiver` is configured
and waived like any other rule.
