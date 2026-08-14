# std/vyx-hints

std/vyx-hints — accessibility, security and performance rules for `.vyx`
components (RFC-0100), in the spirit of `nuxt/hints`.

This library is ordinary Vyrn and holds no privilege. It reads a `.vyx` file
with `std/vyx`'s exported `vyxParseTemplate`, decides what is worth saying
with its own rules, and says it through `std/hints` — which filters by the
project's configuration and by the author's waivers before handing the report
to `std/diag`. Every one of those is a public import. A project that wants
different rules writes `their-hints` beside this and changes one line:

```vyrn
import { vyxHints } from "std/vyx-hints"
import * as _hints from vyxHints("./app/widgets")
```

The second import runs the checks. It binds a namespace nothing has to use;
the diagnostics are the point. To configure, name a JSON file instead — the
project manifest is the obvious one, since `vyrn.json` ignores keys it does
not know:

```vyrn
import { vyxHintsConfigured } from "std/vyx-hints"
import * as _hints from vyxHintsConfigured("./app/widgets", "./vyrn.json")
```

```json
{ "hints": { "perf/img-size": "off", "a11y/img-alt": "error" } }
```

A generator may only read under the paths it was GIVEN, so the config is an
argument rather than a file this library goes looking for. That is the same
facility a third-party hint library gets, and it is why the manifest is not
special here.

## What is checked, and what is deliberately not

`docs/research/vyx-hints.md` ranked sixty-odd rules from `nuxt/hints`,
`html-validate`, Lighthouse, axe-core and OWASP. The ones below are the ones
a template's own text decides. A rule that needs a rendered page (which
element was the LCP, how far the layout shifted), a rule that needs to know
what the author meant (is this PNG a photo or a logo), and a rule that a
component boundary can hide (heading level order) are not here, because a
hint that fires when it is not sure is worse than no hint at all.

Two reports are errors rather than advice — an inline `on*` handler and a
`javascript:` URL. Both are markup the template language already has a
correct spelling for (`@click`), both defeat a content-security policy, and
neither has an honest use. Everything else is a warning, and a project can
move any of them in either direction.

## vyxHints

```vyrn
fn vyxHints(dir: String) -> String
```

`vyxHints(dir)` — check every `<Name>.vyx` under `dir` at this library's
default severities. Emits `//@diag` reports and one trivial declaration, so
the import that runs it may bind a name or a namespace.

## vyxHintsConfigured

```vyrn
fn vyxHintsConfigured(dir: String, config: String) -> String
```

`vyxHintsConfigured(dir, config)` — the same rules, under the `"hints"` object
of the JSON document at `config`.

A config that cannot be read, does not parse, or names a level word nobody
defined is an ERROR, and no file is checked. The alternative — carry on with
the defaults — tells a project its policy is in force while it is not, which
is the fault `find_manifest` was hardened against.

## vhCheck

```vyrn
fn vhCheck(p: Policy, src: String, file: String) -> String
```

Every report `src` (the text of the `.vyx` file at `file`) earns.

Exported so a rule set can be tested, and so a third-party library may reuse
these rules under its own configuration rather than fork them.
