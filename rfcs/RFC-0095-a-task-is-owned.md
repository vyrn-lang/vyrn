# RFC-0095 — A Task Is Owned

- **Status:** **Designed. Not built.** RFC-0087 §10 is the last census row with a
  measurement behind it and no mechanism pointed at it.
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

### M1 — the obligation

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

### M2 — the handle, if M1 leaves it

Only if M1 finds the handle cannot be closed at the same site. Splitting it is
not expected; the milestone exists so that discovering it does not become a
reason to widen M1.

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
