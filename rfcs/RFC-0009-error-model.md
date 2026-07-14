# RFC-0009 — Structured, Accumulating Validation

- **Status:** Draft — **`Issue` + `Validation<T>` implemented**
- **Depends on:** RFC-0002 (generic enums, records), RFC-0005 (`Result`/`Option`)

> **Implementation status.** The built-in `Issue` record and generic
> `Validation<T>` enum are injected into every program and lower through the
> existing generic-enum/record/array machinery. Verified interpreter == native
> (`examples/validation.vela`).

---

## The problem

`Result<T, E>` (RFC-0005) and validated-type construction (RFC-0003) both fail on
the **first** error. Real input validation — a form, a request body, a config
file — needs to report **every** problem at once, each tagged so a UI can render
it and an i18n layer can localize it. That is a different shape: an *accumulating*
error type with *structured, keyed* errors.

## The types (built in)

```vela
type Issue = { key: String, path: String, message: String }
type Validation<T> = | Valid(T) | Invalid(Array<Issue>)
```

- **`key`** — a stable identifier for i18n (`"age.max"`); the message layer maps it.
- **`path`** — which field/location failed (`"age"`), for programmatic handling.
- **`message`** — a default human string (a fallback when no translation exists).

A validator accumulates all failing checks into an issue array and returns
`Invalid(issues)`, or `Valid(value)` when clean:

```vela
fn validatePerson(name: String, age: Int) -> Validation<Person> {
    let mut issues: Array<Issue> = []
    if len(name) == 0 { issues.push(Issue { key: "name.required", path: "name", message: ".." }) }
    if age < 0        { issues.push(Issue { key: "age.min",      path: "age",  message: ".." }) }
    if age > 150      { issues.push(Issue { key: "age.max",      path: "age",  message: ".." }) }
    if issues.length > 0 { return Invalid(issues) }
    return Valid(Person { name: name, age: age })
}
```

The caller `match`es on `Valid`/`Invalid`; every issue is available at once, so the
UI can show all field errors together and localize each via its `key`.

## Why not the pure applicative `check(a) & check(b)`

The classic applicative — `Validation<A> & Validation<B> -> Validation<(A, B)>`,
accumulating errors while combining values — needs **tuples** and **closures**
(to name the combined value / the final constructor). Vela has neither yet, so the
combined-value form is not expressible. The **accumulate-into-an-array** pattern
above reaches the same practical goal (report all errors, keyed, renderable) with
today's features, and reads clearly. A combinator layer (`require`, `all`,
`map2`) is a natural addition once closures/tuples land.

## Interaction with `where` (RFC-0003) — future

Validated types (`type Age = Int where value >= 18`) currently *abort* on a failed
runtime construction. The plan (feeding RFC's extractable-`where` goal) is to let
a validated construction yield a `Validation<T>` whose `Invalid` issue is derived
from the predicate — reusing the predicate metadata for both enforcement and,
eventually, generated docs (OpenAPI). That is the next step and is tracked with
the `where`-reflection work.

## Open questions

- **Q1 — combinators.** `require(cond, key, path) -> Validation<Unit>` and an
  `all([..])` accumulator would make multi-field validation terser; both are easy
  once we decide the surface (they need only arrays, which exist). Worth adding now
  or after closures?
- **Q2 — issue `data`.** Should `Issue` carry a structured `data` payload (e.g.
  the offending value, bounds) beyond `message`? That needs a heterogeneous value
  — the RFC-0007 `Value` enum could serve — vs. keeping `Issue` flat.
- **Q3 — `where` → `Validation`.** The exact lowering of a failed validated
  construction into an `Invalid` issue (key derivation from the predicate).
