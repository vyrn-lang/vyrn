# std/html

The view tree and its string renderer. A view is a pure function
returning `Html`; the server renders it with `toHtmlString`, and the client
ships `toJson(view())` or a `diff` to `web/vyrn-dom.js`, which builds and
patches the DOM.

  import { el, text, cls, attr, on, keyed, empty, toHtmlString, document } from "std/html"

## Attr

```vyrn
type Attr = Cls(String) | Id(String) | A(String, String) | On(String, String, String) | Key(String)
```

An attribute on an element. `On(event, handler, payload)` renders as
`data-on-<event>="handler" data-arg-<event>="payload"`; `web/vyrn-dom.js`
calls the root-exported `export extern fn` named `handler`. `Key(k)` is list
identity for the keyed differ and renders as `data-key="k"`.

## Html

```vyrn
type Html = Empty | Text(String) | Raw(String) | El(String, Array<Attr>, Array<Html>)
```

A node in the view tree. `Text` is always escaped; `Raw` is never escaped
and is the only way to emit markup from a string. `Empty` renders as nothing.

## Sub

```vyrn
type Sub = Every(Int64, String) | Keydown(String, String)
```

A host subscription. An app may export `vyrnSubs() -> String` returning
`toJson(subs())`; after each render `web/vyrn-dom.js` diffs the list by
value and wires what appeared and unwires what went. `Every(ms, handler)`
calls `handler` every `ms` milliseconds; `Keydown(key, handler)` calls it
when `key` (a `KeyboardEvent.key` value) is pressed on the document.

## copyHtmlArray

```vyrn
fn copyHtmlArray(ks: Array<Html>) -> Array<Html>
```

Exported because `ks.copy()` cannot be written for this element type: a
declared `Copy` answers for the whole value, not a part of one.

## el

```vyrn
fn el(tag: consume String, attrs: consume Array<Attr>, kids: consume Array<Html>) -> Html
```

## text

```vyrn
fn text(s: consume String) -> Html
```

## empty

```vyrn
fn empty() -> Html
```

## cls

```vyrn
fn cls(s: consume String) -> Attr
```

## attr

```vyrn
fn attr(n: consume String, v: consume String) -> Attr
```

## on

```vyrn
fn on(event: consume String, handler: consume String, payload: consume String) -> Attr
```

## keyed

```vyrn
fn keyed(k: consume String, node: consume Html) -> Html
```

Keying a non-element node returns it unchanged.

## toHtmlString

```vyrn
fn toHtmlString(h: Html) -> String
```

Renders a view tree to HTML. Text and attribute values are escaped; `Raw`
is not.

# Panics

On a tag, attribute or event name outside `[A-Za-z0-9_:-]`, or children on
a void element.

## PatchOp

```vyrn
type PatchOp = OpSetText(Array<Int64>, String) | OpSetAttrs(Array<Int64>, Array<Attr>) | OpReplace(Array<Int64>, Html) | OpInsert(Array<Int64>, Int64, Html) | OpRemove(Array<Int64>, Int64) | OpMove(Array<Int64>, Int64, Int64)
```

A DOM edit produced by `diff`, applied in order by `web/vyrn-dom.js`; the
wire form is `toJson(diff(a, b))`. A path is a child-index vector from the
mount root (`[]` the root, `[2, 0]` the first child of its third child).
Each vnode maps to one DOM node, so a vnode index is a `childNodes` index.

Each op's path is valid against the DOM as patched by every earlier op:
  - A kind or tag change is one `OpReplace`; otherwise a node's own ops come
    before its children's.
  - Positional children: diff the common prefix, `OpRemove` the surplus
    high to low, then `OpInsert` the new tail low to high.
  - Keyed children (as `patchKeyed` in `web/vyrn-dom.js`): `OpRemove` dropped
    keys high to low; then, left to right, `OpInsert` new keys and `OpMove`
    reused ones (always leftward); then diff matched children at their final
    indices.

## diff

```vyrn
fn diff(old: Html, new: Html) -> Array<PatchOp>
```

Applying the ops in order turns a DOM built from `old` into one built from `new`.

## document

```vyrn
fn document(title: String, head: Array<Html>, body: Html) -> String
```

A full `<!doctype html>` page. `title` is escaped.
