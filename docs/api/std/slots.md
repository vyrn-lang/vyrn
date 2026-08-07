# std/slots

std/slots — a generational slab, written in Vyrn, over `Array` (RFC-0090 M1).

The corpus kept reaching for one thing and calling it four names: identity
for a value that lives inside a collection you own. `freelist`, `genref`,
`linkedlist` and `tree` asked Path B for it, and `slottable` wrote it out by
hand. This module is that one thing, once.

A `Handle<T>` is a plain value: a slot, the generation that was live when it
was issued, and the identity of the container that issued it. It owns no
heap, so it copies freely, never moves, and never has to be released.

    import { Slots, Handle, newSlots, insert, remove, get } from "std/slots"

    let mut people: Slots<Person> = newSlots()
    let h = insert(people, Person { name: "ada" })
    people[h].name = "lovelace"       // traps on a dead handle, like a[i] on OOB
    match get(people, h) { Some(p) => .., None => .. }     // liveness as a value
    remove(people, h)                 // the slot's generation bumps
    // (the container itself is not reclaimed yet — see `impl Owned` below)

**Two spellings, and the difference is what a dead handle does.** `s[h]`
joins the bounds-trap family: the handle must be alive, and the program stops
if it is not. `get` returns `Option<T>`, so staleness is a value the caller
handles. The old `Ref` could only ever trap.

**The identity word.** A `Handle<Person>` from one container used on another
of the same element type type-checks, and the generation compare can pass by
coincidence. Every container takes an identity when it is made and every
handle carries a copy of it, so a foreign handle is dead rather than
plausible. `copy` takes a fresh identity for the same reason — a structural
copy would hand two containers one identity, which is the hole with the
serial numbers filed off.

**What a removed element costs.** `remove` bumps the generation and returns
the slot to the free list. It does NOT clear the payload: there is no value
of type `T` to put there, and the language has no uninitialized place. The
dead payload is released when the slot is reused (a store releases what the
place held, RFC-0089 M3a) or when the container drops. So a `Slots<String>`
holds a removed string until one of those two, and never past them.

**Iteration order is not insertion order.** `for x in s` walks the live
elements, and `remove` fills the hole it makes with the last live element.
That is what keeps `remove` O(1). Iterate handles if the order matters.

**The reading half is `get` again.** Path B reserved `get`, `set` and
`release` (RFC-0004), so M1 spelled the reader `fetch` and said the name
would come free when M4 deleted Path B. M4 landed in Phase 8e and this is
that name coming back. There is still no `set`: `s[h] = v` and `s[h].f = v`
write where the element lives, and a free `set` would only copy one in.

## Handle

```vyrn
type Handle = { slot: Int64, gen: Int64, owner: Int64 }
```

A handle into a `Slots<T>`: which slot, which generation, and which
container. `T` is carried for branding only — a handle stores no element, so
`Handle<Person>` and `Handle<Order>` are different types over the same three
words, and the compiler refuses the mixture the identity word only catches at
run time.

## Slots

```vyrn
type Slots = { vals: Array<T>, gens: Array<Int64>, free: Array<Int64>, dense: Array<Int64>, denseAt: Array<Int64>, owner: Int64 }
```

A generational slab.

`vals` and `gens` are parallel and indexed by slot. `free` holds the slots
nobody is using. `dense` holds the live slots in iteration order and `denseAt`
maps a slot back to its position in `dense`, which is what makes `remove`
O(1) and `for x in s` a walk over live elements rather than over the table.

## newSlots

```vyrn
fn newSlots<T>() -> Slots<T>
```

An empty slab. The element type comes from the context that names it:
`let mut people: Slots<Person> = newSlots()`.

## insert

```vyrn
fn insert<T>(s: Slots<T>, v: T) -> Handle<T>
```

Put `v` in the slab and hand back a handle to it. Reuses a released slot when
there is one, and the store over that slot releases what it held.

## alive

```vyrn
fn alive<T>(s: Slots<T>, h: Handle<T>) -> Bool
```

Whether `h` still names a live element of `s`. False for a handle from
another container, an out-of-range slot, and a slot whose generation has
moved on.

## get

```vyrn
fn get<T>(s: Slots<T>, h: Handle<T>) -> Option<T>
```

Read through a handle. `None` when the handle is not alive — which is the
answer RFC-0090 wanted and the old `Ref` could not give: a value the caller
handles instead of a trap it cannot.

The element is COPIED out, because a return is owned (RFC-0089 rule 3). Read
`s[h]` instead where the element should stay where it is.

## remove

```vyrn
fn remove<T>(s: Slots<T>, h: Handle<T>) -> Bool
```

Release the element `h` names: bump the slot's generation, so every
outstanding handle to it is now dead, and return the slot to the free list.
Answers whether the handle was alive — removing twice is a no-op, not a
double free.

## count

```vyrn
fn count<T>(s: Slots<T>) -> Int64
```

How many elements are live.

## capacity

```vyrn
fn capacity<T>(s: Slots<T>) -> Int64
```

How many slots the table holds, live and free together. This is what grows;
`count` is what a program put in.

## handles

```vyrn
fn handles<T>(s: Slots<T>) -> Array<Handle<T>>
```

A handle to every live element, in iteration order. The way to walk a slab
when the walk needs the identities and not only the values.
