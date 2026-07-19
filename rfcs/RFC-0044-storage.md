# RFC-0044 — `std/storage`: Crash-Safe Persistence

- **Status:** Implemented (see "As landed" below)
- **Depends on:** RFC-0014 (input I/O — `readFile`/`writeFile`, the
  host-boundary model; canonical `@.io.*` error wording), RFC-0018 (the
  JSON codec — `toJson`/`fromJson` the load/save convenience wraps),
  RFC-0043 (the extern-shim pattern for a new host primitive)
- **Evidence (bin dogfood, NOTES):** the store persists via
  `writeFile(path, toJson(store))` — a truncate-then-write with **no
  atomic rename and no fsync**, so a crash mid-write leaves a truncated,
  corrupt file. bin's loader degrades gracefully (`fromJson` `Invalid`
  → empty store + a warning), but "graceful" there means **all data is
  lost**. A real store must never be able to tear.

---

## The problem, precisely

`writeFile` opens, truncates, and streams bytes. A crash (or `Ctrl-C`,
or OOM, or power loss) anywhere in the stream leaves the file
half-written — the old contents gone, the new contents incomplete. For a
whole-file JSON store rewritten on every mutation, that is a
data-corruption window on every save.

The fix is the standard one: **write to a temp file, then atomically
rename it over the target.** Rename is atomic on every real filesystem
(POSIX `rename(2)`, Windows `MoveFileEx`/`ReplaceFile`, WASI
`path_rename`), so a reader/next-boot sees either the complete old file
or the complete new one — never a torn one.

## 1. The host primitive: `renameFile`

`std/storage` needs one new host extern (RFC-0014 has read/write but no
rename), routed to the C shim like RFC-0043's clock:

- **`renameFile(from: String, to: String) -> Result<Unit, IoError>`** —
  native `rename`/`MoveFileExW(REPLACE_EXISTING)`, wasi `path_rename`,
  browser: the wasi-min.js shim over its in-memory/OPFS fs. Overwrites
  `to` atomically where the OS guarantees it (documented per target).
  Errors reuse the canonical `@.io.*` wording (not-found, permission);
  a cross-device rename (`EXDEV`) surfaces as a named `IoError` rather
  than silently falling back to copy (v1 keeps temp and target in the
  same dir, so it can't happen for `writeAtomic`).

## 2. `writeAtomic` — the primitive

```vyrn
import { writeAtomic } from "std/storage"

writeAtomic("data/pastes.json", body)   // temp-then-rename; never tears
```

- **`writeAtomic(path, content) -> Result<Unit, IoError>`**: writes
  `content` to a sibling temp (`<path>.tmp` — same directory, so the
  rename stays intra-device), then `renameFile`s it over `path`. A
  failure at the write step leaves `path` untouched (old data intact);
  the temp is best-effort cleaned. Byte-for-byte, a successful
  `writeAtomic` leaves `path` exactly as `writeFile` would — the
  difference is only the crash window, which is now closed.
- **Durability caveat, stated honestly:** v1 is **crash-consistent**
  (no torn files) but not **power-loss-durable** — without `fsync` the
  rename may not have hit disk when the power dies. `fsyncFile(path)`
  (an optional second host extern) is provided for the paranoid path
  (`fsync` temp → rename → `fsync` dir), documented as the durability
  upgrade; `writeAtomic` alone is the crash-consistent default because
  fsync-on-every-save is a real latency cost most apps don't want.

## 3. The typed load/save convenience (pure, over the codec)

The store pattern bin hand-rolls becomes three helpers — pure Vyrn over
`writeAtomic` + the codec, no new host surface:

```vyrn
import { save, load, loadOr } from "std/storage"

save("data/pastes.json", store)                       // toJson + writeAtomic
let s = loadOr(StoreFile, "data/pastes.json", empty()) // missing/corrupt → default
```

- **`save<T>(path, value: T) -> Result<Unit, IoError>`** =
  `writeAtomic(path, toJson(value))`. (Codable `T` only — the checker's
  existing codability rule; a non-codable `T` is the same named error
  `toJson` gives.)
- **`load<T>(TypeName, path) -> LoadResult<T>`** where `LoadResult` is
  `| Missing | Corrupt(Array<Issue>) | Loaded(T)` — the three honest
  outcomes distinguished (a missing file is NOT the same as a corrupt
  one; bin currently conflates them). `readFile` not-found → `Missing`;
  `fromJson` `Invalid` → `Corrupt(issues)`; else `Loaded`.
- **`loadOr<T>(TypeName, path, default: T) -> T`** — the store-init
  shorthand: `Missing`/`Corrupt` → `default` (the RFC-0029 module-state
  initializer pattern, one call). A store that wants to *log* corruption
  before defaulting uses `load` and matches.

Generic over `T` follows the codec's existing type-name-directed shape
(`load`/`save` take the type name for `fromJson`, exactly as `fromJson`
does today) — no new generics machinery.

## 4. bin adoption (the proof)

- The store's persist path → `save(path, store)`; init → `loadOr(...)`
  (or `load` + a logged `Corrupt` warning, keeping bin's current
  warning). The hand-rolled `writeFile`/`readFile`+`fromJson` dance and
  its manual missing/corrupt handling collapse to these calls.
- **Verification of atomicity** (a crash is hard to script, so prove the
  mechanics): a `writeAtomic` over an existing file leaves no `.tmp`
  behind and the target is the new content; a `renameFile` unit test;
  and — the real proof — inject a write failure (a `writeAtomic` to a
  path whose temp write fails, e.g. a bad dir) and assert the **original
  file is byte-unchanged** (the tear that `writeFile` would cause is
  gone). Restart-survival + the timestamp from RFC-0043 still hold.

## Effects / comptime

`renameFile`/`fsyncFile` are host effects (like `writeFile`) — forbidden
in `gen`/comptime and in spawned tasks (consistent with RFC-0043's
correction that tasks do no host I/O). `save`/`load`/`loadOr` inherit the
effect from `writeAtomic`/`readFile`. Pin it.

## Out of scope

Directory creation (`writeFile` still doesn't `mkdir` — bin's `data/`
must exist; a `mkdirp` is a separate small addition, noted), file
locking / multi-process concurrency, a key-value or embedded-DB
abstraction (this is file-granular persistence — a real DB is a much
later, likely-native story), append/streaming writes, `fsync` as the
default, WAL/journaling, backup rotation.

---

## As landed

**`IoError` shape reused (no parallel type).** The whole family speaks the exact
shape `readFile`/`writeFile` already return: `Result<Bool, String>` — `Ok(true)`
on success, `Err(<canonical String>)` on failure. There is no distinct `IoError`
struct; "IoError" in the prose is the `String` error arm, and "Unit" is `Bool`
(mirroring `writeFile: Result<Bool, String>`). Error wording is single-sourced in
codegen's `@.io.*` format globals — `renameFile`'s common not-found/permission
failure reuses `@.io.writeerr` (`cannot write \`<to>\``, rewriting the
destination); `fsyncFile` reuses it too; the cross-device (`EXDEV`) case gets one
new global `@.io.xdeverr` (`cannot rename \`<to>\` across devices`) rather than
silently degrading to a copy. `writeAtomic` keeps its temp beside the target, so
it never hits `EXDEV`.

**`renameFile` / `fsyncFile` are I/O BUILTINS, not `extern fn`s.** The RFC calls
them "host externs", but the RFC-0043 `extern fn` path is scalar-ABI only
(`hostNowMillis() -> Int64`) and these take `String`s and return a `Result`. So
they landed as siblings of `writeFile` — global builtins recognized by name in
the checker/interp/codegen, lowering to plain C-shim symbols
(`__vyrn_rename_file` / `__vyrn_fsync_file`) with NO `vyrn`-namespace import and
NO native trap stub, exactly the property the RFC-0043 clock has. The shim
implements them on every target, so wasi lowers `rename`→`path_rename` and
`fsync`→`fd_sync` and a storage program is a full three-way parity citizen. The
interpreter special-cases the same two names (`std::fs::rename`, open-`write(true)`
+ `sync_all`). They are in `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN`, so — like
`writeFile` — they are rejected in a `spawn`ed task and in `gen`/comptime (pinned:
`rfc0044_rename_and_fsync_are_rejected_in_a_generator`,
`rfc0044_rename_and_fsync_are_effects_not_tasks`).

**Windows `MoveFileEx` subtlety.** The C `rename` on Windows FAILS when the target
exists (it is not POSIX-replace), so `__vyrn_rename_file` uses
`MoveFileExA(from, to, MOVEFILE_REPLACE_EXISTING)` there — the atomic overwrite —
and maps `GetLastError() == ERROR_NOT_SAME_DEVICE` to the cross-device status. To
avoid leaking `<windows.h>`'s `min`/`max` macros into the JSON codec compiled in
the same shim, the two Win32 symbols are declared inline and pulled from
`kernel32` via `#pragma comment(lib, ...)`. `fsyncFile` opens the file `"rb+"`
(read+write, no truncate) because `FlushFileBuffers`/`_commit` needs write access
— a read-only handle fails on Windows. POSIX/wasi take the plain `rename` +
`errno == EXDEV` path.

**`save` / `load` / `loadOr` are call-site desugars, not module exports (the codec
wall).** These cannot be ordinary generic Vyrn functions: the codec is
type-name-directed — `toJson(value)` requires an *encodable* concrete type and
`fromJson(TypeName, s)` a *declared type name*, and a generic body is checked once
with `T` abstract (verified: `toJson(v: T)` fails "cannot encode `T`"). So, like
the `toJson`/`fromJson` builtins they wrap, they are expanded in the parser where
the arguments are concrete, into plain AST every backend already handles — with
ZERO interp/codegen/checker special-casing for them:

- `save(path, value)` → `writeAtomic(path, toJson(value))`. So a module using
  `save` must `import { writeAtomic } from "std/storage"` (the storage primitive);
  the read helpers are self-contained (`readFile` is a global builtin).
- `load(TypeName, path)` → `match readFile(path) { Ok(t) => match fromJson(TypeName,
  t) { Valid(v) => Loaded(v), Invalid(i) => Corrupt(i) }, Err(e) => Missing }`.
- `loadOr(TypeName, path, d)` → the same, with `Valid(v) => v`, `Invalid`/`Err => d`.

Consequence/deviation from the RFC's `import { save, load, loadOr } from
"std/storage"` prose: they are GLOBAL forms (unimported, like `toJson`/`fromJson`/
`readFile`), reserved names, expanded on exact arity. `std/storage.vyrn` exports
the one thing that IS a real importable Vyrn function — `writeAtomic` (a pure
`writeFile`-temp + `renameFile` over the global builtins). Binding names in the
desugar are `@`-prefixed so they can never collide with a user identifier. Because
`load` inherits `readFile` (mediated/permitted in a generator) and `save` inherits
`writeFile` (forbidden), their effect discipline falls out for free.

**`LoadResult<T>` is a prelude enum.** `type LoadResult<T> = Missing |
Corrupt(Array<Issue>) | Loaded(T)` is injected by the parser alongside
`Validation`/`Issue`, so `load`'s desugar names its variants and any caller
matches all three outcomes without an import (pinned:
`rfc0044_load_result_prelude_enum_is_matchable`). A missing file is deliberately
NOT a corrupt one. One honesty note: `readFile` conflates every I/O failure into
one `Err`, so `Missing` means "the file could not be read" (absent in the common
case), not strictly ENOENT.

**Temp cleanup is best-effort = not-yet (no `removeFile`).** On a *write* failure
`writeAtomic` returns `Err` with `path` untouched and no temp created; on the rare
*rename* failure after a successful temp write, the `.tmp` is left behind — Vyrn
has no `removeFile` primitive to clean it, and adding one was out of scope. The
load-bearing invariant (target never torn) holds regardless; a successful
`writeAtomic` leaves no `.tmp`.

**The atomicity proof (Rust test).** `rfc0044_write_atomic_failed_temp_leaves_
target_unchanged` writes `ORIGINAL` to the target, then forces the temp write to
fail (by making `<path>.tmp` a *directory* so `writeFile` cannot open it), runs
the `writeAtomic` algorithm, and asserts the target still reads `ORIGINAL` — the
tear a bare `writeFile` would cause is gone. Companions:
`rfc0044_rename_file_over_existing_replaces` (atomic overwrite, source consumed),
`rfc0044_write_atomic_replaces_and_leaves_no_tmp`, `rfc0044_load_three_outcomes`
(Missing/Corrupt/Loaded), `rfc0044_load_or_defaults`,
`rfc0044_fsync_file_ok_and_missing`.

**Parity citizen.** `examples/storage.vyrn` (`save`/`load`/`loadOr`/`writeAtomic`
over a temp `data/*.store.json`, gitignored) is byte-identical interp == native ==
wasm with the wasm column active (`ok storage.vyrn`; 70 checked, 0 failed) — the
`rename`/`path_rename` and codec paths agree across all three. `web/wasi-min.js`
gained `path_rename`/`fd_sync` import stubs so a browser module using storage still
instantiates and degrades gracefully (no filesystem → canonical `Err`, never a
link failure).

**bin adoption.** `examples/bin/persist.vyrn` collapsed: `saveStore` is now
`writeAtomic(path, toJson(snapshot))` (imported from `std/storage`) — crash-safe;
`loadStore` is `match load(StoreFile, path) { Loaded(s) => s, Corrupt(_) =>
warnEmpty(), Missing => emptyStore() }`, keeping bin's corrupt-file warning while
distinguishing it from an absent file. `store.vyrn`'s API (and the RFC-0043
real-timestamp behavior) is untouched. Test/parity counts: 908 → 918 workspace
tests, 17 LSP tests, full three-way parity green.
