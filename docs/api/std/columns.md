# std/columns

A record type laid out as one array per field.

  import { columns } from "std/columns"

  @columns
  export type Body = { x: Float64, mass: Float64 }

`@columns` on a record type `T` writes beside it, exported when `T` is:

- `type TColumns`: one `Array<F>` per field `f: F` of `T`, under the same
  name, with a `where` rule that every column has the first one's length.
  `c.f[i]` reads a field and `c.f[i] = v` writes one in place.
- `tColumns(capacity)`: empty columns with room for `capacity` rows.
- `pushT(c, v)`: appends the row `v` to `c` in place. The pushes are one
  group of stores, so the rule is checked once, after the last column
  grows, and the check is proved.
- `tAt(c, i)`: row `i` read back as a `T`.

`c.f.length` counts the rows. Because of the rule, a loop bounded by one
column's length indexes every column with no bounds check. A walk over one
field of many rows reads only that field's bytes, and a layout of one array
of records reads the whole row.

## columns

```vyrn
fn columns(t: TypeCode, iface: ModuleInterface) -> Code
```

Writes the column layout of the record type it annotates.
