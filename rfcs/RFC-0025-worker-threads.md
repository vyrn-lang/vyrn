# RFC-0025 — Worker Threads: Parallel `spawn` and a Concurrent `serve`

- **Status:** Draft — approved for implementation
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
