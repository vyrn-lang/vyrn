<div align="center">

<a href="https://vyrn-lang.github.io/vyrn/">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/vyrn-logo-dark.svg">
    <img alt="Vyrn" src=".github/assets/vyrn-logo-light.svg" width="312">
  </picture>
</a>

### A systems language with the expressiveness of TypeScript

[![CI](https://github.com/vyrn-lang/vyrn/actions/workflows/ci.yml/badge.svg)](https://github.com/vyrn-lang/vyrn/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/vyrn-lang/vyrn?include_prereleases&label=release&color=1c7f9c)](https://github.com/vyrn-lang/vyrn/releases)
[![Docs](https://img.shields.io/badge/docs-vyrn--lang.github.io-22b8d4)](https://vyrn-lang.github.io/vyrn/)
[![License](https://img.shields.io/badge/license-MIT%20or%20Apache--2.0-123258)](#license)

[Website](https://vyrn-lang.github.io/vyrn/) &middot;
[Install](#install) &middot;
[Examples](examples/) &middot;
[Standard library](docs/api/) &middot;
[Design record](rfcs/)

</div>

Vyrn is a compiled language with no garbage collector and no lifetime syntax. A type carries the rules that make a value valid, not only its shape. Ownership is a capability you declare on a parameter: `read`, `modify` or `consume`. One program compiles to one WebAssembly module, and that module becomes the native binary. Both print the same bytes.

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

    let a = Age(30) // proven valid at compile time, no runtime check
    print(a) // 30
    print(admit(25)) // 25
    print(admit(5)) // -1: 5 is not an Age, and nothing aborts

    // let bad = Age(5)  // compile error: 5 does not satisfy `Age`
    return redeem(t) // t is consumed here; using it again is a compile error
}
```

The two commented-out lines are real compiler errors:

```
bad.vyrn:4:0: 5 does not satisfy `Age` (predicate `where value >= 18` is false)
uac.vyrn:8:0: `a` is used here but was already consumed by `redeem(..)` on line 7
  (a `consume` parameter takes ownership; the value can't be used afterward)
```

## Getting started

Linux and macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/vyrn-lang/vyrn/main/install.ps1 | iex
```

Then run a program:

```bash
vyrn run examples/fib.vyrn
```

[Install](#install) lists the platforms, how to pick a version, and how to build from source.

## Features

- **Validated types.** A `where` clause is part of the type. The compiler rejects an invalid constant, removes the check where it proves the value valid, and emits it where it cannot. ([validate](examples/validate.vyrn), [autovalidate](examples/autovalidate.vyrn))
- **Ownership by declaration.** `read`, `modify`, `consume` and `share` on a parameter drive moves and aliasing. `vyrn why --memory <file>` says where each binding is freed and why. ([consume](examples/consume.vyrn), [ownership](examples/ownership.vyrn))
- **One module, two ways to run it.** `vyrn build --target wasm` emits WebAssembly directly, with no LLVM and no clang. `vyrn build` turns the same module into a native executable, and CI checks that both print the same output.
- **Generators, not compiler features.** A `gen fn` is ordinary Vyrn that runs at compile time and returns source. RPC, UI, i18n, OpenAPI and GraphQL are libraries in [`std/`](std/), not keywords. ([gendemo](examples/gendemo.vyrn))
- **Failure is a value.** No null: `Option<T>`, `Result<T, E>`, exhaustive `match` and `?`. ([option](examples/option.vyrn), [fallible](examples/fallible.vyrn))
- **No async.** The host owns the event loop: the browser page, the HTTP server, or a runtime you write. ([eventloop](examples/eventloop.vyrn), [server](examples/server.vyrn))
- **Tools in the box.** A formatter, a test runner and a benchmark runner for blocks in the source file, a doc generator, a package manager with a lock file, and a language server with a VS Code extension.

## Examples

Run a program, or check it without running it:

```bash
vyrn run examples/templates.vyrn
vyrn check examples/ownership.vyrn
```

Build a WebAssembly module or a native binary:

```bash
vyrn build examples/fib.vyrn --target wasm -o fib.wasm
vyrn build examples/fib.vyrn -o fib
```

Run the tests and benchmarks written next to the code:

```bash
vyrn test examples/testing.vyrn
vyrn bench examples/benching.vyrn
```

Serve an HTTP handler, or a full-stack app with a wasm client:

```bash
vyrn serve examples/server.vyrn
cd examples/fullstack && vyrn dev
```

Read what a generator wrote, or why memory is freed where it is:

```bash
vyrn emit-gen examples/gendemo.vyrn
vyrn why --memory examples/ownership.vyrn
```

### A tour by topic

| Area | Examples |
|------|----------|
| Records, width subtyping, `Omit` / `Pick` / `Merge` | [record](examples/record.vyrn), [utility](examples/utility.vyrn) |
| Enums, exhaustive `match`, control flow | [enum](examples/enum.vyrn), [controlflow](examples/controlflow.vyrn) |
| Generics, protocols and bounds | [generics](examples/generics.vyrn), [protocol](examples/protocol.vyrn) |
| Function values and closures | [lambdas](examples/lambdas.vyrn), [closures2](examples/closures2.vyrn) |
| Arrays, maps, in-place element stores | [arrays](examples/arrays.vyrn), [mapdemo](examples/mapdemo.vyrn), [placeorder](examples/placeorder.vyrn) |
| Strings, templates, regex, UTF-8 | [strings](examples/strings.vyrn), [templates](examples/templates.vyrn), [regex](examples/regex.vyrn) |
| Linear pull streams | [stream](examples/stream.vyrn), [streamops](examples/streamops.vyrn) |
| Portable SIMD | [simd](examples/simd.vyrn), [simdint](examples/simdint.vyrn) |
| Modules, namespaces, remote imports | [modules](examples/modules.vyrn), [namespace](examples/namespace.vyrn) |
| Reflection, JSON Schema in and out | [reflection](examples/reflection.vyrn), [jsonschema](examples/jsonschema.vyrn), [schemaimport](examples/schemaimport.vyrn) |
| I/O, arguments, files, time | [input](examples/input.vyrn), [args](examples/args.vyrn), [files](examples/files.vyrn), [clock](examples/clock.vyrn) |
| Compile-time i18n | [finitekeys](examples/finitekeys.vyrn), [i18ndemo](examples/i18ndemo.vyrn) |
| Web components and full-stack apps | [vyxcomp](examples/vyxcomp/), [fullstack](examples/fullstack/), [shelf](examples/shelf/), [bin](examples/bin/) |

## Install

The install scripts above pick the archive for your machine from the newest release, verify it against the release's `SHA256SUMS`, and unpack it under `~/.vyrn`. A checksum that does not match installs nothing.

- **Platforms:** Linux x86_64, Linux arm64, macOS arm64 and Windows x86_64. On any other platform, build from source.
- **Versions:** set `VYRN_VERSION=v0.1.0-alpha.1` for a specific tag, and `VYRN_INSTALL_DIR` for another location. The [releases page](https://github.com/vyrn-lang/vyrn/releases) has every archive and its checksums.
- **What needs what:** `run`, `check`, `test`, `fmt`, `doc` and `build --target wasm` need only the archive. A native `vyrn build` also needs `clang` on `PATH`, plus wabt's `wasm2c` and simde, which `vyrn update --locked` fetches in a clone of this repository.

### Build from source

You need a recent Rust toolchain. Building and testing the compiler needs no LLVM, no clang and no WASI sysroot.

```bash
git clone https://github.com/vyrn-lang/vyrn.git
cd vyrn/compiler
cargo build --release -p vyrn-cli
cargo run --release -p vyrn-cli -- run ../examples/fib.vyrn
```

The binary finds `std/` and `web/` by walking up from its own path, so it works in place inside a clone. [`compiler/README.md`](compiler/README.md) has the crate map and the build notes for `vyrn-lsp` and `vyrn-genwasm`. The pinned toolchain versions are in [`vyrn.json`](vyrn.json), with every artifact's sha256 in [`vyrn.lock`](vyrn.lock). [`docs/releasing.md`](docs/releasing.md) says how a release is cut.

## Status

**Vyrn is an alpha.** Every release is a pre-release, and the language changes without a deprecation period. Do not build anything on it that you are not willing to fix next month.

The verification is stable. Every example that is meant to run runs as wasm and as a native binary, and both must agree byte for byte, including trap messages and exit codes. The examples that exist to be refused are pinned with the error they must produce. CI runs the tests on the four release platforms, the judgments over every program in the repository, and a check that each program compiles to the same wasm bytes on every platform. [`ci.yml`](.github/workflows/ci.yml) lists every job.

## Learn more

- [`rfcs/`](rfcs/) is the design record, where decisions are made and argued. Start with [RFC-0001 Vision](rfcs/RFC-0001-vision.md), [RFC-0003 Validated Types](rfcs/RFC-0003-validated-types.md) and [RFC-0004 Capabilities and Memory](rfcs/RFC-0004-capabilities-and-memory.md).
- [`docs/api/`](docs/api/) is the generated reference for the standard library, and CI fails if it drifts from [`std/`](std/).
- [`ROADMAP.md`](ROADMAP.md) says what ships today and what is next.
- [`editor/vscode/`](editor/vscode/) is the VS Code extension. Every release publishes it as a `.vsix`.
- Not in v1: higher-kinded types, dependent types, macros, class inheritance and `async`/`await` ([RFC-0001, Non-goals](rfcs/RFC-0001-vision.md)).

## License

Vyrn is licensed under either [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE), at your option. The licence covers the C runtime shim and the `std/` modules compiled into your programs, so shipping a Vyrn program costs attribution and nothing else.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be dual licensed as above, without any additional terms or conditions.
