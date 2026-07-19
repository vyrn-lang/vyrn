# RFC-0046 — `std/strings`: The String Library (+ a `slice` builtin)

- **Status:** Draft (design locked)
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
