# Census — Operator Overloading, and Computing on a GPU

Three questions the owner asked together, because the same language machinery touches all three: what operators cost, what array computing asks of a language, and what a GPU kernel needs.

---

## 1. Operator overloading across nine languages

### C++

Which operators: nearly all of them — `+ - * / % += -= *= /= %= == != < > <= >= && || & | ^ ~ ! << >> ++ -- -> ->* , () [] new delete` and their combinations. C++23 adds `<=>` (the three-way comparison operator). New operators: no. The set is fixed by the grammar. Precedence: fixed by the language, unchangeable by the programmer. An overloaded `+` has the precedence of `+`. Dispatch: static by default. A `+` on two operands resolves at compile time to either a built-in or a free function (`operator+(T, U)`) or a member (`T::operator+(U)`), by overload resolution. Virtual dispatch is possible if the operator is itself a virtual member function, which is legal but rare. What stops unreadable code: nothing in the language. `cout << "x"` is operator overloading used as syntax, and the community opinion on it is divided. The convention is that operators should match the semantics of the built-ins (`+` adds, `==` compares), enforced by review, not by the compiler.

### Rust (traits)

Which operators: `+ - * / % == != < > <= >= && || ! & | ^ << >>` and `[]` (indexing), `*` (dereference). Each maps to a trait: `Add`, `Sub`, `Mul`, `Div`, `Rem`, `PartialEq`, `PartialOrd`, `BitAnd`, `BitOr`, `BitXor`, `Shl`, `Shr`, `Index`, `Deref`. New operators: no. The set is fixed. Precedence: fixed by the language. Dispatch: static. Trait resolution is monomorphized; the compiler picks the impl at compile time and generates a direct call. There is no virtual dispatch on operators unless the trait object is explicitly boxed (`dyn Add`), which is legal but defeats the purpose. What stops unreadable code: the trait must be in scope. A user who has not imported `std::ops::Add` cannot overload `+`. The orphan rule prevents two crates from implementing the same trait for the same type. The naming convention — `Add::add` — and the requirement that the trait be named after the operation create a discoverability trail that C++'s free functions lack.

### Python (dunder methods)

Which operators: all of them, plus more than any other language here — `+ - * / // ** @ == != < > <= >= << >> & | ^ ~` and their in-place forms (`+=`, etc.), unary `- + ~`, indexing `[]`, slicing, `with` (context manager), iteration, `len`, `repr`, `str`, `format`, and attribute access itself (`__getattr__`, `__setattr__`). New operators: no. The set is fixed. Precedence: fixed by the language. Dispatch: dynamic. `a + b` calls `a.__add__(b)`, and if that returns `NotImplemented`, Python tries `b.__radd__(a)`. The method is looked up on the runtime type, through the MRO, at every call. There is no compile-time resolution because there is no compile time. What stops unreadable code: nothing. A library can define `__add__` to send an email. The community convention is that operators should match built-in semantics, enforced by review and by the expectation that `__repr__` round-trips.

### Swift (custom operators)

Which operators: the standard set can be overloaded by implementing protocol methods (the `AdditiveArithmetic`, `Equatable`, `Comparable` protocols). New operators: yes. Swift lets a programmer declare a new operator with `infix operator +++: AdditionPrecedence` and implement it as a static function `static func +++ (lhs: T, rhs: T) -> T`. Precedence: the programmer assigns a precedence group. `precedencegroup AdditionPrecedence { associativity: left higherThan: RangeFormationPrecedence }`. Dispatch: static. Swift compiles to a direct call through protocol witness tables that are known at compile time. Generic code is specialized; unspecialized code goes through a witness table, not a per-call MRO walk. What stops unreadable code: the operator must be declared at file scope, and the precedence group must be named. A reader can find the declaration. But nothing prevents `+++` from doing anything, and the community has debated whether custom operators are a net positive — the Swift API Design Guidelines discourage them.

### Haskell (typeclasses)

Which operators: all of them. `+ - * /` through `Num`, `== /=` through `Eq`, `< >` through `Ord`, `>>= >>` through `Monad`, `<>` through `Semigroup`, `$` through its own definition. New operators: yes, freely. Any sequence of operator characters is a valid operator name. `x -*- y` is a legal definition. Precedence: the programmer declares it with `infixl 5 -*-` (left-associative, precedence 5). Dispatch: static. Haskell's typeclass resolution is done at compile time, and GHC specializes calls where it can. The dictionary is passed implicitly, not looked up on a value. What stops unreadable code: the type system. An operator has a type, and the type says what it does. `(+*) :: Num a => a -> a -> a` tells the reader it is arithmetic. The cost is that the operator set in real Haskell code is large, and a reader who does not know the library must look up each one. Hoogle (the type-based search engine) is the community's answer.

### Scala

Which operators: all of them. In Scala, every operator is a method. `a + b` is `a.+(b)`. The set of characters that can form an operator is large (`+ - * / % & | ^ ! < > = ~ : @ #`). New operators: yes, by defining a method with an operator name. Precedence: fixed by the first character of the method name, in a table the language defines. `*` binds tighter than `+` because `*` is above `+` in the precedence table, not because the programmer said so. Dispatch: static when the type is known, dynamic when it is not (Scala runs on the JVM, so dispatch is virtual by default, but the JIT inlines monomorphic call sites). What stops unreadable code: convention. Scala's community has a strong convention toward symbolic operators in libraries (akka, cats, spark) and an equally strong backlash against them. The language provides extension methods and given/using as alternatives.

### Kotlin

Which operators: a fixed set, each mapped to a naming convention. `+` is `operator fun plus(...)`, `*` is `times`, `[]` is `get`/`set`, `in` is `contains`, `..` is `rangeTo`. New operators: no. The set is closed. Precedence: fixed by the language. Dispatch: static when the receiver type is known. Kotlin compiles to JVM bytecode (or native, via Kotlin/Native), so the dispatch is whatever the target provides. What stops unreadable code: the `operator` keyword. A function must be marked `operator` to be usable as one, which makes the intent visible at the definition site.

### Julia

Which operators: the standard set, and new operators can be defined. `a + b` is `+(a, b)`, and `+(a::MyType, b::MyType) = ...` overloads it. New operators: yes, from a fixed alphabet of operator characters. Precedence: fixed by the language for the built-in set; a new operator gets the precedence of its first character. Dispatch: dynamic, through multiple dispatch. Julia selects the method based on the runtime types of all arguments, not just the receiver. This is more general than single dispatch but pays the lookup at every call — mitigated by Julia's JIT, which specializes for the observed types. What stops unreadable code: convention. Julia's numerical community uses operators heavily (`*`, `\`, `÷`, `∘` for composition) and the convention is that they match mathematical notation.

### C\#

Which operators: a fixed set. `+ - * / % == != < > <= >= && || & | ^ ! << >>` and `++ --`, `true`/`false`, indexing `[]`, casting (`implicit`/`explicit` operator). New operators: no. Precedence: fixed by the language. Dispatch: static. C# resolves overloaded operators at compile time through the `op_Addition` etc. methods. What stops unreadable code: the operator methods have compiler-mandated names (`op_Addition`, `op_Equality`), and the static modifier is required. The .NET Guidelines say operators should be symmetric and match built-in semantics.

### Summary table

|language|which operators|new operators|precedence|dispatch|readability guard|
|---|---|---|---|---|---|
|C++|nearly all, including `() [] -> ,`|no|fixed|static (virtual possible)|convention only|
|Rust|all standard, via traits|no|fixed|static, monomorphized|trait must be in scope; orphan rule|
|Python|all, plus attribute access and context managers|no|fixed|dynamic, MRO|convention only|
|Swift|standard via protocols; new ones yes|yes, with precedence group|programmer-declared|static|discouraged by guidelines|
|Haskell|all, plus any operator character sequence|yes, freely|programmer-declared|static, typeclass dictionaries|the type signature|
|Scala|all, every operator is a method|yes, method name|fixed by first character|static or JVM virtual|convention; extension methods as alternative|
|Kotlin|fixed set, `operator fun`|no|fixed|static when type known|`operator` keyword|
|Julia|standard, plus new from fixed alphabet|yes|fixed by first character|dynamic, multiple dispatch|convention; math notation|
|C#|fixed set, `op_` methods|no|fixed|static|naming convention, static required|

---

## 2. What Python pays, and what a statically dispatched language gets for free

The owner asked how to beat Python. The answer is three costs Python pays that a statically dispatched, monomorphized language does not.

### Cost 1: dynamic dispatch through `__add__` and `__radd__`

Python's `a + b` is, at runtime:

1. Look up `__add__` on `type(a)` through the MRO.
2. Call it with `b`.
3. If it returns `NotImplemented`, look up `__radd__` on `type(b)` through the MRO.
4. Call it with `a`.
5. If that also returns `NotImplemented`, raise `TypeError`.

Every operator call is a method lookup on the runtime type. There is no compile-time type to resolve against, because Python variables have no type. The MRO walk is a linear search through the base classes, and the method itself is a Python function call — which means a frame is allocated, arguments are packed into a tuple, and the interpreter dispatches through its own eval loop.

A statically dispatched language resolves the operator at compile time. `a + b` where `a: F32x4` and `b: F32x4` becomes, in Vyrn's textual backend, `fadd <4 x float>` — one instruction, no lookup, no frame, no `NotImplemented` check. The checker's `binop_type` function (`compiler/vyrn-frontend/src/checker.rs:5278`) matches the operand types and returns the result type at check time. The interpreter's `binop` function (`compiler/vyrn-frontend/src/interp.rs:6107`) pattern-matches the `Val` variants. Neither looks up a method on a value.

### Cost 2: boxing every intermediate

Python's `int` is a heap-allocated `PyObject` (28 bytes on CPython 3.12 for a small integer). `float` is 24 bytes. An array of 1000 floats is 1000 pointers to 1000 heap objects. `a + b + c` allocates a new `float` for `a + b`, then another for `(a + b) + c`. The intermediate is boxed because Python has no unboxed value — every value is a `PyObject*`.

A monomorphized language lowers `a + b` to a register or stack slot. `F32x4` in Vyrn is a 16-byte value held in a `v128` register on the wasm backend or a `<4 x float>` on the textual backend (`compiler/vyrn-frontend/src/ast.rs:711`). The interpreter holds it as `[f32; 4]` inline in the `Val` enum (`compiler/vyrn-frontend/src/interp.rs:319`). No heap allocation, no pointer chase, no refcount increment.

### Cost 3: no fusion

Python evaluates `a + b * c` as: compute `b * c` (allocate a new array), then compute `a + (that)` (allocate another new array). For NumPy arrays, each operation is a separate ufunc call that produces a temporary array. There is no way for the language to fuse `a + b * c` into a single pass over the arrays, because the `__add__` and `__mul__` methods are opaque Python functions the interpreter cannot look inside.

A statically dispatched, monomorphized language lowers each operator to a known instruction. LLVM sees `fadd <4 x float>` after `fmul <4 x float>` and can fuse them into `vfmadd` (if the ISA permits and the precision contract allows — which Vyrn forbids, per RFC-0083 §"The upgrade path"). The point is that the *opportunity* to fuse is free: the compiler sees the actual operations, not opaque method calls. Vyrn deliberately declines to use FMA fusion for float (it would change bits), but integer fusion is available and measured: RFC-0083 records that LLVM vectorises a scalar `Int32` sum and beats hand-written `I32x4` by 2x (`rfcs/RFC-0083-portable-simd.md:58-66`).

### What Vyrn protocols are, and why this matters

RFC-0084 — "Protocol Dispatch Is Static Everywhere" — established that Vyrn protocol dispatch is static and monomorphized on all three engines. The checker computes a type key from the *static* receiver type and resolves to a mangled function name (`compiler/vyrn-frontend/src/parser.rs:804-830`). The interpreter stamps the type name on the value at coercion boundaries and dispatches from it (`compiler/vyrn-frontend/src/checker.rs:2896-2910`). There are no vtables. There is no dynamic dispatch. Every protocol method call is a direct call to a monomorphized function (`rfcs/RFC-0084-static-protocol-dispatch.md:248-250`).

The implication for operator overloading is direct: if operators were protocols, dispatch would be static and monomorphized. The three Python costs — dynamic dispatch, boxing, no fusion — are costs of *dynamic* dispatch and *runtime* values. A protocol-based operator in Vyrn would pay none of them.

### Is operator overloading just a protocol with syntax?

Vyrn today has three compiler-internal bounds — `Num`, `Ord`, `Eq` — that unlock operators on type parameters (`compiler/vyrn-frontend/src/checker.rs:5292-5301`). These are not protocols. They have no declarations, no `impl` blocks, no method bodies. They are a hardcoded implication chain: `Num` implies `Ord` implies `Eq` (`compiler/vyrn-frontend/src/checker.rs:2905-2907`). A type parameter `<T: Num>` can use `+ - * / %`, `< <= > >=`, and `== !=`. A type parameter `<T: Ord>` can use the comparison operators but not arithmetic. A type parameter `<T: Eq>` can use `== !=` only.

These bounds are checked against concrete types by `type_satisfies` (`compiler/vyrn-frontend/src/checker.rs:2916-2920`), which admits only `Int`, `Float`, `Float32`, and sized `IntN`. SIMD types (`F32x4`, `I32x4`, `F64x2`) are *not* in this set. Their operators are handled by separate arms in `binop_type` (`compiler/vyrn-frontend/src/checker.rs:5331-5348`), not by the `Num` bound.

The built-in protocols (`Copy`, `Show`, `Iterate`, `Fallible`, `Index`) are declared with `protocol` blocks and implemented with `impl` blocks, and they resolve to mangled function names. A protocol method call `x.show()` becomes `Show__Key__show(x)`. If `+` were a protocol method, `a + b` would become `Add__Key__add(a, b)`, and the checker would resolve it the same way it resolves every other protocol call: by the static type of the receiver.

So the answer is: yes, in Vyrn, operator overloading *is* a protocol with syntax — or rather, it would be. The mechanism already exists. The `Num`/`Ord`/`Eq` bounds are a prototype of the idea, hardcoded for the built-in numeric types. Generalizing them to protocols would mean: declaring `protocol Num { fn add(self, other: Self) -> Self; ... }`, implementing it for each numeric type, and having the checker rewrite `a + b` to `a.add(b)` the same way it rewrites `x.show()` to `Show__Key__show(x)`. The static dispatch, the monomorphization, and the fusion opportunity are all already there.

The cost is in the parser, not the dispatch. Today, `binop_type` is a flat `match` on `(op, l, r)` with one arm per type pair. A protocol-based system would need the parser to rewrite `a + b` into a protocol method call before the checker sees it, the way it already rewrites `x.show()` into `Show__Key__show(x)` (`compiler/vyrn-frontend/src/parser.rs:196-206`). That is the same machinery, applied to a new syntactic surface.

---

## 3. Array and GPU computing

### NumPy broadcasting and the ufunc protocol

NumPy's `a + b` on two arrays is not Python's `__add__`. It is a ufunc: a C function that operates element-wise, with broadcasting. Broadcasting is the rule that makes shapes compatible: a `(3, 4)` array and a `(4,)` array produce a `(3, 4)` result, because the shorter shape is padded with 1 on the left and the 1 is stretched to match. The ufunc protocol (`np.frompyfunc`, `np.vectorize`) lets a user define a Python function and have it called element-wise. But the overhead is the same Python function-call overhead from §2: each element is boxed, each call is a dispatch, and there is no fusion. NumPy is fast because its ufuncs are C loops over unboxed memory, not because the language helps.

The ufunc protocol is the boundary between "fast" and "slow" in NumPy. A built-in ufunc (`np.add`) is a C loop. A user-defined ufunc (`np.frompyfunc(my_func)`) calls back into Python per element. The performance gap between them is 100-1000x, and it is exactly the gap between static dispatch with unboxed values and dynamic dispatch with boxed values. NumPy's model is: the array is the fast path, and everything else pays.

### Julia broadcast fusion and why it composes

Julia's `f.(a) .+ g.(b)` is not three passes. It is one pass. Julia's broadcast is a syntax transformation: `f.(a) .+ g.(b)` becomes a single loop that calls `f(a[i]) + g(b[i])` at each index, with no temporary arrays. This is "broadcast fusion", and it works because:

1. Broadcasting is syntactic (the `.` before the operator), not a method call.
2. The fused loop is lowered to Julia IR, where the JIT specializes it for the element types.
3. There is no allocation of intermediate arrays, because the compiler sees the whole expression.

This composes: `f.(a) .+ g.(b) .* h.(c)` is one loop with three calls. The key is that Julia's multiple dispatch resolves `f`, `g`, `h` to concrete functions at specialization time, and the JIT inlines them into the loop. A NumPy expression `f(a) + g(b) * h(c)` is three ufunc calls with two temporaries, because each ufunc is an opaque C function the caller cannot look inside.

### JAX tracing and `jit`

JAX takes a different route to fusion: tracing. When `jax.jit(f)(x)` is called, JAX does not execute `f`. It traces `f` by feeding it abstract values (shaped arrays with no data) and recording every operation into a Jaxpr — an intermediate representation. The Jaxpr is then compiled by XLA, which fuses operations, optimizes memory layout, and lowers to the target (CPU, GPU, TPU).

The consequence is that `f` must be pure: no side effects, no in-place mutation, no control flow that depends on the value of a traced array (only on its shape). `jax.jit` produces a single fused kernel from a Python function. The Python overhead — the tracing — is paid once. The compiled kernel runs without Python.

JAX's model is the one closest to what a GPU kernel needs, because XLA targets GPUs directly. But it works by leaving Python: the traced function is a specification, not the execution. The execution is XLA's output.

### CUDA and HIP programming models

A CUDA kernel is a C++ function marked `__global__`, launched with a grid of blocks, each block containing threads. Every thread has: a thread index (`threadIdx`, `blockIdx`, `blockDim`), access to shared memory (per block), and access to global memory (the device). The kernel is compiled by `nvcc` to PTX (NVIDIA) or by `hipcc` to GCN (AMD/ROCm). HIP is AMD's CUDA-equivalent API, designed to be source-compatible: `hip` code is `cuda` code with `cuda` replaced by `hip`.

The programming model is SPMD: one function, many threads, each with a unique index. The hardware executes threads in warps (NVIDIA, 32 threads) or wavefronts (AMD, 64 threads), and the performance model is about coalesced memory access, occupancy, and divergence.

### What a kernel needs from a language

A function that runs as a GPU kernel needs four things:

1. **Contiguous memory.** The kernel reads and writes arrays by address. If the array is not contiguous (or the strides are not known), the compiler cannot generate address arithmetic. NumPy's `ascontiguousarray` exists because of this.

2. **No allocation.** A GPU kernel cannot call `malloc`. Global memory is pre-allocated; shared memory is statically sized. A language whose array operations allocate temporaries (NumPy, Python) cannot run as a kernel without fusion or tracing.

3. **No dynamic dispatch.** A GPU has no vtable, no MRO, no method lookup. Every function call in a kernel must resolve to a direct call at compile time. Python's `__add__` cannot run on a GPU. Julia's multiple dispatch can, because the JIT specializes before launch. JAX's tracing can, because XLA compiles before launch.

4. **A known thread index.** The kernel needs a way to read `threadIdx`/`blockIdx`. This is either a builtin (`threadIdx.x` in CUDA) or a language construct (Julia's `indices(1)` in a kernel, JAX's implicit mapping in `vmap`/`pmap`).

---

## 4. HVM4

The repository is at `https://github.com/HigherOrderCO/HVM4`. As of 2026-08-23 it has 116 stars, 28 forks, 24 open issues. The README says: "you're here before launch. Use at your own risk."

### What it evaluates

HVM4 is a C runtime for the Interaction Calculus, a model of computation that extends the lambda calculus with two forms: duplications (`!x &= v; body`, which makes `x` available as `x₀` and `x₁`) and superpositions (`&{a, b}`, which places two values in one position). These correspond to Lafont's fan nodes in Interaction Combinators (1997). The system has four core rewrite rules: APP-LAM (application eliminates lambda), DUP-SUP (duplication eliminates superposition, same label), APP-SUP (application propagates through superposition), DUP-LAM (duplication propagates through lambda). When labels differ, DUP-SUP commutes instead of annihilating (`docs/theory/interaction_calculus.md`).

The runtime is a single C file (`src/hvm.c`). It parses HVM source into static "book" terms (immutable, de Bruijn levels), lazily allocates dynamic heap terms (mutable, linked by pointers), reduces with a stack evaluator applying WNF interactions, and prints results through a collapser that lifts superpositions and enumerates branches (`AGENTS.md`).

Every term is a 64-bit word: `TAG (8 bits) | EXT (24 bits) | VAL (32 bits)`. The tag is the constructor variant, EXT carries metadata (dup label, constructor name, op code), and VAL is the payload (heap location or immediate). This is a compact representation that fits in a register (`docs/hvm/memory.md`).

### What parallelism it claims

The Interaction Calculus is inherently parallel: each interaction is a local rewrite that touches two adjacent nodes, and two non-overlapping interactions can proceed simultaneously. This is the property that makes Interaction Combinators "optimal" — the number of rewrite steps is minimal for the given term, regardless of evaluation order.

The HVM4 README and documentation do not publish benchmark numbers comparing against a reference implementation. The `devs/bench/` directory contains four benchmarks: `u32_fib` (Fibonacci of 38), `lambda_eval` (lambda calculus normalization via Church numerals), `gen_mul4k` (generic multiplication), and `cnot_24` (24-fold composed NOT on Church booleans). These are benchmark programs, not published results. The README shows how to run them (`hvm devs/bench/u32_fib.hvm -s`) but does not print the output or claim a speedup.

The parallelism claim is theoretical: the model is parallel by construction, but the C runtime is single-threaded. The issues list confirms this: issue #57 ("wnf_at is not thread-safe on shared cells") and issue #21 ("recursive dups cause race conditions") are open. Issue #33 ("WNF in CUDA") and issue #30 ("WNF in Metal") are feature requests, not implementations. The runtime does not yet exploit the parallelism the model promises.

### What the claims are measured against

Nothing published. The repository has no benchmark results, no comparison table, no "HVM is Nx faster than Y" claim in its README. The prior project (HVM3 / Bend, by the same organization) published benchmarks; HVM4 is a rewrite and has not yet reached the point of publishing its own. The `devs/bench/` files are the apparatus for future measurements.

### Where it is on the path from research to use

Pre-launch. The README says so. The issues are about correctness (thread safety, divergence analysis, a JIT compiler) and platform (CUDA, Metal, cross-platform embedding). The `devs/issues/` directory contains bug reproductions (a dynamic dup bug, a fork syntax bug, an interpreter-vs-compiler OOM). The runtime evaluates pure programs and prints the result of `@main`. There is no FFI, no I/O, no file system, no network. The language has no type system. The collapser (the readback from interaction nets to lambda terms) has an open bug (#37: "CNF does not lift INC through non-lambda nodes").

HVM4 is a research runtime for a theoretical model. The model is interesting — optimal sharing, parallel by construction — but the implementation has not yet demonstrated the parallelism or published the measurements that would let a reader evaluate the claim. It is earlier on the path than JAX (which ships), earlier than Julia (which ships), and earlier than HVM3/Bend (which published benchmarks). The distance from here to a GPU kernel is large: the runtime needs a parallel evaluator, the evaluator needs a GPU target, and the GPU target needs a mapping from interaction nets to thread blocks, which is what issues #33 and #30 ask for.

---

## 5. What Vyrn has today

Vyrn has SIMD value types: `F32x4`, `I32x4`, `F64x2`, `Mask32x4`, `Mask64x2` (`compiler/vyrn-frontend/src/ast.rs:711-758`). These are value types — no heap, no ownership beyond a scalar's, nothing to drop (`rfcs/RFC-0083-portable-simd.md:95-97`). They are declared in RFC-0083, which shipped M1 through M4 (`rfcs/RFC-0083-portable-simd.md:3-9`).

Operators on these types are hardcoded arms in the checker's `binop_type` (`compiler/vyrn-frontend/src/checker.rs:5331-5348`) and the interpreter's `binop` (`compiler/vyrn-frontend/src/interp.rs:6107-6174`). Each operator (`+`, `-`, `*`, `/`, `<`, `<=`, `>`, `>=`, `==`, `!=`, `&`, `|`, `^`, `~`, unary `-`) has a specific arm per type. These are `BinOp` and `UnOp` variants — they never reach the interpreter's `Call` dispatch, so they never look up a method (`rfcs/RFC-0083-portable-simd.md:547-549`).

Vyrn has static, monomorphized protocol dispatch (RFC-0084, `rfcs/RFC-0084-static-protocol-dispatch.md:248-250`). A protocol method call resolves at check time to a mangled function name, and the interpreter stamps the type name on the value at coercion boundaries so it can do the same. There are no vtables. There is no dynamic dispatch. Every call is a direct call to a concrete function.

Vyrn has compiler-internal bounds — `Num`, `Ord`, `Eq` — that unlock operators on type parameters (`compiler/vyrn-frontend/src/checker.rs:5292-5301`). These are hardcoded: `Num` implies `Ord` implies `Eq`, and the admissible types are `Int`, `Float`, `Float32`, and sized `IntN` (`compiler/vyrn-frontend/src/checker.rs:2916-2920`). They are not protocols. They have no declarations and no `impl` blocks.

Vyrn has built-in protocols — `Copy`, `Show`, `Iterate`, `Fallible`, `Index` — that *are* declared and implemented, and that *do* resolve through the static dispatch mechanism (`compiler/vyrn-frontend/src/types.rs:245-329`). These are for methods (`x.copy()`, `x.show()`, `for x in xs`), not for operators.

Vyrn has three backends: the tree-walking interpreter (`interp.rs`), the native textual LLVM backend (`lib.rs`), and the direct wasm backend (`direct.rs`). They agree byte-for-byte (`rfcs/RFC-0101-a-backend-is-an-emitter.md:57-62`). The native baseline is `x86-64-v2`, and `-ffp-contract=off` is unconditional (`rfcs/RFC-0083-portable-simd.md:965-1004`).

Vyrn has a lowering layer (RFC-0101 M1, `compiler/vyrn-lower/src/lib.rs`) that produces a single lowered representation consumed by both compiled backends. The checker's answers become a value; a backend reads it and encodes it.

Vyrn has the constraint stated in RFC-0082: containers are Vyrn, not backend primitives. The standard library is written in Vyrn. Builtins are declarations (RFC-0094), lowered by each backend, not implemented per-backend in a host language. A SIMD type is a `Type` variant with lowering instructions in each backend, not a Vyrn library function. The standing constraint is: no backend-specific standard library implementations. The standard library runs on all three engines, and what differs between backends is encoding, not semantics.

### What would have to be true for a Vyrn function to run as a GPU kernel

A Vyrn function that runs as a GPU kernel needs:

1. **Contiguous memory.** Vyrn's `Array<T>` is `{ ptr, len, cap }` (`compiler/vyrn-frontend/src/ast.rs:830-831`). The data is heap-allocated and contiguous. A kernel that reads `xs[i]` accesses `ptr + i * sizeof(T)`, which is contiguous. Vyrn has this, as long as the array is not a slice or a view with strides. Vyrn has no slice type today, so every array access is to a contiguous buffer.

2. **No allocation.** A Vyrn function that calls `Array.push` or constructs a new array allocates. A kernel cannot allocate. Vyrn does not have a way to forbid allocation in a function today — there is no `@kernel` annotation or `noalloc` effect. The checker does not track whether a function allocates. This is missing.

3. **No dynamic dispatch.** Vyrn already has this. Every protocol call resolves statically (RFC-0084). Every operator is a `BinOp` or `UnOp` arm. There are no vtables. A Vyrn function compiled to a GPU would have direct calls only. This is the property Vyrn already has that Python, Julia (at runtime), and NumPy do not.

4. **A known thread index.** A kernel needs a way to read the thread's position in the grid. Vyrn has no such builtin. This would be a new declaration — the same kind of builtin that `F32x4.load` is, in the census's terms: a name the checker knows and each backend lowers. This is missing.

5. **A GPU backend.** Vyrn has two compiled backends: native (LLVM IR to clang) and wasm (direct encoding). Neither targets a GPU. A GPU backend would emit SPIR-V (for Vulkan/OpenCL), PTX (for NVIDIA), or GCN (for AMD). RFC-0101 establishes that a backend is an emitter: it reads the lowered form and encodes it. A GPU backend would be a third emitter, reading the same lowered form. RFC-0103 establishes that a target is a capability set: `native`, `wasi`, `browser`. A GPU target would be a fourth capability set. The architecture has room for this — RFC-0101 §"It does not make a fourth backend free" prices the target-specific residue at a third of each existing backend (`rfcs/RFC-0101-a-backend-is-an-emitter.md:3023-3026`). This is missing.

6. **No backend-specific standard library.** The standing constraint. A GPU kernel's standard library — `F32x4.load`, `F32x4.splat`, the arithmetic operators — must lower to GPU instructions from the same declarations that lower to CPU instructions. The SIMD types already do this: `F32x4.add` lowers to `fadd <4 x float>` natively and `f32x4.add` on wasm. A GPU backend would lower it to a GPU SIMD instruction. The declarations do not change; the encoding does. This constraint is already satisfied by the design, because the types are `Type` variants lowered by each backend, not Vyrn library functions.

### What Vyrn already has

| requirement | Vyrn has it? | where |
|---|---|---|
|contiguous memory|yes — `Array<T>` is `{ptr, len, cap}`, contiguous|`ast.rs:830-831`|
|no dynamic dispatch|yes — protocols are static (RFC-0084), operators are `BinOp` arms|`rfcs/RFC-0084-static-protocol-dispatch.md:248-250`|
|no backend-specific std|yes — builtins are declarations, backends encode|`rfcs/RFC-0094-a-builtin-is-a-declaration.md`, `rfcs/RFC-0082-containers-are-vyrn.md`|
|monomorphization|yes — every generic call is specialized|`parser.rs:804-830`|
|SIMD value types|yes — `F32x4`, `I32x4`, `F64x2`|`ast.rs:711-758`|
|no allocation in a function|no — no effect tracking, no `@kernel` annotation|missing|
|known thread index|no — no GPU builtin|missing|
|GPU backend|no — two backends, neither targets GPU|missing|

Vyrn has four of the seven requirements. The three it lacks are: an allocation-forbidding annotation, a thread-index builtin, and a GPU codegen target. The first two are language-level additions. The third is a backend, priced by RFC-0101 at roughly a third of an existing backend's work.

---

## 6. The options

RECOMMENDATION, NOT A DECISION.

### Design A: operator protocols

Declare `protocol Add { fn add(self, rhs: Self) -> Self }` (and `Sub`, `Mul`, `Div`, `Rem`, `Eq` for `==`, `Ord` for `<`). The parser rewrites `a + b` to `a.add(b)` the same way it rewrites `x.show()` to `Show__Key__show(x)`. The checker resolves the protocol method by the static receiver type, exactly as it does today. The existing `Num`/`Ord`/`Eq` compiler-internal bounds become sugar for `<T: Add + Sub + Mul + Div + Rem + Eq + Ord>`, or are replaced by the protocol bounds outright.

The SIMD operators in `binop_type` move from hardcoded arms to protocol impls: `impl Add for F32x4 { fn add(self, rhs: F32x4) -> F32x4 { ... } }`. The body is the same lane-wise operation. The lowering is the same: the checker resolves the call, and the backend encodes the instruction. The difference is that a user can now write `impl Add for Vec3 { fn add(self, rhs: Vec3) -> Vec3 { ... } }` and `a + b` on two `Vec3` values works, with static dispatch and no boxing.

|design|description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|A: operator protocols|operators are protocol methods, rewritten at parse time|rewrite `a + b` to `a.add(b)` in the method table, one entry per operator|delete the `binop_type` SIMD arms; protocol resolution handles them|nothing — the mangled call is what the backends already lower|the `Num`/`Ord`/`Eq` bounds change shape; every generic function that uses `<T: Num>` needs the new bound|Rust (traits), Swift (protocols)|

The cost is in the parser's method table. Today, `v.lane(k)` and `x.show()` are already rewritten there (`compiler/vyrn-frontend/src/parser.rs:129-206`). Adding `a + b` → `a.add(b)` is one entry per operator, the same shape. The risk is precedence: the parser already groups operators by precedence before rewriting, so `a + b * c` is `a + (b * c)` before any rewrite, and the rewrite sees the grouped tree. The checker cost is negative: `binop_type` loses its SIMD arms and its `Num`/`Ord`/`Eq` bound-checking arms, because protocol resolution handles all of them.

### Design B: fused broadcast expressions (Julia-style)

Add a broadcast syntax: `a .+ b` or `map(f, xs)`, lowered to a single loop with no temporaries. The broadcast is syntactic — the parser sees `.+` as a distinct operator, groups it, and lowers the whole expression to a loop before the backends see it. This is the Julia model: the fusion is in the lowering, not in a library.

|design|description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|B: fused broadcast|`a .+ b .* c` lowers to one loop over the arrays|new operator `.` prefix in the lexer; grouping rule for broadcast precedence|type-check the broadcast shape (broadcasting rules); element type from the operator|new lowering pass: build a loop with the element operations inlined|nothing in existing code — broadcast is a new surface|Julia (`.+`), NumPy (ufunc, no fusion)|

The cost is a new lowering pass. The lowered form (RFC-0101) would carry a `Broadcast` node: a set of arrays, a set of element operations, and a result shape. The backends encode it as a loop. The interpreter executes it as a loop. The fusion is free because the compiler sees the whole expression. This design does not require operator protocols — it can work with the existing `BinOp` arms on the element types — but it composes with them: if `Vec3` has `impl Add`, then `xs .+ ys` on two `Array<Vec3>` fuses into a loop of `Add__Vec3__add` calls.

### Design C: `@kernel` annotation and a GPU backend

Add a `@kernel` annotation on functions. The checker verifies that the function body does not allocate (no `Array.push`, no record construction that owns a heap array, no protocol method that allocates), does not call a function that allocates (transitive check, the same shape as the capability floor in RFC-0103), and reads a thread index from a builtin (`threadIdx`, `blockIdx`). The GPU backend (a third emitter, reading the lowered form) encodes the function as a SPIR-V or PTX kernel.

|design|description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|C: `@kernel` + GPU backend|annotated functions compile to GPU kernels with an allocation-free check and a thread-index builtin|one new annotation; one or two new builtins (`threadIdx`, `blockDim`)|transitive allocation check (new, same shape as RFC-0103's capability floor); thread-index type rule|nothing — the lowered form is the same; the GPU backend reads it|nothing in existing code — `@kernel` is opt-in|CUDA (`__global__`), JAX (`jit` + XLA), Taichi|

The cost is the allocation check and the backend. The allocation check is a transitive property: does this function, or anything it calls, allocate? This is the same shape as RFC-0103's capability floor — a union over the call closure, checked at compile time — but for an effect (allocation) rather than a capability (filesystem access). RFC-0103's floor already walks the import closure; the allocation check would walk the call closure. The GPU backend is a third emitter, priced by RFC-0101 at roughly a third of an existing backend.

### Design D: tracing (JAX-style)

Add a `@trace` annotation that, at compile time, feeds abstract values (shaped arrays with no data) to the function, records every operation into a Jaxpr-like IR, and compiles the IR to a fused kernel. The function body is Vyrn; the traced IR is a new representation; the kernel is the backend's output.

|design|description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|D: tracing|annotated functions are traced into a fused IR, then compiled to a kernel|one new annotation|purity check (no side effects, no in-place mutation); shape inference for the trace|new IR; new compilation pass from IR to backend target|nothing in existing code — `@trace` is opt-in|JAX (`jit` + XLA)|

The cost is the tracer and the IR. A tracer is a second evaluator: it walks the function body with abstract values and records operations, rather than executing them. Vyrn's interpreter is already a tree-walking evaluator; a tracer is the same walk with abstract values and a recording side effect. The IR is new, and the compilation pass from IR to a backend target is new. This is the most work of the four designs, and it buys the most: automatic fusion, automatic differentiation (if the IR supports it), and a path to TPU/accelerator targets that XLA already serves.

### Design E: do nothing

Operators stay hardcoded. SIMD types keep their `binop_type` arms. The `Num`/`Ord`/`Eq` bounds stay compiler-internal. No user-defined operators. No GPU target. No broadcast fusion. The language's numerical surface is what RFC-0083 shipped: `F32x4`, `I32x4`, `F64x2`, with explicit lane operations, and scalar loops that LLVM auto-vectorises for integers.

|design|description|parser cost|checker cost|lowering cost|what breaks|who else does it|
|---|---|---|---|---|---|---|
|E: do nothing|operators stay hardcoded, no user overloading, no GPU|nothing|nothing|nothing|nothing|C (fixed operator set), Kotlin (fixed set)|

The cost is zero. The opportunity cost is what the other designs buy: user-defined numeric types that work with operators, fused array expressions, and GPU execution. A user who wants `Vec3 + Vec3` writes `vec3Add(a, b)` today, and a user who wants GPU execution writes nothing, because Vyrn does not target a GPU.

---

## What Vyrn has today

Vyrn has SIMD value types (`F32x4`, `I32x4`, `F64x2`, `Mask32x4`, `Mask64x2`) with hardcoded operator arms in the checker (`compiler/vyrn-frontend/src/checker.rs:5331-5348`) and interpreter (`compiler/vyrn-frontend/src/interp.rs:6107-6174`). It has static, monomorphized protocol dispatch (RFC-0084, `rfcs/RFC-0084-static-protocol-dispatch.md:248-250`). It has compiler-internal bounds (`Num`, `Ord`, `Eq`) that unlock operators on type parameters but are not protocols (`compiler/vyrn-frontend/src/checker.rs:5292-5301`). It has built-in protocols (`Copy`, `Show`, `Iterate`, `Fallible`) for methods, not operators (`compiler/vyrn-frontend/src/types.rs:245-329`). It has a lowering layer (RFC-0101) and a capability floor (RFC-0103). It has no allocation tracking, no thread-index builtin, and no GPU backend. The standing constraint is that the standard library has no backend-specific implementations: builtins are declarations, and backends encode them (`rfcs/RFC-0094-a-builtin-is-a-declaration.md`, `rfcs/RFC-0082-containers-are-vyrn.md`).

## The options

RECOMMENDATION, NOT A DECISION.

|design|description|parser cost|checker cost|lowering cost|what breaks in existing code|who else does it|
|---|---|---|---|---|---|---|
|A: operator protocols|operators are protocol methods, rewritten at parse time|one method-table entry per operator; `a + b` → `a.add(b)`|delete `binop_type` SIMD arms; protocol resolution handles them|nothing — mangled calls are what backends already lower|`Num`/`Ord`/`Eq` bounds change shape; generic functions need new bounds|Rust (traits), Swift (protocols)|
|B: fused broadcast|`a .+ b .* c` lowers to one loop|new `.` prefix in lexer; broadcast precedence rule|broadcast shape inference; element type from operator|new `Broadcast` node in lowered form; backends encode as loop|nothing — broadcast is a new surface|Julia (`.+`), NumPy (ufunc, no fusion)|
|C: `@kernel` + GPU backend|annotated functions compile to GPU kernels|one annotation; thread-index builtins|transitive allocation check (same shape as RFC-0103 floor)|nothing — lowered form unchanged; GPU backend reads it|nothing — `@kernel` is opt-in|CUDA (`__global__`), JAX (`jit`+XLA), Taichi|
|D: tracing|annotated functions traced into fused IR, then compiled|one annotation|purity check; shape inference|new IR; new compilation pass|nothing — `@trace` is opt-in|JAX (`jit` + XLA)|
|E: do nothing|operators stay hardcoded, no GPU, no user overloading|nothing|nothing|nothing|nothing|C (fixed set), Kotlin (fixed set)|

## Decision (2026-08-28)

**Operators: design A in principle, on demand. GPU: design E, and C stays priced.** Operator protocols are the survey's own conclusion — a protocol with syntax, static and monomorphized exactly as RFC-0084 built, none of Python's three costs — so the direction is fixed: when generic numeric code wants `+` on a user type, the work is rewiring the parser's operator table onto protocol rows, and nothing about it is open-ended. It is not implemented today because no program in the tree defines a numeric type that wants an operator. The GPU half stays closed: a third backend at a third of a backend's price (RFC-0101's own estimate) against zero kernels anyone has asked to run, and HVM4 remains pre-launch with unpublished numbers, as the census measured. Design B (broadcast fusion) would follow operators, not precede them. Reopen the GPU question only with a workload in hand.
