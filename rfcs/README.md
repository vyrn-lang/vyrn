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
| [0019](RFC-0019-rpc.md) | Typed RPC | Draft: the codec as an RPC layer, end-to-end typed calls |

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
are now **Implemented** too. RFC-0019 (typed RPC over the codec) is the open
draft.

## Process

1. A change to language semantics starts as an edit to the relevant RFC.
2. Open questions get resolved by writing the smallest prototype that answers
   them, then recording the answer back in the RFC.
3. Only once an RFC section is **Accepted** does it earn implementation effort.
