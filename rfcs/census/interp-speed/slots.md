# Resolving variable references to slots: what shipping implementations do

Research for Vyrn. Read-only. Nothing in `N:/wt-bytefix` or `N:/lang` was changed.

Sources fetched 2026-08-24. Source files quoted by upstream path and by the line
number in the copy fetched that day from the project's default branch. Local
copies sit in `scratchpad/research/src/`.

---

## 1. How shipping implementations do it

| Implementation | What the resolved reference is | Where resolution happens |
|---|---|---|
| jlox (Crafting Interpreters, ch. 11) | A hop count only. The name is still looked up in a map at that depth | Separate resolver pass over the AST, after parse, before run |
| clox (ch. 22, 25) | A single byte: an index into the call frame's stack window. Flat, one window per function | The single-pass compiler, as it emits bytecode |
| CPython | An index into the frame's `localsplus` array. Cells for closures get their own index in the same array | `Python/symtable.c` decides the scope class; `Python/compile.c` and `Python/codegen.c` pick the opcode |
| Lua 5.4 | A register number inside the function's activation record | `lparser.c`, at parse time, in the same pass that emits code |
| Wren | An index into a flat `locals` array per function, matching the stack slot | `wren_compiler.c`, single pass |
| Ruby (YARV) | A pair: `(idx, level)`. `level` counts iseq nesting, not block nesting | `compile.c`, `get_dyna_var_idx` |
| starlark-rust | `LocalSlotId(u32)`, one flat frame per `def` | A dedicated scope-resolution pass, `eval/compiler/scope.rs` |
| Boa (JavaScript, Rust) | A "binding locator" built at compile time, held in the `CodeBlock` | A scope analyser pass |

### jlox — distance, not index

The resolver stores one number per variable expression: how many environments to
walk out. It does not store an index. The interpreter then looks the name up in
that environment's `HashMap`.

> "This walks a fixed number of hops up the parent chain and returns the
> environment there."
> — `book/resolving-and-binding.md:783`,
> https://craftinginterpreters.com/resolving-and-binding.html

The book leaves the index step as challenge 4:

> "Our resolver calculates *which* environment the variable is found in, but it's
> still looked up by name in that map. A more efficient environment
> representation would store local variables in an array and look them up by
> index."
> — `book/resolving-and-binding.md`, Challenges

So jlox is the halfway state, and the book says so.

### clox — a flat stack window per function

The compiler keeps `Local locals[UINT8_COUNT]`, a `localCount`, and a
`scopeDepth`. A local's index in that array is its stack slot.

> "In other words, the locals array in the compiler has the *exact* same layout
> as the VM's stack will have at runtime. The variable's index in the locals
> array is the same as its stack slot."
> — `book/local-variables.md:499`,
> https://craftinginterpreters.com/local-variables.html

`resolveLocal` scans the array backwards, so an inner declaration shadows an
outer one. At the end of a block the compiler decrements `localCount` and emits
`OP_POP` for each discarded local, so a later sibling block reuses the same
slots.

There is no separate resolver pass. The one compiler pass builds both pictures.

### CPython — `localsplus` index, decided by the symbol table

`Python/symtable.c` classifies every name as `LOCAL`, `GLOBAL_EXPLICIT`,
`GLOBAL_IMPLICIT`, `FREE` or `CELL`
(`Include/internal/pycore_symtable.h:187-191`). The code generator turns that
class into an opcode:

```c
    case LOCAL:
        if (_PyST_IsFunctionLike(c->u->u_ste)) {
            *optype = COMPILE_OP_FAST;
        }
```
— `Python/compile.c:1025-1028`, `_PyCompile_ResolveNameop`

`LOAD_FAST` then reads by index:

> "Pushes a reference to the local `co_varnames[var_num]` onto the stack."
> — https://docs.python.org/3/library/dis.html

**Why `LOAD_NAME` still exists.** The code above only picks `COMPILE_OP_FAST`
when the block is function-like. A module body and a class body are not
function-like, so their locals fall through to the default `COMPILE_OP_NAME`
(`Python/compile.c:1013`). Those namespaces are real dictionaries that user code
can read and write, so the index cannot be fixed at compile time. The language
reference states the class-body rule:

> "The namespace of the class definition becomes the attribute dictionary of the
> class."
> — https://docs.python.org/3/reference/executionmodel.html

`exec` and `eval` are the other reason a dictionary must stay reachable:

> "The `eval()` and `exec()` functions do not have access to the full environment
> for resolving names."
> — https://docs.python.org/3/reference/executionmodel.html

**Cellvars and freevars.** Since 3.11 cells live in the same indexed array as
plain locals. `MAKE_CELL(i)` "creates a new cell in slot `i`", `COPY_FREE_VARS(n)`
copies the closure's free variables into the frame, and `LOAD_DEREF(i)` "loads
the cell contained in slot `i` of the 'fast locals' storage". The docs record the
index change: "Changed in version 3.11: `i` is no longer offset by the length of
`co_varnames`." — https://docs.python.org/3/library/dis.html

### Lua — registers, allocated during the parse

Lua allocates a register to each local as the parser declares it:

```c
static void adjustlocalvars (LexState *ls, int nvars) {
  int reglevel = luaY_nvarstack(fs);
  for (i = 0; i < nvars; i++) {
    int vidx = fs->nactvar++;
    Vardesc *var = getlocalvardesc(fs, vidx);
    var->vd.ridx = cast_byte(reglevel++);
  }
}
```
— https://github.com/lua/lua/blob/master/lparser.c

`searchvar` scans the active variables backwards and calls `init_var`, which
records both the compiler index and the runtime register:

```c
static void init_var (FuncState *fs, expdesc *e, int vidx) {
  e->k = VLOCAL;
  e->u.var.vidx = cast_short(vidx);
  e->u.var.ridx = getlocalvardesc(fs, vidx)->vd.ridx;
}
```
— `lparser.c`

Upvalues are found by `singlevaraux`, which recurses into `fs->prev`, the
enclosing function's parse state, and calls `markupval` on the block that owns
the captured variable.

### Wren — flat array, 256 slots

```c
  Local locals[MAX_LOCALS];
  int numLocals;
```
— https://github.com/wren-lang/wren/blob/main/src/vm/wren_compiler.c

The comment on `MAX_LOCALS` says the limit "is the maximum number of variables in
scope at one time, and spans block scopes", and that "Since `CODE_LOAD_LOCAL` and
`CODE_STORE_LOCAL` use a single argument byte to identify the local, only 256 can
be in scope at one time." `resolveLocal` scans backwards; `findUpvalue` recurses
outward and flattens.

### Ruby YARV — a real `(index, level)` pair

```
getlocal
(lindex_t idx, rb_num_t level)
{
    val = *(vm_get_ep(GET_EP(), level) - idx);
}
```
— https://github.com/ruby/ruby/blob/master/insns.def:77-85

The compiler produces the pair:

```c
static int
get_dyna_var_idx(const rb_iseq_t *iseq, ID id, int *level, int *ls)
{
    while (iseq) {
        idx = get_dyna_var_idx_at_raw(iseq, id);
        if (idx >= 0) break;
        iseq = ISEQ_BODY(iseq)->parent_iseq;
        lv++;
    }
```
— https://github.com/ruby/ruby/blob/master/compile.c:1766-1786

`get_dyna_var_idx_at_raw` is a linear scan of `local_table` at compile time only.
`level` counts iseq boundaries — methods and blocks — not `if` or `while` bodies.
Ruby specialises the common depths in the instruction set: `defs/opt_operand.def`
declares `getlocal *, 0` and `getlocal *, 1`, which generate `getlocal_WC_0` and
`getlocal_WC_1`.

### starlark-rust — one flat frame per `def`

```rust
pub(crate) struct LocalSlotId(pub(crate) u32);
```
— https://github.com/facebook/starlark-rust/blob/main/starlark/src/eval/runtime/slots.rs:41

The scope pass holds, per function scope:

```rust
    /// Slots this scope uses, including for parameters and `parent`.
    /// Indexed by [`LocalSlotId`], values are variable names.
    pub used: Vec<FrozenStringValue>,
```
— `starlark/src/eval/compiler/scope.rs:137-140`

Slots are handed out by a counter, `LocalSlotIdCapturedOrNot(self.used.len())`
(`scope.rs:175-177`). The runtime frame carries `local_count` and
`max_stack_size` in one allocation: "`local_count` local slots followed by
`max_stack_size` stack slots" (`starlark/src/eval/bc/frame.rs:53`).

### Boa — binding locators

> "A binding locator contains all information about a binding that is needed to
> resolve it at runtime. Binding locators get created at compile time and are
> accessible at runtime via the CodeBlock."
> — https://github.com/boa-dev/boa/pull/1829

**Counter-example, and it ships.** Rhai, an embedded Rust language, keeps a
name-scanned `Scope` at runtime. The book says "A `Scope` is always searched in
reverse order", and the `Scope` type carries a const generic inline capacity of 8
entries. — https://rhai.rs/book/engine/scope.html,
https://docs.rs/rhai/latest/rhai/ . Rhai also exposes `Engine::on_var`, a
user-supplied variable resolver, which a static slot scheme could not offer.
— https://rhai.rs/book/engine/var.html

---

## 2. Keeping the static and dynamic pictures in sync

### The technique has a name: lexical addressing

SICP §5.5.6 names it and defines the pair.

> A lexical address is a "frame number, which specifies how many frames to pass
> over, and a displacement number, which specifies how many variables to pass
> over in that frame."

The compiler carries a *compile-time environment* that "keeps track of which
variables will be at which positions in which frames in the run-time environment
when a particular variable-access operation is executed". It is "a list of
frames, each containing a list of variables", threaded as an extra argument
through compilation.
— https://mitp-content-server.mit.edu/books/content/sectbyfn/books_pres_0/6515/sicp.zip/full-text/book/book-Z-H-35.html

The `(depth, index)` form is also called a de Bruijn index in the literature.

### What real implementations actually do: they remove the dynamic frames

clox, Lua and Wren do not keep a static picture and a dynamic picture in step.
They have **one** picture. A single pass allocates the slot and emits the code
that uses it, so the two cannot drift. Nystrom states the design condition:

> "This alignment obviously isn't coincidental. I designed Lox to be amenable to
> single-pass compilation to stack-based bytecode."
> — `book/local-variables.md:75`

and the property that makes it hold:

> "New locals are always created by declaration statements. Statements don't nest
> inside expressions, so there are never any temporaries on the stack when a
> statement begins executing. Blocks are strictly nested."
> — `book/local-variables.md:66-71`

Ruby and CPython take the other route: a scope only exists at a function-like
boundary. A Ruby `if` body creates no environment. A Python `for` body creates
no environment. Inner blocks contribute names to the one flat table.

### Where a two-pass design is used, the answer is: assert, and expect bugs

jlox is the only one of these that separates the resolver from the evaluator, and
the book warns about exactly the hazard you describe:

> "The interpreter code trusts that the resolver did its job and resolved the
> variable correctly. This implies a deep coupling between these two classes. In
> the resolver, each line of code that touches a scope must have its exact match
> in the interpreter for modifying an environment.
>
> I felt that coupling firsthand because as I wrote the code for the book, I ran
> into a couple of subtle bugs where the resolver and interpreter code were
> slightly out of sync. Tracking those down was difficult. One tool to make that
> easier is to have the interpreter explicitly assert -- using Java's assert
> statements or some other validation tool -- the contract it expects the
> resolver to have already upheld."
> — `book/resolving-and-binding.md:792-804`

That is the whole of the published advice: keep one flat frame, or assert.

starlark-rust asserts. The frame checks the slot against the frame size on the
public path in every build:

```rust
        assert!(slot.0 < self.frame().local_count);
```
— `starlark/src/eval/bc/frame.rs:107` and `:117`

and uses `debug_assert!` on the inner, unsafe paths (`frame.rs:226, 233, 243,
254, 264`). Note the direction of the check: it catches an index past the end of
the frame. It does **not** catch an index that lands on the wrong live variable.
No implementation surveyed catches that at runtime. NOT FOUND: any published
technique that detects a wrong-variable resolution at run time.

---

## 3. Flat frames versus nested

### What it buys

- One allocation per call, sized once. starlark-rust allocates `local_count`
  local slots and `max_stack_size` stack slots in a single object
  (`frame.rs:48-53`).
- One index, not a pair. clox spends a single byte and no outward walk.
- No probe of empty or near-empty frames, which is the cost Vyrn's counters
  measure at 1.30 probes per read.

### What it costs

- The frame is sized for the widest set of *simultaneously live* locals, not for
  the whole function. Both clox and Lua reclaim slots at block exit. clox
  decrements `localCount` at `endScope` ("We discard them by simply decrementing
  the length of the array", `book/local-variables.md`). Lua's `reglevel` scans
  back to the highest variable still in a register and returns the next free one
  (`lparser.c`). So sibling blocks reuse slots and the frame does not grow with
  total declaration count.
- A hard cap appears. Wren: 256 locals per function, because the operand is one
  byte. clox: `UINT8_COUNT`.
- Names stop being available at run time unless kept on the side. See §7.

### A loop body that rebinds a name each iteration

In clox the loop body is a block. Its locals are popped at the end of each
iteration and re-declared at the start of the next, into the same slots. One slot
serves every iteration. That is correct while nothing captures the variable.

### A closure that captures a loop variable

This is the sharp edge, and it is a language-semantics decision, not an
implementation detail.

clox emits `OP_CLOSE_UPVALUE` at block exit for each captured local:

> "Whenever the compiler reaches the end of a block, it discards all local
> variables in that block and emits an `OP_CLOSE_UPVALUE` for each local variable
> that was closed over."
> — `book/closures.md:1404-1406`

Because the close happens at the end of each iteration, each iteration's capture
gets its own heap box, and the shared slot is reused. If a language instead
closes only at function exit, every iteration's closure shares one variable. The
book's design note works the example, and JavaScript `var` gives the wrong answer
for most readers: "You may be surprised to hear that it prints '3' twice."
(`book/closures.md:1586`).

C# shipped a breaking change over this. The `foreach` variable became per
iteration in C# 5; the C-style `for` variable did not change.
— https://ericlippert.com/2009/11/12/closing-over-the-loop-variable-considered-harmful-part-one/ ,
https://ericlippert.com/2009/11/16/closing-over-the-loop-variable-considered-harmful-part-two/

---

## 4. Captures and closures once locals are indices

Three representations are in use.

1. **Open/closed upvalues (clox, Lua, Wren).** The closure holds pointers. While
   the variable is on the stack the upvalue points at the stack slot; at scope
   exit the value moves into the upvalue object and the pointer is redirected to
   it. "When a variable moves to the heap, we are *closing* the upvalue"
   (`book/closures.md`). This is needed because the closure can outlive the
   frame and can still *write* the variable.

2. **Boxed cells (CPython).** A captured local occupies a `localsplus` slot that
   holds a cell object. `MAKE_CELL(i)` builds it, `COPY_FREE_VARS(n)` copies the
   closure's cells into the callee frame, `LOAD_DEREF(i)` reads through it.
   — https://docs.python.org/3/library/dis.html

3. **Copy the value into a slot of the new frame (starlark-rust).** The compiler
   records a plain slot-to-slot copy:

   ```rust
   pub(crate) struct CopySlotFromParent {
       /// Slot in the outer function.
       pub(crate) parent: LocalSlotIdCapturedOrNot,
       /// Slot in the nested function.
       pub(crate) child: LocalSlotIdCapturedOrNot,
   }
   ```
   — `starlark/src/eval/compiler/def.rs:332-337`

   Starlark can do this because the language forbids assigning to a variable
   bound in an enclosing function. It has no `nonlocal`.
   — https://github.com/bazelbuild/starlark/blob/master/spec.md

**Which is simplest when captures are already by copy: option 3.** A capture
becomes a compile-time pair of slot numbers and a `Vec` copy at closure creation.
No cell object, no open/closed state, no close-at-scope-exit instruction, and the
loop-variable question of §3 does not arise, because each closure already owns its
own copy. Boxing (1 and 2) only earns its cost when a closure must observe a later
write, and by-copy capture means it must not.

---

## 5. What the change actually bought, measured

Numbers below are for named changes in named projects. Read the scope of each one
carefully — none of them is exactly "replace a short reverse linear scan over
names with a static index".

| Project | Change | Measured |
|---|---|---|
| Boa (JS in Rust) | Move binding lookup from runtime hashmaps to compile-time binding locators plus fixed-size vectors | Clean js execution 1487.4±7.50 µs → 987.3±3.78 µs (−33.62%). Mini js execution 1217.9±21.70 µs → 902.3±5.38 µs (−25.91%). Compile time rose (Clean js +112.53%, about 2 µs) |
| Cloudflare wirefilter | Replace a name-keyed `HashMap` in the execution context with a fixed array of `Option<LhsValue>` indexed by position | matching benchmark 2,548 ns/iter → 1,227 ns/iter |
| Zef (AST interpreter) | Optimisation #6: new object model where `Context`s "are created ahead of time as part of the AST resolve pass" and scopes allocate a `Storage` sized by the `Context`, **bundled with** inline caches and watchpoints | 4.55x faster, cumulative 1.5x → 6.8x |
| Zef | Optimisation #4: intern every name to a `Symbol*` and compare pointers instead of `std::string` | 18% faster |
| Nederlang (Rust) | `HashMap` → `Vec<Vec<(String, Object)>>` for variable storage | 32s → 24s |
| Nederlang | Split names and values into separate `Vec`s for locality | 24s → 21s |

Sources: https://github.com/boa-dev/boa/pull/1829 ;
https://blog.cloudflare.com/building-fast-interpreters-in-rust/ ;
https://zef-lang.dev/implementation ;
https://www.dannyvankooten.com/blog/2022/rewriting-interpreter-rust/

Three warnings about that table.

- Boa and Cloudflare both replaced **hashing**. Vyrn already removed hashing and
  measured that step at 46.8s → 31.0s. Their numbers are the number Vyrn has
  already collected, not the number still on the table.
- Zef's 4.55x is a bundle of three changes landed together. The author says so:
  "This change combines three changes into one". The slot part cannot be
  separated from the inline caches.
- Nederlang never moved to compile-time indices. It stopped at the reverse linear
  scan, which is where Vyrn is now, and its next win came from data layout, not
  from resolution.

**NOT FOUND: a published before/after measurement for adding compile-time slot
resolution, in isolation, on top of an already index-free reverse linear scan
over short frames.** Crafting Interpreters leaves it as an unmeasured challenge.
No project surveyed published the isolated figure.

---

## 6. Closure compilation

### What it is

Walk the AST once and build a tree of host-language closures. Each node's closure
captures whatever the node needs, already resolved. Running the program calls the
root closure.

> "the idea is to walk the tree only once, as if we were to compile it, but
> instead of producing a list of instructions, generate a chain of suspended
> function calls"
> — https://pl-rants.net/posts/compile-to-closures/

The original write-up is Marc Feeley and Guy Lapalme, "Using closures for code
generation", *Computer Languages* 12(1), 1987, pp. 47-66.
— https://doi.org/10.1016/0096-0551(87)90012-9 ,
http://www.iro.umontreal.ca/~feeley/papers/FeeleyLapalmeCL87.pdf . The abstract
states: "code generation is replaced by closure generation".

### Measured

- Cloudflare wirefilter, moving from direct AST interpretation to boxed closures:
  > "it showed an immediate ~10-15% runtime improvement in benchmarks and on real
  > examples."
  — https://blog.cloudflare.com/building-fast-interpreters-in-rust/
- RTypes in Elixir, tree-walking checker versus closure-compiled checker:
  roughly 2x on both a simple term (3.67 ms → 1.84 ms) and a complex term
  (2.54 ms → 1.21 ms).
  — https://pl-rants.net/posts/compile-to-closures/

### Does it subsume slot resolution?

**No. It is orthogonal, and the Cloudflare post is the evidence.** That project
did both, separately. The closure compilation gave 10-15%. The name-to-index
change in the execution context gave 2,548 → 1,227 ns/iter, and it was a distinct
change described in a distinct section of the same post.

Closure compilation removes the *dispatch* — the match on the node kind, and the
re-walk of the tree. It removes a name lookup only for the things the closure can
capture once at build time, which means constants, resolved function targets, and
anything whose location does not change per call. A local variable's location
still has to be described some way at closure-build time, and the natural way is
an index. In other words, closure compilation gives you a place to put the
resolution result; it does not tell you what the result is.

---

## 7. What goes wrong

### The static and dynamic pictures drift

Already quoted in §2: Nystrom "ran into a couple of subtle bugs where the
resolver and interpreter code were slightly out of sync. Tracking those down was
difficult." His rule is the operative one: "each line of code that touches a scope
must have its exact match in the interpreter".

### The name-keyed view and the indexed array drift

CPython carried this for years. PEP 667 describes the history plainly: the
namespaces "ceased being consistent" after the performance change, and "odd bugs
crept in over the years as threads, generators and coroutines were added". The
3.12 implementation built a dict on the fly from the array, and
`PyFrame_LocalsToFast()` wrote debugger changes back, "which can result in the
array and dictionary getting out of sync with each other". PEP 667 replaced the
dict with a write-through proxy in Python 3.13, and accepts a cost for it:
`len(proxy)` became O(n).
— https://peps.python.org/pep-0667/ , https://peps.python.org/pep-0558/

### Debuggers need the names back

Lua keeps names as debug information and loses them when it is stripped:

> "Variable names starting with '(' (open parenthesis) represent variables with no
> known names (internal variables such as loop control variables, and variables
> from chunks saved without debug information)."
> — https://www.lua.org/manual/5.4/manual.html, `debug.getlocal`

`string.dump(f, strip)`: "If strip is a true value, the binary representation may
not include all debug information about the function, to save space." The C API
has the same hole: with no activation record, "only parameters of Lua functions
are visible (as there is no information about what variables are active)"
(`lua_getlocal`).

starlark-rust has to convert slots back to names to run a debugger expression,
and the code says how well that goes:

```rust
    /// There are lots of health warnings on this code. Might not work with frozen modules, unassigned variables,
    /// nested definitions etc. It would be a bad idea to rely on the results of continued execution
    /// after evaluating stuff randomly.
```
— `starlark/src/debug/evaluate.rs:30-32`

The body copies every local out by `(slot, name)`, evaluates, then copies back
(`evaluate.rs:66-99`).

### `eval` and dynamic scope defeat the analysis

Python's own reference states the limit: `eval()` and `exec()` "do not have access
to the full environment for resolving names", and free variables inside them
resolve in the global namespace, not the enclosing one.
— https://docs.python.org/3/reference/executionmodel.html

MDN states the cost directly:

> "Modern JavaScript interpreters convert JavaScript to machine code. This means
> that any concept of variable naming gets obliterated. Thus, any use of `eval()`
> will force the browser to do long expensive variable name lookups to figure out
> where the variable exists in the machine code and set its value."

and

> "Minifiers give up on any minification if the scope is transitively depended on
> by `eval()`, because otherwise `eval()` cannot read the correct variable at
> runtime."
> — https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/eval

A direct `eval` reads the calling function's locals; an indirect one does not.
Vyrn has no `eval`, so this hazard does not apply today. It does apply to any
future feature that reads a variable by a name computed at run time.

### The loop-variable capture semantics get frozen in

Once a slot is shared across iterations, a language has decided that captures
share it — unless it also emits a per-iteration close. C# had to ship a breaking
change to `foreach` in C# 5 to undo that decision. Lippert calls it the single
most common incorrect bug report the team received.
— https://ericlippert.com/2009/11/12/closing-over-the-loop-variable-considered-harmful-part-one/

### Stack traces

NOT FOUND: a write-up attributing lost or degraded stack traces to slot
resolution specifically. The name loss documented above is about local variable
names, not frame names.

---

## What this says for Vyrn

**RECOMMENDATION, NOT A DECISION.**

### The measured case for the change is weak

Boa's −33.6% and Cloudflare's 2x both come from deleting a hash. Vyrn deleted its
hash already and banked 46.8s → 31.0s. The remaining work per read is 1.30 frame
probes, and 99.2% of those probes look at three names or fewer, each compare
starting with a length check. Nobody has published a number for replacing *that*
with an index. Section 5 says NOT FOUND, and I will not invent one.

### The risk is higher for Vyrn than for any implementation surveyed

Every implementation that resolves to a slot uses **one flat frame per function
boundary**. clox, Lua and Wren build the static and the dynamic picture in the
same pass, so drift is impossible. Ruby and CPython create a scope only at a
function-like boundary, so an `if` body or a loop body cannot add or skip a frame.
starlark-rust runs a separate pass but targets one flat frame per `def`.

Vyrn pushes frames at seven places
(`N:/wt-bytefix/compiler/vyrn-frontend/src/interp.rs`: 2942 lambda params, 3096
block, 4140 `if let`, 4236 `for in`, 4399 projection body, 6279 `match` arm, 7339
stream loop), and one of them is **conditional at run time on a property of the
block**:

```rust
let owns_frame = block.stmts.iter().any(|s| matches!(s, Stmt::Let { .. }));
if owns_frame {
    scope.push(Frame::default());
```
— `interp.rs:3094-3096`

Mirroring that predicate in a resolver means writing the same three-line rule
twice and keeping it identical forever. That is precisely the failure Nystrom
records. It is not caught by an assert, because the wrong index is in range.

Two further specifics in the tree make a naive design worse:

- `Expr::Var { name, line }` has no place to put a resolved slot
  (`compiler/vyrn-frontend/src/ast.rs:1219-1222`). Resolution needs a new AST
  field, and the AST is shared with two backends and the LSP.
- `project::inline` **clones the function body and renames its bindings with a
  fresh global counter on every call**
  (`compiler/vyrn-frontend/src/project.rs:372` onwards). Slots annotated on the
  original AST do not survive that clone, and the renaming changes the names the
  resolver saw. This path needs its own answer before any slot scheme is sound.

### If you do it, do the real change, not the small one

The safe form is not "annotate `Expr::Var` with `(depth, index)`". It is: delete
the dynamic frames. Give a function body one flat frame; give block, `match` arm,
`if let`, loop variable and inlined projection locals distinct indices in that one
frame; keep a frame boundary only at a call. That is what every cited
implementation does, and it removes the sync hazard rather than managing it. It is
a large change to seven call sites plus the checker, and it interacts with drop
ordering, which Vyrn tracks per frame today (`interp.rs:3226`).

### Cheaper rungs to try first, in order

1. **The 183,682,210 coerce calls, 97.6% of them no-ops.** That is a bigger
   counter than the remaining scan cost and needs no static/dynamic contract.
2. **Intern names to a `u32` symbol id.** The reverse scan then compares
   integers instead of `str`. Zef measured its equivalent change at 18%
   (Optimisation #4, https://zef-lang.dev/implementation). It carries no
   resolver-versus-interpreter contract at all, because the scan still answers by
   identity. This is the highest value per unit of risk on the list.
3. **Split `Vec<(String, Slot)>` into parallel name and value vectors.**
   Nederlang measured 24s → 21s for exactly that
   (https://www.dannyvankooten.com/blog/2022/rewriting-interpreter-rust/).
4. Only then consider flat frames.

### Blunt summary

The change is real, it is standard, and it is what a fast implementation does.
But Vyrn has already taken the part of the win that others measured, the counters
say the remaining scan is 1.3 frames of 3 names, no published number exists for
the increment being proposed, and Vyrn's seven dynamic frame pushes — one of them
conditional — are the exact configuration the literature warns produces silent
wrong-variable reads. Do 1 and 2 first. Re-count. If the scan still shows up in
the profile after names are integers, then do flat frames properly, in one pass
with the checker, and delete the dynamic pushes rather than mirror them.
