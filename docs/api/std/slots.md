# std/slots

std/slots: a generational slab over `Array`, in Vyrn.

A `Handle<T>` is a plain value: a slot, the generation live when it was
issued, and the identity of the container that issued it. It owns no heap, so
it copies freely and is never released.

    import { Slots, Handle, newSlots, insert, remove, get } from "std/slots"

    let mut people: Slots<Person> = newSlots()
    let h = insert(people, Person { name: "ada" })
    people[h].name = "lovelace"       // traps on a dead handle, like a[i] on OOB
    match get(people, h) { Some(p) => .., None => .. }     // liveness as a value
    remove(people, h)                 // the slot's generation bumps

A handle from another container of the same element type is dead, not
plausible: every container takes an identity and every handle carries it.
`remove` does not clear the payload, because the language has no
uninitialized place; the dead payload is released when the slot is reused or
the container drops. `remove` moves the last live element into the hole, so
iteration order is not insertion order; iterate `handles` if the order
matters.

## Handle

```vyrn
type Handle = { slot: Int64, gen: Int64, owner: Int64 }
```

A handle into a `Slots<T>`. `T` only brands it, so the compiler refuses a
`Handle<Order>` where a `Handle<Person>` belongs.

## Slots

```vyrn
type Slots = { vals: Array<T>, gens: Array<Int64>, free: Array<Int64>, dense: Array<Int64>, denseAt: Array<Int64>, owner: Int64 }
```

A generational slab.

`vals` and `gens` are parallel and indexed by slot; `free` holds the unused
slots. `dense` holds the live slots in iteration order and `denseAt` maps a
slot back to its position in `dense` (-1 when free), so `remove` is O(1) and
iteration skips free slots.

## newSlots

```vyrn
fn newSlots<T>() -> Slots<T>
```

An empty slab. The element type comes from the context:
`let mut people: Slots<Person> = newSlots()`.

## insert

```vyrn
fn insert<T>(s: modify Slots<T>, v: consume T) -> Handle<T>
```

Puts `v` in the slab and returns a handle to it. Reuses a free slot when there
is one, and the store releases the dead payload it held.

## alive

```vyrn
fn alive<T>(s: Slots<T>, h: Handle<T>) -> Bool
```

Whether `h` still names a live element of `s`. False for a handle from
another container, an out-of-range slot, a slot whose generation has moved
on, and a free slot: `remove` sets the generation the next `insert` will
issue, so a handle built by hand can match a free slot's generation.

## get

```vyrn
fn get<T>(s: Slots<T>, h: Handle<T>) -> Option<T>
```

The element `h` names, copied out, or `None` when `h` is not alive.

Use `s[h]` to read in place, or `if let Some(v) = s.tryAt(h)` when the value
does not outlive the test; neither copies.

## remove

```vyrn
fn remove<T>(s: modify Slots<T>, h: Handle<T>) -> Bool
```

Releases the element `h` names: bumps the slot's generation, so every handle
to it is dead, and returns the slot to the free list. Answers whether `h` was
alive, so removing twice is a no-op.

## count

```vyrn
fn count<T>(s: Slots<T>) -> Int64
```

How many elements are live.

## capacity

```vyrn
fn capacity<T>(s: Slots<T>) -> Int64
```

How many slots the table holds, live and free together.

## handles

```vyrn
fn handles<T>(s: Slots<T>) -> Array<Handle<T>>
```

A handle to every live element, in iteration order.
