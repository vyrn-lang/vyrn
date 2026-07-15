# RFC-0014 — Input: Args, Stdin, Files, Bytes

- **Status:** Draft — approved for implementation
- **Depends on:** RFC-0005 (`Option`/`Result`), RFC-0012 (the browser has its
  own input story — `extern`; this RFC is about native/WASI input)

> **Motivation.** A Vela program can compute, validate, and print — but it
> cannot *read*: no CLI args, no stdin, no files. Every program computes from
> constants. This is the single biggest "real language" gap, and the
> prerequisite for the server half of the long-range goal (same-language
> server + client). The review direction was: `Array<UInt8>` byte buffers →
> `fromBytes` → input I/O.

---

## Surface (M1 — text)

```vela
fn main() -> Int64 {
    let who = args()                          // Array<String>: argv[1..]
    let line = readLine()                     // Option<String>: one stdin line, None at EOF
    let cfg = readFile("config.json")         // Result<String, String>
    let ok = writeFile("out.txt", "content")  // Result<Bool, String>
    return 0
}
```

- **`args() -> Array<String>`** — the program's arguments, *excluding* the
  program name (argv[1..]). Empty array when none.
- **`readLine() -> Option<String>`** — one line from stdin, **without** its
  trailing newline (`\r\n` and `\n` both stripped, so Windows and POSIX pipes
  read identically). `None` at EOF. Repeated calls stream through stdin.
- **`readFile(path: String) -> Result<String, String>`** — the whole file as
  a UTF-8 String.
- **`writeFile(path: String, contents: String) -> Result<Bool, String>`** —
  create/truncate + write; `Ok(true)` on success.

All four are **effects** (free builtins, like `print`/`logger` — the
established taxonomy) and are **never spawn-safe** (join `SPAWN_FORBIDDEN`).
They are not constant (`where`/consteval never see them).

## The parity rule for errors (critical)

OS error text differs between backends (Rust `io::Error` vs libc `strerror`),
so **error payloads are canonical Vela wording, never OS text**:

- `readFile` → `Err("cannot read `<path>`")` — any reason (missing,
  permission, directory).
- `readFile` on invalid UTF-8 → `Err("`<path>` is not valid UTF-8")` (the
  native side reuses the existing UTF-8 validator DFA; the interpreter gets
  this from Rust naturally — the *wording* is what must match).
- `writeFile` → `Err("cannot write `<path>`")`.

Coarse on purpose: parity-exact today beats errno detail that diverges. Finer
error enums can come later behind the same canonical strings.

## Parity harness support

I/O examples need fixtures. Conventions (extending the harness, mechanism
precedent: `EXPECTED_CHECK_FAILURE`/`WASM_ONLY` were additions of the same
kind):

- **Stdin:** if `examples/<name>.stdin` exists, the harness pipes it into all
  three backends (interp, native, wasmtime run).
- **Files:** examples use paths relative to the repo's `examples/` directory;
  the harness sets the working directory to `examples/` for all three runs
  and passes `--dir .` to wasmtime (WASI preopens). `velac run`/native run
  inherit the cwd naturally.
- Written outputs go to a temp name and are cleaned up by the example itself
  or ignored via .gitignore — an example must remain re-runnable.

**Browser:** `wasi-min.js` grows *graceful degradation*, not file access:
`fd_read` on stdin returns EOF (so `readLine()` yields `None`),
`path_open` returns an error errno (so `readFile` yields the canonical
`Err`) — a module using input still loads and runs in a page, it just sees
an empty world. Real browser input remains the `extern` story (RFC-0012).

## Implementation shape

- **Interpreter:** Rust `std::env::args` / `Stdin::lock` lines / `fs` — with
  the canonical error mapping above.
- **Native + wasm (one IR):** C shim helpers with 64-bit-clean prototypes
  (the established pattern): `__vela_args_count/__vela_args_get`,
  `__vela_read_line` (returns malloc'd ptr or null at EOF, strips `\r?\n`),
  `__vela_read_file` / `__vela_write_file` (out-params for ptr+len; status
  codes 0/1/2 for ok/io-error/not-utf8 — the IR builds the canonical `Err`
  strings so wording lives in ONE place, the codegen). The shim's C `main`
  becomes `main(int argc, char** argv)` and stashes argv for `args()`.
  wasi-libc provides argv on WASI identically.
- **Checker/LSP:** builtin signatures in the call table + LSP hover/member
  tables where applicable.

## M2 — bytes (same RFC, second milestone)

- **`Array<UInt8>`** must work end-to-end (it type-checks today; M2 verifies
  and hardens the codegen element stride for i8 and adds paired tests +
  an example).
- **`readFileBytes(path: String) -> Result<Array<UInt8>, String>`** — binary
  read, same canonical error.
- **`s.bytes() -> Array<UInt8>`** (verify — a `bytes` builtin already exists;
  align it to return `Array<UInt8>` if it doesn't) and
  **`stringFromBytes(b: Array<UInt8>) -> Result<String, String>`** — UTF-8
  validated (`Err("bytes are not valid UTF-8")`), the inverse. Round-trip
  `stringFromBytes(s.bytes()) == Ok(s)` is a pinned test.
- If Array<UInt8> needs deep codegen surgery, M2 may land separately — M1
  must not block on it.

## Out of scope

Streaming/buffered readers, directories/metadata/deletion, sockets/network,
async I/O (waits on the async RFC), environment variables (trivial to add
later), binary stdin. Each is additive behind the same effect taxonomy.
