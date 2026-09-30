# std/columns

std/columns: a record type laid out as one array per field.

  import { columns } from "std/columns"
  import { BodyColumns, bodyColumns, pushBody, bodyAt } from columns("./bodies")

`columns` is a `gen fn` that reflects a module and writes, for each record
type `T` it exports:

- `type TColumns`: one `Array<F>` per field `f: F` of `T`, under the same
  name, with a `where` rule that every column has the first one's length.
  `c.f[i]` reads a field and `c.f[i] = v` writes one in place.
- `tColumns(capacity)`: empty columns with room for `capacity` rows.
- `pushT(c, v)`: `c` with the row `v` appended, as `c = pushT(c, v)`.
- `tAt(c, i)`: row `i` read back as a `T`.

`c.f.length` counts the rows. Because of the rule, a loop bounded by one
column's length indexes every column with no bounds check. A walk over one
field of many rows reads only that field's bytes, and a layout of one array
of records reads the whole row.

## columns

```vyrn
fn columns(module: String) -> String
```

Emits a module exporting the column layout of every record type `module`
exports.
