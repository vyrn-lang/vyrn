# std/hints

std/hints — the shape of a checking library (RFC-0100).

`std/diag` (RFC-0099) is the whole compiler-side mechanism: a generator may
report a diagnostic, at a severity it chooses. What it does not carry is the
two things every checking library then needs, and would otherwise hand-roll:
a way for the PROJECT to turn one rule off or move its severity, and a way
for the AUTHOR to waive one report at the line that earns the waiver.

Both are policy, not compiler. So they live here, in an ordinary Vyrn
library, and they know nothing about `.vyx`, about HTML, about the web, or
about Vyrn. A rule is a `code` — a string this module never interprets — and
an input file is text with lines in it.

A hint library is then three lines of plumbing and its own rules:

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

Nothing above is privileged. `std/vyx-hints` is written against exactly this
surface, and a third-party library for another framework — or for another
file format entirely, since a generator reads text — is written against it
the same way.

## Configuration

A project moves or silences a rule with a JSON object mapping code to level:

```json
{ "hints": { "a11y/img-alt": "error", "perf/img-size": "off" } }
```

The document is any JSON file the generator was GIVEN as a constant path
argument — `vyrn.json` is the obvious home, since the manifest ignores keys
it does not know — and the top-level `key` is the library's own, so two hint
libraries in one project configure independently.

[`policyOf`] is strict, for the reason `find_manifest` is strict: a config
that does not parse, or that names a level word nobody defined, is a fault to
report — not a reason to quietly check nothing. A library that swallows it
tells the project its rules are on while they are off.

## Waivers

`vyrn-ignore <code>` in the reported line, or in the line above it, drops
that one report. The marker is plain text, so it rides whatever comment the
input file's own language spells:

```text
<!-- vyrn-ignore sec/raw-html: the summary is sanitized upstream -->
<div v-html="summary"></div>
```

The count of waivers in a repository is the audit a rule like `sec/raw-html`
is actually for.

## HintPolicy

```vyrn
type HintPolicy = { codes: Array<String>, levels: Array<String> }
```

Per-code severity overrides, as parallel arrays (a code and its level word).
Built by [`policyOf`]; [`noPolicy`] is the empty one, under which every rule
runs at the severity its author chose.

## noPolicy

```vyrn
fn noPolicy() -> HintPolicy
```

The empty policy: no override, every rule at its default severity.

## policyOf

```vyrn
fn policyOf(configText: String, key: String) -> Result<HintPolicy, String>
```

Read a policy out of `configText`, from its top-level `key`.

`Ok(noPolicy())` when the document has no such key — a project that says
nothing gets the library's defaults. `Err` when the document does not parse,
when the key is not an object of strings, or when a level word is not `off`,
`warning` or `error`. Report the `Err`; do not fall back to checking nothing.

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

One report: the `//@diag` line for it, or `""` when the project turned `code`
off or the author waived it at that line.

`src` is the text of `file` — needed to read the waiver comment, and the
reason this takes source a rule has already read rather than reading it
again. The `code` rides the message, which is RFC-0099's convention until a
consumer for a separate field exists.

## waived

```vyrn
fn waived(src: String, line: Int64, code: String) -> Bool
```

Whether `code` is waived at `line` of `src` — a `vyrn-ignore <code>` marker
on that line or on the one above it.
