# std/von

std/von (RFC-0097 M1) — VON, Vyrn Object Notation: Vyrn's record-literal
grammar saved to a file.

  import { parseVon, toVon, Von, VonDoc } from "std/von"

A `.von` document is an `import type { T } from "…"` header followed by ONE
literal value. Every production is already Vyrn's; the delta is subtractive.
There are no operators, no calls, no `if`, no `match`, no bindings and no
string interpolation, so reading a document costs exactly one parse and the
file says what it says.

The reader runs at GENERATION time. `lex()` — the compiler's own lexer, which
is what makes VON a subset of Vyrn rather than an imitation of one — is
available only inside a `gen fn` (RFC-0054), so `parseVon` is one too. That is
the right place for it: a config error becomes a build error. A runtime
`fromVon` would need a byte reader of its own (RFC-0097 M4), and it is not
this walk.

Strictness (RFC-0097 §4), each rule against a documented failure of an
existing format:

  - a bare word is a variant name, never a boolean and never a string
    (YAML's `NO` is Norway);
  - duplicate record fields and duplicate map keys are errors naming both
    lines (JSON leaves duplicates undefined);
  - numbers keep their VERBATIM source text, a leading zero is an error
    (YAML's `mode: 0777` is 511 under 1.1 and 777 under 1.2);
  - `\{` in a string is an error, because it means interpolation in Vyrn;
  - a tab in indentation and a byte-order mark are errors.

Errors carry `line N, col M:` positions in the `.von` source, the shape
`std/jsonread` uses, so a caller prefixes the file name and nothing else.

## VonField

```vyrn
type VonField = { name: String, value: Von, line: Int64 }
```

One `name: value` field of a record literal. `line` is the field name's own
1-based source line — what a duplicate-field error names, and what an origin
map (RFC-0033) anchors to.

## VonEntry

```vyrn
type VonEntry = { key: String, value: Von, line: Int64 }
```

One `"key": value` entry of a map literal.

## Von

```vyrn
type Von = VRecord(String, Array<VonField>) | VVariant(String, Array<Von>) | VArray(Array<Von>) | VMap(Array<VonEntry>) | VStr(String) | VInt(String) | VFloat(String) | VBool(Bool)
```

A VON value. Numbers carry their RAW, validated source text rather than a
parsed number: a config file's `9007199254740993` and its `18446744073709551615`
both survive the round trip, and emit → parse → emit is byte-stable.

## VonImport

```vyrn
type VonImport = { names: Array<String>, module: String }
```

One `import type { A, B } from "spec"` line of a document header.

## VonDoc

```vyrn
type VonDoc = { imports: Array<VonImport>, value: Von }
```

A whole document: its header, and the one value that follows it.

## copyVonArray

```vyrn
fn copyVonArray(xs: Array<Von>) -> Array<Von>
```

`copy` over a list of values. Exported because `xs.copy()` cannot be written
for this element type — see the impl above.

## copyVonFields

```vyrn
fn copyVonFields(fs: Array<VonField>) -> Array<VonField>
```

`copy` over a list of record fields, for `xs.copy()`'s reason.

## copyVonEntries

```vyrn
fn copyVonEntries(es: Array<VonEntry>) -> Array<VonEntry>
```

`copy` over a list of map entries, for `xs.copy()`'s reason.

## VonTok

```vyrn
type VonTok = { kind: String, text: String, line: Int64, col: Int64 }
```

One lexed token, in a type this module owns.

`lex()`'s own `Token` is generation-only — it may not be named in a type
declaration outside a `gen fn` (RFC-0054) — so `vonLex` copies each row into
this one. That confinement is worth having on its own terms: exactly one
function in this module needs a generation context, and the whole strictness
walk below is ordinary Vyrn over ordinary records. A future runtime reader
(RFC-0097 M4) needs a tokenizer, not a second walk.

## parseVon

```vyrn
fn parseVon(src: String) -> Result<VonDoc, String>
```

Read a whole VON document: a header of one or more `import type` lines, then
exactly one value.

A `gen fn` because `lex()` is generation-only (RFC-0054) — see the module
header. Errors read `line N, col M: <reason>`, positioned in the `.von`
source the caller passed.

## emitVon

```vyrn
fn emitVon(v: Von) -> String
```

One value as canonical VON text, with no header.

The writer recurses per level like the reader, and returns a `String` with no
room to refuse anything — so the bound is [`vonMaxDepth`], spent at the read
side: nothing `parseVon` hands back can be deep enough to reach the engine's
call cap here, which makes the read → write round trip whole. A value a
program built past that depth itself is the program's own bound to keep.

## toVon

```vyrn
fn toVon(doc: VonDoc) -> String
```

A whole document as canonical VON text: the header, a blank line, the value,
and a closing newline.

Comments do not survive a read/write round trip — `lex()` emits no token for
one — so `toVon` is for writing a document, not for rewriting one somebody
else wrote (RFC-0097 §8).

## jsonToVon

```vyrn
fn jsonToVon(json: Json, typeName: String, module: String) -> Result<String, String>
```

Convert a JSON document to VON text, headed by
`import type { <typeName> } from "<module>"`.

The result is a starting point, not an answer: JSON says nothing about types,
so every NESTED object arrives as a map and it is the author — who has the
type — who promotes one to a record. What the conversion does answer is
everything JSON got wrong on its own terms: a duplicate key is rejected by
`std/json`'s reader before this walk runs, `null` has no VON spelling, and a
number keeps its verbatim digits.
