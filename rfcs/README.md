# Vyrn RFCs

This directory is the **design record** for Vyrn. It is the north star: when the
implementation and an RFC disagree, that is a bug in one of them, and the RFC is
where the argument gets settled.

The RFCs capture *decisions and their rationale*, not just syntax. Each one lists
its open questions explicitly so we know what is settled and what still needs a
prototype to answer.

## Reading order

| RFC | Title | What it settles |
|-----|-------|-----------------|
| [0001](RFC-0001-vision.md) | Vision & Principles | Mission, audience, non-goals, the philosophy every other RFC answers to |
| [0002](RFC-0002-type-system.md) | Type System | Structural typing, utility types, generics, pattern matching, no casts |
| [0003](RFC-0003-validated-types.md) | Validated Types | Refinement types, compile-time vs runtime validation, the signature feature |
| [0004](RFC-0004-capabilities-and-memory.md) | Capabilities & Memory | read/modify/consume/share model, memory strategy, the open research |
| [0005](RFC-0005-error-handling.md) | Errors & Null | `Result`, `Option`, `?` propagation, no null |
| [0006](RFC-0006-diagnostics.md) | Diagnostics | The compiler-as-teacher output format |
| [0007](RFC-0007-string-templates.md) | String Templates | `\{ }` interpolation, tagged templates, injection-safe `sql`/`latex` |
| [0008](RFC-0008-logging.md) | Logging | SLF4J-style facade + Logback-style backend; retires `print` |
| [0009](RFC-0009-error-model.md) | Error Model | `Issue` + accumulating `Validation<T>` with i18n keys |
| [0010](RFC-0010-modules.md) | Modules | `import`/`export`, loader/linker, JSON-Schema type imports, `vyrn.json` manifest, lock-pinned reproducible remotes |
| [0011](RFC-0011-array-mutation.md) | In-Place Array Mutation | `a[i] = v` / `pop` / `swapRemove`, and `a[i].field = v` write-through |
| [0012](RFC-0012-js-interop.md) | JS Interop (`extern`) | Host imports/exports on wasm; the first feature whose behavior differs per backend |
| [0013](RFC-0013-module-state-event-loop.md) | Module State & Event Loop | Top-level `let` state, host-driven handler dispatch |
| [0014](RFC-0014-input-io.md) | Input I/O | `args()`, `readLine`, file + byte builtins, canonical I/O errors |
| [0015](RFC-0015-testing.md) | Testing | `test` blocks, `assert`/`assertEq`, `vyrn test` |
| [0016](RFC-0016-server.md) | The Server | `vyrn serve`, `Request`/`Response`, the async decision |
| [0017](RFC-0017-formatter.md) | Canonical Formatter | `vyrn fmt`: one style, no options |
| [0018](RFC-0018-json-codec.md) | The JSON Codec | `toJson`/`fromJson`: canonical encode, decode into `Validation<T>` with accumulated `Issue`s |
| [0024](RFC-0024-enums-on-the-wire.md) | Payload Enums on the Wire | **Implemented**: the codec's v2 — payload enums & `Result<T, E>` cross the wire externally tagged (`"Unit"` / `{"Circle":5}` / `{"Rect":[2,3]}`), with a matching `oneOf` jsonSchema (emit↔import byte-exact) and a `Result`-returning RPC procedure |
| [0019](RFC-0019-rpc.md) | Typed RPC | **Implemented**: the codec as an RPC layer, end-to-end typed calls — a library (`std/rpc`) over RFC-0021's `moduleInterface`, with `vyrn dev` + browser runtimes |
| [0020](RFC-0020-i18n.md) | i18n | **Implemented (M1 + M2)**: M1 finite string types & interpolation containment (a proven `"nav.\{s}.label" ⊆ TransKey` skips its runtime check + literal completion); M2 the `std/i18n` generator on RFC-0021 — a Vyrn-authored JSON + ICU reader that flattens locales, checks cross-locale drift, and compiles each message (interpolation / plural via CLDR rules / select) into ordinary Vyrn, emitting `TransKey`, a `Locale` enum + module state, and per-key typed functions with `///` docs |
| [0021](RFC-0021-generator-imports.md) | Generator Imports | **Implemented**: `gen fn` + `import { .. } from gen(args)` — comptime-pure module synthesis, mediated `readFile`/`listDir`/`moduleInterface`, content-addressed cache |
| [0022](RFC-0022-ergonomics.md) | Ergonomics Batch | **Implemented**: `else if` chaining; string ordering (`< <= > >=`, byte-wise); `s[i] : UInt8` (aligned with `bytes(s)`); import aliasing (`import { X as Y }`, alias-aware LSP) — the last erases RFC-0019's `call<Proc>` deviation |
| [0023](RFC-0023-function-values.md) | Function Values | **Implemented**: `fn`-typed parameters + lambda literals (`\|x\| expr`), parameter-only and call-argument-only, read-only captures, per-lambda monomorphization — zero runtime function pointers in any backend; ships generic `std/arrays` (`map`/`filter`/`fold`/`any`/`all`) |
| [0025](RFC-0025-worker-threads.md) | Worker Threads | **Implemented**: `spawn` on real OS threads natively (shim-resident: Win32/pthreads, wasm stays inline, `VYRN_SEQUENTIAL_SPAWN=1` escape hatch) — byte-identical by isolation, zero parity exclusions; `vyrn serve/dev --workers N` runs `handle` on per-worker interpreters behind the module-state gate (refusal names the call path) |

## Status legend

Each RFC header carries a status:

- **Draft** — under active discussion, decisions may still flip.
- **Accepted** — decision made; implementation may lag.
- **Implemented** — reflected in `compiler/` and covered by tests.
- **Superseded by RFC-XXXX** — kept for history.

The early RFCs (0001–0009) began as **Draft**, distilled from the founding
design conversation; most of the surface they describe — and RFCs 0010–0016 —
is now **Implemented** in `compiler/` and covered by the three-way parity
corpus (each RFC header carries its own status). RFC-0004's memory model is the
part still expected to move; RFC-0017 (the formatter, `vyrn fmt` + LSP
`textDocument/formatting`) and RFC-0018 (the JSON codec, `toJson`/`fromJson`)
are now **Implemented** too. RFC-0021 (generator imports — user code that runs
at compile time and synthesizes a module) is **Implemented** and is the
mechanism RFC-0019 (typed RPC) and RFC-0020 (i18n) are now built as libraries
over: with the `moduleInterface` reflection primitive and mediated file reading,
both shed their compiler-flavored special cases. Both are now **Implemented** —
`std/rpc` and `std/i18n` are ordinary Vyrn `gen fn`s the compiler knows nothing
about. RFC-0025 (worker threads) is **Implemented** too — the promise RFC-0016's
async decision made ("worker threads calling `handle` in parallel, gated on the
isolation analysis") is kept, with `spawn` on real OS threads under the same
byte-identical corpus.

## Process

1. A change to language semantics starts as an edit to the relevant RFC.
2. Open questions get resolved by writing the smallest prototype that answers
   them, then recording the answer back in the RFC.
3. Only once an RFC section is **Accepted** does it earn implementation effort.
