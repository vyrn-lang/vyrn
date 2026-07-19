# RFC-0046 — `std/strings`: The String Library (+ a `slice` builtin)

- **Status:** Implemented (as-landed notes at the end)
- **Depends on:** RFC-0014 (bytes/String — `bytes()`/`stringFromBytes`,
  the UTF-8 invariant), RFC-0007 (string templates), RFC-0045 (bitwise —
  used inside some helpers), RFC-0027/0031 (the name-resolution machinery
  the privacy fix touches)
- **Evidence (vlog dogfood, NOTES-dogfood-vlog — and repeatedly before):**
  `std/strings` is only `repeat`/`joinWith`, so every text tool hand-rolls
  `slice`/`split`/`trim`/case/`indexOf` (vlog wrote six; bin and shelf
  wrote their own `util`). The deepest cut: **a byte has no
  one-character String** — `s[i]` is a `UInt8` and `"\{s[i]}"` prints
  `"97"`, so building any substring round-trips `bytes()` → mutate
  `Array<UInt8>` → `stringFromBytes` → `match Ok/Err` with a UTF-8
  revalidation that can never fail. That is both boilerplate and O(n)
  per slice.

---

## 1. The one language hook: a `slice` builtin

`slice(s: String, start: Int64, end: Int64) -> String` — a **byte-range**
substring, the primitive every other op builds on:

- **O(1) validated, not revalidated.** A substring of valid UTF-8 is
  valid iff both cut points sit on codepoint boundaries — an O(1) check
  (the bytes at `start` and `end` must not be UTF-8 continuation bytes
  `0x80..=0xBF`). The builtin checks exactly those two bytes and skips the
  whole-slice revalidation the hand-rolled version pays. This is why it is
  a builtin and not pure Vyrn: pure Vyrn has no way to construct a String
  from bytes *without* `stringFromBytes`' full O(n) revalidation.
- **Traps on a mid-codepoint boundary** (`start`/`end` inside a multi-byte
  char) — the canonical trap protocol (`error: slice splits a UTF-8
  character`, exit 1, byte-identical three-way), consistent with the
  array-OOB and shift-range traps. Out-of-range indices (`start<0`,
  `end>len`, `start>end`) trap as `error: slice index out of range`
  (mirrors array OOB). ASCII offsets (the common case, from `indexOf` on
  ASCII needles) never trap.
- That is the ONLY compiler surface this RFC adds. Everything in §2 is
  pure Vyrn built on `slice` + `bytes()`.

## 2. `std/strings` (pure library on `slice`)

The ops every text tool needs, ASCII-aware where Unicode case/whitespace
is out of scope (documented per function):

- **Split/join:** `split(s, sep: String) -> Array<String>` (empty `sep`
  → per-`slice` chars is a hazard — reject empty sep with a trap, or
  document char-splitting; **decide and pin**), `lines(s) -> Array<String>`
  (splits on `\n`, strips a trailing `\r` — the `\r\n` case vlog
  hand-rolled), `splitWhitespace(s) -> Array<String>`, `joinWith` (exists
  — kept), `repeat` (exists — kept).
- **Trim:** `trim`/`trimStart`/`trimEnd` (ASCII whitespace ` \t\r\n`).
- **Case (ASCII only, documented):** `toLower`/`toUpper` — non-ASCII bytes
  pass through unchanged (a real Unicode case fold is out of scope).
- **Search:** `indexOf(s, needle) -> Int64` (`-1` absent — or `Option`,
  pick the house idiom and pin), `lastIndexOf`, `contains`/`startsWith`/
  `endsWith` (these EXIST as methods — re-export/align so one import
  covers the surface; note which are methods vs functions).
- **Build:** `replace(s, from, to)`, `padStart(s, len, fill)`/
  `padEnd(s, len, fill)`, `substring(s, start, end)` as a friendly alias
  of `slice`.
- **Bytes bridge:** `toHex(bytes)` (vlog's fingerprint) if it reads
  cleanly here rather than `std/hash`.

All are pure → **`std/strings` is a parity citizen** (`examples/strings.vyrn`
exercising every op three-way, incl. a `slice` mid-codepoint **trap** and
a multi-byte UTF-8 string through `lines`/`slice`/`indexOf`).

## 3. Hygiene fix: non-exported names are private to name resolution

vlog hit a real bug: a **private** `fn pad2` in `std/time` collided with
a local `pad2`, forcing a rename — "private names aren't private to name
resolution." A non-exported top-level decl should never force a rename on
a *consumer*: it is invisible outside its module, so the linker must
**auto-rename a non-exported decl on any cross-module name collision**
(the RFC-0027 `member__fromN` rename machinery already exists; extend it
to always fire for a non-exported decl whose name clashes, since renaming
something no one can import by name is always safe). Pin: two modules each
with a private same-named helper link cleanly; a user local may shadow a
std-internal name without error.

## 4. Doc fix + consumer migration

- **Doc drift:** `examples/stringops.vyrn` (and any doc) says `s[i]` is
  `Int64` — it is `UInt8`. Correct it.
- **Consumers:** `vlog` drops its six hand-rolled ops for `std/strings`
  (the proof it removes the pain); `examples/bin`'s and `shelf`'s `util`
  hand-rolled string code migrates where `std/strings` covers it. Emitted
  behavior unchanged; the apps just shrink.

## Out of scope

Unicode case folding / normalization / grapheme segmentation (ASCII
case + byte/codepoint ops only), regex-based split/replace (RFC-0020's
regex is a separate surface), a `char`/`rune` type (String stays UTF-8
bytes; `slice` + codepoint-aware iteration is the model), locale
collation, `format`/`printf`-style templating (string templates already
cover interpolation), a `StringBuilder` (append via `+`/`joinWith` for
now — revisit only with a measured perf case).

---

## As landed

- **`slice` builtin (§1).** `slice(s: String, start: Int64, end: Int64) ->
  String`, implemented in checker (signature), interp (`is_char_boundary` —
  the two-continuation-byte test in one call), and codegen (shared
  native/wasm IR + a `@__vyrn_bytecopy` runtime helper; the copy buffer is
  region-arena'd inside a `region`, else `malloc`'d, exactly like `concat`).
  The O(1) boundary check reads the single byte at each cut point and masks
  `(b & 0xC0) == 0x80` (the same test `chars` uses); reading `s[len]` is safe
  (the NUL terminator, never a continuation byte), so whole-string / empty-
  tail slices fall through. Two traps, single-sourced beside the array-OOB
  globals: `error: slice index out of range` (start<0 / end>len / start>end,
  mirroring array OOB) and `error: slice splits a UTF-8 character`.
  Byte-identical three-way including both trap lines (verified via
  `examples/strings.vyrn`; parity 73 checked, 0 failed).
- **Name-privacy fix (§3) — mechanism.** In the loader's `resolve_aliases`,
  a new pass (right after the namespace-rename pass, before the reference
  rewrite) mints a `member__fromN` rename for every **non-exported**
  top-level decl whose name also appears in another module
  (`name_module_count >= 2`), so `link`'s uniqueness check never sees the
  clash. `rename_decl_in_module` (pass 3) rewrites the owning module's own
  references. **Guard:** a name the module *itself imports* is skipped — that
  is a genuine import-vs-declaration clash the user must resolve, and it
  still errors (this preserved three existing collision tests). Injected
  line-0 types are skipped. Pins: two modules with a private same-named
  helper link cleanly; a local may shadow a private std-internal (`pad2`).
- **`split` empty-separator decision (§2).** Returns `[s]` (the input
  unsplit). Per-byte "char" splitting would cut multi-byte UTF-8 (a `slice`
  trap), and pure Vyrn cannot raise a custom-worded trap (no `panic`/assert
  outside tests), so the safe, documented no-op was chosen over both listed
  options.
- **`indexOf` return idiom (§2).** `Option<Int64>` (`None` when absent), not
  a `-1` sentinel — matching the house idiom (`parse`, `Map` lookup). Same
  for `lastIndexOf`.
- **methods vs functions (§2).** `contains`/`startsWith`/`endsWith` stayed
  compiler **builtins** (methods via UFCS, also free functions), available
  everywhere without importing `std/strings`; documented in the module
  header so one `import { .. } from "std/strings"` plus those builtins
  covers the surface. `toHex(n: UInt64)` landed here (vlog's fingerprint),
  not in `std/hash`.
- **Consumers (§4).** `vlog` dropped all six hand-rolled ops + the private
  `padTwo` (~110 lines) for `substring`/`indexOf`/`trim`/`toLower`/`toUpper`/
  `lines`/`padStart`/`toHex`; overview parity byte-identical, 11 tests pass.
  `shelf/util` collapsed to a `std/strings`-based `splitTrim` (its `sep` is
  now a `String`); `bin/store`'s `prefixOf` became `substring`.
  `examples/stringops.vyrn` doc drift (`s[i]` is a `UInt8`) corrected.
- **No wall.** The privacy fix did not fight the rename machinery — the
  `member__fromN` mint + `rename_decl_in_module` already did exactly what was
  needed; the only subtlety was the import-vs-declaration guard. `slice`'s
  UTF-8 boundary check had no codegen subtlety (the NUL terminator makes the
  end-of-string read safe without a special case).
