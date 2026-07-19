# Dogfood notes — building `examples/vlog` (an NDJSON log CLI) on the text/stdin axis

A record of every point of friction found while building **vlog** — a real,
usable structured-log processor — on a **different axis** than the web dogfoods
(`shelf`, `bin`): a pure CLI. No server, no UI, no `vyrn dev`. Just `args()`,
stdin, files, the JSON codec, `Map`, `std/time`, `std/hash`, and a great deal of
**string processing**. The app is the pretext; this report is the deliverable.

The headline probe was **STRING PROCESSING**. `std/strings` today is two
functions (`repeat`, `joinWith`). A log tool needs substring, split, indexOf,
trim, case-folding, and integer-to-hex — none of which exist — so this app
hand-rolled all six, and the shape of that pain is the main finding. The second
probe was **stdin/streaming ergonomics**: `readLine()` (streaming) vs
`readFile` + a hand-rolled split. Both are exercised here on purpose so they can
be compared directly.

The app works, end to end, as a three-way parity citizen:

```
$ vyrn run examples/vlog.vyrn < examples/vlog.stdin      # overview: fmt+count+stats
$ vyrn run examples/vlog.vyrn count   < log.ndjson       # level histogram (a Map)
$ vyrn run examples/vlog.vyrn filter --level error       # (also --contains <sub>)
$ vyrn run examples/vlog.vyrn filter --level=info --contains retry
$ vyrn run examples/vlog.vyrn tail -n 2  < log.ndjson    # last N lines
$ vyrn run examples/vlog.vyrn fmt        < log.ndjson    # HH:MM:SS LEVEL msg
$ vyrn run examples/vlog.vyrn stats      < log.ndjson    # min/max ts, span, FNV
$ vyrn run examples/vlog.vyrn count --file log.ndjson    # file source (readFile+split)
```

A malformed line is reported to stderr with its line number and **skipped**, not
crashed (`[WARN] vlog: line 6: unexpected character at position 1`). `vyrn test
examples/vlog.vyrn` runs 11 in-language tests over the pure logic (the
hand-rolled string ops, the histogram tally, the arg parser, duration
rendering). No 4th silent bug surfaced on the string/stdin path — the codec,
`Map`, and I/O behaved exactly as documented (calibration in §3).

---

## TL;DR — top friction items (evidence in one line each)

1. **`std/strings` is nearly empty → six hand-rolled string ops.** substring,
   split, indexOf, trim, lower/upper case, int→hex all had to be written in the
   app. This is the clear **#1 next-RFC candidate (`std/strings`)**. (library)
2. **A byte has no one-character `String` form, so EVERY substring round-trips
   through `bytes()` + `stringFromBytes`.** `s[i]` is a `UInt8`; there is no
   `String(byte)` and `"\{s[i]}"` interpolates the *number* (`"97"`), so
   `sliceStr` must build an `Array<UInt8>` and revalidate it as UTF-8. This makes
   even a 3-line helper allocate-and-revalidate. (language/library)
3. **No `split` → `readFile` + a hand-rolled `splitLines`; `readLine()` is the
   ergonomic path.** The streaming loop needs no splitting (readLine strips
   `\r?\n` for free); the file path has to dice the whole String on `\n` by hand
   AND strip a surviving `\r` later via `trim`. Splitting is the headline gap for
   any file-oriented text tool. (library — `split`/`lines`)
4. **`match` arms are expression-only (no statement blocks) + no `if let`/`while
   let` → bool-tagged records to branch a sum type.** Reading stdin, decoding a
   line, and choosing a source each wrap their `Option`/`Validation` in a
   `{ ok, ... }` record so the *next* statements can run. Three types (`Read`,
   `Decoded`, `Source`) exist only to work around this. (language ergonomics)
5. **No arg parser → hand-rolled `getFlag`.** `--flag value` and `--flag=value`
   both work, but only because the app re-derives them from `indexOfByte`/
   `sliceStr`. A tiny `std/args` would erase this. (library, lower priority)
6. **A module's *private* `fn pad2` still collides across the linked program.**
   `std/time`'s unexported `pad2` forced me to rename my local `pad2` → `padTwo`
   (`` `pad2` is defined in both … top-level names must be unique ``). Private
   helpers are not private to name resolution. (compiler — reported, not fixed)
7. **Doc drift: `s[i]` is a `UInt8`, but `examples/stringops.vyrn` documents it
   as `Int64`.** Cost one compile-error round-trip (`==` needs matching scalar
   operands, found UInt8 and Int64). (docs)

Nothing here is a silent-wrong bug; #6 and #7 are the compiler/docs issues, both
surfaced loudly at compile time.

---

## 1. Friction, as it happened (wrote-vs-wished)

### STRING PROCESSING (the headline)

Building an NDJSON tool is mostly string work, and `std/strings` covers almost
none of it. The full inventory of what I hand-rolled, in `examples/vlog.vyrn`:

| Wanted | Have | Wrote (app helper) |
|---|---|---|
| `s.substring(a,b)` / slice | — | `sliceStr(s,start,end)` via `bytes()`+`stringFromBytes` |
| `s.split(sep)` / `lines()` | — | `splitLines(s)` (byte scan on `\n`) |
| `s.indexOf(ch)` | — | `indexOfByte(s,needle)` (linear scan) |
| `s.trim()` | — | `trim(s)` (two scans + `sliceStr`) |
| `s.toLowerCase()`/`toUpperCase()` | — | `toLowerAscii`/`toUpperAscii` (ASCII only) |
| `n.toHex()` / radix format | `.toString()` decimal-only | `toHex(n)` (nibble loop, bitwise) |
| `pad2` (zero-pad) | private in `std/time` | `padTwo(n)` |

What DID exist and helped: **`s.contains(sub)`, `s.startsWith(sub)`,
`s.endsWith(sub)`** are builtins (used for `--contains`, and for the "is this a
flag?" test `a[0].startsWith("-")`). So the *predicate* half of string handling
is covered; the *transform/extract* half is not.

**The deepest cut — no byte→String.** `s[i]` yields a `UInt8`. There is no way
to turn one byte into a one-character `String`: `"\{s[i]}"` renders the number
(`97`), and there is no `String(byte)` or char-append. The ONLY path to a
substring is:

```vyrn
let b = bytes(s)
let mut buf: Array<UInt8> = []
let mut i = start
while i < end { buf.push(b[i]); i = i + 1 }
return match stringFromBytes(buf) { Ok(x) => x, Err(e) => "" }
```

Every one of `sliceStr`, `trim`, `toLowerAscii`, `toUpperAscii`, `splitLines`,
`toHex` ends in that same `bytes()` → mutate `Array<UInt8>` → `stringFromBytes`
→ `match Ok/Err` boilerplate, including a UTF-8 revalidation that can never fail
(the bytes came from a valid String or a fixed ASCII table). It works and it is
parity-clean, but it is a lot of ceremony for "give me characters 11..19".

**`toHex` — decimal-only integer formatting.** `.toString()` is base-10 only.
The `stats` content fingerprint wants hex, so `toHex` walks the 16 nibbles with
`(n >> sh) & 15` (RFC-0045 bitwise — which worked perfectly, see §3) and indexes
a `"0123456789abcdef"` byte table. Clean once written, but every base-N need
re-invents this.

**Case-folding is ASCII-only by necessity.** Vyrn strings are UTF-8 bytes; I
fold `A–Z`/`a–z` by numeric range. Fine for log levels; a real `toLowerCase`
would need Unicode tables the app can't carry. This is squarely a `std/strings`
job (and a reason it is library, not a one-liner).

### stdin / streaming ergonomics

- **`readLine()` is the natural surface** and needs *no* string processing: it
  streams, strips `\r?\n`, and returns `None` at EOF. The stdin reader
  (`readStdinLines`) is a plain loop. This is the ergonomic path — full marks.
- **EOF via `Option` is fine, but branching it is not.** `readLine()` returns
  `Option<String>`; with no `while let`, the loop needs a helper (`readOne`)
  that repackages `Option` into a `{ eof, line }` record so the `while` body can
  be ordinary statements (see the match-arm friction below). input.vyrn's
  example uses a `"<eof>"` **sentinel string** for the same reason — that is the
  established idiom, and it is a workaround.
- **The file path is where splitting bites.** `readFile` returns the whole file
  as one `String`; to reach the same per-line pipeline I had to hand-roll
  `splitLines` and then lean on `trim` to shed the `\r` that `readLine()` would
  have removed. So the two sources are asymmetric: stdin is turnkey, a file is
  "now go write a splitter." A `lines(s)` / `split(s, sep)` in `std/strings`
  closes the gap and would let both sources share one code path.
- **Reading stdin is once-only**, correctly. I buffer all lines up front
  (`loadLines`) and every subcommand shares the buffer — the right model, no
  friction, just worth noting there is no rewind.

### args parsing (hand-rolled)

- No `std/args`. `getFlag(a, name)` scans the `Array<String>` for `--name value`
  and `--name=value` (the `=` form needs `indexOfByte('=')` + two `sliceStr`s —
  more string work). It is ~15 lines and testable, but every CLI will rewrite it.
- **Subcommand-vs-flag disambiguation** is manual: the subcommand is "the first
  arg that does not `startsWith("-")`", so `--file` before a subcommand doesn't
  masquerade as one. A parser library would own this.
- Verdict: a small `std/args` (flags, values, subcommand, `-n`/`--lines`
  aliases) is **wanted but second** to `std/strings` — and note it would itself
  be *built on* `std/strings`, so strings come first.

### codec on per-line untrusted JSON

- **`fromJson(LogLine, line)` per line is exactly right** — it never traps, and
  a malformed line becomes a `Validation.Invalid` I log and skip. This is the
  single best part of the CLI story (calibration in §3).
- **Two real modeling frictions, both worked around cleanly:**
  - **No dynamic/`any` JSON value.** Real log lines carry an arbitrary
    `"fields":{...}` object with heterogeneous value types (`"port":8080`,
    `"ms":210`). There is no `JsonValue`/`any`, and `Map<String,String>` would
    reject numeric values, so I **model only `ts`/`level`/`msg` and let the codec
    drop `fields`** (unknown fields are ignored — documented behavior). Correct
    for this tool, but a log tool that wants to filter on `fields.userId` can't,
    today, without a concrete schema per shape.
  - **Enum decode is capitalized-variant-name; real levels are lowercase.** A
    `type Level = | Info | Error` decodes from `"Info"`, but logs emit `"info"`.
    So `level` stays a **free `String`** in `LogLine` (tallied case-folded), and
    the `Level` *enum* is used only to **validate the `--level` CLI argument**
    (`--level wizard` → exit 2). That split is arguably the right design, but it
    was forced by the case mismatch, not chosen.

### Map / time / hash ergonomics on the CLI

- **`Map<String,Int64>` as the histogram is perfect** — insertion-ordered
  (deterministic output), honest `m[k] -> Option<V>` lookup, `m.keys()` snapshot.
  The tally is `m[lv] = match m[lv] { Some(c) => c+1, None => 1 }`. No friction.
- **`std/time` did the `fmt` and `stats` timestamps beautifully** from the epoch
  millis in the DATA (no `now()` needed → no fixed clock needed for parity). One
  papercut: I wanted `HH:MM:SS` (chars 11..19 of `format()`), but with no
  substring I recomputed from the exported `hour`/`minute`/`second` — and had to
  re-roll `pad2` because `std/time`'s is private. See the `pad2` collision below.
- **`std/hash` `fnv1aStr` gave a one-call content fingerprint** over the joined
  corpus. Zero friction; the only reason it needs `toHex` is the decimal-only
  formatting gap.

### Language / std / tooling papercuts

- **`match` arms are expression-only.** A block-bodied arm
  (`Some(x) => { stmt; stmt }`) is a parse error (`unexpected token: LBrace`).
  Combined with **no `if let` / `while let`**, any "inspect a sum type, then run
  statements" needs either a helper returning a value or a bool-tagged record.
  This produced three otherwise-pointless types (`Read`, `Decoded`, `Source`).
  This is the most pervasive ergonomic tax in the whole app.
- **Private module names are not private to linking (compiler).** `std/time`'s
  unexported `fn pad2` collides with a local `fn pad2`:
  `` `pad2` is defined in both `std/time.vyrn` and `vlog.vyrn` — top-level names
  must be unique across the program ``. A private helper should not consume a
  name in every importer's global namespace. Reported, not fixed; worked around
  by renaming to `padTwo`.
- **Doc drift (docs).** `examples/stringops.vyrn` says `s[i]` is an `Int64`; it
  is a `UInt8`. My `indexOfByte(needle: Int64)` failed to compile until `needle`
  became `UInt8`. Minor, loud, one round-trip.
- **`logger(...)` to stderr is a clean fit for a CLI** — `[WARN] vlog: …` /
  `[ERROR] vlog: …` land on stderr, keeping stdout pure data (pipeable). Good.
  Minor wish: no `eprint`-style raw-stderr primitive, so the `[LEVEL] name:`
  prefix is mandatory; fine for this tool, occasionally more than you want.

---

## 2. Prioritized next-RFC candidates (with evidence)

### #1 — `std/strings` (the clear winner). Library, mostly; one language hook.

- **Pain evidence:** six hand-rolled helpers in one small app (`sliceStr`,
  `splitLines`, `indexOfByte`, `trim`, `toLowerAscii`/`toUpperAscii`, `toHex`),
  every substring round-tripping through `bytes()`+`stringFromBytes`.
- **Scope:** `split(s, sep) -> Array<String>`, `lines(s)`, `substring/slice(s,
  a, b)`, `indexOf(s, sub)`, `trim`/`trimStart`/`trimEnd`, `toLowerCase`/
  `toUpperCase` (ASCII first; Unicode later), `padStart`/`padEnd`, and integer
  radix formatting (`toHex`/`toRadix`). Most are ordinary Vyrn over `bytes()`
  (three-way parity for free, exactly like `std/math`/`std/arrays`).
- **The one language hook worth considering:** a cheap **byte→`String`** (or
  `String` from an `Array<UInt8>` slice without a full revalidate when the source
  was already valid). Everything string-building today pays a UTF-8 revalidation
  it provably doesn't need. Even just a `char`/1-byte-string constructor would
  remove the most boilerplate. Library can ship on top of `bytes()` immediately;
  this hook is the optional performance/ergonomics follow-up.

### #2 — a `lines()` / streaming-split helper (a subset of #1, but call it out).

- **Pain evidence:** stdin (`readLine`) is turnkey; a **file** source forced
  `splitLines` + manual `\r` handling, making the two input paths asymmetric.
- **Scope:** `lines(s) -> Array<String>` that strips `\r?\n` per element (so a
  file matches `readLine()` semantics exactly), plus maybe a lazy
  `readLines()` iterator over stdin. Small, pure, unblocks every file-oriented
  text tool. Ships inside `std/strings`.

### #3 — `std/args` (a real papercut, but built on #1).

- **Pain evidence:** `getFlag` + subcommand/flag disambiguation hand-rolled;
  every CLI will repeat it.
- **Scope:** flags (`--name`, `--name=value`, `-n value`), booleans, positional
  args, subcommand extraction, and a usage/error convention. Library, written in
  Vyrn over `std/strings` — hence strictly after #1.

### #4 — `if let` / `while let` (or block-bodied `match` arms). Language.

- **Pain evidence:** three bool-tagged record types (`Read`, `Decoded`,
  `Source`) exist ONLY because a `match` arm can't run statements and there is no
  `while let`. This is the most repeated ergonomic tax in the app.
- **Scope:** either allow statement-block `match` arms, or add
  `if let Some(x) = expr { … }` / `while let Some(x) = readLine() { … }`. Pure
  surface sugar; large ergonomic payoff for exactly the I/O-loop and
  decode-then-act shapes a CLI is made of.

---

## 3. What worked well (calibration)

Not everything hurt. The pieces the recent RFCs added were the ones that made
this app *possible on the CLI axis*, and they were friction-free:

- **`fromJson` per untrusted line never traps.** `Validation.Invalid` for a bad
  line, logged-and-skipped; `Valid` otherwise. Robust log parsing fell out of
  the codec with zero defensive code. Best part of the story.
- **`Map<String,Int64>` histogram** — insertion-ordered (deterministic stdout),
  honest `Option` lookup, `keys()` snapshot. The `count` subcommand is six lines.
- **`std/time` from real data** — `format`/`hour`/`minute`/`second`/`fromMillis`
  rendered `fmt` and `stats` timestamps straight from the epoch millis in the
  log lines. Because no `now()` is called, the app is a parity citizen with **no
  fixed clock needed** — a nice demonstration that the RFC-0043 breakdown is pure.
- **`std/hash` `fnv1aStr`** — a one-call content fingerprint over the corpus,
  deterministic across backends (RFC-0045 wrapping `UInt64` math).
- **RFC-0045 bitwise ops** — `(n >> sh) & 15` for the hex nibbles worked
  first try, at `UInt64` width, byte-identical three-way.
- **`args()` + `readLine()` + `readFile` + `writeFile`-adjacent I/O (RFC-0014)**
  — the whole input surface a CLI needs, with the `.stdin` fixture convention
  letting the **main path be a true three-way parity citizen** (`ok vlog.vyrn`
  in the harness).
- **`logger(...).warn/.error` to stderr** — clean stdout/stderr separation, so
  `vlog count | sort` pipes cleanly while diagnostics go to stderr.
- **`vyrn test` in the same file as `main`** — 11 tests over the pure helpers,
  stripped from `run`/`build`, so the example stays a parity citizen AND is
  unit-tested. Exactly the RFC-0015 promise.

---

## Verification & parity

- `examples/vlog.vyrn` + `examples/vlog.stdin` — the **no-arg overview is a
  three-way parity citizen**: the harness pipes `vlog.stdin` into interp, native,
  and wasm and they agree byte-for-byte, INCLUDING the `[WARN]` stderr line for
  the malformed input. `parity: 72 checked, 2 skipped, 0 failed`; the run logs
  `ok vlog.vyrn`. Subcommand dispatch is driven by `args()`, which the harness
  leaves empty (so the overview path is what parity covers) — real subcommands
  verified manually (transcripts above).
- Full suite unchanged: **922 workspace + 17 LSP passing, 0 warnings**; parity
  green (interp == native == wasm) with `vlog` added — counts only-add (71 → 72
  checked examples).
- `vyrn test examples/vlog.vyrn`: **11 passed, 0 failed**. `vyrn fmt
  --check`: clean.
