# RFC-0111 — A Program Can Write Bytes

- **Status:** Accepted. Implemented in this branch.
- **Closes:** the `p-mandelbrot` / `p-binout` gap named in
  [RFC-0104](RFC-0104-a-benchmark-is-a-claim-about-a-gap.md):408-416.
- **Evidence:** [rfcs/census/blocked-byte-sink.md](census/blocked-byte-sink.md).

## The gap in one sentence

Every content-carrying output builtin takes a `String`, a Vyrn `String` cannot
hold a NUL or invalid UTF-8, and a binary file is neither — so a program can
compute bytes it has no way to emit.

## What it looked like

`mandelbrot` computes a PBM P4 image. Its committed fixture,
`rfcs/bench-0104/mandelbrot-200.expected`, begins:

```
50 34 0a 32 30 30 20 32 30 30 0a 00 …
   P  4 \n  2  0  0     2  0  0 \n NUL
```

The header is text. The twelfth byte is a NUL, and every pixel row after it is
arbitrary bytes. `print` and `writeFile` take a `String`; `stringFromBytes`
refuses a NUL before it even reaches the UTF-8 check (RFC-0014 §NUL rule). So
the header could be written and the picture could not.

The fixture stayed committed with no program beside it. That is what a named
gap looks like in a corpus, and this closes it.

## What is added

Two builtins, beside `writeFile` and `readFileBytes`:

```vyrn
writeFileBytes(path: String, bytes: Array<UInt8>) -> Result<Bool, String>
writeStdout(bytes: Array<UInt8>) -> Unit
```

`writeFileBytes` mirrors `writeFile` exactly: create, truncate, write all,
`Ok(true)` or the canonical `@.io.writeerr` wording. `writeStdout` mirrors
`print` — no result, because a write to a closed stdout is not a condition a
Vyrn program can act on, and `print` has answered nothing since RFC-0008.

Neither takes a `String`, and neither validates. That is the point: these are
the two operations whose whole job is to move bytes that are not text.

## Why builtins and not a `std/` module

The standing rule is that there are no different implementations for different
backends and the standard library is not hard-implemented inside a backend.
This obeys it, and the distinction is worth stating exactly because it is easy
to read the wrong way.

A `std/` FUNCTION with a Rust body would break the rule. `writeFileBytes` is
not that. It is a BUILTIN — the same class as `readFileBytes`, `writeFile`,
`readLine` and `args` — and that class has always been one canonical behaviour
with a per-engine body, because reaching the operating system is the one thing
Vyrn source cannot do for itself. RFC-0014 built that class; this adds two rows
to it.

The test of the rule is whether `std/` could have written it. `std/storage.vyrn`
wraps `writeFile` in Vyrn and gains real behaviour doing so. Nothing in Vyrn can
express "hand these bytes to fd 1". So the wrapper belongs in `std/` and the
syscall belongs in the builtin table, which is where every other syscall is.

A PBM encoder in Rust WOULD break the rule. There is none. `examples/bench/`
holds the encoder, in Vyrn, and it is the same source on all three engines.

## The two traps

Both were found by the census before any code was written, and both are
transport-level: the payload is identical by construction, because one array of
bytes is handed to each engine.

### Binary mode on Windows

A native Windows build writes `\r\n` where the interpreter and wasm write `\n`,
because C stdio opens stdout in text mode. The bench harness normalises line
endings before diffing, which is correct for text and destroys binary: a `0x0A`
inside a packed pixel row would become `0x0D 0x0A` on native-Windows only, and
no normalisation can undo that safely, because it cannot tell which `0x0D 0x0A`
was a real pixel pair.

`__vyrn_write_file` already opens `"wb"`, so files were never at risk.
`writeStdout` is, and the shim now sets `stdout` to binary mode for the duration
of the write and restores it after. Restoring matters: a program that calls
`writeStdout` and then `print` must still get the platform's own newline
behaviour from `print`.

### The browser decodes before anyone sees it

`wasi-min.js` decoded every `fd_write` chunk as UTF-8 text before handing it to
a consumer. A packed row is not UTF-8, so the browser would show corruption for
bytes the other two engines wrote intact. The shim now hands out the raw
`Uint8Array` alongside the decoded text, and only decodes for the text path.
This is an API-visible shape change to the chunk hook and is documented at its
definition.

## What is NOT added

- **No `Stream<UInt8>` sink.** Design B in the census. The stream combinators
  are a pull model built for sources; a sink inverts them, and flush ordering
  differs per engine today — which is why `close` is already spawn-forbidden.
  It adds machinery this gap does not need. If a use appears that streams
  rather than writes a whole buffer, price it then.
- **No general output handle.** Design C. That is a redesign of output, not an
  unblocking of mandelbrot, and it would put buffering mode, newline mode and
  encoding into per-handle state that three backends must agree on for every
  combination.
- **No `readStdinBytes`.** Nothing needs it. `readLine` and `readFileBytes`
  cover every input in the corpus.

## Parity, and the thing parity cannot see

`examples/mandelbrot.vyrn` is in the three-way corpus, so all 40 parity tests
run over it. That is not enough, and finding out why is the most useful thing
this work produced.

**The parity harness is blind to exactly this class of bug.** It compares runs
through `common::norm`, which is:

```rust
String::from_utf8_lossy(bytes).replace("
", "
")
```

Both halves destroy a binary artifact. `from_utf8_lossy` replaces every invalid
sequence with U+FFFD, and the `replace` collapses the CRLF distinction that
text-mode stdio creates. That is the right normalisation for text — a native
Windows build genuinely does end `print`'s lines differently, and that is not a
parity failure — and it is the wrong one for bytes.

Measured, not reasoned about: removing the binary-mode guard from
`__vyrn_write_stdout` makes the native `mandelbrot` write **5013 bytes instead
of 5011**, and `examples_interp_native_parity` still passes.

So binary output has its own gate, `compiler/vyrn-cli/tests/bytesink.rs`, which
compares BYTES with no normalisation anywhere in it. Restoring the guard is what
makes it pass; removing it again fails with the byte counts named. Four more
tests there pin the hostile bytes — a NUL, a lone `0x0A`, a `0x0D 0x0A` pair, an
invalid UTF-8 lead byte — through both sinks, and pin that a `writeStdout`
leaves `print`'s own newline behaviour untouched afterwards.

The general lesson is worth stating on its own: **a normalising comparison
cannot police a format whose content the normalisation alters.** Any future
builtin that moves bytes rather than text needs a gate outside the parity
harness, for the same reason.

Both are in `RESERVED`, `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN`.

`writeFileBytes` carries `Capability::Fs` in the RFC-0103 floor, so a target
with no filesystem refuses it at compile time rather than at run time.
`writeStdout` carries NONE, and deliberately: the floor has four capabilities
and stdout is not among them, because every target can write it. `print` is
absent from that table for the same reason, and a second output builtin is not
the occasion to invent a fifth capability.
