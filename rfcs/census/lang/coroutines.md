# Census — Coroutines

A survey of how nine systems model concurrent execution that can suspend and resume, what each one costs, and what those costs say about Vyrn.

## The two families

Every coroutine design is either stackful or stackless.

A stackful coroutine has its own call stack. Suspending saves the whole stack; resuming restores it. The function body does not know it is running on a coroutine — any function can suspend, and any function can call a function that suspends. The cost is a stack per coroutine: memory, a switch, and a growth strategy.

A stackless coroutine has no separate stack. The compiler transforms the function body into a state machine. Suspending saves the program counter and the live locals; resuming jumps to the saved point and reloads the locals. The cost is that the function is no longer an ordinary function — it returns a state machine object, and callers must treat it differently. This is the "function colouring" problem.

## The nine systems

### Go goroutines

Go goroutines are stackful. Each goroutine starts with a 2 KB stack that grows by allocating a larger stack and copying the live frames over. The old stack is freed. Shrinking is also possible. A guard page below the stack catches overflow.

The scheduler is the Go runtime's GMP model: goroutines (G) are scheduled onto logical processors (P) that each have a local run queue, and OS threads (M) execute them. Work-stealing balances load across processors. The scheduler is preemptive at function call boundaries and at async safe points; since Go 1.14, it can also preempt at the instruction level via asynchronous preemption signals.

No function is coloured. Any function can be called from a goroutine, and any function can call `runtime.Gosched` to yield. A call site that does not spawn pays nothing — `go f()` is a statement, not a type.

Cancellation is cooperative, via channels and `context.Context`. There is no language-level cancellation; a goroutine that ignores its context runs until it returns or the program exits.

### Rust async/await

Rust async/await is stackless. An `async fn` is compiled to a state machine struct implementing `Future`. Each `.await` point is a suspension: the state machine saves which branch it was on and yields `Poll::Pending`. The struct holds all live locals across the await, so its size is the sum of the cross-await live set.

Rust has no built-in scheduler. A runtime (tokio, async-std, smol) provides the executor that polls futures. The language provides the `Future` trait and the `async`/`await` syntax; the runtime provides the reactor, the thread pool, and the timer.

`async fn` colours the function. It returns `impl Future<Output = T>`, not `T`. A synchronous function cannot call an async function and get `T` back without an executor. A call site that does not use async pays nothing — an ordinary `fn` is unchanged.

Cancellation is by dropping the future. When the runtime stops polling a future and drops it, the state machine's destructor runs, releasing any held resources. This is implicit: there is no `cancel` keyword. A future that is never polled again is cancelled. The cost is that every `async fn` must be safe to drop at any suspension point, which constrains how it can hold locks and buffers.

### The `may` crate (Rust stackful coroutines)

The `may` crate (https://github.com/Xudong-Huang/may) provides stackful coroutines for Rust, modelled on Go goroutines. The `go!()` macro spawns a closure on a coroutine. The implementation is built on the `generator` crate (https://github.com/Xudong-Huang/generator-rs), which creates a stack and switches to it with assembly-level `swap` routines.

Each coroutine gets a fixed-size stack with a guard page. The default is 64 KB, configurable per coroutine. The stack does not grow — overflow triggers a segmentation fault. The README documents this as the main restriction: the stack must be large enough for the application.

The scheduler runs a configurable number of OS worker threads, each with a local run queue and work-stealing. Coroutines are cooperative: they yield at I/O points (the crate hooks epoll/kqueue/IOCP) and at explicit `yield_now()` calls. There is no preemption.

No function is coloured. A closure passed to `go!()` can call any function, and any function can call `yield_now()`. The caveat documented in the README is that thread-local storage is unsafe across a yield, because the coroutine may resume on a different OS thread. The crate provides coroutine-local storage (CLS) as a replacement.

Cancellation is supported via a `Canceler` handle. The crate also supports scoped coroutine creation, panic isolation (a panicking coroutine does not crash other coroutines), and selection over multiple coroutine operations.

A call site that does not spawn pays nothing. Ordinary Rust functions are unchanged.

### Kotlin coroutines

Kotlin coroutines are stackless. A `suspend` function is compiled to a continuation-passing-style function: it receives a `Continuation<T>` parameter, and each suspension point becomes a state in a state machine that the continuation holds. The continuation object is allocated on the heap and holds the live locals across suspensions.

Kotlin's `suspend` keyword colours the function. A `suspend` function can only be called from another `suspend` function or from a coroutine builder (`launch`, `async`, `runBlocking`). A non-suspend function cannot call a `suspend` function directly. This is the same colouring as Rust's `async fn`.

The scheduler is provided by a `CoroutineDispatcher` (Dispatchers.Default, Dispatchers.IO, etc.). The language does not mandate a scheduler; `runBlocking` runs on the current thread. Structured concurrency is enforced through `CoroutineScope`: every coroutine has a parent, and cancelling the parent cancels all children.

Cancellation is cooperative via `CoroutineContext` and `CancellationException`. A coroutine that does not check for cancellation runs until it finishes or suspends again. The `ensureActive()` check and the suspension machinery cooperate to deliver cancellation at suspension points.

A call site that does not use coroutines pays nothing. Ordinary functions are unchanged.

### Lua coroutines

Lua coroutines are stackful. `coroutine.create(f)` allocates a full stack for `f`. `coroutine.resume(co, ...)` runs it until it calls `coroutine.yield(...)`. The stack persists across yields; resuming continues from the yield point.

There is no scheduler. Coroutines are cooperative and manual: the caller decides when to resume. There is no preemption. A coroutine that never yields runs until it returns.

No function is coloured. Any Lua function can call `coroutine.yield`, and any function can be the body of a coroutine. The yield propagates up the call stack through ordinary calls, because the whole stack is saved.

There is no cancellation mechanism. A coroutine is either alive, suspended, or dead. The caller can stop resuming it, but there is no way to force it to stop from outside. A dead coroutine's resources are reclaimed by the garbage collector.

A call site that does not use coroutines pays nothing. Ordinary functions are unchanged.

### Zig async/await (removed)

Zig had stackless async/await from version 0.5 through 0.10. An `async fn` was compiled to a state machine, and `await` suspended. The implementation used a function frame allocated on the heap or the stack, holding the live locals.

Zig removed async/await in 0.11 (2023). The reasons, as stated by the Zig project, were:

1. Function colouring. An `async fn` could only be called from an `async` context. This split the standard library into async and sync variants and made every I/O decision a colour decision. The language could not offer one I/O API that worked in both contexts.

2. The colouring infected the type system. An `async fn` returned a different type than its body declared. Callers had to know. Generics that worked over both sync and async functions were complex.

3. The implementation complexity was high. The state machine transformation, the event loop integration, and the interaction with Zig's manual memory management created a surface that the project judged was not worth the benefit for a systems language where OS threads are cheap and explicit.

After removal, Zig uses OS threads and explicit synchronization. A function that would have been `async` is a regular function that blocks, and concurrency is achieved by spawning threads. The language has no coroutine concept.

A call site that does not use async paid nothing even before removal. The cost was in the library, not the call site.

### Java virtual threads

Java virtual threads (JEP 444, JDK 21, September 2023) are stackful. A virtual thread is a lightweight thread managed by the JVM, not the OS. It runs on a carrier thread (a platform OS thread) and is unmounted when it blocks on I/O or a synchronization operation. The JVM stores the virtual thread's stack on the heap while it is unmounted.

The stack is a continuation: the JVM saves the call stack as a `Continuation` object and restores it when the virtual thread is rescheduled. The stack can grow because it is heap-allocated and not fixed in size. When mounted, the virtual thread runs on its carrier thread's stack; the JVM copies the continuation back.

No function is coloured. Any method can run in a virtual thread, and any method can call a blocking operation that triggers unmounting. `Thread.ofVirtual().start(runnable)` creates one; `Executors.newVirtualThreadPerTaskExecutor()` creates a pool.

The scheduler is the JVM's built-in fork-join pool that manages carrier threads. Virtual threads are preempted at blocking points (I/O, `LockSupport.park`, timed waits). There is no instruction-level preemption — a virtual thread that does CPU work without blocking stays mounted on its carrier.

Cancellation is via `Thread.interrupt()`, the same mechanism as platform threads. A virtual thread that does not check for interruption runs until it finishes.

A call site that does not create a virtual thread pays nothing. Ordinary methods are unchanged.

### C++20 coroutines

C++20 coroutines are stackless. A function with `co_await`, `co_yield`, or `co_return` is a coroutine. The compiler allocates a coroutine frame on the heap (or on the caller's stack if the allocator is customized), holding the live locals and the resume point.

The coroutine returns a type determined by its `promise_type` (e.g. `std::future<T>`, `std::generator<T>`, or a user-defined type). The promise type controls how the coroutine is created, suspended, and resumed. This colours the function: the return type is not `T` but the coroutine wrapper.

C++20 provides no scheduler. The library provides `std::coroutine_handle` as a resume mechanism, and the application or a third-party library (cppcoro, boost.asio) provides the event loop. The coroutine is resumed by calling `handle.resume()`.

There is no built-in cancellation. The promise type can define behaviour for the coroutine being destroyed (`handle.destroy()`), which runs the destructors of live locals. Cancellation is whatever the application builds on top of this.

A call site that does not use coroutines pays nothing. Ordinary functions are unchanged.

### Erlang processes

Erlang processes are stackful. Each process has its own heap and its own stack, allocated by the BEAM VM. A process starts with a small stack and heap (about 2 KB) that grow as needed. There is no shared memory between processes; communication is by message passing.

The scheduler is the BEAM VM's preemptive scheduler. Each process gets a number of "reductions" (about 2000 function calls) before it is preempted. Preemption happens at reduction-count checkpoints, not at instruction level. The scheduler runs on multiple OS threads (one per core by default), and processes migrate between schedulers for load balancing.

No function is coloured. Any Erlang function can run in a process, and any function can call `receive` (which blocks the process until a message arrives). The process abstraction is explicit (`spawn/3`), not a property of the function.

Cancellation is via exit signals. A process can be sent an exit signal (`exit(Pid, Reason)`), and if it is trapping exits, it receives the signal as a message. A linked process that dies sends an exit signal to its links. This is a per-process mechanism, not a language-level keyword.

A call site that does not spawn pays nothing. Ordinary functions are unchanged.

## Summary table

|system|stackful/stackless|stack allocation|stack growth|scheduler|colours functions?|cancellation|call-site cost if unused|
|---|---|---|---|---|---|---|---|
|Go goroutines|stackful|runtime-allocated, 2 KB initial|copy to larger stack|GMP, preemptive|no|channels + context|none|
|Rust async/await|stackless|state machine struct (heap or stack)|fixed (sum of cross-await locals)|external (tokio, etc.)|yes — `async fn` returns `impl Future`|drop the future|none for sync `fn`|
|`may` crate|stackful|fixed, 64 KB default, guard page|no growth — overflow traps|work-stealing on N OS threads|no|`Canceler` handle|none|
|Kotlin coroutines|stackless|continuation object on heap|fixed (sum of cross-suspend locals)|`CoroutineDispatcher`|yes — `suspend` functions|`CancellationException` via scope|none|
|Lua coroutines|stackful|full stack per coroutine|Lua-managed, grows|none — manual resume|no|none|none|
|Zig async (removed)|stackless|function frame|fixed|was event loop|yes — `async fn`|was implicit|none|
|Java virtual threads|stackful|heap-allocated continuation|grows (heap-resident)|JVM fork-join, preemptive at blocks|no|`Thread.interrupt()`|none|
|C++20 coroutines|stackless|coroutine frame on heap|fixed (sum of cross-suspend locals)|none — external|yes — return type is wrapper|destroy handle (run destructors)|none|
|Erlang processes|stackful|per-process heap + stack|grows|BEAM reductions, preemptive|no|exit signals|none|

## What is Vyrn's `spawn` today

Vyrn has `spawn` and `join`. The example at `examples/concurrency.vyrn:35-40` shows the shape: `spawn transform(cfg, 1)` returns a `Task<T>`, and `t.join()` yields the result.

It is not a goroutine, not a green thread, not an async task. It is a **pure, isolated function run as a concurrent task**. The design is in RFC-0004 Q4 (`rfcs/RFC-0004-capabilities-and-memory.md:546-579`) and the implementation is in RFC-0025 (`rfcs/RFC-0025-worker-threads.md`).

The key constraint: the spawned function must be **isolated**. The checker proves this transitively. A monotone fixpoint over the call graph (`compiler/vyrn-frontend/src/checker.rs:575-643`) starts with all non-`extern`, non-`modify`, non-module-state, non-`drop` functions as "spawn-safe" and removes any function that calls an unsafe one, iterating to a fixed point. A function that reads or writes module state is not spawn-safe (`checker.rs:581-583`). A function that calls `print` or a logging sink is not spawn-safe (`checker.rs:9332-9362`, `SPAWN_FORBIDDEN`). The result: a spawned task's only observable is its return value.

Because tasks are pure, the result is schedule-independent. RFC-0004 Q4 says it directly: "Because tasks are pure, the result is the same under any schedule — so the interpreter (which runs them one after another) and native code agree" (`rfcs/RFC-0004-capabilities-and-memory.md:549-552`).

The three backends run the same schedule differently:

- **Native**: `spawn` lowers to `__vyrn_spawn(thunk, frame)` in the C shim (`compiler/vyrn-codegen/src/toolchain.rs:441-656`). The thunk is a per-callee function that loads arguments from a heap frame, calls the task function, and stores the result. On Win32/pthreads, `__vyrn_spawn` creates a real OS thread (detached) that runs the thunk; `__vyrn_join` blocks on a per-task event/condvar and returns the frame. Wall-clock evidence in RFC-0025: 8 x `fib(36)` tasks, 12-core Windows machine, threaded ~0.08 s vs sequential ~0.37 s, ~4.6x, identical stdout (`rfcs/RFC-0025-worker-threads.md:139-141`).
- **Wasm**: `__vyrn_spawn` runs the thunk **inline** and returns a `VTask` holding the frame. There are no threads (`compiler/vyrn-codegen/src/toolchain.rs:496-501`). The direct wasm backend emits the call at the spawn point with no function table, no `call_indirect` — `spawn f(a)` IS `f(a)` at the spawn point (`compiler/vyrn-codegen/src/direct.rs:8613-8645`).
- **Interpreter**: runs tasks eagerly, one after another (`compiler/vyrn-frontend/src/interp.rs:6030-6034`). `Expr::Spawn` evaluates arguments, calls the function, and returns the result in a `Task` wrapper.

`Task<T>` is **linear** (RFC-0095 M1, `rfcs/RFC-0095-a-task-is-owned.md:54-56`). `t.join()` consumes the task and yields `T`. `drop t` waits for completion, releases the result by its type, then frees the frame, the record and the OS handle (`rfcs/RFC-0095-a-task-is-owned.md:66-72`). A task that is never discharged is refused at compile time (`rfcs/RFC-0095-a-task-is-owned.md:78-80`). The `@join` builtin declares `consume Task<T> -> T` in the prelude (`compiler/vyrn-frontend/src/prelude.rs:326-329`).

So: today, Vyrn's `spawn` is a **fork-join parallelism primitive over pure functions**, not a general coroutine. It cannot suspend mid-computation. It cannot do I/O. It cannot share mutable state. It runs to completion, and `join` reads the answer. The three backends agree because purity guarantees that schedule does not affect output.

## The wasm constraint

Vyrn compiles to three backends: native (LLVM/C), wasm (direct emitter), and the interpreter. A stackful coroutine needs a stack switch — save one stack, load another. On native, this is a `swapcontext` or assembly `jmp` on the stack pointer. On wasm, it is **not possible today**.

The WebAssembly stack-switching proposal (https://github.com/WebAssembly/stack-switching) adds continuation-based stack switching: `cont.new` creates a continuation, `resume` runs it, `suspend` yields back. The explainer describes coroutines, async/await, generators, and lightweight threads as the motivating use cases (https://github.com/WebAssembly/stack-switching/blob/main/proposals/stack-switching/Explainer.md).

The proposal is at **Phase 3 — Implementation Phase (CG + WG)** in the WebAssembly standardization process (https://github.com/WebAssembly/proposals/blob/main/README.md). Phase 3 means spec text is available and engines are prototyping, but the feature is not standardized (Phase 5) and not merged into the spec.

Implementation status, from the WebAssembly features tracker (https://raw.githubusercontent.com/WebAssembly/website/main/features.json):

- **Chrome**: behind an experimental flag (`chrome://flags/#enable-experimental-webassembly-stack-switching`). Not shipped by default.
- **Firefox**: not shipped (the tracker shows no stable version for stack switching in Firefox).
- **Safari/WebKit**: not shipped.
- **Wasmtime**: version 7.0 and later support stack switching behind the `--ext:stack-switching` flag.
- **wasmer**: `false` — not supported.
- **wasm3**: `false` — not supported.
- **V8 (standalone)**: `true` — V8's trunk has the implementation, but Chrome only exposes it behind the flag above.

No production browser ships stack switching unflagged. No wasm runtime that Vyrn targets (the direct backend emits `wasm32-wasi` modules, run by wasmtime or the browser's `wasi-min.js`) has it on by default.

This is the most important fact in this file: **a stackful coroutine design cannot run on Vyrn's wasm backend today**. The wasm target has a single linear stack and no instruction to switch it. The proposal that would fix this is at Phase 3 with no shipped implementation in any browser or default runtime Vyrn uses.

Vyrn's current `spawn` sidesteps this entirely. On wasm, `spawn` runs eagerly and inline — no stack switch, no second stack, no coroutine. The purity guarantee means the eager schedule produces the same answer as any parallel schedule, so the wasm backend never needs to switch stacks.

A general coroutine (one that can suspend mid-computation and resume later, like an async/await or a generator) would need either:
- a stackless lowering (state machine, no stack switch needed), or
- the wasm stack-switching proposal to ship and become a baseline Vyrn can require.

The first is available now. The second is not.

## What a coroutine that suspends does to a borrow

Vyrn has ownership and a move checker (`compiler/vyrn-frontend/src/movecheck.rs`). A `consume` parameter takes ownership; a `read`/`share` parameter borrows. Rule 2 says a borrow is second-class: it may not be stored, captured by an escaping closure, or returned (`compiler/vyrn-frontend/src/movecheck.rs:29-35`).

A coroutine that suspends mid-computation holds its live locals across the suspension. If one of those locals is a borrow, the borrow must survive the suspension. This is the central problem Rust had to solve for async/await, and the solution is one of the most criticized parts of Rust's design.

### How Rust answered it

A Rust `async fn` is compiled to a state machine struct. The struct holds every local that is live across an `.await` point. If a local is a reference (`&T` or `&mut T`), the struct holds the reference. The struct itself is a value that can be moved — placed in a `Box`, stored in a `Vec`, passed to a function. Moving the struct moves the reference inside it.

The problem: the reference may point into the struct itself. Consider:

```rust
async fn f(mut v: Vec<u32>) {
    let r = &mut v[0];
    something().await;  // v and r are both live here
    *r += 1;
}
```

After the `.await`, the state machine struct holds both `v` (the `Vec`) and `r` (a `&mut` into `v`). If the struct is moved — which it will be, because the executor stores it on the heap and may move it between polls — `r` still points at `v`'s old address. The borrow is invalid.

Rust's answer is **`Pin`** (RFC 2349, stabilized in Rust 1.33). `Pin<P>` is a wrapper that guarantees the pointee will not be moved. An executor polls a future through `Pin<&mut Self>`, and the pinning contract says the future's memory will not be invalidated. The self-referential borrow inside the state machine stays valid because the struct cannot move.

The cost of `Pin`:

1. Every `async fn` returns a type that is only safe to poll through `Pin<&mut Self>`. The `Future` trait's `poll` method takes `Pin<&mut Self>`, not `&mut Self`.
2. Every combinator (`map`, `then`, `join`, `select`) must propagate pinning. An executor must construct the pin safely. A user who holds an `Unpin` future can poll it freely; a user who holds a `!Unpin` future must pin it first, with `Box::pin` or `tokio::pin!`.
3. The pinning safety proof is subtle. `Pin` is a library type with an unsafe constructor; the safety invariant is documented, not enforced. Getting it wrong causes undefined behaviour.
4. The self-referential case arises from ordinary code — `let r = &v[0]; something().await;` — so the compiler must detect it and generate the state machine correctly, and the borrow checker must accept the self-referential pattern that would be rejected in synchronous code.

### What Vyrn's move checker does today

Vyrn's `spawn` moves arguments across the task boundary exactly like a direct call. A `consume` parameter takes ownership; the move checker records the argument as `Gone::Captured` — "a lambda or a spawn holds it, and either can outlive this block" (`compiler/vyrn-frontend/src/movecheck.rs:150-151`, `3858-3868`). A spawned frame outlives the statement that spawns it, so the spawning block releases nothing it was handed (`compiler/vyrn-frontend/src/movecheck.rs:3866-3868`).

There are no borrows across a `spawn` because `spawn` takes arguments by value, not by reference. The `share` capability lets multiple tasks hold concurrent read access to the same immutable value (`rfcs/RFC-0004-capabilities-and-memory.md:252`), but a `share` parameter does not create a borrow in the move checker's sense — it is a shared reference to an immutable value, and the spawned function cannot mutate it.

The borrow-across-suspension problem does not arise for Vyrn's current `spawn` because `spawn` has no suspension points. The task runs to completion. A general coroutine that could suspend mid-expression — say, an `await` that yields control back to a scheduler while holding a `read` borrow of a local — would create exactly the problem Rust solved with `Pin`.

If Vyrn adopted a stackless coroutine (state machine), the state machine struct would hold the borrow across the suspension. The move checker's rule 2 ("a borrow may not be stored") would need a new exception for the coroutine's own state, or the borrow would need to be banned across suspension points. Both choices have costs.

If Vyrn adopted a stackful coroutine, the borrow lives on the coroutine's stack. The stack is a contiguous block of memory. If the stack is fixed in place (not movable), borrows into it are valid — this is the Lua and Go model, and it works because the stack is not a value that gets moved. If the stack is movable (copied on growth, like Go), borrows into it must be updated — Go's stack growth does this with a pointer map and a scan. The cost is the growth machinery and the scan, not a `Pin`-type abstraction.

---

## What Vyrn has today

The parser has a `Spawn` expression (`compiler/vyrn-frontend/src/ast.rs:1300-1304`) and the lexer has a `Spawn` token (`compiler/vyrn-frontend/src/lexer.rs:259`). The parser handles `spawn f(args)` as a keyword expression (`compiler/vyrn-frontend/src/parser.rs:4581-4584`). The checker has a `spawn_safe` fixpoint that proves isolation transitively (`compiler/vyrn-frontend/src/checker.rs:575-643`). The type `Task<T>` exists as a variant in the AST (`compiler/vyrn-frontend/src/ast.rs:839-841`) and lowers to the result type `T` itself — "a deterministic fork-join needs no boxing" (`compiler/vyrn-frontend/src/ast.rs:839-840`).

The move checker handles `spawn` as a move site: arguments cross the task boundary, and a spawned frame outlives its block (`compiler/vyrn-frontend/src/movecheck.rs:3858-3868`). The `@join` builtin declares `consume Task<T> -> T` in the prelude (`compiler/vyrn-frontend/src/prelude.rs:326-329`). `Task<T>` is linear: it joins `Stream<T>` on the must-use row (`compiler/vyrn-frontend/src/movecheck.rs:4010-4016`, `compiler/vyrn-frontend/src/own.rs:358-361`).

The native backend lowers `spawn` to `__vyrn_spawn` in the C shim, which creates a real OS thread (`compiler/vyrn-codegen/src/toolchain.rs:441-656`). The wasm backend runs the thunk inline — no threads, no stack switch (`compiler/vyrn-codegen/src/toolchain.rs:496-501`, `compiler/vyrn-codegen/src/direct.rs:8613-8645`). The interpreter runs tasks eagerly (`compiler/vyrn-frontend/src/interp.rs:6030-6034`).

What nearly exists: the `spawn_safe` fixpoint is a purity analysis that already runs. It could serve as the safety proof for a more general coroutine (one that can do I/O or suspend), because it already answers "what can this function transitively touch?"

What would have to change for a general coroutine: the wasm backend has no stack switch, so a stackful coroutine is impossible without the stack-switching proposal shipping. A stackless coroutine would need the compiler to transform function bodies into state machines, which does not exist. The move checker's rule 2 would need a new answer for borrows that live across a suspension point. The `Task<T>` type would need to represent a suspended computation, not just a running one.

## The options

RECOMMENDATION, NOT A DECISION.

|design|one-sentence description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|**A. Keep `spawn` as fork-join, add no coroutine**|`spawn` stays a pure fork-join primitive; concurrency with I/O stays on the `serve` event loop (RFC-0016), not the language|none|none|none|nothing — the corpus is unchanged|Go before async I/O; Zig after removing async; Erlang (processes, not coroutines)|
|**B. Stackless `async`/`await` (state machine)**|`async fn` compiles to a state machine; `await` suspends; a runtime polls; `Task<T>` becomes a future|new `async`/`await` keywords and expression forms|`async fn` colours functions; borrow checker needs a `Pin`-like answer for self-referential state machines|new lowering pass: transform body to state machine struct; per-suspension-point fields; resume dispatch; wasm needs no stack switch|every function that wants to do concurrent I/O changes signature; `serve` integration; the `spawn_safe` fixpoint extends to `async fn` purity|Rust, Kotlin, C++20, Python|
|**C. Stackful green threads on native, eager on wasm**|`go f(args)` spawns a stackful coroutine on native (a `may`-style library or a runtime); on wasm it runs eagerly like `spawn` does today|new `go` keyword (or overload `spawn`)|isolation analysis widens: a coroutine that does I/O is not pure, so the `spawn_safe` fixpoint needs an effect system, not a purity proof|native: stack allocation and switch (assembly or `boost::context`); wasm: eager inline (same as today)|a coroutine that suspends on wasm cannot actually suspend — it must run to completion, which means the wasm program is sequential for any coroutine that blocks; native and wasm diverge in capability, not in output|Go, `may` crate, Java virtual threads (on native); no one does this on wasm because wasm cannot|
|**D. Stackful coroutines gated on wasm stack-switching**|same as C, but the wasm backend uses the stack-switching proposal when available and falls back to eager when not|same as C|same as C|wasm: emit `cont.new`/`resume`/`suspend` if the feature is detected; eager otherwise; native: same as C|two wasm code paths; the feature is Phase 3 and unflagged in no browser — Vyrn would be depending on a proposal that may change; programs that suspend on wasm only work on engines with the flag|no language ships this today — the proposal is too new|
|**E. Stackless generators as the primitive, build async on top**|add `gen`-style generators (which Vyrn already has for compile-time, RFC-0021) as a runtime coroutine primitive; `async`/`await` desugars to generator yield/resume|reuse the `gen` keyword|generators already have a purity analysis (`comptime_purity`); extending it to runtime generators is incremental|generators already lower to wasm (RFC-0076); a runtime generator that yields to a scheduler is a state machine the backend already emits|the `gen` machinery is built for compile-time generation, not runtime scheduling — the host interface, the cache, and the sandbox are wrong for a runtime coroutine; refactoring them is the cost|Python (generators → async), JavaScript (generators → async iterators), C# (iterators → async)|

### Notes on the options

**Option A** is the status quo. Vyrn's `spawn` is a parallelism primitive, not a concurrency primitive. Concurrent I/O is handled by `serve` (RFC-0016), which runs an event loop in the runtime, not in the language. The interpreter, native, and wasm backends all agree because purity guarantees schedule independence. The cost of A is the cost of not having a language-level coroutine: a server that wants to interleave I/O with computation uses the `serve` loop's callback model, not a linear `async`/`await` sequence. The benefit is zero new complexity in the parser, checker, or backends, and no wasm constraint.

**Option B** is what Rust and Kotlin did. It colours functions: `async fn` returns a different type, and callers must know. It needs a `Pin`-like answer for self-referential borrows across suspension points. It does not need a stack switch, so it works on wasm today. The state machine transformation is a new compiler pass that does not exist in Vyrn. The `spawn_safe` fixpoint would extend to an effect system: an `async fn` that does I/O is not pure, so the current binary "pure or not" answer becomes a set of effects.

**Option C** is what Go and the `may` crate did. It does not colour functions. It needs a stack switch on native, which is well-understood (assembly `swap` on the stack pointer, or `boost::context`, or the `generator-rs` crate that `may` uses). On wasm, it cannot suspend — so a coroutine that blocks on I/O on wasm must run to completion, which means the wasm program is sequential for any blocking operation. This is the same as today's `spawn` on wasm, but it means native and wasm have different capabilities: a program that suspends a coroutine on native cannot do the same on wasm. The output is the same (purity), but the concurrency is not.

**Option D** is option C with the wasm stack-switching proposal as the path to suspension on wasm. The proposal is at Phase 3, unflagged in no browser, and not supported by default in wasmtime or wasmer. Vyrn would be betting on a proposal that may change before standardization. No language ships this today.

**Option E** reuses the generator infrastructure Vyrn already has (RFC-0021, RFC-0076) as a runtime coroutine primitive. The appeal is that the lowering, the purity analysis, and the wasm emission already exist for compile-time generators. The cost is that the `gen` machinery is built for compile-time: the host interface serves the compiler's loader, the cache is keyed on source hashes, and the sandbox bars module state. A runtime generator that yields to a scheduler needs none of those and needs a scheduler, a reactor, and a cancellation mechanism that the `gen` path does not have. Refactoring the shared lowering to serve both is the cost.
