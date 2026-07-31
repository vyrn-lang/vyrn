# RFC-0082 — Containers Are Vyrn: `Array` Is the Primitive

- **Status:** **Accepted**, **M1 shipped** (see "As landed"), M2–M4 not started.
  Milestones are gated on measurement the way RFC-0081's were: nothing is
  deleted until a number says it should be.
- **Depends on:** RFC-0078 (the census, and question **A**, which this
  reframes), RFC-0011 (`a[i] = v` element store, and the `a[i].field = v`
  desugar this extends), RFC-0028 (`Map<String, V>`), RFC-0056 (`SmallArray`),
  RFC-0079 (`panic`, so a Vyrn container can trap), RFC-0081 (the measurement
  discipline, and "unreferenced multiplicity" as the actual objection)
- **Supersedes:** RFC-0078's open language question **A**, "a raw-memory view".
  That question is withdrawn rather than answered — see below.

## The question this replaces

RFC-0078 recorded that `Array`, `Map`, `SmallArray`, the slot table and the
allocator "would be Vyrn if the language could name raw memory — 16 of the 62
census rows", and framed the decision as **"what a checked language gives up to
write its own allocator, and whether an `unsafe`-shaped region is a price worth
paying for a row of the census."**

That framing is wrong, and the reason is worth stating precisely because it was
believed for four RFCs.

## Why Rust needs `unsafe` for `Vec`, and why Vyrn does not

The operation that forces `unsafe` in `Vec` is **`set_len`**: it asserts that
memory is initialized when the type system cannot see that it is. That is the
entire reason `MaybeUninit` exists — a `Vec` holding uninitialized elements is
undefined behaviour under most safe operations, including being dropped. The
unsafety is not "pointers"; it is **uninitialized capacity being observable**.

Vyrn's `Array` never exposes that state. It owns `len` and `cap` together, every
read is checked against `len`, and spare capacity is allocated but unreadable —
there is no `set_len` to be unsafe. This is the arrangement Java, C# and Go use:
collections are ordinary safe code over an array primitive, and nobody reaches
for pointers to write one.

So `Array<T>` is *already* the right primitive, and the containers standing on it
were never blocked on a memory view. They were blocked on something much smaller.

## The demonstration

A generation-checked slot table — Vale's whole mechanism, and four of the sixteen
census rows — in ordinary Vyrn over `Array<T>`. It ran even before M1, with the
move-out / mutate / move-back written by hand:

```vyrn
    if t.free.length > 0 {
        let mut fr = t.free          // the workaround, four times over
        let i = fr[fr.length - 1]
        fr.pop()
        t.free = fr
        let mut sl = t.slots
        let mut s = sl[i]
        s.val = v
        s.live = true
        sl[i] = s
        t.slots = sl
        return Handle { slot: i, gen: s.gen }
    }
```

Since M1 that is what the compiler writes, and the source says what it means:

```vyrn
type Slot = { gen: Int64, val: Int64, live: Bool }
type Handle = { slot: Int64, gen: Int64 }
type Table = { slots: Array<Slot>, free: Array<Int64> }

fn newCell(t: modify Table, v: Int64) -> Handle {
    if t.free.length > 0 {
        let i = t.free[t.free.length - 1]
        t.free.pop()
        t.slots[i].val = v
        t.slots[i].live = true
        return Handle { slot: i, gen: t.slots[i].gen }
    }
    t.slots.push(Slot { gen: 1, val: v, live: true })
    return Handle { slot: t.slots.length - 1, gen: 1 }
}

fn fetch(t: Table, h: Handle) -> Option<Int64> {
    if h.slot < 0 || h.slot >= t.slots.length { return None }
    let s = t.slots[h.slot]
    if !s.live || s.gen != h.gen { return None }
    return Some(s.val)
}
```

Output, with `recycle` bumping the generation and pushing the slot on the free
list: `42` live, `-1` after release, `7` for the reused slot, `-1` for the old
handle *while that slot is live again*, `99` untouched. No pointers, no `unsafe`,
no capability, no new primitive. The whole thing is `examples/slottable.vyrn`,
byte-identical on all three engines.

## The real blocker, pinned

| | |
|---|---|
| `a[0] = 9`, plain array variable | works |
| `r.a[0] = 9`, array in a record field | **refused** — "must be a plain array variable" |
| reading `r.a[0]`, and `r.a.push(v)` | work |

A container *is* a record holding arrays, so every container hits this on its
first line. Two smaller gaps ride along: `pop` also demands a plain array
receiver, and `cell` / `get` / `release` are reserved names.

*(M1 closed the first two rows and `pop`, and found a third row this table
missed: `rows[i][j] = v` was refused too, while `rows[i].push(v)` was not. The
reserved names are still reserved.)*

**The workaround is a move, not a copy** — measured, because the whole question
turns on it. Moving the array out of the field, mutating, and moving it back is
**O(1) per write**: 57 / 53 / 65 ms at N = 5,000 / 10,000 / 20,000, flat as N
quadruples. Every line of the slot table above uses it, which is exactly why that
code is uglier than it should be and no slower than it should be.

So the blocker is **ergonomic, not a capability gap**, and the fix is a desugar
rather than a language-identity decision.

## What this costs, compared with what it replaced

The raw-memory design carried three costs, and all three are gone:

- **No `unsafe`.** Nothing acquires the power to violate ownership, drops or
  validation.
- **No capability or module-path gate**, so **no ecosystem ceiling**: a
  third-party container gets exactly the substrate `std` gets. The version of
  this RFC with a `std/`-only memory import would have made `std/array.vyrn`
  readable Vyrn that users were forbidden to imitate — a visible ceiling where
  today's is invisible.
- **No heap simulation in the interpreter.** `Array` stays `Rc<Vec<Val>>` there.
  This was the largest measured risk in the raw-memory design: the interpreter is
  180x a Rust one-liner on hot paths (RFC-0081), and array indexing is far hotter
  than float formatting. It does not arise, because `Array` is not being moved.

## Milestones

### M1 — `r.a[i] = v` — **shipped**

Desugar an index assignment whose base is a field access into the move-out /
mutate / move-back that is legal today, exactly as `a[i].field = v` already
desugars through a temp (RFC-0011). Extend `pop` and the other plain-array-
receiver builtins the same way.

M1 is independently useful and ships alone: it removes a wart every user who
puts an array in a record hits, whether or not any container ever moves.

Pin: the slot table above, written the *natural* way, byte-identical on three
engines — and a test that the desugar is a move, since a copy would be correct
and quadratic. Prefer a structural assertion over timing.

#### As landed

One function, `place_receiver` in the parser, and no backend change at all —
LLVM, the interpreter and the direct wasm backend never learn that a field is
involved. It is recursive rather than one-level, which made it *shorter* than
the special case and picked up two shapes the spec did not ask for:

- `r.inner.a[i] = v` — an array two records deep.
- `rows[i][j] = v` — an element of an element. This was refused before, even
  though `rows[i].push(v)` was not, so the wart was wider than "a record field"
  and the table at the top of this RFC understated it.

The pin is `examples/slottable.vyrn` (three engines, `42 / -1 / 7 / -1 / 99` as
predicted) plus `vyrn-cli/tests/places.rs`, which asserts the emitted IR for a
field index-assign, a field `pop` and a two-deep chain contains **no call
outside the trap path** — no `malloc`, no `memcpy`, no per-element loop. The
negative control is the same function doing `s.xs.push(i)`, which does emit one.

Four things the spec got wrong or did not see:

1. **`pop` is not "the same way".** `a[i] = v` is a statement, so the move-back
   has an obvious home. `pop`/`swapRemove`/`remove` mutate *and* return a value,
   so they live inside an expression and the move-back has nowhere safe to go in
   general: in `if r.a.pop() == None { .. r.a .. }` the branch body would read
   the field before the write-back landed. They are therefore hoisted only when
   the mutating call **is** the whole statement — `r.a.pop()` alone, or
   `let x = r.a.pop()`. Everything else keeps the old error. `match r.a.pop() {
   .. }` is the case a user will hit; it needs the mutation to name a place the
   backends can address, not a bigger desugar, and that is M2's problem if M2
   needs it.
2. **`push` was left alone**, so `r.inner.a.push(v)` is still refused while
   `r.inner.a[i] = v` works. `push` already writes back at one level via a
   single `SetField`; routing it through `place_receiver` would turn the common
   `r.a.push(v)` from one statement into three to buy a case nobody has asked
   for.
3. **The trap question resolves trivially**, but only because traps abort: with
   the array moved out, an out-of-bounds `r.a[99] = v` leaves the field stale for
   the instant before `exit(1)`, and nothing can observe it. If Vyrn ever gains a
   recoverable trap, this desugar has to be revisited — RFC-0079's `panic` does
   not, today.
4. **`cell` / `get` / `release` are still reserved**, which the RFC noted as a
   gap "riding along" and M1 did not close. `examples/slottable.vyrn` says
   `newCell` / `fetch` / `recycle`. It is a naming collision with the `Ref`
   builtins, not a capability gap, but a container library written in Vyrn cannot
   spell its own operations the obvious way until it is resolved.

### M2 — the slot table in Vyrn

The smallest container, four census rows, and the one already demonstrated.
**Delete nothing.** Land it beside the builtin and measure all three engines plus
the example corpus, the way RFC-0081 M1 did.

Gate: the interpreter's number. `cell`/`get`/`set`/`release` are not as hot as
array indexing, but they are hotter than float formatting was, and RFC-0081 found
the interpreter 180x on an arithmetic loop. If that number is bad here, M3 and M4
do not happen and M2 still paid for itself by replacing an assumption with a
measurement.

### M3 — `Map` over `Array`

Three rows. `Map<String, V>` is `String`-keyed, and `std/hash` already exists, so
this needs nothing new. Same gate.

### M4 — `Array`'s derived operations

`@list`, `@toArray`, `@pop`, `@swapRemove` — four rows that are operations *on*
the primitive rather than the primitive. Same gate.

## What stays primitive, and why that is the honest end state

`array`, `push`, `at`, `alen`, `afree` — the growable buffer itself. Five rows,
not sixteen. You cannot write the thing that allocates memory in terms of itself,
which is the same argument the `Syscall` category makes and is not a deferral.

If every milestone lands, "the runtime is Vyrn" holds for every container a
program touches, and the irreducible core is a buffer and a syscall table.

## What this does not decide

**Whether a raw-memory view should ever exist** for its own sake — FFI struct
layouts, SIMD alignment, zero-copy over foreign buffers. Those are real and this
RFC does not serve them. It only withdraws the claim that *containers* need one.

**`Array` itself in Vyrn.** That needs the raw view, and the interpreter cost
measured in RFC-0081 suggests it would be expensive. Not proposed.
