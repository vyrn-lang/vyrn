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
