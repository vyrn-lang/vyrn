# RFC-0005 — Errors, Null & Concurrency

- **Status:** Draft — **`Option`, `Result`, `match`, and `?` implemented in v0.1**
- **Depends on:** RFC-0002 (unions), RFC-0003 (construction returns Result)

> **Implementation status (v0.1).** `Option<T>`, `Result<T, E>`, exhaustive
> `match`, and the `?` operator are implemented end to end — parser, checker,
> interpreter, and both codegen backends:
> ```vyrn
> fn parse_age(n: Int) -> Result<Age, Int> {
>     if n >= 18 { return Ok(Age(n)); }
>     return Err(n);
> }
> fn age_plus(n: Int, extra: Int) -> Result<Int, Int> {
>     let a = parse_age(n)?;      // `?` early-returns Err(n) on failure
>     return Ok(a + extra);
> }
> ```
> - `None`/`Ok`/`Err` are inferred from context (return type, `let` annotation,
>   argument); with no context it's a compile error rather than a guess.
> - `match` must cover **both** variants of the scrutinee (`Some`/`None` or
>   `Ok`/`Err`), and all arms must share a type. `match` is an expression.
> - `?` unwraps `Some`/`Ok`, or returns the `None`/`Err` from the enclosing
>   function — which must return a matching `Option`/`Result` (checked). In the
>   interpreter this rides a control-signal channel; in native code it lowers to a
>   tag test that returns the aggregate directly.
> - Native lowering: `Option`/`Result` are both a `{ i1, i64 }` aggregate
>   (tag + i64 payload); constructors use `insertvalue`, `match` is a tag test +
>   `phi`. The native backend supports **Int payloads** and rejects Bool payloads
>   at the constructor; the interpreter supports any scalar payload.
>   See `examples/option.vyrn` and `examples/result.vyrn`.
>
> **Not yet implemented:** wiring validated construction (RFC-0003) to *return*
> `Result` directly (today it aborts, but the fallible pattern composes cleanly —
> see `parse_age` above), the concurrency model (§6), and typed error sets (Q2).

---

## Summary

Errors are explicit values, not exceptions. Absence is `Option<T>`, not null.
Propagation is terse via `?`. Concurrency favors message passing over shared
mutable state. All of these are ordinary unions (RFC-0002), so nothing here needs
special typing machinery.

---

## 1. No null

`null` does not exist. Absence is explicit:

```vyrn
type Option<T> =
    | Some { value: T }
    | None
```

Field access, array indexing, and map lookups that might miss return `Option<T>`;
there is no nullable reference to forget to check.

## 2. Errors as values: `Result`

```vyrn
type Result<T, E> =
    | Ok  { value: T }
    | Err { error: E }
```

Fallible operations return `Result`. This composes and keeps failure paths
visible — the RFC-0001 predictability principle applied to control flow.

## 3. Propagation: `?`

`?` unwraps `Ok`/`Some` or short-circuits the enclosing function with the
`Err`/`None`.

```vyrn
fn load_user(path: String) -> Result<User, IoError> {
    let raw = read_file(path)?          // returns Err early on failure
    let email = Email(raw.email)?       // RFC-0003: validated construction
    Ok(User { email, ... })
}
```

`?` works uniformly on `Result` and `Option` (with the obvious conversion rules,
TBD in Q1).

## 4. Handling: `match`

No special-casing — `Result`/`Option` are unions, so `match` handles them and the
compiler enforces exhaustiveness (RFC-0002):

```vyrn
match load_user(p) {
    Ok(user)  => greet(user),
    Err(e)    => log(e),
}
```

## 5. Panics vs errors

Two tiers, kept distinct:

- **Recoverable** → `Result` / `Option`. The default for anything a caller might
  reasonably handle (I/O, parsing, validation).
- **Unrecoverable** → a panic/abort for genuine bugs (broken invariants,
  exhausted memory). Not catchable as normal control flow; ends the program (or
  the actor, §6) deterministically.

Validation failures on **untrusted input** are recoverable (`Result`, RFC-0003).
Validation failures the compiler *proved impossible* never reach runtime.

## 6. Concurrency (direction, not yet specified)

Bias: **safe communication over unrestricted shared mutation.** Shared mutable
state is available only when explicitly requested.

Areas to design (own RFC later):

- **Actors** — isolated state, communicate by messages. Pairs naturally with the
  capability model (RFC-0004): an actor owns its state; messages transfer
  `consume` or `share` capabilities.
- **Channels** — typed hand-off between tasks.
- **async / await** — for I/O-bound concurrency.
- **Isolated mutable state** — a `modify` capability never crosses a task boundary
  without an explicit, checked transfer.

Capability-based references (Pony, Project Verona) are the prior art to mine here,
because they express *what a reference may do across threads* in the type — which
is exactly the RFC-0004 surface.

---

## Open questions

- **Q1.** `?` mixing `Result` and `Option`: auto-convert `None → Err(...)` given a
  context error, or require an explicit `.ok_or(...)`? Leaning explicit.
- **Q2.** Error type ergonomics: a single boxed error type, an open enum, or
  something like typed error sets (Zig)? Typed error sets fit "visible failure"
  well.
- **Q3.** Does `?` interact with the memory model when short-circuiting through
  scopes that own regions (RFC-0004)? Destructors must still run on the early
  return — deterministic destruction (Principle 8) requires it.
- **Q4.** Full concurrency model is out of scope for this RFC; it gets its own
  once the memory model (RFC-0004) settles, since the two are coupled.
