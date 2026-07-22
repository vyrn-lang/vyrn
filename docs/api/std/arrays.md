# std/arrays

std/arrays — higher-order array helpers (RFC-0023), written in Vyrn itself.
Being ordinary Vyrn, every function here gets interpreter == native == wasm
parity for free, and each `fn`-typed parameter is monomorphized away at every
call site (no function value survives to runtime).

## map

```vyrn
fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U>
```

Apply `f` to every element, collecting the results into a new array.

## filter

```vyrn
fn filter<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Array<T>
```

Keep only the elements for which `pred` returns `true`.

## fold

```vyrn
fn fold<T, A>(xs: Array<T>, init: A, f: fn(A, T) -> A) -> A
```

Left fold: thread `acc` through `f` for every element, starting from `init`.

## any

```vyrn
fn any<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool
```

`true` if `pred` holds for at least one element.

## all

```vyrn
fn all<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool
```

`true` if `pred` holds for every element (vacuously true when empty).

## includes

```vyrn
fn includes(xs: Array<String>, x: String) -> Bool
```

Whether `xs` contains the string `x`. The shared replacement for the
`twContains`/`i18n`/`vyx`/`ui` private copies several generators used to carry
(the string builtin `s.contains` is substring search WITHIN a String; this is
membership in an `Array<String>`). Concrete `String` on purpose — every private
copy it replaces was `Array<String>`, and a concrete parameter also accepts a
fixed-size array literal, which a generic `Array<T>` parameter does not.

## sortBy

```vyrn
fn sortBy<T>(xs: Array<T>, key: fn(T) -> Int64) -> Array<T>
```

A stably sorted copy, ascending by an `Int64` key (insertion sort). The key
extractor is an ordinary `fn` parameter, so a lambda, a named function, or
a STORED function value (RFC-0037) all flow in.
