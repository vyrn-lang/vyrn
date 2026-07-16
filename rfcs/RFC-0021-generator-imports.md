# RFC-0021 — Generator Imports: Compile-Time Module Synthesis

- **Status:** Draft — approved for implementation
- **Depends on:** RFC-0010 (loader/linker, remote imports, the lock/cache),
  RFC-0014 (file reading), RFC-0015 (`test` — generators are testable code)
- **Supersedes the design of:** RFC-0020 M2's compiler-flavored `import i18n`
  (now a library on this mechanism); long-term, `import type` (schema) is
  reimplementable as a std generator and the language sheds format knowledge
  entirely.

> **Motivation.** The compiler was starting to accrete file-format knowledge:
> JSON Schema (`import type`), then a proposed translations flavor. Each is a
> crutch for the same missing general mechanism: **user code that runs at
> compile time and synthesizes a module**. Vela can offer this with unusually
> strong guarantees, because the compiler already contains a deterministic
> interpreter, a capability-mediated module resolver, and a content-addressed
> lock/cache — the three things that make compile-time codegen safe,
> reproducible, and fast elsewhere only by convention.

---

## Surface

```vela
// gen/i18n.vela — an ordinary Vela module (local, std/, or github:@pinned)
export gen fn i18n(dir: String) -> String {
    // read <dir>/<locale>.json via readFile/listDir (scoped — see sandbox),
    // parse, check cross-locale drift (fail with a clear trap message),
    // and RETURN VELA SOURCE TEXT for the module to synthesize.
}
```

```vela
// app code
import { i18n } from "./gen/i18n"
import { t, TransKey, setLocale } from i18n("./locales")
```

- **`gen fn`** — a contextual modifier (the `extern`/`rpc` precedent). An
  ordinary function otherwise: callable at runtime too (useful for testing),
  formatted by `fmt`, covered by `test` blocks, distributed like any module
  (incl. `github:` + `vela.lock` — generators are left-pad-proof).
- **An import target may be a `gen fn` call** whose arguments are
  compile-time constants (consteval-provable; paths resolve relative to the
  importing file). The loader runs the call in the compiler's interpreter;
  the returned `String` is Vela source, lexed/parsed/linked as a synthesized
  module through the ordinary pipeline. Checker, backends, parity, and the
  LSP stay module-unaware, as always.
- A generator that **traps** (drift check failed, malformed input) turns
  into a load diagnostic at the import site, carrying the trap message.

## The sandbox (what makes this safe)

A `gen fn` (and everything it transitively calls) is checked by a
**comptime-purity analysis** — the spawn-isolation machinery's sibling:

- **Forbidden:** `extern`, `spawn`, module state, `writeFile`, `readLine`,
  `args`, logging sinks. (No clock or randomness exists in Vela — good.)
- **Permitted, mediated:** `readFile` and a new `listDir(path) ->
  Array<String>` builtin — at generation time these route through the
  loader's `ModuleResolver`, restricted to paths under the generator call's
  constant path arguments. The resolver is exactly how the LSP stays
  read-only and how a future remote-input story stays lockable.
- Runtime execution of the same fn (outside an import) uses ordinary I/O
  rules; the restriction is a property of the *generation context*.

Consequence: **same inputs ⇒ same output**, mechanically. Contrast: Rust
proc-macros run arbitrary native code with ambient authority; TS codegen
writes artifacts that go stale. A generator is interpreted, scoped,
deterministic, and pinned.

## Caching (what makes this fast)

Deterministic + declared inputs ⇒ content-addressed output:
`sha256(linked generator sources ++ args ++ every input file read)` keys a
cache of generated source (`~/.vela/cache/gen/<hex>` — the M4
infrastructure). Cold generation runs once; rebuilds and per-keystroke LSP
re-analysis hit the cache. The resolver records which files were read — the
synthesized module's true dependency set, available to any future watcher.

## Diagnostics & debugging

- Errors *inside generated source* report against the generated text with a
  banner naming the generator call site; **`velac emit-gen <file>`** dumps
  every synthesized module for inspection.
- Output must parse and link like any module; name collisions with user
  code are ordinary load errors.
- Guardrails: a generation size cap and step budget (runaway generators
  fail loudly, not hang builds).

## Hygiene stance (v1, honest)

Generated code is **source text** — transparent, diffable via `emit-gen`,
and formatted. There is no macro hygiene: generators own their namespace
choices (convention: prefix or PascalCase-derive from inputs, as the i18n
generator does with `tCartItems`). Emitting AST behind a reflection API is
a possible v2 if hygiene ever earns its complexity.

## What becomes a library

- **i18n (RFC-0020 M2)** — the first real generator: ICU-subset parsing,
  key flattening, drift checking, CLDR plural tables, all in Vela.
- **Future, all libraries, zero compiler patches:** `.proto` emit/import,
  GraphQL SDL, OpenAPI clients, SQL schema types, CSV-to-types, route
  tables. `import type` (schema) is grandfathered but reimplementable here.

## Out of scope

AST-level emission / reflection, macro hygiene, generators writing files,
network inputs at generation time (inputs are local or lock-pinned module
files), incremental regeneration beyond the whole-call cache, expression-
position macros (generators synthesize *modules*, nothing smaller — that
restraint is the feature).
