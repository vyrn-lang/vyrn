# std/storage

std/storage — crash-safe persistence (RFC-0044).

`writeFile` truncates then streams, so a crash mid-write leaves the file torn
— the old contents gone, the new ones incomplete. For a whole-file JSON store
rewritten on every mutation that is a corruption window on every save. The fix
is the standard one: write a sibling temp file, then atomically RENAME it over
the target (`renameFile`, a host primitive — POSIX `rename`, Windows
`MoveFileEx(REPLACE_EXISTING)`, wasi `path_rename`). A reader/next-boot then
sees either the complete old file or the complete new one, never a torn one.

`writeAtomic` is that primitive over a `String`. The typed convenience helpers
`save` / `load` / `loadOr` (over the JSON codec) are GLOBAL forms — like the
`toJson` / `fromJson` / `readFile` builtins they wrap, they are not module
exports (the codec is type-name-directed, so they are expanded at the call
site where the type is concrete). `save(path, value)` expands to
`writeAtomic(path, toJson(value))`, so a module using `save` imports
`writeAtomic` from here; `load(TypeName, path)` / `loadOr(...)` are
self-contained (they wrap the global `readFile`) and yield the prelude
`LoadResult<T> = Missing | Corrupt(Array<Issue>) | Loaded(T)`.

**Durability:** `writeAtomic` is CRASH-CONSISTENT (no torn files) but not
power-loss-durable — without an `fsync` the rename may not have reached the
disk when the power dies. `fsyncFile(path)` (a global builtin) is the opt-in
upgrade for the paranoid path (fsync temp → rename → fsync dir); it is left
out of `writeAtomic` because fsync-on-every-save is a latency cost most apps
don't want.

## writeAtomic

```vyrn
fn writeAtomic(path: String, content: String) -> Result<Bool, String>
```

Write `content` to `path` atomically: stream it to a sibling temp
(`<path>.tmp`, same directory so the rename stays intra-device), then rename
the temp over `path`. A failure at the write step leaves `path` UNTOUCHED (the
old data is intact — the tear a bare `writeFile` would cause is gone).
Byte-for-byte a successful `writeAtomic` leaves `path` exactly as `writeFile`
would; only the crash window differs. HOST EFFECT (forbidden in
generators/comptime, cannot cross a `spawn` boundary).
