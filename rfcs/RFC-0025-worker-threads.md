# RFC-0025 — Worker Threads: Parallel `spawn` and a Concurrent `serve`

- **Status:** Implemented (see "As landed" at the end for the resolved details)
- **Depends on:** RFC-0004 (the isolation analysis — the whole safety story),
  RFC-0016 (`serve` and its async decision, which named this follow-up)
- **The promise being kept:** "when concurrency comes to the server it will
  be worker threads calling `handle` in parallel, gated on the isolation
  analysis — same output, better wall-clock, **no new language surface**."

---

## Why this is safe by construction

A `spawn`ed task is **isolated** (checker-enforced, transitively): no
effects, no module state, no I/O, no `drop` of shared cells. An isolated
computation's only observable is its return value — so ANY schedule
produces byte-identical program output. Parallelism here is pure wall-clock
optimization, invisible to the parity invariant. Nothing about `spawn`'s
semantics, syntax, or the checker changes.

## Native: `spawn` on real threads

- Codegen's `spawn` lowers to `__vyrn_spawn` in the C shim (Win32 threads /
  pthreads behind one 64-bit-clean wrapper): arguments are packed into a
  heap frame, the task body (already a synthesized function) runs on the
  thread, the result lands in the task handle's slot; `t.join()` blocks on
  completion and reads it. Traps inside a task: the canonical trap must
  still print once and exit(1) — lock: a trapping task performs the
  standard trap protocol itself (stderr + exit), which is what the eager
  semantics already produce; the wording/exit are unchanged.
- **Interpreter and wasm stay sequential/eager** — semantically identical
  (isolation guarantees it), zero risk, no wasm-threads adventure. The
  parity corpus needs no exclusions; existing spawn examples simply run.
- A `--sequential-spawn` env (`VYRN_SEQUENTIAL_SPAWN=1`) forces the old
  eager path natively — a debugging escape hatch, documented.

## Server: `vyrn serve --workers N` (and `vyrn dev`)

- **Gate:** if `handle` (transitively) touches module state, workers are
  refused with a clear startup message naming the offending path — the
  sequential loop remains the only sound mode (this is the isolation
  analysis doing its job; the existing purity machinery answers the
  question, no new analysis).
- **Mechanism:** a pure `handle` means each worker thread owns a fully
  independent interpreter instance (the `Interp` is not `Sync` and does not
  need to be — nothing is shared). The accept loop dispatches connections
  to a fixed pool of N workers over an spmc channel (std only).
- **Honesty about ordering:** access-log lines may interleave across
  workers (each line stays atomic). The serve host is CLI runtime, not
  language semantics — the parity corpus is untouched. `main` still runs
  once, before any worker starts.
- Default remains sequential (`--workers` absent = today's behavior);
  `vyrn dev` passes the flag through.

## Verification

- Paired tests: a spawn-heavy program's output byte-identical between the
  threaded native build and the interpreter (the corpus already proves
  this shape; add one deliberately thread-racy-looking-but-isolated
  example, e.g. N parallel fib tasks joined in order).
- A wall-clock smoke (not a test assertion — timing is not parity):
  demonstrate speedup in the report, N tasks × heavy compute.
- serve integration: `--workers 4` + concurrent requests (std TcpStream
  from threads) → all correct responses; the module-state gate refuses
  with the named path; sequential default unchanged.

## Out of scope

Wasm threads, work-stealing/schedulers, task priorities, `spawn` semantic
changes (still eager-equivalent), async I/O overlap (the RFC-0016 revisit
triggers stand), shared-memory anything.

## As landed

The details the implementation resolved, recorded here so the RFC matches
what ships:

- **One shared IR; the threading lives entirely in the C shim.** `spawn
  f(args)` evaluates + coerces its arguments at the spawn site (the eager
  interpreter's order — argument effects belong to the caller), packs them
  into a malloc'd frame behind a leading result slot, and emits
  `call ptr @__vyrn_spawn(ptr @__vyrn_task_<callee-sym>, ptr %frame)`. The
  per-callee thunk (`define void @__vyrn_task_<sym>(ptr %frame)`) loads the
  arguments back, calls the callee directly, and stores the result into the
  frame; spawn sites of the same callee share one thunk. `t.join()` is
  `call ptr @__vyrn_join(ptr %t)` + a load from the returned frame. The shim
  implements `__vyrn_spawn` three ways behind `#ifdef`: Win32 threads /
  pthreads (detached, completion via a per-task manual-reset event / condvar
  — safe for any number of joiners), inline on `__wasi__` (no threads), and
  inline under `VYRN_SEQUENTIAL_SPAWN=1` natively (the documented escape
  hatch). Byte-identical in all modes, by isolation.
- **The thunk vs. the no-function-pointer invariant (RFC-0023).** The thunk
  symbol handed to `__vyrn_spawn` is a function pointer at the C boundary
  ONLY. No Vyrn-level function value exists; every emitted `call` still names
  an `@symbol` (the RFC-0023 IR test's assertion is about call instructions
  and holds unchanged — a new test re-runs the same scan over a spawn-heavy
  module), and Vyrn code adds no indirect-call table entry to the wasm module.
- **`Task<T>` is now a real handle** (`ptr` at runtime, previously the eager
  result value itself). `join` is idempotent — task records and frames are
  never freed (a task may be joined again; the count is bounded by the number
  of spawns — the "unproven ownership leaks, which is always safe" rule).
- **A trapping task** performs the standard trap protocol itself — the one
  canonical `error: ...` line to stderr, then `exit(1)` — from whichever
  thread it runs on: same wording, same exit code, printed once (stdout is
  flushed by `exit`, and each trap message is a single `fputs`). Tasks never
  joined are joined at process exit, in spawn order, so a leaked task's work
  (and trap) is never lost — eager semantics ran every task. Interleaving of
  a leaked trapping task's exit with stdout the program printed AFTER the
  spawn is the one schedule-dependent corner; no corpus example has that
  shape, and joining the task pins it.
- **The region arena stack became `thread_local`** — `region { .. }` is
  memory management, not an effect, so isolated tasks may use it; per-thread
  stacks keep it race-free. Single-threaded targets (wasm32-wasip1) lower TLS
  to plain globals. Every other mutable runtime global (the `Ref` cell slab,
  the log stream, module state) is unreachable from an isolated task by the
  checker's own rules.
- **The serve gate is module state, exactly.** `checker::module_state_use`
  (BFS over `fn_calls` + `touches_globals` with the protocol-impl expansion
  the spawn fixpoint uses) reports the shortest call chain and the touched
  global; the refusal prints it verbatim: ``error: `--workers` needs a
  module-state-free `handle`: `handle` -> `bump` reads or writes module state
  `hits` (shared by definition) — run without `--workers` for the sequential
  loop``. `print`, logging, and file I/O deliberately do NOT gate workers —
  thread-compatible host effects, each output line atomic.
- **Per-worker interpreters and globals.** `interp::serve_pool` runs module
  state init + `main` ONCE on a setup interpreter (its output appears once),
  then each worker builds a fully independent `Interp` and re-runs
  `init_globals` on its own copy — an interpreter needs a well-formed global
  frame to exist, and the gated `handle` can never observe the copies.
  Connections dispatch over an spmc channel (std only); access-log lines may
  interleave across workers but each line stays atomic. `--workers` absent is
  today's sequential loop byte-for-byte; `vyrn dev` passes the flag through.
- **Wall-clock evidence** (not a test): 8 × `fib(36)` tasks, 12-logical-core
  Windows machine — threaded ≈ 0.08 s vs `VYRN_SEQUENTIAL_SPAWN=1` ≈ 0.37 s
  (~4.6×), identical stdout.
