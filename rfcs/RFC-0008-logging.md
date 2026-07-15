# RFC-0008 — Logging

- **Status:** Draft — **leveled logger + threshold + single sink (stderr/stdout/
  file) implemented; per-logger overrides, multi-sink fan-out, and pattern not yet**
- **Depends on:** RFC-0007 (string templates — the message surface), RFC-0004
  (`String` heap values), RFC-0002 (enums, records)
- **Replaces:** the anonymous `print` builtin (eventually — see below)

> **Implementation status (Phase 1).** The facade is live: `logger(name)` returns a
> `Logger` (a new opaque type lowering to a `ptr` to its name); the five levels are
> called subject-first — `log.trace/debug/info/warn/error("…")` — via the RFC-0007
> method sugar, and messages use RFC-0007 interpolation. Logs are written to
> **stderr** as `[LEVEL] name: message` (native via `fprintf(__acrt_iob_func(2), …)`
> on this MSVC toolchain; interpreter via `eprintln!`), deliberately off stdout so
> program output and diagnostics are separable — the "where does it print" concern.
> Interpreter == native verified (stdout + stderr identical modulo the documented
> CRLF/LF artifact). Logging is barred from spawned tasks (it is observable I/O), and
> the log methods are read-only for ownership analysis. See `examples/logging.vela`.
>
> **Phase 2 (threshold + config).** A `logging { level: <name> }` top-level block
> sets the threshold; the default is `Info` (so `trace`/`debug` are off unless
> lowered). Filtering is **compile-time**: a call below the threshold emits no write
> in either backend — but its **arguments are still evaluated** (matching the
> interpreter, which evaluates args before the level check), so message side effects
> are consistent. `Program` gained a `log_level` ordinal; a shared
> `log_level_ordinal(name)` keeps the interpreter and codegen in lockstep. See
> `examples/logging.vela` (threshold `debug` — `trace` is dropped).
>
> **Sinks (implemented, single sink).** `logging { level: .., sink: .. }` selects
> the destination: `stderr` (default), `stdout`, or `file("path")`. The sink is
> compile-time-known, so the log call targets the right stream directly — MSVC's
> `__acrt_iob_func(1)`/`(2)` for stdout/stderr, and for a file a `@__vela_log_file`
> global opened once with `fopen`(mode `w`, truncating) in `@main` before
> `vela_main` and `fclose`d after. The interpreter mirrors this with
> `println!`/`eprintln!`/a `std::fs::File`. Verified interpreter == native on the
> **file contents** too. See `examples/logging.vela`.
>
> **Not yet:** **per-logger** level overrides (`loggers: { "db": Debug }` — hard
> under compile-time filtering since a logger's name is a runtime value), **multi**
> -sink fan-out (`sinks: [ .. ]`), and the **pattern** layout. `print` is **not
> retired yet** — that waits until the logging surface settles, to avoid churning
> every example prematurely.

---

## Summary

Two layers, exactly the SLF4J/Logback split:

- **Facade** — what call sites use. A **named logger** with the five standard
  levels. It knows nothing about where logs go.

  ```vela
  let log = logger("dynarray");
  log.info("collected \{n} squares, sum = \{total}");   // RFC-0007 template
  log.warn("nothing to sum");
  log.error("index \{j} out of range");
  ```

- **Backend** — declarative config, set once. Chooses the threshold, per-logger
  overrides, and where output goes.

  ```vela
  logging {
      level:   Warn,                        // root threshold
      loggers: { "dynarray": Debug },       // per-name overrides (Logback <logger>)
      sinks:   [ stdout(), file("app.log") ],
      pattern: "\{time} \{level} \{name}: \{msg}",
  }
  ```

The two are decoupled: call sites never name a destination, so the same code logs
to a file in production and stderr in a test with no edit — the RFC-0004 "control
happens at the boundary, not the call" principle applied to output.

## The facade

`logger(name: String) -> Logger` returns (or creates) the logger for `name`.
`Logger` has five methods, one per level:

```
log.trace(msg)   log.debug(msg)   log.info(msg)   log.warn(msg)   log.error(msg)
```

- `msg` is an RFC-0007 **template** (or a plain `String` — a `String` is just the
  degenerate no-interpolation template). Passing a template, not a pre-rendered
  string, is what makes structured logging possible (below).
- Each method is subject-first (`log.info(...)`), the shape RFC-0007's surface
  work established — never `info(log, ...)`.
- A call below the effective threshold is **suppressed cheaply**: the level check
  happens before the template is rendered, so `log.debug("expensive \{f()}")`
  pays nothing when debug is off. (Whether the *arguments* are still evaluated is
  Q4.)

### Levels

`type Level = | Trace | Debug | Info | Warn | Error;` — totally ordered
`Trace < Debug < Info < Warn < Error`. A logger emits a record iff its level `>=`
its effective threshold (its own override, else the root `level`).

## The backend

A single `logging { ... }` config block, evaluated once at startup (Logback's
`logback.xml`, but in-language and typed):

| field | meaning | default |
|-------|---------|---------|
| `level` | root threshold | `Info` |
| `loggers` | per-name threshold overrides | none |
| `sinks` | where records go; one or many | `[ stderr() ]` |
| `pattern` | layout of a rendered line | `"\{level} \{name}: \{msg}"` |

**Sinks** (appenders): `stdout()`, `stderr()`, `file(path)`. Multiple sinks fan
out. `stdout`/`stderr` are trivial (`fprintf` to the stream); `file` requires
**real file I/O** — `open`/`write`/`close` — which Vela does not have today and
which must be added to **both** the interpreter and the native backend in
lockstep so the invariant holds. That is the single biggest cost in this RFC and
the reason `file` is a later phase.

**Pattern** is itself a template over the fields `\{time}`, `\{level}`,
`\{name}`, `\{msg}` — so RFC-0007 does double duty as the layout language.

## Structured logging (free from templates)

Because `log.info` receives a *template* (parts + typed values), a logger can keep
the structure instead of flattening it:

```
log.info("collected \{n} squares in \{ms}ms")
   ⇒ record { msg_template: "collected {} squares in {}ms",
              fields: [ IntVal(n), IntVal(ms) ] }
```

A text sink renders `parts`+`values` to a line; a future JSON sink emits
`{"msg":"collected {} squares in {}ms","fields":[42,7]}`. No separate structured
API — it is the same call, a different sink. This is the payoff of building
logging *on* RFC-0007 rather than on ad-hoc `{}` formatting.

## Retiring `print`

`print(x)` is removed. The mechanical migration is `print(x)` → a module logger at
`info`, or — for the handful of examples that are really "show me this value" —
kept as `log.info("\{x}")`. Examples and tests migrate in the Phase 1 change.

## Phased implementation

Each phase is independently shippable and keeps interpreter == native.

1. **Leveled console logger.** `logger(name)`, five levels, `Level`, a global root
   threshold, output to `stderr` with a fixed default pattern. Message is a plain
   `String` for now (build it with `concat`/`str` until RFC-0007 lands). Retire
   `print`. No file I/O, no config block yet. *Lands fast.*
2. **Templates as the message.** Wire RFC-0007 templates into the log methods;
   add the level-gated no-render fast path; add structured records.
3. **The `logging { }` config block.** Root `level`, per-logger overrides,
   `pattern` layout. Still console sinks only.
4. **Sinks / file I/O.** `stdout()`/`stderr()`/`file(path)`, multi-sink fan-out —
   file I/O added to both backends in lockstep. This is the heavy phase.

## Open questions

- **Q1 — logger identity.** Is a `Logger` a first-class value you pass around, or
  only obtainable via `logger(name)` at use sites? *(Leaning: first-class value,
  but interned by name so `logger("x")` twice is the same logger.)*
- **Q2 — config timing.** Is `logging { }` a static top-level declaration
  (evaluated before `main`), or a runtime call? A static block is more Logback-like
  and lets levels be known at compile time (enabling dead-code elimination of
  disabled call sites); a runtime call is more flexible. *(Leaning: static block.)*
- **Q3 — compile-time level elision.** With a static root level, calls provably
  below threshold could be **removed at compile time** (zero cost, like a disabled
  `assert`). Worth it, or keep it a runtime check for simplicity?
- **Q4 — argument evaluation under suppression.** When a call is below threshold,
  are the interpolated expressions still evaluated (side effects preserved) or
  skipped (cheaper, but `log.debug("\{sideEffect()}")` changes behavior by level)?
  SLF4J evaluates them; a macro-style template could skip them. Must be pinned for
  interpreter == native.
- **Q5 — file I/O surface.** `file(path)` is the first real filesystem access in
  Vela. Does it get a general file API (RFC-00xx), or a logging-only sink with no
  user-facing file handle? *(Leaning: logging-only sink first; a general I/O RFC
  later.)*
- **Q6 — errors from sinks.** A failing `file()` write (disk full, bad path) — does
  logging swallow it, log-to-stderr about it, or surface a `Result`? *(Leaning:
  swallow + one-time stderr warning, so logging never crashes the program.)*
