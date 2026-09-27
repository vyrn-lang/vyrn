# std/von

std/von: VON, Vyrn Object Notation, Vyrn's literal grammar saved to a file.

  import { parseVon, toVon, Von, VonDoc } from "std/von"

A `.von` document is one or more `import type { T } from "..."` lines and
one literal value, with no operators, calls, bindings or interpolation.
`parseVon` reads it with the compiler's `lex()`, which only a `gen fn` can
call, so a config error is a build error. The strictness rules:
a bare word is a variant name, never a boolean or a
string; duplicate fields and map keys are errors naming both lines; numbers
keep their source text and refuse a leading zero; `\{` in a string, a tab in
indentation and a byte-order mark are errors. Errors read
`line N, col M: <reason>`.

## VonField

```vyrn
type VonField = { name: String, value: Von, line: Int64 }
```

One `name: value` field of a record literal. `line` is the field name's
1-based source line, which duplicate-field errors and origin maps use.

## VonEntry

```vyrn
type VonEntry = { key: String, value: Von, line: Int64 }
```

One `"key": value` entry of a map literal.

## Von

```vyrn
type Von = VRecord(String, Array<VonField>) | VVariant(String, Array<Von>) | VArray(Array<Von>) | VMap(Array<VonEntry>) | VStr(String) | VInt(String) | VFloat(String) | VBool(Bool)
```

A VON value. Numbers hold their raw, validated source text, so
`18446744073709551615` survives and emit -> parse -> emit is byte-stable.

## VonImport

```vyrn
type VonImport = { names: Array<String>, module: String }
```

One `import type { A, B } from "spec"` line of a document header.

## VonDoc

```vyrn
type VonDoc = { imports: Array<VonImport>, value: Von }
```

## copyVonArray

```vyrn
fn copyVonArray(xs: Array<Von>) -> Array<Von>
```

`copy` over a list of values; `xs.copy()` is refused for this element type.

## copyVonFields

```vyrn
fn copyVonFields(fs: Array<VonField>) -> Array<VonField>
```

`copy` over a list of record fields, for the same reason.

## copyVonEntries

```vyrn
fn copyVonEntries(es: Array<VonEntry>) -> Array<VonEntry>
```

`copy` over a list of map entries, for the same reason.

## VonTok

```vyrn
type VonTok = { kind: String, text: String, line: Int64, col: Int64 }
```

One lexed token. `lex()`'s `Token` can be named only inside a `gen fn`,
so `vonLex` copies each row into this type and only `vonLex` needs a
generation context.

## parseVon

```vyrn
fn parseVon(src: String) -> Result<VonDoc, String>
```

Reads a whole VON document. A `gen fn` because `lex()` is generation-only.
Errors are positioned in `src`.

## emitVon

```vyrn
fn emitVon(v: Von) -> String
```

One value as canonical VON text, with no header. It recurses per level;
`parseVon` bounds depth, so only a tree a program builds deeper can reach
the call cap.

## toVon

```vyrn
fn toVon(doc: VonDoc) -> String
```

A whole document as canonical VON text: the header, a blank line, the
value, and a closing newline. Comments do not survive a read and write, so
use it to write a document, not to rewrite one.

## jsonToVon

```vyrn
fn jsonToVon(json: Json, typeName: String, module: String) -> Result<String, String>
```

Converts a JSON tree to VON text, headed by
`import type { <typeName> } from "<module>"`. Every nested object becomes a
map for the author to promote; `null` has no VON spelling and is refused.
