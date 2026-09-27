# The Vyrn compiler

The Rust workspace that turns Vyrn source into one WebAssembly module. `vyrn
run` runs that module in an embedded wasmtime; `vyrn build` turns the same
module into a native binary through wasm2c and clang.

`docs/compiler.md` is the guide for a person who changes the compiler: the
pipeline, what each crate owns, the kernel, the emitter and what each gate
proves. `docs/memory.md` describes the ownership model. `AGENTS.md` holds the
process and the exact gate commands.

## Build and test

The workspace builds and tests with no LLVM, no clang and no wasi sysroot.

```sh
cd compiler
cargo build --release -p vyrn-cli     # target/release/vyrn
cargo nextest run --release --workspace
```

`vyrn-lsp`, `vyrn-genwasm` and `vyrn-play` are outside the workspace. Test the
first two with `--manifest-path`:

```sh
cargo test --manifest-path vyrn-lsp/Cargo.toml
cargo test --manifest-path vyrn-genwasm/Cargo.toml
```

Build `vyrn-play` from its own directory, so its `.cargo/config.toml` sets the
linker stack size:

```sh
cd vyrn-play && cargo build --release --target wasm32-unknown-unknown
```

## Try it

```sh
target/release/vyrn run   ../examples/fib.vyrn        # prints 55
target/release/vyrn check ../examples/fib.vyrn        # ok
target/release/vyrn build ../examples/fib.vyrn -o fib # a native binary
target/release/vyrn emit-wat ../examples/fib.vyrn     # the module as WAT
target/release/vyrn emit-gen ../examples/gendemo.vyrn # what a generator import emits
```

`vyrn build` finds clang through `$CLANG`, then `PATH`, then the default Windows
install. It finds wasm2c and simde through `$VYRN_WASM2C` and `$VYRN_SIMDE`,
then the pin in `vyrn.lock`, then a `tools/` directory above the source or the
compiler.
