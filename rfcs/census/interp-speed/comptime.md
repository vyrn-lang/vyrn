# Compile-time execution: what five languages did about a slow interpreter

Research date: 2026-08-24. Every claim below carries a citation. Where I could not find
evidence, the line says `NOT FOUND`.

Source files quoted by line number were downloaded to
`.../scratchpad/research/src/` on 2026-08-24 from the `master`/`devel`/`main` branch of
each project. Line numbers are for that snapshot.

---

## 1. Zig `comptime`

### How it runs today

Zig has no dedicated comptime interpreter. Semantic analysis and comptime execution are
the same pass, in one file. `src/Sema.zig` lines 1 to 6:

> Semantic analysis of ZIR instructions. [...] Does type checking, comptime control flow,
> and safety-check generation. This is the the heart of the Zig compiler.

Source: <https://github.com/ziglang/zig/blob/master/src/Sema.zig> (37,745 lines at the
snapshot). Values live in the `InternPool`. Comptime-mutable memory has a separate
representation, `MutableValue`, added 2024-03-25 in commit `5ec6e3036`, part of PR #19437
(<https://github.com/ziglang/zig/pull/19437>).

### Was it rewritten?

The C++ implementation (stage1) was replaced by the self-hosted `Sema.zig`. Inside
`Sema.zig` the call path was rewritten again in PR #22414, merged 2025-01-10, +798/−1177
across 37 files (<https://github.com/ziglang/zig/pull/22414>). That PR is about
correctness and code size. Its author states the speed effect nowhere; it changes eval
branch quota accounting as a side effect and calls that a breaking change.

I found no Zig change that replaced the evaluator with a bytecode VM. NOT FOUND.

### `@setEvalBranchQuota` and the cost model

`Sema.zig:193`:

```zig
pub const default_branch_quota = 1000;
```

`Sema.zig:77-78` holds `branch_quota` and `branch_count` on the `Sema` itself.
`Sema.zig:5428-5429` shows the builtin only ever raises the number:

```zig
sema.branch_quota = @max(sema.branch_quota, quota);
```

The count moves in exactly one place, `emitBackwardBranch`, `Sema.zig:26724-26739`:

```zig
sema.branch_count += 1;
if (sema.branch_count > sema.branch_quota) { ... "evaluation exceeded {d} backwards branches" ... }
```

The langref (`doc/langref.html.in:5329-5338`) says the builtin raises "the maximum number
of backwards branches that compile-time code execution can use", and that a value smaller
than 1000 is ignored.

**What this tells you about the cost model.** The quota counts loop iterations and inline
calls. It does not count instructions, allocations, bytes, or time. It is a guard against
a program that never stops. It is not a budget for a program that is merely slow. A user
who hits it raises it and the compile then takes as long as it takes. Zig therefore has no
mechanism that limits the cost of comptime — only one that limits its non-termination.

### Published work on making comptime faster

Issue #4055, "improve comptime performance to roughly, generally the same as CPython
execution speed of equivalent Python code", opened 2020-01-03, **still open** on
2026-08-24, milestone `upcoming`, labels `enhancement, optimization, frontend`.
<https://github.com/ziglang/zig/issues/4055>

The opening report measures a comptime `std.sort` of 10,000 elements: 75 s and 3 GB, then
failure at 100,000 backwards branches. 1,000 elements: 3 s and 500 MB.

Andrew Kelley, 2023-07-23, in that issue, on a comptime loop of 100,000,000 iterations:

> To summarize, it's 189s in zig comptime vs 6s in CPython, and the zig compiler used 14
> GiB of memory.
>
> This is due to the way comptime is currently implemented in zig, and we'll need to make
> some fundamental changes to the compiler in order to address it. For starters, we need to
> model comptime-mutable memory as actual mutable memory, rather than tossing everything
> into the garbage-collected InternPool.

That named fix landed: `MutableValue` (commit `5ec6e3036`, 2024-03-25). I found no
published before/after measurement for it. NOT FOUND.

Issue #20163, "Compiletime evaluation is really slow" (2024-06-02): 56 m 20 s to scan a
9 MB embedded file at comptime, against 2.187 s for the same loop at run time. It was
closed the same day as a duplicate of #4055.
<https://github.com/ziglang/zig/issues/20163>

**The Zig answer is (a), fix the value representation, and it is not finished.** The
canonical issue has been open for six and a half years.

---

## 2. Nim's compile-time VM and `vmops`

I found the source. `compiler/vmops.nim`, 426 lines.
<https://github.com/nim-lang/Nim/blob/devel/compiler/vmops.nim>

### What it actually is

**It is not a second implementation of the standard library.** It is a name-to-function
binding table. `vmops.nim` line 12 onward imports the real standard library modules into
the compiler:

```nim
from std/math import sqrt, ln, log10, log2, exp, round, arccos, arcsin, ...
from std/envvars import getEnv, existsEnv, delEnv, putEnv, envPairs
from std/private/oscommon import dirExists, fileExists
from std/hashes import hash
```

Then a template pair converts each into a VM callback. `vmops.nim:48-49` and `74-77`:

```nim
template mathop(op) {.dirty.} =
  registerCallback(c, "stdlib.math." & astToStr(op), `op Wrapper`)

template wrap1fMath(op) {.dirty.} =
  proc `op Wrapper`(a: VmArgs) {.nimcall.} =
    doAssert a.numArgs == 1
    setResult(a, op(getFloat(a, 0)))
  mathop op
```

The wrapper unpacks VM arguments, calls the identifier `op` — which is the standard
library proc, already compiled into the compiler binary — and packs the result back. The
body is not duplicated anywhere.

Registration and lookup. `compiler/vmdef.nim:328-331`:

```nim
proc registerCallback*(c: PCtx; name: string; callback: VmCallback): int {.discardable.} =
  result = c.callbacks.len
  c.callbacks.add(callback)
  c.callbackIndex[reverseName(name)] = result
```

`compiler/vmgen.nim:2229-2238` builds the key from the symbol's owner chain and looks it
up. `vmgen.nim:2259-2260` shows a callback wins over every other treatment:

```nim
if procIsCallback(c, s): discard
elif importcCond(c, s): c.importcSym(n.info, s)
```

### Why those procs and not others

The reason is in `vmgen.nim:1765-1770`:

```nim
proc importcCond*(c: PCtx; s: PSym): bool {.inline.} =
  ## return true to importc `s`, false to execute its body instead (refs #8405)
  result = false
  if sfImportc in s.flags:
    if s.kind in routineKinds:
      return isEmptyBody(getBody(c.graph, s))
```

A proc marked `importc` with an **empty body** has no Nim code for the VM to run. Check
`lib/pure/math.nim:368`:

```nim
func sqrt*(x: float32): float32 {.importc: "sqrtf", header: "<math.h>".}
```

There is no body. The VM cannot interpret it. Without a callback, the compile fails.
`vmops.nim` exists to make such procs work at all, not to make working procs faster.

The same shape applies to the rest of the table:

- **Environment and file system** (`getEnv`, `putEnv`, `dirExists`, `fileExists`,
  `readFile`, `writeFile`, `readLines`, `walkDir`, `createDir`, `getCurrentDir`): the VM
  has no operating system. Several are gated behind `--experimental:vmopsDanger`
  (`wrapDangerous1svoid` / `wrapDangerous2svoid`, `vmops.nim:117-136`), and under
  `nimsuggest` or `nim check` the dangerous ones become no-ops.
- **Compiler introspection** (`getProjectPath`, `getCurrentCompilerExe`, `querySetting`,
  `symBodyHash`, `isExported`, `macrocache.hasKey`, `effecttraits.getRaisesListImpl`,
  `typetraits.hasClosureImpl`): the answer only exists inside the compiler.
- **`gorgeEx`, `execCmdEx`, `cpuTime`, `getTimeImpl`**: side effects the VM has no way to
  produce. `cpuTime` returns the constant `5.391245e-44` unless benchmarking is on
  (`vmops.nim:344-347`).
- **Float formatting and marshalling** (`addFloatRoundtrip`, `addFloatSprintf`,
  `formatBiggestFloat`, `marshal.toVM`, `marshal.loadVM`).

I found **no** comment in `vmops.nim` giving speed as the reason for any entry.
The commit that started the file, 2014-08-30, is titled "VM supports math and a few os
procs" — a capability claim, not a speed claim.

### The one entry that was a speed fast path, and what happened to it

`hashVmImpl` is different. `lib/pure/hashes.nim:386-397` declares four stubs with no real
body at all:

```nim
proc hashVmImpl(x: string, sPos, ePos: int): Hash =
  raiseAssert "implementation override in compiler/vmops.nim"
```

`vmops.nim:317-335` registers native implementations for them. The library then chose
between the fast native path and the interpreted one with `when nimvm:`.

**Every one of those call sites is commented out today.** `hashes.nim:533-536`:

```nim
    #when nimvm:
    #  result = hashVmImpl(x, 0, high(x))
    when true:
      result = murmurHash(toOpenArrayByte(x, 0, high(x)))
```

Five sites, disabled in commit `4faa15f3a` (2024-07-07), PR #23793, whose subject is a
hash algorithm change, not a VM change:
<https://github.com/nim-lang/Nim/pull/23793>. The registration in `vmops.nim` still
exists and is now unreachable from the library. The fast path went stale when the
function beside it changed.

### Nim's cost model and its real array fix

`compiler/options.nim:713`: `maxLoopIterationsVM: 10_000_000`. `compiler/vm.nim:472-479`
counts down and calls `globalError` with "too many iterations".

Issue #19075, "Very slow compiling when computing big constant arrays" (2021-10-31): a
50,000-element array copy at compile time took **237 seconds**.
<https://github.com/nim-lang/Nim/issues/19075>

The fix, PR #21318, merged 2023-01-31, "put big arrays on the constant seqs; don't inline
them in the VM; big performance boost". Two files, +24/−2. The whole change to the VM is
four lines in `vmgen.nim`:

```nim
    of skConst:
      let constVal = if s.astdef != nil: s.astdef else: s.typ.n
-     gen(c, constVal, dest)
+     if dontInlineConstant(n, constVal):
+       genLit(c, constVal, dest)
+     else:
+       gen(c, constVal, dest)
```

The other file added is a regression test carrying `timeout: 10`.
<https://github.com/nim-lang/Nim/pull/21318>

A four-line change to how a value is represented removed 237 seconds. No library function
was touched.

---

## 3. Rust `const` evaluation

### How intrinsics are handled

`copy_nonoverlapping` has no Rust body. It is an intrinsic. The interpreter must implement
it or refuse it. `compiler/rustc_const_eval/src/interpret/intrinsics.rs:1067-1091`
resolves the arguments, checks size, alignment and provenance, then calls `mem_copy`.

`compiler/rustc_const_eval/src/interpret/memory.rs:1596-1618` is the native path:

```rust
let size_in_bytes = size.bytes_usize();
// For particularly large arrays (where this is perf-sensitive) it's common that
// we're writing a single byte repeatedly. So, optimize that case to a memset.
if size_in_bytes == 1 {
    let value = *src_bytes;
    dest_bytes.write_bytes(value, (size * num_copies).bytes_usize());
} else if src_alloc_id == dest_alloc_id {
    ... ptr::copy(src_bytes, dest_ptr, size_in_bytes); ...
} else {
    ... ptr::copy_nonoverlapping(src_bytes, dest_ptr, size_in_bytes); ...
}
```

**Yes, there is a fast path, and the comment says it is there for speed.** There is a
second one just above it: if the source range is entirely uninitialised, the copy is
skipped and the destination is marked uninitialised, "so that the backing allocation is
never touched" (`memory.rs:1573-1583`).

`write_bytes` and `compare_bytes` get the same treatment
(`intrinsics.rs:1128-1170`); `compare_bytes` reads both ranges and calls `Ord::cmp` on two
Rust slices.

The important structural point: `Vec::extend_from_slice`, `copy_from_slice`,
`slice::to_vec` and the rest all bottom out in `copy_nonoverlapping`. One intrinsic
implemented natively covers the whole library above it. Rust reimplemented **no library
function** to get this.

### Cost model

`compiler/rustc_const_eval/src/const_eval/machine.rs:33-41`:

```rust
/// When hitting this many interpreted terminators we emit a deny by default lint
/// that notfies the user that their constant takes a long time to evaluate.
const LINT_TERMINATOR_LIMIT: usize = 2_000_000;
const TINY_LINT_TERMINATOR_LIMIT: usize = 20;
/// After this many interpreted terminators, we start emitting progress indicators at every
/// power of two of interpreted terminators.
const PROGRESS_INDICATOR_START: usize = 4_000_000;
```

`increment_const_eval_counter` (`machine.rs:927`) fires the `LONG_RUNNING_CONST_EVAL`
lint, which the user can allow. There is no hard cap. Rust chose a *warning* where Zig
chose an *error*.

### Measured performance work on const-eval

NOT FOUND. I could not locate a rustc perf run or PR with before/after numbers attributed
to const-eval interpreter speed specifically. The memset fast path exists in the source
with a performance comment, but I could not find the commit or its measurement.

---

## 4. D's CTFE

### The cause, from the people who worked on it

Stefan Koch, quoted in "Project Highlight: The New CTFE Engine", The D Blog, 2016-11-18
(<https://blog.dlang.org/archive/2016/11/18/project-highlight-the-new-ctfe-engine/>):

> The current interpreter interprets every AST-Node it sees directly. This leaves very
> little space to collect information about the code that is being interpreted. It doesn't
> know when something will be used as a reference, so it needs to copy every variable on
> every mutation. It has to do a deep-copy for this. That means it copies the whole chain
> of mutations every time.

And on trying to patch it:

> That flaw looked unfixable. Indeed the whole architecture in dinterpret.d is very
> convoluted and hard to understand. I did a few experiments on improving memory-management
> of the interpreter but it proved fruitless.

His own post, "The New CTFE Engine", The D Blog, 2017-04-10
(<https://blog.dlang.org/archive/2017/04/10/the-new-ctfe-engine/>):

> It's an AST interpreter, which means that it interprets the AST while traversing it. To
> represent the result of interpreted expressions, it uses DMD's AST node classes. This
> means that every expression encountered will allocate one or more AST nodes. Within a
> tight loop, the interpreter can easily generate over 100_000_000 nodes and eat a few
> gigabytes of RAM.

He cites DMD issue 12844 (`std.regex` CTFE taking more than 16 GB for one pattern) and
issue 6498 (a 0-to-10,000,000 loop running out of memory).

The root cause was named by Don Clugston in 2011. DMD issue 17528, "[CTFE] copy-on-write
is slow and causes huge memory usage", opened **2011-08-15**, **still open** on
2026-08-24: <https://github.com/dlang/dmd/issues/17528>

> This is the main reason why CTFE is so slow.

### The newCTFE project

It existed. Timeline from Koch's own post:

- 2016-05-09: announcement of the plan.
- 2016-05-28: "Simple memory management change failed."
- 2016-06-03: "Decision to implement a bytecode interpreter."
- 2016-06-30: first integer arithmetic runs.
- 2017-04: after eleven months, function calls and more complex cases still did not work.

Koch's own assessment of the cost:

> compiling code to a virtual ISA is exactly as much work as compiling it to a real ISA

**It was never merged.** The branch `devel/newCTFE` still exists in `dlang/dmd`. Its last
commit is `4a7569647`, 2017-08-24, "comment out a debugline I missed".

**DMD still uses the AST interpreter.** `compiler/src/dmd/dinterpret.d` is 7,666 lines and
is actively maintained — commits on 2026-08-09, 2026-08-12 and 2026-08-14. The deep copy
Don named in 2011 is still there: `copyLiteral(...).copy()` appears at lines 557, 1042,
1846, 2559, 2575, 2611, 2699, 2794, 2816, 2946, 3562, 3865, 3911, 4118, 4137, 4244, 4915
and more.

### D's own `vmops` equivalent

`compiler/src/dmd/builtin.d`, 478 lines. Header:

> Implement CTFE for intrinsic (builtin) functions.
> Currently includes functions from `std.math`, `core.math` and `core.bitop`.

`determine_builtin` (`builtin.d:80-100`) refuses anything outside `core.math`,
`core.bitop`, `core.builtins` and `std.math`, then matches on identifier: `sqrt`, `sin`,
`cos`, `tan`, `exp`, `log`, `floor`, `ceil`, `round`, `fabs`, `fmin`, `fmax`, `fma`,
`copysign`, `isnan`, `bsf`, `bsr`, `bswap`, `popcnt`, `ctfeWrite`. Every one of these maps
to a hardware or C intrinsic with no D body to interpret. **No string, array or copy
routine is in the list.** D has never used this table to make working D code faster.

---

## 5. C++ `constexpr`

### Clang

Two evaluators exist in the same compiler.

The default is a tree walker: `clang/lib/AST/ExprConstant.cpp`, **23,346 lines**. It
special-cases library-visible builtins natively. `ExprConstant.cpp:10841-10870` handles
`__builtin_memcpy`, `__builtin_memmove`, `__builtin_wmemcpy`, `__builtin_wmemmove`.
`ExprConstant.cpp:17873-17881` handles `__builtin_strlen` via `EvaluateBuiltinStrLen`.
`ExprConstant.cpp:17883-17900` handles `__builtin_strcmp`, `__builtin_memcmp` and family.

The second is a real bytecode interpreter, `clang/lib/AST/ByteCode/` — `Compiler.cpp`,
`ByteCodeEmitter.cpp`, `Interp.cpp`, `Opcodes.td`, `InterpStack.cpp`, and
`InterpBuiltin.cpp` at 7,136 lines for its own native builtin set. It is enabled with
`-fexperimental-new-constant-interpreter` and **is not the default**.

Measured, from Timm Bäder, "Clang bytecode interpreter update", Red Hat Developer,
2025-10-15
(<https://developers.redhat.com/articles/2025/10/15/clang-bytecode-interpreter-update>):

| workload | tree evaluator | bytecode interpreter | GCC 14.3.1 |
|---|---|---|---|
| `#embed` of a 9 MB file | 36.49 s | 14.80 s | — |
| 10,000 heap allocations | 27.79 s | 4.24 s | 43.83 s |
| 1 MB array allocation | 1048 s | 440 ms | 117 ms |

And the counter-result, in the same article: for array initialisation with no function
calls, "the bytecode interpreter is slower", because of its extra overhead. Clang test
failures fell from 155 to 90; libc++ failures from over 750 to zero. The project began in
2019 (Red Hat Developer, 2024-10-21,
<https://developers.redhat.com/articles/2024/10/21/new-constant-expression-interpreter-clang>)
and is still not default in 2026.

Cost model: `clang/include/clang/Basic/LangOptions.def:389-392`.

```
LANGOPT(ConstexprCallDepth, 32, 512, Benign, "maximum constexpr call depth")
LANGOPT(ConstexprStepLimit, 32, 1048576, Benign, "maximum constexpr evaluation steps")
```

### GCC

`gcc/cp/constexpr.cc`, **13,418 lines**, a tree walker over the C++ front end's own tree
nodes. Limits, from `gcc/c-family/c.opt:1891-1910`:

```
-fconstexpr-depth=      Init(512)
-fconstexpr-cache-depth= Init(8)
-fconstexpr-loop-limit= Init(262144)
-fconstexpr-ops-limit=  Init(33554432)
```

Enforced at `constexpr.cc:3105` (depth), `9024` (loop), `9373` (ops).

### The known cost cliff, and where the library fixes it

The library, not the compiler, decides whether a copy is one call or a loop. libc++
`include/__string/constexpr_c_functions.h:205-233`:

```cpp
__constexpr_memmove(_Tp* __dest, _Up* __src, __element_count __n) {
  size_t __count = static_cast<size_t>(__n);
  if (__libcpp_is_constant_evaluated()) {
#ifdef _LIBCPP_COMPILER_CLANG_BASED
    if _LIBCPP_CONSTEXPR (is_same<...>::value) {
      ::__builtin_memmove(__dest, __src, __count * sizeof(_Tp));
      return __dest;
    } else
#endif
    {
      ... for (size_t __i = 0; __i != __count; ++__i)
            std::__assign_trivially_copyable(__dest[__i], __src[__i]);
    }
  } else ...
```

Three cases: constant-evaluated on Clang gets one `__builtin_memmove`; constant-evaluated
on GCC gets a hand-written element loop the evaluator must step through; run time gets
`__builtin_memmove`. Same for `__constexpr_strlen`
(`constexpr_c_functions.h:48-63`), which carries the reason for GCC's slow path in a
comment:

> GCC currently doesn't support `__builtin_strlen` for heap-allocated memory during
> constant evaluation. <https://gcc.gnu.org/bugzilla/show_bug.cgi?id=70816>

**That interpreted fallback loop is a second implementation of the same function.** It
exists only because one compiler's evaluator cannot take the fast path. The library pays
for it in maintenance every time either branch changes.

### MSVC

NOT FOUND. I found no public description of how MSVC evaluates `constexpr`.

---

## 6. The shared lesson

Judged by what the projects **did**, not by what they wrote in documentation:

**(a) Make the interpreter faster — this is what actually shipped and actually worked.**

- Nim: PR #21318, four lines in `vmgen.nim`, 237 s removed. A representation change.
- Zig: `MutableValue` (commit `5ec6e3036`), the exact change Kelley named as needed, "model
  comptime-mutable memory as actual mutable memory".
- Clang: a bytecode interpreter, measured at 2.4x on `#embed`, 6.6x on heap allocation,
  2400x on the 1 MB array case.
- Rust: the memset and all-uninit fast paths in `mem_copy_repeatedly`.

**(b) Native fast paths for hot library functions — nobody did this for speed.**

Every table I found — Nim `vmops.nim`, D `builtin.d`, Clang's builtin cases, Rust's
intrinsics — exists because the function has **no interpretable body**. Nim's own
`importcCond` states the rule in a comment: importc the symbol only when its body is
empty, otherwise "execute its body instead". D's `builtin.d` covers only `core.math`,
`core.bitop` and `std.math`. No project put a string, array or copy routine in such a table
to make working code faster.

The one entry in any of these tables that was a genuine speed fast path — Nim's
`hashVmImpl` — is dead code today. It was disabled by a change to the hash algorithm
beside it, in a pull request that was not about the VM at all.

**(c) Cache aggressively — universal, and every project has had to narrow the key.** See
section 7.

**(d) Tell users to do less — not a stated policy anywhere, but the observed outcome.**
Zig #4055 has been open since 2020 with the goal set at *CPython speed*. D #17528 has been
open since 2011. Both projects' users work around the problem.

**(e) Something else — rewriting the whole engine as a bytecode VM. Two attempts, one
finished, one not.** D's newCTFE ran fifteen months, reached about 12,000 lines, was never
merged, and its branch has not moved since 2017-08-24. Clang's bytecode interpreter started
in 2019, works, is measurably faster on most workloads, is measurably **slower** on array
initialisation without calls, and after seven years is still behind an experimental flag.

**The honest summary:** the fixes that landed and paid were changes to how values are
represented and copied inside the evaluator. The fixes that stalled were whole-engine
rewrites. The native-function tables were never a speed measure in any of these projects.

---

## 7. Caching

### What each project does

| Project | Between builds? | Key |
|---|---|---|
| Rust | **Yes** | `eval_to_allocation_raw`, `eval_static_initializer` and `eval_to_const_value_raw` are all marked `cache_on_disk` (`rustc_middle/src/queries.rs:1329-1370`). Key is a `GlobalId` (item plus generic arguments). Correctness comes from the incremental dependency graph, not the key. |
| Zig | Within one compilation only | `memoized_call` interned in the `InternPool`, keyed on `{func, arg_values}` (`Sema.zig:7772-7793`). **Disabled entirely when incremental compilation is on.** |
| GCC | Within one translation unit only | `constexpr_call_table` (`constexpr.cc:1544`), keyed on `{fundef, bindings}` plus a third axis. Reset to `NULL` at `constexpr.cc:13414`. |
| Clang | NOT FOUND | I found no constexpr result cache in `ExprConstant.cpp`. |
| Nim | NOT FOUND for VM results | `--incremental:on` exists (`compiler/commands.nim:1090-1097`). I found no evidence that it caches VM evaluation results across builds. |
| D | NOT FOUND | No memoisation table in `dinterpret.d`. |

### Prior art on what belongs in the key

**Zig — three separate corrections, all because the key was too narrow.**

1. The original commit, `f378b0adc`, 2021-08-21, admits its own gap in the message:

   > It is keyed on the `*Fn` and the comptime arguments, but it does not yet properly
   > detect comptime function pointers and avoid memoizing in this case. So it will have
   > false positives for when a comptime function call mutates data through a pointer
   > parameter.

   The current guard is `val.canMutateComptimeVarState(zcu)` (`Sema.zig:7762-7764`): if any
   argument can reach mutable comptime state, do not memoise.

2. Commit `9cf8a7661`, 2024-08-19, "compiler: handle eval branch quota in memoized calls":

   > In a `memoized_call`, store how many backwards braches the call performs. Add this to
   > `sema.branch_count` when using a memoized call. If this exceeds the quota, perform a
   > non-memoized call to get a correct "exceeded X backwards branches" error.
   >
   > Also, do not memoize calls which do `@setEvalBranchQuota` or similar, as this affects
   > global state which must apply to the caller.

   A cached call had been *free*, so raising the cache hit rate silently changed which
   programs compiled. The fix stores the branch count **in the value** and charges it to
   the caller. The `branch_count` field is explicitly excluded from hash and equality
   (`Sema.zig:7777-7779`, `.branch_count = undefined, // ignored by hash+eql`).

3. Incremental compilation. `Sema.zig:7756-7761`:

   ```zig
   // TODO: comptime call memoization is currently not supported under incremental compilation
   // since dependencies are not marked on callers. If we want to keep this around (we should
   // check that it's worthwhile first!), each memoized call needs an `AnalUnit`.
   if (zcu.comp.config.incremental) break :m false;
   ```

   The key holds the function and the arguments. It does not hold what the call *read*. Under
   incremental compilation that is unsound, so Zig turned memoisation off — and the comment
   questions whether it was ever worth having.

**GCC — an extra key dimension and seven disqualifiers.**

The key gained a third axis. `constexpr.cc:1138-1157`:

```c
struct GTY((for_user)) constexpr_call {
  constexpr_fundef *fundef = nullptr;
  tree bindings = NULL_TREE;
  /* Result of the call, indexed by the value of
     constexpr_ctx::manifestly_const_eval.
       unknown_type_node means the call is being evaluated.
       error_mark_node means that the evaluation was erroneous or otherwise
       uncacheable (e.g. because it depends on the caller).  */
  tree results[3] = { NULL_TREE, NULL_TREE, NULL_TREE };
```

The same function with the same arguments can return **three different answers**, depending
on whether the context is manifestly constant-evaluated. `std::is_constant_evaluated()`
made the old two-part key wrong.

Beyond that, GCC sets `cacheable = false` in seven distinct places
(`constexpr.cc:4665-4783`): an exception was thrown; `ctx->global->state_dependent`;
heap allocations and deallocations did not balance; the result required rewriting
`RESULT_DECL`; a jump target escaped; the result is not a reduced constant expression;
a contract statement fired. Constructors are excluded separately, with the comment
"there's no need to put such a call in the hash table."

Every one of those is the same lesson: **the result was not a pure function of the key, so
either widen the key or refuse to cache.** GCC chose to refuse in six of the seven cases.

**Rust — the key stays small and the dependency graph does the work.** The key is
`GlobalId` — item plus generic arguments. Nothing about the environment is in it. Validity
comes from rustc's red-green dependency tracking, which records what the query actually
read and invalidates on that. This is the only design among the five that survives across
builds.

### What goes wrong when the key is too narrow

Documented in the sources above, in this order of severity:

1. **A stale result is served and the build is silently wrong.** Zig's original pointer-
   mutation false positive; GCC's `state_dependent` and `manifestly_const_eval` cases.
2. **A limit stops being enforced.** Zig's branch quota: a cached call cost nothing, so the
   same program compiled or failed depending on cache state.
3. **The cache must be turned off wholesale.** Zig under incremental compilation.
4. **The fast path drifts out of use and nobody notices.** Nim's `hashVmImpl`.

---

## What this says for Vyrn

**RECOMMENDATION, NOT A DECISION.**

### On the constraint: is an interpreter-only fast path the same as implementing a standard library function inside a backend?

**It depends entirely on whether a second body of code exists. The evidence separates the
two cases cleanly.**

**Not the same thing — a *binding*.** Nim's `vmops.nim` writes no arithmetic. Line 12
imports the real `std/math` into the compiler; `wrap1fMath` unpacks arguments, calls the
imported proc, packs the result. One definition of `sqrt` exists in the project. If it
changes, the VM path changes with it, because it *is* that path. The same holds for Rust:
`copy_nonoverlapping` is an intrinsic with no Rust body anywhere, so the interpreter's
implementation is the *only* implementation for const context, and every library routine
above it inherits the fast path for free.

**The same thing — a *reimplementation*.** Nim's `hashVmImpl` and libc++'s
`__constexpr_memmove` fallback loop are second bodies of code for functions that already
had one. Both carry the cost the owner's rule is guarding against, and Nim's case shows the
failure mode in the record: the fast path is registered in `vmops.nim` today and every call
site in `hashes.nim` is commented out, disabled by a commit about a hash algorithm. Nobody
removed the dead registration.

So: **an interpreter fast path that calls the compiler's own single copy of the routine is
not what the rule forbids. An interpreter fast path that restates the routine's behaviour
in Rust is exactly what the rule forbids, and it rots — that is measured, not predicted.**

For a Vyrn `gen fn` running Vyrn source, this distinction has bite. The hot byte-copy loops
are written in Vyrn. There is no second copy to bind to. A Rust fast path for them would be
a reimplementation, in the forbidden sense, unless the loop is first replaced by a
*builtin* — a name with no Vyrn body — as `slice` already is. That is the Rust and D shape:
give the operation no interpretable body, then implement it once.

### What the evidence says to do first

1. **Look at the value representation before the fast path.** This is the only measure with
   a track record. Nim removed 237 seconds with four lines and no library change. Zig's
   named fix was mutable memory, not native functions. D's cause is deep copy on every
   mutation. Vyrn's own number agrees: removing hash lookups from variable reads gave
   46.8 s → 31.1 s, about 33%, with no library function touched. Check whether Vyrn's byte
   loops copy a string or array per iteration, or per read of a module-level constant. Nim's
   fix was exactly that.

2. **Note the ceiling before spending on fast paths.** 13.7% plus 7% is 20.7% of
   generation. A perfect native replacement for both functions removes at most that much.
   The measured representation fix already removed 33% of a longer workload.

3. **If a fast path is warranted, make the function a builtin, not an override.** Follow
   Rust: one operation with no source-level body, implemented once, with the library above
   it routing through it. Do not follow Nim's `hashVmImpl`: a Vyrn body plus a Rust body
   for the same function, chosen by a `when` — that is the pattern that died.

4. **The generator cache is already stronger than most of the prior art.** From
   `compiler/vyrn-frontend/src/loader.rs:1652-1720`, the key is module key, generator name,
   argument representation and allowed input roots; the entry records a per-input hash list
   re-checked at hit time, with an `ABSENT` sentinel and a check that the entry records the
   generator's own module. That is closer to Rust's dependency-graph validation than to
   Zig's `{func, args}`. Zig's own comment says a `{func, args}` key cannot survive
   incremental compilation.

   The gap worth auditing, on GCC's and Zig's evidence, is the **disqualifier list**: what
   makes a generation *uncacheable* rather than merely keyed. GCC refuses to cache in seven
   situations; Zig refuses when an argument can reach mutable comptime state, and when the
   call changes global evaluation state. If a Vyrn generator can observe or change anything
   not in the recorded inputs — a limit, a counter, an ambient setting, a diagnostic count —
   that generation must not be cached, regardless of key. Zig learned this specific lesson
   from its branch quota three years after shipping memoisation.

5. **Add a timed regression test to whatever you fix.** Nim's fix shipped with a test
   carrying `timeout: 10`. That is what stops the 237 seconds coming back.

### What the evidence says not to do

Do not rewrite the interpreter as a new engine on the strength of the 13.7% figure. D spent
fifteen months and about 12,000 lines and merged nothing. Clang spent seven years, produced
a working and mostly faster interpreter, and it is still behind a flag and still slower on
one measured workload — array initialisation without function calls, which is close in
shape to Vyrn's byte-copy loops.
