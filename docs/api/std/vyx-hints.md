# std/vyx-hints

Accessibility, security and performance rules for `.vyx` components.

The library holds no privilege: it parses with `std/vyx`'s
`vyxParseTemplate` and reports through `std/hints`, so a project swaps in
its own rules by changing one import. The second import runs the checks;
the namespace it binds is unused.

```vyrn
import { vyxHints } from "std/vyx-hints"
import * as _hints from vyxHints("./app/widgets")
```

To configure, pass a JSON file; a generator reads only under the paths it
is given:

```vyrn
import { vyxHintsConfigured } from "std/vyx-hints"
import * as _hints from vyxHintsConfigured("./app/widgets", "./vyrn.json")
```

```json
{ "hints": { "perf/img-size": "off", "a11y/img-alt": "error" } }
```

Only rules the template's own text decides are here
(`docs/research/vyx-hints.md` ranks the rest). A rule that needs a rendered
page, the author's intent, or a view across component boundaries would
fire when unsure. Nine rules are errors: `sec/inline-handler`,
`sec/unsafe-url`, and seven `html-validate` ports that keep its `"error"`
preset severity. The rest are warnings.

## vyxHints

```vyrn
fn vyxHints(dir: String) -> String
```

Checks every `<Name>.vyx` under `dir` at default severities. Emits
`//@diag` reports and one declaration for the import to bind.

## vyxHintsConfigured

```vyrn
fn vyxHintsConfigured(dir: String, config: String) -> String
```

[`vyxHints`] under the `"hints"` object of the JSON document at `config`.
A config that cannot be read or is refused by `policyOf` is an error, and no
file is checked.

## vhCheck

```vyrn
fn vhCheck(p: HintPolicy, src: String, file: String) -> String
```

Every report `src` (the text of the `.vyx` file at `file`) earns. Exported
for tests and for reuse under another library's configuration.
