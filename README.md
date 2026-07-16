# Vyrn

> A systems programming language with the expressiveness of TypeScript and the
> performance of native code — where safety is **predictable** instead of a
> puzzle, and types describe not just the *shape* of data but the *rules that
> make it valid*.

**Vyrn** is a working codename. It is easy to change: the name appears only in
these docs and in the crate names under `compiler/`.

Vyrn compiles ahead-of-time to native code through LLVM. There is no tracing
garbage collector. There are no unchecked casts. Every conversion is either
proven safe by the compiler or checked at runtime — never a blind
reinterpretation of memory.

---

## The one-paragraph pitch

Rust says *"here are the rules."* Vyrn tries to say *"here is what you're trying
to do."* You think in terms of **read / modify / consume / share**; the compiler
figures out ownership, borrowing, moves, and lifetimes underneath. Types are
**structural** like TypeScript, carry **validation rules** as part of their
definition, and support first-class **compile-time type transformations**
(`Omit`, `Partial`, `Pick`, …). If a value can't be proven valid at compile
time, the compiler *generates* the runtime check for you — you can't forget it.

## Design pillars

1. **Intent over mechanism** — program in capabilities, not ownership jargon.
2. **Predictability** — you can tell whether code is legal by reading it.
3. **Validated types** — validation lives in the type, not scattered call sites.
4. **Safety by default** — no null, no dangling pointers, no unchecked casts.
5. **Compiler as an assistant** — it infers what it can and *explains* what it can't.

## Repository layout

```
lang/
├── README.md              ← you are here
├── rfcs/                  ← the design record (start with RFC-0001)
│   ├── README.md          ← RFC index + process
│   ├── RFC-0001-vision.md
│   ├── RFC-0002-type-system.md
│   ├── RFC-0003-validated-types.md
│   ├── RFC-0004-capabilities-and-memory.md
│   ├── RFC-0005-error-handling.md
│   └── RFC-0006-diagnostics.md
├── compiler/              ← the prototype (Rust workspace)
│   ├── Cargo.toml         ← workspace
│   ├── vyrn-frontend/     ← lexer + parser + AST + checker (no LLVM needed)
│   ├── vyrn-codegen/      ← LLVM IR emission via Inkwell (feature-gated)
│   └── vyrn-cli/          ← the `vyrn` driver
└── examples/              ← sample .vyrn programs
```

## Status

This repo holds the **full RFC design record** plus a working compiler. Every
feature below is verified to produce identical output and exit codes under the
tree-walking interpreter and the clang-compiled native binary (34 examples, 145
tests). Highlights:

- **Validated types (RFC-0003) — the signature feature — implemented** end to
  end: `type Age = Int64 where value >= 18;`. Provably-invalid constants are
  compile errors; valid ones cost nothing at runtime; non-constant values are
  checked at runtime. See [`examples/validate.vyrn`](examples/validate.vyrn).
- **Finite string types & interpolation containment (RFC-0020 M1)** — a
  validated `String` whose regex denotes a *finite* language is a finite string
  type. An interpolation `"nav.\{s}.label"` over finite holes is itself a finite
  language, and flowing it into a validated type checks `L ⊆ T` by DFA
  containment (a product-automaton walk, not the union cross-product TypeScript
  expands): proven ⇒ the runtime check is erased; not contained ⇒ a compile error
  naming the offending key. The editor completes `t("` with every key. See
  [`examples/finitekeys.vyrn`](examples/finitekeys.vyrn).
- **`Option<T>`, `Result<T, E>`, `match`, and `?` (RFC-0005) implemented** end to
  end — no null; absence/failure are explicit values, read via exhaustive
  `match`, and `?` propagates `None`/`Err` out of a function. `Option` **and**
  `Result` payloads can hold any type (a `Ref`, string, or record), so `Option<Ref<Node>>` gives
  **recursive heap data structures** — a nil-terminated linked list and a binary
  tree, each built, traversed, and *reclaimed* by a recursive `release` walk. See
  [`examples/linkedlist.vyrn`](examples/linkedlist.vyrn),
  [`examples/tree.vyrn`](examples/tree.vyrn),
  [`examples/freelist.vyrn`](examples/freelist.vyrn),
  and [`examples/option.vyrn`](examples/option.vyrn) (Option, Result, and `?`).
- **Structural records with width subtyping (RFC-0002)** — compatibility by
  shape, no casts, implemented end to end *including native code*. Width
  subtyping lowers to a copy coercion at each boundary. Fields of a `mut` record
  are mutable (`c.value = ...`). See
  [`examples/record.vyrn`](examples/record.vyrn).
- **Compile-time type transformers `Omit`/`Pick`/`Merge` (RFC-0002 §7)** — derive
  new record shapes from existing ones; pure type-level functions, erased before
  codegen. See [`examples/utility.vyrn`](examples/utility.vyrn).
- **User-defined enums / sum types with exhaustive `match` (RFC-0002 §4)** —
  `type Shape = | Circle(Int64) | Unit;`; native-lowered to a tagged aggregate +
  `switch`. See [`examples/enum.vyrn`](examples/enum.vyrn).
- **Generics — functions, records, and enums (RFC-0002 §6)** — `fn id<T>(x: T)`,
  `type Box<T> = { value: T }`, `type Opt<T> = | Wrap(T) | Empty`, inferred per use
  and **monomorphized** for native code. See
  [`examples/generics.vyrn`](examples/generics.vyrn).
- **`consume` and `modify` capabilities (RFC-0004)** — a `consume` parameter takes
  ownership (using the value after is a compile error); a `modify` parameter is
  changed in place with the change visible to the caller (by-reference, and the
  argument must be a `mut` variable). Ownership and mutation as *intent*. See
  [`examples/consume.vyrn`](examples/consume.vyrn) and
  [`examples/modify.vyrn`](examples/modify.vyrn).
- **Nominal types, intersection `A & B`, `Partial`/`Readonly`, multi-payload
  variants, fallible construction `Age?(n)`, and constrained generics
  `<T: Ord>`** — see [`examples/generics.vyrn`](examples/generics.vyrn) and
  [`examples/validate.vyrn`](examples/validate.vyrn).
- **Checked conversions** — `str(Int64) -> String` (total) and
  `parse(String) -> Option<Int64>` (fallible, so the "not a number" case is an
  explicit `None` you must `match`). See [`examples/stringops.vyrn`](examples/stringops.vyrn).
- **Strings are UTF-8** — a `String` is an immutable sequence of UTF-8 bytes.
  `s.length` counts *bytes* (equal to code points for ASCII, larger for
  multi-byte text: `"é".length == 2`), the regex engine matches byte-wise, and
  source files are read as UTF-8. One consequence: JSON-Schema
  `minLength`/`maxLength` bounds count bytes, not UTF-16 code units — a
  deliberate, documented divergence. See [`examples/stringops.vyrn`](examples/stringops.vyrn).
- **Arrays** — growable `Array<T>` (a `Vec`: `array()` / `push` / `at` / `alen` /
  `afree`, a doubling heap buffer, bounds-checked, with explicit reclamation) and
  **fixed-size `Array<T, N>`** (a const generic: the stack value aggregate `[N x T]`,
  no heap, written with an array literal `[a, b, c]`). See
  [`examples/arrays.vyrn`](examples/arrays.vyrn) and
  [`examples/map.vyrn`](examples/map.vyrn) (an integer-keyed map written in Vyrn).
- **The heap + deterministic reclamation (RFC-0004)** — dynamic strings
  (`concat`/`len`), plus *two* ways memory gets freed, no GC:
  - a **`region { .. }`** block frees a whole *group* of allocations when it exits
    (the checker stops heap values from escaping and dangling),
  - **ownership auto-drop** frees an *individual* heap temporary the compiler
    proves never escapes its block — no region needed, and
  - **ownership transfer** lets a function hand a freshly-allocated value back to
    its caller, whose binding then owns and frees it (inferred by fixpoint over
    the call graph).

  All measured flat (~3 MB) where the same million-allocation loop leaks 1.2 GB.
  See [`examples/region.vyrn`](examples/region.vyrn) and
  [`examples/ownership.vyrn`](examples/ownership.vyrn).
- **Generational references (RFC-0004, Path B)** — a `Ref<T>` is a freely-copyable
  handle to a mutable heap cell holding any `T` (a scalar, `String`, record, or
  another `Ref`); unlike an owned value it can be *aliased*. Each access is
  generation-checked, so a reference used after it's released is caught (instead of
  dangling) — even after the slot is reused. `release` is **inferred** (the same
  ownership analysis that frees strings auto-releases a non-escaping cell), and
  because the payload is boxed a record may hold a `Ref` to its own type without
  becoming infinite. See [`examples/genref.vyrn`](examples/genref.vyrn) and
  [`examples/autorelease.vyrn`](examples/autorelease.vyrn).
- **Structured concurrency (RFC-0004 §Q4)** — `spawn f(args) -> Task<T>` and
  `join`, a deterministic fork-join. The compiler *proves* a spawned function is
  isolated (no I/O, no shared mutable state, transitively), so it's data-race-free
  by construction and the result is the same under any schedule. `share` is the
  capability for concurrent read access. See
  [`examples/concurrency.vyrn`](examples/concurrency.vyrn).
- **A working native path**: `vyrn build prog.vyrn` emits LLVM IR and links a
  real executable with `clang` (verified end-to-end; interpreter and native
  binary agree on output and exit codes, including runtime validation failures).
- **A server (RFC-0016)** — `vyrn serve prog.vyrn [--port N]` runs an ordinary
  `fn handle(req: Request) -> Response` over a hand-rolled HTTP/1.1 host
  (`std::net` only, no crates). The host owns the accept loop; module state
  (`let mut`) persists across requests. `handle` is a plain function — `main`
  and `test` blocks can call it directly, so a served file is a normal three-way
  parity citizen. Same program shape as the browser client (RFC-0013); Vyrn adds
  no `async`/`await` (the async decision, recorded in the RFC). See
  [`examples/server.vyrn`](examples/server.vyrn).

See [`compiler/README.md`](compiler/README.md) for how to build and run it, and
the status of the Inkwell backend (now also builds and runs against an LLVM 22
dev SDK, matching the interpreter on `fib` — but covers only the v0.1 subset, so
the text-IR path remains the full reference backend).

## What's deliberately *not* in v1

Higher-kinded types, full dependent types / theorem proving, macros, class
inheritance, and metaclasses. See [RFC-0001 §Non-goals](rfcs/RFC-0001-vision.md).
