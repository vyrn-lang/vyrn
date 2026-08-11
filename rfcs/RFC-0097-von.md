# RFC-0097 — VON, Vyrn Object Notation

- **Status:** **M0 and M1 shipped. M2, M3 and M4 not started.** M1 is the
  strictness reader, the canonical writer and the JSON converter, all in
  `std/von`. The reader runs at **generation** time only, and that is a real
  limit rather than a staging choice — see §7. The zero-compiler-change claim
  held for the language and did not hold for the driver: no crate under
  `compiler/` changed except `vyrn-cli`'s argument handling, which gained one
  flag and no format knowledge (§9).
- **Depends on:** RFC-0002 (records), RFC-0003 (validated types — M3),
  RFC-0009 (`Validation<T>` / `Issue` — M3), RFC-0010 (modules, manifest,
  `import type`), RFC-0017 (`vyrn fmt`), RFC-0021 (generator imports,
  `readFile` at compile time), RFC-0028 (`Map`), RFC-0033 (origin maps — M3,
  once a value is spliced against its type; M1 anchors its own diagnostics
  from the token positions and needs no directive),
  RFC-0054 (code quotes, `lex()`), RFC-0059 (`std/json` — the reader the
  converter uses), RFC-0096 (`impl Owned` on a self-referring type — `Von` is
  one)
- **Research:** [`docs/research/von.md`](../docs/research/von.md). Part 1 is a
  census of JSON, YAML, TOML, HCL, JSON5/JSONC, KDL, Dhall, CUE, Pkl, Nickel,
  StrictYAML and Jsonnet, with every URL and issue number checked. This RFC
  does not repeat it; it cites it.
- **Principle:** VON is not a new format. **VON is Vyrn's record-literal
  grammar saved to a file**, with an `import type` header that names its own
  schema.

---

## 1. The question

A Vyrn project writes its configuration in JSON, and JSON is the one format in
the census that cannot say what it means: duplicate keys are legal and
undefined, there is one numeric production and no integer, no two of thirty
parsers agree, and a human cannot write a comment. The research note's §1.1
carries the citations.

Every alternative in the census bought a fix and paid for it with a second
language. §1.8 records what that cost: one more language to learn beats every
technical argument, evaluation is structurally expensive, editor support
arrives years late, and the types are erased at the file boundary anyway.

Vyrn already owns every part of a configuration format except the file
extension — object syntax, array syntax, dictionary syntax, a schema, `where`
constraints, a schema exporter, a formatter, a lexer that library code can
call, and a compile-time file reader. So the answer is not to design a format.
It is to **stop the subtraction at the literal grammar and give the result an
extension.**

---

## 2. The design

A VON document is a header followed by **one value**.

```vyrn
/// The vondemo service, configured.
import type { AppConfig } from "./vondemo"

AppConfig {
    name: "vondemo",
    // A comment, which the JSON this replaces could not hold at all.
    port: 8443,
    workers: Some(8),
    level: Warn,
    hosts: ["0.0.0.0", "::1"],
    limits: ["maxBodyBytes": 1048576, "requestTimeoutMs": 30000],
}
```

Every production is already Vyrn's. The delta is **subtractive**: VON removes
operators, calls, `if`, `match`, bindings, string interpolation and every
statement form. There is nothing to add, which is why there is no second
grammar to keep in sync and no second specification to write.

The header is required, and it is what keeps VON a subset rather than an
extension. The top value is a *named* record literal, exactly as in a `.vyrn`
file, so no contextual bare-`{` production is needed. §11 carries the
headerless case forward as open.

### The one real decision: pure data

**VON is pure data.** No interpolation, no references, no anchors, no value
imports, no arithmetic, no conditionals, no functions. The argument is not that
expressions are bad. It is that the expression tier has to live somewhere, and
Vyrn is a better place for it than a configuration format is. Repetition across
environments — the reason YAML anchors, Jsonnet, Dhall functions and CUE
unification exist — is answered one layer up: an `Option<T>` field defaults in
the type, `merge(base, prod)` is an ordinary function over records, and a
config that must genuinely be computed is written as a `gen fn` whose output is
committed.

This is the move the typed-config tier could not make, because none of those
tools had a host language to delegate to.

---

## 3. Strictness

Each rule answers a documented failure of an existing format.

1. **No implicit typing.** A bare word is a variant of the field's declared
   type, or an error. YAML's Norway problem is not fixed here, it is
   *unrepresentable*: no code path guesses a type from a token's spelling.
2. **Duplicate record fields are an error**, naming both lines.
3. **Duplicate map keys are an error**, naming both lines. A deliberate
   divergence from `fromJson`, where duplicates are last-wins: the wire must
   accept what arrives, a file you wrote must be right.
4. **Numbers are decimal digits and nothing else.** A leading zero is an error,
   so YAML's `mode: 0777` — 511 under 1.1, 777 under 1.2 — has no VON reading
   at all. No hex, no exponent, no digit separator, no `+` prefix, no `NaN`, no
   `Infinity`, no sexagesimal. Numbers keep their **verbatim** source text
   through the reader and the writer, so `9007199254740993` and
   `18446744073709551615` both survive.
5. **Strings are quoted, always.** Escapes are Vyrn's escapes. `"""…"""` is the
   multi-line form.
6. **`\{` in a string is an error.** It means interpolation in Vyrn, so it must
   not silently mean something else here.
7. **UTF-8, no byte-order mark, no tab in indentation.** Indentation carries no
   meaning, which is exactly why a tab must not be able to change how a file
   looks.
8. **No version field.** The **type is the version**, and a type lives in a
   module that `vyrn.lock` pins by content hash. A version number says a schema
   changed; a sha256 says which schema you have. KDL 2.0 turned every v1
   boolean into a syntax error; a format whose schema is a pinned type moves
   one project at a time.

The research note's §2.4 rule 4 — **unknown fields are an error**, with a
did-you-mean over the declared field names — needs the declared type, so it
lands with M3. §11 carries it as open.

---

## 4. Mapping to Vyrn types

| VON | Vyrn type |
|---|---|
| `Name { … }` | record (field order in the file is irrelevant) |
| `[a, b]` | `Array<T>` |
| `["k": v]`, `[:]` | `Map<String, V>`, insertion order preserved |
| `"s"` | `String` and every validated string over it |
| `123` | `Int64`, `Int8/16/32`, `UInt8`… range-checked against the width |
| `1.5` | `Float64`, `Float32` |
| `true` / `false` | `Bool` |
| `Admin` | payload-less variant |
| `Rect(2, 3)` | payload variant |
| `Some(v)`, or the field omitted | `Option<T>` |
| — | `Ref`, `Task`, `Template`: not representable |

---

## 5. What M1 shipped

`std/von.vyrn` (1361 lines), and nothing else in `std/`.

### The value tree

```vyrn
export type Von =
    | VRecord(String, Array<VonField>)
    | VVariant(String, Array<Von>)
    | VArray(Array<Von>)
    | VMap(Array<VonEntry>)
    | VStr(String)
    | VInt(String)
    | VFloat(String)
    | VBool(Bool)
```

`VInt` and `VFloat` carry the **raw source text**, not a parsed number. That is
what makes rule 4 true through the writer as well as the reader, and it is why
emit → parse → emit is byte-stable.

`Von` reaches itself through four variants, so the structural release walk has
no bottom. `impl Owned for Von` and `impl Copy for Von` are the declarations
RFC-0096 asks for, in the shape `std/json`'s `Json` already uses.

### The reader

`parseVon(src) -> Result<VonDoc, String>` — a strictness walk over `lex()`'s
token stream. Two facts make the walk small:

- **Comments never arrive.** `lex` emits no token for `//`, and a whole string
  literal is one token, so a `{` inside a comment or a string cannot be
  mistaken for structure. `///` doc comments arrive as `doc` tokens and are
  skipped.
- **The lexer is the compiler's own.** That is the whole thesis in one call:
  VON is a subset of Vyrn because it is read by Vyrn's lexer, not by an
  imitation of it.

The one place the walk looks past `lex()` is the number rule. A token carries
the lexer's *parsed* value, and `0777`, `777` and `00777` parse alike, so the
reader reads the digits back out of the source bytes at the token's line and
column.

### The writer

`toVon(doc)` and `emitVon(v)` produce the canonical `fmt` layout: 4-space
indentation, one part per line with a trailing comma wherever a container
breaks, and one line wherever every part is flat. `toVon` differs from `toJson`
in exactly one way, and for one reason: `toJson` is compact because it is a
wire format, `toVon` is laid out because it is a file a person reads and
`git diff` shows.

The canonical text is a **fixed point of `vyrn fmt`** — pinned by a test, not
asserted (§6).

### JSON in, VON out

`jsonToVon(json, typeName, module)`, and `vyrn fmt --from-json <file.json>
[--as <Type>] [--from <module>]` at the command line.

The result is a starting point, not an answer: JSON says nothing about types,
so every **nested** object arrives as a `Map` and the author, who has the type,
promotes what should be a record. What the conversion does answer is everything
JSON got wrong on its own terms — `std/json`'s strict reader rejects a
duplicate key before the walk runs, `null` has no VON spelling and says so, and
a number keeps its verbatim digits.

### The compile-time loader

`examples/lib/gen_von.vyrn` is a generator import (RFC-0021): it reads a `.von`
file, runs the walk, and emits an ordinary Vyrn module. A malformed document
therefore fails the **build**, positioned in the `.von` file. M1's module bakes
the document's canonical text and its shape; loading it as a **checked value of
the type the header names** is M3.

---

## 6. Milestones

| Milestone | Content | Status |
|---|---|---|
| **M0** | Nothing to build. `vyrn fmt` already formats a `.von` file, because a `.von` file is Vyrn tokens. Verify it; register the extension in the editor. | **Shipped** |
| **M1** | `std/von`: the strictness walk over `lex()`, the canonical writer, and `vyrn fmt --from-json`. | **Shipped** |
| **M2** | `std/manifest`, and `vyrn.von` accepted wherever `vyrn.json` is. If both exist, `vyrn.json` wins and a warning names the shadowed file. `vyrn.json` is **never** removed. | Not started |
| **M3** | `import { x } from von("./x.von")` loading a **checked value**: the document is spliced against its declared type, every `where` clause runs, and failures accumulate as `Array<Issue>`. This is the milestone that makes VON worth having over JSON. | Not started |
| **M4** | `fromVon(T, s) -> Validation<T>` at run time, and the interop guarantee that `toJson` reaches anything outside Vyrn. | Not started |

### M0 acceptance evidence

- `vyrn fmt --check examples/vondemo.von` exits 0 with **no formatter change of
  any kind**. Pinned by
  `compiler/vyrn-cli/tests/von.rs::a_von_document_is_already_canonically_formatted_vyrn`.
- `editor/vscode/package.json` contributes a `von` language over `.von` and
  maps it to the **existing** `source.vyrn` grammar. It is a separate language
  id rather than an alias of `vyrn` on purpose: the LSP's document selector
  names `vyrn` and `vyx`, and a `.von` file is not a Vyrn *program*, so
  aliasing would have put a parse error under every document. Highlighting on
  day one, no false diagnostics.

### M1 acceptance evidence

- **15 inline tests** in `std/von.vyrn`, green under `vyrn test std/von.vyrn`:
  the round trip as a fixed point, the bare word as a variant, verbatim number
  text, the leading zero, hex/exponent/separator, duplicates naming both lines,
  interpolation and operators refused, a keyword as a field name, the required
  header and the single value, the tab, the line-breaking rule, a variant
  payload's layout, escapes, and the JSON conversion with its four refusals.
- **`compiler/vyrn-cli/tests/von.rs`**, six tests through the real `vyrn`
  binary: the inline suite runs green, a `.von` file is already canonically
  formatted, `toVon`'s output is a fixed point of `vyrn fmt`, a malformed
  document fails `vyrn check` with the position in the `.von` file, and
  `--from-json` converts `examples/shelf/vyrn.json` to a document that `vyrn
  fmt --check` then accepts.
- **`examples/vondemo.vyrn` + `examples/vondemo.von`** — the reader end to end
  under all three backends. The parity harness discovers every
  `examples/*.vyrn`, so the document is read at generation time and its
  canonical text is the program's output, identically under the interpreter,
  the native backend and wasm.

### Diagnostics

Every reader error carries `line N, col M:` in the **`.von` source**, and the
caller prefixes the file name. The generator import prefixes the path it was
given, so a build failure reads:

```
./bad.von: line 4, col 11: `0777` has a leading zero; write it as a decimal
number, or quote it if it is text
```

No message names a token kind, a synthesized module, or any other word the
reader cannot type. `showTok` exists for that reason alone: it reports a
token's **source spelling**, or `a string`, or `the end of the file` — never
`punct`. The test asserts the absence of `punct`, `ident token` and
`generated by` in the failure output.

The `vyrn fmt --from-json` path is the same rule from the other side. The
converter is a small Vyrn program the CLI carries as a constant, and a trap in
it would otherwise report a position in a module the user never wrote and
cannot open, so the internal location is stripped and the **input** file's name
takes its place.

---

## 7. The limit M1 stopped at

**The reader runs at generation time only.** `lex()` is a generation-only
builtin (RFC-0054) with no lowering in any backend, so every caller of
`parseVon` must be a `gen fn`. That is the right place for M1's job — a config
error becomes a build error — and it is the binding limit on M4: a run-time
`fromVon` needs a tokenizer of its own, not a second strictness walk. The walk
below `vonLex` is ordinary Vyrn over ordinary records precisely so that M4 can
reuse it unchanged.

Two smaller limits, recorded rather than fixed:

- **A multi-line string round-trips to the single-line form.** `"""a\nb"""`
  reads correctly and `toVon` writes it back as `"a\nb"`. The value survives;
  the spelling does not.
- **Comments do not survive a round trip**, because `lex` emits no token for
  one. `toVon` is therefore for *writing* a document, not for rewriting one
  somebody else wrote. §11 carries this forward.

---

## 8. Alternatives refused

| Alternative | Verdict | Reason |
|---|---|---|
| A hand-written VON parser in `std/von` | Refused | It would make VON an *imitation* of Vyrn's grammar rather than a subset of it, and the two would drift. `lex()` is the whole argument. |
| A VON reader in Rust, in the compiler | Refused | Same drift, plus it puts format knowledge back in the compiler — the opposite of RFC-0021's stated direction. The CLI's `--from-json` carries bytes and a Vyrn program, and no format knowledge. |
| A second JSON reader for `--from-json` | Refused | `std/json`'s strict reader already rejects duplicate keys and trailing commas (RFC-0059). A converter that disagreed with the reader would be worse than no converter. |
| String interpolation `"\{x}"` | Refused | Needs a scope; a scope needs bindings; bindings need an evaluation order. It is the first step of the whole staircase, and `\{` is a hard error so that a pasted Vyrn template cannot silently change meaning. |
| Anchors, aliases, merge keys | Refused | The largest single source of "the file does not say what it does" in YAML, and the billion-laughs vector (CVE-2019-11253). |
| `${ENV_VAR}` substitution | Refused | Hides the effective value and puts secret-shaped holes in a file that looks static. The host reads the environment, visibly, in code. |
| `null` | Refused | Vyrn has no null. Absence is `None`. Two spellings of nothing is one too many. |
| A `von` language keyword | Refused | RFC-0019's `rpc` keyword was built and reverted for this reason: a generator import is already the mechanism, and a keyword adds surface without adding capability. |
| Aliasing `.von` onto the `vyrn` language id in the editor | Refused | It would run the LSP over a file that is not a Vyrn program. A separate id with the same grammar costs six lines and no false errors. |
| Emitting the document as a compact string constant | Refused | The generated module carries the **canonical** text, so `emit-gen` output is diffable and a reader can see the document they wrote. |

---

## 9. The zero-compiler-change verdict

The research note claimed zero compiler changes. Measured against the shipped
diff:

- **The language needed nothing.** No change to the lexer, the parser, the
  checker, the interpreter, code generation, the loader or the formatter. The
  whole reader, writer and converter is `std/von.vyrn`, a Vyrn module.
- **`vyrn fmt` needed nothing.** M0's claim was that the formatter already
  formats a `.von` file. It does, and the test pins it. The research note's
  §2.10 anticipated a "VON-only addition to the formatter's rule set" for
  trailing commas; that turned out unnecessary, because `toVon` writes the
  layout `fmt` already leaves alone rather than asking `fmt` to produce it.
- **The driver needed one flag.** `compiler/vyrn-cli/src/main.rs` is the only
  file under `compiler/` that changed (+95/-1): the `--from-json` argument, and
  a function that reads the input file, runs a constant Vyrn program through
  the interpreter, and rewrites the error's position. It holds no VON grammar
  and no JSON grammar.

So the honest verdict is **zero language changes, one CLI flag**. The claim
holds where it matters — a format that needed a parser change would not have
been a subset — and the note overstated it by one file.

---

## 10. Against the six things that killed the typed tier

| The failure (research note §1.8) | VON's answer |
|---|---|
| One more language to learn | Zero. VON is Vyrn's record literal. |
| Evaluation cost is structural | VON does not evaluate. Load time is parse time, by construction. |
| Editor support arrives years late | Day one, on the existing grammar and the existing formatter. |
| "It compiles to YAML anyway" | It compiles to nothing. M3 makes the loaded value a checked Vyrn record, so the type survives the file boundary. |
| Toolchain weight | Zero new binaries. The reader is a std module. |
| A spec without implementations is a document | There is no separate specification and no second implementation. VON is a restriction on one lexer. |

The honest limit is the same one the research note states: this argument works
**inside a Vyrn project**. VON has no claim on a Kubernetes cluster or a Python
service, and `toJson` is the escape hatch that keeps it from needing one.

---

## 11. Open questions

Carried forward from the research note's §2.12, plus two this milestone added.

- **OPEN — headerless documents.** `fromVon(T, s)` (M4) on a string with no
  header needs a top-level bare `{ … }`, which is not a legal Vyrn expression.
  Contextual literals already exist for `[]` and `[:]`, so a contextual `{}` is
  consistent, but it is one production of grammar delta and it breaks the "VON
  is exactly a subset" claim. Options: require the header everywhere; allow the
  bare form only for embedded VON; or require `T { … }` even when `T` is given
  at the call site, and check that the two agree.
- **OPEN — where the header's path resolves.** A `.von` file's
  `import type { T } from "./config.vyrn"` will be re-emitted into a
  synthesized module at M3. Relative to what — the `.von` file, or the
  importing `.vyrn` file? RFC-0021 resolves generator arguments relative to the
  importing file. The two must not disagree silently. M1 does not resolve the
  path at all, so nothing is decided yet.
- **OPEN — document composition.** Per-environment overlays are the one genuine
  use for a feature VON refuses. The candidate answer is a Vyrn function
  (`merge(base, prod)`) plus a `Partial<T>`-typed overlay document, which needs
  no format feature. Try it before considering a format-level `include`.
- **OPEN — unknown fields.** §3 makes them an error at M3. That is right for a
  hand-edited file and wrong for a manifest read by an older compiler than the
  one that wrote it. Is a per-type opt-in worth the complexity, or is
  version-by-content-hash (§3 rule 8) enough?
- **OPEN — `Result` in a config file.** `Ok(v)` / `Err(e)` is representable and
  probably meaningless in configuration. Ban it, or leave it legal because
  banning costs a special case? M1 reads it as an ordinary variant.
- **OPEN — comments in `toVon` output.** A round trip loses comments, which
  makes `toVon` unsafe as a rewriting tool — the `vyrn add` path rewrites
  `vyrn.json` textually today for exactly this reason. Either `toVon` stays
  documented as output-only, or VON needs a comment-preserving edit API. The
  formatter's comment-preserving lex pass is most of the machinery.
- **OPEN — the extension.** `.von` collides with nothing known, but it is also
  not obviously readable. `.vyn`? The name is the cheapest thing here to
  change, and M1 shipping does not settle it.
- **OPEN (new) — multi-line strings through the writer.** `toVon` writes
  `"""a\nb"""` back as `"a\nb"`. Should the writer choose the `"""` form when a
  string carries a newline, or is the escaped form the canonical one? It is a
  writer decision, not a grammar decision, and the reader accepts both either
  way.
- **OPEN (new) — a run-time tokenizer.** M4's `fromVon` cannot call `lex()`
  (§7). A byte-level tokenizer in `std/von` would be a *second* lexer, which is
  the drift this RFC refuses everywhere else. The alternative is to make the
  lexer available at run time, which is a language change and needs its own
  RFC. Neither option is cheap, and M4 should not start until one is chosen.
