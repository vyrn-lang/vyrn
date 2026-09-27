# Contributing to Vyrn

Vyrn is an alpha. The language changes without a deprecation period, so a pull request may change behaviour, provided its tests and the docs change with it.

## Before you write code

- A bug: open an issue with the Bug form. The smallest program, the command, its output and its exit code are what make it fixable.
- A language, std or tooling change: open a Proposal first. Read [`docs/decisions.md`](docs/decisions.md) before you do; a proposal that reverses a decision says why the reason no longer holds.
- The design is in [`docs/`](docs/): [`language.md`](docs/language.md), [`std.md`](docs/std.md), [`memory.md`](docs/memory.md), [`compiler.md`](docs/compiler.md) and [`tooling.md`](docs/tooling.md). A change to what one of them describes updates it in the same pull request.

## Build and test

[`compiler/README.md`](compiler/README.md) has the build and test commands. You need a recent Rust toolchain and nothing else: no LLVM, no clang, no WASI sysroot.

## Where a change and its test go

| You change | Its test goes in |
|---|---|
| What a program prints | `examples/<name>.vyrn`, with its `.stdout`, `.stderr` and `.exit` in `examples/expected/`. Record them with `VYRN_FIXTURES=write cargo test --release -p vyrn-cli --test fixtures -- --ignored`, then read every line it wrote. |
| What the compiler refuses | `compiler/vyrn-cli/tests/refusals/<name>.vyrn` and a row in `compiler/vyrn-cli/tests/refusals.rs`. A refusal keeps its sentence and its line. |
| A leak or a double free | `compiler/vyrn-cli/tests/memory.rs`; the program must run clean under `VYRN_LEAK_CHECK=1`. |
| A std module | `test` blocks in the module itself. |
| The language server | `compiler/vyrn-lsp/tests/`. |

Write the test first and watch it fail. A bug fix names the invariant it restores, not only the symptom.

## Before you open a pull request

- Run the short gate list in [`AGENTS.md`](AGENTS.md#the-gate-list). If a gate writes a pin (a file under `compiler/vyrn-cli/tests/pins/`), rerun it with `VYRN_PIN=write` or `VYRN_WASM_MANIFEST=write`, read every line that moved, and commit it.
- Commit messages follow Conventional Commits: `type(scope): subject`, then a body that says why. [`AGENTS.md`](AGENTS.md#commits-and-pull-requests) lists the types and scopes.
- Fill in the pull request template. `Fixes #N` closes the issue on merge.
- Code and docs follow the writing rules in [`AGENTS.md`](AGENTS.md#writing-for-developers): plain ASCII, LF line endings, comments that say why and not what.

CI runs every gate on four platforms. A pull request merges with a merge commit once CI is green.

## License

Vyrn is dual-licensed under [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE). A contribution you submit is licensed under both, with no additional terms.
