# RFC-0082 — Containers Are Vyrn: `Array` Is the Primitive

- **Status:** **Accepted**, **M1 shipped** (see "As landed"), **M2 stopped at its
  own gate** — the port is blocked twice over and the gate failed anyway; M3 and
  M4 are **withdrawn**. The two interpreter quadratics M2 found instead of the
  port — the write `t.xs[k] = v` and the append through a place, at both the
  field `t.xs.push(v)` and the element `rows[i].push(v)` — are all **fixed**
  (M2's "As landed"). Milestones are gated on measurement the way
  RFC-0081's were: nothing is deleted until a number says it should be, and here
  a number said stop.
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

The operation that makes a safe `Vec` *unbuildable from safe parts* is
**`set_len`**: it asserts that memory is initialized when the type system cannot
see that it is, which is the entire reason `MaybeUninit` exists — a `Vec` holding
uninitialized elements is undefined behaviour under most safe operations,
including being dropped.

**This paragraph originally said "the operation that forces `unsafe` in `Vec`",
which is too strong and was repeated as settled several times before anyone
checked it.** `Vec`'s implementation needs `unsafe` for more than that: pointer
arithmetic to reach element *i*, `ptr::read`/`ptr::write` to move a value out of
a slot without dropping it, and `dealloc`. `set_len` is the reason a *safe*
`Vec` cannot be assembled from safe primitives; it is not the only unsafe
operation inside the real one.

The conclusion survives by a different and simpler route: **every one of those
operations is about addressing a pointer, and none of them arises if the
primitive is a bounds-checked growable buffer instead.** That is the Java, C#
and Go arrangement — collections are ordinary safe code over an array — and it
is what `Array` already is.

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

**The workaround is a move, not a copy — on the compiled backends only, and this
paragraph originally said it without the qualifier.** Moving the array out of the
field, mutating, and moving it back is O(1) per write natively and on wasm:
57 / 53 / 65 ms at N = 5,000 / 10,000 / 20,000, flat as N quadruples.

Those numbers were taken with `vyrn build` and the resulting executable — **the
native path — and then stated as a property of the language.** They are false on
the interpreter, which is the dev loop. Measured there, the same program is
quadratic: 998 ms / 886 ms / **29,881 ms** at N = 2,000 / 8,000 / 32,000, against
a flat 58 / 65 ms for `xs[k] = v` on a plain variable. M2 found this and named the
mechanism; see its "As landed", item 5. The claim that the whole RFC turns on is
therefore **half true**, and the half that is false is the half a user meets
first.

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

**That pin covers two engines of three, and M2 found the third is a copy.** See
M2's fifth finding: the interpreter's write-through is O(len) per write, so the
"O(1) per write, not a copy" claim below is true of LLVM and the direct wasm
backend and false of the interpreter. The claim is not wrong about the *desugar*
— it is wrong about how many engines the desugar's shape survives into.

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

#### As landed — **the port did not happen, and the gate failed**

M2 asked two questions before writing anything. Both answered *no*, independently,
and the gate then failed on a proxy measurement. Nothing was ported and nothing
was deleted. What follows is the measurement that replaced the assumption.

**1. `Ref<T>` cannot become a Vyrn record.** Not because a record is the wrong
shape — `{ slot: Int64, gen: Int64 }` is byte-for-byte the `{ i64, i64 }` the two
compiled backends already use — but because **the slab is type-erased and the
Vyrn one cannot be**. Natively the payload is `malloc`'d and the slab is
`[65536 x ptr]`, so one global table serves every `T`; in the interpreter it is
`Vec<CellSlot>` holding a heterogeneous `Val`. A Vyrn table is `Array<Slot<T>>`,
one per `T`, and `examples/genref.vyrn` alone instantiates `Ref<Int64>`,
`Ref<String>` and `Ref<Ref<Int64>>` in one program. Vyrn has no generic module
state to hold them — `let mut cells: Array<Slot<T>> = []` is `unknown type
\`T\``, because a module-level `let` has no `<T>` to bind. Nor can a `routes` row
express it: a route is a rename whose "argument types are fixed", and
`cell(v: T) -> Ref<T>` is type-directed in exactly the way `charCountV` is not.

Threading the table explicitly — what `examples/slottable.vyrn` does — is not the
same API and does not answer the question: `cell(v)` takes no table, and `get(r)`
takes only `r`, so **the ambient slab is part of the type's contract**, not an
implementation detail behind it.

Three more things a Vyrn record would silently drop, recorded because they are
the real cost of ever revisiting this: `own.rs` maps `Type::Ref(_)` to
`DropKind::ReleaseRef`, and `examples/autorelease.vyrn` runs a million
allocations through 65,536 slots *because* that auto-release fires — a plain
record gets none of it; `set` has a bespoke escape rule in `own.rs` and a
`region_store_guard` in the checker; and `Type::Ref` is what breaks the size
cycle for `Node = { value, next: Option<Ref<Node>> }`.

**2. The table cannot be module state — a generator would break.** `cell`/`get`/
`set`/`release` are *not* in `COMPTIME_FORBIDDEN`, and a `gen fn` really does use
them: a generator allocating a cell, mutating it and reading it back at
generation time compiles and prints `42` today. The moment that slab is a
module-level `let`, `check_comptime_purity` rejects it — the diagnostic is
already exact, and it would name a `std/` internal:

> `gen fn g` is not comptime-pure: it reaches `poke` (via g -> poke), which reads
> or writes module state

So the second pre-question is not "is module state plausible here" but "is
`cell` allowed in a generator", and it is.

**3. The gate, measured on a proxy.** The port is blocked, but the number the
gate wants — *what does a container written in Vyrn over `Array<T>` cost* — is
measurable without it, because `examples/slottable.vyrn` is that container. The
workload is `examples/freelist.vyrn`'s, unchanged in shape: build a 5-node list,
sum it, free it, N times. Best of 3, milliseconds, floor in parentheses (a
`print` and exit: interp 10 ms, native 5 ms, wasm 13 ms).

| N (cells) | interp builtin | interp Vyrn | native builtin | native Vyrn | wasm builtin | wasm Vyrn |
|---|---|---|---|---|---|---|
| 25,000 | 124 | 2,009 | 6 | 9 | 15 | 18 |
| 100,000 | 449 | 7,873 | 8 | 19 | 19 | 26 |
| 400,000 | 1,751 | 31,456 | 17 | 59 | 33 | 58 |

Floor-subtracted, that is **~18x on the interpreter**, ~4.5x native, ~2.3x wasm,
and flat in N at all three. Byte-identical output (`15`) on all three engines at
every size.

**18x fails the gate.** RFC-0081's 180x was an arithmetic loop and 6.2x was a
print-heavy program; this sits far nearer the bad end, and unlike `charCountV` it
would be paid by `Ref`, which is a *memory primitive* rather than a formatting
one. **M3 and M4 are withdrawn**, not deferred: `Map` over `Array` is the same
shape with a hotter access pattern, and `@pop`/`@swapRemove` are operations on
the primitive whose whole value is being one instruction.

**4. What the 18x is not.** It is not the ambient-slab difference dressed up: the
Vyrn version threads a `Table` parameter and does its bounds/live/generation
checks in Vyrn while the builtin does them in Rust, and that is exactly the
comparison — it is what the census row would cost. The stale-handle rejection,
which is the entire point of the generation, is byte-identical on all three
engines both ways: the Vyrn table answers `-1` and the builtin traps with
`error: reference used after release` and exit 1, and all three engines agree on
both.

**5. The finding that outranks the gate: the interpreter's write-through is a
copy.** The freelist workload's table never exceeds five slots, which is
precisely the case a copying desugar survives — so M2 ran the other shape, N
cells live at once, and the interpreter fell off a cliff the compiled backends
never saw. At N = 4,000, floor-subtracted:

| | builtin | Vyrn table |
|---|---|---|
| interp | 5 ms | 23,943 ms |
| native | ~0 ms | ~0 ms |
| wasm | 3 ms | 2 ms |

**~5,000x on the interpreter, indistinguishable on both compiled backends.** That
asymmetry names the cause exactly. Isolated to one statement, `t.xs[k] = v` in a
loop against `xs[k] = v` on a plain variable:

| N | plain `xs[k] = v` | field `t.xs[k] = v` |
|---|---|---|
| 2,000 | 0.026 s | 0.118 s |
| 8,000 | 0.029 s | 0.747 s |
| 32,000 | 0.051 s | 20.6 s |

The plain column is flat (it is all process floor); the field column is
quadratic. The mechanism is three lines: M1's desugar emits `let mut t.xs[] =
t.xs`, which in the interpreter clones the `Rc<Vec<Val>>` to refcount 2, so the
`Rc::make_mut` in `Stmt::IndexSet` (`interp.rs`) **deep-copies the whole vector
on every write**, and the write-back drops the original. In the compiled backends
the same three statements copy a `{ ptr, len, cap }` header into an alloca and
write through the *same* buffer — the aliasing is benign and free, which is why
`places.rs` correctly finds no `memcpy` there.

This is **not an M1 regression**: the pre-M1 hand-written `let mut p = t.xs; p[k]
= v; t.xs = p` measures identically (0.106 s vs 0.111 s at N = 2,000), because it
is the same three statements. M1 made a quadratic idiom *reachable by writing the
obvious thing*, and the RFC's claim that the workaround is "O(1) per write,
measured" was measured on the compiled path only.

**The fix is not small, and M2 deliberately did not attempt it.** The obvious
repair — have the interpreter *take* the field instead of cloning it — is
unsound as the desugar stands, because the clone is load-bearing for correctness:
`t.xs[t.xs.length - 1] = 99` and `u.xs[0] = u.xs[2] + 5` both read the field
while it is moved out, and both give the right answer today (`99`, `8`) only
because the field still holds a copy. Making the interpreter's move a real move
requires the desugar to rewrite reads of the field inside the index and value
expressions to read the temp — which is a change to the desugar in all three
engines, not an interpreter patch, and it is its own RFC.

##### As landed — the take, and two more quadratics underneath it

Two parts, in this order. **The desugar hoists** every operand that can reach a
place — the index, the value, a nested index, a mutating call's arguments — into
its own temp *before* the move-out, so nothing reads the container while it is
gone. **The interpreter then takes**: `let mut t.xs[] = t.xs` replaces the field
with `Val::Unit` and hands over the `Rc`, so `Rc::make_mut` in `IndexSet` sees
refcount 1 and writes through. Neither compiled backend changed, and that was
checked rather than assumed: `places.rs`'s IR assertions are byte-for-byte the
same before and after, because hoisting adds statements that lower to the same
loads and both backends already wrote through one buffer.

Isolated `t.xs[k] = v`, best of 3, milliseconds, three engines (process floor
~35 ms):

| N | interp before | interp after | native | wasm |
|---|---|---|---|---|
| 4,000 | 95 | 42 | 27 → 33 | 39 → 42 |
| 8,000 | 276 | 46 | 30 → 31 | 40 → 41 |
| 16,000 | 981 | 58 | 28 → 34 | 37 → 42 |
| 32,000 | 9,942 | 75 | 28 → 39 | 39 → 41 |
| 64,000 | 39,116 | 114 | 30 → 34 | 42 → 44 |

The after column matches `xs[k] = v` on a plain local (38 / 43 / 45 / 57 / 71) to
within noise. That is the complexity class changing, not a constant.

**The self-referential cases are the pin, and the M2 spec was right about them**:
`examples/placeorder.vyrn` runs `99`, `8`, `f`/`g` once each left to right, a
nested index evaluated once, a two-deep place and a call reaching module state —
byte-identical on three engines. The complexity class is pinned in `places.rs` by
a *ratio*: the same N writes, once on a local and once through a field, must land
within 4x of each other. Before, that ratio was 503x. There is no IR to count for
the interpreter, which is exactly why this shipped.

Four things this milestone found that the diagnosis above did not:

1. **Vyrn already has the recoverable trap the M1 note said would force a
   revisit.** `vyrn test` catches a trapping test and runs the next one, so a
   hole left in *module state* outlives the trap and the next test reads
   `at of non-Array/Int64` — a value no program can otherwise produce. The take
   is therefore **locals only**: a local's frame is popped on the error path and
   never read again, and a `modify` parameter's write-back does not run either
   (checked). A global keeps the copy and stays quadratic. Pinned by
   `a_trapping_test_does_not_leave_a_hole_in_module_state`.
2. **The measurement above was two quadratics, not one.** M2's program built its
   array with `t.xs.push(0)` in a loop, and `push` through a field is its own
   quadratic — 138 / 481 / 1,582 / 8,186 ms at N = 4,000 → 32,000, untouched by
   this fix and fixed by the next one (below).
   `Stmt::Assign` has an in-place fast path for `xs.push(v)` on a local;
   `Stmt::SetField` has none, and the `push` builtin clones its argument
   unconditionally so the take alone would not help. That is the non-monotonic
   998 / 886 ms at N = 2,000 / 8,000 in the table at the top of this RFC: at those
   sizes both programs were mostly compile floor.
3. **`rows[i][j] = v` is still quadratic, for a third reason.** The write-back
   `rows[i] = rows[]` coerces into the declared element type, and `coerce` on an
   array **rebuilds the whole vector** even when the element type cannot change a
   value. Confirmed by probe: short-circuiting `coerce` for `Array<Int64>` takes
   the nested loop from 786 / 2,613 / 16,633 / 65,304 ms to 44 / 50 / 67 / 99 at
   N = 8,000 → 64,000. Not fixed here — a general `coercion_free` predicate
   decides when automatic validation may be skipped, and that is a validation
   change, not a places change. (Fixed below, with finding 6: it turned out to
   be the same change, because a field store that validates cannot afford it.)
4. **A nested index was evaluated twice.** `place_receiver` cloned the index
   expression into both the load and the write-back, so `rows[f()][0] = 1` called
   `f` twice — visible in the baseline binary, and fixed by the hoist that
   soundness needed anyway.

The trap caveat is unchanged in shape and narrower in scope: between the take and
the write-back only a trap can escape (every `?` and every call now runs during
the hoists), the field is `Val::Unit` for that instant, and no locals survive the
one boundary that recovers.

##### As landed — the append, which needed the take's rule and not its take

Finding 2 above, fixed. `t.xs.push(v)` desugars to `t.xs = push(t.xs, v)`, so the
general path read the field into a *second* `Rc` while the field still held the
first, and the `push` builtin's `Rc::make_mut` copied the whole vector on every
append. Isolated, best of 3, milliseconds, three engines (process floor ~45):

| N | interp before | interp after | native | wasm |
|---|---|---|---|---|
| 4,000 | 135 | 44 | 44 → 40 | 49 → 50 |
| 8,000 | 310 | 48 | 44 → 39 | 49 → 54 |
| 16,000 | 1,705 | 55 | 44 → 46 | 49 → 49 |
| 32,000 | 10,704 | 57 | 47 → 47 | 55 → 51 |

`xs.push(i)` on a plain local is 45 / 44 / 49 / 49 before and 45 / 45 / 52 / 52
after — the same numbers as the field column now is, which is the point: process
floor either side, and the class changed. Both compiled backends were flat at
every N before *and* after, so the whole cliff was the interpreter's, again.

That the backends did not change was checked and not assumed, and the check needed
a correction: the `.exe` embeds a timestamp and the direct wasm backend's DATA
section is not reproducible across compiler builds — a dead `panic` string from
`std/num` is interned or not depending on the compiler binary's own layout, which
reproduces from adding an unused function to `interp.rs` and has nothing to do
with this change. The emitted LLVM IR is byte-identical, and so is the wasm
**code** section (10) along with every other section but the pool and the
heap-base global derived from its length.

**The take was the wrong tool here, and that is the finding.** `take_place`
exists because the index-store desugar had already split the statement in three,
so the container HAD to live in a temp and the move-out has to leave `Val::Unit`
behind it. An append is one statement, so the array never has to leave the
record: the fast path clones the field's `Rc` *before* the item is evaluated,
drops the field's own reference *after*, and grows the now-unshared snapshot.
Dropping that reference is what makes the append O(1); cloning it first is what
keeps the general path's evaluation order, and that is not academic —
`t.xs.push(f(t))` with `f` taking `t: modify T` reaches the same field
mid-statement, and its write is discarded by all three engines both before and
after. Taking early would have made that program trap on `push of non-Array
Unit`; appending in place without the snapshot would have made it print `2`
where every engine prints `1`.

**The locals-only rule is reused, not re-derived, and it is weaker here on
purpose.** The only escape between dropping the field's reference and storing the
grown array back is an out-of-memory `reserve`, and locals are the exact scope in
which `vyrn test`'s recovery cannot observe a hole (finding 1). A global keeps the
copy and stays quadratic — the same sentence as the take, and the same
`store.xs.push(v)` it already named.

Pinned by `the_interpreter_does_not_copy_the_array_once_per_append` in
`places.rs`, the sibling of the write ratio and deliberately the same shape: N
appends onto a local against N appends through a field, within 4x. On the pre-fix
binary that ratio is 449x (8.90 s against 19.8 ms) and the test fails; after, it
is 1.1x. The two ratios now share one `best_of_3`.

Two more things this found, neither fixed here (finding 6 is fixed in the section
after this one; finding 5 is still open):

5. **`rows[i].push(v)` — an append through an array ELEMENT — is the same
   quadratic and is still there**: 275 / 735 / 3,307 ms at N = 4,000 → 16,000.
   It desugars to `Stmt::IndexSet` rather than `SetField`, so it is a third
   receiver form and a third copy of the same twenty lines. Left until something
   measures it in a real program: `t.xs.push(v)` is what the corpus writes.
   (**Fixed**, in the last section of this milestone. It was a third receiver
   form and it was *not* a third shape, which is why it did not become a third
   copy.)
6. **The interpreter does not validate an append through a field at all**, and
   both compiled backends do. `type Age = Int64 where value >= 18` with
   `t.xs: Array<Age>` and `t.xs.push(5)` prints `5` under `vyrn run` and traps
   with `validation failed for `Age`` under native and wasm — two engines against
   one, so the interpreter is the wrong one. This predates the fix above and
   survives it unchanged (`Stmt::SetField`'s general path never coerced, and the
   fast path was written to match it rather than to quietly change it): the
   element type would have to be resolved through the record's declared type,
   which is the same `coerce`-and-validation question finding 3 is, and it is not
   a places change either. No example reaches it, which is why parity is green.

##### As landed — findings 6 and 3, which really were one change

Finding 6 was the more serious of the two by a wide margin: the interpreter
accepted a value that failed its own predicate, so RFC-0078 M3's "a Vyrn program
cannot even spell a value that failed its own predicate" was false as written for
a whole class of program. `Stmt::SetField` was the only typed boundary in the
interpreter with no coercion behind it, and coercion is where automatic
validation lives.

The fix is the field's declared type, resolved the way the checker's own
`SetField` resolves it (the binding's type, then that record's field). The append
fast path coerces just the pushed element; the general path coerces the value.
Module state needed one more thing: an unannotated global kept `ty: None`, unlike
an unannotated local, whose `Stmt::Let` has inferred its type since RFC-0018 so
`toJson` can order fields. A global record therefore had no field types to check
against at all, and `g.xs.push(v)` was the last spelling still diverging after
the local and `modify` ones were fixed. Globals now infer the same way.

The audit around it mattered more than the fix. Five sibling spellings, three of
which nobody had reason to believe were correct:

| spelling | before |
|---|---|
| `t.xs.push(v)` on a local | **diverged** — interp printed 5 |
| `t.xs.push(v)` through a `modify` parameter | **diverged** |
| `g.xs.push(v)` on module state | **diverged** |
| `t.xs[i] = v` | correct — the desugar's temp is typed by inference, and `IndexSet` coerces its element |
| `xs.push(v)` on a local | correct — `Stmt::Assign` coerces through the binding's declared type |
| `rows[i].push(v)` | correct — `IndexSet` coerces into the element type |
| `t.m[k] = v` on a `Map` field | correct — the map path coerces into `V` |

Finding 3 is closed by the same change, and not as a bonus — as a prerequisite.
A field store that coerces lands on the write-back every place desugar ends with
(`t.xs[i] = v` is `let mut t.xs[] = t.xs .. t.xs = t.xs[]`), so the container
would be re-walked once per store. `coerce` now returns the value untouched when
coercing into a type can neither change it nor reject it — no width to wrap to,
no predicate to run, nothing nested with either. Measured on 160,000 element
stores, varying only the row length so the number of writes is constant:

| grid | before | after |
|---|---|---|
| 1600 x 100 | 457 | 268 |
| 400 x 400 | 964 | 265 |
| 100 x 1600 | 2,902 | 269 |
| 40 x 4,000 | 6,950 | 272 |

The before column is the row length; the after column is not. Pinned by
`the_interpreter_does_not_rebuild_a_row_per_element_store` in `places.rs`, a
ratio between the first and last rows for the reason the other two ratios there
are ratios.

That leaves the case the short-circuit cannot help, which is the one the
validation is FOR: `t.xs[i] = v` where the elements really are validated. 8,000
stores through an `Array<Age>` field measured **13,467 ms** with the whole-array
coerce on the write-back, against 76 for `Array<Int64>`. So a plain variable
already OF the field's type is skipped — its values passed their own boundary on
the way in, and this is the compiled backends' own rule rather than an
interpreter shortcut: `validation_required` answers `None` when `from == to`,
which is why native emits no element loop at a field store at all. Variables
only, deliberately — `push(t.xs, v)` is statically `Array<Age>` too, and its
element has been validated by nothing at that point. 13,467 → 79 ms, and
`a_validated_element_type_costs_a_constant_per_store` says so.

The validation itself is cheap where it now runs: 32,000 appends through a field
cost 88 ms into `Array<Age>` against 79 into `Array<Int64>`.

`examples/validate_store.vyrn` is the corpus's half — a local, a `modify`
parameter and module state, valid stores printing before an invalid one traps.
Its absence is exactly why this survived: no example pushed a runtime value into
a validated array through a field, and a literal is folded by `consteval` before
any engine runs.

Finding 5 is still open and still quadratic — 206 / 402 / 1,648 ms at
N = 4,000 → 16,000, down from 275 / 735 / 3,307 because the short-circuit removes
the coerce but not the `push` builtin's clone, which is what makes it quadratic.
(Closed in the section below.)

7. **The audit found a second, unrelated validation divergence, and this one is
   the textual backend's.** A validated payload inside an `Option` or a `Result`
   is never checked natively: `let a: Option<Age> = Some(rt(6))` prints and exits
   0 under `vyrn build`, and traps under `vyrn run` and under wasm. One engine
   against two, so native is the wrong one — the mirror image of finding 6, found
   only because the field audit asked what else a value can flow into. It reaches
   through every spelling tried: a `let`, a return, a record literal, a field
   store, and `Ok(..)` as well as `Some(..)`.

   The seam is exact and small. `Some`/`Ok`/`Err` push the expected payload type
   so the payload is *typed* by it (RFC-0037), and then never coerce into it; the
   outer `coerce` cannot make up the difference because `validation_required`
   looks at the `Option` and not through it. Four lines at each constructor fix
   every case above — and were tried, and are not in this commit.
   `result_array_payload_rematerializes_on_coerce` fails with them: coercing the
   payload at construction reshapes an `ArrayN` literal into the growable
   `Array` at the source, so the outer tag-branch `rebox_sum` it pins no longer
   fires for that shape. That is plausibly an improvement and it is certainly a
   change to what a *correct* program compiles to, which is not a thing to do at
   the end of somebody else's milestone. Whoever takes it should decide first
   whether `rebox_sum` still has a caller, because the answer may be no.

   **Taken, and the answer was no.** Both constructors now coerce the payload
   into the expected type before boxing it, exactly as the user-enum constructor
   beside them always has. The three diverging spellings — `Some`, `Ok`, `Err`
   with a scalar validated payload — all trap natively with the bytes the other
   two engines already produced. The neighbours were already correct and for a
   reason worth writing down: an array payload (`Some([rt(21), rt(6))])` into
   `Option<Array<Age>>`) and a record payload (`Ok(Row { age: rt(6) })`) validate
   at the *literal*, where the element and field expectations already coerce, so
   the payload was a validated value before the constructor ever saw it. Only a
   scalar makes the constructor itself the boundary. `Option<Option<Age>>` is not
   expressible: the checker rejects nested `Option`/`Result` in v0.1, on all three
   engines.

   `rebox_sum` had no caller left and is deleted. Subsumed, not bypassed, and the
   checker is what settles the difference: it already reports these constructors
   at the *expected* payload type (`Some` returns `Option<want>`, not
   `Option<typeof x>`), and it refuses an `Option<Array<T, N>>` flowing into an
   `Option<Array<T>>` for any expression except an array literal directly under a
   constructor — `fn g() -> Option<Array<Int64, 3>>` passed to an
   `Option<Array<Int64>>` parameter is a type error, not a reshape. So the only
   value that could ever reach that repair was the one construction now reshapes
   at the source. The pin stays, renamed to
   `result_array_payload_is_boxed_in_the_target_representation`: same invariant —
   the boxed payload is the growable triple and the arm loads one back — asserted
   where it is now held, plus the absence of any `rebox` branch, since one
   reappearing would mean construction had stopped reshaping.

   The cost is a class, and it is smaller than expected: 89 of the 92 examples
   emit byte-identical IR, because `coerce` adds instructions only when
   `validation_required` says so or a representation actually reshapes. Of the
   three that moved, one got *shorter* — `enumarray.vyrn` loses the whole
   `rebox` branch and one of its two `malloc`s per `Ok([..])`, since the old path
   boxed the fixed literal and then allocated a second buffer to re-materialize it
   — and one got longer for the right reason: `i18ndemo.vyrn` gained seven
   `__vyrn_regex_run` calls, one per `Some(k)` returning a validated `TransKey`.
   That is this same bug, a second instance of it, inside `std/i18n`, which native
   had been skipping and the other two engines had been paying all along.

   `examples/validate_sum.vyrn` is the corpus's half, beside
   `validate_store.vyrn` and for the same reason: no example put a runtime value
   into a validated sum payload, and a literal is folded by `consteval` before any
   engine runs.

##### As landed — finding 5, which was a third receiver form and not a third shape

The last quadratic. `rows[i].push(v)`, isolated, best of 3, milliseconds, three
engines (process floor ~45):

| N | interp before | interp after | native | wasm |
|---|---|---|---|---|
| 4,000 | 211 | 50 | 46 → 42 | 58 → 54 |
| 8,000 | 401 | 50 | 41 → 45 | 54 → 53 |
| 16,000 | 1,744 | 55 | 38 → 43 | 49 → 51 |
| 32,000 | 9,420 | 65 | 42 → 51 | 67 → 50 |

`xs.push(i)` on a plain local is 49 / 47 / 58 / 55 either side — the after
column is that column. Both compiled backends flat at every N before and after,
which was checked and not assumed: **all 93 examples emit byte-identical IR and
byte-identical wasm**, whole modules this time and not only the code section,
because the change touches no backend at all.

**Finding 5 was wrong about the cost and right about the receiver.** It is a
third receiver form; it is not a third *shape*, and the shape is what decides
which mechanism applies. The parser emits ONE `Stmt::IndexSet` for this
statement — `rows[i] = push(rows[i], v)` — exactly as it emits one
`Stmt::SetField` for `t.xs.push(v)`. Nothing is split into a temp, so, by the
argument the append above makes, nothing has to be *taken*: the snapshot is the
right tool at both places and `take_place` stays what it is, the move-out half of
a desugar that had already split its statement in three. So the twenty lines
became one `append_snapshot` that both sites call, each supplying only how to
find its container and how to drop its reference — the field site lost its
inline copy in the same commit, which is the diff being *shorter* than the
duplication it avoided.

**The one thing the element form has that the field form does not is a second
read of the index.** The receiver's index is the parser's clone of the
statement's own, so the general path reads it twice: `rows[f()].push(g())` calls
`f` twice and `g` once, on all three engines, before and after. Finding 4 hoisted
exactly this double read out of `rows[f()][j] = v` because soundness needed the
hoist anyway; here nothing needs it, and doing it would move IR in both backends
at the end of the milestone. So the fast path fires only when re-reading the
index is unobservable — a variable or a literal, which is what a loop writes —
and it re-reads it rather than assuming the two expressions are the same one.
Any other index keeps the copy and stays quadratic, exactly as a global does.

Locals only and the validation rule are both reused, not re-derived, and both
were checked rather than inherited on faith. `type Age = Int64 where value >= 18`
with `rows: Array<Array<Age>>` and a runtime `rows[0].push(rt(6))` traps with the
same bytes on all three engines before and after — the fast path coerces the
ITEM into `Age`, where the general path coerced the whole grown row into
`Array<Age>` and re-proved every element already proven. `a[0].push(a[0][1] +
10)` still reads the row it is growing (`12`), and `b[0].push(grow(b))` with
`grow` pushing into that same row still loses the callee's write — the snapshot's
whole point, identical on three engines either side of the change.

Pinned by `the_interpreter_does_not_copy_the_row_once_per_append`, the third of
`places.rs`'s ratios and deliberately the second's shape: N appends onto a local
against N appends into `rows[0]`, within 4x. On the pre-fix binary it is 438x
(9.08 s against 20.8 ms) and the test fails; after, 1.1x.

That is every receiver form fast, and the M2 findings list is closed.

### M3 — `Map` over `Array` — **withdrawn**

Three rows. `Map<String, V>` is `String`-keyed, and `std/hash` already exists, so
this needs nothing new — except a number it cannot get. Withdrawn on M2's gate:
18x on the interpreter for a colder access pattern than a hash map's, and M2's
fifth finding makes a large live table the worst case rather than the ordinary
one.

### M4 — `Array`'s derived operations — **withdrawn**

`@list`, `@toArray`, `@pop`, `@swapRemove` — four rows that are operations *on*
the primitive rather than the primitive. Withdrawn on the same gate, and more
plainly: these are single instructions whose entire value is being single
instructions.

## What stays primitive, and why that is the honest end state

`array`, `push`, `at`, `alen`, `afree` — the growable buffer itself. Five rows,
not sixteen. You cannot write the thing that allocates memory in terms of itself,
which is the same argument the `Syscall` category makes and is not a deferral.

If every milestone lands, "the runtime is Vyrn" holds for every container a
program touches, and the irreducible core is a buffer and a syscall table.

**M2 says that end state is not reached, and the honest count is sixteen minus
one.** M1 landed and is worth having on its own terms; every container row after
it stayed Rust. The claim this RFC actually established is the *negative* one it
set out to establish — containers do not need a raw-memory view, and
`examples/slottable.vyrn` proves it — and that claim is unaffected by the gate.
What the gate added is the second half: they do not need one, *and they should
not be written that way anyway*, because the interpreter charges ~18x for the
privilege and the language cannot give a `std/` table the ambient, type-erased,
generation-checked slab that `Ref<T>` actually is.

## What this does not decide

**Whether a raw-memory view should ever exist** for its own sake — FFI struct
layouts, SIMD alignment, zero-copy over foreign buffers. Those are real and this
RFC does not serve them. It only withdraws the claim that *containers* need one.

### The capability boundary, stated so it is not read as broader than it is

This RFC's title and headline invite the reading "Vyrn does not need `unsafe`".
What it establishes is narrower, and the difference matters because the premise
it replaced survived four RFCs unexamined. Checked against the code:

**Covered — every use of `unsafe` that exists to *build a safe abstraction*:**

| | mechanism |
|---|---|
| containers over uninitialized capacity | `Array` owns `len`/`cap`; spare is unreadable |
| aliasing the ownership analysis rejects — graphs, doubly-linked lists, intrusive structures, self-reference | the generation-checked slot table; `Type::Ref` is also what breaks the size cycle for `Node = { next: Option<Ref<Node>> }` |
| FFI in both directions | `extern` (RFC-0012) |
| shared mutable state across threads | designed out — RFC-0025's isolation analysis is "the whole safety story": no module state, no I/O, no `drop` of shared cells, so there is nothing to race and no atomics to need |
| arenas | `region { }` (RFC-0004) |
| float ↔ bits, String ↔ bytes | `floatBits`/`floatFromBits`, `bytes`/`stringFromBytes` — four specific views, verified to be the only ones |

**Not covered — every use of `unsafe` that exists to *reach past* the
abstraction.** There is no `transmute` and no unchecked indexing anywhere in the
front end; both were grepped for, not assumed:

general reinterpretation · custom allocators beyond `region` (pool, slab, bump
with its own policy) · SIMD intrinsics · inline assembly · atomics and lock-free
structures · `get_unchecked` in a hot loop · memory-mapped I/O, device
registers, DMA · zero-copy over a buffer Vyrn does not own.

The pattern is consistent enough to be a design statement rather than a list of
gaps, so it is worth writing as one: **Vyrn serves the abstraction-building uses
of `unsafe` and none of the abstraction-escaping ones.** That is coherent for
the language's stated direction, and it also means Vyrn is not today a kernel,
embedded, HPC or codec language, and will not become one without the view this
RFC declined to add *for containers*.

**`Array` itself in Vyrn.** That needs the raw view, and the interpreter cost
measured in RFC-0081 suggests it would be expensive. Not proposed.
