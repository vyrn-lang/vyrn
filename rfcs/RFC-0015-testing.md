# RFC-0015 — Testing: `test` Blocks, `assert`, `velac test`

- **Status:** Draft — approved for implementation
- **Depends on:** RFC-0006 (diagnostics), RFC-0010 (modules — test files may
  import), the editor CodeLens (run affordance precedent)

> **Motivation.** Vela has no test construct: the compiler's own 500+ Rust
> tests guard the language, but a Vela *user* has nowhere to put one. The
> editor's Run CodeLens stops at `fn main`. A test story this small — one
> declaration form, one builtin, one command — buys the whole ecosystem
> habit early.

---

## Surface

```vela
import { clamp } from "std/math"

test "clamp holds the bounds" {
    assert(clamp(5, 0, 10) == 5)
    assert(clamp(-3, 0, 10) == 0)
    assertEq(clamp(99, 0, 10), 10)
}

test "empty pops to None" {
    let mut xs: Array<Int64> = []
    assert(match xs.pop() { Some(v) => false, None => true })
}
```

- **`test "name" { body }`** — a top-level declaration: a string name (must
  be unique per file) and a block body (checked exactly like a Unit function
  body: locals, effects like `print` allowed, spawn rules apply). Root and
  imported modules may declare tests; `velac test <file>` runs the **root
  file's** tests only (a library's tests run when *it* is the argument).
- **`assert(cond: Bool)`** — traps the current test with
  `assertion failed at line N` when false. Usable **only inside a `test`
  body** (an assert in ordinary code is a checker error pointing at
  validated types / `Result` as the production tools).
- **`assertEq(a, b)`** — both sides the same equatable type; failure message
  `assertion failed at line N: <a> != <b>` using the canonical `toString`
  rendering (parity-identical by construction).

## `velac test [file]`

- Runs every test block of the root file **in declaration order** under the
  interpreter (the reference semantics — fast, no clang needed):

  ```
  test "clamp holds the bounds" ... ok
  test "empty pops to None" ... ok

  2 passed, 0 failed
  ```

- A failing assert (or any runtime trap inside the body) marks that test
  FAILED with the trap message, **continues to the next test**, and the
  process exits 1 if any failed. Test stdout (`print`) passes through.
- `velac test <file> --name "<substring>"` filters by name.
- Manifest-aware like the other commands (`velac test` with no file uses
  `vela.json`'s `main`).

Native/wasm test execution is deferred: program *semantics* are already
three-way parity-gated; `velac test` is a dev loop tool. (The declaration
must still PARSE and CHECK identically everywhere — see below.)

## Compilation model

- `velac run` / `build` / `emit-ir` **ignore** test blocks entirely (checked,
  then stripped before interp/codegen — a shipped binary contains no tests;
  the string pool and regex collection must not collect from them either).
- A file consisting only of tests (+ imports) needs no `main` (extends the
  library-module rule: exports OR tests ⇒ no `main` required).
- Checker: test bodies are checked as Unit-returning function bodies with a
  synthetic name (`test@<index>` — unspellable, the `@` precedent), so every
  existing analysis (movecheck, ownership, spawn purity) applies unchanged.

## Editor

- Grammar: `test` as a contextual starter (identifier elsewhere — same
  treatment as `extern`); the name string is a plain string scope.
- CodeLens: "▶ Run test" above each `test` block →
  `velac test <file> --name "<name>"`; "▶ Run all tests" above the first.
- LSP: test blocks in the symbol index / outline (kind Method → "test" detail
  `test "name"`), so the outline shows the test list.
- Snippet: `test`.

## Parity note

`examples/` gains a `testing.vela`… **no** — examples are runnable programs;
tests live next to code in real projects. Instead: `velac new` scaffolds a
`src/main.test.vela`? Also no — v1 keeps zero conventions: tests can live in
ANY .vela file. The parity corpus is unaffected (test blocks are stripped
from run/build; an example may contain a test block and stay byte-identical
across backends precisely because of that). One example
(`examples/testing.vela`) demonstrates the form: it has BOTH tests and a
`main` (so it stays a normal parity citizen), with a doc comment showing the
`velac test` output.

## Out of scope

Native-backend test execution, `#[should_panic]`-style expected traps
(write `match` on `Result` instead), fixtures/setup-teardown, test
parallelism, coverage, snapshot testing, doc-tests from `///` examples
(attractive later — the `///` markdown already exists).
