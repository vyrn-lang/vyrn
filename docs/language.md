# The Vyrn language

This is the reference for writing Vyrn. It states each rule once, with a short example where one helps, and says in one line what the compiler refuses and why. The standard library is in [std.md](std.md), ownership and release in [memory.md](memory.md), and the `vyrn` commands in [tooling.md](tooling.md).

## A program is a file

A program is a `.vyrn` file with a `main` that returns the exit code. `print` writes one line to standard output.

```vyrn
fn main() -> Int64 {
    print("hello, world")
    return 0
}
```

`main` must be `fn main() -> Int64`. A single file needs no project and no import: `vyrn run file.vyrn` compiles it to WebAssembly and runs it, and `vyrn build` makes a native binary from the same module.

## Lexical rules

- A statement ends at the end of its line. `;` is an optional separator, so `let x = 1; let y = 2` is two statements.
- `//` starts a comment. `///` is a documentation comment on the declaration that follows; `vyrn doc` and editor hover read it. There are no block comments.
- Names are lowerCamelCase for values and functions, UpperCamelCase for types and variants.
- Keywords: `fn let mut if else while for in drop protocol import export impl self return true false type enum where match region break continue`. The words `gen`, `extern`, `test`, `bench`, `contract`, `logging`, `read`, `modify`, `consume`, `await`, `lazy` and `as` are keywords only where the grammar expects them.

Literals:

| Form | Type | Notes |
|---|---|---|
| `42` | `Int64` | Adapts to a sized integer sibling: `x + 5` with `x: Int32`. |
| `2.5` | `Float64` | Adapts to a `Float32` sibling. No exponent form. |
| `'a'`, `'\n'`, `'\x7f'` | `UInt8` | A byte. Escapes: `\n \t \r \' \\ \0 \xNN`. |
| `true`, `false` | `Bool` | |
| `"text \{expr}"` | `String` | Escapes: `\n \t \r \\ \" \u{HEX}`. A raw newline is text. |
| `"""a "quoted" word"""` | `String` | A lone `"` or `""` is text. |
| `[1, 2, 3]` | `Array<Int64, 3>` | Becomes `Array<T>` where the context wants one. |
| `[:]`, `["a": 1]` | `Map<K, V>` | The context gives the value type. |

Integer literals are decimal only: there is no hex, binary or `_` separator form. A string cannot hold a NUL byte, so `\u{0}` is refused.

## Declarations

A file holds these top-level forms, in any order:

| Form | Purpose |
|---|---|
| `import ...` | Takes names from another module. |
| `fn`, `gen fn`, `mut fn`, `extern fn`, `export extern fn` | Functions. |
| `type` | A record, an enum, a validated type or an alias. |
| `protocol`, `impl` | Named method sets and their implementations. |
| `contract` | The exports a module may have, read by generators. |
| `let`, `let mut` | Module state. |
| `logging { .. }` | The log threshold and sink (root module only). |
| `test "name" { .. }`, `bench "name" { .. }` | Tests and benchmarks. |

`export` before a `fn`, `type`, `protocol` or `contract` makes it visible to importers. Declaration order does not matter, except that a module-state initializer may read only earlier state.

## Bindings

`let` binds a value once. `let mut` allows assignment. The type is inferred from the initializer, or written.

```vyrn
let name = "Ada"
let mut count: Int64 = 0
count = count + 1
```

- Assigning to a binding declared without `mut` is refused.
- An inner binding shadows an outer one of the same name: a `let`, a `for` variable, a pattern binder and a lambda parameter each start a new binding.
- A binding also shadows a function of its name in a bare call: `f(x)` calls the local `f` when it has a `fn` type, and is refused when it has another type. A dot call `x.f()` skips a local `f` that is not a function.
- A binding that holds heap data moves on assignment. `let b = a` makes `a` unusable when `a` owns a `String`, an array, a map or a record that holds one. Write `a.copy()` when both need a value. A value with no heap, such as an `Int64` or a record of numbers, copies.

## Types

### Numbers

Every number type states its width. There is no unsized `Int`.

| Type | Values |
|---|---|
| `Int8`, `Int16`, `Int32`, `Int64` | Signed two's complement. |
| `UInt8`, `UInt16`, `UInt32`, `UInt64` | Unsigned. |
| `Float32`, `Float64` | IEEE-754. `Float32` rounds to single precision at every step. |
| `Bool` | `true`, `false`. |

- Integer arithmetic wraps at the type's width, `Int64` included: `Int64` max plus one is `Int64` min.
- A literal must fit its type, its sign included: `let x: Int8 = -128` is accepted, and `let b: UInt8 = -1` is refused.
- Division by zero traps. `%` is the truncated remainder with the sign of the dividend, so `a == (a / b) * b + a % b`.
- Operands of `+ - * / %`, comparisons and bitwise operators share one type. Nothing widens by itself: `Int32 + Int64` is refused.
- A conversion is a call named after the target type: `Int64(x)` widens or sign-extends, `UInt8(300)` wraps to `44`, `Int64(2.9)` truncates to `2`, `Float64(n)` converts.
- Bitwise operators are `& | ^ ~ << >>` on integers. `>>` is arithmetic on a signed operand and logical on an unsigned one. `~` stays within the width. A shift amount outside `0` to width minus one traps, and a constant one is refused.
- `x.clz()`, `x.ctz()` and `x.popcount()` count leading zeros, trailing zeros and set bits of an `Int32`, `UInt32`, `Int64` or `UInt64`, at the operand's type. Each is one wasm instruction. On zero, `clz` and `ctz` answer the width.
- `x.sqrt()` is the correctly rounded square root of a `Float64` or `Float32`, one wasm instruction. [std/math](std.md#numbers) holds the other float functions.
- `float32Bits(x)` and `float64Bits(x)` give a float's IEEE-754 bits as the `UInt32` or `UInt64` of its width, and `float32FromBits(b)` and `float64FromBits(b)` read them back. Each is one wasm instruction and keeps every bit: a NaN's sign and payload, `-0.0` and the subnormals. `Float64(x)` of a `Float32` NaN gives a NaN whose payload is the engine's.
- `print` and interpolation render a float with six decimals: `0.1 + 0.2` prints `0.300000`.

Operator precedence, loosest first: `||`, `&&`, `== != =~`, `< <= > >=`, `??`, `|`, `^`, `&`, `<< >>`, `+ -`, `* / %`, then unary `- ! ~`. Bitwise binds tighter than comparison, so `flags & mask == mask` is `(flags & mask) == mask`. `??` is right-associative.

### Strings

A `String` is UTF-8 bytes with no NUL.

- `s.byteLength` is the length in bytes, read in constant time. `s.charCount()` walks the string and counts Unicode scalar values.
- `s[i]` is the byte at offset `i`, a `UInt8`. `bytes(s)` is the whole `Array<UInt8>`. `stringFromBytes(b)` answers `Result<String, String>` and refuses invalid UTF-8 and NUL.
- `+` concatenates. `==`, `!=`, `<` and the other comparisons compare bytes.
- `s =~ "pattern"` is a whole-string regular-expression match against a string literal, compiled at build time. [std/regex](std.md#text) searches, counts and replaces.
- `x.toString()` renders a number, a `Bool`, a `String` or a type with `impl Show`.

Offsets in every string function are byte offsets. [std/strings](std.md#text) holds split, join, trim, case, search and padding.

### Interpolation and templates

`\{expr}` inside a string literal inserts the rendered value. `{` and `}` on their own are ordinary characters.

```vyrn
print("\{w} by \{h} is \{w * h}")
```

A value renders if it is a number, a `Bool`, a `String`, or its type has `impl Show`. Anything else is refused, with a hint to add the impl.

A name before a string literal is a tagged template. `tag"a \{x} b"` calls `tag(["a ", " b"], [value(x)])`: the literal parts arrive as `Array<String>` and the holes as `Array<Value>`, where `Value = enum { IntVal(Int64), StrVal(String), BoolVal(Bool) }`. A hole is always data and never structure, which is how a `sql` tag builds a parameterized query that no input can inject into.

`template"..."` builds the `Template` record `{ parts: Array<String>, values: Array<Value> }` without calling anything. `vyrn"..."` is a code quote; see [Generators](#generators).

### Arrays

| Type | Storage |
|---|---|
| `Array<T>` | Growable, on the heap. |
| `Array<T, N>` | Fixed length `N`, inline. The type of an array literal. |
| `SmallArray<T, N>` | The `Array<T>` interface, with the first `N` elements inline (`1 <= N <= 64`). Spills to the heap past `N` and stays spilled. |

- `xs[i]` reads and `xs[i] = v` writes in place on a `mut` binding. The index is checked, and an out-of-range index traps.
- `xs.length` is the element count.
- `xs.push(v)` appends. `xs.pop()` answers `Option<T>`. `xs.swapRemove(i)` removes element `i` and moves the last one into its place.
- `xs.reserve(n)`, `xs.append(ys)`, `xs.copyFrom(ys)` and `xs.clear()` grow, extend, overwrite and empty in place. `small.toArray()` copies a `SmallArray` out.
- `for x in xs { .. }` visits each element. `for x in consume xs` takes each element as an owned value and leaves `xs` dead after the loop.
- `push` on a fixed `Array<T, N>` is refused, because its length is part of its type.

A nested store writes through a field or an element: `r.a.b = v`, `xs[i] = v`, `xs[i].f = v` and `t.xs[k] = v` are legal, to any depth: `xs[i].f.g = v` too. A user container's `atSet` answers its step wherever the step sits: `h.bag[0] = v`, `bs[0][0].f = v`.

### Map

`Map<K, V>` keeps insertion order.

```vyrn
let mut scores: Map<String, Int64> = [:]
scores["ada"] = 5
scores["ada"] = 6          // an update keeps the key's slot
let ada = scores["ada"]    // Option<Int64>
```

- `m[k]` answers `Option<V>`. `m[k] = v` inserts or updates. A remove and a later insert move the key to the end.
- `m.has(k)`, `m.remove(k)` (answers whether a key was removed), `m.keys()` (an `Array<K>` in order) and `m.length`.
- `m.tally(k, n)` adds `n` to a count map in one probe. `m.tallyBytes(w, n)` keys a `Map<String, Int64>` by raw bytes and builds the `String` only on a miss.
- A key is a `String`, an `Int64`, or a user type with no heap that declares `impl Hashable` (from `std/hash`): a record of scalars or a fieldless enum. The map hashes the key's bytes itself and never calls the impl.
- A float key is refused, because `NaN != NaN` and `+0.0 == -0.0` break key equality. Any other key type is refused by name.

On the wire a map is a JSON object.

### Records

A record type is a shape: named fields, in order.

```vyrn
type User = { name: String, age: Int64 }

let u = User { name: "Ada", age: 36 }
let mut next = u.copy()
next.age = next.age + 1
```

- Compatibility is by shape. A value with more fields fits where fewer are wanted, with no cast; two record types with the same fields are interchangeable. Width applies at the top of a value only: a record inside an array, an `Option`, `Result` or enum payload, a `Map`, a function type or another record's field must have the same fields as the one expected. Copy explicitly, for example with `xs.map(..)`.
- A type argument counts even where no field mentions it: a `Handle<Order>` is no `Handle<Person>`, at the top of a value or inside one, as in `Array<Handle<Order>>`. Two arguments with the same fields are interchangeable, so the brand is the argument's shape. A record type with the same fields and no argument fits in either direction, which is how `Handle { .. }` builds a handle of any brand.
- A field of a `mut` binding is assigned in place.
- `Omit<T, f, ..>`, `Pick<T, f, ..>`, `Merge<A, B>` (B wins a conflict) and `Partial<T>` (every field an `Option`) derive record types at compile time.
- `==` and `!=` work on scalars and `String` only. Comparing two records or two enum values is refused; compare fields or `match`.

### Enums

An enum value is exactly one variant, and a variant may carry payloads.

```vyrn
type Shape = enum {
    Circle(Int64),
    Rect(Int64, Int64),
    Point,
}

type Level = enum { Debug, Info, Warn }
```

- An enum declares one variant or more, separated by commas; a trailing comma is allowed.
- A variant name is unique across the whole program, so `Circle(2)` needs no qualifier. Declaring the same variant name in two enums is refused.
- A nullary variant takes its type from context: `let s: Shape = Point`.
- A record or enum type may take type parameters: `type Box<T> = { value: T }`, `type Opt<T> = enum { Wrap(T), Empty }`.

### Option and Result

There is no null and no exception. `Option<T>` is `Some(T)` or `None`. `Result<T, E>` is `Ok(T)` or `Err(E)`. Both are ordinary enums that every program can name without an import. See [Failure](#failure) for `?`, `??` and `panic`.

### Validated types

A `where` clause makes a rule part of a type. `value` names the value being checked.

```vyrn
/// The port to listen on.
type Port = Int64 where value >= 1 && value <= 65535
type Slug = String where value =~ "[a-z]+(-[a-z]+)*"
type User = {
    name: String where value.byteLength >= 3,
    age: Int64 where value >= 18,
} where age < 150
```

- A predicate uses comparisons, `&&`, `||`, arithmetic, `value.byteLength` and `=~`. On a record, a trailing `where` states a rule across fields, and a `where` on a field constrains that field.
- `Port(n)` constructs. A constant that satisfies the rule is proven at compile time and costs nothing; a constant that breaks it is refused; a runtime value that breaks it traps. The compiler drops the check where the facts before it prove the rule, and a value of a validated integer type gives the compiler its rule's comparisons as facts: with `i: Idx` and `type Idx = Int64 where value >= 0 && value < 8`, `xs[i]` on an `Array<Int64, 8>` runs no index check.
- `Port?(n)` answers `Option<Port>`: `None` when the rule fails. Use it for input the program does not control.
- Every boundary checks the rule without a call: a `let` annotation, an assignment, an argument, a return, a record field, an array element and a map insert.
- A parameter may state a rule over its argument and the arguments before it: `fn at(b: Array<Int64>, i: Int64 where value >= 0 && value < b.length)`. A comparison's side is an integer literal, or `value` or an earlier `read` parameter: the integer itself, or its `.length`, `.byteLength` or integer field. So `skip: Array<UInt32> where value.length >= 256` states the parameter's own length. `&&` joins the comparisons, and only a `read` parameter carries a clause. Every call checks the clause and the body assumes it. The compiler drops the check where the caller's facts prove it; otherwise a failure traps at the call with ``validation failed for parameter `i` of `at` ``.
- A function may state a rule over its result after the result type: `fn mid(lo: Int64, hi: Int64 where value > lo) -> Int64 where value >= lo && value < hi`. `value` is the result, and the clause may name every `read` parameter, in the grammar of a parameter clause. Each `return` checks it, as does the end of a body that falls off it, and every direct caller assumes it, its arguments in place of the parameters. The compiler drops a check the body's facts prove; otherwise a failure traps at the return with ``validation failed for the result of `mid` ``. A clause on `-> Unit` that names only parameters (`-> Unit where i >= 0 && i < n`) is a lemma: a call proves it for the caller. Write the clause on the signature's line: `vyrn fmt` indents a line by its open brackets alone, so a `where` that starts a line sits in column 0.
- A clause may state a fact of an array: `sorted<O>(value)`, no element before the one ahead of it under the order `O`, or `strict<O>(value)`, each element before the next. An order is a type from `std/order`: `Asc<Person, age>` orders people by age, `Desc<O>` reverses `O`, and `Then<A, B>` breaks `A`'s ties by `B`. A fact is a conjunct of its clause, never under `||` or `!`, and has no run-time check: every call proves a parameter's fact and every return the result's, or the program is refused. The proof follows the flow. A `std/order` establisher such as `sort` gives a sorted result, so does a private function whose every return the flow shows sorted, an empty array holds every fact, a `push` keeps a fact where a `before` answer or a pop shows the value in order, and any other write drops it. So `lowerBound<ByAge>(sort<ByAge>(ps), p)` is accepted, and `lowerBound` on an array no row shows sorted is refused with ``` `lowerBound` needs `xs` sorted by `Asc<Int64>`, and nothing shows it is ```.
- A `let` with a type may state a clause in the same grammar over `value` alone: `let mut ys: Array<Person> where sorted<ByAge>(value) = sort<ByAge>(raw)`. It is a promise. Every write of the binding keeps it: the `let`'s value, an assignment, a store into a part, a `push` and a call that modifies it. A comparison is checked after each write unless the compiler proves it, and a failure traps with ``validation failed for the `where` clause of `i` ``. A fact is never checked, so a write that may break it is refused at the write: ``the push on line 8 may break the promise of `ys` on line 6, `sorted<ByAge>(value)` ``. `insertSorted` keeps the order.
- A looser `let` removes a promise and keeps its facts: after `let mut ys: Array<Person> = ps` the push is accepted, `ys` is sorted until the push, and a later `let` may promise the order again while it holds. A `let` without a clause promises nothing, so a write that breaks a fact is refused only where a fact is needed.
- A parameter clause needs a call to check it, so a function with one is not a function value, takes no `fn` parameter, and is not a `gen fn` or an `export extern fn`, which the host enters by name.
- A run of consecutive statements that store into (assign, push onto, or otherwise modify) the fields of one record with a trailing `where` is a group. The rule is checked once, after the group's last statement, with the trap of any boundary; the prover drops the check where it proves the rule, as when each column grows by one from equal lengths. Any other statement ends the group, so `c.a.push(1)` then `print(c.a.length)` checks `c` before the `print`. A group whose check fails on every run that reaches it is refused: after that push, `c.a` is longer than `c.b`.
- Between a group's first store and its check, nothing reads the record whole. When the record is a `modify` parameter, the group also calls no function the program declares and no function value, and has no `?`, because the caller would see the record with the rule unchecked. A `?` or a trap inside the group leaves a record the function owns unobserved, so both are allowed there. A `return` statement ends the group, so the check runs before it.
- An element of an array field the rule reads only as `.length` is assigned in place with no check: `c.xs[i] = v` keeps every length. A store into a field of module state, or into a field of a nested record with its own rule, is refused; rebuild the record.
- A validated value decays to its base type, so a `Port` is usable as an `Int64`.
- Branches of an `if` or arms of a `match` of different validated scalar types over one base join to their union. `if fast { Port(8080) } else { Age(30) }` is a `Port | Age`: an `Int64` where `Port`'s rule or `Age`'s holds. Each branch proved its own side, so the join adds no check.
- A union is written `Port | Age` in any type position: an annotation, a parameter, a return, a record field, a type argument and an alias (`type Open = Port | Age`). `|` binds loosest in a type, so `Option<Port | Age>` holds a union and `fn() -> Port | Age` returns one.
- Each member is a validated scalar type, or a union of them, over one base. A record, an array, an enum, a type parameter, a generic validated type and a member over another base are refused, each with one sentence. A union lists each member once, in source order, and a union of unions flattens. A member equal to the base absorbs the rest, so `Port | Int64` is `Int64`, and a union of one member is that member. Validated records and arrays, and types over different bases, do not join.
- A written union is a validated type whose rule is the `||` of its members' rules. Every boundary checks it, and a raw base value that satisfies no member traps with ``validation failed for `Port | Age` ``. A value of a member type, or of a union of its members, fits with no check, and a constant or an interpolation proven in one member costs nothing. A union decays to its base, and where a `Port` is required it is checked at the boundary, as an `Age` is.
- A union has no name of its own, so it has no constructor and no `?` form. An alias of one is transparent: `Open` and `Port | Age` assign to each other. A `where` over a union is refused, as over any non-scalar base.
- A join decays to its base in every pass after the checker. So where a join meets a written union the boundary check runs, and a `let mut` binding of a join holds its base.
- An alias without `where` is transparent: `type Id = Int64` names `Int64`, and each value assigns to the other. An alias has no constructor. A distinct type is a validated type, or a generic record applied to another argument (see Records). A record with one field is not distinct: it fits wherever a record with that one field is wanted.
- A `String` type whose regular expression denotes a finite language is a finite string type. An interpolation whose holes are all finite types has a finite type too, and assigning it to a validated `String` type is proven by automaton containment at compile time, or refused with the key that escapes.

`schemaOf<T>()` reads a validated type's declaration at compile time and answers a `Schema` record: name, base type, `///` doc, bounds, `multipleOf`, length bounds and pattern. `jsonSchema<T>()` writes the JSON Schema document for any type, and a union's as the `anyOf` of its members' schemas. `schemaOf` of a union is refused, because one `Schema` describes one declaration. Neither is runtime reflection: the compiler computes the answer, and types are erased before the program runs.

### Proofs

`prove(c)` from `std/proof` asks the compiler to show `c` where the call stands. `c` is one integer comparison, or several joined by `&&`.

```vyrn
import { prove } from "std/proof"

fn mid(xs: Array<Int64>, lo: Int64 where value >= 0, hi: Int64 where value > lo && value <= xs.length) -> Int64 where value >= lo && value < hi && value < xs.length {
    prove(lo < xs.length)
    let q = (hi - lo) / 2
    prove(q < xs.length)
    return lo + q
}
```

- The compiler proves `c` from the body, the parameters' clauses, the validated types of the names it reads and the result clauses of the functions the body calls. It reads nothing it inferred about another function, so the proof holds in every program that links the module. A comparison it cannot show is refused with the reason: ``` `prove` cannot show its comparison here: it needs `i < n`, and `i` is a parameter, so the function cannot know what its callers pass ```.
- Every check after the call assumes `c`. So a `prove` splits a chain the prover cannot take in one step into steps it can: in `mid`, every check and the result clause are proved, and a binary search that calls `mid` runs no index check.
- A sum or difference in `c` states a fact only where the compiler shows it stays within `-2^62..=2^62`. Elsewhere it may wrap, and the refusal says ``the sum `a + b` may wrap``.
- The call runs nothing: the emitter erases it as it erases a lemma call (below). `prove` is no function value, because a call through a value proves nothing.

A project demands a proof of every check in a module by naming it in the `proved` key of `vyrn.json`: `{ "main": "main.vyrn", "proved": ["core/**", "scan.vyrn"] }`. Each entry is a glob relative to the manifest's directory: `*` and `?` match within one name, and `**` matches any number of directories.

- A check that a body of such a module keeps is refused with its reason and the fix, if one exists: ``` the index check stays at run time, and this module is in `proved` (vyrn.json): it needs `i >= 0`, and `i` is a parameter, so the function cannot know what its callers pass; add `where value >= 0` to `i`, and each call checks it where the caller cannot show it ```.
- The proof reads what a `prove` reads: the body, the clauses, the validated types and the declared result clauses of the callees. A fact the compiler infers about a callee does not count, so a callee's result needs its clause written: `-> Int64 where value < hi`.
- A parameter's clause is the module's boundary. A call from a module outside `proved` keeps its clause check at run time; a call inside one must prove it.

The emitter erases a call whose result is `Unit` or unused when its callee, after elision, has an empty effect set, takes every parameter by `read`, is trap-free, is not recursive, runs only counter loops and calls only such functions. Such a call writes nothing, traps nowhere and ends, so the program prints the same without it. A lemma whose clause the compiler proves costs no call at run time; its arguments are still evaluated.

- `vyrn why --cost` prints each lemma call that stays with the first rule its callee fails: ``call kept: `down(..)`, because it is recursive``.
- A recursive lemma runs: no measure shows that its recursion ends.

### Waiting

A function waits when it reaches a host wait: `sleep` from `std/task`, or a call of a function that waits. Nothing declares it: the compiler infers it as it infers every effect, and `await` marks each call that waits.

```vyrn
import { sleep } from "std/task"

fn pause(ms: Int64) -> Int64 {
    await sleep(ms)
    return ms
}

fn twice(ms: Int64) -> Result<Int64, String> {
    let first = await pause(ms)
    return Ok(first + await pause(ms))
}
```

- `await` marks the call after it, as `consume` marks a place. `await f(x)?` is `(await f(x))?`. It takes no name from programs: `let await = 1` is a binding, and `await(x)` is a call of it.
- A waiting call without `await` is refused, and its fix says to write `await` before the call. `await` before a call that does not wait is refused too, so the mark stands exactly where other code may run.
- A `gen fn`, a module-state initializer and a `where` predicate cannot wait: a build is deterministic, state is set before `main` runs, and a predicate calls nothing.
- Other tasks may run while one waits, so an `await` may write every global. It ends every borrow of module state and every fact about it, as a call that writes the global does: a borrow of a module-state field held across an `await` and read after it is refused.
- No future or task handle is a value, so a waiting call finishes where it stands.
- `vyrn why --cost` shows `waits` in a function's effect set, and the editor's hover of a function that waits says so.

Waiting code does not build yet. `vyrn check` judges it, and a build refuses a program with a waiting function: ``waiting code does not build yet: `pause` waits at line 4``.

### Function types

`fn(A, B) -> R` is a type. A function value can be a named function or a lambda, and it can be stored in a binding, a field, an array, a map, an `Option` or module state. A function value reads every argument, so a function with a `consume` or `modify` parameter is not a value of any `fn` type.

```vyrn
type Transform = fn(Int64) -> Int64

fn makeAdder(n: Int64) -> Transform {
    return x -> x + n
}
```

### Lazy fields

A record field typed `lazy T` holds a computation. The construction site passes a thunk; every read of the field runs it and yields a `T`. Nothing is cached, so two reads run it twice. `toJson` reads every lazy field.

```vyrn
type Book = { title: String, body: lazy String }
let b = Book { title: "Dune", body: () -> loadBody(1) }
print(b.body)   // runs loadBody here
```

`lazy` is legal only in field position.

### Streams

`Stream<T>` is a linear sequence: produced once and disposed exactly once, on every path. The compiler checks this.

```vyrn
fn total(s: Stream<Int64>) -> Int64 {
    let mut sum = 0
    for v in s {
        sum = sum + v
    }
    return sum
}
```

- `fromArray(xs)` makes a stream from an array. `unfold(seed, step)` from `std/stream` makes a pull-based one whose step reads and writes a `Cursor`; it may never end.
- A stream is disposed by `for v in s` (a `break` or early `return` also disposes it), by returning it, or by `close(s)`.
- A stream parameter carries the obligation into the callee.
- A stream that is never disposed is refused: "`s` is a `Stream` and is never disposed".

`std/stream` has the lazy combinators `map`, `filter`, `take` and `merge`. A user type takes the same obligation with `impl MustUse for T {}`, and its disposal is its `impl Owned` release.

### SIMD

`F32x4`, `F64x2` and `I32x4` are fixed-width vector values with no heap. `F32x4(a, b, c, d)` builds one and `F32x4.splat(x)` repeats a lane. Arithmetic is lane-wise; `v.lane(i)` reads and `v.replaceLane(i, x)` writes a lane. A lane-wise comparison yields `Mask32x4` or `Mask64x2`, read with `anyTrue()` and `allTrue()`. `I32x4` has bitwise operators and no division.

## Functions

```vyrn
/// Returns the area of a rectangle.
fn area(width: Int64, height: Int64) -> Int64 {
    return width * height
}
```

- Every parameter states its type, and the result type follows `->`. Nothing is inferred across a function boundary but a private function's facts: what its result holds is inferred as a summary, while an exported function states its facts in `where` clauses. A function without `->` returns `Unit`.
- `return` leaves the function. A function that returns a value must return on every path.
- `x.f(a)` calls `f(x, a)` when `f` is in scope, so any function reads as a method on its first argument.
- `f<T>(..)` passes type arguments explicitly. A partial list is legal; the rest are inferred.
- `mut fn` declares that a function changes state. Nothing checks it; `std/graphql` reads it to tell a mutation from a query.

### Capabilities

A parameter states what the function does with its argument.

| Word | Meaning |
|---|---|
| `read` (the default) | Observes the value. The caller keeps it. |
| `modify` | Changes it in place. The argument must be a `mut` binding or a field of one, and the change is visible to the caller. |
| `consume` | Takes ownership. The caller may not name the value again. |

```vyrn
fn addSeat(t: modify Ticket) {
    t.seats = t.seats + 1
}
fn redeem(t: consume Ticket) -> Int64 {
    return t.id
}
```

- A method receiver takes the same words: `read self` (a bare `self`), `modify self`, `consume self`.
- Using a consumed value is refused, naming the line that took it.
- A `modify` or `consume` argument is exclusive: another argument of the same call that names it or an overlapping path is refused, because the callee could write or free it before it reads the other argument. Paths compare field by field: `f(p.a, p.b)` is accepted; `f(p, p.a)` and `f(p.a, p.a.b)` are refused.
- The words are erased before the program runs.

`consume place` in an expression moves a value out of a binding or a field chain; the place is dead afterwards. `drop x` releases a value and ends the binding. `region { .. }` frees every allocation made inside it at its closing brace; storing a heap value made inside into a binding that outlives the region is refused. [memory.md](memory.md) states when the compiler releases each value.

### Lambdas and closures

A lambda is `x -> expr`, `(a, b) -> expr`, `() -> expr`, or `x -> { statements }` with `return`. Its parameter types come from the expected `fn` type.

- A lambda that reads an outer local captures a copy of it, taken where the lambda is evaluated. A later assignment to the local does not change the capture.
- Module state is not captured: a lambda reads it live.
- A `fn`-typed parameter is resolved per call site at compile time. A stored function value becomes one enum per signature with one variant per lambda, so every call is a direct call and no function pointer exists at run time.

```vyrn
import { map, filter, fold } from "std/arrays"

let evens = filter(nums, x -> x % 2 == 0)
let total = fold(nums, 0, (acc, x) -> acc + x)
```

## Control flow

- `if cond { .. } else if cond { .. } else { .. }` is a statement. In an expression position each branch is one expression in braces and `else` is required: `let t = if n > 9 { "big" } else { "small" }`. Only the taken branch runs.
- `while cond { .. }` loops. `for x in xs { .. }` iterates an array, a stream, a map's `keys()` or a type with `impl Iterate`.
- `break` and `continue` act on the innermost loop. They are unlabeled, and outside a loop they are refused.
- There is no `loop` keyword; write `while true`.

## Pattern matching

`match` covers every variant of an enum. A missing variant is refused by name, so adding a variant lists every site that must answer for it.

```vyrn
let text = match s {
    Circle(r) => "circle of radius \{r}",
    Rect(w, h) => "rect \{w} by \{h}",
    Point => "a point",
}
```

- A pattern is a variant name with one binder per payload. There are no wildcard arms, no nested patterns and no literal patterns; write an arm per variant, and bind an unused payload to `_`.
- `match` is an expression, and its arms unify to one type, or to a union of validated types ([Validated types](#validated-types)). An arm that calls `panic` has type `Never` and fits any arm type.
- In statement position an arm may be a block: `Wide(a, b) => { .. }`. A block arm yields no value.
- `match consume s { .. }` moves the payloads out of `s`.

`if let` reads one variant and ignores the others. `while let` loops while the pattern matches, and evaluates its scrutinee once per turn.

```vyrn
if let Some(at) = indexOf(haystack, needle) {
    print("found at \{at}")
} else {
    print("absent")
}
while let Some(v) = next() {
    print(v)
}
```

A refutable `let` binds a variant's payloads in the enclosing scope, and traps when the value is another variant: `let Tagged(tag, n) = entry`. The trap reads ``let `Tagged(..)` did not match (file:line)``. The binders borrow the payload the value still owns.

## Failure

- `expr?` on an `Option` or `Result` yields the success payload, or returns the `None` or `Err` from the enclosing function. The function must return the same kind: `?` on a `Result` in a function that returns `Option` is refused. The failure is returned whole, so there is no error conversion.
- `?` on any other enum resolves through `Fallible` from `std/fallible`: the impl says whether a value is a success and how to read the success payload. The failing value reaches the caller unchanged.
- `a ?? b` yields `a`'s success payload, or `b`. It works on `Option` and `Result`, and `b` may be `panic(..)`.
- `panic(msg)` ends the program with `error: msg (file:line)` on standard error and exit code 1. It is for a case that cannot happen. Nothing unwinds and nothing catches a panic.
- A trap (an index out of range, a division by zero, a failed validation) ends the program with `error: <reason>` on standard error and exit code 1. Every engine prints the same bytes.
- Recursion has no fixed limit. A recursion that outgrows the stack traps `call stack exhausted`; the depth at which that happens depends on the engine and on the frames.
- `Validation<T> = enum { Valid(T), Invalid(Array<Issue>) }` reports every problem at once. An `Issue` is `{ key, path, message }`, where `key` is a translation key. `fromJson` and `std/cli` answer in this form.

```vyrn
fn quadrupled(text: String) -> Result<Int64, String> {
    let twice = doubled(text)?
    return Ok(twice * 2)
}
```

## Generics and protocols

A type parameter is written after the name, and inferred at each call. Every use compiles to one specialized function per concrete type, so a generic costs nothing at run time.

```vyrn
fn pick<T>(cond: Bool, a: T, b: T) -> T {
    if cond { return a.copy() }
    return b.copy()
}
```

A bound limits a parameter: `<T: Ord>`, `<T: Show + Eq>`. The built-in bounds are `Eq` (`==`), `Ord` (comparisons) and `Num` (arithmetic); `Num` implies `Ord` and `Ord` implies `Eq`. Any protocol is also a bound.

A protocol names methods. `impl P for T` provides them for one type, a built-in type included.

```vyrn
protocol Show {
    fn show(self) -> String
}
impl Show for Bool {
    fn show(self) -> String {
        return if self { "yes" } else { "no" }
    }
}
fn label<T: Show>(x: T) -> String {
    return "[" + x.show() + "]"
}
```

- Dispatch is static: the compiler picks the implementation per concrete type. There is no table lookup at run time.
- An impl must provide every method with the protocol's signature and receiver capability. A missing method, an extra method or a mismatched receiver is refused. There are no inherent methods: a helper is a top-level `fn`, which `x.helper()` calls.
- Dispatch keys on the type constructor, so a protocol has at most one impl per constructor: `impl Show for Option<Int64>` beside `impl<T> Show for Option<T>` is refused.
- A record dispatches by its static type, so a value assigned into a slot of another record type with the same shape uses that type's impl.
- `impl<T> P for Box<T>` is a generic impl, specialized per instantiation.
- A protocol may declare an associated type, `type Output`, that each impl fixes: `type Output = Int64`. A method signature names it.
- An impl for a validated scalar type is refused. Give the type a record shape instead.

The compiler knows these protocol names, and a program implements them without declaring or importing them. The prelude declares `Show`, `Owned`, `MustUse`, `Hashable` and `Fallible`, so an impl of one is checked as an impl of any declared protocol is; a program's own declaration of the name replaces the prelude's. An impl of `Copy`, `Index` or `Iterate` is checked for each member's receiver and parameter capabilities only, because its members' types are the impl's own and `Index`'s members are each optional.

| Protocol | Members | Effect |
|---|---|---|
| `Show` | `fn show(self) -> String` | Renders the type in `print`, `toString()` and interpolation. A scalar always renders as the language renders it. |
| `Copy` | `fn copy(self) -> T` | Replaces the structural `x.copy()`. |
| `Owned` | `fn release(consume self)` | Says how the type is released. A self-referring type must declare it. |
| `MustUse` | none | A value must be disposed of by name, as a `Stream` must. |
| `Index` | `at`, `atSet` projections | `x[i]` and `x[i] = v` on a user type. |
| `Iterate` | `fn size(self) -> Int64`, `nth` projection | `for x in c` on a user type. |
| `Hashable` | `fn hash(self) -> UInt64` | Admits a heapless type as a `Map` key. |
| `Fallible` | `type Output`, `isSuccess`, `success` | `?` on a user enum. |

## Projections

A member whose result carries a capability, `-> read T` or `-> modify T`, is a projection. It returns a place inside its receiver, not a copy. It is never called: every access site inlines its body, so the borrow cannot outlive the access.

```vyrn
type Window = { data: Array<Int64>, start: Int64 }

impl Index for Window {
    fn at(read self, i: Int64) -> read Int64 {
        return self.data[self.start + i]
    }
}
// w[2] reads self.data[self.start + 2] in place
```

- A projection may compute before its `return`; those statements run in the caller's frame, and the index expression runs once.
- `at` answers `x[i]`, `atSet` (`modify self` to `-> modify T`) answers `x[i] = v`, and `nth` serves `Iterate`. Any other name dispatches at a method call: `ledger.tag(i)`.
- A result `-> read Option<T>` is optional. It is consumed only by `if let Some(v) = x.tryAt(h) { .. } else { .. }`, which reads the element in place on a hit and builds no `Option`.
- A protocol may declare a projection requirement; the impl satisfies it with a projection of the same name, receiver and signature.

## Modules and imports

A file is a module. `export` decides what leaves it.

```vyrn
import { joinWith, padStart } from "std/strings"
import { scale as scaled } from "./lib/mathutil"
import * as math from "std/math"
import type { User, Port } from "./user.schema.json"
```

- A relative specifier resolves beside the importing file, with `.vyrn` appended. `std/name` is the standard library. A bare name resolves through the manifest's `dependencies`.
- `import { a as b }` binds `a` under the local name `b`. `import * as ns` reaches the exports as `ns.name`.
- Importing a name the module does not export is refused. An import cycle is refused.
- `export to "./editor", "std/ui" fn f()` names the only modules that may import `f`. Each path resolves as an import written in the exporting module would; one that does not resolve is refused. Any other import of `f`, a namespace use of it, or a generator call of it is refused with a sentence naming the permitted importers. The declaration is still an export everywhere else: in `moduleInterface`, a contract and the module's own uses. `vyrn doc` leaves it off the module's page, and editor completion offers it only where it may be imported. `to` is a word only after `export`, before a path; an `extern fn` takes none.
- Every module links into one program. An exported name is program-wide, so two flat imports of the same name from two modules are refused, even under an alias. Import one of them as a namespace instead. A private name stays private to its module.
- An `impl` applies program-wide, wherever it is written.
- `main` and `logging` are root-module only. Module state exists in every module and is private to it; `export let` is refused, so export accessor functions.

`import type { .. } from "x.schema.json"` synthesizes validated types from a JSON Schema document. Bounds, lengths and patterns become `where` clauses, `required` decides `Option`, `$defs` entries are importable, and a string `enum` becomes a fieldless enum. A root `title` that is also a `$defs` key is refused. A schema keyword Vyrn cannot express is refused, never weakened.

A remote specifier is `github:owner/repo@ref/path`, `gist:user/id[@rev]/file` or an `https://` URL. The first resolve pins it in `vyrn.lock` as specifier, immutable URL and SHA-256; every later load verifies the hash against the cache under `~/.vyrn/cache`. `--offline` never fetches. A remote module's relative imports stay inside its pinned base.

`vyrn.json` names the project: `main` (the entry file), `dependencies` (alias to specifier), `toolchain` (tool to version, pinned in the lock like any dependency), and the artifact and audience keys [tooling.md](tooling.md) describes. A single file needs none of it.

### Module state

A top-level `let` or `let mut` lives for the whole program and is shared by every function in its module. State initializes once, before `main`, in link order: a module's dependencies first, then its own bindings in declaration order.

- An initializer may use literals, operators, builtins and functions imported from another module. Calling a function of the same module is refused, because its state may not exist yet.
- An initializer that reads a later binding is refused.

An exported handler reads and writes module state across calls from a host, so a host can drive an event loop: see `export extern fn` below.

## Generators

A `gen fn` is ordinary Vyrn that runs while the program compiles and returns Vyrn source. An import target can be a call to one.

```vyrn
// gentable.vyrn
export gen fn squares() -> String {
    let mut out = "export fn squareOf(n: Int64) -> Int64 {\n"
    let mut i = 1
    while i <= 9 {
        out = out + "    if n == \{i} { return \{i * i} }\n"
        i = i + 1
    }
    return out + "    return 0\n}\n"
}

// main.vyrn
import { squares } from "./gentable"
import { squareOf } from squares()
```

- The loader runs the call with constant arguments, caches the result by content, and links the returned text as an ordinary module. `vyrn emit-gen file.vyrn` prints what each generator wrote.
- A generator is comptime-pure, and so is everything it calls: no `extern`, module state, `print`, `writeFile`, `readLine`, `args`, `readFileBytes`, clock, entropy, logging sink or wait. A call to one is refused, naming it. So a generator gives the same answer on every machine.
- A generator may read with `readFile`, `listDir` and `listDirKinds`, limited to the paths its caller passed.
- `moduleInterface(path)` reflects a module's exported functions and types as a `ModuleInterface` record. `contractOf(Name)` reflects a contract. Both work only during generation.
- A `TypeInfo`'s `shape` is its declaration as an array of `TypeNode`s, node 0 the root: record fields, enum variants and their payloads, arrays, `Option`, `Result`, `Map`, and each `where` as source. A declared name is a `named` leaf, found in `ModuleInterface.types`.
- `lex(src)` runs the compiler's lexer over text. `std/scan` is a comment- and string-aware cursor for other languages.
- `derive(g, x)` runs generator `g` after the check, on the type the checker gave `x`, and calls the function it wrote for that type. `g` is a `gen fn g(t: TypeArg) -> String`; for each root node `r` of `t` it defines `fn <t.nodes[r].name>(v: <t.nodes[r].spelling>) -> String`, and `vyrn check` refuses an entry of another signature at the call. Each node is a checked type, with its wire form as `kind`. `g` runs once per program, and its code is checked with the program.
- A line of generator output can carry a diagnostic; `std/diag`'s `report` writes one, and every tool shows it at the file and line the generator read.

### Code quotes

A generator can return `Code` instead of `String`. `vyrn"..."` and `vyrn"""..."""` are code quotes, legal only in generation. The quote's text is parsed when the generator itself is checked, so a typo in it is a diagnostic in the generator's file. A hole is spliced by its grammatical position: a `String` in expression position becomes an escaped string literal, a number becomes a literal, a `Code` splices verbatim, and a `String` in identifier position must be a valid identifier. `Code + Code` concatenates, and `render(c)` produces the text. `raw(text)` and `rawAt(text, path, line, col)` splice trusted text, the second with its origin.

### Contracts

A `contract` declares the exports a module may have. A generator compares a module with it; the compiler checks nothing else about it.

```vyrn
export contract Page {
    fn head() -> Head = noHead()
    fn data() -> Query<T> = noQuery()
    let title: String = ""
}
```

- A member is `fn name(types) -> R` or `let name: T`. A `= default` makes it optional and gives the value a generator substitutes.
- A name may appear several times with different signatures; the module supplies one of them.
- `fn *(..) -> R` is the open rule: it admits any other export whose result is `R`. A contract without one is closed.
- Type parameters are open per member: `Query<T>` admits any `T`.
- `contractOf(Page)` in a generator yields a `ContractInfo`, and `std/contract`'s `checkContract` compares it with a `moduleInterface`.

## Host interop

`extern fn name(..) -> R` declares a function the WebAssembly host supplies from the `vyrn` import namespace. Only a call crosses the boundary; on a target with no such host, the call traps with ``extern `name` is not available on this target``.

`export extern fn name(..) { .. }` is an ordinary function that is also exported to the host, so a browser timer or button can call it.

An extern signature may use `Int64`, the sized integers, `Float64`, `Float32`, `Bool` and `String`. Any other type is refused at the declaration.

## Input and output

These builtins need no import. Paths are relative to the working directory.

| Call | Result |
|---|---|
| `print(x)` | Writes `x` and a newline to standard output. |
| `writeStdout(bytes)` | Writes raw bytes. |
| `args()` | `Array<String>`: the arguments after the program name. |
| `readLine()` | `Option<String>`: the next line of standard input, `None` at the end. |
| `readFile(p)`, `readFileBytes(p)` | `Result<String, String>`, `Result<Array<UInt8>, String>`. |
| `writeFile(p, s)`, `writeFileBytes(p, b)` | `Result<Bool, String>`. `writeFile` truncates first. |
| `renameFile(a, b)`, `fsyncFile(p)` | `Result<Bool, String>`. |
| `listDir(p)`, `listDirKinds(p)` | `Result<Array<String>, String>`; the second marks a directory with a trailing `/`. |
| `toJson(x)` | The canonical JSON `String`: fields in declaration order, no whitespace, a `None` field omitted. |
| `fromJson<T>(s)` | `Validation<T>`. Runs every `where` clause, ignores unknown fields, and reports every problem with its path. A union decodes its base and holds where one member's rule does; its issue names the union. |

`save(path, value)`, `load(T, path)` and `loadOr` persist a typed value atomically through `std/storage`; `load` answers `Missing`, `Corrupt(issues)` or `Loaded(value)`.

`vyrn serve` runs a program that declares `fn handle(req: Request) -> Response`. `Request` and `Response` are records every program can name; request header names are lowercase.

### Logging

`logger(name)` returns a logger, and `log.trace`, `log.debug`, `log.info`, `log.warn` and `log.error` write `[LEVEL] name: message` to standard error. The root module may configure the threshold and the sink:

```vyrn
logging { level: debug, sink: stderr }
```

A call below the threshold is removed at compile time. The default threshold is `info`. The sink is `stderr`, `stdout` or `file("path")`.

## Tests and benchmarks

A `test` block sits beside the code it tests. `vyrn test` runs the root module's blocks; `vyrn run` and `vyrn build` check them and then strip them, so a shipped program contains none.

```vyrn
test "clamp holds the bounds" {
    assertEq(clamp(99, 0, 10), 10)
    assert(clamp(7, 0, 10) == 7)
}
```

- `assert` and `assertEq` exist only inside a `test` block.
- A test name is unique within its file. `vyrn test --name <substring>` selects tests.
- Only the root module's `test` and `bench` blocks are parsed and checked. An importer pays nothing for another module's tests, and a broken test fails only its own module.

A `bench` block is a body the timer runs. `vyrn bench` compiles it natively and reports the minimum, median and mean over many samples; `--check` runs each body once. `blackBox(x)` hides a value from the optimizer, so the measured work is not folded away.

```vyrn
bench "hash to 1000" {
    blackBox(hashTo(blackBox(1000)))
}
```

## Refusals at a glance

Each line is a program the compiler refuses, and the reason.

- A `match` that misses a variant: every case must be answered.
- An `if` expression with no `else`: every branch must yield a value.
- Arithmetic on two different number types: nothing widens by itself.
- A value used after it moved or was consumed: it has one owner.
- A constant that breaks a `where` clause: the type cannot hold it.
- A parameter's `where` clause that names a later or a `modify` parameter: it would not hold for the whole call.
- A call whose argument the flow does not show sorted, by the order a fact clause names: a fact has no run-time check.
- A write that may break a fact a `let` promises: the promise holds at every write, and nothing checks a fact at run time.
- A `prove` the compiler cannot show from the body, the clauses and the declared result clauses of its callees: it checks nothing at run time.
- A check kept in a module the `proved` key of `vyrn.json` names: the project asked for none at run time there.
- Assignment to a binding without `mut`.
- `?` in a function whose return kind differs from the operand's.
- A `Stream` or `MustUse` value that is not disposed on every path.
- A heap value stored out of a `region`: it would dangle.
- A float or heap-owning `Map` key, or a user key type without `impl Hashable`.
- `==` on records or enums: equality is defined for scalars and `String`.
- A `gen fn` that reaches an effect: a build must be deterministic.
- A waiting call without `await`, and `await` before a call that does not wait: the mark shows where other code may run.
- A module-state initializer that waits: state is set before `main` runs.
- A module-state initializer that calls a function of its own module or reads later state.
- Two modules' exports of one name imported flat: a top-level name is program-wide.
- An enum variant name declared twice.
- An `impl` of a protocol for a validated scalar type.
- An extern signature with a type the host boundary cannot carry.
