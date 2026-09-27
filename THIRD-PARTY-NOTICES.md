# Third-party notices

Vyrn itself is under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE). This
file lists the third-party code committed to this repository, whose own notices
must travel with it. It is short on purpose: there is one entry.

## Björn Höhrmann's UTF-8 decoder DFA

`utf8d_table()` in `compiler/vyrn-codegen/src/lib.rs` reproduces the transition
table from "Flexible and Economical UTF-8 Decoder". The table is emitted into
every artifact `vyrn build` produces — `@__vyrn_utf8d` in the textual LLVM IR,
and the same bytes in a data segment in the direct wasm backend — so this
notice travels with those artifacts too.

    Copyright (c) 2008-2009 Bjoern Hoehrmann <bjoern@hoehrmann.de>
    See http://bjoern.hoehrmann.de/utf-8/decoder/dfa/ for details.

    Permission is hereby granted, free of charge, to any person obtaining a copy
    of this software and associated documentation files (the "Software"), to deal
    in the Software without restriction, including without limitation the rights
    to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    copies of the Software, and to permit persons to whom the Software is
    furnished to do so, subject to the following conditions:

    The above copyright notice and this permission notice shall be included in all
    copies or substantial portions of the Software.

    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
    SOFTWARE.

## Not third party, though the algorithm has a name

These are independent implementations written for this repository, from published
descriptions of algorithms their authors put in the public domain or a standard.
They are credited where they live and need no notice here: FNV-1a in
`std/hash.vyrn`, SplitMix64 in `std/random.vyrn` and `web/wasi-min.js`, and
SHA-256 in `compiler/vyrn-frontend/src/hash.rs`.

## Dependencies, which are not committed here

The Rust crates the compiler builds against are fetched by Cargo, not vendored,
so their sources carry their own notices. Every one is permissive:
`wasm-encoder` and `wasmtime` (Apache-2.0 WITH LLVM-exception, Bytecode
Alliance), `lsp-server`, `serde` and `serde_json` (MIT OR Apache-2.0),
`lsp-types` (MIT). The VS Code extension bundles `vscode-languageclient` and its
transitive dependencies (MIT, Microsoft).

A native binary `vyrn build` produces also links the C library and compiler
builtins from whatever toolchain the user supplies — wasi-libc and compiler-rt
for `--target wasm`. Those come from the user's own sysroot, not from here.
