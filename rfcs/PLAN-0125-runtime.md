# RFC-0125 — the runtime in Vyrn: design for M4

- **Status:** Design (2026-09-02); §6 steps 0 and 1 landed on `track-e` the same
  day, see the results under the §6 table. This document is
  the design RFC-0125 §2.4 states in one paragraph, written out so M4 can be
  built one family at a time and each family can be gated. It decides the
  primitive set, the fence, the allocator and the migration order; §8 lists
  what it could not decide.
- **Depends on:** RFC-0125 (the RFC this serves: §1.4 measured the allocator,
  §2.2 names the effects, §2.5 names the routes), RFC-0077 (the direct wasm
  backend whose runtime this replaces), RFC-0089 M1a (the String header),
  RFC-0028 and RFC-0117 (the three Map layouts), RFC-0114 §25 (the free audit
  and the residue ratchet this deletes), RFC-0072 and RFC-0103 (the fence and
  the floor §3 chooses between), RFC-0076 (the embedded wasmtime §5 reuses).
- **Evidence:** every count below was taken at the tree this document was
  written in, with the scripts described in §1.0. The C shim is the string
  `RUNTIME_SHIM_TEMPLATE` in `compiler/vyrn-codegen/src/toolchain.rs`; the
  wasm runtime is `fn runtime` in `compiler/vyrn-codegen/src/direct.rs`; the
  native backend's own runtime is the six IR string constants at the top of
  `compiler/vyrn-codegen/src/lib.rs`.

---

## The question

RFC-0125 §2.4 says: allocator, Map, String, Array operations, validation, the
trap table and their wording live in one Vyrn module, compiled by the emitter
into every program, over a fenced set of raw-memory primitives. Today that
runtime is written three times. This document answers, with the code counted:
what the three copies contain and where they disagree (§1); which primitives
the one module needs and why each cannot be a library (§2); how the fence
keeps every other module off them (§3); what the allocator is (§4); what the
interpreter does with the module before M5 deletes it (§5); in what order the
families move and what gates each (§6); and what it costs (§7).

---

## 1. The inventory

### 1.0 How the counts were taken

The C shim is one Rust string, lines 26 to 1,098 of `toolchain.rs`, 1,073
lines. A brace-matching pass over it finds 85 top-level function definitions
holding 638 lines between them; the rest is comments, `typedef`s, statics and
`#if` arms. Twelve of the 85 are `#if` twins (the audit lock, the task
functions, the generator host entry), so the shim defines 73 distinct
functions.

The wasm runtime is `fn runtime` in `direct.rs`, lines 15,355 to 19,559:
4,205 lines. It emits 55 functions, each announced by one `rt.next_is(m, ..)`
line; a function's count below is the span from its announcement to the next
one, so it includes the function's own comment block (703 comment lines over
the 55). The 135 lines before the first announcement intern the trap wording
and reserve the runtime's cells. The `Rt` table and its `slots` constructor,
lines 14,941 to 15,354, are another 414 lines of index bookkeeping and doc.

The native backend's runtime is in two places: the shim above, called through
113 distinct `@__vyrn_*` symbols at 231 call lines in `lib.rs`, and 420 lines
of hand-written LLVM IR in six constants at the top of `lib.rs`
(`REGION_RUNTIME` 131, `STREAM_RUNTIME` 29, `ENCODING_RUNTIME` 45,
`STRING_RUNTIME` 95, `REGEX_RUNTIME` 28, `IO_RUNTIME` 92), plus the internal
helpers every program gets formatted in (`__vyrn_trap_msg`, `__vyrn_trap_idx`,
`__vyrn_panic`, `__vyrn_call_enter`, `__vyrn_call_exit`,
`__vyrn_globals_init`, `__vyrn_globals_teardown`).

The emitter side is counted too, because it is what changes shape: `direct.rs`
references the `rt` table 156 times outside `fn runtime`, and the trap wording
table `vyrn-frontend/src/trap.rs` (296 lines, 22 public items) is read 16
times in `direct.rs`, 21 in `lib.rs` and 47 in `interp.rs`.

### 1.1 By family

Line counts are function spans as defined above. "—" means the family has no
function there; where the work is done inline by the emitter instead, the
cell says so.

**Allocation**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| `malloc` | `__vyrn_malloc` 13 + `__vyrn_alloc_check` 7 | `malloc` 174 | — |
| `free` | `__vyrn_free` 17 | `free` 49 | — |
| `realloc` | `__vyrn_realloc` 18 | none: `str_append`, `push`, `region_keep` allocate, copy and free | — |
| free audit and leak check | 12 functions, 110 lines (`vyrn_audit_*`, `__vyrn_leak_check_on`, `__vyrn_audit_exit`, `__vyrn_teardown_begin`) | none | — |
| total | 165 | 223 | 0 |

**Strings**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| header new / grow / setlen | `vstr_new` 7, `vstr_grow` 5, `vstr_setlen` 4 (private to the shim) | `str_new` 35 | `STRING_RUNTIME` 95 (`str_new`, `str_free`, `str_len`, `str_setlen`, `str_bytes`, `str_bytes_range`) |
| length, compare, prefix | `__vyrn_strlen` 1; `strcmp` is libc | `strlen` 25, `strcmp` 48, `starts` 57 | — |
| concatenation and append | inline in the emitter, over `__vyrn_realloc` | `concat` 66, `str_append` 112 | — |
| integer to text | `__vyrn_snprintf` 8 (`vsnprintf`) | `int_str` 86, `bool_str` 12, `print_i64` 9 | — |
| text to integer | `strtoll` (libc) | `parse_i64` 110, `str_i64` 93 | — |
| UTF-8 validation | — | `utf8valid` 91, `str_from_bytes` 116 | `ENCODING_RUNTIME` 45 |
| regex DFA runner | — | `regex_run` 100 | `REGEX_RUNTIME` 28 |
| line and column | `__vyrn_line_at` 8, `__vyrn_col_at` 8 | `line_at` 40, `col_at` 57 | — |
| total | 41 | 1,057 | 168 |

Float formatting is absent from all three columns on purpose: `std/num`'s
`f64Str` builds it out of bytes in Vyrn (RFC-0081 M2). `charCount` left the
shim for `std/text` (RFC-0078; "47 exports to 46" in the shim's own comment),
the codecs left `lib.rs` for `std/codecs` (about 520 lines of IR, per the
comment above `ENCODING_RUNTIME`), and `strncmp` left for `std/strpred`. Those
four moves are this document's precedent: each was a runtime function that
became Vyrn over a smaller primitive, and parity held.

**Arrays**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| element access, push, reserve, append, copyFrom, clear | — (inline in the emitter) | — (inline in the emitter) | — (inline) |
| bounds trap | — | `trap_idx` 45 | `__vyrn_trap_idx` (formatted per program) |

Arrays have no runtime function in any engine. Every operation is emitted at
its call site, three times, which is the per-builtin product RFC-0125 §1.1
counts. They are in this inventory because the module of §2.4 gives them
functions for the first time.

**Maps** (three key layouts: String, Int64, canonical pack — RFC-0028,
RFC-0117 M1, RFC-0117 M2)

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| hash | `map_hash` 5, `map_hash_i64` 6, `map_hash_pack` 9 | `map_hash` 42 (String); the i64 and pack hashes are inside their `slot` | — |
| bucket probe | `map_slot` 6, `_i64` 6, `_pack` 6 | `map_slot` 57, `_i64` 78, `_pack` 121 | — |
| find | `__vyrn_map_find` 9, `_bytes` 17, `_i64` 1, `_pack` 1 | `map_find` 65, `_bytes` 165, `_i64` 58, `_pack` 59 | — |
| record an append | `__vyrn_map_index_add` 3, `_i64` 1, `_pack` 1 | `map_put` 36, `_i64` 34, `_pack` 40 | — |
| reindex after grow or remove | `map_reindex` 6, `_i64` 6, `_pack` 6 | `map_reindex` 52, `_i64` 51, `_pack` 62 | — |
| reserve, remove_at, keys_copy | 3 × 3 functions, 33 lines | inline at their single call sites (the `Rt` doc says why) | — |
| total | 25 functions, 133 lines | 14 functions, 920 lines | 0 |

**I/O**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| bytes out | `__vyrn_write_stdout` 3, `__vyrn_stdout`/`__vyrn_stderr` 2 | `write_all` 136, `print_str` 15 | — |
| standard input | `__vyrn_in_byte` 13, `__vyrn_read_line` 18 | `getbyte` 68, `read_line` 158 | — |
| files | `__vyrn_read_file` 22, `_bytes` 18, `__vyrn_write_file` 9, `_bytes` 8, `__vyrn_rename_file` 9, `__vyrn_fsync_file` 15 | `open_at` 62, `read_all` 122, `read_file` 71, `_bytes` 55, `write_file` 31, `_bytes` 68, `rename_file` 99, `fsync_file` 81 | — |
| the error message with the path in it | `__vyrn_snprintf` (shared with int formatting) | `err3` 121 | `IO_RUNTIME` 92 (`read_err`, `write_err`, `rename_err`, `args`) |
| arguments and environment | `__vyrn_args_count` 3, `__vyrn_args_get` 1 | `args` 125, `env_get` 88 | — |
| directory listing (generator path only) | — | `list_dir` and `list_dir_kinds` 168 | — |
| total | 121 | 1,468 | 92 |

**Time and randomness**

| function | C shim | wasm runtime |
|---|---|---|
| `now`, `monotonic`, `randomSeed` | 7, 19, 14 = 40 | 23, 40, 35 = 98 |

**Traps**

| item | C shim | wasm runtime | native IR |
|---|---|---|---|
| the exit with a message | `__vyrn_alloc_check`'s `fputs($OOM); exit(1)`; `extern_trap_stubs` per `extern fn` | `trap` 13, `trap_idx` 45 | `__vyrn_trap_msg`, `__vyrn_trap_idx`, `__vyrn_panic`, formatted per program |
| the wording | one hole, `$OOM`, filled from `trap.rs` | 11 messages interned in the 135-line preamble from `trap.rs` | strings formatted from `trap.rs` |
| call-depth accounting | — | inline in every prologue (RFC-0125 M1 priced it at 0.25 s on nbody) | `__vyrn_call_enter`/`_exit` |

The wording is already one table, `trap.rs`. What is three is the site: the
function that prints and exits, and the prologue that counts.

**Regions**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| enter, alloc, keep, exit, pop, pop_except | — | `region_keep` 103, `region_free` and `region_pop` 142 (one loop emits both) | `REGION_RUNTIME` 131: `region_enter`, `region_alloc`, `region_pop`, `region_exit`, `region_pop_except` |

Both are the same design: a per-depth side list of block pointers over
`malloc`, 16 entries then doubling, 64 frames deep (`REGION_MAX` in
`interp.rs`, `$NEST` in the IR), freed one pointer at a time at the closing
brace. RFC-0125 §1.3 calls this "the syntax of an arena and none of the
effect", and §4 below is where the effect arrives.

**Tasks**

| function | C shim | wasm runtime |
|---|---|---|
| `spawn`, `join`, `task_release`, `join_all` | wasi: 4 functions, 13 lines (eager); native: 12 functions, about 140 lines (Win32 events or pthreads, a doubly linked registry, the exit walk) | none: the emitter runs the thunk at the spawn point |

**Generator host**

| function | C shim | wasm runtime |
|---|---|---|
| `__vyrn_gen_init` 12, `__vyrn_gen_fini` 1, `__vyrn_gen_libc_keep` 1, `main` 11 | 25 | `list_dir`, `list_dir_kinds`, and the `vyrn_gen` import table (`direct.rs` lines 141 to 180) |

**Streams**

| function | C shim | wasm runtime | native IR |
|---|---|---|---|
| box, unbox, close | — | inline, one `__vyrn_stream_close_<T>` per element type | `STREAM_RUNTIME` 29 |

### 1.2 Totals

| copy | functions | lines |
|---|---|---|
| C shim (`toolchain.rs`) | 73 distinct | 1,073 in the string, 638 in bodies |
| wasm runtime (`direct.rs` `fn runtime`) | 55 | 4,205, of which 703 comment |
| native IR prelude (`lib.rs`) | 6 constants, 7 formatted helpers | 420 in the constants |
| the `Rt` table and `slots` (`direct.rs`) | — | 414 |

RFC-0125 §1.6 estimated "about 1,600 lines of C runtime that the wasm backend
re-emits by hand". The count is 1,073 lines of C, 420 of IR, and 4,619 of
Rust that emits wasm: the wasm copy is four times the C copy because it is
written as instruction lists with the doc inline.

### 1.3 Where the copies disagree

Each row was found by reading the two implementations side by side. "Agree
by construction" means one copy's comment names the other and the shape is
the same; it is still two copies.

| item | C shim / native | wasm runtime | kind |
|---|---|---|---|
| allocator | platform `malloc` behind a wrapper with an audit branch | segregated free list, 113 size classes, four per power of two | design. RFC-0125 §1.4: binary-trees 1.87 s native, 0.88 s wasm, same source |
| request too large | refused above `SIZE_MAX` (never on LP64) | refused above 2 GiB, and a heap top past 4 GiB | limit differs; both print `OUT_OF_MEMORY` |
| `realloc` | in place through libc | none; every grow is allocate, copy, free | behaviour differs on the copy count; observable only in time |
| free of a foreign pointer | under the audit: `free audit: double or foreign free`, exit 134; without it, undefined | silently refused when below `HEAP_BASE` or the class is out of range; the block leaks | a class of defect one engine reports and the other hides |
| leak check | `VYRN_LEAK_CHECK` walks the audit table after teardown, exit 135 | none | the residue ratchet measures the native binary only |
| poison on free | `0xDD` fill under the audit | none | a use-after-free reads stale bytes on one engine and `0xDD` on the other |
| integer to text | `vsnprintf("%lld")` through `__vyrn_snprintf` | `int_str`, 86 lines by hand | agree by test, not by construction |
| text to integer | `strtoll` | `parse_i64` and `str_i64`; `str_i64` skips no whitespace and does not clamp, and says so | `strtoll` accepts leading whitespace; only harness inputs reach it |
| `strcmp` | libc | 48 lines by hand | agree by construction |
| standard output | stdio's buffer, `fwrite`, binary mode set once in `main` | a 4,096-byte write-behind buffer in `write_all`, flushed before any write to another descriptor | agree on order; different flush points |
| standard input | 4,096-byte buffer over `read` | 4,096-byte buffer over `fd_read` | agree by construction (the shim's comment names the wasm one) |
| `read_line` growth | 64 then doubling | 64 then doubling | agree by construction |
| `read_file` growth | 1,024 then doubling | 1,024 then doubling | agree by construction |
| monotonic clock on Windows | wall clock (`timespec_get`), "an adequate elapsed source" | `clock_time_get(MONOTONIC)` on every host | a clock that can go backwards on one engine; the fixed-clock harness path (`1e9 + n × 1e6`) is identical |
| random seed | `rand_s` / `getentropy` | `random_get` | same contract, different source; the fixed-seed path identical |
| cross-device rename | status 2 on `EXDEV` / `ERROR_NOT_SAME_DEVICE` | `path_rename` through the preopen table; the status is `err3`'s | not compared beyond the corpus |
| I/O error text | `__vyrn_snprintf` with a format string from `io_message_parts` | `err3` over three interned pieces from `io_message_parts` | one wording, two builders |
| Map `reserve`, `remove_at`, `keys_copy` | shim functions | inline at the call site | same code, different place |
| Map hashes | FNV-1a to the NUL, SplitMix64 finalizer, FNV-1a over the pack | the same three | agree by construction; `std/hash`'s `fnv1a` is a fourth copy of the first, in Vyrn |
| `map_find_bytes` | 17 lines | 165 lines | the same probe |
| UTF-8 validation | 45 lines of IR, 8-byte ASCII skip | 91 lines, 8-byte ASCII skip | the same DFA (`@__vyrn_utf8d`) both walk |
| regex runner | 28 lines of IR | 100 lines | the same DFA tables, emitted per pattern by the compiler |
| regions | thread-local frames (`thread_local global`) | global cells | tasks: a region inside a spawned task is per thread natively and shared in wasm, where tasks are eager so it cannot be observed |
| a `return` out of a region | `region_pop`: the frame's other blocks leak | `region_pop`: the same, "which is what the textual backend also chooses" | agree, on a leak |
| tasks | real threads natively, eager under wasi | eager | schedule differs; output is byte-identical because tasks are isolated |
| call-depth counter | two functions | inline in every prologue | agree on the limit and the words |
| `listDir` | absent; the native build is refused | present on the generator path | RFC-0103's census records the refusal |

Twenty-six rows. Four are design differences that change what a program pays
(the allocator, `realloc`, the audit, the flush points); the rest are one
algorithm written twice and kept equal by the parity corpus. That is the
maintenance RFC-0125 §"The question" describes, measured on this one file
pair.

---

## 2. The primitives

The runtime module needs what a library cannot write. A library function is
Vyrn source over Vyrn values; a primitive is a wasm instruction the emitter
maps a `prim` row to (RFC-0125 §2.3). Everything below is one instruction or
one host import. Everything not below is Vyrn.

### 2.1 Raw memory

| primitive | signature | wasm instruction | why it cannot be library |
|---|---|---|---|
| `load8`, `load16`, `load32`, `load64` | `(addr: Int32) -> UInt8 / UInt16 / UInt32 / UInt64` | `i32.load8_u`, `i32.load16_u`, `i32.load`, `i64.load` | a Vyrn value has a type; a byte at an address has none until the runtime gives it one |
| `loadF32`, `loadF64` | `(addr: Int32) -> Float32 / Float64` | `f32.load`, `f64.load` | the same, for the two float widths |
| `store8` … `store64`, `storeF32`, `storeF64` | `(addr: Int32, v: T)` | `i32.store8` … `f64.store` | the same |
| `copy` | `(dst: Int32, src: Int32, n: Int32)` | `memory.copy` | a loop of `load8`/`store8` is correct and one hundred times slower; every String and Array move is one of these |
| `fill` | `(dst: Int32, byte: UInt8, n: Int32)` | `memory.fill` | the same; the Map's `reindex` and the canonical pack's zeroing use it |
| `pages` | `() -> Int32` | `memory.size` | the allocator's top-of-heap test (`malloc` lines 15,760 to 15,790) |
| `grow` | `(delta: Int32) -> Int32` | `memory.grow` | the one way the heap gets bigger; returns −1 on refusal, which the allocator turns into `OUT_OF_MEMORY` |
| `heapBase` | `() -> Int32` | the `HEAP_BASE` global | where the data segment ends and the heap begins; `free` refuses anything below it |

Addresses are `Int32` because the route is wasm32 (RFC-0125 §2.8 records
the 4 GB limit). A `Ptr<T>` type is not proposed: the runtime module is the
only reader of addresses, and inside it an address is an integer with a
comment. Typing addresses would be a second type system for one file.

### 2.2 Host imports

The runtime module imports exactly the `wasi_snapshot_preview1` table the
wasm backend already declares (`direct.rs` lines 77 to 140): `fd_write`,
`fd_read`, `fd_close`, `proc_exit`, `path_open`, `path_rename`, `fd_sync`,
`fd_prestat_get`, `args_sizes_get`, `args_get`, `environ_sizes_get`,
`environ_get`, `clock_time_get`, `random_get`. Fourteen. Each is an I/O
effect under RFC-0125 §2.2's effect judgment, which is what a host import is
by that section's definition. The generator path adds `vyrn_gen.read` and its
directory listing; the `vyrn` namespace for `extern fn` is unchanged (RFC-0012).

Each import cannot be library because it is the boundary: there is no Vyrn
below `fd_write`.

### 2.3 The trap primitive

`trap(msg: Int32, len: Int32) -> Never` is `fd_write` to descriptor 2 then
`proc_exit(1)`. It is written once, in the module, over the two imports; the
emitter's `trap` row (RFC-0125 §2.1) is a call to it with a table index. It
is listed here because RFC-0125 M1 measured the call site: every bounds check
parks its message and branches to one trap block per function, and that stays.

### 2.4 What is not a primitive

`strlen`, `strcmp`, `int_str`, `parse_i64`, `utf8valid`, `regex_run`,
`line_at`, `col_at`, every Map function, `str_append`, `concat`, the
allocator, the regions, the stdin and stdout buffers, `err3`. Each is a loop
over `load8` and `store8` or over another runtime function. Each is in the
wasm runtime today as an instruction list, and each becomes Vyrn source.

The four SIMD lanes (RFC-0075) are not runtime; they are `prim` rows the
emitter already has.

---

## 3. The fence

### 3.1 The choice

The language has three mechanisms that restrict what a module may see or do:

| mechanism | RFC | what it decides | who declares it |
|---|---|---|---|
| audience | RFC-0072 | which modules may import a module, by path segment | `vyrn.json`, `audience` map; a fence |
| capability floor | RFC-0103 | which capabilities an artifact's closure may need, by target | the target, fixed by physics; a floor |
| module contract | RFC-0071 | which exports a module may have and their shapes | a `contract` declaration, checked by `std/contract` |

The primitives of §2.1 must be visible to one module and invisible to every
other, and that visibility must not be something a manifest can widen. The
floor is the right kind of guarantee — "nobody can relabel it" — but it
answers the wrong question: it checks an artifact's closure against a target,
and the runtime module is in every closure, so the floor would either refuse
every artifact or exempt the runtime by name. A contract constrains a module's
exports, not its importers, so it says nothing about who may call `load8`.

Audience is the mechanism with the right shape: RFC-0125 §2.2 already says "an
audience is the set of declarations it may see" and makes it an inclusion
check in the kernel. The fence is therefore an audience — with one change
that turns it from a fence into a floor for this one case: **the audience of
the primitives is declared by the compiler, not by `vyrn.json`.**

### 3.2 How it works

1. The primitives are declarations in a compiler-supplied module, `std/mem`,
   with the signatures of §2.1 and no bodies. RFC-0094 made a builtin a
   declaration; these are declarations whose lowering is one `prim` row each.
2. `std/mem` has a fixed audience: the set `{ std/runtime }`. The audience is
   a constant in the checker beside the standard-library table, not a key the
   manifest reads. `vyrn why std/mem` prints "audience: `std/runtime`, declared
   by the compiler".
3. The checker's existing audience edge check (RFC-0072 §Enforcement) refuses
   any other import of `std/mem` with the existing diagnostic shape:
   `` error: `app/main.vyrn` cannot import `std/mem`, whose audience is the runtime ``.
4. `std/runtime` is shipped with the compiler the way `std/json` is
   (`include_str!` in `interp.rs`, lines 8,944 to 8,964), and its hash is
   recorded in the compiler's own build, so a user cannot substitute a module
   at that path. That is what makes step 2 a floor: the one importer is a file
   the compiler carries.
5. The kernel's effect judgment (RFC-0125 §2.2) is unchanged. A `prim` load or
   store has no effect; `grow` has the allocates effect; a host import has the
   I/O effect. The fence is about visibility, the judgment about behaviour, and
   they do not overlap.

### 3.3 What this costs and forbids

A user cannot write an allocator, a custom Map layout or an unchecked byte
loop. That is the intent: RFC-0125 §2.4 says the primitives are "the only
unsafe surface in the language, fenced in that module, and reviewed there". A
user who needs `memory.copy` speed gets it through `std/runtime`'s exported
functions (`copyFrom`, `append`, the byte views RFC-0109 decided), which are
the safe surface over the same instruction.

Extending the fence later — a second audience member, a `trusted` keyword — is
one constant in the checker and a sentence in RFC-0072. It is not proposed.

---

## 4. The allocator and the regions

### 4.1 What exists

The wasm runtime's `malloc` (`direct.rs` line 15,626, 174 lines) is a
segregated free list. A request `n` rounds to eight and floors at eight; the
class is `shift × 4 + sub` where `shift = 29 − clz(n − 1)` and `sub` is the
two bits under the leading one, so there are four classes per power of two,
113 in use (`MIN_CLASS` 3 to `MAX_CLASS` 115), the smallest 8 bytes and the
largest 2 GiB. Every block carries an 8-byte header holding its class. A
class's free blocks are a singly linked list whose link lives in the freed
payload (the 8-byte floor is what makes room for it). `malloc` pops the head
if there is one, else bumps `HEAP` by header plus class size and grows memory
one page at a time until the top fits. `free` (line 15,800, 49 lines) checks
the pointer is at or above `HEAP_BASE` and the header's class is in range,
then pushes. There is no coalescing, no `realloc`, and no size in the header —
the class implies it.

RFC-0125 §1.4 measured it: binary-trees at depth 18, 0.88 s under wasmtime
against 1.87 s natively through the platform allocator and the audit branch.
That is the number this design keeps.

### 4.2 The design

The module's allocator is that free list, written in Vyrn over `load32`,
`store32`, `pages`, `grow` and `heapBase`. Its shape in source:

```
// std/runtime — the allocator. Classes are four per power of two.
let heads: FixedArray<Int32, 116>   // module state, one head per class

fn classOf(n: Int32) -> Int32 { .. }      // the shift/sub arithmetic of §4.1
fn sizeOf(cls: Int32) -> Int32 { .. }      // (sub + 5) << shift
fn malloc(n: Int64) -> Int32 { .. }        // pop or bump; grow; trap OUT_OF_MEMORY
fn free(p: Int32) { .. }                   // refuse below heapBase(); push
```

Three decisions beyond a transcription:

1. **The audit and the poison go.** RFC-0125 §2.2 says the linear judgment
   makes a double free and a leak compile errors, so the C shim's 110 lines of
   audit table, lock and `0xDD` fill (§1.1, Allocation) have no run-time job
   left. `free` keeps its two silent refusals (below `heapBase`, class out of
   range) because a data-segment literal is still handed to `free` by `drop`
   on a static String (the `free` comment, line 15,802), and those are one
   compare each. The residue ratchet stays as a CI gate until M5 (§6).
2. **`realloc` is not added.** The three growers (`str_append`, `push`,
   `region_keep`) allocate, copy and free today, and the copy is a
   `memory.copy` at the width of the old block. A class-aware `realloc` that
   returns the same block when the new size fits the class is eight lines and
   is the one optimisation this design permits, because it removes the copy
   from every doubling that stays in class — which is half of them at four
   classes per power of two. It is written when a probe shows the copy, not
   before.
3. **The header keeps the class, not the size.** `free` needs the class and
   nothing needs the size; a String carries its own `{ len, cap }` header
   (RFC-0089 M1a) and an Array its own triple.

The native route gets this allocator too, through wasm2c or Cranelift
(RFC-0125 §2.5), so the platform allocator leaves the picture with the shim.
M4's gate in RFC-0125 — "binary-trees under the native route at or below its
wasmtime time from §1.4" — is the check that it did.

### 4.3 Regions as a bump arena

Today `region { .. }` is a side list of pointers freed one at a time (§1.1,
Regions). With the free list in the runtime and drops placed by the kernel,
a region becomes what its syntax says:

- `regionEnter()` records the current bump top `HEAP` and the depth. The
  frame is three words: the saved top, the saved free-list heads' generation
  (see below), and a keep pointer.
- Inside the region, `malloc` bumps only: it does not pop a free list, so
  every block allocated in the region is contiguous above the saved top.
  Blocks the region allocates are never pushed to a class list by `free`
  either — a `free` inside a region of a block above the saved top is a
  no-op, which is what the emitters' `region_depth == 0` gates already do
  for a `String` drop (`lib.rs` `emit_drop`, line 5,005).
- `regionExit()` resets `HEAP` to the saved top. One store. The 64-frame
  depth limit and its trap wording are unchanged (`REGION_MAX`).
- `regionPopExcept(keep)` — the value a `return` carries out — copies the kept
  block to a fresh `malloc` outside the region before the reset, exactly the
  copy `__vyrn_region_pop_except` avoids today by leaking the frame's other
  blocks. The copy is the price of not leaking, and it is one `memory.copy`
  of the value's own size.

Two things this changes for the linear judgment, and both are already in it:
a value allocated in a region is consumed by the region's close (RFC-0125 open
question 2 says so), and a value that leaves the region is a move the kernel
already sees as a `return`. A `free` of a block below the saved top from
inside the region — a value from outside the region, dropped inside it — goes
to the class list as usual; the address compare decides, not a flag.

Whether `region { }` keeps its syntax or becomes `std/runtime`'s `Arena` type
is left open (§8), because the regions census's verdict on inferred regions
stands either way and the runtime is the same in both spellings.

---

## 5. The interpreter, before M5

### 5.1 What the interpreter can use

The interpreter already runs Vyrn as wasm: RFC-0076's `vyrn-genwasm` compiles
a generator with the direct backend, instantiates it in the embedded wasmtime
(`compiler/vyrn-genwasm/src/lib.rs`, `fn run`, line 149), copies inputs in and
results out through linear memory, and caches the module by source hash. That
is the mechanism, and it serves the runtime module for the families whose
interface is bytes in and bytes out:

| family | interpreter today | with the module |
|---|---|---|
| UTF-8 validation | `String::from_utf8` at three sites (`interp.rs` 6,435, 6,466, 6,748) | `runtime.utf8valid` over a copied buffer |
| text to integer | `parse_int` (line 1,472) and `str::parse::<i64>` | `runtime.parseI64` |
| integer to text | Rust `format!` | `runtime.intStr` |
| regex | `crate::regex` (line 8,057), a Rust DFA compiler and runner | `runtime.regexRun` over the compiler's tables |
| trap wording | `trap.rs`, 47 references | unchanged; the table is already one |
| I/O error text | `trap.rs` and `io_message_parts` | `runtime.err3` |

Each is a call with a copy in and a copy out, the shape the generator engine
already pays per run. For these families the interpreter stops being a third
implementation and becomes the module's first consumer, which is worth having
for one reason: parity for those functions becomes byte-identity by
construction, one engine short of what M5 delivers for everything.

### 5.2 What it cannot use, and why

The allocator, the String and Array storage, the Maps and the regions manage
addresses in a linear memory. The interpreter has no linear memory: a `Val` is
a Rust enum whose `String` is a `Vec<u8>` and whose `Map` is an `Rc`
(RFC-0125 §1.1's third picture). To use the module's `malloc` the interpreter
would have to keep its values in the module's memory, which is to say it would
have to stop being a tree-walker over `Val` and become the wasm engine. That
is M5, by another name and with two value models alive at once during the
change.

So the interpreter uses the module for the six byte-in, byte-out families and
for nothing else, and M5 inherits the rest with this reason written: the
interpreter's value model is the thing being deleted, and no adapter makes it
share an allocator with a linear memory without becoming the replacement.

One consequence for the probes: until M5, a leak probe under `rfcs/probes-0125/`
is measured natively and under wasmtime, never under `vyrn run`, because the
interpreter frees by `Rc` and cannot leak what the plan forgets.

---

## 6. Migration order

One family at a time. Each step deletes its C, its IR and its wasm copy only
when its gate is green on the Vyrn copy, per RFC-0101 §3.0's second rule.
Every step runs the same three gates unless it says otherwise:

- **parity:** `cargo test -p vyrn-cli --release --test parity -- --ignored`
  (41 programs, three engines byte-identical);
- **residue:** `cargo test -p vyrn-cli --test residue -- --ignored` (the leak
  ratchet, still measured natively until step 3 moves the native route);
- **probes:** the four programs under `rfcs/probes-0125/` flat at 200,000 and
  400,000 turns, natively and under wasmtime.

| step | family | what moves | extra gate |
|---|---|---|---|
| 0 | the fence | `std/mem` declared, its audience fixed, `std/runtime` an empty module shipped with the compiler; the emitter lowers `std/mem` calls to `prim` rows | `vyrn-cli/tests/audience.rs` gains the refusal of a user import of `std/mem`; every existing gate unchanged |
| 1 | strings, pure | `strlen`, `strcmp`, `starts`, `int_str`, `parse_i64`, `str_i64`, `utf8valid`, `line_at`, `col_at`, `regex_run` into `std/runtime`; the wasm backend calls them; the interpreter calls them through §5.1 | fasta and reverse-complement re-timed (RFC-0125 §1.5b: 0.80 s and 0.46 s native, 0.93 s and 1.06 s wasm) within noise |
| 2 | the allocator | `malloc`, `free` into `std/runtime`; the wasm backend's 223 lines deleted | binary-trees under wasmtime at 0.88 s or better; the audit still on natively |
| 3 | the native route | the native binary is the wasm through wasm2c and clang (RFC-0125 §2.5, measured there at 1.5x, 1.9x, 1.8x); the C shim's allocator, strings and maps deleted; the shim becomes the 200-line WASI host RFC-0125 §2.4 names | the residue ratchet re-based on the new route, with the audit deleted and the leak witnesses of RFC-0114 refused by the kernel instead (RFC-0125 M2's gate); binary-trees native at or below 0.88 s (M4's gate) |
| 4 | strings, allocating | `str_new`, `concat`, `str_append`, `str_from_bytes`; the three `STRING_RUNTIME` accessors | `census-strings.md`'s builders re-run; `std/json`'s six append sites unchanged in output |
| 5 | maps | the three key layouts, one Vyrn `Map` body over a `keyBytes` per layout, `find`/`put`/`reindex`/`reserve`/`removeAt`/`keysCopy` | k-nucleotide (RFC-0104's Map row) within noise; `finitekeys`, `heapkey`, `floatkey` examples byte-identical |
| 6 | arrays | `push`, `reserve`, `append`, `copyFrom`, `clear`, `at` become runtime functions for the first time (§1.1, Arrays); the emitter's inline copies deleted from both backends | nbody, spectral-norm, fannkuch re-timed against RFC-0125 M1's table (1.98 s, 2.83 s, 3.58 s Cranelift; 1.30 s, 1.90 s, 3.64 s release) |
| 7 | I/O | `write_all`, `getbyte`, `read_line`, `open_at`, `read_all`, `read_file`, `write_file`, `rename_file`, `fsync_file`, `args`, `env_get`, `err3`, time and randomness | `examples/files.vyrn`, `storage.vyrn`, `clock.vyrn` byte-identical under the fixed clock and seed; RFC-0103's capability census re-run (`census-0103`) |
| 8 | regions | §4.3 | `census-regions.md` §5a's three shapes re-measured: the region-per-iteration shape at the flat line the region-around-the-loop shape holds |
| 9 | traps and tasks | `trap`, `trap_idx`, the depth counter in the prologue; tasks stay eager in the module and native threads stay in the host until the threads proposal (RFC-0125 §2.8) | every `error:` line in the parity corpus byte-identical; `concurrency.vyrn` byte-identical |

Steps 1 and 2 can land before step 3 and are useful on their own: they
remove the two largest hand-emitted families from `direct.rs` and put the
allocator where §4 wants it. Step 3 is the route decision RFC-0125 §2.5 left
open, taken as its measurements say — release through wasm2c and clang — and
nothing after it is possible without it, because until then the C shim is
still the native runtime and every family would be moved into Vyrn while its
C twin stays.

Each step's prediction, per RFC-0101 §3.0's first rule, is the number in its
extra-gate column, and the report goes into RFC-0125 §3 M4 either way.

**Results, steps 0 and 1 (2026-09-02, branch `track-e`).** Step 0: `std/mem`
declares the eighteen primitives of §2.1 and §2.3 as exported functions whose
bodies the emitter never reads (`Fn_::mem_prim` lowers each call to its
instruction; `vyrn-lower` builds no instance for them); its audience is the
constant `runtime_fence` in the loader, beside `RT_MODULES`, and a user import
of `std/mem` or `std/runtime` is refused with the RFC-0072 wording whether or
not `vyrn.json` declares an audience (`tests/audience.rs`, three new tests;
`vyrn why std/mem.vyrn` prints "declared by the compiler"). `std/runtime` is an
`RtModule` with `always: true`, so every load links it. Step 1: the ten pure
string functions are Vyrn in `std/runtime.vyrn` (264 lines with their doc, over
`load8`, `load32` and `load64`); the wasm emitter reserves their indices before
its hand-emitted runtime, which calls them, and the ten hand-emitted bodies
are deleted — 692 lines out of `direct.rs`, 198 in (the `VyrnRt` table and its
signature check). `utf8Valid` takes the DFA table's address as a third
argument, and `parseI64` returns the `Option` rather than writing through a
pointer. The runtime's frames are not counted against the call-depth budget,
because the copies they replace had no prologue. Gates: frontend, codegen and
the ten `vyrn-cli` suites green; parity 41 of 41 byte-identical; residue
green. Timing, same machine, `run.py --runs 10` with another worktree's parity
job sharing the CPU (spread 13 to 68 percent, so read the wasm column against
the baseline taken the same way before the change):

| program | native before | native after | wasm before | wasm after | recorded (RFC-0125 §1.5b) |
|---|---|---|---|---|---|
| fasta | 0.74 s | 0.80 s | 0.85 s | 0.89 s (cpu 0.89 s) | 0.80 s / 0.93 s |
| reverse-complement | 0.35 s | 0.37 s | 1.01 s | 1.07 s to 1.12 s (cpu 1.06 s) | 0.46 s / 1.06 s |

The native column does not run the Vyrn copies (step 3 moves that route), so
its movement is the noise floor of the run; the wasm column moves inside it.
The interpreter does not call through (§5.1): RFC-0076's engine compiles a
whole generator program, not one function of a linked module, so a per-call
bridge would be a mechanism this plan has not designed, and M5 deletes the
interpreter's copies anyway. The lowered-form gate's synthesized count went
from 1,296 at step 0 to 1,233 with the module linked (1,261 on an earlier run;
the count is address-collision noisy by construction), under the 1,400 ceiling.

**Results, step 2 (2026-09-02, branch `track-g`).** `malloc` and `free` are
Vyrn in `std/runtime.vyrn` (106 lines with their comment, over `load32`,
`store32`, `memorySize`, `grow` and `heapBase`), and the wasm emitter's copy is
deleted: 286 lines out of `direct.rs`, 20 in (two rows of the `VyrnRt` table
and the wiring), the `HEAP` global out of `wasm.rs`. The design is §4.1's and
§4.2's — 113 classes, four per power of two, the class in an eight-byte header,
the link in the freed payload, `Int64` request, the width check before the
rounding — with four differences from the transcription. (1) The class heads
and the bump offset live in the heap's own first 480 bytes (116 words of
heads, the offset at 464, the first block at 480), not in a reserved table and
a mutable global: wasm memory arrives zeroed, so nothing initializes them, and
`std/mem` needed no `getHeap`/`setHeap` pair. (2) The bump path grows memory by
the pages the new top needs in one `grow`, not one page per loop turn; a
refusal still traps with `out of memory`. (3) There is no `clz` primitive, so
the shift is a loop over `t >> 2`, which runs `shift` times — zero or one for
a block under 32 bytes. (4) The trap is `panic("out of memory")`, and the loader
does not stamp the runtime module's `panic` sites with a file and line (census
U5's rewrite), because the wording is `trap.rs`'s and parity compares stderr
byte for byte. §4.2 decision 1 (the audit and the poison go): the wasm copy
never had either, and the C shim's stay behind `VYRN_FREE_AUDIT` and
`VYRN_LEAK_CHECK` until step 3 deletes the shim's allocator; the residue
ratchet still measures natively. Nothing outside the allocator read the
header: `args`'s comment on the header slack is the one mention, and it
describes a refusal that still holds (the second header word is never
written). Gates: workspace, kernel (ratchet held), lowered, fixtures, parity 41
of 41, residue green; the cross-engine generator gate fails on the same five
programs at base and at head (the placement defect another branch owns). The
extra gate, binary-trees at depth 18 under wasmtime, median of five, base and
head interleaved on a machine shared with six other worktrees' builds (which is
why neither column reaches §1.4's 0.88 s, taken on an idle one):

| round | base (hand-emitted) | head (`std/runtime`) |
|---|---|---|
| 1, six builds running | 1.41 s | 1.41 s |
| 2, gates finished | 1.00 s | 0.99 s |
| 3, quietest | 0.95 s | 0.94 s |

The head moves inside the noise of the base every round, which is §7.3's
prediction; no grow or class profile was needed. Depth 10 prints the fixture.

**Results, step 3, first slice (2026-09-02, branch `track-j`).** The native
route of RFC-0125 §2.5 exists as a flag, `vyrn build --route wasm2c`, and the
text-IR route is untouched and still the default: this slice puts the numbers
on the table and deletes nothing, so the route decision of §8's question 3 is
taken with them and not before. What the flag does: `direct::compile` writes
`<out>.wasm`; `wasm2c -n prog` writes `<out>.w2c.c` and `.h`; the driver writes
`<out>.host.c` from `vyrn-codegen/src/wasi_host.c` (574 lines, the host RFC-0125
§2.4 names) and runs clang over the two, wabt's `wasm-rt-impl.c` and
`wasm-rt-mem-impl.c`, with `add_native_clang_flags` (`-O2 -ffp-contract=off
-march=x86-64-v2`), `-I` for wabt's `include/`, `share/wabt/wasm2c/` and simde,
and `-DWASM_RT_MAX_CALL_STACK_DEPTH=4000`: wasm-rt counts call depth on Windows,
at 500 frames by default, and Vyrn's own counter traps at 1,000 user frames
with the runtime's frames uncounted, so the host's limit sits above the
program's and `error: call depth exceeds 1000` stays the program's wording. The
host: the fifteen `wasi_snapshot_preview1` imports of §2.2, each doing what
`wasmrun.rs` does — the same errno per failure, `.` and `..` first in
`fd_readdir`, `MOVEFILE_REPLACE_EXISTING` for `path_rename`, `BCryptGenRandom`
for `random_get`, binary stdio so the bytes are the guest's. A trap the program
did not spell (a wasm `unreachable`, a memory access past the guard page)
prints `error: <wasm-rt's wording>` and exits 1, the shape `wasmrun.rs` gives;
the `wasmtime` CLI prints its own backtrace and exits 3 for the same trap, so
that one line is the one import behaviour that is not byte-identical, and no
corpus program reaches it. Tools: `toolchain::wasm2c_from` and `simde_from`
follow `wasmtime_from`'s order without the pin step (`$VYRN_WASM2C` and
`$VYRN_SIMDE`, then `tools/wabt-*/bin/wasm2c` and `tools/simde/`), the version
is what `wasm2c --version` prints, and a missing tool is the refusal "could
not find `wasm2c`. Unpack a wabt release under tools/ (tools/wabt-<version>/
bin/wasm2c) or set VYRN_WASM2C to the executable." CI has no wabt, so
`tests/route.rs` is ignored by default and skips without the tools, under the
same `VYRN_REQUIRE_TOOLS` rule as every other tool; nothing required changed.
The route gate: 171 corpus programs byte-identical against the `wasmtime` CLI
on raw stdout, stderr and exit code, 33 skipped (the parity loop's refusals and
the one host-only program; `listdir.vyrn` is checked, because the route runs
the wasm). Gates: fmt, workspace, the seven `vyrn-cli` suites, parity 41 of 41,
residue, `doc --verify`; the cross-engine generator gate fails on the same five
programs at base and head. The `wasmhash` manifest check is red on 172 of 172
examples at base and at head with the same emitted hashes on both: the route
reads the wasm and emits nothing (the branch touches no emitter file), and the
committed `rfcs/census/wasm-sha256.tsv` predates the five emitter commits of
steps 1 and 2 and `listDir`; whoever lands next on the emitter regenerates it
with `VYRN_WASM_MANIFEST=write`. Timing, `rfcs/bench-0104/harness/run.py` with the
new `vyrn-wasm2c` contestant, RFC-0104's timing sizes, medians of five, one
machine, contestants interleaved per program:

| program | native (text-IR) | wasmtime 46 | wasm2c + clang | wasm2c ÷ native | make, native / wasm / wasm2c |
|---|---|---|---|---|---|
| nbody, 25 M steps | 0.95 s | 2.23 s | 1.54 s | 1.6x | 0.63 s / 0.03 s / 1.47 s |
| spectral-norm, n = 5500 | 1.06 s | 4.13 s | 2.31 s | 2.2x | 0.66 s / 0.03 s / 1.54 s |
| fannkuch, n = 11 | 2.09 s | 3.98 s | 3.93 s | 1.9x | 0.62 s / 0.02 s / 1.48 s |
| binary-trees, depth 18 | 2.14 s | 1.05 s | 1.07 s | 0.5x | 0.66 s / 0.03 s / 1.43 s |
| fasta, n = 5 M | 0.80 s | 0.91 s | 0.76 s | 0.9x | 1.22 s / 0.03 s / 1.77 s |
| reverse-complement, fasta n = 4 M | 0.38 s | 1.05 s | 0.97 s | 2.6x | 0.66 s / 0.03 s / 1.49 s |
| k-nucleotide, fasta n = 400 k | 0.13 s | 0.29 s | 0.20 s | 1.5x | 0.80 s / 0.04 s / 1.63 s |

The spread of the wasmtime column on spectral-norm was 47 percent (the
others under 15), so read that cell against RFC-0125 M1's 2.83 s. The three
kernels hold M1's shape within the run's noise. binary-trees under the route
is at its wasmtime time, which is what M4's gate asks of the native route
once the route is this one, and it is the allocator of step 2 running natively
for the first time. reverse-complement's 2.6x and k-nucleotide's 1.5x are the
string and map families the route now runs from the wasm, before steps 4 and
5 rewrite them; they are the next numbers to move, and the route is not what
moves them. Not in this slice: an `extern fn` (RFC-0012) has no stub on the
route, so `externdemo.vyrn` fails at link rather than trapping with the
canonical message as the text-IR route's stub does; the shim's audit, poison
and the residue ratchet's re-basing, which wait for the decision.

**Results, step 4 (2026-09-02, branch `track-l`).** `str_new`, `concat`,
`str_append` and `str_from_bytes` are Vyrn in `std/runtime.vyrn` (`strNew`,
`strConcat`, `strAppend`, `strFromBytes`: 115 lines with their comment, over
`load32`, `store8`, `store32`, `copy`, `malloc`, `free` and `utf8Valid`), and
the wasm emitter's four bodies are deleted: 342 lines out of `direct.rs`, 55
in (four rows of the `VyrnRt` table, the wiring, and the two interned failure
messages handed to `strFromBytes` as arguments). The growth policy is the one
it replaces: an accumulator that is not the frame's is copied into a buffer of
at least 32 bytes, and a full one doubles. A doubling always leaves its size
class at four classes per power of two, so §8's class-aware `realloc` has no
in-class case on the append path; the probe is not taken, and the builder rows
below are the record either way. `strFromBytes` returns a
`Result<Int64, Int64>` whose payloads are addresses: `build_sum2` writes the
tag, one word and a zero, which is the `Result<String, String>` shape both
call sites already read, so the emitter's copy of that layout went with the
body. `concat` is a reserved name (`checker.rs`, `RESERVED`), hence
`strConcat`. The three `STRING_RUNTIME` accessors of §1.1 (`str_len`,
`str_setlen`, `str_bytes`) are the native IR's; the wasm emitter never had a
function for them, because a header read is one inline `i32.load` (`str_len`
and `cap_at` in `direct.rs`), so there is no wasm copy to delete and they
leave with the IR constants at step 3. The native route does not run the Vyrn
copies until step 3 flips it, so `std/json`'s six append sites are proved
unchanged by parity (41 of 41, three engines) and the fixtures. Gates:
fmt, workspace, kernel (3 refused, at the ratchet), lowered, the nine
`vyrn-cli` suites, parity 41 of 41 (one native run failed on a transient
`NotFound` while the shared tools tree was being emptied, and passed alone),
residue, the cross-engine generator gate, `doc --verify` (41 files unchanged),
site export 33 of 34 (the version test fails on local fixture data); the
wasm2c route gate skipped for a missing wabt. The lowered-form gate's
synthesized count is 1,281, under the
1,400 ceiling. The extra gate, `census-strings.md` §3's builder
(`out = out + piece`, N appends of one piece) and the two string programs of
step 1, under wasmtime, base and head interleaved, medians of five, on a
machine shared with other worktrees' builds; outputs compared byte for byte
between the two:

| program | base (hand-emitted) | head (`std/runtime`) |
|---|---|---|
| the builder at the census's four sizes (1,000 to 8,000 appends of ten bytes) and at a thousand times each, 15 M appends in all | 0.106 s | 0.107 s |
| the builder at 48 M appends of one byte | 0.192 s | 0.194 s |
| fasta, n = 5 M | 0.845 s | 0.847 s |
| reverse-complement, fasta n = 4 M | 0.989 s | 0.990 s |

The census's own numbers (0.08 s to 0.10 s per size) were the interpreter's
and are not comparable; the compiled rows are flat across the change, which
is §7.3's prediction. fasta and reverse-complement sit at the step-1 record
(0.89 s and 1.07 s) and RFC-0125 §1.5b's 0.93 s and 1.06 s.

**Results, step 5 (2026-09-02, branch `track-m`).** The three map families
are one body in `std/runtime.vyrn`: `mapFind`, `mapPut`, `mapReindex`,
`mapReserve`, `mapRemoveAt` and `mapKeysCopy` (272 lines with their comment,
over `load8`, `load32`, `load64`, `store32`, `store64`, `copy`, `fill`,
`malloc`, `free` and `strCmp`), and the wasm emitter's fourteen hand-emitted
map functions — the String, Int64 and pack chains of `find`, `slot`, `put` and
`reindex`, with `map_find_bytes` and `map_hash` — are deleted together with the
inline `reserve`, `remove_at` and `keys_copy` at their single sites: 1,194
lines out of `direct.rs`, 131 in (five rows of the `VyrnRt` table,
`MapKey::kind`, the call sites). The key layout is a pair of constants the
emitter passes at every call, `kind` and `klen`: 0 for a String column
(FNV-1a to the NUL, `strCmp`), 1 for an `Int64` column (SplitMix64's
finalizer, the bits), 2 for a packed user key of `klen` bytes (FNV-1a over
them, a byte compare), and 3 for `tallyBytes`'s byte window against a String
column (RFC-0116), which was `map_find_bytes` and is `mapFind` with the
window's length as `klen`. The key travels as an `Int64` whatever the layout,
the value for kind 1 and an address zero-extended for the rest, so one
signature serves the four and the emitter spills nothing. So the three
instantiations are not instantiations: not RFC-0023 fn values (a `hash` and an
`eq` parameter would put a call behind every compare, and the probe loop is
the row below) and not three entry points; the layout is a compare, and a map
program carries the body once. `knucleotide.wasm` is 14,146 bytes at base and
15,388 at head, because an Int64-keyed program now carries the byte lane and
`strCmp` too. `reserve`, `remove_at` and `keys_copy` are functions for the
first time on this backend, because the header is one fixed 32-byte shape
(`{ ptr, ptr, i64, i64, ptr }`) the module can read; the emitter keeps the
`len + 1 > cap` test in front of `mapReserve`, the release of a removed entry's
key and value in front of `mapRemoveAt`, and the per-element dup of String keys
after `mapKeysCopy`, because those three know the types and the functions do
not. The hashes are the C shim's three and stay unobservable: the columns
decide every order a program can see. `std/hash`'s `fnv1a` (§8 question 5) is
the same arithmetic over an `Array<UInt8>` value, and the runtime hashes bytes
at an address with no array to hand it; neither calls the other, and the
question closes as two functions of one arithmetic at two value boundaries,
not a layering decision. Gates: fmt, workspace, kernel (the ratchet held),
lowered (1,334 synthesized, under the 1,400 ceiling), the nine `vyrn-cli`
suites, parity 41 of 41, residue, the cross-engine generator gate, `doc
--verify`, site export 33 of 34 (the version test fails on local fixture
data); the wasm2c route gate skipped for a missing wabt. The extra gate,
k-nucleotide at RFC-0104's timing size (fasta n = 400,000, the 2 M-base THREE
sequence) under wasmtime 46, the same base `.wasm` on every row and the head
rebuilt per design, base and head interleaved, medians of five on a quiet
machine, with the three designs the row rejected on the way to the one that
ships:

| design | base (hand-emitted) | head (`std/runtime`) |
|---|---|---|
| the hash and the compare behind calls (`mapHash`, `mapKeyEq`) under one probe body | 0.30 s | 0.36 s |
| the `Int64` lane written into the one probe body, calls only for the byte layouts | 0.28 s | 0.34 s |
| two lane functions behind a `mapSlot` dispatcher | 0.28 s | 0.34 s |
| `mapFind` and `mapPut` choose the lane themselves (ships), round 1 | 0.283 s | 0.299 s |
| round 2 | 0.283 s | 0.306 s |
| round 3 | 0.284 s | 0.297 s |

The rejected rows say what a level costs: a wasm call under wasmtime 46 is
about four nanoseconds, this program makes fourteen million probes, and every
function between the caller and the probe loop was one call per probe, which
is the 20 percent, and nothing took it back: `wasmtime run -O help` at 46
lists no inlining option, and the rows say none ran (§7.3 assumed one, and
the record now says otherwise for this engine). What ships is
the hand-emitted copy's two levels, find then slot, the layout chosen by one
compare in `mapFind` and one in `mapPut`. The head is 5 percent over the base,
outside this run's 2 percent spread, and 0.297 s against the 0.29 s step 3's
table recorded for the same row on this machine; the 5 percent is the two
compares and the two extra arguments per probe. A layout-specific entry point
would remove them and put the probe loop in the module a second time, and
this step does not take it. `finitekeys`, `heapkey` and `floatkey`: the second
and third are RFC-0117's compile-time refusals and print the same line as
before; `finitekeys` is a String-keyed map and is byte-identical across the
three engines under parity and the fixtures. The interpreter keeps its own
`Map` (§5.1), as at every step before this one.

**Results, step 6 (2026-09-02, branch `track-p`).** The array family has
runtime functions for the first time in any engine (§1.1, Arrays): `arrPush`,
`arrReserve`, `arrAppend`, `arrCopyFrom` and `arrClear` are Vyrn in
`std/runtime.vyrn` (136 lines with their comment, over `load32`, `load64`,
`store32`, `store64`, `copy`, `malloc` and `free`), and the wasm emitter's
five inline copies are deleted: 388 lines out of `direct.rs`, 122 in (five
rows of the `VyrnRt` table, `arr_recv`, which parks the receiver and allocates
the result slot for all five, and the call sites). The element type never
reaches the module. The emitter passes the element STRIDE as a constant, the
way the maps are passed a layout, and each function reads the triple at `src`
and writes the rebuilt triple at `dst`, a fresh frame slot, because `xs.push(v)`
is `xs = @push(xs, v)` and the write-back is the assignment. Alignment is not
passed: `malloc` is eight-aligned and no element aligns coarser. The growth
policy is the one it replaces (double from four; `append` to the larger of
the doubling and the need; `reserve` and `copyFrom` to exactly the need), the
products are `Int64` and `malloc` judges them, so the one trap the family has
is still `out of memory` at the same request. `arrPush` does all but the
element store, which the emitter keeps because it knows the type, and answers
the buffer a growth left behind: the emitter frees it AFTER the store, for the
reason the inline copy gave (`std/hash` reads the array inside its own
`push`). The `SmallArray` push (RFC-0056) stays inline: its header is a
different shape with two live states, and it is not this family. `at` stays
inline, measured: a throwaway build (not kept) put the bounds check and the
address arithmetic of an `Array` read behind one call to a module function,
`arrAt(data, len, i, stride)`, and nbody at 25 M steps went from 1.95 s to
7.28 s under wasmtime 46, fannkuch at n = 11 from 3.43 s to 5.91 s — step 5's
finding again, on the hottest path there is: wasmtime 46 inlines nothing
across the call, and every value live across it is spilled. So `a[i]` is the
check-and-branch of RFC-0125 M1 at its site, with the one trap site per
function that M1 measured, and it is not a candidate for the module until the
engine inlines. Gates: fmt, workspace, kernel (the ratchet held), lowered
(1,242 synthesized, under the 1,400 ceiling), the nine `vyrn-cli`
suites, parity 41 of 41, residue, the cross-engine generator gate, `doc
--verify`, site export 33 of 34 (the version test fails on local fixture
data); the wasm2c route gate skipped for a missing wabt. The extra gate, the
five programs at RFC-0104's timing sizes under wasmtime 46, the same base
`.wasm` per row, base and head interleaved, medians of five, outputs byte-equal
between the two, on a machine shared with other worktrees' builds:

| program | base (inline) | head (`std/runtime`) | recorded (RFC-0125 M1, Cranelift) |
|---|---|---|---|
| nbody, 25 M steps | 1.974 s | 1.960 s | 1.98 s |
| spectral-norm, n = 5500 | 2.845 s | 2.849 s | 2.83 s |
| fannkuch, n = 11 | 3.413 s | 3.403 s | 3.58 s |
| binary-trees, depth 18 | 0.845 s | 0.833 s | 0.88 s (§1.4) |
| k-nucleotide, fasta n = 400 k | 0.298 s | 0.289 s | 0.297 s (step 5) |

The three kernels do not push, so they are the no-change check, and they sit
on M1's numbers. binary-trees and k-nucleotide are the push-heavy rows, and
each is inside its run's spread of the base: a push that fits its capacity
now pays one call it did not pay before, and the row does not see it, which
is §7.3's prediction for a function that is not on a per-element path.

---

## 7. What it costs

### 7.1 Deleted

| file | what | lines |
|---|---|---|
| `toolchain.rs` | the C shim string: allocator, audit, strings, maps, I/O, time, tasks, generator host | 1,073 → about 200 (the WASI host: `fd_write`, `fd_read`, `path_open`, `clock_time_get`, `random_get`, `proc_exit`, `main`, and the native thread pool for `spawn`) — **about 870 deleted** |
| `toolchain.rs` | `extern_trap_stubs` and the shim cache | kept; they are the toolchain, not the runtime |
| `direct.rs` | `fn runtime`, 55 functions | 4,205 → about 150 (the `Wasi` import table, the trap wording interning, the reserved cells) — **about 4,050 deleted** |
| `direct.rs` | the `Rt` table and `slots` | 414 → about 40 (the module's export table, read from the compiled runtime) — **about 370 deleted** |
| `direct.rs` | inline array, map-reserve, map-remove, map-keys, stream-close and region emission at call sites (step 5, 6, 8) | not counted here; RFC-0125 M3 owns the emitter walk, and these become calls when the core does |
| `lib.rs` | the six IR constants | 420 → 0 — **420 deleted** |
| `lib.rs` | the 231 `@__vyrn_*` call lines and the seven formatted helpers | deleted with the LLVM emitter if §2.5's one-emitter route holds; kept as calls into the same module if the two-emitter fallback is taken |
| `interp.rs` | `parse_int`, the three `from_utf8` sites, `crate::regex`'s runner | about 200, replaced by six calls through §5.1 |
| `trap.rs` | — | unchanged: it is already the one table |

About 5,900 lines deleted on the one-emitter route, about 5,700 on the
fallback.

### 7.2 Written

The Vyrn module is the C bodies, which are the shortest of the three copies,
plus the array family that has no function anywhere today:

| family | C body lines today | Vyrn estimate | why the estimate |
|---|---|---|---|
| allocator | 48 (without the audit) | 80 | the class arithmetic is the same; `grow` and the trap are explicit |
| strings | 41 in C, 168 in IR | 250 | `int_str`, `parse_i64`, `utf8valid` and `regex_run` have no C body to transcribe; the wasm bodies less their instruction overhead |
| arrays | 0 | 120 | six functions at twenty lines |
| maps | 133 | 200 | the three layouts share one body with a `keyBytes` per layout, so one chain rather than three |
| I/O | 121 in C, 92 in IR | 300 | `open_at` through the preopen table and `err3` are the wasm shapes; there is no libc to lean on |
| time, randomness | 40 | 40 | the fixed-clock arithmetic and two imports |
| traps, depth | 20 | 40 | one function and one counter |
| regions | 131 in IR | 60 | §4.3 is shorter than a side list |
| tasks | 13 (eager) | 20 | the native pool stays in the host |
| **total** | **638 + 420** | **about 1,100** | |

About 1,100 lines of Vyrn against about 5,900 of Rust, C and IR. The ratio
is not the point; the count of copies is. After M4 a runtime rule is one
function in one file, and the parity corpus stops being what keeps three
transcriptions equal.

### 7.3 What it costs at run time

Nothing measured yet, and one thing predicted. The module is compiled by the
same emitter as the program and runs in the same engine, so a runtime function
is a wasm call today and a wasm call after; Cranelift's inliner declined
`cell` at fifty instructions (RFC-0125 M1, spectral-norm), so it will decline
`malloc` too, and the release route's clang will inline what it inlines
today. The prediction is that steps 1, 2 and 4 hold every RFC-0125 §1.4, §1.5b
and M1 number within noise, and each step's extra gate is that prediction
written as a test.

---

## 8. Open questions

1. **`region`'s spelling.** §4.3 makes the arena real under either `region
   { .. }` or a `std/runtime` `Arena` type. Keeping the syntax keeps the seven
   corpus uses and the census; a type makes the arena a value the linear
   judgment tracks like any other and removes `region_depth` from the
   emitters. Not decided here; the runtime is the same in both.
2. **Class-aware `realloc`.** §4.2 permits it and defers it to a probe. The
   probe is `census-strings.md`'s append builder at the sizes where doublings
   stay in class.
3. **The route.** Step 3 assumes RFC-0125 §2.5's release route, wasm2c and
   clang, because M1 measured it inside the gate. If the two-emitter fallback
   is taken instead, the LLVM emitter keeps calling the same Vyrn module
   through the text-IR path, and `lib.rs`'s 231 call lines become calls into
   compiled Vyrn rather than into C. §7.1 counts both.
4. **Threads.** Tasks are eager in the module. The native pool of `spawn`
   stays in the 200-line host until the wasm threads proposal is available on
   the chosen route, and `Task<T>`'s linearity (RFC-0095) is unchanged. Whether
   a host-side pool is still "runtime in Vyrn" is a definition, and this
   document says no: it is the host.
5. **`std/hash`'s `fnv1a`.** It is a fourth copy of the String Map's hash, in
   Vyrn. After step 5 the Map should call it rather than carry a fifth, which
   makes `std/hash` an import of `std/runtime`; whether the standard library
   may be imported by the runtime module, or the function moves the other way,
   is a layering question this document leaves to step 5. *Closed at step 5
   (§6, results): the two are one arithmetic at two value boundaries —
   `fnv1a` takes an `Array<UInt8>` value, the map hashes bytes at an address
   and has no array to hand it — so neither calls the other and no layering
   decision was needed.*
