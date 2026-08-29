# RFC-0119 — a listing carries its kinds

- **Status:** Implemented (2026-08-29): `listDirKinds` in the interpreter's
  runtime and generation paths, the genwasm engine, and the direct backend's
  generator host; `std/vyx-hints`, `std/ui` and `std/rpc` converted, their
  three `isDir` helpers deleted.
- **Evidence:** the std-quality census's pattern 5 (redundant filesystem
  work), and its own correction note: four modules double-listed every
  subdirectory, the obvious repair was proven wrong in writing, and the
  conclusion was "an entry listing that carries a kind ... is a host
  capability, not a library change".

## The gap

`listDir` answers `Ok(names)` or `Err(text)`, and nothing else. A regular
file lists as an error and an unreadable directory lists as an error, and the
two are the same value. A recursive scanner must not skip an unreadable
directory silently — `std/vyx-hints`' own comment records why: a checker that
did would tell a project its tree is checked while it is not. So every
scanner grew the same helper — `vhIsDir`, `uiIsDir`, `rpcIsDir` — that lists
a subdirectory once just to test directory-ness, and then lists it again to
walk it. The census measured 601 `listDir` calls on a 300-entry tree.

Distinguishing the two failures from the error TEXT is not available either:
the project single-sources its canonical I/O error strings and refuses to
depend on operating-system wording, which is what telling `ENOTDIR` from
`EACCES` would require.

## The design: a trailing slash, not a record

`listDirKinds(path) -> Result<Array<String>, String>` — the same listing,
with a `/` appended to each directory entry's name. One convention instead
of a new record type, and unambiguous, because no entry NAME can contain a
slash. The caller's test is `name.endsWith("/")`; the walk strips the marker
it just read.

What the convention buys over `{ name, dir }` records: the builtin keeps
`listDir`'s exact type, so every seam it crosses — the prelude row, the
resolver trait, the generation cache's `\n`-joined recording, the direct
backend's blob-splitting lowering — carries it without a new shape. The
direct backend's lowering is literally the same function emitted against a
second host mode; the guest's splitter never learns kinds exist.

An entry whose type cannot be read is reported as a file: the caller's walk
then surfaces the real error at the entry itself, instead of this listing
guessing.

## What it replaces, measured by shape

- `std/vyx-hints`' `vhScan`: one `listDirKinds` per directory where the tree
  paid one `listDir` per directory PLUS one per subdirectory (`vhIsDir`,
  deleted). The census's 601 calls on a 300-entry tree become 301.
- `std/ui`'s `uiScanAll` / `uiReadDirOf`: same shape, `uiIsDir` deleted.
- `std/rpc`'s `rpcScan`: same shape, `rpcIsDir` deleted.
- `std/icons` was named in the same census pattern but its redundancy is a
  repeated `readFile`, which a kinded listing does not touch — out of scope.

## Mediation, recording, and the engines

At generation time the listing goes through the loader's resolver
(`ModuleResolver::list_kinds`, defaulting to the same refusal as `list`), is
path-scoped exactly as `listDir` is, and is recorded as a synthetic cache
input under the same directory key — the suffixed names are distinct bytes,
so a kind that changes invalidates the cache exactly as a name that changes
does. The genwasm engine serves it as mode 4 of the one mediated read; the
direct backend emits its `list_dir` runtime function twice, once per mode.
At runtime (`vyrn run`) the interpreter lists the real filesystem and asks
each entry its type. Neither compiling backend lowers it, in `listDir`'s own
words, with the name the user wrote.
