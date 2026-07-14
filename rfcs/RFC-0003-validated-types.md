# RFC-0003 — Validated Types

- **Status:** Draft — **core implemented in v0.1** (see below)
- **Depends on:** RFC-0002
- **Related:** RFC-0005 (construction returns `Result`/`Option`)

> **Implementation status (v0.1).** Scalar refinement types are implemented end
> to end — parser, checker, interpreter, and both codegen backends:
> ```vela
> type Age  = Int where value >= 18;
> type Port = Int where value >= 1 && value <= 65535;
> ```
> - Compile-time-constant constructions are validated at compile time and
>   **rejected if provably invalid** (`Age(5)` is a compile error); when valid
>   they erase to the value — **zero runtime cost**.
> - Non-constant constructions (e.g. from a function parameter) are checked at
>   **runtime**; failure aborts via `exit(1)` in native code and errors in the
>   interpreter.
> - A validated type **decays to its base** (an `Age` is usable as an `Int`), but
>   a raw `Int` is **not** usable as an `Age` without construction.
>
> **Not yet implemented:** the `Result`/`Option`-returning construction path
> (§3, §Q1) — v0.1 uses abort-on-failure rather than recoverable errors, pending
> RFC-0005. Predicates are limited to pure expressions over `value` (no calls),
> and const generics (Level 1) are not started. See `examples/validate.vela` and
> `examples/validate_fail.vela`.

---

## Summary

A type describes not only the *structure* of data but also the **rules that make
a value valid**. Validation lives in the type definition, not in scattered
runtime checks that every caller must remember to invoke.

This is Vela's candidate **signature feature**. It is a deliberately restricted,
practical slice of dependent typing — no theorem proving — governed by one rule
from RFC-0001:

> **If the compiler can prove it, there is no runtime cost. If it cannot, the
> validation is generated automatically.**

---

## 1. The core idea

Today, in most languages, data and its validation are separate:

```ts
type User = { email: string; age: number }
function validate(u: User): Result<User> { ... }   // easy to forget to call
```

In Vela the type owns the rules:

```vela
type Email = String where isEmail
type Age   = Int    where value >= 18

type User = {
    email: Email,
    age: Age,
}
```

**There is no such thing as an invalid `User`.** If a value has type `User`, its
`email` passed `isEmail` and its `age` is ≥ 18 — guaranteed, everywhere, with no
call-site ceremony.

## 2. Levels of validation (what we build, in order)

Adapted from the design discussion's "levels of dependent typing":

### Level 1 — Const generics *(practical, high priority)*
Compile-time integers/booleans as type parameters. Well-trodden (Rust has this).

```vela
type Matrix<T, R: Int, C: Int> = ...

fn mul<T, M: Int, N: Int, P: Int>(
    a: Matrix<T, M, N>,
    b: Matrix<T, N, P>,      // shared N enforced at compile time
) -> Matrix<T, M, P>
```

`Matrix<3,4> * Matrix<7,9>` is a **compile error**, not a runtime one.

### Level 2 — Value-refined types *(the signature feature)*
A base type plus predicates.

```vela
type Port        = Int    where value in 1..=65535
type Percentage  = Float  where value >= 0.0 && value <= 100.0
type Password    = String where length >= 12 && hasUpper && hasDigit
type Username     = String where length in 3..=20
type HexColor    = String where matches("#[0-9A-Fa-f]{6}")
```

### Level 3 — Full dependent types *(explicit non-goal — RFC-0001)*
Types depending on arbitrary runtime values, proofs like `Sorted<Vector<N>>`.
**Not in Vela.** This is where languages become theorem provers.

## 3. Compile-time vs runtime — the automatic split

The compiler decides *when* validation happens based on what it can prove.

### Value known at compile time ⇒ checked at compile time, zero cost
```vela
let p = Port(8080)     // OK, proven at compile time
let q = Port(70000)    // COMPILE ERROR: 70000 not in 1..=65535
let a = Age(25)        // OK
let b = Age(-5)        // COMPILE ERROR
```

### Value from outside (input, disk, network) ⇒ validation generated, returns Result
```vela
let raw: String = read_line()
let email = Email(raw)          // type of `email` is Result<Email, ValidationError>
let email = Email(raw)?         // or propagate with `?` (RFC-0005)
```

The programmer **cannot** obtain a bare `Email` from untrusted input without the
check — the type of the constructor forces it. Validation is never silently
skipped.

## 4. Why this matters at the API boundary

Validation moves *out* of function bodies and *into* signatures.

```vela
// Before: every implementation re-checks
fn login(email: String) {
    if !isEmail(email) { return err }
    ...
}

// Vela: the type already guarantees it
fn login(email: Email) {
    // email is valid, always. No check here.
}
```

Combined with structural records (RFC-0002), a whole aggregate becomes
impossible to construct in an invalid state:

```vela
type Account = {
    id: UserId,          // nominal String
    email: Email,        // where isEmail
    age: Age,            // where value >= 18
    color: HexColor,     // where matches(...)
}
// Constructing an Account validates every field, at compile time where possible,
// at runtime (as Result) where not.
```

## 5. Predicates

A predicate is a pure, total, compile-time-analyzable function `(T) -> Bool`.

- Simple numeric/range/length predicates and boolean combinations are always
  supported and often provable at compile time.
- Predicates the compiler cannot evaluate at compile time still work — they
  simply always run at construction time (returning `Result`).
- Regex predicates are supported but flagged as harder to reason about
  statically; a literal `matches("...")` on a literal string *can* be checked at
  compile time, otherwise it runs at construction.

```vela
fn isEmail(s: String) -> Bool { ... }        // ordinary predicate
```

## 6. Interaction with the type system (RFC-0002)

- `Omit`/`Pick`/`Partial` **preserve** the `where` clauses of retained fields.
- Structural widening cannot *remove* a refinement: a value of a refined type is
  usable where the base type is expected (an `Email` is a `String`), but a bare
  `String` is **not** usable where `Email` is expected without construction.
- Refinements participate in exhaustiveness: `match` can narrow on refined
  variants.

---

## Open questions

- **Q1.** Constructor syntax when validation may fail. Does `Email(x)` return
  `Result` implicitly (type-directed), or is there an explicit fallible form
  (`Email.parse(x)`)? Implicit is more ergonomic but less visible — tension with
  RFC-0001 Principle 2 (predictability). *Leaning: fallible construction is
  visible in the return type, and `?` makes it terse.*
- **Q2.** How much arithmetic does the const-generic evaluator support (just
  `+ - *`? comparisons? user functions)? Start minimal.
- **Q3.** Can refinements reference **other fields** of the same record
  (`type T = { lo: Int, hi: Int where hi >= lo }`)? Powerful, but pulls toward
  Level 3. *Tentatively: yes for same-record fields, no for arbitrary values.*
- **Q4.** Do refinements survive across FFI / serialization boundaries, and how
  are they re-established on deserialization? (Answer likely: deserialization is
  "outside input" ⇒ returns `Result`.)
