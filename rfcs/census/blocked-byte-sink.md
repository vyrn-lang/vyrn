# The byte sink — what blocks mandelbrot, and what would unblock it

Status: **CLOSED. Design A was chosen and built** —
[RFC-0111](../RFC-0111-a-program-can-write-bytes.md), 2026-08-25.
`examples/mandelbrot.vyrn` writes the committed fixture's 5,011 bytes on all
three engines. Both traps this file predicted were real and both are now gated:
the Windows text-mode one costs exactly two bytes and is pinned by
`compiler/vyrn-cli/tests/bytesink.rs`, and the browser's decode-before-anyone-
sees-it cost 3,960 bytes of U+FFFD and is fixed in `web/wasi-min.js`.

A THIRD thing this file did not predict: the parity harness cannot see either
trap, because it compares runs through a lossy UTF-8 decode and a CRLF-to-LF
replace, and both halves alter exactly the content at issue. Binary output
needed a gate of its own. The evidence below is left as written.

## Why this file exists

`rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md:408-416` records the gap:

> **regex-redux and mandelbrot are not here, and their absence is the boundary
> rather than an omission.** Neither was worked around. `=~` is an anchored full
> match against a compile-time-constant pattern — it answers neither "how many"
> nor "where", and there is no substitution by pattern — so regex-redux needs a
> runtime regex that searches, counts and replaces. mandelbrot's pixels are right
> and cannot leave the program: `print` and `writeFile` both take a `String` and
> `stringFromBytes` refuses a packed row, so it needs a byte sink.
> `regexredux-1000.expected` and `mandelbrot-200.expected` stay committed with no
> program beside them, which is what a named gap looks like in a corpus.

The earlier statement at `rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md:86`
names it `p-mandelbrot`, `p-binout`: "the game writes a binary PBM, `print` and
`writeFile` both take a `String`, and `stringFromBytes` refuses a packed row with
`bytes are not valid UTF-8`".

**Both statements are still true.** Checked on 2026-08-23:

- Every content-carrying output builtin still takes a `String`
  (`compiler/vyrn-frontend/src/checker.rs:6556-6558`).
- `stringFromBytes` still refuses a NUL byte before UTF-8, and refuses invalid
  UTF-8 (`rfcs/RFC-0014-input-io.md:63-64`; interpreter error strings pinned in
  `compiler/vyrn-frontend/src/interp.rs:7459-7472`; native rule restated at
  `compiler/vyrn-codegen/src/direct.rs:14916-14918`).
- A PBM P4 row is arbitrary bytes. The committed fixture begins
  `50 34 0a 32 30 30 20 32 30 30 0a 00 …` ("P4\n200 200\n" then zero bytes,
  `rfcs/bench-0104/mandelbrot-200.expected`). A NUL cannot enter a Vyrn
  `String` (`rfcs/RFC-0014-input-io.md:56-58`), so the header could be printed
  and the pixels could not.

## Every way a Vyrn program sends data out today

| path | signature | source of the signature |
|---|---|---|
| `print(x)` | `Unit`; x must render: `Int`, sized ints, `Float`, `Float32`, `Bool`, `String` | `checker.rs:6203-6225`; the type gate is `types::renders`, `compiler/vyrn-frontend/src/types.rs:287-292` |
| `writeFile(path, contents)` | `Result<Bool, String>`; both operands `String` | `checker.rs:6543-6560`; `rfcs/RFC-0014-input-io.md:37-38` |
| `renameFile(from, to)` | `Result<Bool, String>`; moves one file over another | `checker.rs:6562-6565` |
| `fsyncFile(path)` | `Result<Bool, String>` | `checker.rs` RESERVED list, `checker.rs:228-232`; capability table `compiler/vyrn-frontend/src/floor.rs:134-138` |
| `writeAtomic(path, content)` | `Result<Bool, String>`; std wrapper: temp write + `renameFile` | `std/storage.vyrn:51-54` |
| logging sinks | `logging { level: .., sink: stderr / stdout / file("path") }`; messages are `String`s | parser: `compiler/vyrn-frontend/src/parser.rs:2277-2352`; sink type `ast.rs:114-118` |
| `extern` calls | declared foreign functions; in a browser they reach JS | `web/wasi-min.js:20-33` |

Every one of these carries text. There is no builtin whose parameter or payload
is `Array<UInt8>` on the way OUT. The only binary primitive is on the way IN:
`readFileBytes(path) -> Result<Array<UInt8>, String>`
(`rfcs/RFC-0014-input-io.md:113-115`).

## Where the String requirement is enforced

- **Checker.** `` `writeFile` needs String arguments, found {t} `` —
  `checker.rs:6556-6558`. For `print`, the gate is `types::renders`
  (`types.rs:287-292`): numbers, `Bool`, `String` — nothing else, except a type
  with a user-declared `Show` impl (`checker.rs:6213-6223`).
- **Interpreter.** The runtime re-checks: `writeFile` matches its two operands
  as `Val::Str` and errors otherwise (`compiler/vyrn-frontend/src/interp.rs:5310-5322`),
  then writes with `std::fs::write(path, contents.as_bytes())`
  (`interp.rs:5323`). A byte array value has no arm.
- **Native backend.** The lowering passes two pointers to
  `__vyrn_write_file(path, contents)` (`compiler/vyrn-codegen/src/lib.rs:9823-9826`,
  emitted call asserted at `lib.rs:14644-14648`). The C shim takes
  `const char*` and uses `strlen` for the length, "A Vyrn String is
  NUL-terminated and never contains a NUL, so strlen is its full length"
  (`compiler/vyrn-codegen/src/toolchain.rs:336-339`). An `Array<UInt8>` has no
  NUL terminator, so the ABI itself is String-shaped.
- **Wasm backend.** The same direct-backend lowering
  (`compiler/vyrn-codegen/src/direct.rs:6781-6791`) reaches wasi-libc's
  `fopen(path, "w")` through WASI `path_open` with CREAT\|TRUNC
  (`direct.rs:583-587`) and writes through the same shim family.

## What stringFromBytes refuses, and why

Quoted from `rfcs/RFC-0014-input-io.md:63-64`:

> `stringFromBytes` of bytes containing NUL → `Err("bytes contain a NUL
> byte")` (and invalid UTF-8 → `Err("bytes are not valid UTF-8")` as below).

The rule behind it, same file, lines 56-58: a NUL byte is valid UTF-8 but cannot
live in a NUL-terminated Vyrn String. Both refusals are pinned by tests on every
engine (`interp.rs:7459-7472`; the native side reuses Höhrmann's UTF-8 DFA,
`lib.rs:12050-12053`). A packed PBM row fails both: it contains NULs and almost
every byte value outside valid UTF-8 sequences.

## What each backend can actually do today

| backend | writing arbitrary bytes to a FILE | writing arbitrary bytes to STDOUT |
|---|---|---|
| interpreter | Yes, in principle — it owns a real file system and already calls `std::fs::write` (`interp.rs:5323`). Only the `String` type in front of it stops bytes. | Yes — `println!`/stdout under the host process (`interp.rs:216-224`); buffered when the interpreter itself runs in a browser tab (`playhost.rs:7-16`, drained as `(String, String)`). The buffer is Rust `String`, so binary bytes would need a buffer-type change too. |
| native | Yes — the C shim does create/truncate + write-all (`toolchain.rs:336-339`) via clang on MSVC, glibc and wasi-libc alike (`lib.rs:1302-1308`). Needs a length-parameter variant, since `strlen` is String-only. | Yes — `__vyrn_stdout()` + `fprintf` (`toolchain.rs:56-57`, `lib.rs:1306-1308`); an `fwrite(ptr, 1, n, stdout)` sibling is the same shape. |
| wasm (wasi-libc target) | Yes where a WASI filesystem exists — `path_open` CREAT\|TRUNC behaves like `fopen(path, "w")` (`direct.rs:583-587`). Under wasmtime with a preopen, binary bytes flow. | Yes — wasi-libc buffers stdout and issues `fd_write`; WASI iovecs carry raw bytes. |
| browser shim (`web/wasi-min.js`) | **No, deliberately.** `path_open` returns `ERRNO_NOENT` so `readFile`/`writeFile` fail with their canonical `Err` payloads; the module loads and runs, it sees an empty world (`wasi-min.js:11-16`, `277-280`). | Text only. `fd_write` decodes the iovec bytes with a `TextDecoder` in streaming mode and appends to a JS `String` per stream (`wasi-min.js:143-176`). Invalid UTF-8 becomes U+FFFD, so packed pixel rows would arrive corrupted even though the syscall carried them intact. |

So the missing piece is not capability on any engine — three of the four hosts
can move arbitrary bytes today. The missing piece is a language-level API that
is not shaped like a NUL-terminated `String`.

## Three designs

| design | the API | checker changes | interpreter changes | native changes | wasm changes | browser behaviour | parity risk |
|---|---|---|---|---|---|---|---|
| A. Two new builtins | `writeFileBytes(path: String, b: Array<UInt8>) -> Result<Bool, String>` and `writeStdout(b: Array<UInt8>) -> Unit` | two signature arms beside `writeFile` (`checker.rs:6543`); add to RESERVED, `SPAWN_FORBIDDEN`, `COMPTIME_FORBIDDEN`, capability table `Capability::Fs` (`floor.rs:134-138`) | two arms beside `interp.rs:5310`: `std::fs::write(path, bytes)`, `std::io::stdout().write_all(bytes)` | one new shim function `__vyrn_write_bytes(path, ptr, len)` (length replaces `strlen`), plus `fwrite(ptr, 1, len, stdout)`; IR emits calls beside `lib.rs:9823` | none beyond the shared direct-backend lowering — the same C shim compiles against wasi-libc, and `fd_write` already carries raw bytes | files stay canonical-`Err` (no FS, unchanged). stdout needs the shim to stop decoding: chunk hooks must hand out `Uint8Array`s instead of decoded text, or packed rows reach the page corrupted (`wasi-min.js:143-176`) | LOWEST. Byte-for-byte identical payloads by construction; the only divergence left is transport-level (below) |
| B. A `Stream<UInt8>` sink | e.g. `stdoutBytes() -> Stream<UInt8>` consumed by push/close, built on the linear stream type (`std/stream.vyrn:1-9`) | stream handles are already spawn-restricted (`close` is in `SPAWN_FORBIDDEN`, `checker.rs:9339-9342`); needs a capability story for "this stream IS stdout/file", plus ownership rules for who may close | a producer-side slab entry backed by a real writer instead of a cursor (`stream.vyrn:34-62` shows the cursor shape that would be mirrored) | the shim grows a FILE*-backed sink object with manual lifetime | same, plus WASI fd-backed sink; the pull-model combinators (`map`, `filter`, `take`, `stream.vyrn:18-23`) assume a SOURCE — a sink inverts the direction, which the module says it never modelled | a stream whose backing store does not exist; would have to degrade to a buffer the page drains — a third behaviour beside "canonical Err" and "real write" | MEDIUM-HIGH. One abstraction, three backing stores, manual close ordering per backend; a missed close flushes differently on each engine. Costs more surface than the problem has |
| C. One general output handle | `openWrite(path) -> Result<Handle, String>`; `print` and `writeFile` become uses of it | rewrites the signatures of every builtin in the table above; print's newline semantics become handle state | rewrites every `Val::Str` output arm | new handle objects in the shim with open/close/flush | new WASI fd-facing handle objects | needs a virtual FS or per-handle JS callbacks — the largest browser lift | HIGHEST. Touches every existing program and every parity test at once; the newline-per-print question becomes a handle-mode flag that can silently differ per engine |

### Where parity could break

All three backends must produce identical bytes. Per design:

- **A.** The payload is identical by construction — one array of bytes handed to
  each engine. Two transport risks remain. First, newline translation: a native
  Windows build already writes `\r\n` where the interpreter and wasm write `\n`,
  and the bench harness normalises line endings before diffing
  (`RFC-0104:166-169`, harness note at `RFC-0104:513-515`). That comes from
  text-mode stdio. Design A's file shim must open in binary mode (`"wb"`), or a
  byte 0x0A inside a packed row becomes 0x0D 0x0A on Windows native only — a
  corruption no normalisation can undo safely. Second, the browser shim's
  decode-as-text step corrupts non-UTF-8 chunks before any JS consumer sees them;
  fixing it is a `wasi-min.js` change with an API-visible shape (chunk type).
- **B.** Flush points. A stream closes when its last reference dies or `close`
  runs; the interpreter (Rust drop), native (manual close through the shim) and
  wasm (fd lifecycle) order these differently today. Output that ends unflushed
  differs per engine. This is exactly why `close` is already spawn-forbidden
  (`checker.rs:9340-9342`) — the rule would grow, not shrink.
- **C.** Everything in B, multiplied: buffering mode, newline mode and encoding
  all become per-handle state that three backends must agree on for every
  combination.

## Recommendation, not a decision

Ranked:

1. **Design A** — smallest surface, follows the precedent set by
   `readFileBytes` (a binary builtin with per-engine bodies behind ONE canonical
   behaviour, `RFC-0014-input-io.md:113-115`), and leaves `std/` free to build
   friendlier wrappers in pure Vyrn afterwards, the way `std/storage.vyrn:51-54`
   wraps `writeFile`.
2. **Design B** — worth doing only after A, if a use appears that streams
   rather than writes whole buffers; it adds machinery the mandelbrot gap does
   not need.
3. **Design C** — a redesign of output, not an unblocking of mandelbrot; price
   it separately if the owner wants it.

Note on the rules: the census brief's standing backend rule (quoted in full in
`blocked-regex.md`; the brief file itself is retired with the tool that read
it) forbids adding a native body for
a standard-library FUNCTION and forbids divergent per-backend implementations.
The existing I/O builtins already carry per-engine bodies behind one canonical
behaviour (`RFC-0014`'s structure); design A extends that existing class of
thing rather than moving `std/` code into a backend. Any design that put, say, a
PBM encoder into Rust would violate the rule; putting `writeFileBytes` next to
`writeFile` does not. The owner rules on that reading.
