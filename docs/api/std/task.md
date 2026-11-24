# std/task

Waiting for the host: `sleep`, the host's timer, and the run queue.

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

The results of the two tasks of a `both`, in argument order.

## Race

```vyrn
type Race = enum { First(A), Second(B) }
```

The task of a `race` that returned first, and its result.

## join

```vyrn
fn join<T>(t: consume Task<T>) -> T
```

Waits for `t` to end and returns its result. If `t` is cancelled, the
caller is cancelled where it waits.

## cancel

```vyrn
fn cancel<T>(t: consume Task<T>) -> Unit
```

Ends `t` where it waits, releasing what it holds, or releases its result
when it returned. Does not wait.

## both

```vyrn
fn both<A, B>(first: consume Task<A>, second: consume Task<B>) -> Both<A, B>
```

Waits for both tasks: their results, in argument order. If one is
cancelled, `both` cancels the other and is cancelled.

## race

```vyrn
fn race<A, B>(first: consume Task<A>, second: consume Task<B>) -> Race<A, B>
```

The first task to return, in argument order once both started, and its
result: `await race(fork f(a), fork sleep(500))`. The other is cancelled
where it waits, or released if it returned. If the first to end was
cancelled, `race` is cancelled.

## timeout

```vyrn
fn timeout<T>(ms: Int64, t: consume Task<T>) -> Option<T>
```

`Some` of `t`'s result if it returns within `ms` milliseconds, else `None`,
and `t` is cancelled where it waits.

## joinAll

```vyrn
fn joinAll<T>(ts: consume Array<Task<T>>) -> Array<T>
```

Waits for every task: the results, in index order. If one is cancelled,
the rest are, and `joinAll` is cancelled.

## Group

```vyrn
type Group = { tasks: Array<Task<T>>, limit: Int64 }
```

Tasks of one result type, at most `limit` of them running at once, joined
or cancelled together.

## group

```vyrn
fn group<T>(limit: Int64) -> Group<T>
```

An empty group whose tasks run at most `limit` at once.

## room

```vyrn
fn room<T>(g: modify Group<T>) -> Unit
```

Waits until fewer than the limit of `g`'s tasks run: call it before each
`fork` the group will hold.

## add

```vyrn
fn add<T>(g: modify Group<T>, t: consume Task<T>) -> Unit
```

Adds `t` to `g`.

## joinGroup

```vyrn
fn joinGroup<T>(g: consume Group<T>) -> Array<T>
```

Waits for every task of `g`: the results, in the order they were added.

## cancelGroup

```vyrn
fn cancelGroup<T>(g: consume Group<T>) -> Unit
```

Cancels every task of `g` where it waits.
