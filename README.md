# Vyrn

[![CI](https://github.com/vyrn-lang/vyrn/actions/workflows/ci.yml/badge.svg)](https://github.com/vyrn-lang/vyrn/actions/workflows/ci.yml)

**Vyrn is a systems language with the expressiveness of TypeScript.** Types
carry the rules that make a value valid, not only its shape. Ownership is a
capability you declare on a parameter — `read`, `modify`, or `consume` — so
there is no garbage collector and no lifetime syntax. One program compiles to
three targets: an interpreter, a native binary, and a WebAssembly module. All
three produce the same bytes.

```vyrn
type Age = Int64 where value >= 18

type Ticket = { id: Int64, seats: Int64 }

// A parameter states the function's intent: read it, change it, or take it.
fn seatCount(t: read Ticket) -> Int64 { return t.seats }

fn addSeat(t: modify Ticket) { t.seats = t.seats + 1 }

fn redeem(t: consume Ticket) -> Int64 { return t.id }

// `Age?(n)` returns None when the rule fails, so untrusted input never traps.
fn admit(n: Int64) -> Int64 {
    return match Age?(n) {
        Some(a) => a,
        None => 0 - 1,
    }
}

fn main() -> Int64 {
    let mut t = Ticket { id: 7, seats: 1 }
    addSeat(t)
    print(seatCount(t)) // 2

    let a = Age(30) // proven valid at compile time - no runtime check
    print(a) // 30
    print(admit(25)) // 25
    print(admit(5)) // -1 - 5 is not an Age, and nothing aborts

    // let bad = Age(5)  // compile error: 5 does not satisfy `Age`
    return redeem(t) // t is consumed here; using it again is a compile error
}
```

Both commented-out lines are real compiler errors:

```
bad.vyrn:4:0: 5 does not satisfy `Age` (predicate `where value >= 18` is false)
uac.vyrn:8:0: `a` is used here but was already consumed by `redeem(..)` on line 7
  (a `consume` parameter takes ownership; the value can't be used afterward)
```

## Status

**Vyrn is an alpha.** Every release is a pre-release: the language changes
without a deprecation period, and the design record in [`rfcs/`](rfcs/) moves
ahead of the implementation. Do not build anything on it that you are not
willing to fix next month.

What is stable is the verification. Every example in [`examples/`](examples/)
that is meant to run runs under all three backends and must agree byte for byte,
including trap messages and exit codes. CI enforces that on every push. The
exceptions are listed, not implied: the examples that exist to be REFUSED are
pinned with the diagnostic they must produce (`EXPECTED_CHECK_FAILURE` in
`compiler/vyrn-cli/tests/common/mod.rs`), and one example is wasm-only
(`WASM_ONLY`). Both lists are part of the same harness — an example cannot leave
the comparison without appearing in one of them.

## Why Vyrn

**Validated types.** A `where` clause is part of the type. The compiler rejects
a provably-invalid constant, erases the check where it can prove validity, and
emits the check where it cannot. You cannot forget a check you never wrote.
See [`examples/validate.vyrn`](examples/validate.vyrn) and
[`examples/autovalidate.vyrn`](examples/autovalidate.vyrn).

**Ownership by declaration.** You write `read`, `modify`, `consume` or `share`
on a parameter. The compiler enforces moves and aliasing from that. There is no
lifetime syntax and no borrow annotations. Memory is reclaimed at a site the
compiler names — `vyrn why --memory <file>` reports, per binding, whether it is
reclaimed, how, and the reason when it is not. See
[`examples/consume.vyrn`](examples/consume.vyrn),
[`examples/modify.vyrn`](examples/modify.vyrn) and
[`examples/ownership.vyrn`](examples/ownership.vyrn).

**Three targets, one meaning.** The tree-walking interpreter is the reference
semantics. `vyrn build` emits textual LLVM IR and links it with `clang`.
`vyrn build --target wasm` emits the WebAssembly module directly — no LLVM, no
clang, no WASI sysroot. The parity harness scans `examples/`, runs each program
three ways, and compares stdout, stderr and exit code.

**No async.** Function suspension is not in the language. The host owns the
loop: the browser page, the HTTP server, or the runtime you write. See
[`examples/eventloop.vyrn`](examples/eventloop.vyrn) and
[`examples/server.vyrn`](examples/server.vyrn).

**Compile-time generators, not compiler features.** A `gen fn` is ordinary Vyrn
that runs at compile time and returns Vyrn source. An import target can be a
call to one. RPC, UI, i18n, OpenAPI and GraphQL are libraries built this way, in
[`std/`](std/) — none of them is a keyword. See
[`examples/gendemo.vyrn`](examples/gendemo.vyrn) and
`vyrn emit-gen <file>` to read the synthesized module.

**Failure is a value.** No null. `Option<T>`, `Result<T, E>`, exhaustive
`match`, and `?` propagation. See [`examples/option.vyrn`](examples/option.vyrn)
and [`examples/fallible.vyrn`](examples/fallible.vyrn).

## Feature tour

| Area | Where to look |
|------|---------------|
| Structural records, width subtyping, `Omit`/`Pick`/`Merge` | [`record.vyrn`](examples/record.vyrn), [`utility.vyrn`](examples/utility.vyrn) |
| Enums, exhaustive `match`, control flow | [`enum.vyrn`](examples/enum.vyrn), [`controlflow.vyrn`](examples/controlflow.vyrn) |
| Generics, monomorphization, protocols and bounds | [`generics.vyrn`](examples/generics.vyrn), [`protocol.vyrn`](examples/protocol.vyrn) |
| Function values and closures | [`lambdas.vyrn`](examples/lambdas.vyrn), [`closures2.vyrn`](examples/closures2.vyrn) |
| Arrays, maps, places, in-place element stores | [`arrays.vyrn`](examples/arrays.vyrn), [`mapdemo.vyrn`](examples/mapdemo.vyrn), [`placeorder.vyrn`](examples/placeorder.vyrn) |
| Strings, templates, regex, UTF-8 bytes | [`strings.vyrn`](examples/strings.vyrn), [`templates.vyrn`](examples/templates.vyrn), [`regex.vyrn`](examples/regex.vyrn) |
| Pull streams, linear and lazy | [`stream.vyrn`](examples/stream.vyrn), [`streamops.vyrn`](examples/streamops.vyrn) |
| Structured concurrency: `spawn` / `join` | [`concurrency.vyrn`](examples/concurrency.vyrn), [`parallel.vyrn`](examples/parallel.vyrn) |
| Portable SIMD | [`simd.vyrn`](examples/simd.vyrn), [`simdint.vyrn`](examples/simdint.vyrn) |
| Modules, namespaces, remote imports | [`modules.vyrn`](examples/modules.vyrn), [`namespace.vyrn`](examples/namespace.vyrn) |
| Reflection, JSON Schema in and out | [`reflection.vyrn`](examples/reflection.vyrn), [`jsonschema.vyrn`](examples/jsonschema.vyrn), [`schemaimport.vyrn`](examples/schemaimport.vyrn) |
| I/O, arguments, files, time, storage | [`input.vyrn`](examples/input.vyrn), [`args.vyrn`](examples/args.vyrn), [`files.vyrn`](examples/files.vyrn), [`clock.vyrn`](examples/clock.vyrn) |
| Compile-time i18n over finite string types | [`finitekeys.vyrn`](examples/finitekeys.vyrn), [`i18ndemo.vyrn`](examples/i18ndemo.vyrn) |
| Tests and benchmarks in the source file | [`testing.vyrn`](examples/testing.vyrn), [`benching.vyrn`](examples/benching.vyrn) |

The standard library is 32 modules in [`std/`](std/), written in Vyrn. Generated
API docs are committed under [`docs/api/`](docs/api/), and CI fails if they drift
from the source.

## The web and full-stack story

`.vyx` single-file components compile to Vyrn through the `std/vyx` generator —
a `<script>` block of ordinary Vyrn and a `<template>` block of markup. See
[`examples/vyxcomp/`](examples/vyxcomp/).

`vyrn dev` builds the client to wasm and serves the server root, the static
files and the runtimes in one process. `vyrn serve` runs a plain
`fn handle(req: Request) -> Response` over an HTTP/1.1 host, with
`--workers N` for parallel handling. `vyrn routes` prints the resolved wire
table and where each route came from.

Three full applications are in the tree:
[`examples/fullstack/`](examples/fullstack/) (the smallest one),
[`examples/shelf/`](examples/shelf/) and [`examples/bin/`](examples/bin/) (a
pastebin that survives restarts).

[`web/`](web/) holds the browser demos and the host-side runtimes: `wasi-min.js`
is a dependency-free WASI preview1 shim, and `vyrn-dom.js`, `vyrn-nav.js`,
`vyrn-rpc.js` and `vyrn-query.js` are the client halves of the UI and RPC
libraries.

## Tooling

```
vyrn run | check | fix | build | test | bench | serve | dev | fmt | doc
vyrn why <file> | why --contract <file> | why --memory <file>
vyrn routes | emit-ir | emit-gen
vyrn new <name> | add <specifier> | update | vendor | deps
```

- `vyrn fmt` is a canonical formatter. `--check` is the CI gate.
- `vyrn test` runs `test` blocks written in the source file next to the code.
- `vyrn bench` runs `bench` blocks, with `--json`, `--compare` and a
  deterministic `--check` mode.
- `vyrn doc` generates Markdown API docs from `///` comments.
- Modules resolve from `vyrn.json`. Remote imports (`github:`, `gist:`,
  `https:`) are pinned in `vyrn.lock` and cached by sha256 under `~/.vyrn`.
  `--offline` refuses to fetch.
- `vyrn-lsp` is a synchronous language server: diagnostics, hover,
  go-to-definition and completion, across linked files. The VS Code extension
  is in [`editor/vscode/`](editor/vscode/).

## Getting started

### Install

Linux and macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.ps1 | iex
```

Either script picks the archive for your machine from the newest release,
verifies it against that release's `SHA256SUMS`, and unpacks it under `~/.vyrn`
(`%USERPROFILE%\.vyrn`) with the binary at `~/.vyrn/bin/vyrn`. A checksum it
cannot match is a hard failure: nothing is installed. Then:

```bash
vyrn run examples/fib.vyrn
```

Published builds are **Linux x86_64, Linux arm64, macOS arm64 and Windows
x86_64**. On any other platform, build from source. To install a specific tag rather than the
newest, set `VYRN_VERSION=v0.1.0-alpha.1`; to install elsewhere, set
`VYRN_INSTALL_DIR`. You can also download the archive and `SHA256SUMS` from the
[releases page](https://github.com/vyrn-lang/vyrn/releases) and check it by
hand.

**What needs what.** `vyrn run`, `check`, `test`, `fmt`, `doc` and
`build --target wasm` need nothing beyond the archive. **`vyrn build` — a native
binary — needs `clang` on `PATH`** (or `$CLANG`); it emits textual LLVM IR and
links it. Running the three-way parity harness also needs a `wasmtime` binary,
through `$VYRN_WASMTIME`.

### Build from source

You need a recent Rust toolchain. No LLVM, no clang and no wasi sysroot are
needed to build or test the compiler.

```bash
git clone https://github.com/vyrn-lang/vyrn.git
cd vyrn/compiler
cargo build --release -p vyrn-cli
cargo run --release -p vyrn-cli -- run ../examples/fib.vyrn
```

A native binary needs `clang` on `PATH` (or `$CLANG`):

```bash
cargo run -p vyrn-cli -- build ../examples/fib.vyrn -o fib.exe
```

A wasm module needs nothing extra:

```bash
cargo run -p vyrn-cli -- build ../examples/fib.vyrn --target wasm -o fib.wasm
```

The built binary finds `std/` and `web/` by walking up from its own path, so it
works in place inside a clone. See the `parity` job in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) for the exact wasmtime
and wasi-sdk versions CI uses.

[`compiler/README.md`](compiler/README.md) has the detailed build notes, the
crate map, and how to build the excluded crates (`vyrn-lsp`, `vyrn-genwasm`).
[`docs/releasing.md`](docs/releasing.md) is how a release is cut.

## Repository layout

```
lang/
├── rfcs/         the design record, numbered from RFC-0001; rfcs/README.md indexes them
├── compiler/     the Rust workspace
│   ├── vyrn-frontend/  lexer, parser, checker, move check, interpreter, diagnostics
│   ├── vyrn-codegen/   textual LLVM IR emitter, and the direct wasm encoder
│   ├── vyrn-cli/       the `vyrn` driver
│   ├── vyrn-lsp/       language server (excluded from the workspace)
│   └── vyrn-genwasm/   runs `gen fn` generators as compiled wasm (excluded)
├── std/          the standard library, written in Vyrn
├── examples/     single-file programs, plus multi-file apps in subdirectories
├── web/          browser demos and the JavaScript host runtimes
├── docs/api/     generated std API docs, checked for drift by CI
├── editor/       the VS Code extension
├── bench/        the benchmark baseline
├── tools/        local toolchain downloads (not tracked)
└── ROADMAP.md    what ships today and what is next
```

## What CI proves

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs four jobs. The first
is a matrix over the four platforms releases ship — Linux x86_64, Linux arm64,
macOS arm64, Windows x86_64 — so every published binary is built by a machine
whose tests ran. The other three are Linux, where the toolchain lives.

1. **tests (workspace + LSP)**, on all four platforms — `cargo fmt --check` over
   all three manifests, the workspace test suite, the LSP suite, the browser
   runtime tests, the `docs/api/` drift gate, the install scripts (they install,
   and they refuse an archive whose checksum does not match), and
   `vyrn bench --check` over every benchmark.
2. **three-way parity** — the interpreter, the clang-linked native binary and
   the wasm module must agree on every example. The known-divergent list is
   empty and must stay empty. This job also runs the codegen integration tests
   that need clang, a wasi sysroot and wasmtime, including the one that checks
   the layout engine against clang's own answers on wasm32.
3. **cross-engine generation** — every `gen fn` must produce identical source
   under the interpreter and under wasm.
4. **benchmarks**, on pushes to `main` only — every bench still builds and runs.
   The regression half is not live: `bench/baseline.json` is a placeholder, so
   `--compare` reports every bench as new. `ci.yml` says exactly what that gate
   does and does not prove.

[`.github/workflows/site.yml`](.github/workflows/site.yml) builds the website and
runs its tests on pull requests too, and
[`.github/workflows/release.yml`](.github/workflows/release.yml) refuses to
publish a tag whose commit has no successful CI run.

## The design record

[`rfcs/`](rfcs/) is where decisions are made and argued. When the implementation
and an RFC disagree, one of them is a bug. Start with
[RFC-0001 Vision](rfcs/RFC-0001-vision.md),
[RFC-0003 Validated Types](rfcs/RFC-0003-validated-types.md) and
[RFC-0004 Capabilities & Memory](rfcs/RFC-0004-capabilities-and-memory.md).
[`rfcs/README.md`](rfcs/README.md) indexes them, with the status each one
carries.

## Not in v1

Higher-kinded types, dependent types, macros, class inheritance, metaclasses,
and `async`/`await`. See
[RFC-0001 §Non-goals](rfcs/RFC-0001-vision.md).

## License

Vyrn is licensed under either of [MIT](LICENSE-MIT) or
[Apache License 2.0](LICENSE-APACHE), at your option. That is the pair Rust
uses, and it is chosen for the same reason.

The choice reaches your programs, not just the compiler's source. `vyrn build`
compiles a small C runtime shim into every native binary, and every `std/`
module a program imports is compiled in too. Both are under the licence above,
so shipping a Vyrn program costs you attribution and nothing else. A copyleft
licence here would have reached the same code and cost far more.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you shall be dual licensed as above, without any
additional terms or conditions.
