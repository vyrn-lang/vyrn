# std/ui.editor

Editor support for the `.vyx` pages `std/ui` claims.

No program links this module, so a program that uses `std/ui` pays
nothing for it. A tool finds it beside `std/ui` by its name.

## vyrnSourceUi

```vyrn
fn vyrnSourceUi(path: String) -> String
```

The Vyrn a tool reads in the `.vyx` page at `path`: its `<script>` at the
file's own positions, then the `page` its `<template>` compiles to.
