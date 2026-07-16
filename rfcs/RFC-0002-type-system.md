# RFC-0002 — Type System

- **Status:** Draft — **structural records implemented end to end in v0.1**
- **Depends on:** RFC-0001
- **Related:** RFC-0003 (validated types build directly on this)

> **Implementation status (v0.1).** Structural record types (§1) are implemented
> across the parser, checker, interpreter, **and the native (text-IR) backend**:
> ```vyrn
> type Named = { name: Int };
> type User  = { name: Int, age: Int };
> fn greet(w: Named) -> Int { return w.name; }   // accepts anything Named-shaped
> let u = User { name: 7, age: 30 };
> greet(u);                                       // OK — width subtyping, no cast
> ```
> - Records are compared **structurally** with **width subtyping**: a value with
>   more fields is usable where fewer are expected. The reverse (missing a field)
>   is a compile error. There is no cast.
> - Struct literals `Name { field: expr, ... }` require every field; field access
>   is `value.field`. The parser disambiguates `Name {` in `if`/`while`/`match`
>   conditions (block, not literal) via a no-struct-literal context.
> - **Native lowering:** a record is an LLVM literal struct (`{ i64, i64 }` for two
>   Int fields); struct literals use `insertvalue`, field access `extractvalue`.
>   Width subtyping is a **coercion by copy**: at each boundary (call arg, return,
>   annotated `let`/assign, nested field) a value is rebuilt in the target's
>   layout by copying the required fields by name. Records with validated-type
>   fields and record-returning functions work too. Verified against the
>   interpreter. See `examples/record.vyrn`.
>
> **Compile-time transformers (§7)** are implemented for records:
> ```vyrn
> type User    = { id: Int, name: Int, password: Int };
> type Public  = Omit<User, password>;   // { id, name }
> type Id      = Pick<User, id>;         // { id }
> type Tracked = Merge<User, Audit>;     // fields of both (B wins on conflict)
> ```
> `Omit`/`Pick`/`Merge` are pure type-level functions that evaluate to a concrete
> record and are **fully erased before codegen** — the shared resolver in
> `vyrn-frontend::types` reduces them, so the checker and both backends treat the
> result as an ordinary record (width subtyping, struct literals, native lowering
> all work over derived shapes). Unknown transformer keys are a compile error.
> See `examples/utility.vyrn`.
>
> **User-defined enums / sum types (§4)** are implemented end to end, including
> native code:
> ```vyrn
> type Shape = | Circle(Int) | Square(Int) | Unit;
> fn area(s: Shape) -> Int {
>     return match s { Circle(r) => 3*r*r, Square(w) => w*w, Unit => 0 };
> }
> ```
> A variant carries an optional single payload (Int in the native backend). `match`
> must cover every variant (exhaustive) and binds payloads. Constructors are the
> variant names (`Circle(2)`, `Unit`); variant names are global and must be
> unique. Native lowering: a `{ i64 tag, i64 payload }` aggregate; `match` is an
> LLVM `switch` on the tag + `phi`. `Option`/`Result` remain distinct built-ins.
> See `examples/enum.vyrn`.
>
> **Generics (§6)** — functions *and* types — are implemented end to end,
> including native code:
> ```vyrn
> fn id<T>(x: T) -> T { return x; }
> type Box<T> = { value: T };
> fn unbox<T>(b: Box<T>) -> T { return b.value; }
> let n = Box { value: 41 };   // Box<Int>, inferred from the field
> ```
> - A type parameter is opaque while the body is checked (no arithmetic/field
>   access on `T`) and inferred at the call/construction site.
> - **Generic functions** monomorphize to one specialized machine function per
>   concrete type (`vyrn_id__Int(i64)`, `vyrn_id__Str(ptr)`); generic‑calls‑generic
>   works via substitution in the worklist; only reachable instantiations are
>   emitted.
> - **Generic types** need no separate functions: after substituting arguments
>   they're concrete records that the record backend already lowers
>   (`Box<Int>` → `{ i64 }`, `Pair<Int, String>` → `{ i64, ptr }`). Value‑position
>   type arguments are **inferred from the fields** (no turbofish, which would
>   collide with `<` in expressions); type‑position uses the explicit `Box<Int>`.
>
> **Generic enums** complete the picture — `type Opt<T> = | Wrap(T) | Empty;`
> with the type argument inferred from a payload variant (`Wrap(41)` → `Opt<Int>`)
> or taken from context for a nullary one. The native backend needed **no changes**
> for these: an enum's `{ i64 tag, i64 payload }` layout is independent of its type
> parameters. With this, `Option`/`Result` could be ordinary library types rather
> than built‑ins.
>
> **Not yet implemented:** constrained type params; `Partial`/`Readonly`
> transformers; intersection syntax (`A & B`); anonymous record types in
> signatures; nominal `nominal type`; casts/checked narrowing (§3); multi‑payload
> variants; protocols (§5). The excluded Inkwell backend rejects records/enums/
> generics (the text‑IR backend is the feature‑complete native path).

---

## Summary

Vyrn's type system is **structural by default**, with **nominal opt-in**,
**no unchecked casts**, first-class **compile-time type transformations**, unions
and pattern matching, and practical generics. The guiding constraint from
RFC-0001: types communicate *what operations are available and what a valid value
looks like*, not merely the shape of memory.

---

## 1. Structural typing

Compatibility is based on structure, not declared name.

```vyrn
type User = {
    name: String,
}

type Employee = User & {
    salary: Int,
}
```

An `Employee` is usable anywhere a `User` is expected — **no cast**, because
`Employee` contains everything `User` requires (width subtyping).

```vyrn
let e: Employee = Employee { name: "Ada", salary: 100 }
let u: User = e          // OK: Employee ⊇ User
```

The reverse is a **compile error**, not a cast:

```vyrn
let u: User = ...
let e: Employee = u      // ERROR: User is missing `salary`
```

### Layout consequence
Structural compatibility at the *type* level does not imply *representation*
interchangeability. When a function takes `User` and receives an `Employee`, the
compiler passes a **view** that exposes `name`; it does not reinterpret an `Employee`
as some other struct with a different layout. This is why width subtyping is safe
here but a "reinterpret cast" never is. → RFC-0004 for how views interact with
capabilities.

## 2. Nominal opt-in

When identity matters (units, IDs, domain distinctions that share a shape),
declare a nominal type. Two nominal types with identical structure are **not**
interchangeable.

```vyrn
nominal type UserId = String
nominal type SessionId = String
// UserId and SessionId never mix, despite both being String-shaped.
```

### String encoding (decision)

A `String` is an **immutable sequence of UTF-8 bytes**. `s.length` is the
**byte** count — identical to the number of code points for ASCII, but larger
for multi-byte text (`"é".length == 2`, `"日".length == 3`). The regex engine
is byte-wise (`.` matches one byte), and source files are read as UTF-8.

This makes `.length` and indexing O(1) with no hidden scan, at one documented
cost: a `String where` predicate's length bounds are byte bounds, so when they
are emitted as JSON-Schema `minLength`/`maxLength` (RFC-0003 / `schemaOf`) the
numbers pass through unchanged and therefore bound **bytes**, whereas a JSON
validator counts UTF-16 code units. For ASCII these agree; for non-ASCII text
they deliberately differ. A code-point-counting `.length` was rejected because
it would turn every length query into an O(n) walk.

## 3. No casts

There is no `as`. Every conversion is one of:

- **Structural widening** — automatic, zero-cost (`Employee → User`).
- **Compiler-proven** — the compiler shows equivalence, zero-cost.
- **Runtime-checked narrowing** — returns `Option<T>` / `Result<T, E>`.

```vyrn
// narrowing a wider/unknown value: checked, never blind
match value {
    Employee(emp) => ...,   // runtime verified to have `salary`
    User(usr)     => ...,
}
```

or:

```vyrn
let maybe: Option<Employee> = value as? Employee   // syntax TBD; result is Option
```

**Never** a silent reinterpretation of memory.

## 4. Unions & pattern matching

Tagged unions are a core type. Pattern matching is exhaustive; the compiler
errors on missing arms.

```vyrn
type Shape =
    | Circle { r: Float }
    | Rect { w: Float, h: Float }

fn area(s: Shape) -> Float {
    match s {
        Circle { r }   => 3.14159 * r * r,
        Rect { w, h }  => w * h,
    }   // no default needed; both arms present ⇒ exhaustive
}
```

`Option<T>` and `Result<T, E>` are ordinary unions (→ RFC-0005), so `match`
handles them with no special cases.

## 5. Protocols (no inheritance)

Behavior is shared through **protocols**, satisfied structurally. There is no
class inheritance (RFC-0001 non-goal).

```vyrn
protocol Drawable {
    fn draw(self: read Self, canvas: modify Canvas)
}

// Any type with a matching `draw` satisfies Drawable — no `implements` needed,
// though an explicit `impl` is allowed for documentation and coherence.
```

Intersection composes protocols cleanly, replacing deep hierarchies:

```vyrn
fn render(x: Drawable & Serializable) { ... }
```

## 6. Generics

Practical, constraint-based generics. Constraints are protocols.

```vyrn
fn max<T: Ord>(a: T, b: T) -> T {
    if a > b { a } else { b }
}
```

Focus is expressiveness for everyday code — **not** HKTs or dependent theorem
proving (RFC-0001 non-goals). Const generics (`Array<T, N>`) are specified in
RFC-0003.

## 7. Compile-time type transformations (utility types)

First-class, and — unlike TypeScript, where they are library-level — part of the
language. They are pure functions from types to types, erased before codegen.

```vyrn
type PublicUser = Omit<User, "password">
type Editable   = Partial<User>
type Frozen     = Readonly<User>
type WithoutMeta<T> = Omit<T, "createdAt" | "updatedAt">
```

Mapped/derived forms:

```vyrn
type Api<T> = Async<Readonly<Omit<T, "password">>>

type OnlyStrings<T> = Filter<T, IsString>
```

### Built-in transformer set (v1 target)
`Partial<T>`, `Required<T>`, `Readonly<T>`, `Pick<T, Keys>`, `Omit<T, Keys>`,
`Merge<A, B>`, `Map<T, F>`, `Filter<T, Pred>`, `Keys<T>`.

**Invariant:** every transformer resolves to a concrete layout at compile time.
None of them exist at runtime.

---

## Interaction notes

- With **validated types** (RFC-0003): `Omit`/`Pick` preserve the `where` clauses
  of the fields they keep; a `Partial<T>` makes each field independently optional
  but does **not** drop its validation when present.
- With **capabilities** (RFC-0004): a structural view (`Employee → User`) carries
  the *narrower* capability set; you cannot gain `modify` on a field the source
  only lent as `read`.

## Open questions

- **Q1.** Syntax for checked narrowing (`as?` vs `match`-only). Leaning
  `match`-only for v1 to keep one obvious way.
- **Q2.** Do we allow *anonymous* structural types in signatures
  (`fn f(x: { name: String })`) in v1, or require named types first?
- **Q3.** Coherence for structural protocol satisfaction: if two protocols both
  match a type structurally and conflict, how is dispatch resolved? (Likely:
  require explicit `impl` when ambiguous.)
- **Q4.** How far does the transformer language go — is it Turing-complete
  (TypeScript-style, with recursion limits) or deliberately total?
