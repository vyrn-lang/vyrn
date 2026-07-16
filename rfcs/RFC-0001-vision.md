# RFC-0001 — Vision & Principles

- **Status:** Draft
- **Supersedes:** —
- **Depends on:** —

---

## Mission

Create a systems programming language that combines the **expressiveness of
TypeScript** with the **performance of native code**, while making safety
*predictable and intuitive* rather than exposing compiler implementation
details.

The programmer focuses on *what they want to do*; the compiler determines *how
to do it safely*.

## Target audience

- TypeScript / JavaScript developers who want native performance.
- Rust developers who want less day-to-day cognitive overhead.
- Game, backend, tooling, and CLI developers.

## Explicitly *not* competing with

| Language | Owns the niche of | Why Vyrn doesn't chase it |
|----------|-------------------|---------------------------|
| C        | maximal low-level control | Vyrn is memory-safe by default |
| Rust     | maximal control + zero-overhead safety | Vyrn trades some control for predictability |
| Go       | simplicity via a GC | Vyrn has no tracing GC |
| TypeScript | running on JS everywhere | Vyrn is native, ahead-of-time |

Vyrn occupies its own niche: **"systems TypeScript."**

---

## The ten principles

These are the test every later decision must pass. When a feature is proposed,
the question is: *does this make the programmer's intent more obvious without
weakening the guarantees?*

### 1. Intent over mechanism
The programmer reasons about **read / modify / consume / share**, not ownership,
lifetimes, or borrow state. The compiler translates intent into safe, efficient
code. → RFC-0004.

### 2. Predictability
A programmer can predict whether code is legal **by reading it**. Compiler errors
confirm expectations rather than teaching hidden rules.

### 3. Capabilities are visible
What you may do with a value is visible — in the type, the signature, or the
tooling. When an operation is temporarily unavailable, the compiler explains
exactly why and when it returns. → RFC-0004, RFC-0006.

### 4. Zero unsafe defaults
No unchecked casts. No dangling pointers. No undefined behavior from memory
misuse. No hidden null. If something cannot be proven safe, it must be checked.

### 5. Structural types
Types describe **shape**, not identity. A value whose shape satisfies a type is
usable as that type with no cast. Nominal identity is an explicit opt-in. → RFC-0002.

### 6. Powerful compile-time types
Type transformation is first-class: `Partial<T>`, `Readonly<T>`, `Pick`, `Omit`,
`Merge`, `Map`, `Filter`. These vanish after compilation — zero runtime cost. → RFC-0002.

### 7. Compiler does the hard work
The compiler infers ownership, borrowing, moves, copies, and lifetimes whenever
it can. The programmer annotates only when multiple valid interpretations exist.

### 8. Deterministic memory
Memory is always released predictably at well-defined points. No tracing GC, no
surprise pauses. → RFC-0004.

### 9. Escape hatches exist, but are explicit
Low-level control is available, isolated, and clearly marked (`unsafe`-like
regions). The common path never needs it.

### 10. Great diagnostics
The compiler is a **teacher, not a gatekeeper**. Errors describe the programmer's
intent, name what blocks it, and suggest concrete fixes. → RFC-0006.

---

## The bet

The differentiator is not the type system and not the backend — both borrow
heavily from prior art. It is the *combination*:

1. A practical, expressive type system (structural + validated + compile-time).
2. Native performance with strong safety and no GC.
3. A programming model built around **intent** instead of **implementation**.

Rust is known for ownership. Haskell for type theory. Lean for dependent types.
Vyrn's opportunity: **the best *practical* type system** — type-level power that
feels like ordinary programming, not type theory.

---

## Non-goals (v1)

Deferred deliberately to keep the core small and coherent. Each *may* return once
the core proves itself.

| Feature | Verdict | Reason |
|---------|---------|--------|
| Full higher-kinded types | ❌ skip (long) | High complexity, low weekly value for target audience |
| Full dependent types / theorem proving | ❌ skip | Turns the compiler into a proof assistant; see RFC-0003 for the *practical* subset we keep |
| Macros | ❌ v1 | Compile-time reflection covers most use cases |
| Class inheritance | ❌ forever | Fragile base classes, diamonds; composition + protocols instead |
| Operator overloading | ⏳ post-core | Good via traits/protocols, but not until the core is stable |
| Custom allocators, metaclasses | ❌ v1 | Niche |

**Rule of thumb for admission:** *Does this feature help 90% of programmers every
week?* If yes, consider it. If no, wait.

### What we keep from the "hard" features
A **restricted, practical form of dependent typing** — const generics
(`Array<T, N>`, `Matrix<T, R, C>`) and value-refined types
(`type Age = Int where value >= 18`). This is the sweet spot, and it graduates to
its own RFC → RFC-0003.

---

## Open questions

- **Q1.** How much should the compiler infer before it feels "magical" and
  unpredictable? (Tension between Principle 2 and Principle 7.) *Resolve with a
  prototype, not by argument.*
- **Q2.** Final codename. "Vyrn" is provisional.
- **Q3.** Does const-generic dependent typing (RFC-0003) ship in v1 or v1.x?
