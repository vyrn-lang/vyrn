# std/html

std/html — the view tree and the string renderer (RFC-0026 M1).

A UI in Vyrn is a pure function returning an `Html` tree (The Elm
Architecture — no stored closures, no runtime reactivity, a host that owns
the loop). This module is plain, pure Vyrn: it compiles the same under the
interpreter, the native binary, and wasm, and every view built with it is a
three-way parity citizen. The compiler knows nothing about UI — `Html` is an
ordinary self-recursive payload enum (RFC-0024's recursion-safe per-type
codec carries it across the wire unchanged), and everything below is
constructors + string building.

  import { el, text, cls, attr, on, keyed, empty, toHtmlString, document } from "std/html"

Two consumers share this one tree:
  - the SERVER renders `toHtmlString(view())` (SSR);
  - the CLIENT ships `toJson(view())` to `web/vyrn-dom.js`, which builds and
    diffs the real DOM. Both are pure, so components are snapshot-testable
    in `vyrn test` via `assertEq(toHtmlString(..), "..")`.

This module is comptime-pure (plain string building over `bytes`/
`stringFromBytes`), so it is equally usable from a `gen fn` and its callees.

## Attr

```vyrn
type Attr = Cls(String) | Id(String) | A(String, String) | On(String, String, String) | Key(String)
```

A single attribute on an element.

  - `Cls(v)` — `class="v"`.
  - `Id(v)`  — `id="v"`.
  - `A(n, v)` — any attribute: `A("href", "/items")` → `href="/items"`.
  - `On(event, handler, payload)` — a named event binding. Renders as two
    attributes so neither name nor payload needs in-attribute delimiters:
    `On("click", "removeItem", "42")` → `data-on-click="removeItem"
    data-arg-click="42"`. `web/vyrn-dom.js` walks to the nearest
    `data-on-<event>` and invokes the root-exported `export extern fn` of
    that name (RFC-0026 M2's handler ABI).
  - `Key(k)` — list identity for the M2 keyed differ. Renders as
    `data-key="k"` so a server-rendered list carries the same identity the
    client tree does.

## Html

```vyrn
type Html = Empty | Text(String) | Raw(String) | El(String, Array<Attr>, Array<Html>)
```

A node in the view tree.

  - `Empty` — renders as nothing; the unit of conditionals (`{#if}` with no
    else lowers to `Empty`).
  - `Text(s)` — ALWAYS escaped (`& < > "`).
  - `Raw(s)` — NOT escaped: the loud, greppable escape hatch, the only way
    to emit markup from a string.
  - `El(tag, attrs, kids)` — an element. `Html` is self-recursive through
    `Array<Html>` here; the codec handles the recursion.

## Sub

```vyrn
type Sub = Every(Int64, String) | Keydown(String, String)
```

A host subscription — effects as data, the Elm answer to timers and global
key listeners. An app optionally exports `vyrnSubs() -> String` returning
`toJson(subs())`; after each render `web/vyrn-dom.js` diffs the declared
list by value and wires what appeared / unwires what disappeared, so there
is nothing to leak.

  - `Every(ms, handler)` — call the root-exported `handler` every `ms`
    milliseconds.
  - `Keydown(key, handler)` — call `handler` when `key` (a `KeyboardEvent.key`
    value, e.g. `"Enter"`, `"Escape"`) is pressed anywhere on the document.

`Sub` lives here, alongside `Html`, on purpose: it is codable library data a
client app hands straight to the host runtime, and keeping it in `std/html`
means the whole UI-runtime surface (`Html` + `Sub`) is one import. The
vocabulary is deliberately tiny — it grows by demand, and third parties can
define their own `Sub`-like types with their own runtime.

## el

```vyrn
fn el(tag: String, attrs: Array<Attr>, kids: Array<Html>) -> Html
```

An element: `el("li", [cls("row")], [text("hi")])`.

## text

```vyrn
fn text(s: String) -> Html
```

An escaped text node.

## empty

```vyrn
fn empty() -> Html
```

The empty node (renders as nothing).

## cls

```vyrn
fn cls(s: String) -> Attr
```

A `class="…"` attribute.

## attr

```vyrn
fn attr(n: String, v: String) -> Attr
```

An arbitrary `name="value"` attribute.

## on

```vyrn
fn on(event: String, handler: String, payload: String) -> Attr
```

An event binding: `on("click", "removeItem", "42")`.

## keyed

```vyrn
fn keyed(k: String, node: Html) -> Html
```

Attach a list-identity `Key` to an element for the M2 keyed differ. Keying a
non-element node is a no-op (the differ only reorders element children), so
this is total.

## toHtmlString

```vyrn
fn toHtmlString(h: Html) -> String
```

Render a view tree to an HTML string (SSR). Text is escaped, attribute
values are escaped, `Raw` bypasses, void elements self-close. Total: any
`Html` value renders to a string, never a trap.

## PatchOp

```vyrn
type PatchOp = OpSetText(Array<Int64>, String) | OpSetAttrs(Array<Int64>, Array<Attr>) | OpReplace(Array<Int64>, Html) | OpInsert(Array<Int64>, Int64, Html) | OpRemove(Array<Int64>, Int64) | OpMove(Array<Int64>, Int64, Int64)
```

A minimal edit to the live DOM, produced by `diff` and applied strictly in
order by `web/vyrn-dom.js`. Moving the diff into wasm means only the CHANGES
cross the extern boundary each event (O(changes)), not the whole view tree
(O(tree)).

A `path` is a child-index vector from the mount root: `[]` is the root node,
`[2]` its third child, `[2, 0]` that child's first child. Each vnode maps to
exactly one DOM node (`Empty`→comment, `Text`→text, `Raw`→wrapper,
`El`→element), so a vnode child index equals the live DOM `childNodes`
index — the host resolves a path by walking `childNodes` naively.

  - `OpSetText(path, s)` — set the text of the node AT `path` to `s`.
  - `OpSetAttrs(path, attrs)` — replace the attribute set of the element AT
    `path` wholesale; the host reconciles attribute-by-attribute against the
    live element (event attrs, `data-key`, form `value`/`checked` as
    properties) exactly as the full-view loop does.
  - `OpReplace(path, html)` — build `html` and replace the node AT `path`
    (a tag change, a `Raw` string change, or a kind change).
  - `OpInsert(parent, index, html)` — build `html` and insert it as the
    `index`-th child of the element AT `parent`.
  - `OpRemove(parent, index)` — remove the `index`-th child of `parent`.
  - `OpMove(parent, from, to)` — move `parent`'s child from index `from` to
    index `to` (reused DOM node — focus/typed-value/JS-property survive).

## Emission-order convention (locked)

`diff` emits ops so that each op's path/index is valid against the DOM AS
ALREADY PATCHED by every preceding op — the host applies them naively in
order and lands on the correct tree. The discipline, per level:

  - node level: a kind/tag change is a single `OpReplace` (no recursion into
    it); otherwise `OpSetText`/`OpSetAttrs` for this node come BEFORE its
    children's ops.
  - positional children: recurse the common prefix first (those indices are
    stable), then `OpRemove` the surplus tail HIGH-TO-LOW (so lower indices
    stay valid), then `OpInsert` the new tail LOW-TO-HIGH.
  - keyed children (mirrors `web/vyrn-dom.js`'s `patchKeyed`): first
    `OpRemove` every dropped key HIGH-TO-LOW; then, scanning the new order
    LEFT-TO-RIGHT, `OpInsert` each new key and `OpMove` each reused key into
    place (reused nodes only ever move leftward, so a single `insertBefore`
    realizes each move); finally recurse into the surviving matched children
    at their FINAL indices. Structure is fully settled before any content op
    for that level's children, so every content path is the node's final
    position.

`PatchOp` is an ordinary payload enum (RFC-0024), so `toJson(diff(a, b))` is
the wire form the host parses, and `diff` is a pure, total, three-way parity
citizen like the rest of `std/html`.

## diff

```vyrn
fn diff(old: Html, new: Html) -> Array<PatchOp>
```

Diff two view trees into a minimal, ordered `PatchOp` stream. Pure and total:
any two `Html` values diff to a stream whose in-order application turns a DOM
built from `old` into one identical to a DOM built from `new`.

## document

```vyrn
fn document(title: String, head: Array<Html>, body: Html) -> String
```

Wrap a view in a full `<!doctype html>` page. `title` is escaped as text;
`head` is a list of head nodes (meta/link/script via `el`/`Raw`); `body` is
the page body tree. Used by SSR and by M3's route dispatcher.
