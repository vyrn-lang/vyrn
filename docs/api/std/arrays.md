# std/arrays

std/arrays: higher-order array helpers, in Vyrn.

## map

```vyrn
fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U>
```

Applies `f` to every element, collecting the results into a new array.

## filter

```vyrn
fn filter<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Array<T>
```

Keeps only the elements for which `pred` returns `true`.

## fold

```vyrn
fn fold<T, A>(xs: Array<T>, init: A, f: fn(A, T) -> A) -> A
```

Left fold: threads `acc` through `f` for every element, starting from `init`.

## any

```vyrn
fn any<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool
```

Whether `pred` holds for at least one element.

## all

```vyrn
fn all<T>(xs: Array<T>, pred: fn(T) -> Bool) -> Bool
```

Whether `pred` holds for every element; true when empty.

## includes

```vyrn
fn includes(xs: Array<String>, x: String) -> Bool
```

Whether `xs` contains the string `x`. The parameter is `Array<String>`, not
generic, because a generic `Array<T>` parameter does not accept a fixed-size
array literal.

## sortWith

```vyrn
fn sortWith<T>(xs: Array<T>, cmp: fn(T, T) -> Int64) -> Array<T>
```

A stable sorted copy, ordered by a comparator: negative puts `a` before `b`,
positive after, zero keeps arrival order. Insertion sort.

## sortBy

```vyrn
fn sortBy<T>(xs: Array<T>, key: fn(T) -> Int64) -> Array<T>
```

A stable sorted copy, ascending by an `Int64` key. Insertion sort.
