# Memory and ownership

Vyrn has no garbage collector, no reference counts and no runtime ownership
check. Every value has one owner. The language defines who owns a value from
its type and from the signatures it passes through; the compiler does not
guess. The compiler places every release, and a kernel re-checks the result on
every compile. A program the kernel cannot prove is refused with a named fix.

The first half of this page is for a person who writes Vyrn. The second half,
from "Where the rules live", is for a person who changes the compiler.

## A type that owns heap moves; any other type copies

A type owns heap when a value of it holds an allocation: `String`, `Array<T>`,
`SmallArray<T, n>`, `Map<K, V>`, `Stream<T>`, a function value, a type that
reaches itself (its recursive field is boxed), and every record, enum or fixed
array with such a part. `declared::owns_heap` in `vyrn-frontend` states the
rule.

A value that owns heap moves when it is bound to another name, returned,
passed to a `consume` parameter or stored into a place. After a move the
source name is gone, and a use of it is refused:

```
fn main() -> Int64 {
    let s = "a" + "b"
    let t = s
    print(s)
    return 0
}
```

```
a.vyrn:3:0: `s` was moved here into the binding `t`
line 4: ... and `s` is used again here
  fix: `s.copy()` if both sides need a value
```

A branch of an `if` or `match` that yields a name bound outside it moves the
name into the result. In `let rel = if c { st } else { st + "/" }`, `st` is
gone after the join on the path that yielded it, so a later use of `st` is
refused, and the fix is `{ st.copy() }` in the branch. The branch that does
not yield `st` releases it at the join. `xs = if c { xs.push(1) } else { xs }`
hands `xs` back to itself and stays legal.

A value that owns no heap (a number, a `Bool`, a record or enum of them)
copies on `let` and on assignment. It still moves into a `consume` parameter:
a `consume` parameter always takes its argument, whatever the type.

## `.copy()` is the only way to have two

`x.copy()` makes an independent deep copy. It is the only copy of heap the
language makes; no assignment, call or return copies heap on its own. A type
can replace the structural copy with `impl Copy for T { fn copy(self) -> T }`.

Every move diagnostic ends with a menu of `fix:` lines. `vyrn fix <file>`
applies the one entry that is an edit, `.copy()`. It refuses the others,
because each is a decision about ownership.

## A parameter borrows unless it says `consume`

A parameter has one of three capabilities. The capability is the calling
convention.

| Capability | Spelling | The callee may |
|---|---|---|
| `read` | `x: T` (the default) | observe the value |
| `modify` | `x: modify T` | change the value in place, exclusively |
| `consume` | `x: consume T` | own the value: release it or pass it on |

`read` and `modify` are second-class borrows. A borrow lives for one call. It
may not be stored in a field, pushed into a container, put into a
constructor, returned, captured by a closure that outlives the call, or passed
to a `consume` parameter. Because a borrow never escapes its call, it needs no
lifetime annotation.

A `modify` argument is exclusive: the same value may not be passed as `modify`
and read again in the same call. The caller sees the callee's writes after
the call returns.

A receiver follows the same rule: `read self`, `modify self`, `consume self`.

A prefix `consume` at an argument takes a field out of a value the caller
holds: `take(consume p.name)` moves `p.name` and leaves `p` with a hole at
`.name`. The rest of `p` stays usable. A use of `p` as a whole, or a `drop p`,
is refused while the hole is open; a store into `p.name` fills it. An element
is not a place a take reaches: `consume xs[0]` is refused.

## A result is owned, unless the signature lends it

`fn f(..) -> T` gives the caller its own `T`. A function may not return a
borrow: `return xs` from a `read` parameter is refused, with two fixes, declare
the parameter `consume` or return `xs.copy()`. An exported function owns its
result by the same rule.

A member of an `impl` or a `protocol` may lend a place inside its receiver
instead:

```
fn at(read self, i: Int64) -> read T {
    return self.vals[i]
}

fn atSet(modify self, i: Int64) -> modify T {
    return self.vals[i]
}
```

This is a projection. The body is a prologue of statements and one final
`return <place>`, where the place is a field and element chain rooted in
`self`. A projection is never called: its body is inlined at the access site,
so the borrow cannot outlive the expression that reads it. `a[i]` uses `at`,
`a[i] = v` uses `atSet`, and `for` uses `nth`; any other name is called as
`x.name(args)`. A projection on a free function is refused, because a free
function has no receiver to outlive the access.

`-> read Option<T>` is an optional projection. Its body is a prologue, one
`if <miss> { return None }`, a prologue that runs only on a hit, and
`return Some(<place>)`. It is consumed only by `if let`; the `Some` arm binds
the place, and no `Option` is built. `std/slots` `tryAt` and `std/json`
`tryField` are the models.

## Loops and patterns borrow unless they say `consume`

`for x in xs` binds `x` as a borrow of each element, so `out.push(x)` is
refused. Two fixes exist: `for x in consume xs` takes the container, so each
`x` is owned and `xs` is gone after the loop; or `out.push(x.copy())`. A loop
over a value that is not a place, such as `for x in make()`, owns its
elements without the word, because nothing else holds that container.

A payload binder in `match` or `if let` borrows from the scrutinee. `match
consume s { .. }` takes `s`, and each binder is owned. A refutable `let`
(`let Node(l, r) = t`) binds in the enclosing scope and traps when the
variant does not match; its binders borrow as a `match` arm's do.

A value read out of a place is an alias of that place. `let a = b.xs[0]`
followed by a store of `a` is refused, because `b.xs` still owns the element.
A write to the place ends every alias that reads out of it; a later read of
the alias is refused.

## Module state is never taken

A top-level `let` is module state. Any function may read or write it, and a
store into it releases the old value. No function may take it: returning it,
passing it to a `consume` parameter or looping over it with `for .. in
consume` is refused. Copy it with `.copy()`.

## Identity is a handle into a container you own

A program that needs a graph, a free list or two names for one node keeps the
nodes in `std/slots`. `Slots<T>` is a container; `Handle<T>` is a plain value
that owns no heap and copies freely. `slots[h]` traps on a dead handle, as
`a[i]` traps out of range. `slots.get(h)` answers `Option<T>` and
`slots.tryAt(h)` lends the element under `if let`. `remove(h)` bumps the
slot's generation, and dropping the container releases every node. A dead
handle is a value-level miss, never a use after free. `std/slots` is ordinary
Vyrn.

## A type can say how it is released and that it must be used

`impl Owned for T { fn release(consume self) { .. } }` gives `T` its own
release. A type that reaches itself needs one: the structural release walk has
no bottom on a cycle, so without a declared release its values are never
freed. `vyrn why --memory` names such a binding, "nothing releases the type
Tree yet". `examples/binarytrees.vyrn` shows the declaration.

`impl MustUse for T` gives `T` an obligation: on every path a value is handed
on by name or released with `drop`. `Stream<T>` carries the same obligation,
discharged by `for .. in`, a return or `close(s)`. A path that never disposes
of it, or disposes of it twice, is refused.

## A region frees a group at once

`region { .. }` opens an arena. Every String the block allocates comes from
the arena, and the arena is freed when the block exits. A value the region
allocated may not be stored where it outlives the region, and may not be
passed to a `consume` parameter inside it. A value the block returns is the
caller's to release.

## The compiler places every release

A program never frees memory by hand. `drop x` releases a value early; it is
never required for a value that owns heap.

The compiler releases an owned value at each exit where it is still held:

- the end of the block that bound it;
- a `return`, and a `?` that propagates;
- a `break` or `continue`, for the frames the loop body opened;
- the end of a `match`, `if let` or `for` that owns a temporary scrutinee;
- the end of an arm, for a payload binder the arm did not move;
- each edge of a join where one path consumed the value and another did not.

A store releases the value its place held, unless the place owns no heap or
the value hands the place back (`xs = xs.push(v)`). Releasing a record, enum
or container releases its places; for an enum, only the live variant.

`vyrn why --memory <file>` prints, per binding, whether it is reclaimed, how,
and the reason when it is not. An excerpt, with its dashes shown as `-`:

```
  fn main() -> Int64
    line 2     s                reclaimed at block exit - freeing the String buffer
    line 6     n                NOT reclaimed - the type Int64 owns no heap
```

The editor shows the same rows as memory hints. `vyrn emit-lowered <file>`
prints the named core, where each release is an explicit `drop` and `!` marks
an owned name:

```
fn main()
  {
    let s! = fn greet(read lit "ada")
    let @t1 = read s!.byteLength
    let @t2 = prim gt(@t1, lit 3)
    if @t2
      {
        do builtin print(read s!)
      }
    drop s! minus []
    return lit 0
  }
```

## What the compiler refuses

An ordinary value that owns heap never leaks by omission: where a release is
owed, the compiler places it. What the compiler refuses is a program in which
one owner is not enough:

| Refused | Example sentence |
|---|---|
| A use after a move | `` `s` was moved here into the binding `t` `` |
| A use after a `consume` (the second consume would free twice) | `` `s` is used here but was already consumed by `take(..)` on line 6 `` |
| A `consume` inside a loop of a value bound outside it | `` `x` is consumed by `take(..)` inside a loop, so it would be used again on the next iteration `` |
| A borrow that escapes: stored, returned, captured, consumed | `` `ys` may not be returned `` |
| A write to a place while an alias reads out of it | `` `t.xs[..]` is written here while `before` still reads out of it `` |
| A `modify` argument read again in the same call | `` `a` is passed to `bump` as `modify` and read again in the same call `` |
| A whole use of a value with a hole | `` `p.name` was taken out of `p` here `` |
| A must-use value never disposed, or disposed twice | `` `s` is a `Stream` and is never disposed `` |
| A region value that escapes the region | ``cannot store a heap value into `kept`, which outlives the enclosing `region` `` |
| A take of module state | `` module state `names` may not be passed to a `consume` parameter `` |

Each sentence is followed by `fix:` lines. `compiler/vyrn-cli/tests/refusals.rs`
holds one minimal program per refusal and pins its whole sentence.

One leak is not refused: a recursive type with no `impl Owned`. `vyrn why
--memory` reports it. Module state is not released at exit; it lives until
the process ends.

## The run-time layout

The target is wasm32. A pointer is 4 bytes.

- A `String` is the address of its bytes, which end in a NUL. The eight bytes
  before the address are its header: `len` at -8 and `cap` at -4, both
  32-bit. A literal lives in the data segment with `cap` all ones, and
  `free` ignores any address below `heapBase()`, so a literal is never freed.
- An `Array<T>` is `{ ptr, len: i64, cap: i64 }`, 24 bytes with a hole.
- A `Map` header is `{ keys, vals, len, cap, idx }`.
- A boxed enum payload lives in its own block.

The allocator is a segregated free list in `std/runtime`, four size classes
per power of two, with an eight-byte block header. The region arena routes
through `strNew`. Only `std/runtime` may import `std/mem`, the raw loads,
stores and host calls; the loader refuses any other importer.

## Where the rules live

Each rule has one home. The names below are the entry points.

- `vyrn-frontend/src/declared.rs`: `Owned` answers what owns heap
  (`owns_heap`), how a type is released (`release_kind`, a `DropKind`) and
  whether it must be used. `None` means "do not release", so an unknown type
  leaks rather than frees twice.
- `vyrn-frontend/src/prelude.rs`: every builtin's signature, with a
  capability per parameter, so a builtin borrows, modifies or consumes by the
  same rule as user code.
- `vyrn-frontend/src/own.rs`: the release vocabulary (`Release`, `Exit`,
  `DropKind`), the `vyrn why --memory` rows (`MemoryRow`), and the slots
  through which `vyrn-lower` installs the placer and its judgments.
  `own::analyze` makes one `Ownership` per program and memoizes it for the
  command.
- `vyrn-lower/src/core.rs`: `core::build` lowers a body into the named core.
  Every value has a name, every access is a place, and every release is a
  `St::Drop`. `core::augment` is the placer.
- `vyrn-lower/src/kernel.rs`: the linear judgment. `kernel::check` refuses a
  body; `kernel::placement` reports the releases it is missing.
- `vyrn-lower/src/typed.rs`: the typed judgment, and `typed::obligation`, the
  must-use rule.
- `vyrn-frontend/src/movecheck.rs`: the driver. `movecheck::refusals` merges
  the kernel's and the typed judgment's refusals into one list in source
  order. It states no rule itself.
- `vyrn-codegen/src/direct.rs`: the emitter reads each `St::Drop` and each
  placed `Release` row and emits a call. It places nothing.

## How a release is placed

`core::augment` runs inside `own::analyze`. It builds the core of every
instance, `test` and `bench` body, then asks `kernel::placement` of each
frame. The kernel walks the body in placement mode. Where an owned name is
still held at an exit and no release stands there, it records a `Missing` row
(`MissingKind` says whether at an exit, on a join edge, at an arm's end or at
a store), treats the name as released and goes on. `augment` turns each row
into a `Release`, keyed by the node the emitter walks, and writes the
`MemoryRow`s. A function with new rows is built again, so the core states
each release as a `St::Drop`. A refusal no placement repairs (a second
consume, a use after a release) is kept as the kernel's refusal.

A generic function is also judged once as written, with every type parameter
treated as owning heap, so a generic that is wrong for a `String` is refused
even if no caller instantiates it at one. Each projection body is judged on
its own, because no instance builds it.

A name is `Held`, `Gone` or `Static` (bound to a literal: nothing to release
until a store gives it a value). At every join, each owned name and each hole
is in the same state on every edge that reaches it; a path that returned,
broke or trapped reaches no join. Around a loop, a name bound outside has the
same state at the back edge as at entry, and a name bound inside is consumed
before the back edge.

Ownership and release are separate questions. `Kernel::owned` answers whether
the body owns a name, which holds for every value. `Kernel::releases` answers
whether it owes a release, which holds only for heap. A name that owes a
release moves at every take; one that owes none moves only into a `consume`
parameter.

A borrow is a name the body does not own whose type owns heap. A read out of
a place is an alias of the place: a take of it is refused, and a write to the
place, including a `modify` argument, ends it. A borrow with no place (a
parameter, a second name for one, a capture) carries a `core::BorrowKind`,
and a take of it is refused. A call that may store into module state ends
every borrow of that global (`effects::writes_state`), including a borrow it
takes as an argument, since the callee reads that until it returns.

A body the core cannot build returns a `Gap`. The kernel reports it as
"internal error: the core cannot state ..., so `f` is not judged", and the
emitter refuses to emit it. A gap is a compiler defect, never a user error.

## Diagnosing a refusal

`VYRN_NO_KERNEL=1 vyrn check <file>` stands the kernel aside. A refusal that
survives it is the checker's or the typed judgment's; one that disappears is
the kernel's. `refusals.rs` records the owner of each sentence this way, and
`testsweep.rs` runs every Vyrn program written inside a test's string
literals both ways. A program only the kernel refuses is a finding.

`columns.rs` holds every refusal to a real token, so the editor underlines the
name, not the whole line.

## The free audit checks the emitted module

The kernel claims, statically, that every owned name is released once. The
free audit checks that claim at run time, on the emitted wasm, so it also
sees defects in the emitter and the runtime.

Set `VYRN_LEAK_CHECK=1` in the compiler's environment. `std/runtime`'s
allocator then counts every block (`auditBirth`, `auditDeath`) and the
emitter calls `auditInit` first in `_start` and `auditExit` last. The program
reports:

- a double or foreign free: one line on standard error and exit 134, at the
  site;
- residue at exit: `free audit: N block(s), M bytes, never freed` and exit 135;
- otherwise its own exit code.

An unaudited build emits no audit call, and `Module::sweep` drops the audit
functions, so a shipped module holds none of it. The generator host and the
language server never reach it.

The audit is a test instrument, not a language rule. `tests/residue.rs` runs
every corpus program under it on both engines against a baseline of `clean`,
`leak N` and `other` rows. A double free fails whatever the baseline says; a
`clean` row that leaks fails; a `leak N` row may only shrink. `tests/memory.rs`
runs selected shapes under it. The audit stays until the kernel has a second
oracle that catches what the audit catches.
