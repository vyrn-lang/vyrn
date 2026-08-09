# RFC-0095 — A Task Is Owned

- **Status:** **M1 and M3 built** (see "As landed" at the end; M2 was priced and
  refused). RFC-0087 §10 was the last census row with a measurement behind it and
  no mechanism pointed at it, and it is closed. M3 made the must-use scan
  arm-granular and gave `for x in consume xs` the release RFC-0092 M5 left open.
- **Depends on:** RFC-0086 M1 (`impl Owned`), RFC-0092 M4 (a container carries
  its element's obligation), RFC-0093 M1 (`consume` as a prefix), RFC-0025
  (`spawn`), RFC-0004 Q4 (`Task<T>`).
- **Principle:** RFC-0089 rule 1 — a value that owns heap moves. A task owns a
  frame, a record and an operating-system handle, and has never said so.

---

## The measurement

A loop of `let t = spawn work(i)` then `t.join()`, native, peak working set:

| spawns | peak |
|---|---|
| 1,000 | 4,392 KiB |
| 50,000 | 8,332 KiB |
| 200,000 | 20,120 KiB |

**81 bytes per spawn, linear** — and the Windows handle count rises by one per
spawn as well: 19,946 handles at 20,000 spawns. The task record holds an event
object created per task and closed never.

The frame is about 32 of those 81 bytes. **A handle is the part that matters.**
Bytes are a leak; handles are a resource with a per-process ceiling, and a
long-running server that spawns per request meets that ceiling.

## Why it is still open

The shim says so itself, and reading it is the fastest route to the design:

> Ownership: task records and frames are never freed — a task may be joined more
> than once (join is idempotent) … Read that as a decision, not an omission.

`__vyrn_join` hands the **frame pointer** back and the caller loads the result
off it. So a free at the first join gives a second join a dangling read, and
`t.join()` twice is a legal program on all three engines today.

Two shapes were considered by the census and neither closes the row. Copying the
result into the record at completion frees the frame and keeps the record, so
the growth stays linear and only the constant moves. Freeing at the *last* join
needs to know no further join can happen — **which is ownership of the `Task<T>`
value**, and that is this RFC.

---

## The rule

> **`Task<T>` is linear. `join` consumes it and yields the result. `drop`
> discharges it without taking the result. A task that is never discharged is
> refused.**

Three consequences.

**`t.join()` consumes `t`.** "Free at the last join" becomes "free at *the*
join", because there is only one. A second `t.join()` is refused by rule 1, at
compile time, with the message rule 1 already writes for a use after a move.
This is a **breaking change** and the corpus does not use it — no file joins one
task twice.

**`drop t` waits, then releases.** A task the program abandons must still be
waited for, and this is not an optimisation detail: the shim joins unjoined
tasks at process exit *so that a trap inside one is not lost*. A `drop` that
freed the frame while the worker still wrote to it would corrupt the heap, and
one that skipped the wait would swallow a trap. So `drop t` is: wait for
completion, release the result **by its type**, then free the frame, the record
and the handle.

Releasing the result by its type is the half that is easy to miss. A
`Task<String>` the program drops has a `String` in its frame that the worker
allocated, and nothing else will ever free it.

**An undischarged task is refused**, exactly as an undischarged `Stream<T>` is.
That is the mechanism RFC-0086 M1 built and RFC-0092 M4 taught to recurse
through containers, so `Array<Task<Int64>>` carries the obligation for free.

---

## The corpus has exactly one hard case, and it is already written

`examples/branchtypes.vyrn` spawns in one `match` and joins in **one arm** of
another:

```vyrn
let task: Task<Int64> = match pick {
    Some(n) => spawn addOne(n),
    None => spawn addOne(0),
}
let joined = match pick {
    Some(n) => task.join(),
    None => 0 - 1,
}
```

On the `None` path the task is never joined. **Today that path leaks a thread
handle silently.** Under this rule it is refused until the arm discharges the
task, and the fix is one `drop task`. The example exists, it predates this RFC,
and it is the shape that proves the obligation is worth having rather than a
tax.

Everything else in the corpus is `let a = spawn f(x)` followed by `a.join()`,
which is already correct and does not change.

---

## The three engines

**Native** is where the leak is and where the work is: free the frame, free the
record, `CloseHandle` the event (and the pthread equivalent), after the wait.

**Wasm** has no threads. `__vyrn_spawn` runs the thunk inline and `VTask` holds
only a frame pointer, so the release is two frees and the wait is nothing.

**The interpreter** runs tasks eagerly and holds Rust values, so it cannot leak
and the release is observationally empty.

**Parity is the gate and the expected result is byte-identical output**, on the
same argument RFC-0093 M1 used: a task is isolated by the checker — no I/O, no
module state, no shared cells — so *when* its storage goes back is not
observable. What must not change is the trap protocol: a trapping task prints
one canonical line and exits 1, from whichever thread it runs on, and a dropped
task must still be able to do that.

---

## Milestones

### M1 — the obligation — **BUILT**

`Task<T>` becomes must-use. `join` consumes. `drop t` is legal on a task and
releases it. `examples/branchtypes.vyrn` gains its `drop task`.

**Gate.** Three-way parity byte-identical including traps. The memory suite at
15 rows, 15 steady. A new row: **`spawnFrame` flips from "native-only, invisible
here" to a measured native steady state** — the row exists today and records that
this harness cannot see §10, so M1 must give it something that can. 200,000
spawns must not grow the working set linearly, and the handle count must be
flat.

**A trapping task that is dropped rather than joined must still print its line
and exit 1.** That is a test, not a hope.

### M2 — the handle, if M1 leaves it — **NOT NEEDED**

Only if M1 finds the handle cannot be closed at the same site. Splitting it is
not expected; the milestone exists so that discovering it does not become a
reason to widen M1. **M1 closed the handle at the same site**, so this milestone
is closed unbuilt.

---

## Rejected

- **A flag and a free at the first join.** The census measured why: `__vyrn_join`
  returns the frame pointer, so the second join reads freed memory. A
  use-after-free on a joined task is a worse answer than a bounded leak, which is
  why the leak was deliberate.
- **Copying the result into the record at completion.** It frees 32 of 81 bytes,
  needs the result's size at the shim's ABI, keeps the growth linear, and leaves
  the reader holding two lifetimes.
- **A task pool that reuses records.** It bounds the count without saying who
  owns a task, and this repo has spent an arc replacing exactly that shape — a
  fixed table with a generation counter is what RFC-0090 deleted.
- **Making `join` idempotent by copying the result out.** Two owners of one
  `String` result, which rule 1 exists to prevent.
- **Leaving it, and documenting harder.** A per-process handle ceiling is not a
  documentation problem.

---

## M1 as landed

**Built. §10 is closed.** The same loop, native, `let t = spawn work(i)` then
`t.join()`:

| spawns | peak, before | handles, before | peak, after | handles, after |
|---|---|---|---|---|
| 1,000 | 4,248 KiB | 853 | 3,860 KiB | 71 |
| 20,000 | 5,924 KiB | 20,076 | 4,332 KiB | 72 |
| 50,000 | 8,312 KiB | 50,076 | 3,880 KiB | 72 |
| 200,000 | 20,252 KiB | 200,076 | 3,880 KiB | 72 |

The before column reproduces the RFC's own numbers (4,392 / 8,332 / 20,120 KiB,
19,946 handles at 20,000) to within measurement noise, and the handle column
reproduces exactly: one per spawn. After M1 both are flat. The same holds under
`VYRN_SEQUENTIAL_SPAWN=1`: 3,872 KiB and 70 handles at 200,000 spawns.

### The shape it took

`Task<T>` joins `Stream<T>` on the must-use row and stays off the automatic
reclamation table, for the reason `Stream` is off it: the release is emitted by
the construct that DISCHARGES the value, so a block-exit row would release it a
second time. `own::release_kind` answers `None` for a `Task`, `own::linear_kind`
answers `Linear::Task`, and `own::owns_heap` answers `true` for every `Task<T>`
including `Task<Unit>` — the handle is there whatever `T` is.

The shim gained one function. `__vyrn_task_release` waits, unlinks the record from
the exit-time registry, closes the event handle (or destroys the mutex and
condition variable), and frees the frame and the record. The two discharges call
it:

- **`t.join()`** loads the result out of the frame and then releases the task. The
  order is the safety argument: `__vyrn_join` answers with the frame's ADDRESS.
- **`drop t`** joins (which waits), releases the result **by its type** through the
  ordinary `emit_drop` — the frame pointer IS a slot holding the result, so no
  second walk was needed — and then releases the task.

The registry is now doubly linked, so a record can leave it, with a `listed` flag
that makes the unlink idempotent. `__vyrn_join_all` DETACHES each record before
waiting on it, so a release running on another thread cannot free the pointer the
exit walk is holding. The registry is empty in every program the checker accepts;
it is kept as the net under a hole in that proof, because what it protects is a
trap that would otherwise be lost.

wasm has no threads, so the whole task is one heap box: the join frees it after
reading, and `drop` releases the result by its type and then frees it. The
interpreter runs tasks eagerly over Rust values, and its `drop` already released
the value by its type, so it needed no change at all.

### Three things this RFC had wrong

**The corpus DID join one task twice.** `examples/parallel.vyrn` did it
deliberately, with the comment "join is idempotent — a second join re-reads the
settled result". It reads each result once now, into a `let`, and prints the same
four numbers.

**`examples/branchtypes.vyrn` could not be given a `drop`.** Its leaking path is a
`match` ARM, `drop` is a statement, and an arm is an expression — so `drop task`
is unwritable there. Both arms join instead, which discharges the task on every
path and prints the same line.

**The obligation does not catch the branchtypes shape anyway.** The must-use scan
is statement-granular: a mention anywhere in a statement is a disposal on every
path through it, so a join inside one arm of a `match` expression reads as a
disposal on both. That is a limit `Stream` has had since RFC-0075 and this
milestone did not widen. What the obligation does catch is the plain shape —
spawned and never mentioned, joined twice, or joined on one branch of an `if`
STATEMENT — which is `examples/task_abandoned.vyrn`, three refusals in one file.
**M3 widened it** — see "As landed — M3" below — so the sentence above is now
history: an arm is a path, and `task_abandoned.vyrn` has a fourth refusal.

### The known limit it leaves

An `Array<Task<T>>` carries the obligation (RFC-0092 M4), but `drop ts` on one
frees the buffer and leaks the tasks in it: a task's release is a wait and three
frees at a site holding the frame pointer, not a walk over bytes, and the deep
release walk has no arm for it. The diagnostic therefore names the discharge that
works — `for t in consume ts`, joining each element — rather than the `drop` menu
a container usually gets. `Array<Stream<T>>` has had the identical hole since
RFC-0075, so this is one shape of one open question and not a new one.

**M2 is not needed.** The handle closes at the same site as the frame and the
record, which is what the milestone expected.

### What proves it

- Three-way parity byte-identical including traps, 36 tests, wasm column live.
- `a_dropped_task_that_traps_still_prints_once_and_exits_1` — a DROPPED trapping
  task prints the canonical line once and exits 1, on all three engines, and the
  line printed before the drop survives. This is the risk the RFC named, as a
  test.
- The memory suite reads 15 rows, all steady. `spawnFrame` measures the release
  rather than an absence now: it is a `Task<String>` that is dropped, plus 64
  joined tasks a call, and removing either release makes the row grow (589,824
  bytes at 500 calls against 2,162,688 at 2,000 — verified by removing each).
- `the_spawn_handles_go_back_natively`, beside the table, is the native
  measurement the wasm harness cannot make: the handle count after 8,000 spawns
  equals the count after 2,000. Removing the `CloseHandle` makes it read 2,088
  against 8,088 — verified.
- `examples/task_abandoned.vyrn` is an expected-check-failure corpus file.
- `borrow_store_sites` reads 0 across the corpus; the projection census reads
  `stores: 0`, `returns: 0`, `elem-store: 0`, `elem-return: 0`.

## As landed — M3

M2 was priced and refused above, so this is the next number. It is the limit M1
recorded and did not widen, plus the leaking row beside it in RFC-0092 M5.

**An arm is a path.** The scan asked its expressions one question — does this
name the binding — and read the answer as a disposal on every path. It asks two
now: does SOME path name it, and does EVERY path. The two differ at exactly the
expressions that can skip a sub-expression — a `match`, and an `if` used as an
expression — and everywhere else they are the same answer, because everything
else evaluates all of its parts. Where they disagree the acquisition is reported,
which is the merge rule the `if` STATEMENT has used since RFC-0075. So this is
one walk learning what the other already knew, not a second mechanism: `mentions`
is the first component of the new one, and the arms merge with `all` where the
statement form merges two blocks. The `return` arm reads the second component
too, so `return match p { Some(n) => t, None => 0 }` is refused for the same
reason the `let` form is. `Stream` and every `impl MustUse` row are covered at
once, because the three share the walk.

**The corpus cost was one test fixture and no program.** `vyrn check` over all of
`std/` and `examples/` — 216 files — refuses exactly the 13 files
`EXPECTED_CHECK_FAILURE` already lists, and `examples/branchtypes.vyrn`, which
joins in both arms, still compiles. One site was newly refused, and it is not a
Vyrn program: the `a join` case of `direct.rs`'s branch-lowering table was
`return match o { Some(n) => t.join(), None => 0 }` — the abandoned shape, in a
fixture that exists to check a lowering. It joins in both arms now and lowers the
same join.

**A limit this milestone does NOT close.** `let t2 = match c { A => t, B => u }`
hands the obligation on and `t2` inherits nothing: `owed_let` reads a `let` whose
value is a `Var`, a `Call` or a `spawn`, and a branching expression is none of
the three. Both arms dispose, so the program is accepted and `t2` is a task
nothing answers for. It is the same shape one level up, and closing it needs a
type this walk does not have.

**Closed, and the spelling above was wrong.** `owed_let` reads a `match` and an
if-expression now: the binding inherits the obligation ANY arm carries, which is
the union, and the first arm that carries one answers for the rendering because
the checker has already made the arms agree. Closing it needed no type. What the
paragraph above got wrong is the example: `let t2 = match c { A => t, B => u }`
over two live bindings is **already refused**, at `t`, which one arm hands on and
the other does not — M3's own arm-granular scan. The shape that reaches the hole
ACQUIRES in each arm, where no earlier binding is there to answer:
`let t = match o { Some(k) => feed(), None => feed() }` was accepted and left a
stream nobody answered for. It is refused now, and
`a_branch_acquires_into_the_binding` is the test. The corpus cost is zero: `vyrn
check` over all 216 files refuses exactly the files `EXPECTED_CHECK_FAILURE`
already lists.

**And `join` got its seeded row** (RFC-0094's residue, closed with this change).
M1 made `t.join()` consume the task, and that fact lived as a property of the
must-use walk rather than as a line: every mention of a linear binding is a
disposal there. `@join` declares `consume Task<T> -> T` in `prelude.rs` now, and
`movecheck::sinks` reads it. There was no hand-written special case to delete,
and rule 1 stands aside as it does for `close` — the obligation on the linear
TYPE refuses a second join first, with the better words.

**And a leaking row nobody had filed.** `for x in consume xs` took the buffer and
nothing released it — RFC-0092 M5's row with one keyword written on it, which
that milestone recorded as open because the census does not name it. The loop
gets M5's row now, and the early-exit argument is written where the gap was, in
RFC-0092 M5's "What M5 does not close": a row that survives to the exit is a body
that moved nothing out, so `break`, a `return` out of the body and the
fall-through each give back the visited and the unvisited elements alike, exactly
once. No index is counted, and a double free is not expressible on any path. A
body that keeps one element, or drops one by hand, leaks the buffer whole — the
allowed direction.

### What proves M3

- The match-arm task repro is refused, as `examples/task_abandoned.vyrn`'s fourth
  function; `examples/stream_abandoned.vyrn` carries the stream half.
- **The memory suite reads 16 rows, 16 steady.** The new row, `consumingLoop`, is
  negative-tested: with the release not minted it reads 1,114,112 bytes at 500
  calls against 4,259,840 at 2,000.
- Three-way parity byte-identical including traps, 36 tests, wasm column live.
  `genwasm`: 11 passed. Workspace green with `--no-fail-fast`, plus 75 in
  `vyrn-lsp`.
- `examples/consumeloop.vyrn` gained the two early exits — a `break` and a
  `return` out of a consuming loop — and prints the same bytes on all three
  engines.
- `borrow_store_sites` reads 0 across 216 files.

---

## The recommendation

**Build M1.**

1. **It is the last census row with a measurement and no mechanism**, and the
   mechanism it needs already exists and is already pointed at two other things.
2. **The handle is a resource ceiling, not a byte count.** 81 bytes per spawn is
   a leak somebody can live with; one operating-system handle per spawn is a
   server that stops.
3. **The corpus cost is one `drop`,** in an example that is leaking a handle on
   that path today.
4. **The gate can refuse it.** If `drop t` cannot wait without changing the trap
   protocol, the answer is to say so and keep the documented leak, because the
   failure mode of guessing here is a use-after-free on a joined task.
