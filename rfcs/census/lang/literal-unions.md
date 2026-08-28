# Census: unions of types with fixed property values

The owner's question: can a type be a union where one property is fixed to a known value, so a test on that property tells the compiler which member a value is? And what does that do to `Array<TheUnion>`?

This file surveys eleven languages on five questions, then records what Vyrn has today and lays out the options. It recommends nothing.

## The five questions

1. **Select by one field?** Can a member be chosen by testing a single field (a tag, a discriminant, a literal property)?
2. **Exhaustive?** Does the compiler check that the test covers every member?
3. **Narrow?** Inside the branch, does the compiler know the value is that one member?
4. **`Array<TheUnion>`?** Is the array covariant, invariant, or refused? Can `Array<Member>` pass where `Array<TheUnion>` is wanted? What breaks if it can?
5. **Add a member?** Can a member be added without breaking every existing match?

---

## TypeScript

TypeScript discriminated unions are the reference design. A union of object types shares a literal-typed property — the discriminant — and a narrowing read of that property collapses the union to one member.

```ts
type Shape =
  | { kind: "circle"; r: number }
  | { kind: "rect"; w: number; h: number }
  | { kind: "empty" };

function area(s: Shape): number {
  switch (s.kind) {
    case "circle": return Math.PI * s.r * s.r;   // s is { kind:"circle"; r:number }
    case "rect":   return s.w * s.h;
    case "empty":  return 0;
  }
}
```

- **Select by one field?** Yes. `s.kind` is the discriminant. A `switch`, an `if (s.kind === "circle")`, or the `in` operator all narrow. https://www.typescriptlang.org/docs/handbook/unions-and-intersections.html#discriminating-unions
- **Exhaustive?** Partially. A `switch` on the discriminant narrows in each case, but TypeScript does not force a default arm unless the result type demands it. The standard trick is a `never`-assigning default: `const assertNever = (x: never): never => { throw new Error }; default: return assertNever(s);`. Adding a member then turns the default's `s` from `never` into the new member, a compile error at the call site. The exhaustiveness check is opt-in, not enforced.
- **Narrow?** Yes. Inside `case "circle"`, `s` has type `{ kind: "circle"; r: number }`. Narrowing also works through `Array.prototype.filter` type guards and user-defined type predicates (`x is Circle`).
- **`Array<TheUnion>`?** `Array<A | B>` is the type of an array whose elements are each `A` or `B`. Arrays are **covariant** in TypeScript: `Array<Circle>` is assignable to `Array<Shape>` when `Circle` (an object with `kind:"circle"`) is assignable to `Shape`. This is unsound for mutation — `shapes.push({ kind: "rect", w: 1, h: 1 })` on an array that is really `Array<Circle>` stores a `Rect` where only `Circle` values live — but TypeScript accepts it because its arrays are read-mostly in practice and the variance is inherited from the function-parameter variance rules. `Array<A>` cannot be passed where `Array<A | B>` is wanted *if* the direction is reversed (an `Array<Shape>` is not an `Array<Circle>`). The covariance is one-way: subtype element → subtype array. What goes wrong is the Java-array disease: a write through the wider type can store a member the narrower array cannot hold, and a later read through the narrower type crashes or returns a value of the wrong member. TypeScript allows it; the runtime does not catch it.
- **Add a member?** Yes, without touching existing `switch` arms — but only the `assertNever` default notices. Without that default, every match silently stays open and the new member falls through.

## Rust

Rust enums are tagged unions with struct-like or tuple-like variants. The tag is implicit; the compiler selects a variant by name, not by a user-visible field.

```rust
enum Shape {
    Empty,
    Circle { r: f64 },
    Rect { w: f64, h: f64 },
}
fn area(s: &Shape) -> f64 {
    match s {
        Shape::Empty => 0.0,
        Shape::Circle { r } => std::f64::consts::PI * r * r,
        Shape::Rect { w, h } => w * h,
    }
}
```

- **Select by one field?** No, not by a field the user reads. The discriminant is an internal tag; selection is by variant name in a `match` or `if let`. There is no surface property to test.
- **Exhaustive?** Yes, strictly. A `match` must cover every variant or carry a `_` catch-all. Adding a variant without a catch-all is a compile error at every `match`. https://doc.rust-lang.org/reference/expressions/match-expr.html
- **Narrow?** Yes. In `Shape::Circle { r }`, `r` has type `f64`, and the value is statically that variant.
- **`Array<TheUnion>`?** `Vec<Shape>` holds any variant per element. Arrays and `Vec` are **invariant** in Rust: `Vec<CircleLike>` is not a type here (variants are not separate types), so the question as posed does not arise. For generic containers, `Vec<T>` is invariant in `T` because `T` may be mutated. `&[T]` (a shared slice) is covariant in `T` because it is read-only. `&mut [T]` is invariant. A `Vec<A>` cannot be passed where `Vec<A | B>` is wanted — there is no subtyping between distinct enum types, and even with lifetimes the invariance of `Vec` blocks it. Nothing goes wrong, because the type system refuses the conversion.
- **Add a member?** Breaks every exhaustive `match` without a `_` arm. This is deliberate: the compiler forces every call site to decide what the new variant means.

## Swift

Swift enums carry associated values. Like Rust, the discriminant is internal and selection is by `case` pattern, not by a user field.

```swift
enum Shape {
    case empty
    case circle(radius: Double)
    case rect(width: Double, height: Double)
}
func area(_ s: Shape) -> Double {
    switch s {
    case .empty: return 0
    case .circle(let r): return .pi * r * r
    case .rect(let w, let h): return w * h
    }
}
```

- **Select by one field?** No. The tag is implicit; `switch case` selects.
- **Exhaustive?** Yes. A `switch` must be exhaustive; the compiler errors on a missing case. https://docs.swift.org/swift-book/LanguageGuide/Enumerations.html
- **Narrow?** Yes. In `case .circle(let r)`, the associated value is bound and the value is that case.
- **`Array<TheUnion>`?** `[Shape]` holds any case per element. Swift generics are **invariant** by default; `[Circle]` is not a type (cases are not standalone types). There is no subtyping between enum cases, so `Array<Case>` passing for `Array<Enum>` is not expressible. Swift arrays are value types (copy-on-write), so even a covariant read would be safe — but the language does not rely on that; it refuses the conversion. Nothing goes wrong.
- **Add a member?** Breaks every `switch` without a `default`. Deliberate.

## Kotlin

Kotlin sealed classes are a nominal closed hierarchy. A `when` over a sealed type is exhaustive when used as an expression.

```kotlin
sealed class Shape {
    object Empty : Shape()
    data class Circle(val r: Double) : Shape()
    data class Rect(val w: Double, val h: Double) : Shape()
}
fun area(s: Shape): Double = when (s) {
    is Shape.Empty -> 0.0
    is Shape.Circle -> Math.PI * s.r * s.r
    is Shape.Rect -> s.w * s.h
}
```

- **Select by one field?** No. Selection is by `is` type check, not by a property value. There is no literal discriminant field; the runtime class tag is the implicit selector.
- **Exhaustive?** Yes, for `when` used as an expression over a sealed type (since Kotlin 1.1). A statement `when` is not checked for exhaustiveness. https://kotlinlang.org/docs/sealed-classes.html
- **Narrow?** Yes. After `is Shape.Circle`, `s` is smart-cast to `Shape.Circle` and `s.r` is visible.
- **`Array<TheUnion>`?** `Array<Shape>` holds any subclass per element. Kotlin declaration-site variance: `Array<T>` is **invariant** (it is a Java array under the hood on the JVM, and Java arrays are covariant, but Kotlin's `Array` type is invariant to close that hole). `Array<out Shape>` is covariant (read-only). A `List<Shape>` is covariant (`List<out E>`). So `List<Circle>` IS assignable to `List<Shape>` — covariance — and this is sound because `List` is read-only (no `add`). `MutableList<Shape>` is invariant; `MutableList<Circle>` cannot be passed for `MutableList<Shape>`. What goes wrong if a mutable covariant list were allowed: the Java `ArrayStoreException` — storing a `Rect` into a `Circle[]` throws at runtime. Kotlin prevents it at the type level by making mutable containers invariant.
- **Add a member?** Breaks every exhaustive `when` expression (the compiler reports the missing branch). Adding a sealed subclass in the same file or module is the only way to extend.

## Java (sealed interfaces, pattern matching)

Java 17 sealed interfaces with `permits`, and pattern matching `switch` (finalized in Java 21).

```java
sealed interface Shape permits Shape.Empty, Shape.Circle, Shape.Rect {
    record Empty() implements Shape {}
    record Circle(double r) implements Shape {}
    record Rect(double w, double h) implements Shape {}
}
static double area(Shape s) {
    return switch (s) {
        case Shape.Empty e -> 0;
        case Shape.Circle c -> Math.PI * c.r() * c.r();
        case Shape.Rect r -> r.w() * r.h();
    };
}
```

- **Select by one field?** No. Selection is by `case Type` pattern, which tests the runtime class. A record's components are accessed after the match, not used to discriminate.
- **Exhaustive?** Yes. A pattern `switch` over a sealed type is exhaustive if it covers every permitted subtype; the compiler infers a default that throws `MatchException` only when coverage is incomplete, but for sealed types it reports the missing case at compile time. https://openjdk.org/jeps/441
- **Narrow?** Yes. In `case Shape.Circle c`, `c` has compile-time type `Shape.Circle`.
- **`Array<TheUnion>`?** `Shape[]` holds any permitted subtype per element. Java arrays are **covariant**: `Circle[]` is assignable to `Shape[]`. This is unsound for stores: `shapes[0] = new Rect(...)` throws `ArrayStoreException` at runtime — the runtime guards every store against the array's true element type. `List<Shape>` is invariant at the type level (`List<Circle>` is not a `List<Shape>`), but `List<? extends Shape>` is the covariant wildcard (read-only). So the array is the unsound covariant container (runtime-guarded), and the generic `List` is invariant (compile-time refused). What goes wrong: the runtime check is the only thing between covariance and corruption. It works, but it is a tax on every store.
- **Add a member?** Breaks every exhaustive pattern `switch` (the compiler reports the missing case) and requires the new subtype in the `permits` list.

## Scala 3

Scala 3 has both nominal sum types (`enum` with cases) and structural union types (`A | B`).

```scala
enum Shape:
  case Empty
  case Circle(r: Double)
  case Rect(w: Double, h: Double)

def area(s: Shape): Double = s match
  case Shape.Empty => 0
  case Shape.Circle(r) => math.Pi * r * r
  case Shape.Rect(w, h) => w * h
```

Scala 3 also allows `String | Int` as a first-class union type, narrowed by `if` type tests.

- **Select by one field?** For `enum`, no — selection is by case pattern. For a union of case objects or literal-like types, an `isInstanceOf` check or a `match` type pattern selects. There is no literal-property discriminant in the Scala `enum` model.
- **Exhaustive?** Yes for `enum` `match` (the compiler checks coverage). For structural unions, exhaustiveness is checked when the scrutinee is a sealed sum or a union with a known finite set. https://docs.scala-lang.org/scala3/reference/enums/enums.html
- **Narrow?** Yes. In `case Shape.Circle(r)`, `r` is `Double` and the value is that case.
- **`Array<TheUnion>`?** Scala arrays are Java arrays — **covariant** and runtime-guarded (`ArrayStoreException`). `Array[Shape]` holds any case. `Array[Circle]` is assignable to `Array[Shape]` (covariance), and a store of a wrong case throws at runtime. Scala's generic `List` is invariant (`List[Circle]` is not `List[Shape]`); the covariant form is `List[? <: Shape]`. For structural union types, `Array[A | B]` is an array whose elements are `A` or `B`; `Array[A]` is a subtype of `Array[A | B]` by covariance, with the same runtime-store guard. What goes wrong: same as Java — runtime `ArrayStoreException` is the safety net.
- **Add a member?** Breaks every exhaustive `match` over the `enum`. A structural union `A | B` widened to `A | B | C` breaks any exhaustive match that assumed only two members.

## Flow

Flow's disjoint unions use literal property values as discriminants, the same shape as TypeScript.

```js
type Shape =
  | { kind: "circle", r: number }
  | { kind: "rect", w: number, h: number }
  | { kind: "empty" };

function area(s: Shape): number {
  switch (s.kind) {
    case "circle": return Math.PI * s.r * s.r;
    case "rect":   return s.w * s.h;
    case "empty":  return 0;
    default:       return 0;
  }
}
```

- **Select by one field?** Yes. `s.kind` with a literal value is a "refinement" that Flow recognizes as a disjoint-union discriminant. https://flow.org/en/docs/types/unions/
- **Exhaustive?** No. Flow does not enforce exhaustiveness on a `switch`; the `default` is the user's responsibility. Adding a member does not break existing code.
- **Narrow?** Yes. Inside `case "circle"`, `s` is refined to the circle member, and `s.r` is `number`.
- **`Array<TheUnion>`?** `$ReadOnlyArray<Shape>` is covariant; `Array<Shape>` is invariant in Flow (Flow treats mutable arrays as invariant, unlike TypeScript). So `Array<Circle>` cannot be passed where `Array<Shape>` is wanted — Flow refuses it. `$ReadOnlyArray<Circle>` can be passed where `$ReadOnlyArray<Shape>` is wanted (covariance, sound because read-only). What goes wrong if a mutable array were covariant: the array-store corruption; Flow avoids it by making mutable arrays invariant.
- **Add a member?** No breakage. Existing matches keep their `default`; the new member falls through.

## Python (`Literal`, `TypedDict`, `Union`)

Python's type hints support `Literal["circle", "rect"]` and `Union` of `TypedDict`s, checked by mypy or pyright.

```python
from typing import Literal, Union, TypedDict

class Circle(TypedDict):
    kind: Literal["circle"]
    r: float
class Rect(TypedDict):
    kind: Literal["rect"]
    w: float
    h: float
Shape = Union[Circle, Rect]

def area(s: Shape) -> float:
    if s["kind"] == "circle":
        return 3.14 * s["r"] ** 2
    return s["w"] * s["h"]
```

- **Select by one field?** Yes. A `Literal`-typed key on a `TypedDict` is a discriminant that mypy/pyright narrow on. https://typing.python.org/en/latest/spec/literal.html
- **Exhaustive?** No. Python type checkers do not enforce exhaustiveness; an `else` or early return is the user's responsibility. Pyright has an opt-in `reportMissingCaseInUnion` but it is not default.
- **Narrow?** Yes in pyright, partially in mypy. After `s["kind"] == "circle"`, pyright narrows `s` to `Circle` and `s["r"]` is `float`.
- **`Array<TheUnion>`?** `list[Shape]` is the type of a list whose elements are each `Shape`. Python's `list` is **invariant** in type checkers: `list[Circle]` is not `list[Shape]`. (Python's runtime does not enforce types at all; a `list[Circle]` and a `list[Shape]` are the same object at runtime.) The covariant read-only form is `Sequence[Shape]` (covariant). So `list[Circle]` cannot be passed where `list[Shape]` is wanted — refused by the checker. What goes wrong if it were allowed: nothing at runtime (Python is dynamically typed), but the static guarantee is lost. The checkers keep the guarantee by refusing.
- **Add a member?** No breakage. Existing `if`/`else` chains keep their fallback; the new member hits the `else`.

## Haskell (GADTs)

Haskell GADTs let each constructor carry a precise return type, so the type checker learns the payload type from the constructor alone.

```haskell
data Shape a where
    Empty  :: Shape ()
    Circle :: Double -> Shape Double
    Rect   :: Double -> Double -> Shape (Double, Double)

area :: Shape a -> Double
area s = case s of
    Empty      -> 0
    Circle r   -> pi * r * r
    Rect w h   -> w * h
```

- **Select by one field?** No. Selection is by constructor pattern. The constructor name IS the tag; there is no field to read.
- **Exhaustive?** Yes. A `case` must cover every constructor (with no catch-all, or the catch-all disables the precision). https://downloads.haskell.org/ghc/latest/docs/users_guide/exts/gadt.html
- **Narrow?** Yes, and more: a GADT match refines the *type index* `a`, so inside `Circle r` the type `a` is known to be `Double`. This is the strongest narrowing in the survey.
- **`Array<TheUnion>`?** Haskell `Array` and `Vector` are invariant in their element type (no subtyping exists in Haskell, so variance is moot — all types are equal or unrelated). `Array (Shape a)` holds one element type. There is no `Array<Shape>` across all `a` because `Shape a` for different `a` are different types. A heterogenous container needs an existential wrapper. Nothing goes wrong because the type system refuses the mixing.
- **Add a member?** Breaks every `case` without a catch-all. Deliberate.

## OCaml (polymorphic variants)

OCaml polymorphic variants are open, structural, and tagged by name.

```ocaml
type shape = [ `Circle of float | `Rect of float * float | `Empty ]

let area = function
  | `Empty -> 0.0
  | `Circle r -> pi *. r *. r
  | `Rect (w, h) -> w *. h
```

- **Select by one field?** No. Selection is by variant tag (the `` `Circle `` name). The tag is the discriminant, but it is not a user-visible field.
- **Exhaustive?** Yes for closed variant types. Polymorphic variants can also be open (`[> `Circle of float ]`), in which case a `function` does not need to cover every tag. When the type is closed (`[ `A | `B ]`), the compiler checks exhaustiveness. https://v2.ocaml.org/manual/polyvariant.html
- **Narrow?** Yes. In `` `Circle r ``, `r` is `float`.
- **`Array<TheUnion>`?** OCaml arrays are invariant (no subtyping). `shape array` holds any variant per element. There is no `Array<`A>` as a subtype of `Array<`A | `B>` — OCaml has no subtyping between variant types. A subtype relation exists only through explicit coercion (`(x :> t)`), and arrays do not participate covariantly. Nothing goes wrong.
- **Add a member?** For a *closed* variant type, adding a tag breaks every match. For an *open* polymorphic variant, adding a tag does not break existing matches (they keep their fallthrough). This is the one language in the survey where openness is a first-class choice.

## Zig (tagged unions)

Zig tagged unions pair an enum tag with a union body. The tag is a named field the program reads.

```zig
const Shape = union(enum) {
    empty: void,
    circle: f64,
    rect: struct { w: f64, h: f64 },
    fn area(self: Shape) f64 {
        return switch (self) {
            .empty => 0,
            .circle => |r| 3.14159 * r * r,
            .rect => |r| r.w * r.h,
        };
    }
};
```

- **Select by one field?** Yes, indirectly. `self` has a tag the program can read (`std.meta.activeTag(self)`), and `switch` on the union dispatches on it. The tag is a real value, not just a pattern name.
- **Exhaustive?** Yes. A `switch` on a tagged union must cover every tag or have an `else`. https://ziglang.org/documentation/master/#Tagged-unions
- **Narrow?** Yes. In `.circle => |r|`, `r` is `f64` and the payload is that of the circle member.
- **`Array<TheUnion>`?** Zig arrays are invariant (no subtyping in Zig). `[]Shape` holds any tag per element. There is no subtype relation between a single-tag union and the full union, so `Array<`Circle-only>` passing for `Array<Shape>` is not expressible. Nothing goes wrong.
- **Add a member?** Breaks every `switch` without `else`. Deliberate.

---

## Summary table

| language | select by one field | exhaustive | narrow | `Array<U>` variance | `Array<Member>` for `Array<U>` | add a member |
|---|---|---|---|---|---|---|
| TypeScript | yes (literal prop) | opt-in (`never` default) | yes | covariant (unsound for writes) | yes (subtype) | no breakage without `never` default |
| Rust | no (internal tag) | yes | yes | invariant | refused | breaks every match |
| Swift | no (internal tag) | yes | yes | invariant | refused | breaks every switch |
| Kotlin | no (`is` type test) | yes (expression `when`) | yes | invariant (`Array`); covariant (`List`, read-only) | refused for `Array`; yes for `List` | breaks expression `when` |
| Java | no (`case Type`) | yes (pattern `switch`) | yes | covariant, runtime-guarded | yes (with `ArrayStoreException`) | breaks pattern `switch` |
| Scala 3 | no (case pattern) | yes | yes | covariant, runtime-guarded (Java arrays) | yes (with `ArrayStoreException`) | breaks `match` |
| Flow | yes (literal prop) | no | yes | invariant (mutable); covariant (read-only) | refused for mutable | no breakage |
| Python | yes (`Literal` key) | no (opt-in) | yes (pyright) | invariant (checker) | refused | no breakage |
| Haskell | no (constructor) | yes | yes + type index | invariant (no subtyping) | refused | breaks `case` |
| OCaml | no (variant tag) | yes (closed) / no (open) | yes | invariant | refused | breaks closed; safe for open |
| Zig | yes (tag is a value) | yes | yes | invariant | refused | breaks `switch` |

The pattern: languages with a user-visible literal discriminant (TypeScript, Flow, Python) do not enforce exhaustiveness by default, because the union is structural and open. Languages with a nominal tag (Rust, Swift, Kotlin, Java, Scala, Haskell, OCaml-closed, Zig) enforce exhaustiveness, because the set of members is closed and known. The array question splits three ways: invariant and refused (Rust, Swift, Flow-mutable, Python, Haskell, OCaml, Zig), covariant and runtime-guarded (Java, Scala), and covariant and unsound (TypeScript). The covariant-and-unsound camp is the one the owner's "what goes wrong" question targets.

---

## What Vyrn has today

Vyrn has nominal enums, not structural unions. A sum type is declared with a leading `|` and one variant per arm, each carrying zero or more payload types:

```
type Shape = | Dot | Circle(Int64) | Rect(Int64, Int64)
```

The AST node is `Type::Enum(Vec<EnumVariant>)` (`compiler/vyrn-frontend/src/ast.rs:785`), where `EnumVariant { name, payload: Vec<Type> }` (`compiler/vyrn-frontend/src/ast.rs:275-278`). The parser requires the leading `|` to disambiguate enums from other type forms (`compiler/vyrn-frontend/src/parser.rs:2625-2647`). Real examples: `type Tree = | Leaf | Node(Tree, Tree)` (`examples/binarytrees.vyrn:24`), `type Color = | Red | Green` (`examples/jsondecbytes.vyrn:33`), `type Role = | Guest | Member | Admin` (`examples/jsoncodec.vyrn:21`).

There is no structural union type `A | B` of arbitrary types. The `Type` enum has no `Union` variant (`compiler/vyrn-frontend/src/ast.rs:686`). The `|` in type position means "enum variant list," nothing else.

### Selection and narrowing

A `match` over an enum dispatches to `check_match_enum` (`compiler/vyrn-frontend/src/checker.rs:5033`). Selection is by **variant name**, not by a field the user reads. The pattern `Circle(r)` names the variant and binds its payload. Inside the arm, each binder gets the payload type (`compiler/vyrn-frontend/src/checker.rs:5085-5093`):

```rust
for (bname, pty) in bind.iter().zip(&ev.payload) {
    inner.last_mut().unwrap().insert(bname.clone(),
        Binding { ty: pty.clone(), mutable: false });
}
```

So in `Circle(r) => 3 * r * r`, `r` has type `Int64`. Narrowing is by construction: the arm is reached only for that variant, and the binders carry that variant's payload types. There is no discriminant field to test; the variant name is the test.

### Exhaustiveness

`check_match_enum` requires every variant covered exactly once. A missing variant is a hard error (`compiler/vyrn-frontend/src/checker.rs:5098-5101`):

```rust
for v in evs {
    if !seen.contains(&v.name) {
        return Err(cerr!(line, "`match` is missing variant `{}`", v.name));
    }
}
```

A duplicate arm is also rejected (`compiler/vyrn-frontend/src/checker.rs:5069-5071`). There is no wildcard or `_` catch-all — the match must name every variant. Confirmed by smoke test: `vyrn check` on a `Shape` match missing `Rect` reports `` `match` is missing variant `Rect` `` (measured, `N:/lang` working copy, `compiler/target/release/vyrn check /tmp/enum_missing.vyrn`).

### Adding a member

Because every `match` must name every variant, adding a variant to an enum breaks every `match` over it. This is the Rust/Swift position, not the TypeScript position. There is no opt-out.

### Array variance

`Array<T>` is covariant in `T`. The assignability rule (`compiler/vyrn-frontend/src/checker.rs:2118-2119`):

```rust
if let (Type::Array(a), Type::Array(b)) = (from, to) {
    return self.assignable_d(a, b, depth + 1);
}
```

`SmallArray<T, N>` is covariant in `T` and invariant in `N` (`compiler/vyrn-frontend/src/checker.rs:2121-2124`). `Map<K, V>` is covariant in both (`compiler/vyrn-frontend/src/checker.rs:2115-2116`). `Option` and `Result` are covariant in their payloads (`compiler/vyrn-frontend/src/checker.rs:2107-2112`).

Arrays are mutable. The surface supports `arr.push(x)`, `arr[i] = v`, `arr.pop()`, `arr.swapRemove(i)` (`examples/arrays.vyrn:5-13`, `examples/arrays.vyrn:62-63`). Confirmed by smoke test: `Array<Wide>` (a record with fields `a`, `b`) passes where `Array<Narrow>` (a record needing only `a`) is wanted — `vyrn check` accepts it (`N:/lang` working copy, `compiler/target/release/vyrn check /tmp/recovar.vyrn`, `ok`). So a wider element array flows into a narrower element array slot. The covariance is real and reachable.

This is the TypeScript/Java situation, not the Rust situation. Because the enum is nominal and closed, the covariance does not corrupt the discriminant — an `Array<Shape>` holds `Shape` values whose internal tag is intact. The unsoundness the covariance opens is the record-width one (a `Narrow` pushed into an `Array<Wide>` alias, then `.b` read), not a tag one. But the variance rule is the same rule that would let `Array<CircleOnly>` flow into `Array<Shape>` if such a type existed — and it does not exist today, because variants are not standalone types.

### What would have to change

The owner's question has two halves Vyrn does not yet answer:

1. **A property fixed to a known value.** Vyrn has no literal type and no field-value discriminant. The closest thing is a validated string type whose `where value =~ "lit"` predicate is a pure regex conjunction — a "finite string type" (`compiler/vyrn-frontend/src/finite.rs:1-19`, `compiler/vyrn-frontend/src/finite.rs:126-132`). A finite string type is a closed set of string values, and the compiler can enumerate it (`compiler/vyrn-frontend/src/finite.rs:317-319`). But it is a refinement of one scalar, not a tag that selects a payload shape. There is no `match` on a finite string type's values, and no narrowing of a surrounding record by one. A discriminated-union feature would need a discriminant field with a literal type, a narrowing rule on a read of that field, and (if exhaustive) a way to enumerate the members.

2. **`Array<TheUnion>`.** Vyrn's `Array<T>` is covariant and mutable. If `TheUnion` were a structural union `A | B` and `A` were a subtype of `A | B`, then `Array<A>` would be assignable to `Array<A | B>` by the rule at `compiler/vyrn-frontend/src/checker.rs:2118-2119` — and a `push` of a `B` through the wider alias would corrupt the array. That is the unsoundness. The language has no defense today: there is no `&mut [T]`-style invariant container, no read-only array type, and no runtime store guard. Adding a structural union without changing array variance inherits the TypeScript disease.

---

## The options

RECOMMENDATION, NOT A DECISION.

Each design is scored against the owner's five questions and against what it costs in the parser, the checker, lowering, what it breaks, and who else does it.

### Design A — nominal enum only, as today

Vyrn keeps its nominal `| V | V(T)` enum. No structural unions, no literal discriminant. The member is selected by variant name in `match`. Exhaustiveness is enforced. `Array<Enum>` is covariant and the question does not arise because variants are not standalone types.

| design | description | parser cost | checker cost | lowering cost | breaks | who else |
|---|---|---|---|---|---|---|
| A: nominal enum only | keep `| V | V(T)`, no literal discriminant, no structural union | none | none | none | nothing (status quo) | Rust, Swift, OCaml-closed, Zig |

Costs: none. The owner gets exhaustive narrowing by variant name, but not the "test one field" shape they asked for. Adding a member breaks every match. Arrays are covariant but the variant-not-a-type rule keeps the unsoundness at the record-width level, not the tag level.

### Design B — discriminated records: a literal-typed field as discriminant

Add a structural union of records that share a `kind` field of literal type. A `match` or `if` on `x.kind == "circle"` narrows `x` to the record whose `kind` is `"circle"`. Exhaustiveness is enforced by enumerating the literal values of the discriminant.

```
type Shape =
  | { kind: "circle", r: Int64 }
  | { kind: "rect", w: Int64, h: Int64 }
  | { kind: "empty" }
```

| design | description | parser cost | checker cost | lowering cost | breaks | who else |
|---|---|---|---|---|---|---|
| B: discriminated records | literal `kind` field selects a record member; `match`/`if` on it narrows | a `|` between record types in type position (reuse the enum `|`); a literal type `Lit("circle")` | a narrowing rule: a read of a literal-discriminant field collapses the union to one member; an exhaustiveness check over the literal value set | a runtime tag or a per-field test; the record already carries `kind` as a stored field | nothing in existing enums (separate syntax); existing `Array` variance now applies to a real subtyping relation | TypeScript, Flow, Python |

Costs: the parser gains a literal type and a union-of-records form. The checker gains the hardest piece — a narrowing pass that tracks "after `x.kind == "circle"`, `x` has the circle record type." The lowering stores `kind` as a field (no hidden tag), so a member test is a string compare unless the compiler interns the literals. Adding a member means adding a record and a literal value to the discriminant's set; every exhaustive `match` breaks (TypeScript-with-`never` behavior). `Array<Shape>` is covariant: `Array<{ kind: "circle", r: Int64 }>` would be a subtype of `Array<Shape>`, and `push` of a `rect` through the alias corrupts it. The owner's array question lands squarely on the TypeScript side.

### Design C — discriminated records with invariant arrays

Design B, but `Array<T>` becomes invariant when `T` is a union, or `Array<T>` becomes invariant always (with a separate covariant `ReadArray<T>` or `Array<T>`-as-slice for read-only contexts). This closes the unsoundness.

```
fn feed(ps: Array<Shape>) -> Int64 { ... }
// Array<{ kind: "circle" }> is NOT assignable to Array<Shape>
// a read-only view would be: fn peek(ps: ReadArray<Shape>) -> Int64
```

| design | description | parser cost | checker cost | lowering cost | breaks | who else |
|---|---|---|---|---|---|---|
| C: discriminated records + invariant arrays | as B, but `Array<T>` invariant (or invariant for union `T`) | none beyond B | `assignable_d` at `checker.rs:2118` changes to require `T` equal, not assignable; a read-only array type if wanted | a second array type or a read view | every existing call that relies on `Array<Wide>` → `Array<Narrow>` covariance (the smoke test above would fail) | Rust, Swift, Flow-mutable, Python |

Costs: the change at `compiler/vyrn-frontend/src/checker.rs:2118-2119` is one line, but it makes every array argument an exact-type match. Existing code that leans on record-width covariance in arrays breaks. A read-only array type is new surface and new lowering. This is the only design that answers the owner's array question with "it is refused, and nothing goes wrong."

### Design D — nominal enum with a visible tag field

Keep the nominal enum, but expose the discriminant as a readable field so a user can test `s.tag == "Circle"` (or `s is Circle`) without a full `match`. Exhaustiveness stays on `match`; the tag test is a non-exhaustive narrowing shortcut.

```
type Shape = | Dot | Circle(Int64) | Rect(Int64, Int64)
fn isRound(s: Shape) -> Bool { return s.tag == "Circle" }
```

| design | description | parser cost | checker cost | lowering cost | breaks | who else |
|---|---|---|---|---|---|---|
| D: nominal enum + visible tag | expose the internal discriminant as a literal-valued field; a test on it narrows | a `.tag` or `is` surface | a narrowing rule on a tag read, limited to one variant (no exhaustiveness on the shortcut) | the tag is already stored; just expose it | nothing | Zig (tag is a value), Kotlin (`is`), Java (`case Type`) |

Costs: the cheapest "test one field" answer. The tag already exists at runtime; the parser adds a field access and the checker adds one narrowing rule. But it does not give the owner a *fixed property value* in the type — the tag is a name, not a literal the user writes in a record. And it does not change the array story at all: `Array<Shape>` is still covariant, but variants are still not standalone types, so the subtyping hole stays closed. Adding a member still breaks every `match`; the non-exhaustive `is` shortcut keeps working (returns `false` for the new variant).

### Design E — open structural union with opt-in exhaustiveness

A structural union `A | B | C` of any types, with no closed-set requirement. A `match` is exhaustive only when the union's members are all known (a sealed nominal set or a finite literal set). This is the TypeScript/Scala-3-structural position.

```
type Shape = Circle | Rect | Empty   // structural, open
fn area(s: Shape) -> Int64 {
    return match s {
        Circle { r } => 3 * r * r,   // exhaustive only if Shape is sealed
        Rect { w, h } => w * h,
        Empty => 0,
    }
}
```

| design | description | parser cost | checker cost | lowering cost | breaks | who else |
|---|---|---|---|---|---|---|
| E: open structural union | `A | B` as a first-class type; exhaustiveness opt-in | a `Type::Union` variant; `|` in type position now means union, not just enum-variant-list | a union-normalization and narrowing pass; exhaustiveness only when the set is provably closed | a runtime tag for untagged unions, or a witness field for tagged ones | the existing `|` enum syntax (collides; needs disambiguation) | TypeScript, Scala 3, Flow |

Costs: the most expressive and the most expensive. The parser must disambiguate `| A | B` (enum) from `A | B` (union), which today's leading-`|` rule was built to avoid (`compiler/vyrn-frontend/src/parser.rs:2625-2647`). The checker gains a union type, a subtyping lattice, and a narrowing pass that is the union of all the rules above. Lowering an untagged structural union needs a runtime tag where none exists today, or a restriction to tagged members only. `Array<A | B>` is covariant, `Array<A>` is a subtype, and the unsoundness is fully open — this is TypeScript's disease without TypeScript's escape valve. Adding a member never breaks existing matches unless they opted into exhaustiveness.

---

The owner's hard question — `Array<TheUnion>` — is decided by variance, not by the union shape. Designs B and E inherit unsound covariance from the rule at `compiler/vyrn-frontend/src/checker.rs:2118`. Design C closes it by making arrays invariant. Design A and D dodge it by keeping variants non-standalone. Which trade the owner wants is the decision this file does not make.

## Decision (2026-08-28)

**Design A — nominal enum only, as today.** The union shapes B through E all serve programs this repository does not have: no dogfood app (shelf, in, log, the site) has hit a place where a nominal enum with payloads, exhaustive match and narrowing was the wrong tool, and the two costs the survey names are real — a visible discriminant field changes every match in the tree (B, D), and the sound version of standalone variants makes arrays invariant (C), breaking the covariance at checker.rs:2118 that existing code relies on. The hard question this file ends on — `Array<TheUnion>` — therefore stays unasked rather than answered wrongly. Reopen when a real program wants a value that is one-of-several records selected by a field it can read; design D (a visible tag on the nominal enum) is the likely shape then, because it adds the discriminant without touching variance.
