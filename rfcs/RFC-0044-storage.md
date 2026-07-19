# RFC-0044 — `std/storage`: Crash-Safe Persistence

- **Status:** Draft (design locked)
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
