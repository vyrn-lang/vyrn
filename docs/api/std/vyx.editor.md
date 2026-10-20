# std/vyx.editor

Editor support for the `.vyx` files `std/vyx` claims.

No program links this module, so a program that uses `std/vyx` pays
nothing for it. A tool finds it beside `std/vyx` by its name.

## vyrnCompleteVyx

```vyrn
fn vyrnCompleteVyx(root: String, path: String, line: Int64, col: Int64) -> String
```

Completion items at 1-based `line` and `col` of the `.vyx` at `path`: a
tag name offers the capitalized sibling components and, for a lowercase
prefix, element names; an attribute name offers a component's props or an
element's attributes and the directives; `@` offers the DOM events. `root`
is the directory the generator was pointed at and `path` the file, both
spelled from the importer. Returns a JSON array of `{label, kind, detail,
from}`, empty where the editor's forward map answers instead.

## vyxScriptView

```vyrn
fn vyxScriptView(src: String) -> String
```

`src` with every character outside its `<script>` body replaced by a
space and each LF kept: the Vyrn a tool reads in a `.vyx`, at the file's
own lines and columns. A section never closed keeps nothing.

## vyxHasTemplate

```vyrn
fn vyxHasTemplate(src: String) -> Bool
```

Whether `src` has a `<template>` section outside its `<script>`, closed or
not.

## vyxStemOf

```vyrn
fn vyxStemOf(path: String) -> String
```

The file name of `path` without its `.vyx` extension.

## vyrnSourceVyx

```vyrn
fn vyrnSourceVyx(path: String) -> String
```

The Vyrn a tool reads in the component at `path`: its `<script>` at the
file's own positions, then the view function its `<template>` compiles to.
