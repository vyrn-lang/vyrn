# std/storage

## writeAtomic

```vyrn
fn writeAtomic(path: String, content: String) -> Result<Bool, String>
```

Writes `content` to `path` atomically, through a temp in the same directory.
A failed write leaves `path` untouched. A host effect, so generators and
comptime code cannot call it.

The temp is `<path>.tmp.<seed>` with a per-call seed, so concurrent writers
never share a temp; the last rename wins. There is no delete primitive, so a
failed rename leaves its temp behind: clean `<path>.tmp.*` out of band.
