# std/storage

## writeAtomic

```vyrn
fn writeAtomic(path: String, content: String) -> Result<Bool, String>
```

Write `content` to `path` atomically: stream it to a sibling temp (same
directory, so the rename stays intra-device), then rename the temp over
`path`. A failure at the write step leaves `path` UNTOUCHED (the old data is
intact — the tear a bare `writeFile` would cause is gone). Byte-for-byte a
successful `writeAtomic` leaves `path` exactly as `writeFile` would; only
the crash window differs. HOST EFFECT (forbidden in generators/comptime,
cannot cross a `spawn` boundary).

**Concurrency:** the temp carries a per-call seed — `<path>.tmp.<seed>` — so
two processes saving the same target write DIFFERENT temps and each rename
publishes one complete file. A fixed `<path>.tmp` would let concurrent
writers interleave into one torn temp and rename that torn file into place.
Last writer to rename wins; nothing interleaves.

**Leaked temps:** RFC-0044 offers no delete primitive, so a failed RENAME
abandons its own uniquely-named temp on disk. It can never be mistaken for
real data — it is never `<path>` itself, and it is never reused — but a
program that fails saves repeatedly accumulates `<path>.tmp.*` siblings;
clean them out of band.
