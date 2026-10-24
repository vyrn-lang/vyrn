# std/order

Orders, the comparison under one, and the functions that sort by one.
`Asc`, `Desc`, `Then`, `Elem`, `Order` and `before` are compiler-known:
an order is a type, never a value, and only these three make one.

## Asc

```vyrn
type Asc = Never
```

The ascending order of `T`: by each named field in turn, or by `T`'s own
value with no field. `Asc<Person, age>` orders people by age. A key is
an integer, a `Bool` or a `String`; `String`s compare bytewise.

## Desc

```vyrn
type Desc = Never
```

The order `O` reversed.

## Then

```vyrn
type Then = Never
```

The order `A`, then `B` among the values `A` ties. `A` and `B` order one
type.

## Elem

```vyrn
type Elem = Never
```

The type the order `O` compares: `Elem<Asc<Person, age>>` is `Person`.

## Order

```vyrn
protocol Order
```

The bound of a type parameter that stands for an order. `Asc`, `Desc` and
`Then` satisfy it, and nothing else does.

## before

```vyrn
fn before<O>(a: Elem<O>, b: Elem<O>) -> Bool
```

Whether `a` is strictly before `b` under `O`. Two values tie under `O`
when neither is before the other.

## sort

```vyrn
fn sort<O>(xs: consume Array<Elem<O>>) -> Array<Elem<O>> where sorted<O>(value)
```

`xs` sorted by `O`, stably: values that tie keep their order. An
insertion sort, quadratic in the length.

## insertSorted

```vyrn
fn insertSorted<O>(xs: consume Array<Elem<O>> where sorted<O>(value), v: consume Elem<O>) -> Array<Elem<O>> where sorted<O>(value)
```

The sorted `xs` with `v` inserted after every value it is not before.

## mergeSorted

```vyrn
fn mergeSorted<O>(a: consume Array<Elem<O>> where sorted<O>(value), b: consume Array<Elem<O>> where sorted<O>(value)) -> Array<Elem<O>> where sorted<O>(value)
```

The sorted `a` and `b` as one sorted array. Of two values that tie, `a`'s
comes first.

## dedupSorted

```vyrn
fn dedupSorted<O>(xs: consume Array<Elem<O>> where sorted<O>(value)) -> Array<Elem<O>> where strict<O>(value)
```

The sorted `xs` with each run of values that tie under `O` cut to its
first.

## lowerBound

```vyrn
fn lowerBound<O>(xs: Array<Elem<O>> where sorted<O>(value), v: Elem<O>) -> Int64 where value >= 0 && value <= xs.length
```

The first position in the sorted `xs` whose value `v` is not after: the
count of values before `v`. Binary search.
