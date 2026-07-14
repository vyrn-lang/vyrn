# Vela RFCs

This directory is the **design record** for Vela. It is the north star: when the
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

## Status legend

Each RFC header carries a status:

- **Draft** — under active discussion, decisions may still flip.
- **Accepted** — decision made; implementation may lag.
- **Implemented** — reflected in `compiler/` and covered by tests.
- **Superseded by RFC-XXXX** — kept for history.

Everything here is currently **Draft**, distilled from the founding design
conversation. Nothing has been battle-tested against a real implementation yet;
expect the memory model (RFC-0004) to move the most.

## Process

1. A change to language semantics starts as an edit to the relevant RFC.
2. Open questions get resolved by writing the smallest prototype that answers
   them, then recording the answer back in the RFC.
3. Only once an RFC section is **Accepted** does it earn implementation effort.
