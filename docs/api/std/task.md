# std/task

Waiting for the host: `sleep`, the one host wait, and the run queue.

A function that reaches a wait waits itself, and each call of it is
marked `await` (language.md, Waiting).

## sleep

```vyrn
fn sleep(ms: Int64) -> Unit
```

Waits while the host runs other tasks, until `ms` milliseconds after the
deadline of the wake this task runs in, or after the start of the program
or of the request: the time the task computes before it counts against it.

## Both

```vyrn
type Both = { first: A, second: B }
```

The results of the two branches of a `both`, in argument order.

## Race

```vyrn
type Race = enum { First(A), Second(B) }
```

The branch of a `race` that finished first, and its result.

## both

```vyrn
fn both<A, B>(first: consume A, second: consume B) -> Both<A, B>
```

Runs two calls at once and waits for both: `await both(f(a), g(b))`.

Each argument is a call, and the two are one call's arguments, so a
`modify` or `consume` argument of one may not be named by the other.
`both` starts the first, runs it to its first wait, then starts the
second. If one is cancelled, `both` cancels the other and is cancelled.
Without `await`, no branch waits, and the calls run in order.

## race

```vyrn
fn race<A, B>(first: consume A, second: consume B) -> Race<A, B>
```

Runs two calls at once and returns the first to finish, cancelling the
other where it waits: `await race(f(a), sleep(500))`.

`race` starts both as `both` does. The first branch, in argument order,
that has finished when the starts end wins; after that, the first to
finish. A finished loser's result is released. Without `await`, no branch
waits, and the first wins.

## each

```vyrn
fn each<T, R>(xs: Array<T>, f: fn(T) -> R) -> Array<R>
```

Calls `f` on every element of `xs` at once and waits for every call: the
results, in index order. The calls start in index order, each to its first
wait. If one is cancelled, the others are, and `each` is cancelled.

## timeout

```vyrn
fn timeout<T>(ms: Int64, f: fn() -> T) -> Option<T>
```

Waits for `f()` at most `ms` milliseconds: `Some` of its result, or `None`
when the time ran out first, and `f` was cancelled where it waited.
