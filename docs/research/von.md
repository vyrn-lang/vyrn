# VON — Vyrn Object Notation

- **Status:** Research note. Part 2 became [RFC-0097](../../rfcs/RFC-0097-von.md),
  whose M0 and M1 have shipped. Where this note and that RFC disagree, the RFC
  is the record: it carries what measurement changed, including the
  zero-compiler-change claim in §2.10 (§9 there).
- **Scope:** Part 1 is a census of failures in existing configuration formats.
  Part 2 is a draft design for VON.
- **Related:** RFC-0002 (records), RFC-0003 (validated types), RFC-0006
  (diagnostics), RFC-0009 (`Validation<T>` / `Issue`), RFC-0010 (modules,
  manifest, JSON-Schema type imports), RFC-0017 (`vyrn fmt`), RFC-0018 (JSON
  codec), RFC-0021 (generator imports), RFC-0028 (`Map`), RFC-0033 (origin
  maps), RFC-0047 (semantic highlighting), RFC-0054 (code quotes, `lex()`),
  RFC-0059 (`std/json`), RFC-0072 (`audience`)
- **Sources:** every URL, issue number, CVE and release date in Part 1 was
  fetched and checked. Download counts are npm registry figures for the week
  beginning 2026-08-03; star counts are GitHub API figures for 2026-08-10.
  Unchecked claims carry **[unverified]** and carry no argument.

---

## Part 1 — What is wrong with configuration formats

Every format below is in wide production use. The point of this census is not
that they are bad. It is that each one made a specific trade, that the trade
has a documented cost, and that the costs cluster into four groups:

- **A. The format guesses the type.** YAML, and every format with barewords.
- **B. The format has no type at all.** JSON, TOML, JSON5, KDL — the schema is
  in a second file, in another language, or in the reader's head.
- **C. The format is a program.** HCL, Jsonnet, Dhall, CUE, Nickel, Pkl — you
  must run the file to know what it says.
- **D. The format is unwritable by a human.** JSON: no comments, no trailing
  commas, no multi-line strings.

No format so far avoids all four. VON's claim in Part 2 is that a format
embedded in a typed host language can, because the host absorbs C and the
type system answers A and B.

Every URL and issue number below was checked. Claims that could not be
verified are marked **[unverified]** and are not used to support an argument.

### 1.1 JSON — the format that cannot say what it means

**Duplicate keys are legal and undefined.** RFC 8259 §4 says the names in an
object "SHOULD be unique", then documents the consequence: "When the names
within an object are not unique, the behavior of software that receives such
an object is unpredictable. Many implementations report the last name/value
pair only. Other implementations report an error or fail to parse the object,
and some implementations report all of the name/value pairs, including
duplicates."

A `SHOULD` is not a rule. It is a request. Nicolas Seriot measured what
implementations actually do with `{"a":1,"a":2}`: Go, Python, JavaScript,
Ruby and Rust return `{"a":2}`; Apple's `NSJSONSerialization` and Swift's
`JSONSerialization` return `{"a":1}`; cJSON, R and Lua's JSON return both
pairs. Three answers, all conforming.

**Numbers have no type.** The RFC 8259 grammar has one numeric production:
`number = [ minus ] int [ frac ] [ exp ]`. There is no integer. §6 declines
to fix a range or a precision, and offers interoperability advice instead:
"Since software that implements IEEE 754 binary64 (double precision) numbers
is generally available and widely used, good interoperability can be achieved
by implementations that expect no more precision or range than these
provide." Seriot's measurements: `10000000000000000999` comes back as a
double, an unsigned long long, or a string depending on the parser, and
**cJSON silently returns `10000000000000002048`**. `1E-999` is `0.0` in most
parsers and the string `"1E-999"` in Freddy.

**Nothing is required to agree.** RFC 8259 §9 makes the disagreement legal:
"A JSON parser MAY accept non-JSON forms or extensions. An implementation may
set limits on the size of texts… on the maximum depth of nesting… on the
range and precision of numbers… on the length and character contents of
strings." Seriot's conclusion after running about 300 test cases against more
than 30 parsers: **"out of over 30 parsers, no two parsers parsed the same
set of documents the same way."** He also found crashes, not just
disagreement — Xcode itself crashes on a file made of `[` repeated 10,000
times, and he filed real bugs against SBJSON (#219), Freddy (#199, #206) and
JSON.sh (#47).

- Seriot, *Parsing JSON is a Minefield*:
  https://seriot.ch/security/parsing_json.html — test suite:
  https://github.com/nst/JSONTestSuite. (The older
  `seriot.ch/projects/parsing_json.html` URL is a 404; do not cite it.)
- The spec is also honest about broken strings. §8.2: the grammar "allows
  member names and string values to contain bit sequences that cannot encode
  Unicode characters; for example, `"\uDEAD"`… The behavior of software that
  receives JSON texts containing such values is unpredictable."

**It is hostile to a human editor.** §2 admits only four whitespace bytes.
There is no comment production and no optional trailing separator. Douglas
Crockford's stated reason for removing comments — a 2012 post now lost with
Google+, surviving in a Hacker News mirror
(https://news.ycombinator.com/item?id=3912149) — is that "people were using
them to hold parsing directives, a practice which would have destroyed
interoperability". Note the shape of that argument: comments were removed to
protect interoperability, and §4, §6 and §9 gave it away anyway.

### 1.2 YAML — the format that guesses

**The root cause is two live specifications.** YAML 1.1's `!!bool` type
(https://yaml.org/type/bool.html) resolves this regular expression:

```
y|Y|yes|Yes|YES|n|N|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF
```

YAML 1.2.2's core schema (§10.3.2) resolves only `true|True|TRUE|false|False|FALSE`.
Integers split the same way: 1.1 has leading-zero octal (`[-+]?0[0-7_]+`) and
**base-60** (`[-+]?[1-9][0-9_]*(:[0-5]?[0-9])+`); 1.2 has `0o[0-7]+` and no
sexagesimal at all.

So the same bytes have two meanings, and which one you get depends on your
library, not on your file:

| Parser | Version | Evidence |
| --- | --- | --- |
| PyYAML | 1.1 | `lib/yaml/resolver.py` carries the 1.1 bool regex, the octal int regex and the base-60 int and float regexes |
| SnakeYAML | 1.1 | README: "SnakeYAML is a YAML 1.1 processor for the Java Virtual Machine version 8+" |
| go-yaml v2 | mixed | README: "supports most of YAML 1.1 and 1.2 … base-60 floats from YAML 1.1 are purposefully not supported since they're a poor design" |
| go-yaml v3 | mixed | README: 1.1 booleans work "as long as they are being decoded into a typed bool value. Otherwise they behave as a string", and octals stay `0777` "because most parsers still use the old format". The same README now opens "THIS PROJECT IS UNMAINTAINED" |

**The Norway problem.** `NO` is the ISO code for Norway and a boolean in
YAML 1.1. StrictYAML's write-up gives the canonical example — `countries: [GB, IE, FR, DE, NO]`
becomes `['GB','IE','FR','DE', False]` — and the canonical joke: "It snows a
lot in False."
(https://hitchdev.com/strictyaml/why/implicit-typing-removed/. That page also
claims the behaviour follows the YAML 1.2 spec; it does not. Cite the page for
the anecdote, not for the spec claim.)

Ruud van Asseldonk's *The yaml document from hell*
(https://ruudvanasseldonk.com/2023/01/11/the-yaml-document-from-hell,
2023-01-11) is the best single catalogue. Four failures from one document:

```yaml
geoblock_regions: [dk, fi, is, no, se]   # -> ["dk","fi","is",false,"se"]
port_mapping:     [22:22, 80:80, 443:443] # -> [1342, "80:80", "443:443"]
flush_cache:      { on: [push, memory_pressure] }  # key becomes true
allow_postgres_versions: [9.5.25, 9.6.24, 10.23, 12.13]
                                         # -> ["9.5.25","9.6.24",10.23,12.13]
```

Read the second line again. `22:22` is base-60, so it is 1342. `80:80` is not,
because 80 exceeds 59. One list, two types, no warning. The third line is the
key GitHub Actions workflows use. The fourth is a version list in which two
entries are strings and two are floats, because two of them happen to have one
dot. (The often-repeated "MongoDB port" version of the `22:22` story is
**[unverified]**; Ruud's container port-mapping example is the citable one.)

He also measured tool disagreement: "Vim, my blog generator, GitHub, and
Codeberg, all have a unique way to highlight the example document… No two of
them pick out the same subset of values as non-strings!" Syntax highlighting
is a type checker in this format, and four of them disagree.

- **noyaml.com** (Geoffrey Huntley) adds `- 07` / `- 08` parsing as `[7, "08"]`,
  `04:30` as `16200`, and counts 22 spellings of true and false in YAML 1.1.
- **Octal.** `mode: 0777` is 511 under 1.1 and 777 under 1.2. File modes are
  the single most common place a config file writes a leading zero.

**Indentation is load-bearing and tabs are banned.** YAML 1.2.2 §6.1: "In YAML
block styles, structure is determined by indentation… To maintain portability,
tab characters must not be used in indentation, since different systems treat
tabs differently", and "All sibling nodes must use the exact same indentation
level."

**Anchors, aliases and merge keys.** The merge key `<<` is a YAML 1.1 type
(https://yaml.org/type/merge.html) with no equivalent in the 1.2 core schema —
so the most common composition idiom in production YAML is an extension that
1.2 parsers implement out of habit. Aliases also make the format expand
exponentially:

```yaml
a: &a ["lol","lol","lol","lol","lol","lol","lol","lol","lol"]
b: &b [*a,*a,*a,*a,*a,*a,*a,*a,*a]
c: &c [*b,*b,*b,*b,*b,*b,*b,*b,*b]   # nine levels: ~387 million strings
```

This is not theoretical. **CVE-2019-11253** (CVSS 3.1 = 7.5): "Improper input
validation in the Kubernetes API server in versions v1.0-1.12 and versions
prior to v1.13.12, v1.14.8, v1.15.5, and v1.16.2 allows authorized users to
send malicious YAML or JSON payloads, causing the API server to consume
excessive CPU or memory, potentially crashing." Tracking issue:
kubernetes/kubernetes#83253. Before v1.14.0 the default RBAC policy let
**anonymous** users trigger it.

**Deserialization.** YAML's tag system lets a document name a host type, which
made three of the decade's better-known remote-code-execution bugs:

- **CVE-2017-18342** (CVSS 3.1 = 9.8): "In PyYAML before 5.1, the `yaml.load()`
  API could execute arbitrary code if used with untrusted data."
- **CVE-2013-0156**: Rails "does not properly restrict casts of string values,
  which allows remote attackers to conduct object-injection attacks and execute
  arbitrary code" through YAML or Symbol type conversion.
- **CVE-2022-1471** (CVSS 3.1 = 8.3): "SnakeYaml's Constructor() class does not
  restrict types which can be instantiated during deserialization."

**Size.** RFC 8259 as plain text is about 4,000 words. The YAML 1.2.2
single-page specification is about 22,000, plus a separate errata page. Ruud
again: "The entire json spec consists of six railroad diagrams… Yaml… consists
of 10 chapters with sections numbered four levels deep and a dedicated errata
page."

### 1.3 TOML — right about strings, wrong about shape

TOML fixed JSON's editing problems: comments, first-class dates, unambiguous
strings. Its cost is structural — it is comfortable one level deep and painful
below that.

**Inline tables were single-line for seven years.** TOML 1.0.0 says: "Inline
tables are intended to appear on a single line. A terminating comma (also
called trailing comma) is not permitted after the last key/value pair… **No
newlines are allowed between the curly braces** unless they are valid within a
value. Even so, it is strongly discouraged to break an inline table onto
multiples lines. If you find yourself gripped with this desire, it means you
should be using standard tables."

The counter-argument is in toml-lang/toml issue **#516**, "Proposal: Allow
newlines and trailing commas in inline tables" — opened **2018-01-22**, 44
comments, closed **2022-07-30**: "The only thing that doesn't [feel obvious] is
the explicit crippling of inline tables… Most popular languages that have `{}`
style mappings allow newlines in them (JSON, Python, Javascript, Go). Also
newlines and trailing commas are allowed in lists in the toml spec, so it is
inconsistent."

It shipped in **TOML 1.1.0 on 2025-12-18** — seven years and eleven months
after the issue opened. The changelog entry is one line: "Allow newlines and
trailing commas in inline tables (#904)". Ecosystem lag is now the live cost:
Python's stdlib `tomllib` targets TOML 1.0.0 **[unverified whether 3.14+ ships
1.1 support]**.

**Nesting is an acknowledged weak spot, by the maintainers.** Issue **#781**,
"Does TOML need to make nesting easier?" (2020-10-26, 45 comments, closed
2022-03-02): "This is basically an issue pulled out of #516 and #744… how about
we make writing nested table structures easier?" The maintainer's own answer
in the same thread: "I'm undecided/unsure about the first question."

**Array-of-tables is a positional cursor.** The spec: "Any reference to an
array of tables points to the **most recently defined** table element of the
array", and "If the parent of a table or array of tables is an array element,
that element must already have been defined before the child can be defined.
Attempts to reverse that ordering must produce an error at parse time." So a
`[[products]]` header mutates a hidden insertion point, and moving a block up
the file changes what it attaches to. Issue **#309**, "Revisit array of table
syntax" (2015-03-03), ran to **77 comments**.

Other long-running shape complaints, all real issue titles: **#499** "Allow
keys in key-value pairs to be paths" (86 comments), **#769** "The spec language
seems to forbid writing to the same defined table using dotted keys" (47
comments), **#551** "Newline-Delimited Inner Tables", **#486** "Inconsistent
commas".

**Two more version-boundary hazards.** Heterogeneous arrays were illegal until
1.0.0-rc.1 (2020-04-01), so `[1, "a"]` — a legal JSON array — had no TOML
form. Date-times arrived in 0.5.0 (2018-07-11) as four distinct types (Offset
Date-Time, Local Date-Time, Local Date, Local Time), which most target
languages cannot round-trip into one native type; 1.1.0 then made seconds
optional, another parser-compatibility break.

### 1.4 HCL — a language toolkit, not a data format

The common complaint that HCL has no formal specification is **false for
HCL2**. hashicorp/hcl ships `spec.md` (the syntax-agnostic information model),
`hclsyntax/spec.md` (native syntax), `json/spec.md` (JSON syntax) and a
`specsuite/` conformance directory. HCL1 has none of that and its README says
it is "in maintenance mode only". The v2 README also records the break: "This
is major version 2 of HCL, whose Go API is incompatible with major version 1."

The real problem is what the specification says HCL *is*. From
`hclsyntax/spec.md`: "The language consists of three integrated sub-languages:
the *structural* language… the *expression* language… the *template*
language." And from the README: "HCL provides a set of constructs that can be
used by a calling application to construct a configuration language. The
application defines which attribute names and nested block types are
expected."

So there is no format-level answer to "what does this file mean". The same
bytes decode differently under Terraform, Nomad, Packer and Vault. A `.tf`
file is not data; it is a program in a language whose semantics ship with the
host.

**The JSON variant needs a non-conforming JSON parser.** `json/spec.md`:
"*Parsing* such JSON has some additional constraints not beyond what is
normally supported by JSON parsers, so a **specialized parser may be required**
that is able to: preserve the relative ordering of properties defined in an
object; **preserve multiple definitions of the same property name**; preserve
numeric values to the precision required…; retain source location
information."

The same spec makes string meaning mode-dependent: in literal-only mode
"Template interpolations and directives MUST NOT be processed"; in full
expression mode "the literal string is instead interpreted as a *standalone
template*". One JSON string, two meanings, decided by context. And numbers may
have to be smuggled through strings: "an embedded template interpolation can be
used to faithfully represent a number, such as `"${1e150}"`", because
"off-the-shelf JSON serializers often do not support customizing the processing
of numbers".

**HashiCorp warns against its own feature.** The `dynamic` blocks page
(https://developer.hashicorp.com/terraform/language/expressions/dynamic-blocks):
"**Overuse of dynamic blocks can make configuration hard to read and
maintain**, so we recommend using them only when you need to hide details in
order to build a clean user interface for a re-usable module. Always write
nested blocks out literally where possible." `dynamic` exists because blocks
are not values. The `iterator` argument exists because nested `dynamic` blocks
shadow their parent's type name. Both are workarounds for the data model.

### 1.5 JSON5, JSONC — the right fixes, two incompatible dialects

JSON5 (https://spec.json5.org) adds what JSON refused: `//` and `/* */`
comments, one trailing comma, unquoted ES5 identifier keys, single-quoted and
multi-line strings, hex numbers, `Infinity` and `NaN`, explicit `+`.

**Adoption is not thin — correct that premise.** The `json5` npm package
recorded **222,933,649 downloads in the week of 2026-08-03**. Chromium uses
JSON5 (`runtime_enabled_features.json5`), Babel and Next.js read JSON5 config,
and Apple ships first-party support as `JSONDecoder.allowsJSON5`. What is thin
is stdlib support outside JavaScript and Apple: no Python, Go, Java or Rust
standard-library parser. JSON5's own README concedes the ceiling: "It is *not
intended* to be used for machine-to-machine communication."

**The divergence is the problem, not the adoption.** JSONC — VS Code's dialect
— adds only comments and trailing commas. Its own documentation:
"you can use single line (`//`) as well as block comments (`/* */`)… The mode
also accepts trailing commas, **but they are discouraged and the editor will
display a warning**." No unquoted keys, no single quotes, no hex, no `NaN`.
`jsonc-parser` runs at **59,140,371 downloads a week**. So the ecosystem has
two incompatible relaxations of JSON, each at nine-figure scale, and a JSON5
file is not readable by a JSONC parser. JSONC has no versioned specification
document at all **[unverified whether any formal grammar exists]**.

### 1.6 KDL — a clean design that changed its mind

KDL describes itself as "a small, pleasant document language with XML-like node
semantics that looks like you're invoking a bunch of CLI commands!" The model
is nodes with a name, positional arguments, key/value properties and optional
children, plus `/-` "slashdash" comments that remove a whole node.

v1.0.0 shipped 2021-09-12. **v2.0.0 shipped 2024-12-22 and broke almost every
document.** From the changelog: "`null`, `true`, and `false` are now `#null`,
`#true`, and `#false`. **Using the unprefixed versions of these values is a
syntax error**"; raw strings changed delimiters; `#` stopped being an
identifier character while `,`, `<` and `>` started; multi-line strings moved
to `"""` with automatic dedent; `inf`, `-inf` and `nan` became syntax errors.
Any v1 file containing a boolean fails under v2.

Adoption is measurably thin: `kdljs` recorded **38,747 downloads** in the same
week that `json5` recorded 222.9 million and the `yaml` package recorded 182.6
million. There is no standard-library support anywhere.

### 1.7 The typed tier — and why it stalled

This tier diagnosed the problem correctly. Each of these tools would fix most
of §1.1 to §1.6. None of them displaced YAML. The reasons are documented, and
they repeat.

**Dhall** — total, non-Turing-complete, imports pinned by SHA-256 of an
expression's normal form. Its safety document promises a config that "will
never: throw an exception, crash or segfault, accept malformed input, produce
malformed output, hang or time out." The standard reached v23.1.0 (2025-01-16)
across 29 releases, with a conformance suite and independent bindings.

The totality is also the performance problem, because totality means
normalization. Issue **dhall-lang/dhall-haskell#1890**, "performance advice for
dhall-kubernetes based package" (open since 2020-06-26), contains the measured
number from a Sourcegraph engineer: "my normal form is apparently ~`300x` the
size of the Prelude." The proof-of-concept repository is now named
**`sourcegraph/deploy-sourcegraph-dhall-archived`**, archived, last pushed
2020-09-12. Related: #1960 "slow (or even hanging) dhall-kubernetes based
generation", #412 "Terrible performance when type-checking with a large enum's
constructors", #580 "Performance issues with many deeply nested imports".

Editor support never arrived either. **#1541**, "[LSP] Completion is very slow
in files with big imports", has been open since 2019-11-15; the reporter
measures four to five seconds, and "more than 10-15 seconds and becomes
completely unusable" with the Prelude. **#1558**, "Slow rendering of type error
messages", has been open since 2019-11-19. Installation is a standing
complaint: **#2632** "Unable to install via `cabal`" (open), **#2554** "Can not
install dhall-lsp-server" (open). Non-Haskell bindings decayed: `dhall-golang`
last pushed 2023-02-25, `dhall-nix` archived in 2019.

The adoption commentary is consistent (Hacker News thread 32102203, 2022):
"we had a huge, convoluted dhall project for kube. We ended up switching to a
real language (python)"; "telling people they need to learn Haskell before they
can update a config file". One popular claim does **not** hold up: the Unicode
operators (`λ`, `∀`, `⩓`) drew almost no criticism, because they are optional
aliases. Do not use them as an adoption argument.

**CUE** — the most powerful design here. Types and values are one lattice;
unification is the only composition operator; schema, data, validation and
policy collapse into one construct.

The project documents its own cost. Issue **cue-lang/cue#2850**, "Performance"
(open umbrella, 2024-02-22), records that the maintainer "envisages being able
to achieve performance improvements of several orders of magnitudes
(predominantly by means of reductions in the time complexity)". The reason is
structural, from **#2851**: "Disjunctions can get expensive very fast because
the number of elements introduces a complexity multiplier… we end up
effectively needing to compute the cross product between two such
disjunctions."

The fix was a full evaluator rewrite. Issue **#2884** opened 2024-02-29,
shipped opt-in as `CUE_EXPERIMENT=evalv3` in v0.9.0 (2024-06-06), became the
default in v0.13.0 (2025-05-22) — "the culmination of over a year's worth of
work!" — and closed 2025-12-10. **177 issues carry the `evalv3` label**; two
remain open. The regressions were not small: #3493 "Significant memory
consumption increase over v2", #3372 "8X memory usage increase", #4006 "stack
overflow since the latest closedness changes". Performance regressions were
still landing in 2026 (#4421, #4377).

Debugging unification is the other cost, and it is also filed by the project:
**#2890** "evaluator: bring error messages back up to par with the old
evaluator" and **#2891** "improve error messages with respect to the old
evaluator" have both been open since 2024-02-29. `cue lsp` shipped only in
v0.15.0 (2025-11-06), about six years in.

The clearest adoption evidence in this whole census is Dagger's post,
*Ending CUE support* (2023-12-14, dagger.io/blog/ending-cue-support). Dagger's
first prototype was essentially a CUE front end. They removed it: "we have seen
a steep decline in usage of our original CUE configuration syntax"; "We've
concluded that there simply is not enough interest". And the sentence that
indicts the entire tier:

> "what they really want is to write code in a language they already know.
> Learning a brand new language, however powerful, is simply not what they're
> looking for."

**Pkl** (Apple, open-sourced 2024-01-19) — classes, templates, amend-based
inheritance, output to JSON/YAML/plist, code generation for Java, Kotlin, Swift
and Go. It has the most stars in the tier (11,487) and is still **0.32.1**,
pre-1.0 after two and a half years.

The JVM cost is measured in its own tracker. Issue **#33**, "Slow performance on
JVM?" (2024-02-04): a four-line nested config took **3,089 ms** in Pkl against
53 ms for `kotlinx-serialization-properties`. A maintainer's reply: Pkl "will
take ~0.5-1s just to load the stdlib" and "will *never* win" on static cases.
The same thread measures the GraalVM native binary at **206.59 ms**. Native
images escape the startup cost and bring their own tax: a continuous GraalVM
upgrade stream (#1804, #1769, #1506, #1226, #439), broken downstream embedders
(**#907**, "Downstream `native-image` embedders are broken"), platform gaps (no
Alpine aarch64, no Windows aarch64), and **#543**, "Windows detects virus when
downloading pkl-windows-amd64.exe", open since 2024-06-19.

The star count is misleading, and the gap says so: 11,487 on the core repository
against 271 for `pkl-pantry`, 334 for `pkl-go`, 201 for `pkl-swift`, 153 for
`pkl-vscode`. Stars measure interest in the announcement. The package
repositories measure use. No published list of external production users was
found **[unverified]**. The sharpest comment from the launch thread (Hacker
News 39232976) is the one that applies to every tool in this section: "If you
output YAML or XML then where is your type safety?"

**Nickel** (Tweag) — gradual typing plus contracts, explicitly an evolution of
the Nix language. Correct the common claim: it is **post-1.0**, released
1.0.0 on 2023-05-17, at 1.17.0 (2026-06-09), and actively developed. It is the
best-engineered project in this section and the least adopted. Nix has not
adopted it. Its Nix-facing product, Organist, was last pushed 2025-11-21 while
core Nickel ships weekly. Performance issues track the same curve as the
others: **#1484** "Performance improvement ideas: tracking issue" (open since
2023-07-28), **#1622** "degradation of performance for medium-size codebase
(5~10 kLoC)", **#2344** "`Traverse<RichTerm>` gets exponentially slower".

**StrictYAML** — the cleanest diagnosis in the tier, in the least portable
form. It removes duplicate keys, explicit tags, flow style, implicit typing,
node anchors and references, and direct Python object representations. Its rule
is one sentence: "everything is a string by default."

The limit is structural and its author states it, in
`why/turing-complete-schema/`: schema definition in a non-Turing-complete
language "makes the schema programming language independent and gives it more
potential for being read and understood by non-programmers. However, schema
definition in a non-Turing-complete language also restricts and makes certain
use cases impossible or awkward." He chooses power. The consequence is that the
schema is a Python program. **A StrictYAML document has no meaning without the
Python that defines its schema** — there is no Go, Rust or Java reader, no
editor integration, no shared registry. It is a library, not a format. The
repository was last pushed 2025-05-23.

**Jsonnet** (Google) — the outlier, because it is the one that got adopted. It
is untyped, and it wins anyway: Grafana's Tanka (2,684 stars, pushed
2026-08-10), `grafana/jsonnet-libs`, and the kube-prometheus mixin ecosystem
all run on it. Databricks reported over 40,000 lines of Jsonnet across 1,000
files in 2017. The reason is that its output is JSON, so it drops into an
existing pipeline with no downstream change.

Its criticisms are the mirror image of its adoption: no types at all, and bad
debugging. Issue **google/jsonnet#31**, "Continue after syntax error, present
more than one error at a time", has been open since **2014-12-13**. Its
flagship Kubernetes tool, `ksonnet`, is archived (last push 2019-02-21). A
2023 comparison that chose Jsonnet over Dhall and CUE said exactly why: "Our
decision is driven by prioritizing ecosystem support and a gentle learning
curve over the most technically impressive language."

### 1.8 The six things that killed the typed tier

1. **"One more language to learn" beats every technical argument.** Dagger's
   sentence is the whole finding. And the cost falls on the wrong people:
   config files get edited by the on-call engineer and by the developer
   shipping a flag — the population least willing to spend an hour learning a
   config language.
2. **Evaluation cost is structural, not incidental.** Dhall's totality forces
   normalization (a 300x normal form). CUE's unification forces disjunction
   cross-products. Nickel goes exponential at 5–10 kLoC. Each project paid: CUE
   spent 21 months and 177 labelled regressions rewriting its evaluator and
   survived; Dhall's equivalent issues are still open after six years.
3. **Editor support arrives years late.** `cue lsp` shipped in November 2025.
   Dhall's completion has been "completely unusable" on large imports since
   2019. Nobody adopts a config language whose editor cannot complete a field
   name.
4. **The "it compiles to YAML anyway" trap.** Types are erased at the file
   boundary. The Kubernetes API server re-parses untyped text, `kubectl` shows
   YAML in the debugger, and the consumer's error messages do not improve. You
   took on a compiler and a build step and moved the guarantee nowhere.
5. **Toolchain weight, in whatever form the implementation chose.** Dhall:
   GHC and cabal, with chronic install failures. Pkl: a JVM, escaped only via
   GraalVM, which brings a permanent upgrade treadmill and antivirus false
   positives. CUE and Nickel — single static binaries — are the two still
   gaining ground. That is not a coincidence.
6. **A specification without funded implementations is a document.** Dhall did
   the principled thing: a versioned standard, a conformance suite, independent
   bindings. `dhall-golang` went stale in 2023, `dhall-nix` was archived in
   2019, and everything real still routed through Haskell.

Two YAML-in, YAML-out tools — Helm (30,138 stars) and Kustomize (12,133) —
out-adopt the entire typed tier combined. The market did not reject types. It
rejected a second toolchain.

---

## Part 2 — VON

### 2.1 The thesis

Vyrn already owns every part of a configuration format except the file
extension.

| Part of a config format | Vyrn already has it |
| --- | --- |
| Object syntax | record literal `Name { field: value }` (RFC-0002) |
| Array syntax | `[a, b]` and contextual `[]` |
| Dictionary syntax | `["k": v]` and `[:]` (RFC-0028) |
| Schema | the type declaration |
| Constraints | `where` clauses (RFC-0003) |
| Schema export | `schemaOf(T)` → JSON Schema, byte-exact round-trip (RFC-0010 M2) |
| A reader | `fromJson(T, s) -> Validation<T>` (RFC-0018) |
| Accumulated errors | `Array<Issue>` with key, path, message (RFC-0009) |
| Canonical formatting | `vyrn fmt` (RFC-0017) |
| A lexer usable from library code | `lex()` (RFC-0054) |
| Compile-time file reading and module synthesis | `gen fn` (RFC-0021) |
| Diagnostics anchored in a foreign file | origin maps (RFC-0033) |

So VON is not a new language. **VON is Vyrn's literal grammar, saved to a
file.** A `.von` document is a strict subset of what the Vyrn parser already
accepts. That is the whole design, and everything below follows from it.

Two consequences fall out at once:

1. There is one object syntax in the ecosystem, not two. The rule an author
   learns for `.vyrn` is the rule for `.von`. Constraint (d) is met by
   construction, not by imitation.
2. The reader needs no compiler change. RFC-0021 states the direction
   already: *"long-term, `import type` (schema) is reimplementable as a std
   generator and the language sheds format knowledge entirely."* VON is a
   `gen fn` in `std/von`. See §2.9.

### 2.2 The one real decision: pure data, not expressions

**VON is pure data.** No interpolation, no references, no anchors, no
imports of values, no arithmetic, no conditionals, no functions. A VON
document says exactly what it says, and you can know its meaning by reading
it.

The argument is not "expressions are bad". It is that the expression tier
has to live *somewhere*, and Vyrn is a better place for it than a
configuration format is.

**What a configuration format buys by staying pure**

- **Reading is free.** No evaluation order, no fixpoint, no unification.
  A diff of a VON file is a diff of the values in effect.
- **Failure is local.** Every error names a byte range in one file. There is
  no "this value came from three merged sources" report to untangle.
- **Cost is bounded.** Load time is parse time. No format-level program can
  make a build slow, and none can fail to terminate.
- **Tools are cheap.** A formatter, an LSP, a linter and a diff tool over a
  literal grammar are small. Over an expression language they are compilers.

**What it costs, and where the cost goes instead**

Repetition across environments is a real problem, and it is the reason YAML
anchors, Jsonnet, Dhall functions and CUE unification exist. VON does not
solve it inside the file. It solves it one layer up, in three places that
already work:

1. **Absence is the default.** A field typed `Option<T>` may be omitted; the
   Vyrn code supplies the fallback. Most of what anchors copy is a default
   repeated at every site. Move it into the type and the copies vanish.
2. **Composition is ordinary Vyrn.** `let cfg = merge(base, prod)` is a
   function over records, type-checked, testable, steppable in a debugger.
   Overlay semantics become a function you can read rather than a merge
   algorithm you must learn.
3. **Generation is `gen fn`.** If a config genuinely must be computed, write
   the generator in Vyrn, run it at compile time in the sandboxed comptime
   interpreter (RFC-0021), and commit the VON it produces. The computation
   is in a language with types, tests, `fmt` and diagnostics; the artifact
   stays readable and diffable.

This is the move the typed-config tier could not make. Dhall, CUE, Nickel
and Pkl each had to grow a full expression language, because none of them
had a host language to delegate to. Vyrn does. Delegating is the lazy option
and it is also the correct one.

It is also the move Dagger named when it removed CUE (§1.7): "what they
really want is to write code in a language they already know. Learning a
brand new language, however powerful, is simply not what they're looking
for." A Vyrn developer already knows VON. There is nothing to learn — the
record literal is the same one they write in `.vyrn` files.

**Rejected concessions, and why**

| Feature | Verdict | Reason |
| --- | --- | --- |
| String interpolation `"\{x}"` | Rejected | Needs a scope; a scope needs bindings; bindings need an evaluation order. It is the first step of the whole staircase. `\{` inside a VON string is a **hard error**, not literal text, so a pasted Vyrn template cannot silently change meaning. |
| Anchors / aliases / merge keys | Rejected | The single largest source of "the file does not say what it does" in YAML. Also the billion-laughs vector. |
| `${ENV_VAR}` substitution | Rejected | Hides the effective value, and puts secret-shaped holes in a file that looks static. The host reads the environment; that is the host's job, and it is visible in the code. |
| `null` | Rejected | Vyrn has no null (RFC-0005). Absence is `None`. Two spellings of nothing is one too many. |
| Document imports / overlays | **Open** (§2.11) | Tempting for per-environment layering, but it turns one file into a graph and reintroduces resolution order. Deferred, not refused. |

### 2.3 Grammar sketch

A VON document is a header followed by one value.

```ebnf
document   = header , value , EOF ;

header     = { doc-comment | comment } , type-import , { type-import } ;
type-import= "import" , "type" , "{" , ident , { "," , ident } , "}" ,
             "from" , string ;

value      = record | array | map | string | number | bool | variant ;

record     = ident , "{" , [ field , { "," , field } , [ "," ] ] , "}" ;
field      = ident , ":" , value ;

array      = "[" , [ value , { "," , value } , [ "," ] ] , "]" ;

map        = "[" , ":" , "]"
           | "[" , string , ":" , value ,
                 { "," , string , ":" , value } , [ "," ] , "]" ;

variant    = ident
           | ident , "(" , value , { "," , value } , ")" ;

string     = '"' , { char | escape } , '"'
           | '"""' , { any } , '"""' ;          (* multi-line, RFC-0054 *)

number     = int | float ;
int        = [ "-" ] , digit-nonzero , { digit } | [ "-" ] , "0" ;
float      = int , ( "." , digit , { digit } ) , [ exponent ]
           | int , exponent ;

bool       = "true" | "false" ;

comment    = "//" , { any } , newline ;
doc-comment= "///" , { any } , newline ;
```

Every production above is already in Vyrn's grammar. The delta is
subtractive: VON removes operators, calls, `if`, `match`, bindings,
interpolation and every statement form. There is nothing to add.

Three notes on the grammar:

- **`ident (…)` is variant construction only.** It is not a function call.
  The parser accepts the shape; the checker resolves the name against the
  target type's variants, and an unresolved name is an error that names the
  type and lists its variants. There is no call machinery in a VON reader.
- **Trailing commas are already legal.** `Parser::struct_lit` in
  `compiler/vyrn-frontend/src/parser.rs` breaks out of its loop when a comma
  is followed by `}`. Constraint (c) is met with no work.
- **The header is required in a standalone `.von` file.** This is what keeps
  VON a subset rather than an extension: the top value is a *named* record
  literal, exactly as in `.vyrn`, so no contextual bare-`{` production is
  needed. Embedded VON read through `fromVon(T, s)` may omit the header —
  see §2.11 for the one grammar production that would cost.

### 2.4 Strictness rules

These are the "explicit versions of anything YAML got wrong" (constraint b).
Each is stated as a rule with a reason.

1. **No implicit typing. Ever.** A bareword is a variant of the field's
   declared enum type, or an error. `NO` is not `false`; it is a name looked
   up in a type. If the field is `String`, `NO` is an error telling you to
   quote it. The Norway problem is not fixed in VON, it is
   *unrepresentable* (§1.2): there is no code path that guesses a type from
   a token's spelling, because the type is known before the token is read.
   The precedent is already in the language — `logging { level: debug, sink:
   stderr }` (RFC-0008) resolves `debug` and `stderr` against declared
   enums.
2. **Duplicate record fields are an error**, naming both lines. The checker
   already emits `line N: duplicate field 'x' in record 'R'`.
3. **Duplicate map keys are an error**, naming both lines. This is a
   *deliberate divergence* from `fromJson`, where duplicate map keys are
   last-wins (RFC-0028). The wire must accept what arrives; a file you wrote
   must be right.
4. **Unknown fields are an error**, with a did-you-mean over the declared
   field names. This is the second deliberate divergence from RFC-0018,
   which ignores unknown JSON fields for forward compatibility. A typo in a
   config file is the most common config bug there is, and forward
   compatibility is not worth it in a file that a human edits. A type that
   genuinely wants open extension declares a `Map<String, V>` field and says
   so.
5. **Numbers are exact and unambiguous.** No leading zeros (so no octal
   ambiguity — §1.2's `mode: 0777`), no `+` prefix, no `NaN`, no `Infinity`,
   no hex, no sexagesimal (§1.2's `22:22`). An integer target parses the
   verbatim source text into
   `i64`/`u64` with a range check against the field's declared width — the
   RFC-0018 codec already does exactly this, so `9007199254740993`
   round-trips instead of becoming `9007199254740992` (§1.1: cJSON turns
   `10000000000000000999` into `10000000000000002048`). A float target
   requires a `.` or an exponent, matching
   Vyrn's own literal rule (`examples/floats.vyrn`): `1` is never silently a
   float, and `1.0` is never silently an integer.
6. **Strings are quoted, always, and are UTF-8 bytes.** Escapes are Vyrn's
   escapes. `"""…"""` is the multi-line form (RFC-0054): inside it a lone `"`
   or `""` is literal and only `"""` closes. No trailing-whitespace
   significance, no chomping indicators, no five block-scalar modes.
7. **`\{` in a string is an error.** Interpolation does not exist in VON, so
   the sequence that means interpolation in Vyrn must not silently mean
   something else here.
8. **Encoding and layout.** UTF-8, no BOM. Both LF and CRLF are read; `fmt`
   preserves the file's existing style, as RFC-0017 already specifies, so a
   Windows-authored config is not a spurious diff. Indentation is
   4 spaces and carries **no meaning** — a VON file with every newline
   removed parses identically. Tabs are rejected as indentation.
9. **Version.** VON has no format version field. The **type is the version**,
   and the type lives in a Vyrn module that can be pinned by content hash in
   `vyrn.lock` (RFC-0010 M4). A version number tells you a schema changed; a
   sha256 tells you *which* schema you have. The grammar's version is Vyrn's
   version, because the grammar is a subset of Vyrn's. §1.2 and §1.6 are the
   warning: YAML's two live versions make one document mean two things, and
   KDL 2.0 turned every v1 boolean into a syntax error. A format that
   versions itself has to break documents to move; a format whose schema is
   a pinned type moves one project at a time.

### 2.5 Mapping to Vyrn types

| VON | Vyrn type | Notes |
| --- | --- | --- |
| `Name { … }` | record (structural) | Field order in the file is irrelevant. `toVon` emits declaration order, as `toJson` does. |
| `[a, b]` | `Array<T>` | Also `Array<T, N>`; a length mismatch is an error naming N. |
| `["k": v]`, `[:]` | `Map<String, V>` | Insertion order preserved (RFC-0028). |
| `"s"`, `"""s"""` | `String` and validated strings | `where value.byteLength >= 1`, `pattern`, etc. all run. |
| `123` | `Int64`, `Int8/16/32`, `UInt8`… | Exact parse, range-checked against the width. |
| `1.5`, `1e9` | `Float64`, `Float32` | |
| `true` / `false` | `Bool` | |
| `Admin` | payload-less enum variant | The bareword rule. |
| `Circle(5)`, `Rect(2, 3)` | payload enum variant | |
| `Some(v)` / field omitted | `Option<T>` | Absent means `None`, matching RFC-0018. An explicit `None` is also legal. |
| `Ok(v)` / `Err(e)` | `Result<T, E>` | RFC-0024's shape, spelled in Vyrn rather than externally tagged. |
| — | `Ref`, `Task`, `Template`, `Validation<T>` | Not representable. The checker names the offending field, as the codec already does. |

**Validation is the point.** Every `where` clause in the target type runs on
load. Failures do not stop at the first one: they accumulate into
`Array<Issue>` with `key`, `path` and `message` (RFC-0009), exactly like
`fromJson`. A configuration file with four mistakes reports four mistakes.

**`schemaOf(T)` makes the schema story free.** A VON file's schema is a Vyrn
type; `schemaOf(T)` renders it as JSON Schema for anything outside the Vyrn
world, and `import type { T } from "./x.schema.json"` goes the other way. A
VON file can therefore be checked against a schema that was authored in
JSON Schema by a team that has never heard of Vyrn.

### 2.6 Worked example 1 — `vyrn.json` becomes `vyrn.von`

Today, `examples/shelf/vyrn.json`:

```json
{
  "name": "shelf",
  "server": "server.vyrn",
  "client": "client/boot.vyrn",
  "public": "public",
  "audience": { "server": ["server"], "client": ["client"], "universal": ["app", "shared"] }
}
```

The type it has always had, now written down (`std/manifest.vyrn`):

```vyrn
/// A project name: a non-empty lowercase slug.
export type PackageName = String where value =~ "[a-z][a-z0-9-]*"

/// A path relative to the manifest's own directory.
export type RelPath = String where value.byteLength >= 1

/// The microarchitecture `vyrn build` targets (RFC-0010 M3).
export type NativeTarget = | V1 | V2 | V3 | V4 | Native

export type Manifest = {
    name: PackageName,
    main: Option<RelPath>,
    server: Option<RelPath>,
    client: Option<RelPath>,
    public: Option<RelPath>,
    nativeTarget: Option<NativeTarget>,
    dependencies: Map<String, String>,
    audience: Map<String, Array<String>>,
}
```

And `examples/shelf/vyrn.von`:

```vyrn
/// The shelf demo — a full-stack Vyrn project (RFC-0019).
import type { Manifest } from "std/manifest"

Manifest {
    name: "shelf",

    // Entry points. `server` holds `fn handle`; `client` builds to wasm.
    server: "server.vyrn",
    client: "client/boot.vyrn",
    public: "public",

    // Which directories may be reached from where (RFC-0072).
    audience: [
        "server": ["server"],
        "client": ["client"],
        "universal": ["app", "shared"],
    ],
}
```

What the rewrite bought, concretely:

- `nativeTarget` is an enum. `"v5"` is now caught by the parser with the
  five legal values listed, instead of by the hand-written check in
  `native_target_for` that produces `unknown 'nativeTarget' 'v5' in …`.
  One less piece of format knowledge in `vyrn-cli/src/main.rs`.
- `main` and `server` are `Option<RelPath>`. Omitting both is
  representable; misspelling `serverr` is not.
- The comment above `audience` cannot be written in `vyrn.json` at all.
- The manifest's schema is a published type. `schemaOf(Manifest)` gives
  editors JSON-Schema completion for anyone still writing `vyrn.json`.

### 2.7 Worked example 2 — a server config with real constraints

```vyrn
/// Production configuration for the shelf service.
import type { ServerConfig } from "./config.vyrn"

ServerConfig {
    host: "0.0.0.0",
    port: 8443,          // Port = Int64 where value >= 1 && value <= 65535

    // `workers: None` would mean "one per core"; pin it here.
    workers: Some(8),

    tls: Enabled(TlsFiles {
        cert: "/etc/shelf/fullchain.pem",
        key: "/etc/shelf/privkey.pem",
    }),

    logging: LogConfig {
        level: Warn,     // a variant of `Level`, not the string "warn"
        sink: Stderr,
    },

    // Multi-line strings need no indentation indicator and no chomping mode.
    banner: """
    shelf 2.1
    Report problems to ops@example.com
    """,

    limits: Limits {
        maxBodyBytes: 1048576,
        requestTimeoutMs: 30000,
        maxConnections: 4096,
    },
}
```

The version of this file that YAML would accept, and what each line would do
there: `port: 8443` is fine, `host: 0.0.0.0` is a string only by accident of
not matching a number pattern, `level: warn` is a string that some loader
maps to an enum at runtime, `tls: on` is a boolean in YAML 1.1 and a string
in YAML 1.2, and the banner needs `|-` or `|+` and a correct indentation
column. In VON every one of those is decided by the type, before the file is
read.

### 2.8 Worked example 3 — a theme (the numeric-key case)

`examples/shelf/theme.json` is a nested dictionary whose keys are numbers
written as strings — the case that breaks YAML and that JSON only survives
because JSON keys are always strings.

```vyrn
/// The design token set consumed by `std/tw` (RFC-0032).
import type { Theme } from "std/tw"

Theme {
    // Keys stay strings because `Map<String, V>` says so. There is no rule
    // by which `500` could become an integer.
    colors: [
        "brand": ["500": "#4f46e5", "600": "#4338ca"],
        "gray": ["200": "#e5e7eb", "500": "#6b7280", "700": "#374151"],
        "red": ["500": "#ef4444", "600": "#dc2626"],
        "white": "#ffffff",     // ERROR — see below
    ],

    spacing: ["0": "0", "1": "0.25rem", "2": "0.5rem", "4": "1rem"],
    radius: ["DEFAULT": "0.5rem", "sm": "0.25rem", "lg": "0.75rem"],
    breakpoints: ["sm": "640px", "md": "768px"],

    safelist: [
        "card", "cardbody", "rating", "tags", "issues",
        "book", "meta", "title", "detail", "row", "rate", "danger",
    ],
}
```

The `"white": "#ffffff"` line is the bug that the JSON version ships today:
`colors` is a `Map<String, Map<String, String>>` everywhere except that one
entry, which is a bare string. JSON cannot object. VON's checker does, and
§2.9 shows what it says. The fix is either a shape change in `Theme` (a
`ColorScale` enum with `Scale(Map<String, String>)` and `Flat(String)`
variants) or `"white": ["DEFAULT": "#ffffff"]`. Either way the decision is
made once, in the type, instead of being rediscovered by every reader.

### 2.9 Error messages

VON errors are Vyrn diagnostics (RFC-0006): source-anchored, intent-first,
with a ranked fix. Because a VON file is read by a `gen fn` that emits
origin directives (RFC-0033), the location a diagnostic reports is the
`.von` file's own line and column, not a position in generated text.

**A constraint violation**

```
error: theme.von:12:20 — `500` is not a valid `Hex`

  12 |         "brand": ["500": "#4f46g5", "600": "#4338ca"],
     |                          ^^^^^^^^^ this value fails the type's rule

  `Hex` is declared in std/tw.vyrn:8:
      type Hex = String where value =~ "#[0-9a-f]{6}"
  and "#4f46g5" does not match (byte 5 is `g`).

  Fixes:
    • correct the digit:  "#4f46c5"
    • if this is meant to be a named colour, `Theme.colors` takes a
      `Map<String, Map<String, Hex>>` — add the name to `std/tw`'s palette
```

**A shape mismatch — the `"white"` line from §2.8**

```
error: theme.von:17:18 — `colors."white"` needs a colour scale, not one colour

  16 |         "red": ["500": "#ef4444", "600": "#dc2626"],
     |                ------------------------------------ a scale: Map<String, Hex>
  17 |         "white": "#ffffff",
     |                  ^^^^^^^^^ a single Hex

  `Theme.colors` is `Map<String, Map<String, Hex>>` (std/tw.vyrn:22), so
  every entry is a scale keyed by weight.

  Fixes:
    • give it a weight:   "white": ["DEFAULT": "#ffffff"]
    • widen the type:     make Theme.colors' value a
                          `| Scale(Map<String, Hex>) | Flat(Hex)`
```

**The Norway problem, as VON reports it**

```
error: deploy.von:4:15 — `NO` is not a variant of `Country`

   4 |     region: NO,
     |             ^^ no such variant

  `Country` (std/geo.vyrn:11) has: Norway, Sweden, Denmark, Finland.
  A bare word in VON always names a variant — it is never a boolean and
  never a string.

  Fixes:
    • name the variant:  region: Norway
    • if `Country` should be a plain String, quote it:  region: "NO"
```

**Accumulated failures**

A load reports every problem at once, because it goes through
`Validation<T>` (RFC-0009) rather than failing at the first one:

```
error: config.von — 3 problems

  config.von:6:11   port.max        port      65536 is above `Port`'s maximum of 65535
  config.von:9:5    field.unknown   —         unknown field `wokers` (did you mean `workers`?)
  config.von:14:9   map.duplicate   limits    duplicate key "maxBodyBytes" (first at 12:9)
```

The `key` column is the stable `Issue.key`, so a wrapper can translate these
through `std/i18n` (RFC-0020) without parsing English.

### 2.10 Implementation shape

**Zero compiler changes.** The pieces exist:

| Job | Existing mechanism |
| --- | --- |
| Tokenise the `.von` file | `lex()` — the compiler's own lexer, exposed to generator code (RFC-0054) |
| Reject non-literal forms | a walk over the token stream in `std/von`, in Vyrn |
| Read the file at build time | `readFile` inside a `gen fn`, sandboxed and cached (RFC-0021) |
| Turn it into a checked value | emit `export let config = Manifest { … }` as Vyrn source; the ordinary checker then runs every `where` clause |
| Anchor diagnostics in the `.von` file | `//@origin` directives (RFC-0033), emitted by `vyrn"…"` quotes |
| Format it | `vyrn fmt` — it is a token-stream formatter (RFC-0017) and these are Vyrn tokens |

Compile-time surface:

```vyrn
import { von } from "std/von"
import { config } from von("./app.von")
```

Runtime surface, mirroring RFC-0018 exactly:

```vyrn
fromVon(T, s) -> Validation<T>    // never traps; every problem at once
toVon(x)      -> String           // canonical, fmt-shaped, newlines and indentation
```

`toVon` differs from `toJson` in one way: `toJson` is compact and
whitespace-free because it is a wire format; `toVon` emits the canonical
`fmt` layout because it is a file a human will read and `git diff` will
show. Both are deterministic and both put record fields in declaration
order.

**The formatting rule that `vyrn fmt` needs.** RFC-0017 v1 never joins or
splits lines. For VON that is not quite enough for stable diffs: a
multi-line record should always carry a trailing comma (so adding a field
touches one line, not two) and a single-line record should never carry one.
That is a small, VON-only addition to the formatter's rule set, and it does
not disturb the safety invariant (`lex(fmt(src)) == lex(src)` modulo the
tokens the formatter is licensed to drop).

### 2.11 Migration and adoption

The claim VON has to win is narrow: **stop writing JSON inside a Vyrn
project.** It does not have to displace YAML at large, and it should not try
to. That narrowness is the difference between this and the typed-config
tier, which had to convert the world because it had no world of its own.

| Milestone | Content | Risk |
| --- | --- | --- |
| **M0** | Nothing. `vyrn fmt` already formats a `.von` file, because a `.von` file is Vyrn tokens. Verify it and register the extension in the VS Code grammar. | none |
| **M1** | `std/von`: the strictness walk over `lex()`, plus `toVon`. `vyrn fmt --from-json <file>` converts a JSON file to VON, using `std/json`'s reader (RFC-0059), which already rejects duplicate keys and trailing commas. | small |
| **M2** | `std/manifest`, and `vyrn.von` accepted wherever `vyrn.json` is. If both exist, `vyrn.json` wins and a warning names the shadowed file. `vyrn.json` is **never** removed — a single file must stay runnable with no ceremony (RFC-0010 M3's rule). | medium: two manifest readers until one is retired |
| **M3** | `import { x } from von("./x.von")` — compile-time load. This is the milestone that makes VON worth having over JSON: a config error becomes a build error, and the loaded value is a constant. | medium: generator emitting a module that itself has an `import type` header — see the open question below |
| **M4** | `fromVon` at runtime, and the interop guarantee: `toJson(fromVon(Config, s))` produces JSON for anything outside Vyrn. VON never becomes the only way to talk to the outside world. | small |

The escape hatch matters as much as the format. Every VON document maps to
JSON through `toJson`, and every JSON Schema maps to a Vyrn type through
`import type`. A project that adopts VON does not become unreachable to
`jq`, to a CI system, or to a colleague who does not write Vyrn.

**Against the six things that killed the typed tier** (§1.8):

| The failure | VON's answer |
| --- | --- |
| 1. One more language to learn | Zero. VON is Vyrn's record literal. The reader of a `.von` file has already read a hundred of them. |
| 2. Evaluation cost is structural | VON does not evaluate. Load time is parse time, by construction. This is the whole reason §2.2 refuses expressions. |
| 3. Editor support arrives years late | Day one. `.von` is Vyrn tokens, so `vyrn fmt`, semantic highlighting (RFC-0047), hover and completion work through the code that already exists. |
| 4. "It compiles to YAML anyway" | It does not compile to anything. The loaded value is a Vyrn record with its `where` clauses checked. The type survives past the file boundary, which is the only place a type is worth having. |
| 5. Toolchain weight | Zero new binaries. The reader is a std module; the runtime is `vyrn`, which is already installed because you are writing Vyrn. |
| 6. A spec without implementations is a document | VON has no separate specification and no second implementation. It is a restriction on one parser. There is nothing to keep in sync. |

The honest limit: this argument only works **inside a Vyrn project**. VON has
no claim on a Kubernetes cluster or a Python service, and it should not make
one. Every tool in §1.7 that tried to convert the world lost; the one that
won (Jsonnet) won by emitting JSON and changing nothing downstream. VON's
equivalent is `toJson`.

### 2.12 Open questions

- **OPEN — headerless documents.** `fromVon(T, s)` on a string with no
  header needs a top-level bare `{ … }`, which is not a legal Vyrn
  expression (record literals are always `Name { … }`). Contextual literals
  already exist for `[]` and `[:]`, so a contextual `{}` is consistent, but
  it is one production of grammar delta and it breaks the "VON is exactly a
  subset" claim. Options: require the header everywhere; allow the bare form
  only for embedded VON; or require `T { … }` even when `T` is also given at
  the call site, and check that they agree.
- **OPEN — where the header's path resolves.** A `.von` file's
  `import type { T } from "./config.vyrn"` is read by a generator and re-emitted
  into a synthesized module. Relative to what — the `.von` file, or the
  importing `.vyrn` file? RFC-0021 resolves generator arguments relative to
  the importing file. The two must not disagree silently.
- **OPEN — document composition.** Per-environment overlays are the one
  genuine use for a feature VON refuses. The candidate answer is a Vyrn
  function (`merge(base, prod)`) plus a `Partial<T>`-typed overlay document,
  which needs no format feature at all. It should be tried before any
  format-level `include` is considered.
- **OPEN — unknown fields.** §2.4 rule 4 makes them an error. That is right
  for a hand-edited file and wrong for a manifest read by an older compiler
  than the one that wrote it. Is a per-type opt-in (`type T = { … } open`)
  worth the complexity, or is version-by-content-hash (§2.4 rule 9) enough?
- **OPEN — `Result` in a config file.** `Ok(v)` / `Err(e)` is representable
  and probably meaningless in configuration. Ban it, or leave it legal
  because banning costs a special case?
- **OPEN — comments in `toVon` output.** A round-trip through
  `fromVon`/`toVon` loses comments, which makes `toVon` unsafe as a rewriting
  tool (the `vyrn add` path rewrites `vyrn.json` textually today for exactly
  this reason). Either `toVon` is documented as output-only, or VON needs a
  comment-preserving edit API. The formatter's comment-preserving lex pass
  (RFC-0017) is most of the machinery.
- **OPEN — the extension.** `.von` collides with nothing known, but it is
  also not obviously readable. `.vyn`? Left open deliberately; the name is
  the cheapest thing here to change.
