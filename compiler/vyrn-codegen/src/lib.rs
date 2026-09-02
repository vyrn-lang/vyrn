//! Textual LLVM IR backend for the Vyrn v0 subset.
//!
//! This emits LLVM IR as a string — no LLVM libraries required to *produce* it.
//! Feed the output to a `clang`/`llc` (LLVM 15+, opaque pointers) to get a
//! native object/executable:
//!
//! ```text
//! vyrn emit-ir prog.vyrn > prog.ll
//! clang prog.ll -o prog
//! ```
//!
//! Local variables use `alloca`/`load`/`store` (LLVM's `mem2reg` promotes them
//! to SSA registers), which keeps the emitter simple. `&&`/`||` short-circuit
//! via branches + `phi`, matching the interpreter in [`vyrn_frontend::interp`].

pub mod direct;
pub mod layout;
pub mod toolchain;
pub mod wasm;

use std::collections::HashMap;
use std::fmt::Write;

use vyrn_frontend::ast::*;
use vyrn_frontend::own::DropKind;
/// RFC-0101 M4's exit vocabulary, shared with `vyrn-lower` and the other two
/// engines so the placement and the walks are compared without a translation.
use vyrn_frontend::own::Exit as ExitKind;
use vyrn_frontend::types::solve_param;
use vyrn_frontend::types::INT32;

/// LLVM IR for the region/arena runtime (see the preamble comment in `emit`).
///
/// Two ways out of a region and the difference is the free. `__vyrn_region_exit`
/// pops the frame AND releases its chain, which is what a fall-through (and a
/// `break`/`continue`, whose escapes RFC-0004's `region_store_guard` covers) wants.
/// `__vyrn_region_pop` only pops, and it exists because a `return` out of a region
/// hands the value it carries to its caller — the escape guard examines stores into
/// named bindings, not return values, so `return a + b` is not covered by anything.
/// Popping without freeing gives that value up: the frame's other blocks leak, and
/// the one the return carried is the caller's to release. Before this existed,
/// `return` emitted neither call: the frame was never popped, so the 65th call to a
/// function returning out of a region printed `error: region nesting exceeds 64`
/// where every other engine printed an answer.
///
/// **The chain is not in the blocks at all**, which is what makes the sentence
/// above true. A block the arena hands out is exactly what `__vyrn_malloc`
/// returned — no header, no trailer, not even padding — so `@__vyrn_str_free`,
/// which frees `s - 16`, the `String` header it wrote, hands `free` a pointer
/// `malloc` gave it. While the link lived at the FRONT the user pointer was 8
/// bytes into the block, and a `String` returned out of a region corrupted the
/// native heap the moment its caller released it (RFC-0096 M3, defect 4). The
/// arena still frees at the closing brace what no return carried out, so the
/// partition PR #129 states holds: one owner per block, on both paths.
///
/// What holds the chain is a **side vector of block pointers**, one per frame,
/// grown by doubling. PR #140's trailer kept the same invariant and cost 16
/// bytes an allocation where the front link cost 8; the vector costs 8 exactly,
/// with no rounding, because it is not part of the request. On the census's
/// deferral shape — 2,000,000 concatenations under one region — native peak
/// working set is 100,876,288 B with the front link, 132,493,312 B with the
/// trailer and **116,731,904 B here**: half the trailer's cost given back, and
/// the invariant kept. The vector is the arena's own bookkeeping and never
/// reaches a user, so `region_exit` frees it after its blocks and `region_pop`
/// frees it instead of them.
///
/// A block the arena owns may still never be `realloc`'d: the vector holds the
/// address `free` will be handed, so a moved block would dangle it exactly as it
/// dangled the trailer. `Stmt::Assign` refuses the in-place append inside a
/// region for that reason, and it still must.
///
/// The arena stack is `thread_local` (RFC-0025): `region { .. }` is memory
/// management, not an effect, so an isolated task may use it — and with tasks
/// on real OS threads a shared stack would race. Per-thread stacks keep every
/// region block self-contained on its own thread. On single-threaded targets
/// (wasm32-wasip1) LLVM lowers TLS to ordinary globals, so the shared IR is
/// unchanged in behavior there.
/// The call-depth counter every emitted function's prologue bumps
/// ([`vyrn_frontend::interp::CALL_DEPTH_LIMIT`], audit A5.3).
///
/// Built rather than written out, so the number in the message and the number in
/// the comparison are the same number — a trap whose wording drifts from the
/// check reads exactly like a check that never fires.
///
/// `thread_local` for the same reason the region stack is (RFC-0025): a spawned
/// task recurses on its OWN stack, so it gets its own budget, and a shared
/// counter would race. LLVM lowers TLS to an ordinary global on wasm32-wasip1.
///
/// The exit is a plain decrement with no floor. It cannot go negative: the
/// emitter puts exactly one in front of every `ret` a prologue's function has,
/// and a path that does not reach a `ret` (a trap) ends the process.
fn call_depth_runtime() -> String {
    let limit = vyrn_frontend::interp::CALL_DEPTH_LIMIT;
    let (msg, len) = llvm_str(&vyrn_frontend::trap::line(
        &vyrn_frontend::trap::call_depth(),
    ));
    format!(
        "\
@__vyrn_call_depth = thread_local global i64 0
@.trap.calldepth = private unnamed_addr constant [{len} x i8] c\"{msg}\"

define internal void @__vyrn_call_enter() {{
entry:
  %d = load i64, ptr @__vyrn_call_depth
  %d1 = add i64 %d, 1
  %over = icmp sgt i64 %d1, {limit}
  br i1 %over, label %trap, label %ok
trap:
  %e = call ptr @__vyrn_stderr()
  %w = call i32 @fputs(ptr @.trap.calldepth, ptr %e)
  call void @exit(i32 1)
  unreachable
ok:
  store i64 %d1, ptr @__vyrn_call_depth
  ret void
}}

define internal void @__vyrn_call_exit() {{
entry:
  %d = load i64, ptr @__vyrn_call_depth
  %d1 = sub i64 %d, 1
  store i64 %d1, ptr @__vyrn_call_depth
  ret void
}}

"
    )
}

/// The region runtime, with [`vyrn_frontend::interp::REGION_MAX`] filled in.
///
/// The number was written five times in the text below — three array lengths,
/// one comparison and the trap's own wording — plus a sixth as the hand-counted
/// byte length of that wording, and again in the other backend and in the
/// interpreter. Eight copies of one fact, and the two backends' comparisons had
/// already drifted apart in signedness. Now the text has holes and one constant
/// fills them, the way [`call_depth_runtime`] already builds its own number in.
fn region_runtime() -> String {
    let n = vyrn_frontend::interp::REGION_MAX;
    let (msg, len) = llvm_str(&vyrn_frontend::trap::line(
        &vyrn_frontend::trap::region_depth(),
    ));
    REGION_RUNTIME
        .replace("$NEST", &n.to_string())
        .replace("$TRAPLEN", &len.to_string())
        .replace("$TRAPTEXT", &msg)
}

const REGION_RUNTIME: &str = "\
@__vyrn_region_sp = thread_local global i64 0
@__vyrn_region_blocks = thread_local global [$NEST x ptr] zeroinitializer
@__vyrn_region_lens = thread_local global [$NEST x i64] zeroinitializer
@__vyrn_region_caps = thread_local global [$NEST x i64] zeroinitializer
@.trap.regiondepth = private unnamed_addr constant [$TRAPLEN x i8] c\"$TRAPTEXT\"

define void @__vyrn_region_enter() {
entry:
  %sp = load i64, ptr @__vyrn_region_sp
  %over = icmp uge i64 %sp, $NEST
  br i1 %over, label %trap, label %ok
trap:
  %e = call ptr @__vyrn_stderr()
  %w = call i32 @fputs(ptr @.trap.regiondepth, ptr %e)
  call void @exit(i32 1)
  unreachable
ok:
  %slot = getelementptr [$NEST x ptr], ptr @__vyrn_region_blocks, i64 0, i64 %sp
  store ptr null, ptr %slot
  %lenp = getelementptr [$NEST x i64], ptr @__vyrn_region_lens, i64 0, i64 %sp
  store i64 0, ptr %lenp
  %capp = getelementptr [$NEST x i64], ptr @__vyrn_region_caps, i64 0, i64 %sp
  store i64 0, ptr %capp
  %sp1 = add i64 %sp, 1
  store i64 %sp1, ptr @__vyrn_region_sp
  ret void
}

define ptr @__vyrn_region_alloc(i64 %n) {
entry:
  %raw = call ptr @__vyrn_malloc(i64 %n)
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  %slot = getelementptr [$NEST x ptr], ptr @__vyrn_region_blocks, i64 0, i64 %idx
  %lenp = getelementptr [$NEST x i64], ptr @__vyrn_region_lens, i64 0, i64 %idx
  %capp = getelementptr [$NEST x i64], ptr @__vyrn_region_caps, i64 0, i64 %idx
  %len = load i64, ptr %lenp
  %cap = load i64, ptr %capp
  %full = icmp eq i64 %len, %cap
  br i1 %full, label %grow, label %put
grow:
  %dbl = shl i64 %cap, 1
  %empty = icmp eq i64 %cap, 0
  %newcap = select i1 %empty, i64 16, i64 %dbl
  %bytes = shl i64 %newcap, 3
  %old = load ptr, ptr %slot
  %grown = call ptr @__vyrn_realloc(ptr %old, i64 %bytes)
  store ptr %grown, ptr %slot
  store i64 %newcap, ptr %capp
  br label %put
put:
  %vec = load ptr, ptr %slot
  %el = getelementptr ptr, ptr %vec, i64 %len
  store ptr %raw, ptr %el
  %len1 = add i64 %len, 1
  store i64 %len1, ptr %lenp
  ret ptr %raw
}

define void @__vyrn_region_pop() {
entry:
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  store i64 %idx, ptr @__vyrn_region_sp
  %slot = getelementptr [$NEST x ptr], ptr @__vyrn_region_blocks, i64 0, i64 %idx
  %vec = load ptr, ptr %slot
  call void @__vyrn_free(ptr %vec)
  store ptr null, ptr %slot
  ret void
}

define void @__vyrn_region_exit() {
entry:
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  store i64 %idx, ptr @__vyrn_region_sp
  %slot = getelementptr [$NEST x ptr], ptr @__vyrn_region_blocks, i64 0, i64 %idx
  %vec = load ptr, ptr %slot
  %lenp = getelementptr [$NEST x i64], ptr @__vyrn_region_lens, i64 0, i64 %idx
  %len = load i64, ptr %lenp
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i1, %body ]
  %spent = icmp uge i64 %i, %len
  br i1 %spent, label %done, label %body
body:
  %el = getelementptr ptr, ptr %vec, i64 %i
  %blk = load ptr, ptr %el
  call void @__vyrn_free(ptr %blk)
  %i1 = add i64 %i, 1
  br label %loop
done:
  call void @__vyrn_free(ptr %vec)
  store ptr null, ptr %slot
  ret void
}

define void @__vyrn_region_pop_except(ptr %keep) {
entry:
  %base = getelementptr i8, ptr %keep, i64 -16
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  store i64 %idx, ptr @__vyrn_region_sp
  %slot = getelementptr [$NEST x ptr], ptr @__vyrn_region_blocks, i64 0, i64 %idx
  %vec = load ptr, ptr %slot
  %lenp = getelementptr [$NEST x i64], ptr @__vyrn_region_lens, i64 0, i64 %idx
  %len = load i64, ptr %lenp
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i1, %next ]
  %spent = icmp uge i64 %i, %len
  br i1 %spent, label %done, label %body
body:
  %el = getelementptr ptr, ptr %vec, i64 %i
  %blk = load ptr, ptr %el
  %escaped = icmp eq ptr %blk, %base
  br i1 %escaped, label %next, label %freeit
freeit:
  call void @__vyrn_free(ptr %blk)
  br label %next
next:
  %i1 = add i64 %i, 1
  br label %loop
done:
  call void @__vyrn_free(ptr %vec)
  store ptr null, ptr %slot
  ret void
}

";

/// A boxed stream (RFC-0075 M2c, re-hosted by RFC-0090 M3). A lazy combinator
/// owns the stream it wraps, and a `Stream<T>` cannot be a field of anything —
/// M1 refuses it, because a field would erase the disposal obligation. So the
/// source lives in one heap box and `std/stream` holds its ADDRESS in its own
/// cursor slot: `boxStream` puts it there, `pullAt` asks it for an element, and
/// `unboxStream` takes it back out so the wrapper's step can `close` it in Vyrn.
///
/// The box is `{ i64 magic, Stream }`. `unboxStream` clears the magic before it frees,
/// so a second `unboxStream` of one address is the trap below rather than a stream
/// read out of freed memory. That check is the whole reason the magic exists:
/// an address is an ordinary `Int64` a program can spell.
const STREAM_RUNTIME: &str = "\
$NOBOX

define void @__vyrn_stream_nobox() {
entry:
  %e = call ptr @__vyrn_stderr()
  %r = call i32 @fputs(ptr @.fmt.nob, ptr %e)
  call void @exit(i32 1)
  unreachable
}

define ptr @__vyrn_stream_box(i64 %a) {
entry:
  %z = icmp eq i64 %a, 0
  br i1 %z, label %bad, label %chk
chk:
  %p = inttoptr i64 %a to ptr
  %m = load i64, ptr %p
  %ok = icmp eq i64 %m, 3735928559
  br i1 %ok, label %good, label %bad
bad:
  call void @__vyrn_stream_nobox()
  unreachable
good:
  %d = getelementptr i8, ptr %p, i64 8
  ret ptr %d
}

";

/// The strict UTF-8 validator: Björn Höhrmann's DFA over the `@__vyrn_utf8d`
/// table, which is emitted separately in `emit` and shared with the direct wasm
/// backend (RFC-0077 M2g). It matches Rust's `from_utf8` exactly, which is what
/// makes `stringFromBytes` the single gate on what a `String` may hold.
///
/// An ASCII prefix is skipped eight bytes at a time first (RFC-0125 §1, the
/// output-path trio): a word with no high bit set is eight bytes the DFA would
/// walk from state 0 back to state 0, so the DFA starts where the first
/// non-ASCII word does, in state 0, and answers the same thing. fasta and
/// reverse-complement validate one all-ASCII line per line of output.
///
/// It used to have company: the hex, base64 and percent codecs were ~520 lines of
/// hand-written IR here, with the hex-digit helpers and the base64 alphabet table.
/// RFC-0078 M4c routed those six builtins to `std/codecs`, so what remains is the
/// primitive the Vyrn implementations are written ON — through `stringFromBytes` —
/// rather than one of the operations they duplicated.
const ENCODING_RUNTIME: &str = "\
define i1 @__vyrn_utf8valid(ptr %s, i64 %len) {
entry:
  br label %fast
fast:
  %fi = phi i64 [ 0, %entry ], [ %fi2, %fastnext ]
  %rem = sub i64 %len, %fi
  %has8 = icmp uge i64 %rem, 8
  br i1 %has8, label %fastload, label %slow
fastload:
  %wp = getelementptr i8, ptr %s, i64 %fi
  %w = load i64, ptr %wp, align 1
  %hi = and i64 %w, -9187201950435737472
  %ascii = icmp eq i64 %hi, 0
  br i1 %ascii, label %fastnext, label %slow
fastnext:
  %fi2 = add i64 %fi, 8
  br label %fast
slow:
  br label %loop
loop:
  %i = phi i64 [ %fi, %slow ], [ %i2, %body ]
  %st = phi i64 [ 0, %slow ], [ %st2, %body ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %fin, label %body
body:
  %bp = getelementptr i8, ptr %s, i64 %i
  %b = load i8, ptr %bp
  %bz = zext i8 %b to i64
  %tp = getelementptr i8, ptr @__vyrn_utf8d, i64 %bz
  %ty = load i8, ptr %tp
  %tyz = zext i8 %ty to i64
  %a = add i64 256, %st
  %idx = add i64 %a, %tyz
  %sp = getelementptr i8, ptr @__vyrn_utf8d, i64 %idx
  %sv = load i8, ptr %sp
  %st2 = zext i8 %sv to i64
  %i2 = add i64 %i, 1
  br label %loop
fin:
  %ok = icmp eq i64 %st, 0
  ret i1 %ok
}

";

/// The String value header (RFC-0089 M1a). A `String` is still one `ptr`, and
/// that pointer still addresses NUL-terminated UTF-8 — every C sink (`printf`,
/// `strcmp`, `fopen`) and the extern ABI keep working unchanged. What changed is
/// what sits IN FRONT of it: sixteen bytes holding `{ i64 len, i64 cap }`.
///
/// `len` is the byte length. `s.byteLength` is a load, `a + b` reads two loads
/// instead of scanning both operands, and RFC-0081's `str_append` no longer keeps
/// a length beside the slot — the header IS that length.
///
/// `cap` is the allocated byte capacity, and `cap == STR_STATIC` means
/// **static**: a literal in the data segment, never `realloc`'d and never freed.
/// RFC-0077 M6 put an eight-byte class header on every heap block precisely
/// because a headerless String could not answer this question at a drop site.
///
/// The header is BEHIND the pointer rather than beside it (a `{ptr, len, cap}`
/// value triple, as `Array<T>` is) for two reasons measured here:
///   * an `Option<String>` payload is one word, so a three-word String would box
///     — a `malloc` per `Some(s)`, which moves a census row (`memory.rs` §14);
///   * two aliases of one String share one header, so an append through one is
///     never a stale length in the other. Until the conventions land (RFC-0089
///     M2) aliasing is still legal, and a triple would go stale.
const STR_HDR: i64 = 16;

/// The `cap` a data-segment literal carries: all bits set, which no allocation
/// can ever return.
///
/// It was `0` until the audit measured what that costs. `0` is also the capacity
/// of an EMPTY String built at run time — `""` out of a `slice`, a `join` of
/// nothing, a concat of two empties — so `@__vyrn_str_free` read every one of
/// them as a literal and gave nothing back. Three million empty concats held
/// 88.4 MB against the 3.2 MB the same program holds when the strings are one
/// byte long. A literal is a fact about WHERE the bytes live, and no capacity
/// can state it, so the sentinel moved off the range capacities use.
///
/// All-ones rather than a flag bit because the free is an equality test: a heap
/// block would have to answer `cap == 2^64-1` to be mistaken for a literal. The
/// reserve arithmetic in [`Gen::emit_str_append`] compares capacities UNSIGNED,
/// where all-ones reads as room for everything — it never sees a literal (step 1
/// copies a buffer this path does not own before step 2 reads its capacity), and
/// an equality test keeps that a separate question rather than a shared one.
const STR_STATIC: i64 = -1;

/// `bytes(s)`: an `Array<UInt8>` ({ptr,len,cap}, i8 stride — RFC-0014 M2) of a
/// string's raw UTF-8 bytes. The VIEW every Vyrn string routine is written on, and
/// irreducible for that reason (RFC-0078 M4a's category).
///
/// `chars(s)` shared this block until RFC-0078 M4c: its two-pass decoder was 82
/// lines of IR and is now `std/text`'s `decodeUtf8`.
///
/// `@__vyrn_str_len` / `@__vyrn_str_new` / `@__vyrn_str_free` are the header
/// accessors. They are `define`s rather than open-coded GEPs so the decision
/// lives in one place; `-O2` inlines all three.
const STRING_RUNTIME: &str = "\
define i64 @__vyrn_str_len(ptr %s) {
entry:
  %h = getelementptr i8, ptr %s, i64 -16
  %n = load i64, ptr %h
  ret i64 %n
}

define noalias ptr @__vyrn_str_new(i64 %len, i64 %cap) {
entry:
  %tot = add i64 %cap, 17
  %base = call ptr @__vyrn_malloc(i64 %tot)
  store i64 %len, ptr %base
  %cp = getelementptr i8, ptr %base, i64 8
  store i64 %cap, ptr %cp
  %s = getelementptr i8, ptr %base, i64 16
  %e = getelementptr i8, ptr %s, i64 %len
  store i8 0, ptr %e
  ret ptr %s
}

define void @__vyrn_str_setlen(ptr %s, i64 %n) {
entry:
  %h = getelementptr i8, ptr %s, i64 -16
  store i64 %n, ptr %h
  ret void
}

define void @__vyrn_str_free(ptr %s) {
entry:
  %cp = getelementptr i8, ptr %s, i64 -8
  %cap = load i64, ptr %cp
  %static = icmp eq i64 %cap, -1
  br i1 %static, label %done, label %heap
heap:
  %base = getelementptr i8, ptr %s, i64 -16
  call void @__vyrn_free(ptr %base)
  br label %done
done:
  ret void
}

define noalias ptr @__vyrn_str_concat(ptr %a, ptr %b) {
entry:
  %la = call i64 @__vyrn_str_len(ptr %a)
  %lb = call i64 @__vyrn_str_len(ptr %b)
  %n = add i64 %la, %lb
  %r = call ptr @__vyrn_str_new(i64 %n, i64 %n)
  call void @llvm.memcpy.p0.p0.i64(ptr %r, ptr %a, i64 %la, i1 false)
  %at = getelementptr i8, ptr %r, i64 %la
  call void @llvm.memcpy.p0.p0.i64(ptr %at, ptr %b, i64 %lb, i1 false)
  ret ptr %r
}

define {ptr, i64, i64} @__vyrn_str_bytes_range(ptr %s, i64 %start, i64 %end) {
entry:
  %len = sub i64 %end, %start
  %data = call ptr @__vyrn_malloc(i64 %len)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %ret, label %body
body:
  %off = add i64 %start, %i
  %sp = getelementptr i8, ptr %s, i64 %off
  %b = load i8, ptr %sp
  %dp = getelementptr i8, ptr %data, i64 %i
  store i8 %b, ptr %dp
  %i2 = add i64 %i, 1
  br label %loop
ret:
  %r0 = insertvalue {ptr, i64, i64} undef, ptr %data, 0
  %r1 = insertvalue {ptr, i64, i64} %r0, i64 %len, 1
  %r2 = insertvalue {ptr, i64, i64} %r1, i64 %len, 2
  ret {ptr, i64, i64} %r2
}

; The whole string is the range 0..len, so there is ONE copy loop here and not
; two (RFC-0113). The caller of the three-argument form has already been bounds
; checked by the IR that emits the call.
define {ptr, i64, i64} @__vyrn_str_bytes(ptr %s) {
entry:
  %len = call i64 @__vyrn_str_len(ptr %s)
  %r = call {ptr, i64, i64} @__vyrn_str_bytes_range(ptr %s, i64 0, i64 %len)
  ret {ptr, i64, i64} %r
}

; (The byte-copy helper was here — the `slice` builtin's copy loop, and `slice`
; was its only caller. RFC-0079 M3 routed `slice` into `std/strpred`, where the
; copy is a `while` over the byte view, so the helper went with the lowering
; rather than staying as an unreferenced definition every module still carries.
; Its name is not spelled in this comment on purpose: the test that asserts it is
; gone reads the emitted text.)
";

/// The `=~` regex runner: run a complete DFA (transition table + accepting bytes,
/// both emitted per pattern) over a NUL-terminated string, reporting a full match.
const REGEX_RUNTIME: &str = "\
define i1 @__vyrn_regex_run(ptr %s, ptr %table, i64 %start, ptr %accept) {
entry:
  br label %loop
loop:
  %st = phi i64 [ %start, %entry ], [ %next64, %cont ]
  %i = phi i64 [ 0, %entry ], [ %i1, %cont ]
  %pc = getelementptr i8, ptr %s, i64 %i
  %c = load i8, ptr %pc
  %isend = icmp eq i8 %c, 0
  br i1 %isend, label %done, label %cont
cont:
  %cz = zext i8 %c to i64
  %base = mul i64 %st, 256
  %idx = add i64 %base, %cz
  %tp = getelementptr i32, ptr %table, i64 %idx
  %nx = load i32, ptr %tp
  %next64 = sext i32 %nx to i64
  %i1 = add i64 %i, 1
  br label %loop
done:
  %ap = getelementptr i8, ptr %accept, i64 %st
  %av = load i8, ptr %ap
  %r = icmp ne i8 %av, 0
  ret i1 %r
}

";

/// Input-I/O runtime (RFC-0014). `@__vyrn_args` materializes argv[1..] as an
/// `Array<String>` triple (elements point directly at argv — never freed, per
/// RFC-0011's array-element rule). `@__vyrn_read_err`/`@__vyrn_write_err` build
/// the canonical error payloads from the `@.io.*` format globals, so the wording
/// lives in exactly one place (the codegen). The read/write/line primitives are
/// C helpers in vyrn-cli's shim; these IR helpers wrap them.
const IO_RUNTIME: &str = "\
define {ptr, i64, i64} @__vyrn_args() {
entry:
  %n = call i64 @__vyrn_args_count()
  %sz = mul i64 %n, 8
  %data = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %done = icmp uge i64 %i, %n
  br i1 %done, label %ret, label %body
body:
  %s = call ptr @__vyrn_args_get(i64 %i)
  %sl = call i64 @__vyrn_strlen(ptr %s)
  %own = call ptr @__vyrn_str_new(i64 %sl, i64 %sl)
  call void @llvm.memcpy.p0.p0.i64(ptr %own, ptr %s, i64 %sl, i1 false)
  %dp = getelementptr ptr, ptr %data, i64 %i
  store ptr %own, ptr %dp
  %i2 = add i64 %i, 1
  br label %loop
ret:
  %r0 = insertvalue {ptr, i64, i64} undef, ptr %data, 0
  %r1 = insertvalue {ptr, i64, i64} %r0, i64 %n, 1
  %r2 = insertvalue {ptr, i64, i64} %r1, i64 %n, 2
  ret {ptr, i64, i64} %r2
}

define ptr @__vyrn_read_err(ptr %path, i32 %status) {
entry:
  %is2 = icmp eq i32 %status, 2
  %is3 = icmp eq i32 %status, 3
  %f1 = select i1 %is2, ptr @.io.utf8err, ptr @.io.readerr
  %fmt = select i1 %is3, ptr @.io.nulerr, ptr %f1
  %plen = call i64 @__vyrn_str_len(ptr %path)
  %bsz = add i64 %plen, 40
  %buf = call ptr @__vyrn_str_new(i64 0, i64 %bsz)
  %n = call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr %buf, i64 %bsz, ptr %fmt, ptr %path)
  %n64 = sext i32 %n to i64
  call void @__vyrn_str_setlen(ptr %buf, i64 %n64)
  ret ptr %buf
}

define ptr @__vyrn_write_err(ptr %path) {
entry:
  %plen = call i64 @__vyrn_str_len(ptr %path)
  %bsz = add i64 %plen, 40
  %buf = call ptr @__vyrn_str_new(i64 0, i64 %bsz)
  %n = call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr %buf, i64 %bsz, ptr @.io.writeerr, ptr %path)
  %n64 = sext i32 %n to i64
  call void @__vyrn_str_setlen(ptr %buf, i64 %n64)
  ret ptr %buf
}

define ptr @__vyrn_rename_err(ptr %to, i32 %status) {
entry:
  %isx = icmp eq i32 %status, 2
  %fmt = select i1 %isx, ptr @.io.xdeverr, ptr @.io.writeerr
  %plen = call i64 @__vyrn_str_len(ptr %to)
  %bsz = add i64 %plen, 40
  %buf = call ptr @__vyrn_str_new(i64 0, i64 %bsz)
  %n = call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr %buf, i64 %bsz, ptr %fmt, ptr %to)
  %n64 = sext i32 %n to i64
  call void @__vyrn_str_setlen(ptr %buf, i64 %n64)
  ret ptr %buf
}

define noalias ptr @__vyrn_bytes_dup(ptr %data, i64 %len) {
entry:
  %buf = call ptr @__vyrn_str_new(i64 %len, i64 %len)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %cont ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %ok, label %body
body:
  %sp = getelementptr i8, ptr %data, i64 %i
  %b = load i8, ptr %sp
  %isnul = icmp eq i8 %b, 0
  br i1 %isnul, label %bad, label %cont
cont:
  %dp = getelementptr i8, ptr %buf, i64 %i
  store i8 %b, ptr %dp
  %i2 = add i64 %i, 1
  br label %loop
bad:
  call void @__vyrn_str_free(ptr %buf)
  ret ptr null
ok:
  ret ptr %buf
}

";

/// The private LLVM symbol for an `extern` import (RFC-0012). Prefixed so it
/// cannot collide with a real C symbol on the native target: the generated C
/// trap stub defines exactly this name, and the wasm import name is carried
/// separately by the `wasm-import-name` attribute (the raw Vyrn name).
pub(crate) fn extern_symbol(name: &str) -> String {
    format!("__vyrn_extern_{name}")
}

/// The RFC-0014 I/O wording, and the two readers a backend needs — the list
/// itself is [`vyrn_frontend::trap::IO`], below all three engines (RFC-0101 M5).
/// It was here, which meant the interpreter could not read it and re-spelled all
/// eight at thirteen sites.
pub use vyrn_frontend::trap::{io as io_message, io_parts as io_message_parts, IO as IO_MESSAGES};

/// The host-boundary externs of RFC-0043 (time / randomness), which lower to a
/// real shim symbol at each use site rather than to a host import. The table
/// moved to [`vyrn_frontend::trap::HOST_EXTERNS`] when RFC-0103's floor needed
/// to read it: the frontend must be able to tell a host IMPORT from a shim call,
/// and a second copy of the three names is the drift that file exists to end.
pub use vyrn_frontend::trap::host_boundary_extern;

/// The extern (JS-boundary) ABI value type for one primitive, per the RFC-0012
/// table: `Int64`/`i64`, sized ints ≤32-bit widen to `i32`, `Bool` is `i32`,
/// floats stay `double`/`float`, `String` returns as a bare `ptr`, `Unit` is a
/// missing result. `String` *parameters* are handled separately (they cross as
/// a `(ptr, len)` pair). The checker guarantees no other type reaches here.
///
/// Shared with the direct wasm backend, which maps the answer through
/// [`wasm::abi`] rather than keeping a second table: an ABI written down twice is
/// a misread argument on one backend, not a link error.
pub(crate) fn extern_abi_ll(ty: &Type) -> &'static str {
    match ty {
        Type::Int => "i64",
        Type::IntN { bits: 64, .. } => "i64",
        Type::IntN { .. } => "i32",
        Type::Float => "double",
        Type::Float32 => "float",
        Type::Bool => "i32",
        Type::Str => "ptr",
        Type::Unit => "void",
        // Unreachable: the checker restricts the extern signature domain.
        _ => "i64",
    }
}

/// The parameter list of an `extern` import's `declare`, flattened per the ABI:
/// a `String` becomes two arguments `(ptr, i64)`; every other type is its single
/// ABI value type.
fn extern_decl_params(f: &Function) -> String {
    let mut parts = Vec::new();
    for p in &f.params {
        if matches!(p.ty, Type::Str) {
            parts.push("ptr".to_string());
            parts.push("i64".to_string());
        } else {
            parts.push(extern_abi_ll(&p.ty).to_string());
        }
    }
    parts.join(", ")
}

/// Drain a just-emitted `Gen`'s higher-order outputs (RFC-0023): append each
/// lifted lambda definition once (deduped by symbol) and queue each newly
/// discovered specialization for emission.
fn drain_ho(
    gen: &mut Gen,
    out: &mut String,
    ho_queue: &mut Vec<HoInst>,
    lambda_emitted: &mut std::collections::HashSet<String>,
) {
    for (sym, def) in std::mem::take(&mut gen.lambda_defs) {
        if lambda_emitted.insert(sym) {
            out.push_str(&def);
        }
    }
    for inst in std::mem::take(&mut gen.ho_instances) {
        if !ho_queue.iter().any(|q| q.sym == inst.sym) {
            ho_queue.push(inst);
        }
    }
}

thread_local! {
    /// RFC-0076 M2: whether this module is being emitted to run as a GENERATOR
    /// under the wasm engine, where `listDir` is a host import backed by the
    /// loader's resolver. An ordinary build must keep rejecting it (the language
    /// gives `listDir` no runtime lowering), so the flag gates exactly that one
    /// branch.
    ///
    /// A thread-local rather than a `Gen` field because `Gen` is constructed at
    /// nine sites inside [`emit_with`] and not one of them has an opinion.
    static GEN_HOST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is emitting a generator-host module.
///
/// RFC-0076 M7 gave the flag a second reader: the DIRECT wasm backend emits the
/// same surface without clang, and `llt_of`'s `Code` arm is shared between them,
/// so the flag has to be the one both ask rather than a parameter one of them
/// threads.
pub(crate) fn gen_host() -> bool {
    GEN_HOST.with(|g| g.get())
}

pub(crate) fn set_gen_host(on: bool) {
    GEN_HOST.with(|g| g.set(on));
}

/// RFC-0101 M1: what each compiled backend decides an expression's type is.
///
/// The two backends derive the type of every expression themselves — `peek` and
/// its satellites on the wasm side, the `(String, Type)` return convention on
/// the textual one (RFC-0101 §1.2). Nothing outside a backend can see those
/// answers, so nothing can check them against each other or against the
/// checker's. This makes them visible, off by default, and adds no decision:
/// every hook records what the emitter was about to return anyway.
///
/// Off, the cost is one thread-local `Cell` read per expression. On, the sink
/// grows one row per typed expression per instantiation, which is why it is a
/// gate's tool and not a compiler's.
pub mod observe {
    use vyrn_frontend::ast::Type;

    /// Which engine, and which of its derivations, produced a row.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Site {
        /// `Gen::gen_expr` — the textual backend's threaded `(String, Type)`.
        Native,
        /// `Fn_::expr` — the direct wasm backend's emitting walk.
        Wasm,
        /// `Fn_::peek` — the direct wasm backend's second expression typer.
        Peek,
    }

    /// One backend answer: this node, under this instantiation, has this type.
    #[derive(Debug, Clone)]
    pub struct Row {
        pub site: Site,
        pub kind: &'static str,
        /// The tree the answer was given inside, when it is one an engine
        /// CLONED rather than one the program holds (RFC-0101 M6's second
        /// phase). `""` for an ordinary node.
        ///
        /// Two clones are left in the compiler and this is what sizes each:
        ///
        /// - `"lambda"` — `Fn_::lift_lambda` copies a lambda's body, so every
        ///   node in it, and in every projection expansion built while walking
        ///   it, is off-program by construction. The textual backend never sets
        ///   this: it lifts by walking the literal's OWN nodes.
        /// - `"pred"` — a `where` predicate lives on a `TypeDecl`, both
        ///   backends read theirs out of a cloned `types::decl_map`, and each
        ///   validation site then clones the predicate again to get past the
        ///   borrow checker. Two levels of copy, at every value boundary a
        ///   refined type crosses.
        ///
        /// M6's first phase named the first class and could not size it, and
        /// did not know the second existed. A bucket priced by size is the
        /// mistake its own ledger catches, so both are counted rather than
        /// argued.
        pub ctx: &'static str,
        /// The AST node's address — the identity `own` and `movecheck` use.
        pub node: usize,
        /// The instantiation the emitter was inside, sorted by parameter name.
        pub subst: Vec<(String, Type)>,
        pub ty: Type,
    }

    /// One body this backend decided to emit: a function and the type arguments
    /// it was emitted at.
    ///
    /// RFC-0101 M2's shadow: the two backends each run their own worklist, and
    /// nothing outside a backend has ever been able to see either list, so
    /// "`vyrn-lower` builds the instances the backends build" has been a claim
    /// with no gate under it. This makes both lists readable and adds no
    /// decision — every hook records a body the driver was about to lower anyway.
    ///
    /// A lifted lambda is deliberately NOT here. It is not a function of the
    /// program: it has no name, its body is a clone the backend synthesized, and
    /// its identity is a node address the lowering cannot key against. What that
    /// costs is written into RFC-0101 §3 M2.
    #[derive(Debug, Clone)]
    pub struct Inst {
        pub site: Site,
        pub name: String,
        pub args: Vec<Type>,
    }

    thread_local! {
        static ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static CTX: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
        static ROWS: std::cell::RefCell<Vec<Row>> = const { std::cell::RefCell::new(Vec::new()) };
        static INSTS: std::cell::RefCell<Vec<Inst>> = const { std::cell::RefCell::new(Vec::new()) };
        static CROSSINGS: std::cell::RefCell<Vec<Crossing>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Start recording on this thread, discarding anything already collected.
    pub fn start() {
        ROWS.with(|r| r.borrow_mut().clear());
        INSTS.with(|r| r.borrow_mut().clear());
        CROSSINGS.with(|r| r.borrow_mut().clear());
        ON.with(|o| o.set(true));
    }

    /// Stop recording and take what was collected.
    pub fn take() -> Vec<Row> {
        ON.with(|o| o.set(false));
        ROWS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// The instantiations recorded since [`start`]. Read after [`take`], which is
    /// what stops the recording.
    pub fn take_insts() -> Vec<Inst> {
        INSTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// One boundary crossing an engine actually made: the pair, and the rung it
    /// took (RFC-0101 §1.5's shadow).
    ///
    /// The PAIR is the identity, not a node: the two ladders reach `coerce` from
    /// different call sites, so there is no node they both stand at, and §1.5's
    /// whole claim is about the pair.
    #[derive(Debug, Clone)]
    pub struct Crossing {
        pub site: Site,
        pub from: Type,
        pub to: Type,
        pub rung: crate::Rung,
    }

    /// Record the rung an engine took. Every `return` path of a `coerce` calls
    /// this exactly once, so a rung that stops being reachable stops being
    /// recorded — which is what the corpus gate's floor is for.
    pub(crate) fn note_rung(site: Site, from: &Type, to: &Type, rung: crate::Rung) {
        if !on() {
            return;
        }
        CROSSINGS.with(|r| {
            r.borrow_mut().push(Crossing {
                site,
                from: from.clone(),
                to: to.clone(),
                rung,
            })
        });
    }

    /// The crossings recorded since [`start`]. Read after [`take`], like
    /// [`take_insts`].
    pub fn take_crossings() -> Vec<Crossing> {
        CROSSINGS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    pub(crate) fn note_inst(site: Site, name: &str, args: &[Type]) {
        if !on() {
            return;
        }
        INSTS.with(|r| {
            r.borrow_mut().push(Inst {
                site,
                name: name.to_string(),
                args: args.to_vec(),
            })
        });
    }

    pub(crate) fn on() -> bool {
        ON.with(|o| o.get())
    }

    /// Mark the rows recorded from here on as being inside a cloned tree, and
    /// give back what the mark was so the caller can put it back.
    pub(crate) fn set_ctx(v: &'static str) -> &'static str {
        CTX.with(|f| f.replace(v))
    }

    /// The expression kind a row is reported under — and for a variable, the
    /// NAME too.
    ///
    /// RFC-0101 M5 measured the residue by hand-editing this function, because
    /// `var` as one bucket said "the release receiver is the bulk" and the name
    /// said it is a tenth. A measurement that needs an edit to repeat is a
    /// measurement the next milestone will not repeat, so the name is here
    /// permanently. It is interned rather than owned: [`Row`] holds a
    /// `&'static str`, the pool is bounded by the distinct variable names in one
    /// corpus, and nothing calls this unless [`on`] — the gate — is recording.
    pub fn kind_of(e: &vyrn_frontend::ast::Expr) -> &'static str {
        use vyrn_frontend::ast::Expr as E;
        match e {
            E::Int(_) => "int",
            E::Byte(_) => "byte",
            E::Float(_) => "float",
            E::Bool(_) => "bool",
            E::Str(_) => "str",
            E::Var { name, .. } => intern(format!("var[{name}]")),
            E::Unary { .. } => "unary",
            E::Binary { .. } => "binary",
            E::Call { name, .. } => {
                if name.starts_with('@') {
                    "call@"
                } else {
                    "call"
                }
            }
            E::Match { .. } => "match",
            E::IfExpr { .. } => "ifexpr",
            E::Try { .. } => "try",
            E::StructLit { .. } => "record",
            E::Field { .. } => "field",
            E::TryConstruct { .. } => "tryconstruct",
            E::ArrayLit { .. } => "array",
            E::MapLit { .. } => "map",
            E::Spawn { .. } => "spawn",
            E::Lambda { .. } => "lambda",
            E::Consume { .. } => "consume",
        }
    }

    /// One `&'static str` per distinct string, so a `kind` can carry a name and
    /// a [`Row`] can still be `Copy`-cheap.
    fn intern(s: String) -> &'static str {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        let mut pool = POOL
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap();
        if let Some(hit) = pool.get(s.as_str()) {
            return hit;
        }
        let leaked: &'static str = Box::leak(s.into_boxed_str());
        pool.insert(leaked);
        leaked
    }

    pub(crate) fn record(
        site: Site,
        kind: &'static str,
        node: usize,
        subst: &std::collections::HashMap<String, Type>,
        ty: &Type,
    ) {
        let mut subst: Vec<(String, Type)> =
            subst.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        subst.sort_by(|a, b| a.0.cmp(&b.0));
        ROWS.with(|r| {
            r.borrow_mut().push(Row {
                site,
                kind,
                ctx: CTX.with(|f| f.get()),
                node,
                subst,
                ty: ty.clone(),
            })
        });
    }
}

/// Every `vyrn_gen` import a generator module makes: a signature in LLVM's spelling
/// and the name it imports under, which [`wasm::declare_sig`] turns into the wasm
/// one through [`wasm::abi`] — so `i1`, `i8` and `ptr` are widened in exactly one
/// place and no signature on this boundary is written twice.
///
/// LLVM's spelling and not wasm's because these WERE `declare` lines: RFC-0076 M3a
/// emitted them into the IR with `wasm-import-module` attributes, the way an
/// RFC-0012 `extern` is emitted, and M7's direct backend imports the same set
/// without a textual emitter in the way. Kept in this form because `wasm::boundary`
/// reads the emitter's own `declare` lines the same way, and one parser over one
/// spelling is what stops the two lists drifting.
///
/// The host side of every one of them is the interpreter's own code — the RFC-0054
/// piece arena, `render_code`, the splice table, the real lexer, the real linker —
/// which is what keeps the escaping, the identifier validation and the
/// shortest-roundtrip float formatting byte-identical by construction rather than
/// by testing.
pub(crate) const CODE_IMPORTS: &[(&str, &str)] = &[
    ("i64 @__vyrn_code_text(ptr)", "text"),
    ("i64 @__vyrn_code_splice(i32, i64, ptr, i64)", "splice"),
    ("i64 @__vyrn_code_raw_at(ptr, ptr, i64, i64)", "rawAt"),
    ("i64 @__vyrn_code_concat(i64, i64)", "concat"),
    ("i64 @__vyrn_code_render(i64)", "render"),
    // The M2 stash reader: `render` answers with a length and the guest
    // allocates, because the host must not allocate inside guest memory.
    ("void @__vyrn_gen_fetch(ptr)", "fetch"),
    // RFC-0076 M3b — structured host results. `reflect` asks the host for a
    // value of a known named type (`lex`, `moduleInterface`, `contractOf`) and
    // leaves it as a flat atom stream; `nextInt`/`nextStr` pull the atoms back in
    // the order the decoder walks the type. `nextStr` answers with a length and
    // the guest allocates, exactly as `render` does.
    ("void @__vyrn_gen_reflect(i64, ptr)", "reflect"),
    ("i64 @__vyrn_gen_next_int()", "nextInt"),
    ("i64 @__vyrn_gen_next_str()", "nextStr"),
    // RFC-0076 M2's mediated read, which `readFile`, `readFileBytes` and `listDir`
    // are all served out of. It used to be declared by the C shim rather than the
    // IR, because the shim was what called it; M7 has no shim, so it joins the
    // list it always belonged to.
    ("i64 @__vyrn_gen_read(ptr, i32)", "read"),
];

/// `vyrn_gen.read`'s modes, shared with the host that answers them so the two
/// spellings of "2 means listDir" are one.
/// A stream's step signature (RFC-0075 M2b), which is a function of the ELEMENT
/// type and nothing else — the cursor is two plain `Int64`s precisely so that it
/// is. Both the construction site and the loop that dispatches through it derive
/// the signature from here, because a stored `fn` value is keyed by its signature
/// and two spellings of one type would be two dispatchers.
///
/// The third parameter is the closing flag (RFC-0090 M3). A release is
/// type-erased in the runtime and the cursor slab is `std/stream`'s, so a stream
/// gives its slot back by asking its own step: `closing` is true exactly once per
/// stream, and the step answers `None`.
fn stream_step_sig(elem: &Type) -> Type {
    Type::Fn(
        vec![Type::Int, Type::Int, Type::Bool],
        Box::new(Type::Option(Box::new(elem.clone()))),
    )
}

/// A `Stream<T>`'s header (RFC-0075 M2b), spelled once — see `llt_of` for what
/// each of the six words means under each of the two producer tags.
const STREAM_LL: &str = "{ ptr, i64, i64, i64, i64, i64 }";

/// The reserved name a drop site parks a binding under so a declared `release`
/// goes through the ordinary call path. Unlexable, so no program can name it.
/// The direct backend parks its receiver under the same spelling.
const REL_RECV: &str = "@rel";

/// The module's derived copy over the defunctionalized `fn` enum (Phase 10b).
const FNVAL_COPY: &str = "__vyrn_fnval_copy";
const FNVAL_RELEASE: &str = "__vyrn_fnval_release";

pub const GEN_MODE_READ: i32 = 0;
pub const GEN_MODE_READ_BYTES: i32 = 1;
pub const GEN_MODE_LIST: i32 = 2;
// 3 is the genwasm host's `moduleInterface`, which never reaches this emitter.
/// `listDirKinds` (RFC-0119): the same `\n`-joined listing as `GEN_MODE_LIST`,
/// with a `/` appended to each directory entry's name. An entry name cannot
/// contain either byte, so the encoding stays invertible.
pub const GEN_MODE_LIST_KINDS: i32 = 4;

/// `@__vyrn_gen_reflect`'s kinds — which builtin the host is answering
/// (RFC-0076 M3b). The argument is the module path, the contract NAME, or the
/// source to lex.
pub const REFLECT_MODULE_INTERFACE: i64 = 0;
pub const REFLECT_CONTRACT_OF: i64 = 1;
pub const REFLECT_LEX: i64 = 2;

/// The generator-host entry points the ENGINE synthesizes and this emitter calls
/// (RFC-0076 M3b).
///
/// Each is an ordinary Vyrn function the engine appends to the wrapper program:
/// it asks the host to compute the value, then decodes it by walking the static
/// type. Codegen only redirects the builtin's call site to it, so the decode is
/// compiled by the ordinary emitter rather than hand-written as IR — the arrays,
/// the records and the Options are the ones every other Vyrn program gets.
pub const GEN_ENTRY_MODULE_INTERFACE: &str = "__vyrnGenModuleInterface";
pub const GEN_ENTRY_LEX: &str = "__vyrnGenLex";
/// Suffixed with the contract's name: the argument is a declaration, not a value.
pub const GEN_ENTRY_CONTRACT_OF: &str = "__vyrnGenContractOf_";
/// The text-IR backend's refusal of `listDir`.
///
/// The checker cannot gate the call the way it gates
/// `moduleInterface`/`contractOf`/`lex` — `listDir` has a runtime under `vyrn
/// run` (`list_dir_is_not_generation_only`) — so the one backend without a
/// lowering refuses it itself, in a user's sentence rather than an emitter's
/// note about its own gaps (RFC-0096 M3's addendum). The direct wasm backend
/// lowers it over `fd_readdir` (RFC-0125 §3 M5).
pub const LIST_DIR_NO_LOWERING: &str =
    "`listDir` runs in the interpreter, at generation time and on the wasm target (RFC-0021, \
     RFC-0125); it has no native lowering in v1 — use it in a `gen fn`, under `vyrn run` or with \
     `--target wasm`";

/// `listDirKinds`' copy of the sentence (RFC-0119) — same reasoning, its own
/// name, so the diagnostic names the call the user wrote.
pub const LIST_DIR_KINDS_NO_LOWERING: &str =
    "`listDirKinds` runs in the interpreter, at generation time and on the wasm target \
     (RFC-0119, RFC-0125); it has no native lowering in v1 — use it in a `gen fn`, under `vyrn \
     run` or with `--target wasm`";

/// The atom-stream primitives the synthesized decoders are written against.
pub const GEN_REFLECT: &str = "__vyrnGenReflect";
pub const GEN_NEXT_INT: &str = "__vyrnGenNextInt";
pub const GEN_NEXT_STR: &str = "__vyrnGenNextStr";

/// `@__vyrn_code_splice`'s value tags — which interpreter `Val` the host is to
/// rebuild from the word it was handed. Exactly the set the splice rule accepts
/// (`interp::gen_code_splice`), no more: the checker has already rejected
/// anything else by the time codegen sees the call. `pub` so the host reads the
/// same numbering it is emitted against, rather than a second copy of it.
pub const TAG_STR: i32 = 0;
pub const TAG_CODE: i32 = 1;
pub const TAG_BOOL: i32 = 2;
pub const TAG_INT: i32 = 3;
pub const TAG_UINT: i32 = 4;
pub const TAG_F64: i32 = 5;
pub const TAG_F32: i32 = 6;

/// Emit a complete LLVM IR module for `program`.
///
/// Native only, since RFC-0077 M5 deleted the wasm path — and since RFC-0076 M7
/// there is no `emit_gen_host` beside it either: the generation engine reaches wasm
/// through the direct backend, which needs no C toolchain, so the generator-host
/// variant of THIS emitter had no caller left. The `Code` handle imports, the
/// reflection redirects and `listDir`'s lowering went with it. A code quote outside
/// generation is still the checker's error and this emitter still has no lowering
/// for one, which is what it was before RFC-0076 M3a.
// The two instantiation bounds moved to `vyrn_frontend::types` in RFC-0101 M1,
// and are re-exported here so every existing reader spells them the same way.
// They moved because a bound on monomorphization is not a property of a backend:
// `vyrn-lower` runs the same worklist and sits BELOW this crate, so a copy here
// would be a second number, which is the shape of defect this RFC is about.
pub use vyrn_frontend::types::{MONO_DEPTH_LIMIT, MONO_SIZE_LIMIT};

/// The phrase every instantiation-limit refusal contains. `vyrn check` promotes
/// exactly this one codegen error to a check failure, so it has to recognise it,
/// and one needle both sides read cannot drift.
pub const MONO_LIMIT_NEEDLE: &str = "past the instantiation limit";

/// The phrase every frame-size refusal contains, for the same reason
/// [`MONO_LIMIT_NEEDLE`] exists: a test that pins a limit by quoting its
/// sentence pins the sentence, not the limit.
pub const FRAME_LIMIT_NEEDLE: &str = "past the frame limit";

/// The phrase every statics-size refusal contains, for the reason the two above
/// exist. This one was an `assert!` in [`wasm::Module::finish`] — a limit stated
/// as a Rust panic, in a function whose caller already returns `Result`, so a
/// program with more literals than the module can hold killed
/// `vyrn build --target wasm` with a backtrace and no source at all.
pub const STATICS_LIMIT_NEEDLE: &str = "past the statics limit";

/// Refuse an instantiation whose type arguments pass [`MONO_DEPTH_LIMIT`] or
/// [`MONO_SIZE_LIMIT`].
///
/// The message names the TYPE rather than the chain of calls that built it. The
/// type IS the chain, written down — `P<P<P<..>>>` is one `P` per instantiation —
/// and it is also the thing the author has to change.
pub fn check_inst_depth<'a>(
    name: &str,
    args: impl Iterator<Item = &'a Type>,
    line: usize,
    types: &HashMap<String, vyrn_frontend::ast::TypeDecl>,
) -> Result<(), String> {
    for a in args {
        let d = vyrn_frontend::types::type_depth(a);
        let too_deep = d > MONO_DEPTH_LIMIT;
        let size = vyrn_frontend::types::expanded_size(a, types, MONO_SIZE_LIMIT);
        if !too_deep && size.is_some() {
            continue;
        }
        let what = if too_deep {
            format!("nests {d} levels deep, past the limit of {MONO_DEPTH_LIMIT}")
        } else {
            format!("has more than {MONO_SIZE_LIMIT} parts once its records are written out")
        };
        let mut shown = a.to_string();
        if shown.len() > 80 {
            // The display string may hold multi-byte characters (the lexer
            // accepts Unicode identifiers): back up to a char boundary before
            // cutting, or `truncate` panics mid-character.
            let mut cut = 80;
            while !shown.is_char_boundary(cut) {
                cut -= 1;
            }
            shown.truncate(cut);
            shown.push_str("...");
        }
        return Err(format!(
            "instantiating `{name}` needs a type {MONO_LIMIT_NEEDLE}: it {what}\n  \
             note: `{name}` is declared on line {line}, and the type is `{shown}`\n  \
             note: a generic function that calls itself with a BIGGER type has no \
             finite set of instances — the recursion has to shrink the type, not \
             only the count"
        ));
    }
    Ok(())
}

/// Run the monomorphization the backends run, and report ONLY its depth refusal.
///
/// `vyrn check` reads this. Every other codegen error stays where it is —
/// `check` has never claimed to predict them, and promoting them all here would
/// change its contract by more than the one defect this closes (audit A5.2:
/// `check` said `ok` about a program no backend could finish).
///
/// **RFC-0101 M2b: this used to call [`emit`].** It ran the entire native
/// lowering, built a complete LLVM module as a `String`, matched its error
/// against one needle and threw the module away — because monomorphization only
/// existed inside a backend and the front end had no other way to ask how deep
/// it goes (§1.2). It does now: `vyrn-lower` runs the worklist, from the same
/// two constants, and the refusal is worded by the same [`check_inst_depth`]
/// both backends call. `vyrn-cli/tests/lowered.rs` is what makes that
/// prediction sound — it asserts over the corpus that every instantiation either
/// backend emits is one the lowering's worklist has.
pub fn check_instantiations(program: &Program) -> Result<(), String> {
    let types = vyrn_frontend::types::decl_map(program);
    for u in vyrn_lower::lower(program).unresolved {
        if u.why == vyrn_lower::Why::PastTheLimit {
            check_inst_depth(&u.callee, u.args.iter(), u.line, &types)?;
        }
    }
    Ok(())
}

pub fn emit(program: &Program) -> Result<String, String> {
    set_gen_host(false);
    let mut out = String::new();
    // module preamble: printf/abort + format strings (opaque-pointer style)
    out.push_str("; Vyrn v0.1 — generated LLVM IR (target: LLVM 15+)\n");
    out.push_str("declare i32 @printf(ptr, ...)\n");
    // exit() (not abort()) so stdio buffers flush and the exit code is a clean 1,
    // matching the interpreter.
    out.push_str("declare void @exit(i32)\n");
    out.push_str("declare i32 @strcmp(ptr, ptr)\n");
    // (`declare i32 @__vyrn_strncmp` and `declare ptr @strstr` went with the string
    // predicates — RFC-0078 M4c. Neither had another caller, so the shim lost an
    // exported function and the boundary lost two declarations.)
    // Heap + string runtime (dynamic strings). Allocations are not yet freed —
    // the reclamation strategy is RFC-0004's open question.
    out.push_str("declare i64 @__vyrn_strlen(ptr)\n");
    // (`declare i64 @__vyrn_charcount` went with `charCount` — RFC-0078's census
    // called it the one builtin with no justification, and `std/text`'s
    // `charCountV` is the same byte scan in Vyrn.)
    out.push_str("declare i64 @__vyrn_line_at(ptr, i64, i64)\n");
    out.push_str("declare i64 @__vyrn_col_at(ptr, i64, i64)\n");
    // RFC-0104's loop-facts item, first slice: the allocator family's results
    // are `noalias` — a fresh block aliases nothing, `realloc`'s result is the
    // C guarantee — so the optimizer's alias analysis finally hears what the
    // ownership model always knew. Measured on landing (bt/fk/sp kernels).
    out.push_str("declare noalias ptr @__vyrn_malloc(i64)\n");
    out.push_str("declare noalias ptr @__vyrn_realloc(ptr, i64)\n");
    out.push_str("declare void @__vyrn_free(ptr)\n");
    out.push_str("declare ptr @strcpy(ptr, ptr)\n");
    out.push_str("declare ptr @strcat(ptr, ptr)\n");
    // `llvm.memcpy` — used by `SmallArray` (RFC-0056) to move its inline slots
    // to the heap on spill and to copy elements out in `toArray`. The
    // target-independent intrinsic (always an i64 length) lowers correctly on
    // both native and wasm — unlike libc `memcpy`, whose `size_t` is i32 on
    // wasm32 and i64 on x86-64 (an ABI clash). SmallArray never crosses
    // `extern`, so keeping the copy internal to generated code is sound.
    out.push_str("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)\n");
    // `llvm.memset` — used by the user-keyed map (RFC-0117 M2) to zero a key
    // buffer before the canonical field-wise pack, so padding is never
    // anything but zero and `memcmp` is field-wise equality.
    out.push_str("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)\n");
    // Saturating float→int (RFC-0078 M4a). A bare `fptosi`/`fptoui` is POISON
    // for an out-of-range or NaN operand, which is not a semantics any engine
    // can agree with: the interpreter is Rust's `as` (saturating, NaN→0) and
    // RFC-0077 M2h made the direct wasm backend match it, so native was the odd
    // one out and `examples/numbytes.vyrn` caught it — `Int64(10^300)` was
    // `Int64.min` natively and `Int64.max` in the other two. The saturating
    // intrinsics ARE Rust's `as`, so this is a one-for-one substitution.
    out.push_str("declare i64 @llvm.fptosi.sat.i64.f64(double)\n");
    out.push_str("declare i64 @llvm.fptoui.sat.i64.f64(double)\n");
    out.push_str("declare i64 @llvm.fptosi.sat.i64.f32(float)\n");
    out.push_str("declare i64 @llvm.fptoui.sat.i64.f32(float)\n");
    // Vector `min`/`max`/`abs`/`sqrt` (RFC-0083 M2). `llvm.minimum` and NOT
    // `llvm.minnum`: `minnum` is IEEE-754 `minNum`, which returns the non-NaN
    // operand, while `minimum` propagates NaN and orders `-0.0` below `+0.0` —
    // which is what wasm's `f32x4.min` does. The two answer differently for
    // `min(NaN, 1.0)` and the six-decimal formatter SHOWS that difference, unlike
    // a NaN payload difference. Choosing the intrinsic is how native was made to
    // agree rather than left to whatever the host's `minps` does.
    out.push_str("declare <4 x float> @llvm.minimum.v4f32(<4 x float>, <4 x float>)\n");
    out.push_str("declare <4 x float> @llvm.maximum.v4f32(<4 x float>, <4 x float>)\n");
    out.push_str("declare <4 x float> @llvm.sqrt.v4f32(<4 x float>)\n");
    // Rounding (RFC-0083 M2). `nearest` is roundTiesToEven — wasm's
    // `f32x4.nearest` — and emphatically NOT `llvm.round`, which is
    // roundTiesAwayFromZero. The two are different functions and they differ on
    // exactly the halves: measured before the intrinsic was picked,
    // `llvm.round.v4f32` on `<0.5, 1.5, 2.5, -2.5>` answers `1 2 3 -3` where
    // wasmtime's `f32x4.nearest` answers `0 2 2 -2`. That is the `minnum` bug one
    // operation over, and `f32::round` would have walked the interpreter into it
    // too.
    //
    // The intrinsic that NAMES roundTiesToEven is `llvm.roundeven`, and it is not
    // the one emitted, for a linking reason rather than a semantic one: baseline
    // x86-64 has no `roundps` (that is SSE4.1) and this project's `clang -O2`
    // passes no `-march`, so `llvm.roundeven.v4f32` scalarizes to four calls to
    // `roundevenf` — a C23 symbol the MSVC UCRT does not ship, and the link fails
    // outright. `llvm.rint` lowers to `rintf`, which every libc here has, and
    // under the default rounding mode it IS roundTiesToEven. Vyrn has no `fenv`
    // surface to change that mode with; a host that changed it behind an `extern`
    // would already have moved every `fadd` in the program, so this adds no hole
    // that was not there. The exact halves are pinned in `examples/simdround.vyrn`
    // so a future `-march=x86-64-v2` can switch this line back to `roundeven` and
    // find out immediately if it was wrong.
    out.push_str("declare <4 x float> @llvm.ceil.v4f32(<4 x float>)\n");
    out.push_str("declare <4 x float> @llvm.floor.v4f32(<4 x float>)\n");
    out.push_str("declare <4 x float> @llvm.trunc.v4f32(<4 x float>)\n");
    out.push_str("declare <4 x float> @llvm.rint.v4f32(<4 x float>)\n");
    // There is deliberately no integer equivalent of these (RFC-0083 M3).
    // `llvm.smin`/`smax`/`abs.v4i32` were declared here, `i32x4.min_s`/`max_s`/
    // `abs` were emitted on the other side, and all three were deleted: the
    // reason the float ones earn their place is the NaN rule and the signed zero,
    // and an integer `min` has neither, so LLVM compiles the Vyrn `if a < b`
    // into the same `pminsd` and the intrinsic buys 1.0x.
    //
    // The mask reductions. `<4 x i1>` is fine as an intrinsic ARGUMENT — the ABI
    // objection that kept it out of the mask's own representation is about values
    // crossing function boundaries, and these never leave the block.
    out.push_str("declare i1 @llvm.vector.reduce.or.v4i1(<4 x i1>)\n");
    out.push_str("declare i1 @llvm.vector.reduce.and.v4i1(<4 x i1>)\n");
    // RFC-0083 M4's wide width. `minimum`/`maximum` again and for the same
    // reason — `llvm.minnum.v2f64` would return the non-NaN operand where wasm's
    // `f64x2.min` propagates, and the formatter shows that difference at 64 bits
    // exactly as it does at 32. The four roundings have no `v2f64` declaration
    // here because M4 did not ship them; see the RFC's note.
    out.push_str("declare <2 x double> @llvm.minimum.v2f64(<2 x double>, <2 x double>)\n");
    out.push_str("declare <2 x double> @llvm.maximum.v2f64(<2 x double>, <2 x double>)\n");
    out.push_str("declare <2 x double> @llvm.sqrt.v2f64(<2 x double>)\n");
    out.push_str("declare i1 @llvm.vector.reduce.or.v2i1(<2 x i1>)\n");
    out.push_str("declare i1 @llvm.vector.reduce.and.v2i1(<2 x i1>)\n");
    // Worker threads (RFC-0025): `spawn f(args)` packs its evaluated arguments
    // into a heap frame and hands the shim a per-spawn-site thunk SYMBOL plus
    // that frame; the shim runs the thunk on a real OS thread natively (Win32 /
    // pthreads), inline on wasm (no threads) and under VYRN_SEQUENTIAL_SPAWN=1
    // — one shared IR, byte-identical output on every schedule because tasks
    // are checker-proven isolated. `join` blocks and returns the frame; the
    // result sits in its leading slot. The thunk symbol is a C-boundary detail,
    // not a Vyrn-level function value: every `call` still names a symbol.
    out.push_str("declare ptr @__vyrn_spawn(ptr, ptr)\n");
    out.push_str("declare ptr @__vyrn_join(ptr)\n");
    out.push_str("declare void @__vyrn_task_release(ptr)\n");
    out.push_str("declare i32 @__vyrn_snprintf(ptr, i64, ptr, ...)\n");
    // Logging (RFC-0008) and traps: fprintf/fputs to stderr. `stderr` is a C
    // macro with no portable symbol, so the stream handles come from a tiny C
    // shim (`__vyrn_stderr`/`__vyrn_stdout`, embedded in vyrn-cli and compiled
    // by clang alongside this IR) that works on every libc (MSVC, glibc,
    // wasi-libc).
    out.push_str("declare i32 @fprintf(ptr, ptr, ...)\n");
    out.push_str("declare ptr @__vyrn_stderr()\n");
    out.push_str("declare ptr @__vyrn_stdout()\n");
    // Runtime traps (division, and eventually every trap) fputs to stderr with
    // the interpreter's exact `error: ...` wording, then exit(1).
    out.push_str("declare i32 @fputs(ptr, ptr)\n");
    out.push_str("declare ptr @fopen(ptr, ptr)\n");
    out.push_str("declare i32 @fclose(ptr)\n");
    // Input I/O (RFC-0014): the C shim in vyrn-cli provides these; the error
    // wording is built here in the IR (see the `@.io.*` format globals and the
    // `@__vyrn_read_err`/`@__vyrn_write_err`/`@__vyrn_args` helpers below) so the
    // canonical strings live in exactly one place.
    out.push_str("declare i64 @__vyrn_args_count()\n");
    out.push_str("declare ptr @__vyrn_args_get(i64)\n");
    out.push_str("declare ptr @__vyrn_read_line(ptr)\n");
    out.push_str("declare i32 @__vyrn_read_file(ptr, ptr, ptr)\n");
    out.push_str("declare i32 @__vyrn_read_file_bytes(ptr, ptr, ptr)\n");
    out.push_str("declare i32 @__vyrn_write_file(ptr, ptr)\n");
    // RFC-0111: the byte sink. `write_file_bytes` takes an explicit length
    // because the buffer may hold NULs, so strlen would stop short of it;
    // `write_stdout` answers nothing, for the reason `print` answers nothing.
    out.push_str("declare i32 @__vyrn_write_file_bytes(ptr, ptr, i64)\n");
    out.push_str("declare void @__vyrn_write_stdout(ptr, i64)\n");
    // RFC-0044: atomic rename + fsync host primitives (implemented in the C shim
    // on every target, like the RFC-0043 clock — wasi lowers to path_rename /
    // fd_sync, so a storage program is a three-way parity citizen).
    out.push_str("declare i32 @__vyrn_rename_file(ptr, ptr)\n");
    out.push_str("declare i32 @__vyrn_fsync_file(ptr)\n");
    // The JSON codec runtime is GONE from this boundary (RFC-0078 M2b then M3):
    // `toJson` renders through `std/json`'s `emit` and `fromJson` reads through
    // `std/jsonread` with a generated per-type walk, both of them Vyrn. So nothing
    // this file emits builds, reads or serializes a DOM, nothing accumulates an
    // Issue through a C list, and the twenty-two declares that used to sit here
    // went with the C they named.
    // Map<String, V> runtime (RFC-0028).
    out.push_str(
        "declare i64 @__vyrn_map_find(ptr, i64, ptr, ptr, i64)
",
    );
    out.push_str(
        "declare i64 @__vyrn_map_find_bytes(ptr, i64, ptr, i64, ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_reserve(ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_index_add(ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_remove_at(ptr, i64, i64)
",
    );
    out.push_str(
        "declare ptr @__vyrn_map_keys_copy(ptr, i64)
",
    );
    // The Int64-keyed family (RFC-0117 M1): same shapes, the key by value.
    out.push_str(
        "declare i64 @__vyrn_map_find_i64(ptr, i64, i64, ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_reserve_i64(ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_index_add_i64(ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_remove_at_i64(ptr, i64, i64)
",
    );
    out.push_str(
        "declare ptr @__vyrn_map_keys_copy_i64(ptr, i64)
",
    );
    // The user-keyed family (RFC-0117 M2): packed fixed-stride keys.
    out.push_str(
        "declare i64 @__vyrn_map_find_pack(ptr, i64, ptr, i64, ptr, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_reserve_pack(ptr, i64, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_index_add_pack(ptr, i64, i64)
",
    );
    out.push_str(
        "declare void @__vyrn_map_remove_at_pack(ptr, i64, i64, i64)
",
    );
    out.push_str(
        "declare ptr @__vyrn_map_keys_copy_pack(ptr, i64, i64)
",
    );
    // RFC-0114 §25: the leak-check predicate and the exit assertion.
    out.push_str(
        "declare i32 @__vyrn_leak_check_on()
",
    );
    out.push_str(
        "declare void @__vyrn_audit_exit()
",
    );
    out.push_str(
        "declare void @__vyrn_teardown_begin()
",
    );
    // `extern` imports (RFC-0012): each body-less `extern fn` becomes a wasm
    // import from the fixed `vyrn` namespace. We emit ONE target-neutral IR —
    // a `declare` carrying the wasm-import attributes plus a real `call` at each
    // use site (see `gen_extern_call`). On the wasm target the import resolves
    // against the host page's `vyrn` object; on native the symbol is satisfied
    // by a per-extern C trap stub that vyrn-cli links in (printing the canonical
    // "not available on this target" message and exiting), so a single binary
    // stays honest instead of silently stubbing. Attribute groups are collected
    // here and appended at module end.
    let mut extern_attr_groups = String::new();
    for (i, f) in program
        .functions
        .iter()
        .filter(|f| f.is_extern && host_boundary_extern(&f.name).is_none())
        .enumerate()
    {
        let ret = extern_abi_ll(&f.ret);
        let params = extern_decl_params(f);
        let grp = 100 + i; // arbitrary, distinct ids; no other groups in this IR
        out.push_str(&format!(
            "declare {ret} @{}({params}) #{grp}\n",
            extern_symbol(&f.name)
        ));
        extern_attr_groups.push_str(&format!(
            "attributes #{grp} = {{ \"wasm-import-module\"=\"vyrn\" \"wasm-import-name\"=\"{}\" }}\n",
            f.name
        ));
    }
    // RFC-0043 host-boundary externs resolve to plain C-shim symbols (no `vyrn`
    // import), so they link the same on native and wasm.
    out.push_str("declare i64 @__vyrn_now_millis()\n");
    out.push_str("declare i64 @__vyrn_monotonic_nanos()\n");
    out.push_str("declare i64 @__vyrn_random_seed()\n");
    // For a `file(..)` sink: a global stream handle plus the path/mode constants.
    if let LogSink::File(path) = &program.log_sink {
        out.push_str("@__vyrn_log_file = global ptr null\n");
        let (escaped, len) = llvm_str(path);
        out.push_str(&format!(
            "@.logpath = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n"
        ));
        out.push_str("@.logmode = private unnamed_addr constant [2 x i8] c\"w\\00\"\n");
    }
    // Index traps carry the offending index (fprintf'd to stderr), matching
    // the interpreter's `error: array index {i} out of bounds` byte-for-byte.
    out.push_str(&trap_global(
        "@.trap.aoob",
        &vyrn_frontend::trap::line(&vyrn_frontend::trap::around(
            vyrn_frontend::trap::ARRAY_INDEX,
            "%lld",
        )),
    ));
    // RFC-0116: `tallyBytes` over bytes that are not a String. One wording for
    // both of `stringFromBytes`'s reasons — the caller who wants the WHY has
    // that function.
    out.push_str(&trap_global(
        "@.trap.tbytes",
        &vyrn_frontend::trap::line(vyrn_frontend::trap::io("tbytes")),
    ));
    out.push_str(&trap_global(
        "@.trap.soob",
        &vyrn_frontend::trap::line(&vyrn_frontend::trap::around(
            vyrn_frontend::trap::STRING_INDEX,
            "%lld",
        )),
    ));
    // (`@.trap.sliceoob` and `@.trap.slicesplit` were here. RFC-0079 M3 made
    // `slice` return its failure instead of ending the process, so the catalogue
    // SHRANK by two rows rather than growing — which is the trade RFC-0078's
    // `@abort(kind)` design would have made in the other direction. A caller that
    // still wants to die writes `?? panic("…")` and owns the wording.)
    // `panic(msg)` (RFC-0079): the caller owns the text, the compiler owns the
    // frame. It is a format rather than three `fputs` because the catalogue
    // above already prints through `fprintf` for the traps that interpolate,
    // and `%s` is safe here — a Vyrn `String` cannot contain a NUL (RFC-0014).
    out.push_str("@.panic.fmt = private unnamed_addr constant [11 x i8] c\"error: %s\\0A\\00\"\n");
    // Census U5: the same frame with the site the loader stamped. A `panic`
    // reports where it is WRITTEN — `std/slots.vyrn:189` — and the site travels
    // as a pooled string literal, so a site costs one operand here and one
    // string in `.rodata`, shared by every `panic` that spells the same place.
    out.push_str(
        "@.panic.at = private unnamed_addr constant [16 x i8] c\"error: %s (%s)\\0A\\00\"\n\n",
    );
    // RFC-0074 M3a. `serveStream` hands a producer to the HOST's accept loop, and
    // a compiled binary has no accept loop to hand it to — `vyrn serve` is the
    // interpreter. A build is still legal, because `std/http`'s `mount` reaches
    // this arm whether or not the program mounts a live route, and refusing at
    // compile time would make every REST projection unbuildable to serve a
    // feature it does not use. So it is a runtime trap on the path nothing takes.
    out.push_str(&{ trap_global("@.trap.serve", &serve_stream_trap()) + "\n" });

    // ---- the trap tail, once (RFC-0090 phase 8d) ------------------------
    // Every trap and every `panic` used to emit three calls INLINE at its site:
    // `@__vyrn_stderr`, an `fputs` or a variadic `fprintf`, and `exit`. The
    // block is cold and no program takes it, but LLVM's inliner reads cost
    // before it reads probability — three calls is roughly what a small
    // function is allowed to cost in total. So a guard the program never takes
    // made the function AROUND it too expensive to inline, and `std/slots`'
    // `place at` paid for that at every access.
    //
    // These three functions are that block, once. `noreturn cold` states both
    // halves of the fact: the call does not come back, and it is not the hot
    // path. A trap site is now one call, so the guard costs about what its
    // compare costs. `internal` lets a program that traps in none of the three
    // ways drop the ones it does not use.
    //
    // Nothing about WHAT is printed changed — same stream, same bytes, same
    // exit code — which is why parity is byte-identical across the change.
    out.push_str(
        "define internal void @__vyrn_trap_msg(ptr %m) noreturn cold {\n\
         entry:\n\
         \x20 %e = call ptr @__vyrn_stderr()\n\
         \x20 call i32 @fputs(ptr %m, ptr %e)\n\
         \x20 call void @exit(i32 1)\n\
         \x20 unreachable\n\
         }\n\n\
         define internal void @__vyrn_trap_idx(ptr %f, i64 %i) noreturn cold {\n\
         entry:\n\
         \x20 %e = call ptr @__vyrn_stderr()\n\
         \x20 call i32 (ptr, ptr, ...) @fprintf(ptr %e, ptr %f, i64 %i)\n\
         \x20 call void @exit(i32 1)\n\
         \x20 unreachable\n\
         }\n\n\
         define internal void @__vyrn_panic(ptr %m, ptr %at) noreturn cold {\n\
         entry:\n\
         \x20 %e = call ptr @__vyrn_stderr()\n\
         \x20 %bare = icmp eq ptr %at, null\n\
         \x20 br i1 %bare, label %nosite, label %sited\n\
         sited:\n\
         \x20 call i32 (ptr, ptr, ...) @fprintf(ptr %e, ptr @.panic.at, ptr %m, ptr %at)\n\
         \x20 br label %out\n\
         nosite:\n\
         \x20 call i32 (ptr, ptr, ...) @fprintf(ptr %e, ptr @.panic.fmt, ptr %m)\n\
         \x20 br label %out\n\
         out:\n\
         \x20 call void @exit(i32 1)\n\
         \x20 unreachable\n\
         }\n\n",
    );

    // ---- region / arena runtime (RFC-0004 §4) ---------------------------
    // A `region { .. }` block gives heap allocations a deterministic lifetime:
    // everything allocated while the region is on the stack is freed when the
    // block exits. Implementation: a stack (max depth 64; entering a 65th
    // nested region traps, and the interpreter enforces the same bound) of
    // singly-linked allocation lists. Each region allocation reserves 8 extra
    // header bytes holding the "next" link; `exit` walks the list and frees it.
    // `concat` routes through the arena at runtime when a region is active.
    out.push_str(&region_runtime());
    out.push_str(&call_depth_runtime());

    out.push_str(
        &STREAM_RUNTIME.replace(
            "$NOBOX",
            trap_global(
                "@.fmt.nob",
                &vyrn_frontend::trap::line(vyrn_frontend::trap::NO_STREAM),
            )
            .trim_end(),
        ),
    );
    out.push_str(STRING_RUNTIME);
    // The UTF-8 validator DFA table, then the validator. (The base64 alphabet
    // table went with the codecs -- RFC-0078 M4c; `std/codecs` builds it from a
    // string literal instead.)
    let utf8d = utf8d_table();
    let table_body = utf8d
        .iter()
        .map(|b| format!("i8 {b}"))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "@__vyrn_utf8d = private unnamed_addr constant [364 x i8] [{table_body}]\n"
    ));
    out.push_str(ENCODING_RUNTIME);
    out.push_str(REGEX_RUNTIME);
    // `%lld\n` for i64 — `%ld` would be 32-bit under the Windows/MSVC ABI where
    // `long` is 32 bits, truncating full 64-bit values; `long long` is 64-bit.
    out.push_str("@.fmt.d = private unnamed_addr constant [6 x i8] c\"%lld\\0A\\00\"\n");
    // `%llu\n` for printing unsigned sized ints (UInt8..64) — zero-extended to u64.
    out.push_str("@.fmt.u = private unnamed_addr constant [6 x i8] c\"%llu\\0A\\00\"\n");
    // (`@.fmt.f` and `@.fmt.lf` — `"%f\n"` and `"%f"` — went with RFC-0081 M2:
    // a float prints and interpolates through `std/num`'s `f64Str` now, so this
    // module names no float format at all.)
    // No-newline variants used by `str(..)` (interpolation renders without \n):
    // %lld for signed ints, %llu for unsigned.
    out.push_str("@.fmt.ld = private unnamed_addr constant [5 x i8] c\"%lld\\00\"\n");
    out.push_str("@.fmt.lu = private unnamed_addr constant [5 x i8] c\"%llu\\00\"\n");
    out.push_str("@.fmt.s = private unnamed_addr constant [4 x i8] c\"%s\\0A\\00\"\n");
    out.push_str("@.fmt.true = private unnamed_addr constant [6 x i8] c\"true\\0A\\00\"\n");
    out.push_str("@.fmt.false = private unnamed_addr constant [7 x i8] c\"false\\0A\\00\"\n");
    // No-newline variants for `str(Bool)` (interpolation renders without \n).
    out.push_str("@.str.true = private unnamed_addr constant [5 x i8] c\"true\\00\"\n");
    out.push_str("@.str.false = private unnamed_addr constant [6 x i8] c\"false\\00\"\n");
    // Logging (RFC-0008): "[LEVEL] name: msg\n" and the level-name strings.
    out.push_str("@.fmt.log = private unnamed_addr constant [13 x i8] c\"[%s] %s: %s\\0A\\00\"\n");
    out.push_str("@.lvl.trace = private unnamed_addr constant [6 x i8] c\"TRACE\\00\"\n");
    out.push_str("@.lvl.debug = private unnamed_addr constant [6 x i8] c\"DEBUG\\00\"\n");
    out.push_str("@.lvl.info = private unnamed_addr constant [5 x i8] c\"INFO\\00\"\n");
    out.push_str("@.lvl.warn = private unnamed_addr constant [5 x i8] c\"WARN\\00\"\n");
    out.push_str("@.lvl.error = private unnamed_addr constant [6 x i8] c\"ERROR\\00\"\n");
    // Validation trap messages, one per predicated type — byte-identical to
    // the interpreter's errors as the CLI renders them (`error: {msg}` on
    // stderr, exit 1). A record base gets the cross-field wording.
    for t in &program.type_decls {
        if t.predicate.is_none() {
            continue;
        }
        out.push_str(&trap_global(
            &format!("@.trap.verr.{}", t.name),
            &validation_message(t),
        ));
    }
    // Division trap messages — byte-identical to the interpreter's errors as
    // rendered by the CLI (`error: {msg}` on stderr, exit 1).
    out.push_str(&trap_global(
        "@.trap.div0",
        &vyrn_frontend::trap::line(vyrn_frontend::trap::DIV_ZERO),
    ));
    out.push_str(&trap_global(
        "@.trap.rem0",
        &vyrn_frontend::trap::line(vyrn_frontend::trap::REM_ZERO),
    ));
    out.push_str(&trap_global(
        "@.trap.divovf",
        &vyrn_frontend::trap::line(vyrn_frontend::trap::DIV_OVERFLOW),
    ));
    // Shift-amount-out-of-range trap (RFC-0045): a shift by `>= bitwidth` (or a
    // negative amount) traps with this canonical wording, byte-identical to the
    // interpreter's `shift amount out of range` as the CLI renders it.
    out.push_str(&trap_global(
        "@.trap.shift",
        &vyrn_frontend::trap::line(vyrn_frontend::trap::SHIFT_RANGE),
    ));
    // (`@.fmt.nan`\`@.str.nan` — the literal `NaN` this build selected on an
    // `fcmp uno` because UCRT's `%f` says `-nan(ind)` — went with RFC-0081 M2.
    // `f64Str` spells the three non-finite words itself, in Vyrn, once.)

    // Input-I/O error wording (RFC-0014), from the one list both backends read.
    // These are payload strings (no trailing newline — unlike the trap globals).
    for (name, msg) in IO_MESSAGES {
        let (escaped, len) = llvm_str(msg);
        out.push_str(&format!(
            "@.io.{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n"
        ));
    }
    out.push_str(IO_RUNTIME);

    // Emit one global per distinct string literal; map content -> global name.
    // Built before string collection so `jsonSchema`/`schemaOf` can seed their
    // compile-time-computed strings into the pool.
    let type_map: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    let mut str_globals: HashMap<String, String> = HashMap::new();
    let mut literals: Vec<String> = Vec::new();
    for f in &program.functions {
        collect_strings_block(&f.body, &mut literals, &type_map);
    }
    // A `place` projection is never flattened into `program.functions` — it is
    // inlined at each access site instead (RFC-0091 M2) — so its own literals
    // reach the pool from here or not at all. `panic("..")` inside a projection
    // is what wants them: an `Index` that refuses a dead key says so.
    for imp in &program.impls {
        for f in &imp.places {
            collect_strings_block(&f.body, &mut literals, &type_map);
        }
    }
    // A string literal can also live in a type's refinement predicate
    // (`String where value == "root"`), which is lowered inline at every
    // construction site — collect those too (regex collection below does the
    // same walk for `=~` patterns).
    for t in &program.type_decls {
        if let Some(pred) = &t.predicate {
            collect_strings_expr(pred, &mut literals, &type_map);
        }
    }
    // Module-state initializers (RFC-0013) are lowered in `@__vyrn_globals_init`,
    // so any string literal they mention must be pooled too.
    for g in &program.globals {
        collect_strings_expr(&g.init, &mut literals, &type_map);
    }
    for (i, s) in literals.iter().enumerate() {
        let name = format!("@.str.{i}");
        out.push_str(&static_str_global(&name, s));
        str_globals.insert(s.clone(), static_str_ptr(&name, s));
    }
    // The per-enum variant-name table went with RFC-0078 M2b. It existed so
    // `toJson` on a nullary enum could read the name in O(1) from IR; the encoder
    // is synthesized Vyrn now, and its nullary arm is an ordinary `JStr("Guest")`
    // over the string pool above — so the table had no other reader.
    out.push('\n');

    // Compile every distinct `=~` pattern to a DFA and emit its transition table
    // and accepting-state array as globals (the runner `@__vyrn_regex_run` walks
    // them). The map lets `gen_binary` find a pattern's globals at the use site.
    let mut regex_patterns: Vec<String> = Vec::new();
    for f in &program.functions {
        collect_regex_block(&f.body, &mut regex_patterns);
    }
    for imp in &program.impls {
        for f in &imp.places {
            collect_regex_block(&f.body, &mut regex_patterns);
        }
    }
    // A `=~` can also live in a type's refinement predicate (`String where value
    // =~ "…"`), which is lowered at construction sites — collect those too.
    for t in &program.type_decls {
        if let Some(pred) = &t.predicate {
            collect_regex_expr(pred, &mut regex_patterns);
        }
    }
    for g in &program.globals {
        collect_regex_expr(&g.init, &mut regex_patterns);
    }
    let mut regex_globals: HashMap<String, (String, String, u32)> = HashMap::new();
    for (i, pat) in regex_patterns.iter().enumerate() {
        // The checker already proved every pattern compiles.
        let dfa = vyrn_frontend::regex::compile(pat).expect("regex validated by checker");
        let table_name = format!("@.rx.{i}.table");
        let accept_name = format!("@.rx.{i}.accept");
        let table_body = dfa
            .table
            .iter()
            .map(|n| format!("i32 {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{table_name} = private unnamed_addr constant [{} x i32] [{table_body}]\n",
            dfa.table.len()
        ));
        let accept_body = dfa
            .accepting
            .iter()
            .map(|a| format!("i8 {}", if *a { 1 } else { 0 }))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "{accept_name} = private unnamed_addr constant [{} x i8] [{accept_body}]\n",
            dfa.accepting.len()
        ));
        regex_globals.insert(pat.clone(), (table_name, accept_name, dfa.start));
    }
    if !regex_globals.is_empty() {
        out.push('\n');
    }

    // Signatures of every function, so call sites can type/coerce args and results.
    let ret_types: HashMap<String, Type> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.ret.clone()))
        .collect();
    let param_types: HashMap<String, Vec<Type>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.ty.clone()).collect(),
            )
        })
        .collect();
    let param_caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    // Validated-type + record declarations, for construction, Named→base
    // resolution, and record layout.
    let types: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    // Enum variant -> (tag index, enum name), for construction.
    let mut variants: HashMap<String, (i64, String)> = HashMap::new();
    for t in &program.type_decls {
        if let Type::Enum(vs) = &t.base {
            for (i, v) in vs.iter().enumerate() {
                variants.insert(v.name.clone(), (i as i64, t.name.clone()));
            }
        }
    }

    let funcs: HashMap<String, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let empty_subst: HashMap<String, Type> = HashMap::new();

    // Monomorphization worklist. Non-generic functions are emitted once; generic
    // functions are emitted once per distinct instantiation reachable from them.
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: Vec<(String, Vec<Type>)> = Vec::new();
    // Higher-order specialization worklist and lifted-lambda dedup set (RFC-0023).
    let mut ho_queue: Vec<HoInst> = Vec::new();
    let mut lambda_emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    // RFC-0037 defunctionalization state, threaded through every `Gen` so
    // variant tags are module-global and dispatchers see every source.
    let mut fnval_registry: Vec<FnValVariant> = Vec::new();
    let mut fnval_dispatch: Vec<Type> = Vec::new();
    let mut stream_closers: Vec<Type> = Vec::new();

    // What has ever been queued, so "is it already waiting?" is one lookup. The
    // scan it replaces re-mangled every queued entry per discovered
    // instantiation — O(insts²) mangles, and a mangle now hashes.
    let mut queued: std::collections::HashSet<String> = std::collections::HashSet::new();
    let enqueue = |emitted: &std::collections::HashSet<String>,
                   queued: &mut std::collections::HashSet<String>,
                   queue: &mut Vec<(String, Vec<Type>)>,
                   insts: Vec<(String, Vec<Type>)>| {
        for (n, args) in insts {
            let m = mangle_name(&n, &args);
            // Both sets: `emitted` also holds the higher-order instances, which
            // never pass through here, and `queued` is never drained — an entry
            // popped and emitted must not be queued a second time.
            if !emitted.contains(&m) && queued.insert(m) {
                queue.push((n, args));
            }
        }
    };

    // Whole-program ownership: which functions return owned heap values, and
    // which `let` bindings each function must free at block exit (RFC-0004 §4).
    let ownership = vyrn_frontend::own::analyze(program);
    // RFC-0093 M2: the places a `consume` took out of each droppable `let`,
    // flattened across functions. The key is the `let`'s node address, which is
    // unique in the program, so one map answers for every body — the same
    // flattening the interpreter does with `droppable`.
    let holes_map: HashMap<usize, Vec<String>> = ownership
        .holes
        .values()
        .flatten()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let holes_map = &holes_map;
    let owned_proto = &ownership.proto;
    // The per-node release decisions, one artifact (RFC-0114 §26 step 2).
    let plan = ownership.plan.clone();

    let protocol_methods: HashMap<String, String> = program
        .protocols
        .iter()
        .flat_map(|p| {
            // Projection requirements (RFC-0123 M2) dispatch by receiver type
            // through the places table, never as mangled methods.
            p.methods
                .iter()
                .filter(|m| m.result_cap.is_none())
                .map(|m| (m.name.clone(), p.name.clone()))
        })
        .collect();

    // ---- module state (RFC-0013) ----------------------------------------
    // One LLVM global per binding (`@g.<name>`, `zeroinitializer`), plus a
    // synthesized `@__vyrn_globals_init()` that runs every initializer's stores
    // in declaration order (heap-valued inits — arrays, strings — work because
    // this runs at runtime). It is called from `vyrn_entry` BEFORE `main`. Reads
    // and writes elsewhere resolve through `globals_map` via `Gen::lookup`.
    let mut globals_map: HashMap<String, (String, Type)> = HashMap::new();
    // Census P1: the module-state accumulators, cleared by the same whitelist a
    // local passes, read over every body because a global is reachable from all
    // of them. Slot symbol → the symbol of its ownership flag.
    let mut gappend: HashMap<String, String> = HashMap::new();
    let mut globals_init_ir = String::new();
    if !program.globals.is_empty() {
        let mut gi = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        gi.log_level = program.log_level;
        gi.log_sink = program.log_sink.clone();
        gi.protocol_methods = protocol_methods.clone();
        gi.fnval_variants = std::mem::take(&mut fnval_registry);
        gi.fnval_dispatch = std::mem::take(&mut fnval_dispatch);
        gi.stream_closers = std::mem::take(&mut stream_closers);
        let gaccs = global_append_candidates(program);
        let mut decls = String::new();
        for g in &program.globals {
            // RFC-0037: the declared type is a lambda initializer's signature.
            let pushed = g.ty.is_some();
            if let Some(t) = &g.ty {
                gi.expect.push(t.clone());
            }
            let r = gi.gen_expr(&g.init);
            if pushed {
                gi.expect.pop();
            }
            let (v, vty) = r?;
            let ty = match &g.ty {
                Some(t) => t.clone(),
                None => vty.clone(),
            };
            // Coerce into the declared/inferred type (record width subtyping,
            // sized-int wrapping, and automatic validation via `emit_validation`).
            let (v, _) = gi.coerce(v, &vty, &ty)?;
            let sym = format!("@g.{}", sanitize(&g.name));
            let ll = gi.llt(&ty);
            gi.emit(format!("store {ll} {v}, ptr {sym}"));
            // A later initializer may read this one — register it so its `Var`
            // resolves through `lookup`'s globals fallback.
            gi.globals.insert(g.name.clone(), (sym.clone(), ty.clone()));
            decls.push_str(&format!("{sym} = internal global {ll} zeroinitializer\n"));
            // The accumulator's ownership flag, and what the initializer made
            // true: a literal is data-segment storage nothing allocated, and
            // anything else this initializer built belongs to the global.
            if gaccs.contains(&g.name) && gi.resolve(&ty) == Type::Str {
                let flag = format!("{sym}.own");
                decls.push_str(&format!("{flag} = internal global i64 0\n"));
                gi.emit(format!(
                    "store i64 {}, ptr {flag}",
                    !matches!(g.init, Expr::Str(_)) as i64
                ));
                gappend.insert(sym.clone(), flag);
            }
            globals_map.insert(g.name.clone(), (sym, ty));
        }
        globals_init_ir.push_str("define internal void @__vyrn_globals_init() {\n");
        globals_init_ir.push_str("entry:\n");
        for a in &gi.allocas {
            globals_init_ir.push_str(a);
            globals_init_ir.push('\n');
        }
        for b in &gi.body {
            globals_init_ir.push_str(b);
            globals_init_ir.push('\n');
        }
        globals_init_ir.push_str("  ret void\n");
        globals_init_ir.push_str("}\n\n");
        out.push_str(&decls);
        out.push('\n');
        // An initializer may instantiate a generic or spawn a task (RFC-0025:
        // a spawn emits a per-callee thunk into `lambda_defs`) — drain both so
        // the referenced symbols get defined like any function body's.
        let insts = std::mem::take(&mut gi.instantiations);
        enqueue(&emitted, &mut queued, &mut queue, insts);
        drain_ho(&mut gi, &mut out, &mut ho_queue, &mut lambda_emitted);
        fnval_registry = std::mem::take(&mut gi.fnval_variants);
        fnval_dispatch = std::mem::take(&mut gi.fnval_dispatch);
        stream_closers = std::mem::take(&mut gi.stream_closers);
        // RFC-0114 §25's completeness half: `@__vyrn_globals_teardown` drops
        // every module-state binding in REVERSE declaration order, with the
        // same per-slot drops a block exit runs — so under VYRN_LEAK_CHECK
        // the audit table must come back empty, and "births equal frees" is
        // a checked exit condition instead of a peak-row approximation. It
        // runs only in that mode; a normal exit still leaves module state to
        // the operating system, exactly as before.
        let mut gt = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        gt.log_level = program.log_level;
        gt.log_sink = program.log_sink.clone();
        gt.protocol_methods = protocol_methods.clone();
        gt.fnval_variants = std::mem::take(&mut fnval_registry);
        gt.fnval_dispatch = std::mem::take(&mut fnval_dispatch);
        gt.stream_closers = std::mem::take(&mut stream_closers);
        for g in program.globals.iter().rev() {
            let Some((sym, ty)) = globals_map.get(&g.name) else {
                continue;
            };
            let Some(kind) = gt.rel_kind(ty) else {
                continue;
            };
            // A GENERIC declared release (`impl<T> Owned for Slots<T>`)
            // solves its type arguments from `slot_ty`, which reads the
            // scope — empty here, so the release was silently swallowed and
            // every `Slots` global left its slab at exit (the census's
            // 5-block signature). Park the binding so the lookup answers.
            gt.scope
                .push(vec![(g.name.clone(), sym.clone(), ty.clone())]);
            gt.emit_drop(sym, &kind);
            gt.scope.pop();
        }
        globals_init_ir.push_str("define internal void @__vyrn_globals_teardown() {\n");
        globals_init_ir.push_str("entry:\n");
        for a in &gt.allocas {
            globals_init_ir.push_str(a);
            globals_init_ir.push('\n');
        }
        for b in &gt.body {
            globals_init_ir.push_str(b);
            globals_init_ir.push('\n');
        }
        globals_init_ir.push_str("  ret void\n");
        globals_init_ir.push_str("}\n\n");
        let insts = std::mem::take(&mut gt.instantiations);
        enqueue(&emitted, &mut queued, &mut queue, insts);
        drain_ho(&mut gt, &mut out, &mut ho_queue, &mut lambda_emitted);
        fnval_registry = std::mem::take(&mut gt.fnval_variants);
        fnval_dispatch = std::mem::take(&mut gt.fnval_dispatch);
        stream_closers = std::mem::take(&mut gt.stream_closers);
    }

    // RFC-0114 §26's finish check: the SOURCE names of every function whose
    // body this emission walked — the reachability answer `plan.unconsumed`
    // needs, so a row in dead code alarms nobody.
    let mut fn_emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 1. Non-generic functions (main + others), collecting instantiations.
    for f in &program.functions {
        if !f.type_params.is_empty() {
            continue;
        }
        // An `extern` (RFC-0012) is a `declare`d import, not a `define` — its
        // declaration and attribute group were emitted in the preamble.
        if f.is_extern {
            continue;
        }
        // A `gen fn` (RFC-0021) runs only in the compiler's interpreter at
        // generation time — it is never called in a shipped binary. Its body may
        // use generation-only builtins (`listDir`/`moduleInterface`) with no
        // native/wasm lowering, so it is not emitted as a `define` here. (A
        // program that *calls* one at runtime should use `vyrn run`/`vyrn test`.)
        if f.is_gen {
            continue;
        }
        // `std/runtime` and `std/mem` (RFC-0125 §2.4) belong to the wasm
        // emitter. Until PLAN-0125-runtime §6 step 3 moves the native route onto
        // the wasm, this backend keeps its C copies and emits neither the
        // runtime's bodies nor the primitives' declarations.
        if f.name.starts_with(vyrn_frontend::loader::RUNTIME_PREFIX)
            || f.name.starts_with(vyrn_frontend::loader::MEM_PREFIX)
        {
            continue;
        }
        // A function that takes `fn`-typed parameters (RFC-0023) has no first-order
        // definition — it exists only as monomorphized specializations, emitted on
        // demand from the higher-order worklist. Skip its (unspecializable) shell.
        if f.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
            continue;
        }
        let sym = if f.name == "main" {
            "vyrn_main".to_string()
        } else {
            fn_sym(&f.name)
        };
        observe::note_inst(observe::Site::Native, &f.name, &[]);
        fn_emitted.insert(f.name.clone());
        let mut gen = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        gen.log_level = program.log_level;
        gen.log_sink = program.log_sink.clone();
        gen.protocol_methods = protocol_methods.clone();
        gen.globals = globals_map.clone();
        gen.gappend = gappend.clone();
        gen.fnval_variants = std::mem::take(&mut fnval_registry);
        gen.fnval_dispatch = std::mem::take(&mut fnval_dispatch);
        gen.stream_closers = std::mem::take(&mut stream_closers);
        gen.function(f, &sym, &mut out)?;
        out.push('\n');
        let insts = std::mem::take(&mut gen.instantiations);
        enqueue(&emitted, &mut queued, &mut queue, insts);
        drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
        fnval_registry = std::mem::take(&mut gen.fnval_variants);
        fnval_dispatch = std::mem::take(&mut gen.fnval_dispatch);
        stream_closers = std::mem::take(&mut gen.stream_closers);
    }

    // 2. Generic instantiations and higher-order specializations, transitively.
    // Both worklists feed each other (a generic body may take `fn` params; a
    // specialized instance may call generics), so drain them together.
    loop {
        if let Some((name, type_args)) = queue.pop() {
            let sym = mangle_name(&name, &type_args);
            if !emitted.insert(sym.clone()) {
                continue;
            }
            let f = funcs[&name];
            fn_emitted.insert(name.clone());
            check_inst_depth(&name, type_args.iter(), f.line, &types)?;
            observe::note_inst(observe::Site::Native, &name, &type_args);
            let subst: HashMap<String, Type> = f
                .type_params
                .iter()
                .cloned()
                .zip(type_args.iter().cloned())
                .collect();
            let mut gen = Gen::new(
                &ret_types,
                &param_types,
                &param_caps,
                &types,
                &variants,
                &str_globals,
                &subst,
                &funcs,
                &ownership,
                holes_map,
                owned_proto,
                &regex_globals,
                &program.impls,
                &plan,
                &program.type_decls,
            );
            gen.log_level = program.log_level;
            gen.log_sink = program.log_sink.clone();
            gen.protocol_methods = protocol_methods.clone();
            gen.globals = globals_map.clone();
            gen.gappend = gappend.clone();
            gen.fnval_variants = std::mem::take(&mut fnval_registry);
            gen.fnval_dispatch = std::mem::take(&mut fnval_dispatch);
            gen.stream_closers = std::mem::take(&mut stream_closers);
            gen.function(f, &sym, &mut out)?;
            out.push('\n');
            let insts = std::mem::take(&mut gen.instantiations);
            enqueue(&emitted, &mut queued, &mut queue, insts);
            drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
            fnval_registry = std::mem::take(&mut gen.fnval_variants);
            fnval_dispatch = std::mem::take(&mut gen.fnval_dispatch);
            stream_closers = std::mem::take(&mut gen.stream_closers);
            continue;
        }
        if let Some(inst) = ho_queue.pop() {
            if !emitted.insert(inst.sym.clone()) {
                continue;
            }
            fn_emitted.insert(inst.name.clone());
            check_inst_depth(
                &inst.name,
                inst.subst.values(),
                funcs.get(inst.name.as_str()).map_or(0, |f| f.line),
                &types,
            )?;
            // An RFC-0023 specialization IS an instantiation of a named function
            // — its extra identity is which target each `fn` parameter got, and
            // the type arguments underneath are the same list a generic call
            // hands the other worklist. Read them back in the callee's own
            // parameter order so both backends and the lowering spell one thing
            // one way.
            if observe::on() {
                let args: Vec<Type> = funcs
                    .get(inst.name.as_str())
                    .map(|f| {
                        f.type_params
                            .iter()
                            .map(|p| inst.subst.get(p).cloned().unwrap_or(Type::Unit))
                            .collect()
                    })
                    .unwrap_or_default();
                observe::note_inst(observe::Site::Native, &inst.name, &args);
            }
            let mut gen = Gen::new(
                &ret_types,
                &param_types,
                &param_caps,
                &types,
                &variants,
                &str_globals,
                &inst.subst,
                &funcs,
                &ownership,
                holes_map,
                owned_proto,
                &regex_globals,
                &program.impls,
                &plan,
                &program.type_decls,
            );
            gen.log_level = program.log_level;
            gen.log_sink = program.log_sink.clone();
            gen.protocol_methods = protocol_methods.clone();
            gen.globals = globals_map.clone();
            gen.gappend = gappend.clone();
            gen.fnval_variants = std::mem::take(&mut fnval_registry);
            gen.fnval_dispatch = std::mem::take(&mut fnval_dispatch);
            gen.stream_closers = std::mem::take(&mut stream_closers);
            gen.ho_function(&inst, &mut out)?;
            out.push('\n');
            let insts = std::mem::take(&mut gen.instantiations);
            enqueue(&emitted, &mut queued, &mut queue, insts);
            drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
            fnval_registry = std::mem::take(&mut gen.fnval_variants);
            fnval_dispatch = std::mem::take(&mut gen.fnval_dispatch);
            stream_closers = std::mem::take(&mut gen.stream_closers);
            continue;
        }
        break;
    }

    // RFC-0114 §26's finish: every plan row in an emitted function must have
    // been consumed by a query during emission — a missed site is a silent
    // leak the memory suite would otherwise find by measurement, made loud
    // here at build time instead.
    let missed = plan.unconsumed(&fn_emitted);
    if let Some((owner, class)) = missed.first() {
        return Err(format!(
            "internal: RFC-0114 §26 — the release plan placed {} decision(s) the emission never consumed, first {class} in `{owner}`; a missed site is a silent leak, and this failure is the loudness the plan exists for",
            missed.len()
        ));
    }

    // RFC-0037: the synthesized per-signature dispatchers, emitted after every
    // function so the variant set (one per source that flowed into storage
    // anywhere in the module) is complete. Every call inside them is direct;
    // the defensive default arm needs its message global exactly once.
    if !fnval_dispatch.is_empty() {
        out.push_str(
            &(trap_global(
                "@.fnval.bad",
                &vyrn_frontend::trap::line(vyrn_frontend::trap::BAD_FN_VALUE),
            ) + "\n"),
        );
        let mut dgen = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        dgen.protocol_methods = protocol_methods.clone();
        dgen.globals = globals_map.clone();
        dgen.gappend = gappend.clone();
        dgen.fnval_variants = fnval_registry.clone();
        let sigs = fnval_dispatch.clone();
        for sig in &sigs {
            dgen.emit_fnval_dispatcher(sig, &mut out)?;
        }
    }

    // Phase 10b: the derived copy over the same enum, emitted here for the same
    // reason — a variant's capture layout is only complete once every body has
    // been read. `internal`, so a module that never copies a `fn` value drops it.
    {
        let mut cgen = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        cgen.fnval_variants = fnval_registry.clone();
        cgen.emit_fnval_copy(&mut out)?;
        cgen.emit_fnval_release(&mut out)?;
    }

    // RFC-0090 M3: one release per element type, emitted here for the reason the
    // dispatchers are — a release CALLS a dispatcher, so it cannot be written
    // before the signature set is closed.
    if !stream_closers.is_empty() {
        let mut cgen = Gen::new(
            &ret_types,
            &param_types,
            &param_caps,
            &types,
            &variants,
            &str_globals,
            &empty_subst,
            &funcs,
            &ownership,
            holes_map,
            owned_proto,
            &regex_globals,
            &program.impls,
            &plan,
            &program.type_decls,
        );
        cgen.protocol_methods = protocol_methods.clone();
        cgen.globals = globals_map.clone();
        let elems = stream_closers.clone();
        for elem in &elems {
            cgen.emit_stream_closer(elem, &mut out)?;
        }
    }

    // The module-state initializer function (RFC-0013), defined after the user
    // functions (textual order is immaterial to LLVM).
    out.push_str(&globals_init_ir);

    // C entry point: call Vyrn's main and reduce its i64 to a process exit code.
    // Mask to the low 8 bits so the result matches the interpreter (which does
    // `code & 0xff`) and the POSIX 0–255 exit-status convention — otherwise a
    // return value > 255 would diverge on Windows, which preserves the full i32.
    out.push_str("define i32 @vyrn_entry() {\n");
    out.push_str("entry:\n");
    // Open the log file before running, if the program logs to one.
    let file_sink = matches!(program.log_sink, LogSink::File(_));
    if file_sink {
        out.push_str("  %lf = call ptr @fopen(ptr @.logpath, ptr @.logmode)\n");
        out.push_str("  store ptr %lf, ptr @__vyrn_log_file\n");
    }
    // Initialize module state (RFC-0013) before `main` runs — and therefore
    // before any exported extern handler the host calls afterward.
    if !program.globals.is_empty() {
        out.push_str("  call void @__vyrn_globals_init()\n");
    }
    out.push_str("  %r = call i64 @vyrn_main()\n");
    // Flush and close the log file after running (before returning the code).
    // A failed fopen left the handle null (bad path, unwritable directory):
    // fclose(NULL) is UB, so the close is guarded and the program degrades
    // silently, exactly as the interpreter does on an unopenable log file.
    if file_sink {
        out.push_str("  %lfc = load ptr, ptr @__vyrn_log_file\n");
        out.push_str("  %lfopen = icmp ne ptr %lfc, null\n");
        out.push_str("  br i1 %lfopen, label %log.close, label %log.done\n");
        out.push_str("log.close:\n");
        out.push_str("  %ignore = call i32 @fclose(ptr %lfc)\n");
        out.push_str("  br label %log.done\n");
        out.push_str("log.done:\n");
    }
    // RFC-0114 §25: under VYRN_LEAK_CHECK, drop module state and assert
    // the audit table is empty — the completeness half, as an exit condition.
    out.push_str("  %lk = call i32 @__vyrn_leak_check_on()\n");
    out.push_str("  %lkon = icmp ne i32 %lk, 0\n");
    out.push_str("  br i1 %lkon, label %leak.check, label %leak.done\n");
    out.push_str("leak.check:\n");
    out.push_str("  call void @__vyrn_teardown_begin()\n");
    if !program.globals.is_empty() {
        out.push_str("  call void @__vyrn_globals_teardown()\n");
    }
    out.push_str("  call void @__vyrn_audit_exit()\n");
    out.push_str("  br label %leak.done\n");
    out.push_str("leak.done:\n");
    out.push_str("  %m = and i64 %r, 255\n");
    out.push_str("  %c = trunc i64 %m to i32\n");
    out.push_str("  ret i32 %c\n");
    out.push_str("}\n");

    // Attribute groups for the `extern` imports (referenced by their declares
    // above) — top-level, so their position is immaterial to LLVM.
    if !extern_attr_groups.is_empty() {
        out.push('\n');
        out.push_str(&extern_attr_groups);
    }
    Ok(out)
}

/// One active loop's `break`/`continue` targets (RFC-0060). Captured when a
/// loop body begins emitting; a `break`/`continue` inside emits the releases the
/// placement put at that `break` (RFC-0101 M4) plus a `region_exit` for each
/// region opened past `region_depth`, before branching.
#[derive(Clone)]
struct LoopCtx {
    /// Label to branch to on `break` (the loop's exit block).
    break_label: String,
    /// Label to branch to on `continue` (the condition test, or a `for`'s latch
    /// block that steps the index then re-tests).
    continue_label: String,
    /// `region_depth` at loop-body entry: regions opened past this are exited on
    /// break/continue (the interpreter decrements its region depth on the same
    /// paths, so native must too — keeping the fixed region stack balanced).
    region_depth: usize,
}

/// What one owned binding is released with, in the backend's own vocabulary —
/// RFC-0101 §2.3's half of the split. The placement says which of these runs at
/// which exit and in what order; this says what running one emits.
#[derive(Clone)]
struct DropSlot {
    slot: String,
    kind: DropKind,
    /// Round twenty-seven: the value is provably malloc-side though the drop
    /// site sits inside a `region` — the walk's region guard stands down.
    malloc_side: bool,
    /// Registration order. Under a stack discipline the live bindings come off
    /// in reverse of it, which is the one thing a stream cursor's position still
    /// needs (see [`Gen::cursors`]).
    seq: u32,
}

/// Per-function code generator.
struct Gen<'a> {
    tmp: usize,
    label: usize,
    allocas: Vec<String>,
    body: Vec<String>,
    scope: Vec<Vec<(String, String, Type)>>, // (name, slot-reg, AST type)
    cur_block: String,
    terminated: bool,
    /// The enclosing function's return type (for coercing `return`/`?`).
    fn_ret: Type,
    /// Function name -> declared return type, for typing call results.
    ret_types: &'a HashMap<String, Type>,
    /// Function name -> parameter types, for coercing call arguments.
    param_types: &'a HashMap<String, Vec<Type>>,
    /// Function name -> parameter capabilities, for `modify` by-reference passing.
    param_caps: &'a HashMap<String, Vec<Capability>>,
    /// For the function being emitted: `modify` params to copy back before each
    /// return, as (local slot, incoming pointer, LLVM type).
    modify_copyout: Vec<(String, String, String)>,
    /// Validated-type + record declarations, for construction, resolution, layout.
    types: &'a HashMap<String, TypeDecl>,
    /// The program's OWN type declarations, for the one thing the map above
    /// cannot answer: a `where` predicate's node ADDRESS (RFC-0101 M6's third
    /// phase).
    ///
    /// `types` is `decl_map`'s copy, made once per engine, and this emitter then
    /// copied the predicate out of it again at every validation site. What the
    /// copying costs is not the copy: no recorded type can reach a node the
    /// program does not hold, so 1,043 of the corpus's off-program backend
    /// answers were inside one (RFC-0101 §1.5). Read from here, the predicate is
    /// the same tree the checker typed and the other two engines walk.
    decls: &'a [TypeDecl],
    /// Enum variant name -> (tag index, enum name), for construction.
    variants: &'a HashMap<String, (i64, String)>,
    /// String literal content -> module global name.
    str_globals: &'a HashMap<String, String>,
    /// Generic-parameter bindings for this instantiation (empty if not generic).
    subst: &'a HashMap<String, Type>,
    /// All functions, for resolving generic callees' signatures.
    funcs: &'a HashMap<String, &'a Function>,
    /// Generic instantiations discovered while emitting this function:
    /// (function name, concrete type arguments).
    instantiations: Vec<(String, Vec<Type>)>,
    /// Lexical `region` nesting depth, for routing `concat` (arena vs `malloc`).
    region_depth: usize,
    /// Identities of `let`s whose heap binding is reclaimed at block exit (and
    /// how), for the function currently being emitted (from `vyrn_frontend::own`).
    droppable: HashMap<usize, DropKind>,
    early: HashMap<usize, DropKind>,
    /// The whole program's ownership answers, looked up per emit.
    ownership: &'a vyrn_frontend::own::Ownership,
    /// RFC-0101 M4: the release steps placed at every exit of the function being
    /// emitted, keyed by the node the exit is AT. Read, never derived.
    placed: HashMap<(vyrn_frontend::own::Exit, usize), Vec<(usize, Option<Vec<String>>)>>,
    /// RFC-0093 M2: per `let` node, the places a `consume` took out of it. The
    /// release walk skips them, because the take already gave them away.
    holes_map: &'a HashMap<usize, Vec<String>>,
    /// The same, keyed by the slot the `let` declared — what [`Gen::emit_drop`]
    /// has in its hand. Cleared per function.
    hole_slots: HashMap<String, Vec<String>>,
    /// The holes the walk in progress must skip, relative to the place it is
    /// looking at. Taken at the top of [`Gen::deep_release`], so a walk into
    /// anything that is not a record starts empty.
    rel_holes: Vec<String>,
    /// The `Owned` table (RFC-0086 M1) — the one answer to "how is a value of
    /// this type reclaimed", shared with the automatic block-exit path so an
    /// explicit `drop x` cannot free a different set.
    owned: &'a vyrn_frontend::own::Owned,
    /// Every `impl` block, for `place` projection lookup (RFC-0091 M2). A
    /// projection is not a function, so `funcs` cannot answer for it.
    impls: &'a [vyrn_frontend::ast::ImplBlock],
    /// What each owned binding of this function is released WITH, keyed by
    /// `own`'s own key — the `Stmt::Let`'s node address, or the construct's for
    /// a temporary it owns. `seq` is the order it was registered in.
    ///
    /// **RFC-0101 M4: this is a lookup table, not a plan.** Until the deletion
    /// phase it was `Vec<Vec<(String, DropKind, usize)>>` — a stack of scope
    /// frames this engine pushed, popped and walked from a boundary index, which
    /// is the same stack the direct backend and the interpreter each kept
    /// privately and the same order all three asserted separately. The order is
    /// [`vyrn_frontend::own::Ownership::releases`]' now, read at the exit; what
    /// is left here is the half §2.3 leaves in a backend, which is the alloca a
    /// value lives in and the shape it is reclaimed by.
    drop_slots: HashMap<usize, DropSlot>,
    /// Registrations so far, which is what a [`DropSlot::seq`] counts.
    drop_seq: u32,
    /// The stream cursors a `for x in pull()` opened, innermost last, with the
    /// registration count each was opened at.
    ///
    /// **The one step the placement has nothing for** (RFC-0101 M4's phase-2
    /// gate names it `StreamCursor`, 6 walks over the corpus): the cursor is not
    /// a row of `own`'s map, because RFC-0075 M2b closes a stream's producer
    /// from the loop that made it rather than from a reclamation rule. Its
    /// POSITION in a mixed walk is still frame structure, so it is kept — a step
    /// registered before the cursor is a frame outside the loop, so the cursor
    /// runs first.
    cursors: Vec<(String, u32)>,
    /// The per-node release decisions (RFC-0114 §26) — every site this
    /// lowering frees at is an address in here, and in nothing of its own.
    plan: &'a vyrn_frontend::own::ReleasePlan,
    /// The registers holding those values, innermost call last. Pushed where the
    /// argument is EVALUATED and taken back where its call ends.
    arg_frees: Vec<(String, Type)>,
    /// Active loop targets for `break`/`continue` (RFC-0060), innermost last.
    /// A break/continue reclaims every scope pushed since loop-body entry (drops
    /// + region exits) before branching to the loop's exit / continue target.
    loop_ctx: Vec<LoopCtx>,
    /// The logging threshold ordinal (RFC-0008); calls below it emit no output.
    log_level: usize,
    /// Where log records are written (RFC-0008).
    log_sink: LogSink,
    /// Protocol methods (RFC-0002 §5): method name -> protocol name, for
    /// dispatching `m(recv, ..)` to the receiver type's impl.
    protocol_methods: HashMap<String, String>,
    /// Compiled `=~` patterns: pattern text -> (table global, accepting global,
    /// DFA start state). The globals are emitted once in the module preamble.
    regex_globals: &'a HashMap<String, (String, String, u32)>,
    /// Module-state bindings (RFC-0013): name -> (LLVM global symbol, type). A
    /// variable read/write that misses the local scope falls back to these,
    /// loading/storing through the global just like an alloca slot.
    globals: HashMap<String, (String, Type)>,
    /// Higher-order monomorphization (RFC-0023). While emitting a specialized
    /// instance of a function that takes `fn`-typed parameters, this maps each
    /// such parameter name to how to call it: the target symbol, the capture
    /// values (this instance's own leading extra parameters), and the target's
    /// signature. A call to the parameter becomes a direct call to the target
    /// with the captures prepended — no function pointer anywhere.
    fn_bindings: HashMap<String, FnBinding>,
    /// Higher-order instances discovered while emitting this function, to be
    /// emitted by the driver (like `instantiations`).
    ho_instances: Vec<HoInst>,
    /// Lifted lambda function definitions discovered while emitting this function,
    /// as (symbol, full IR text). The driver appends each once (deduped by symbol).
    lambda_defs: Vec<(String, String)>,
    /// The original name of the function whose body is being emitted, for
    /// deterministic lifted-lambda symbols (RFC-0023).
    cur_fn_name: String,
    /// Source-order ordinal of the next lambda lifted while emitting this function.
    lambda_counter: usize,
    /// RFC-0037 expected-type stack: storage boundaries (let/assign/field/
    /// element stores, returns, constructor payloads, stored-call arguments)
    /// push the declared type they are about to coerce into, so a lambda
    /// literal or bare function name evaluated INSIDE the initializer knows
    /// which `fn(P..) -> R` it is becoming. `IfExpr`/`match` arms deliberately
    /// push nothing — the enclosing target stays on top, so a conditional
    /// lambda adopts it naturally.
    expect: Vec<Type>,
    /// RFC-0037 defunctionalization registry, threaded through every `Gen`
    /// by the driver (tags are module-global): one entry per (signature,
    /// source) that flows into a stored function value, in first-construction
    /// order. The entry's index IS the variant tag.
    fnval_variants: Vec<FnValVariant>,
    /// Signatures called through a stored value anywhere in the module — each
    /// gets one synthesized dispatcher `@__vyrn_fndispatch_<sig>` at the end.
    fnval_dispatch: Vec<Type>,
    /// The element type of each stream header slot, for the release sites whose
    /// slot is a `fresh_alloca` rather than a declared binding (RFC-0090 M3).
    stream_slots: HashMap<String, Type>,
    /// Every element type whose `Stream` is released anywhere (RFC-0090 M3),
    /// threaded exactly as `fnval_dispatch` is and emitted beside it.
    stream_closers: Vec<Type>,
    /// Local `String` accumulators of the function being emitted that may be
    /// appended to in place (from `append_candidates`, recomputed per body).
    append_ok: std::collections::HashSet<String>,
    /// Variable slot -> its "this path allocated the buffer" flag slot, for the
    /// in-place append path. An entry exists only for a `let`-declared local
    /// `String` in `append_ok`; its presence is what licenses the fast path.
    /// Length and capacity live in the String's own header (RFC-0089 M1a).
    str_append: HashMap<String, String>,
    /// Module-state `String` accumulators (census P1): the global's symbol → the
    /// symbol of its one ownership flag. Seeded into `str_append` at the top of
    /// every body, so `g = g + …` takes the same in-place path a local does. A
    /// global's flag has to be a global too: it says whether the buffer THE
    /// PROGRAM holds was allocated by the program, and that outlives any call.
    gappend: HashMap<String, String>,
}

/// One variant of a synthesized stored-function enum (RFC-0037): a source
/// (named function or lifted lambda) registered under a structural signature.
/// The runtime value is `{ i64 tag, i64 payload }`; `payload` is 0 for an
/// empty capture set, else a pointer to a malloc'd capture block.
#[derive(Clone)]
struct FnValVariant {
    /// The normalized `fn(P..) -> R` signature this source flows into.
    sig: Type,
    /// The direct-call target: `vyrn_<name>` or a lifted lambda symbol.
    target_sym: String,
    /// The capture block's field types, in capture order (empty for a named
    /// function). The block is `{ llt(c0), llt(c1), ... }` on the heap.
    cap_tys: Vec<Type>,
    /// The target's OWN parameter/return types (a named function's declared
    /// signature may differ representationally from the slot's — record width
    /// subtyping, validated scalars — so dispatcher arms coerce through them).
    tgt_params: Vec<Type>,
    tgt_ret: Type,
}

/// How to invoke a `fn`-typed parameter inside a specialized instance (RFC-0023).
#[derive(Clone)]
struct FnBinding {
    target_sym: String,
    /// (capture-type, ssa-value) for each capture, prepended to every call. The
    /// ssa values are the specialized instance's own leading extra parameters.
    captures: Vec<(Type, String)>,
    param_tys: Vec<Type>,
    ret: Type,
}

/// A higher-order specialization of a function taking `fn`-typed parameters
/// (RFC-0023): the original function, the generic substitution, and the resolved
/// binding for each `fn`-typed parameter. Keyed (via `sym`) so identical
/// specializations are emitted once.
#[derive(Clone)]
struct HoInst {
    sym: String,
    name: String,
    subst: HashMap<String, Type>,
    bindings: Vec<HoParamBinding>,
}

/// The resolved binding for one `fn`-typed parameter of a higher-order instance.
#[derive(Clone)]
struct HoParamBinding {
    param_name: String,
    target_sym: String,
    /// The capture parameter types (concrete) this instance receives as extra
    /// leading arguments for this parameter.
    capture_tys: Vec<Type>,
    /// The target function's parameter and return types (concrete).
    param_tys: Vec<Type>,
    ret: Type,
}

impl<'a> Gen<'a> {
    fn new(
        ret_types: &'a HashMap<String, Type>,
        param_types: &'a HashMap<String, Vec<Type>>,
        param_caps: &'a HashMap<String, Vec<Capability>>,
        types: &'a HashMap<String, TypeDecl>,
        variants: &'a HashMap<String, (i64, String)>,
        str_globals: &'a HashMap<String, String>,
        subst: &'a HashMap<String, Type>,
        funcs: &'a HashMap<String, &'a Function>,
        ownership: &'a vyrn_frontend::own::Ownership,
        holes_map: &'a HashMap<usize, Vec<String>>,
        owned: &'a vyrn_frontend::own::Owned,
        regex_globals: &'a HashMap<String, (String, String, u32)>,
        impls: &'a [vyrn_frontend::ast::ImplBlock],
        plan: &'a vyrn_frontend::own::ReleasePlan,
        decls: &'a [TypeDecl],
    ) -> Self {
        Gen {
            plan,
            arg_frees: Vec::new(),
            tmp: 0,
            label: 0,
            allocas: Vec::new(),
            body: Vec::new(),
            scope: vec![Vec::new()],
            cur_block: "entry".into(),
            terminated: false,
            fn_ret: Type::Unit,
            ret_types,
            param_types,
            param_caps,
            modify_copyout: Vec::new(),
            types,
            decls,
            variants,
            str_globals,
            subst,
            funcs,
            instantiations: Vec::new(),
            region_depth: 0,
            droppable: HashMap::new(),
            early: HashMap::new(),
            ownership,
            placed: HashMap::new(),
            holes_map,
            hole_slots: HashMap::new(),
            rel_holes: Vec::new(),
            owned,
            impls,
            drop_slots: HashMap::new(),
            drop_seq: 0,
            cursors: Vec::new(),
            loop_ctx: Vec::new(),
            log_level: DEFAULT_LOG_LEVEL,
            log_sink: LogSink::Stderr,
            protocol_methods: HashMap::new(),
            regex_globals,
            globals: HashMap::new(),
            fn_bindings: HashMap::new(),
            ho_instances: Vec::new(),
            lambda_defs: Vec::new(),
            cur_fn_name: String::new(),
            lambda_counter: 0,
            expect: Vec::new(),
            fnval_variants: Vec::new(),
            fnval_dispatch: Vec::new(),
            stream_closers: Vec::new(),
            stream_slots: HashMap::new(),
            append_ok: std::collections::HashSet::new(),
            str_append: HashMap::new(),
            gappend: HashMap::new(),
        }
    }

    /// Resolve a type to its structural form: substitute generic parameters for
    /// this instantiation, then delegate to the shared resolver (which also
    /// evaluates the `Omit`/`Pick`/`Merge` transformers).
    fn resolve(&self, ty: &Type) -> Type {
        let t = vyrn_frontend::types::substitute(ty, self.subst);
        vyrn_frontend::types::resolve(&t, self.types)
    }

    /// The fields of `ty` if it is (resolves to) a record.
    fn record_fields(&self, ty: &Type) -> Option<Vec<Field>> {
        let t = vyrn_frontend::types::substitute(ty, self.subst);
        vyrn_frontend::types::record_fields(&t, self.types)
    }

    /// The widest payload count of the named enum (0 if not an enum).
    fn enum_arity(&self, enum_name: &str) -> usize {
        match self.types.get(enum_name).map(|d| &d.base) {
            Some(Type::Enum(vs)) => vs.iter().map(|v| v.payload.len()).max().unwrap_or(0),
            _ => 0,
        }
    }

    /// The fully-applied type of a just-constructed enum variant. For a generic
    /// enum (`enum E<T>`, the injected `LoadResult<T>`), the concrete type
    /// arguments are inferred by unifying the variant's declared payload types
    /// against the argument types that filled them — mirroring what the built-in
    /// `fromJson` returns (`App("Validation", [target])`). A parameter the
    /// variant does not mention stays `Unit`; a non-generic enum stays `Named`.
    /// Carrying the arguments is load-bearing: a downstream `match` substitutes
    /// them into the variant payloads, so a binding like `Loaded(s) => s`
    /// recovers the concrete payload type instead of the bare `Type::Param`
    /// (which lowers to an invalid `alloca void`).
    fn applied_enum_type(&self, enum_name: &str, variant: &str, arg_tys: &[Type]) -> Type {
        let Some(decl) = self.types.get(enum_name) else {
            return Type::Named(enum_name.to_string());
        };
        if decl.type_params.is_empty() {
            return Type::Named(enum_name.to_string());
        }
        let declared: Vec<Type> = match &decl.base {
            Type::Enum(vs) => vs
                .iter()
                .find(|v| v.name == variant)
                .map(|v| v.payload.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        applied_type(Some(decl), enum_name, &declared, arg_tys)
    }

    /// Whether `t` is a generic enum instantiation whose type arguments are all
    /// concretely known. [`ty_is_concrete_app`] is the rule; this is the
    /// substitution this emitter is in.
    fn ty_is_concrete_app(&self, t: &Type) -> bool {
        ty_is_concrete_app(t, &|a| self.resolve(a))
    }

    /// The LLVM type string for `ty`. Records lower to a `{ .. }` literal struct.
    ///
    /// Substitution is this emitter's (it knows the monomorphization it is in);
    /// the shape rules themselves are [`llt_of`], because RFC-0077's direct wasm
    /// backend needs the same answers and two matches on `Type` would be two
    /// sources of truth for one fact.
    fn llt(&self, ty: &Type) -> String {
        llt_of(
            &vyrn_frontend::types::substitute(ty, self.subst),
            self.types,
        )
    }

    /// Coerce a value of type `from` to type `to`, emitting a field-by-field
    /// rebuild for structural record width subtyping (RFC-0002). For everything
    /// else the bit pattern is unchanged and only the reported type differs.
    /// RFC-0020 M1: coerce `op` (produced by `expr`) into `to`, but SKIP the
    /// runtime validation when the checker's containment proof holds — a string
    /// interpolation whose language ⊆ `to`, or a finite string variable
    /// contained in `to`. The value representation of a validated `String` is
    /// identical to `String`, so a proven flow simply coerces to the base and
    /// retags. Both backends run [`vyrn_frontend::finite::string_flow_proven`]
    /// independently on the same AST, so they skip identically (the consteval
    /// precedent). Any non-string / non-proven flow is the ordinary [`coerce`].
    fn coerce_flow(
        &mut self,
        op: String,
        expr: &Expr,
        from: &Type,
        to: &Type,
    ) -> Result<(String, Type), String> {
        if self.string_flow_proven(expr, to) {
            if let Some(base) = validation_required(from, to, self.types).map(|d| d.base.clone()) {
                let (v, _) = self.coerce(op, from, &base)?;
                return Ok((v, to.clone()));
            }
        }
        self.coerce(op, from, to)
    }

    /// Whether a flow of `expr` into `to` is statically proven contained (so its
    /// runtime validation may be skipped). Resolves interpolation holes / a
    /// finite-string receiver through the local scope's declared types.
    fn string_flow_proven(&self, expr: &Expr, to: &Type) -> bool {
        let resolve = |e: &Expr| match e {
            Expr::Var { name, .. } => self.lookup(name).map(|(_, t)| t),
            _ => None,
        };
        vyrn_frontend::finite::string_flow_proven(expr, to, self.types, &resolve)
    }

    fn coerce(&mut self, op: String, from: &Type, to: &Type) -> Result<(String, Type), String> {
        // A `Never` (RFC-0079) is `poison` in a block the `panic` already left
        // through `unreachable`. There is nothing to reconcile — and running a
        // validation here would emit a live-looking check over a value that does
        // not exist. `poison` is valid at `to` as it stands.
        if matches!(from, Type::Never) {
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::Never);
            return Ok((op, to.clone()));
        }
        // AUTOMATIC VALIDATION: a value flowing into a predicated named type
        // coerces to its base, then runs the `where` predicate inline and traps
        // with the canonical message — mirroring the interpreter's `coerce`.
        // Whether that is required is [`validation_required`]'s call, not this
        // site's, because the direct wasm backend has to reach the same verdict.
        if let Some(decl) = validation_required(from, to, self.types).cloned() {
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::Validate);
            let (v, _) = self.coerce(op, from, &decl.base)?;
            self.emit_validation(&decl, &v)?;
            return Ok((v, to.clone()));
        }
        // A function value flowing between fn-typed spellings (RFC-0037): the
        // structural form and any named alias share the `{ i64, i64 }` enum
        // representation, so this is a re-tag only. (Sources — lambda literals
        // and bare function names — were constructed AS the slot's signature by
        // the expected-type stack, so no reshaping can be needed here.)
        if matches!(self.resolve(to), Type::Fn(..)) && matches!(self.resolve(from), Type::Fn(..)) {
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::FnRetag);
            return Ok((op, to.clone()));
        }
        // NOTE: there is deliberately no Option/Result payload reshape here.
        // Until RFC-0082 the boxed payload of `Ok([1,2,3])` was a fixed
        // `[N x T]` while the target wanted the growable `Array<T>` triple, and
        // this site repaired it afterwards by branching on the tag and
        // re-materializing the arm (`rebox_sum`). That repair is gone because
        // `Some`/`Ok`/`Err` now coerce the payload INTO the expected type before
        // boxing it, as the user-enum constructor always has — so the words are
        // right at the source and there is nothing to reinterpret. Nothing else
        // can produce an `Option<Array<T, N>>` that reaches an `Array<T>`
        // target: the checker refuses that flow for any expression other than an
        // array literal directly under a constructor (`Option<Array<Int64, 3>>`
        // is not assignable to `Option<Array<Int64>>`), which is exactly the case
        // construction now covers.
        // Fixed arrays coerce element-wise (unrolled), so `[x, y]` flowing into
        // an `Array<Age, 2>` validates every element.
        if let (Type::ArrayN(fi, fnn), Type::ArrayN(ti, tn)) =
            (&self.resolve(from), &self.resolve(to))
        {
            if fi != ti && fnn == tn {
                crate::observe::note_rung(observe::Site::Native, from, to, Rung::Elementwise);
                let fell = self.llt(fi);
                let from_ll = format!("[{fnn} x {fell}]");
                let tell = self.llt(ti);
                let to_ll = format!("[{tn} x {tell}]");
                let mut cur = "undef".to_string();
                for i in 0..*tn {
                    let ext = self.fresh_tmp();
                    self.emit(format!("{ext} = extractvalue {from_ll} {op}, {i}"));
                    let (cv, _) = self.coerce(ext, fi, ti)?;
                    let ins = self.fresh_tmp();
                    self.emit(format!(
                        "{ins} = insertvalue {to_ll} {cur}, {tell} {cv}, {i}"
                    ));
                    cur = ins;
                }
                return Ok((cur, to.clone()));
            }
        }
        // A contextual array literal: a fixed `[N x T]` value flowing into a
        // growable `Array<T>` slot (a `let`/arg/return annotation) is copied to
        // the heap and wrapped in the `{ptr,len,cap}` triple — the same lowering
        // `list([..])` used. Element types already match (the checker coerced
        // each element into `T` when it built the literal), so no per-element
        // step is needed here.
        {
            let rf = self.resolve(from);
            let rt = self.resolve(to);
            if let (Type::ArrayN(fi, _), Type::Array(ti)) = (&rf, &rt) {
                // The reshape reinterprets the fixed `[N x T]` buffer as a
                // growable `{ptr,len,cap}` triple; it is sound whenever the
                // element representation matches. Fall back to comparing the LLVM
                // layout so an element whose static type is spelled differently
                // but lowers identically still reshapes — e.g. a lambda literal
                // (`fn(..) -> ..`) flowing into an `Array<AliasFn>`, where the
                // element type is the closure value and `ti` is the fn-type alias.
                if fi == ti || self.llt(fi) == self.llt(ti) {
                    crate::observe::note_rung(observe::Site::Native, from, to, Rung::Heapify);
                    let inner = (**fi).clone();
                    let (triple, _) = self.array_n_to_heap(&op, &inner, &rf)?;
                    return Ok((triple, to.clone()));
                }
            }
            // A contextual array literal `[..]` (a fixed `[len x T]`) flowing
            // into a `SmallArray<T, N>` slot (RFC-0056): lift the elements into
            // the inline buffer (the checker proved `len <= N`).
            if let (Type::ArrayN(_, len), Type::SmallArray(ti, n)) = (&rf, &rt) {
                crate::observe::note_rung(observe::Site::Native, from, to, Rung::Inline);
                let inner = (**ti).clone();
                let (sa, _) = self.array_n_to_smallarray(&op, &inner, *len, *n)?;
                return Ok((sa, to.clone()));
            }
        }
        // A plain integer flowing into a sized-integer slot truncates to `iN`
        // (matching the interpreter's `wrap_intn`). Same-width is a no-op.
        if let Type::IntN { bits, .. } = self.resolve(to) {
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::Resize);
            let fll = self.llt(from);
            let tll = format!("i{bits}");
            if fll != tll && matches!(self.resolve(from), Type::Int | Type::IntN { .. }) {
                let t = self.fresh_tmp();
                // Widening (fll narrower than tll) shouldn't arise post-checker;
                // Int(i64)→iN and wider→narrower both truncate.
                self.emit(format!("{t} = trunc {fll} {op} to {tll}"));
                return Ok((t, to.clone()));
            }
            return Ok((op, to.clone()));
        }
        // A default `double` literal flowing into a `Float32` slot rounds to single
        // precision (`fptrunc`), matching the interpreter's `as f32`.
        if self.resolve(to) == Type::Float32 && self.resolve(from) == Type::Float {
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::FloatCross);
            let t = self.fresh_tmp();
            self.emit(format!("{t} = fptrunc double {op} to float"));
            return Ok((t, to.clone()));
        }
        if let (Some(ff), Some(tf)) = (self.record_fields(from), self.record_fields(to)) {
            if ff == tf {
                crate::observe::note_rung(observe::Site::Native, from, to, Rung::Identity);
                return Ok((op, to.clone()));
            }
            crate::observe::note_rung(observe::Site::Native, from, to, Rung::Rebuild);
            let from_ll = self.llt(from);
            let to_ll = self.llt(to);
            let mut cur = "undef".to_string();
            for (i, need) in tf.iter().enumerate() {
                let (src_idx, src_field) = ff
                    .iter()
                    .enumerate()
                    .find(|(_, h)| h.name == need.name)
                    .map(|(idx, h)| (idx, h.clone()))
                    .ok_or_else(|| format!("field `{}` missing during coercion", need.name))?;
                let ext = self.fresh_tmp();
                self.emit(format!("{ext} = extractvalue {from_ll} {op}, {src_idx}"));
                // Recurse so nested records coerce too.
                let (fv, _) = self.coerce(ext, &src_field.ty, &need.ty)?;
                let field_ll = self.llt(&need.ty);
                let ins = self.fresh_tmp();
                self.emit(format!(
                    "{ins} = insertvalue {to_ll} {cur}, {field_ll} {fv}, {i}"
                ));
                cur = ins;
            }
            return Ok((cur, to.clone()));
        }
        // THE END OF THE LADDER, and it is not the end of the other one: the
        // direct backend refuses an unhandled pair (RFC-0101 §1.5). A pair that
        // reaches here is one this emitter reinterprets — the bits as they are,
        // under a new name — so the corpus gate holds every one of them to the
        // plan's [`Rung::Identity`], and a pair the plan REFUSES landing here is
        // a program that compiles on one target only.
        crate::observe::note_rung(observe::Site::Native, from, to, Rung::Identity);
        Ok((op, to.clone()))
    }

    fn fresh_tmp(&mut self) -> String {
        let t = format!("%t{}", self.tmp);
        self.tmp += 1;
        t
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let l = format!("{prefix}.{}", self.label);
        self.label += 1;
        l
    }

    fn emit(&mut self, line: String) {
        self.body.push(format!("  {line}"));
    }

    /// Everything an emission writes, so a failed one can be taken back.
    ///
    /// The drop sites in `emit_drop` swallow their errors on purpose — a drop
    /// this cannot emit is a leak, never a wrong free — but that is an argument
    /// about the DROP, not about the instructions the failed attempt already
    /// wrote. `gen_call` and `deep_release` write into `body` and `allocas` as
    /// they go and can bail at any nested expression, and what they left behind
    /// went out as half a call: invalid IR, so the reader got a clang parse error
    /// about a temporary nobody defined instead of a Vyrn diagnostic about the
    /// cause. The block cursor comes too, because a bail after `emit_label` would
    /// otherwise leave `cur_block` naming a label the truncation removed.
    fn mark(&self) -> (usize, usize, String, bool) {
        (
            self.body.len(),
            self.allocas.len(),
            self.cur_block.clone(),
            self.terminated,
        )
    }

    /// Undo everything emitted since [`Gen::mark`].
    fn rewind(&mut self, (body, allocas, block, terminated): (usize, usize, String, bool)) {
        self.body.truncate(body);
        self.allocas.truncate(allocas);
        self.cur_block = block;
        self.terminated = terminated;
    }

    /// Emit a terminator and mark the current block finished.
    fn emit_term(&mut self, line: String) {
        self.body.push(format!("  {line}"));
        self.terminated = true;
    }

    fn emit_label(&mut self, label: &str) {
        self.body.push(format!("{label}:"));
        self.cur_block = label.to_string();
        self.terminated = false;
    }

    /// A fresh anonymous stack slot of the given LLVM type (added to the entry
    /// block's allocas). Used for spilling value aggregates to memory.
    /// Allocate `size` bytes on the heap, routed through the active region arena
    /// when one is on the stack (so region examples reclaim it) or plain `malloc`
    /// otherwise. Returns the buffer pointer.
    ///
    /// A `String` buffer is the ONLY thing that may come here, and
    /// [`Gen::str_alloc`] is the only caller. The arena frees what it allocated
    /// at the closing brace, so a second owner is a double free — and the walk
    /// stands off exactly one thing: a `String`, at the binding (`own.rs`'s
    /// `Leak::Region`) and one level down ([`Gen::deep_release`]). A buffer that
    /// `realloc` may move is `__vyrn_malloc`'s: the arena's side vector holds the
    /// address it will hand `free`, so a moved block dangles it, which is why the
    /// in-place append refuses a slot inside a region (`Stmt::Assign`).
    ///
    /// What the walk hands back is the arena's too, on one path: a `return` out
    /// of a region pops the frame without freeing it and the value the return
    /// carried is the caller's to release. That works because the block is
    /// exactly a `__vyrn_malloc` block — see [`REGION_RUNTIME`].
    fn heap_alloc(&mut self, size: &str) -> String {
        let buf = self.fresh_tmp();
        if self.region_depth > 0 {
            self.emit(format!("{buf} = call ptr @__vyrn_region_alloc(i64 {size})"));
        } else {
            self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {size})"));
        }
        buf
    }

    /// A fresh `String` buffer with room for `cap` bytes plus the NUL, its
    /// `{ len, cap }` header written and the terminator placed at `len`
    /// (RFC-0089 M1a). The caller fills the `len` bytes.
    ///
    /// Routed through [`Gen::heap_alloc`], so inside a `region` the header and
    /// the bytes come from the arena together — one block, freed once at region
    /// exit, exactly as before.
    fn str_alloc(&mut self, len: &str, cap: &str) -> String {
        let tot = self.fresh_tmp();
        self.emit(format!("{tot} = add i64 {cap}, {}", STR_HDR + 1));
        let base = self.heap_alloc(&tot);
        let cp = self.fresh_tmp();
        let s = self.fresh_tmp();
        let e = self.fresh_tmp();
        self.emit(format!("store i64 {len}, ptr {base}"));
        self.emit(format!("{cp} = getelementptr i8, ptr {base}, i64 8"));
        self.emit(format!("store i64 {cap}, ptr {cp}"));
        self.emit(format!("{s} = getelementptr i8, ptr {base}, i64 {STR_HDR}"));
        self.emit(format!("{e} = getelementptr i8, ptr {s}, i64 {len}"));
        self.emit(format!("store i8 0, ptr {e}"));
        s
    }

    /// The byte length of a `String` value: one load from its header, where it
    /// used to be a `strlen` scan (RFC-0087 P2).
    fn str_len(&mut self, v: &str) -> String {
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_str_len(ptr {v})"));
        n
    }

    /// Release the operand `v` a String concatenation has just copied out of,
    /// when the operand EXPRESSION allocated it (RFC-0096 M3).
    ///
    /// `"n" + i.toString()` leaks the `@str` result: it feeds `@concat` and no
    /// binding ever owns it, so `own.rs` — which keys every release on a `let`
    /// — has nothing to write a row against. The consumer is the only place
    /// that knows the temporary exists AND knows it is finished with, so the
    /// release goes here. Measured native before the fix, `"n" + i.toString()`
    /// in a loop: 19.9 MB peak at 250,000 turns and 54.1 MB at four times that.
    ///
    /// Inside a `region` the buffer came from the ARENA and the region exit
    /// reclaims it. The two mechanisms partition every allocation, which is the
    /// rule `own` already states as `Fate::Leaked(Leak::Region)`, so this stands
    /// aside there exactly as the block-exit release does.
    fn free_str_temp(&mut self, e: &Expr, v: &str) {
        if self.region_depth > 0 || !vyrn_frontend::own::str_temporary(e) {
            return;
        }
        self.emit(format!("call void @__vyrn_str_free(ptr {v})"));
    }

    /// Concatenate two `String` pointers into a fresh, NUL-terminated buffer.
    /// Shared by the `@concat` builtin (interpolation) and the `a + b` operator
    /// lowering. Routing is lexical: inside a `region` the buffer is drawn from
    /// the arena (freed at region exit); outside, from `malloc` (freed by
    /// ownership analysis if it does not escape, else leaked). The two paths are
    /// mutually exclusive, so no buffer is ever freed twice.
    fn emit_str_concat(&mut self, a: &str, b: &str) -> String {
        // Both lengths are header loads since RFC-0089 M1a, so the two `strlen`
        // scans this used to open with are gone and the copies are `memcpy` of a
        // known count rather than `strcpy`/`strcat` re-scanning what they write.
        //
        // Outside a region that whole sequence is one call to the runtime helper
        // — smaller IR at every `+` on Strings, and the site a reader can count.
        // Inside a region the buffer must come from the arena, so the sequence is
        // emitted here with `str_alloc` doing the routing.
        if self.region_depth == 0 {
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = call ptr @__vyrn_str_concat(ptr {a}, ptr {b})"
            ));
            return r;
        }
        let la = self.str_len(a);
        let lb = self.str_len(b);
        let sum = self.fresh_tmp();
        self.emit(format!("{sum} = add i64 {la}, {lb}"));
        let buf = self.str_alloc(&sum, &sum);
        let at = self.fresh_tmp();
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {a}, i64 {la}, i1 false)"
        ));
        self.emit(format!("{at} = getelementptr i8, ptr {buf}, i64 {la}"));
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {at}, ptr {b}, i64 {lb}, i1 false)"
        ));
        buf
    }

    /// `std/num`'s `f64Str` on an already-lowered float — the fixed six decimal
    /// places, and since RFC-0081 M2 the only float formatter this backend has.
    /// Both `print` and `@str` come here, so the two cannot drift apart.
    ///
    /// A `Float32` promotes first: the interpreter formats `*f as f64`, and single
    /// precision having a path of its own would be a second thing to keep in step.
    ///
    /// The call is emitted rather than built as an `Expr` and handed to `gen_call`
    /// because the value is already lowered — the arm has to see the static type
    /// before it can pick a case, and this emitter has no type-only peek. (The
    /// symbol rule is `call_parts`'s `vyrn_{name}`; getting it wrong is an
    /// undefined symbol at link, not a wrong answer at runtime.) The result is a
    /// fresh allocation — `f64Str` guarantees that, including for the three
    /// non-finite words — so the ownership analysis may free it like any `@str`.
    fn gen_f64_str(&mut self, v: &str, ty: &Type) -> Result<String, String> {
        let d = if matches!(self.resolve(ty), Type::Float32) {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = fpext float {v} to double"));
            t
        } else {
            v.to_string()
        };
        let f = vyrn_frontend::loader::F64_STR;
        if !self.funcs.contains_key(f) {
            // The refusal `toJson` makes when its serializer is not in the link:
            // `std/num` is injected into any program that mentions `print` or
            // `@str`, so reaching this means a program built without a std root.
            return Err(format!(
                "formatting a `Float64` needs `{f}`, which is not in the link — it is \
                 injected into any program that prints or interpolates (RFC-0081 M2), \
                 so this is a program built without a std root"
            ));
        }
        let t = self.fresh_tmp();
        self.emit(format!("{t} = call ptr @{}(double {d})", fn_sym(f)));
        Ok(t)
    }

    /// The ownership flag of a local String accumulator, created on demand.
    ///
    /// This used to be a `(len, cap)` shadow pair, because a Vyrn String had no
    /// header and growing one in place needed both kept beside the slot.
    /// RFC-0089 M1a moved length and capacity into the String itself, so what
    /// remains here is one bit: **did THIS path allocate the buffer the slot
    /// holds?**
    ///
    /// The bit is not the same question as "is this buffer on the heap". A concat result has a
    /// real capacity and is still not ours to grow in place, because `s = t`
    /// aliases and nothing yet forbids that — the conventions do (RFC-0089 M2),
    /// and this flag retires with them. `0` means the next append copies into a
    /// fresh owned buffer first. Every other write to the variable stores 0
    /// back, and the entry block zeroes it, so the invariant holds on every
    /// path (including the second trip through a loop that re-runs the `let`).
    fn str_append_shadow(&mut self, slot: &str) -> String {
        if let Some(flag) = self.str_append.get(slot) {
            return flag.clone();
        }
        let owned = self.fresh_alloca("i64");
        self.allocas.push(format!("  store i64 0, ptr {owned}"));
        self.str_append.insert(slot.to_string(), owned.clone());
        owned
    }

    /// Append `val` to the String in `slot`, in place, growing geometrically.
    /// `emit_str_concat` allocates and copies both halves every time, which
    /// makes `out = out + piece` in a loop quadratic in the result — the shape
    /// every generator is written in. Three steps, each re-reading the String's
    /// header so no phi is needed (mem2reg folds the loads away): take ownership
    /// of the buffer if it is not ours, reserve room, copy.
    ///
    /// Since RFC-0089 M1a the length and the capacity are read from and written
    /// to the String header, not to shadow slots beside the variable. So
    /// `s.byteLength` mid-spine is now correct and O(1), and a drop of the
    /// accumulator recovers the capacity it was grown to.
    /// `free_taken` — the take-ownership copy may FREE the buffer it copied
    /// out of, because the plan proved the place owns its value at this store
    /// (exit-residue round fifteen: the copy used to abandon it, one
    /// initializer buffer per accumulator whose first value was a fresh
    /// concat — `onPair`'s 36 bytes, twelve times per htmltree run). A
    /// borrowed value answers `store_owned_at` false and keeps the old
    /// behavior; a static literal is freed by `str_free`'s own cap guard.
    fn emit_str_append_owned(&mut self, slot: &str, val: &str, free_taken: bool) {
        let flag = self.str_append_shadow(slot);
        let vlen = self.str_len(val);

        // Step 1: the flag is 0 — copy the borrowed buffer into one we own.
        let own_l = self.fresh_label("app.own");
        let have_l = self.fresh_label("app.have");
        let f0 = self.fresh_tmp();
        let owned = self.fresh_tmp();
        self.emit(format!("{f0} = load i64, ptr {flag}"));
        self.emit(format!("{owned} = icmp ne i64 {f0}, 0"));
        self.emit_term(format!("br i1 {owned}, label %{have_l}, label %{own_l}"));
        self.emit_label(&own_l);
        let ob = self.fresh_tmp();
        self.emit(format!("{ob} = load ptr, ptr {slot}"));
        let ol = self.str_len(&ob);
        let need0 = self.fresh_tmp();
        let big0 = self.fresh_tmp();
        let c0 = self.fresh_tmp();
        self.emit(format!("{need0} = add i64 {ol}, {vlen}"));
        self.emit(format!("{big0} = icmp ugt i64 {need0}, 32"));
        self.emit(format!("{c0} = select i1 {big0}, i64 {need0}, i64 32"));
        let nb0 = self.str_alloc(&ol, &c0);
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {nb0}, ptr {ob}, i64 {ol}, i1 false)"
        ));
        if free_taken {
            self.emit(format!("call void @__vyrn_str_free(ptr {ob})"));
        }
        self.emit(format!("store ptr {nb0}, ptr {slot}"));
        self.emit(format!("store i64 1, ptr {flag}"));
        self.emit_term(format!("br label %{have_l}"));

        // Step 2: reserve `len + vlen` content bytes, doubling so N appends are
        // O(N). `cap` counts content, so the NUL always has its own byte.
        self.emit_label(&have_l);
        let grow_l = self.fresh_label("app.grow");
        let copy_l = self.fresh_label("app.copy");
        let cur = self.fresh_tmp();
        self.emit(format!("{cur} = load ptr, ptr {slot}"));
        let len1 = self.str_len(&cur);
        let capp = self.fresh_tmp();
        let cap1 = self.fresh_tmp();
        let need = self.fresh_tmp();
        let short = self.fresh_tmp();
        self.emit(format!("{capp} = getelementptr i8, ptr {cur}, i64 -8"));
        self.emit(format!("{cap1} = load i64, ptr {capp}"));
        self.emit(format!("{need} = add i64 {len1}, {vlen}"));
        self.emit(format!("{short} = icmp ugt i64 {need}, {cap1}"));
        self.emit_term(format!("br i1 {short}, label %{grow_l}, label %{copy_l}"));
        self.emit_label(&grow_l);
        let dbl = self.fresh_tmp();
        let usedbl = self.fresh_tmp();
        let nc = self.fresh_tmp();
        self.emit(format!("{dbl} = shl i64 {cap1}, 1"));
        self.emit(format!("{usedbl} = icmp ugt i64 {dbl}, {need}"));
        self.emit(format!("{nc} = select i1 {usedbl}, i64 {dbl}, i64 {need}"));
        let obase = self.fresh_tmp();
        let ntot = self.fresh_tmp();
        let nbase = self.fresh_tmp();
        let ncapp = self.fresh_tmp();
        let nbuf = self.fresh_tmp();
        self.emit(format!(
            "{obase} = getelementptr i8, ptr {cur}, i64 -{STR_HDR}"
        ));
        self.emit(format!("{ntot} = add i64 {nc}, {}", STR_HDR + 1));
        self.emit(format!(
            "{nbase} = call ptr @__vyrn_realloc(ptr {obase}, i64 {ntot})"
        ));
        self.emit(format!("{ncapp} = getelementptr i8, ptr {nbase}, i64 8"));
        self.emit(format!("store i64 {nc}, ptr {ncapp}"));
        self.emit(format!(
            "{nbuf} = getelementptr i8, ptr {nbase}, i64 {STR_HDR}"
        ));
        self.emit(format!("store ptr {nbuf}, ptr {slot}"));
        self.emit_term(format!("br label %{copy_l}"));

        // Step 3: copy the operand's bytes AND its NUL over the old terminator,
        // then publish the new length in the header.
        self.emit_label(&copy_l);
        let buf = self.fresh_tmp();
        self.emit(format!("{buf} = load ptr, ptr {slot}"));
        let len2 = self.str_len(&buf);
        let dst = self.fresh_tmp();
        let n1 = self.fresh_tmp();
        let nlen = self.fresh_tmp();
        self.emit(format!("{dst} = getelementptr i8, ptr {buf}, i64 {len2}"));
        self.emit(format!("{n1} = add i64 {vlen}, 1"));
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {val}, i64 {n1}, i1 false)"
        ));
        self.emit(format!("{nlen} = add i64 {len2}, {vlen}"));
        self.emit(format!(
            "call void @__vyrn_str_setlen(ptr {buf}, i64 {nlen})"
        ));
    }

    /// Invalidate the append shadow after any other write to `slot`: the
    /// variable now holds a pointer this path did not allocate.
    fn str_append_reset(&mut self, slot: &str) {
        if let Some(flag) = self.str_append.get(slot) {
            let flag = flag.clone();
            self.emit(format!("store i64 0, ptr {flag}"));
        }
    }

    /// Copy a fixed `[N x T]` aggregate value `v` (type `arr_ty`) into a fresh
    /// heap buffer and wrap it in the `{ptr,len,cap}` growable-array triple —
    /// the lowering behind a contextual array literal `[..]` in an `Array<T>`
    /// position (and the old `list([..])`). Always plain `malloc`, never the
    /// region arena: `push` grows this buffer with `realloc` and cleanup uses
    /// `free`, both undefined on an arena interior pointer. Copying (not
    /// aliasing) is what makes the `ArrayN → Array` coercion sound.
    fn array_n_to_heap(
        &mut self,
        v: &str,
        inner: &Type,
        arr_ty: &Type,
    ) -> Result<(String, Type), String> {
        let n = match self.resolve(arr_ty) {
            Type::ArrayN(_, n) => n,
            other => return Err(format!("array_n_to_heap on non-ArrayN {other:?}")),
        };
        let ell = self.llt(inner);
        let aggty = format!("[{n} x {ell}]");
        let szp = self.fresh_tmp();
        let sz = self.fresh_tmp();
        self.emit(format!("{szp} = getelementptr {aggty}, ptr null, i64 1"));
        self.emit(format!("{sz} = ptrtoint ptr {szp} to i64"));
        let buf = self.fresh_tmp();
        self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {sz})"));
        self.emit(format!("store {aggty} {v}, ptr {buf}"));
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"
        ));
        self.emit(format!(
            "{b} = insertvalue {{ ptr, i64, i64 }} {a}, i64 {n}, 1"
        ));
        self.emit(format!(
            "{c} = insertvalue {{ ptr, i64, i64 }} {b}, i64 {n}, 2"
        ));
        Ok((c, Type::Array(Box::new(inner.clone()))))
    }

    /// The LLVM struct type of a `SmallArray<T, N>` (RFC-0056):
    /// `{ i64 len, i64 cap, ptr data, [N x T] inline }`.
    fn sa_ll(&self, inner: &Type, n: usize) -> String {
        format!("{{ i64, i64, ptr, [{n} x {}] }}", self.llt(inner))
    }

    /// Copy a fixed `[len x T]` aggregate value `v` into a fresh `SmallArray<T,
    /// N>` value (RFC-0056), inline state: `len` real elements in the inline
    /// buffer, `cap == N`, `data` null. The lowering behind a contextual array
    /// literal `[..]` in a `SmallArray` position (the checker proved `len <= N`).
    fn array_n_to_smallarray(
        &mut self,
        v: &str,
        inner: &Type,
        len: usize,
        n: usize,
    ) -> Result<(String, Type), String> {
        let ell = self.llt(inner);
        let src = format!("[{len} x {ell}]");
        let inl_ty = format!("[{n} x {ell}]");
        let sa_ll = self.sa_ll(inner, n);
        // Build the inline `[N x T]` by lifting each of the `len` elements out of
        // the source aggregate (constant indices) into it; slots `len..N` stay
        // `undef` (dead — `len` bounds every read).
        let mut inl = "undef".to_string();
        for i in 0..len {
            let e = self.fresh_tmp();
            self.emit(format!("{e} = extractvalue {src} {v}, {i}"));
            let next = self.fresh_tmp();
            self.emit(format!(
                "{next} = insertvalue {inl_ty} {inl}, {ell} {e}, {i}"
            ));
            inl = next;
        }
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        let d = self.fresh_tmp();
        self.emit(format!("{a} = insertvalue {sa_ll} undef, i64 {len}, 0"));
        self.emit(format!("{b} = insertvalue {sa_ll} {a}, i64 {n}, 1"));
        self.emit(format!("{c} = insertvalue {sa_ll} {b}, ptr null, 2"));
        self.emit(format!("{d} = insertvalue {sa_ll} {c}, {inl_ty} {inl}, 3"));
        Ok((d, Type::SmallArray(Box::new(inner.clone()), n)))
    }

    /// Given a `SmallArray<T, N>` SSA value `av`, spill it to a temp slot and
    /// return `(base_ptr, len)` where `base_ptr` points at element 0 in whichever
    /// buffer is live (inline while `cap == N`, else the heap `data`). RFC-0056:
    /// every element access branches on the state to pick this base.
    fn sa_value_base_len(&mut self, av: &str, inner: &Type, n: usize) -> (String, String) {
        let sa_ll = self.sa_ll(inner, n);
        let slot = self.fresh_alloca(&sa_ll);
        self.emit(format!("store {sa_ll} {av}, ptr {slot}"));
        let (base, len, _cap, _data) = self.sa_slot_base(&slot, inner, n);
        (base, len)
    }

    /// Given a `SmallArray<T, N>` binding slot, load its header and return
    /// `(base_ptr, len, cap, data)`. `base_ptr` is the inline field pointer while
    /// `cap == N`, else the heap `data` pointer (RFC-0056).
    fn sa_slot_base(
        &mut self,
        slot: &str,
        inner: &Type,
        n: usize,
    ) -> (String, String, String, String) {
        let sa_ll = self.sa_ll(inner, n);
        let hdr = self.fresh_tmp();
        let len = self.fresh_tmp();
        let cap = self.fresh_tmp();
        let data = self.fresh_tmp();
        self.emit(format!("{hdr} = load {sa_ll}, ptr {slot}"));
        self.emit(format!("{len} = extractvalue {sa_ll} {hdr}, 0"));
        self.emit(format!("{cap} = extractvalue {sa_ll} {hdr}, 1"));
        self.emit(format!("{data} = extractvalue {sa_ll} {hdr}, 2"));
        let inl = self.fresh_tmp();
        self.emit(format!(
            "{inl} = getelementptr {sa_ll}, ptr {slot}, i64 0, i32 3, i64 0"
        ));
        let is_inline = self.fresh_tmp();
        self.emit(format!("{is_inline} = icmp eq i64 {cap}, {n}"));
        let base = self.fresh_tmp();
        self.emit(format!(
            "{base} = select i1 {is_inline}, ptr {inl}, ptr {data}"
        ));
        (base, len, cap, data)
    }

    /// The storage a fixed-array receiver already has, if it has any.
    ///
    /// A `[N x T]` is an LLVM value aggregate, and `getelementptr` cannot index
    /// one by a dynamic index — the array has to be in memory first. A fresh
    /// slot puts it there and copies all N elements to do it, on EVERY read: at
    /// N = 16 that is 128 bytes per element read, and it measured 20x an
    /// `Array<Int64>` read. A binding is already in memory, so its own slot
    /// serves the same `getelementptr` for nothing.
    ///
    /// Only a receiver with an address answers. A call result or a literal has
    /// none, and the caller spills it — that copy is the value form's real cost
    /// and is paid once, not per read.
    fn fixed_place(&mut self, recv: &Expr, aggty: &str) -> Option<String> {
        // The address is only usable if it holds THIS array. Ask before
        // emitting anything, so a receiver that does not match costs no IR.
        let ty = self.static_ty(recv)?;
        if self.llt(&ty) != aggty {
            return None;
        }
        Some(self.place_of(recv)?.0)
    }

    /// The address of a place expression, and its static type. A binding
    /// answers with its slot (a module-state global is already a pointer, so it
    /// answers with itself); a record field answers with a `getelementptr` into
    /// its owner's place. Everything else has no address.
    fn place_of(&mut self, e: &Expr) -> Option<(String, Type)> {
        match e {
            Expr::Var { name, .. } => self.lookup(name),
            Expr::Field { expr, field, .. } => {
                let (base, bty) = self.place_of(expr)?;
                let fields = self.record_fields(&bty)?;
                let idx = fields.iter().position(|f| &f.name == field)?;
                let fty = fields[idx].ty.clone();
                // A `lazy` field is a stored closure that reading CALLS
                // (RFC-0085 M4a). Its address is not its value.
                if vyrn_frontend::types::deferred(&fty).is_some() {
                    return None;
                }
                let bll = self.llt(&bty);
                let p = self.fresh_tmp();
                self.emit(format!(
                    "{p} = getelementptr {bll}, ptr {base}, i64 0, i32 {idx}"
                ));
                Some((p, fty))
            }
            _ => None,
        }
    }

    // ---- RFC-0089 M1b: `x.copy()` -----------------------------------------

    /// Whether a value of `ty` transitively owns heap — the one predicate
    /// [`deep_copy`](Self::deep_copy) copies by, asked of the frontend so the
    /// checker, this backend and the direct one cannot disagree about it.
    fn owns_heap(&self, ty: &Type) -> bool {
        vyrn_frontend::own::owns_heap(
            &vyrn_frontend::types::substitute(ty, self.subst),
            self.types,
        )
    }

    /// The size in bytes of one `ll` value, as LLVM's null-GEP idiom.
    fn size_of_ll(&mut self, ll: &str) -> String {
        let sz = self.fresh_tmp();
        self.emit(format!(
            "{sz} = ptrtoint ptr getelementptr ({ll}, ptr null, i64 1) to i64"
        ));
        sz
    }

    /// Copy `count` elements of `ll` out of `src` into a fresh buffer of `room`
    /// elements, and return the buffer. One byte of slack, so a copy of an empty
    /// container never asks the allocator for nothing.
    ///
    /// Always plain `malloc`, never the region arena — the rule
    /// [`Gen::array_n_to_heap`] states, for the same reason: an `Array`, a `Map`
    /// and a spilled `SmallArray` grow their buffer with `realloc` and hand it
    /// back with `free`, both undefined on an arena interior pointer. This used
    /// to route through [`Gen::heap_alloc`], so a copy made inside a `region`
    /// drew from the arena and was then freed twice — once by the block-exit
    /// walk, which suppresses only a `String`, and once by the arena's own exit
    /// walk (`rfcs/census-regions.md` defect 1).
    fn copy_buf(&mut self, src: &str, count: &str, room: &str, ll: &str) -> String {
        let esz = self.size_of_ll(ll);
        let want = self.fresh_tmp();
        let tot = self.fresh_tmp();
        let buf = self.fresh_tmp();
        self.emit(format!("{want} = mul i64 {room}, {esz}"));
        self.emit(format!("{tot} = add i64 {want}, 1"));
        self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {tot})"));
        let live = self.fresh_tmp();
        self.emit(format!("{live} = mul i64 {count}, {esz}"));
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {src}, i64 {live}, i1 false)"
        ));
        buf
    }

    /// Release each of the first `count` elements of `buf` — the mirror of
    /// [`copy_elems`](Self::copy_elems), and RFC-0092 M2's half of census U4.
    ///
    /// The gate is the element's own release ROW, not whether it reaches heap. A
    /// record reaches two Strings and has no row until M3, and walking into one
    /// here would free fields no rule says the array owns. A row is the proof;
    /// `owns_heap` is only a reachability question.
    fn release_elems(&mut self, buf: &str, count: &str, elem: &Type) -> Result<(), String> {
        if self.rel_kind(elem).is_none() {
            return Ok(());
        }
        let ell = self.llt(elem);
        let idx = self.fresh_alloca("i64");
        self.emit(format!("store i64 0, ptr {idx}"));
        let cond_l = self.fresh_label("rel.el.cond");
        let body_l = self.fresh_label("rel.el.body");
        let end_l = self.fresh_label("rel.el.end");
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let i = self.fresh_tmp();
        let done = self.fresh_tmp();
        self.emit(format!("{i} = load i64, ptr {idx}"));
        self.emit(format!("{done} = icmp uge i64 {i}, {count}"));
        self.emit_term(format!("br i1 {done}, label %{end_l}, label %{body_l}"));
        self.emit_label(&body_l);
        let bi = self.fresh_tmp();
        let ep = self.fresh_tmp();
        let ev = self.fresh_tmp();
        self.emit(format!("{bi} = load i64, ptr {idx}"));
        self.emit(format!("{ep} = getelementptr {ell}, ptr {buf}, i64 {bi}"));
        self.emit(format!("{ev} = load {ell}, ptr {ep}"));
        self.deep_release(&ev, elem)?;
        let i2 = self.fresh_tmp();
        let inext = self.fresh_tmp();
        self.emit(format!("{i2} = load i64, ptr {idx}"));
        self.emit(format!("{inext} = add i64 {i2}, 1"));
        self.emit(format!("store i64 {inext}, ptr {idx}"));
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&end_l);
        Ok(())
    }

    /// Replace each of the first `count` elements of `buf` with a deep copy of
    /// itself. A no-op — and no emitted loop — when the element owns no heap.
    fn copy_elems(&mut self, buf: &str, count: &str, elem: &Type) -> Result<(), String> {
        if !self.owns_heap(elem) {
            return Ok(());
        }
        let ell = self.llt(elem);
        let idx = self.fresh_alloca("i64");
        self.emit(format!("store i64 0, ptr {idx}"));
        let cond_l = self.fresh_label("cp.cond");
        let body_l = self.fresh_label("cp.body");
        let end_l = self.fresh_label("cp.end");
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let i = self.fresh_tmp();
        let done = self.fresh_tmp();
        self.emit(format!("{i} = load i64, ptr {idx}"));
        self.emit(format!("{done} = icmp uge i64 {i}, {count}"));
        self.emit_term(format!("br i1 {done}, label %{end_l}, label %{body_l}"));
        self.emit_label(&body_l);
        let bi = self.fresh_tmp();
        let ep = self.fresh_tmp();
        let ev = self.fresh_tmp();
        self.emit(format!("{bi} = load i64, ptr {idx}"));
        self.emit(format!("{ep} = getelementptr {ell}, ptr {buf}, i64 {bi}"));
        self.emit(format!("{ev} = load {ell}, ptr {ep}"));
        let cv = self.deep_copy(&ev, elem)?;
        self.emit(format!("store {ell} {cv}, ptr {ep}"));
        let i2 = self.fresh_tmp();
        let inext = self.fresh_tmp();
        self.emit(format!("{i2} = load i64, ptr {idx}"));
        self.emit(format!("{inext} = add i64 {i2}, 1"));
        self.emit(format!("store i64 {inext}, ptr {idx}"));
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&end_l);
        Ok(())
    }

    /// `x.copy()` (RFC-0089 M1b): a value of `ty` that shares no heap with `v`.
    ///
    /// Structural and recursive. A type that owns nothing IS its own copy, which
    /// is what makes `copy` one word with one meaning in a monomorphized
    /// generic: the same call site is a fresh buffer for `String` and the value
    /// itself for `Int64`. A `Task<T>` falls in the second group on purpose — it
    /// is a handle, and copying a handle names the same thing.
    fn deep_copy(&mut self, v: &str, ty: &Type) -> Result<String, String> {
        if !self.owns_heap(ty) {
            return Ok(v.to_string());
        }
        match self.resolve(ty) {
            Type::Str => {
                let len = self.str_len(v);
                let buf = self.str_alloc(&len, &len);
                self.emit(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {v}, i64 {len}, i1 false)"
                ));
                Ok(buf)
            }
            Type::Array(inner) => {
                let ell = self.llt(&inner);
                let data = self.fresh_tmp();
                let len = self.fresh_tmp();
                self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {v}, 0"));
                self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {v}, 1"));
                let buf = self.copy_buf(&data, &len, &len, &ell);
                self.copy_elems(&buf, &len, &inner)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                let c = self.fresh_tmp();
                self.emit(format!(
                    "{a} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"
                ));
                self.emit(format!(
                    "{b} = insertvalue {{ ptr, i64, i64 }} {a}, i64 {len}, 1"
                ));
                self.emit(format!(
                    "{c} = insertvalue {{ ptr, i64, i64 }} {b}, i64 {len}, 2"
                ));
                Ok(c)
            }
            // A `SmallArray<T, N>` copies its header by value; the buffer behind
            // it is only there once it has spilled, and a copy that stays inline
            // allocates nothing at all.
            Type::SmallArray(inner, n) => {
                let sa_ll = self.sa_ll(&inner, n);
                let ell = self.llt(&inner);
                let slot = self.fresh_alloca(&sa_ll);
                self.emit(format!("store {sa_ll} {v}, ptr {slot}"));
                let (_, len, cap, data) = self.sa_slot_base(&slot, &inner, n);
                let spilled = self.fresh_tmp();
                let spill_l = self.fresh_label("cp.sa.spill");
                let join_l = self.fresh_label("cp.sa.join");
                self.emit(format!("{spilled} = icmp ne i64 {cap}, {n}"));
                self.emit_term(format!(
                    "br i1 {spilled}, label %{spill_l}, label %{join_l}"
                ));
                self.emit_label(&spill_l);
                let buf = self.copy_buf(&data, &len, &cap, &ell);
                let dp = self.fresh_tmp();
                self.emit(format!(
                    "{dp} = getelementptr {sa_ll}, ptr {slot}, i64 0, i32 2"
                ));
                self.emit(format!("store ptr {buf}, ptr {dp}"));
                self.emit_term(format!("br label %{join_l}"));
                self.emit_label(&join_l);
                let (base, len2, _, _) = self.sa_slot_base(&slot, &inner, n);
                self.copy_elems(&base, &len2, &inner)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {sa_ll}, ptr {slot}"));
                Ok(out)
            }
            // A stored function value copies by its runtime twin: the same
            // tag, the capture block duplicated — and DEEP since RFC-0114
            // §25's round three: a block owns its heap captures (capture is
            // a take, `Gone::Captured` stops the binding's own release), so
            // a shallow copy left two owners of every captured buffer.
            Type::Fn(..) => {
                let tag = self.fresh_tmp();
                let pay = self.fresh_tmp();
                let pay2 = self.fresh_tmp();
                self.emit(format!("{tag} = extractvalue {{ i64, i64 }} {v}, 0"));
                self.emit(format!("{pay} = extractvalue {{ i64, i64 }} {v}, 1"));
                self.emit(format!(
                    "{pay2} = call i64 @{FNVAL_COPY}(i64 {tag}, i64 {pay})"
                ));
                Ok(self.fnval_aggregate_v(&tag, &pay2))
            }
            // Two parallel buffers. String keys are duplicated per entry; Int64
            // keys (RFC-0117) copy with the buffer, since the buffer holds the
            // values themselves. Values duplicate only when the value type owns
            // something.
            Type::Map(kt, vt) => {
                let ik = self.key_is_int(&kt);
                // A packed user key (RFC-0117 M2) copies with its buffer,
                // exactly as an Int64 key does: the buffer holds the values.
                let kll = if ik {
                    "i64".to_string()
                } else if self.key_is_pack(&kt) {
                    self.llt(&kt)
                } else {
                    "ptr".to_string()
                };
                let heap_keys = !ik && !self.key_is_pack(&kt);
                let vll = self.llt(&vt);
                let keys = self.fresh_tmp();
                let vals = self.fresh_tmp();
                let len = self.fresh_tmp();
                let cap = self.fresh_tmp();
                let m = "{ ptr, ptr, i64, i64, ptr }";
                self.emit(format!("{keys} = extractvalue {m} {v}, 0"));
                self.emit(format!("{vals} = extractvalue {m} {v}, 1"));
                self.emit(format!("{len} = extractvalue {m} {v}, 2"));
                self.emit(format!("{cap} = extractvalue {m} {v}, 3"));
                let kb = self.copy_buf(&keys, &len, &cap, &kll);
                if heap_keys {
                    self.copy_elems(&kb, &len, &Type::Str)?;
                }
                let vb = self.copy_buf(&vals, &len, &cap, &vll);
                self.copy_elems(&vb, &len, &vt)?;
                // The index is copied rather than rebuilt: it holds POSITIONS,
                // and a copy keeps the capacity as well as the order, so every
                // bucket still names the entry it named.
                let ix = self.fresh_tmp();
                let nb = self.fresh_tmp();
                self.emit(format!("{ix} = extractvalue {m} {v}, 4"));
                self.emit(format!("{nb} = mul i64 {cap}, 2"));
                let ib = self.copy_buf(&ix, &nb, &nb, "i64");
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                let c = self.fresh_tmp();
                let d = self.fresh_tmp();
                self.emit(format!("{a} = insertvalue {m} undef, ptr {kb}, 0"));
                self.emit(format!("{b} = insertvalue {m} {a}, ptr {vb}, 1"));
                self.emit(format!("{c} = insertvalue {m} {b}, i64 {len}, 2"));
                self.emit(format!("{d} = insertvalue {m} {c}, i64 {cap}, 3"));
                let e = self.fresh_tmp();
                self.emit(format!("{e} = insertvalue {m} {d}, ptr {ib}, 4"));
                Ok(e)
            }
            Type::Record(_) => {
                let fields = self
                    .record_fields(ty)
                    .ok_or_else(|| format!("`copy` of a record with no fields: {ty:?}"))?;
                let rll = self.llt(ty);
                let mut cur = v.to_string();
                for (i, f) in fields.iter().enumerate() {
                    if !self.owns_heap(&f.ty) {
                        continue;
                    }
                    let fll = self.llt(&f.ty);
                    let fv = self.fresh_tmp();
                    self.emit(format!("{fv} = extractvalue {rll} {cur}, {i}"));
                    let cv = self.deep_copy(&fv, &f.ty)?;
                    let next = self.fresh_tmp();
                    self.emit(format!("{next} = insertvalue {rll} {cur}, {fll} {cv}, {i}"));
                    cur = next;
                }
                Ok(cur)
            }
            // A fixed `[N x T]` is a value, so the copy is unrolled — `N` is a
            // constant and there is no buffer to allocate.
            Type::ArrayN(inner, n) => {
                let all = self.llt(ty);
                let ell = self.llt(&inner);
                let mut cur = v.to_string();
                for i in 0..n {
                    let ev = self.fresh_tmp();
                    self.emit(format!("{ev} = extractvalue {all} {cur}, {i}"));
                    let cv = self.deep_copy(&ev, &inner)?;
                    let next = self.fresh_tmp();
                    self.emit(format!("{next} = insertvalue {all} {cur}, {ell} {cv}, {i}"));
                    cur = next;
                }
                Ok(cur)
            }
            Type::Option(inner) => self.copy_sum(v, &[(Some("1"), vec![*inner])]),
            Type::Result(ok, err) => {
                self.copy_sum(v, &[(Some("1"), vec![*ok]), (Some("0"), vec![*err])])
            }
            Type::Enum(vs) => self.copy_enum(v, &vs),
            // A handle names something; copying it names the same thing. A
            // `Task<T>`/`lazy T` is a promise, which is the same shape.
            Type::Task(_) | Type::Lazy(_) => Ok(v.to_string()),
            other => Err(format!(
                "`copy` of {other:?} is not lowered — the checker should have refused it"
            )),
        }
    }

    /// Release the heap a value of `ty` holds — the mirror of
    /// [`deep_copy`](Self::deep_copy), with `free` where that has `malloc`
    /// (RFC-0089 rule 4, Phase 5).
    ///
    /// One walk, both directions: `copy` decided what a value's own storage IS,
    /// and releasing that value gives exactly that storage back. Writing the two
    /// as one shape is what keeps them from disagreeing about a boxed enum
    /// payload — the encoding Phase 3 measured and the one a hand-written release
    /// gets wrong.
    ///
    /// It releases an `Array<T>`'s ELEMENTS since RFC-0092 M2 — census U4 — and
    /// releases each the way that element's own type is released, so an element
    /// with no row of its own is left alone. The two builtins that used to hand
    /// back a buffer of somebody else's element words, `m.keys()` and
    /// `sa.toArray()`, copy them now. A `Map` and a `SmallArray` still give back
    /// their buffers alone; their element rows are M3.
    /// Call the `release` a type declared (RFC-0086 M1), on a value the release
    /// walk is holding.
    ///
    /// [`Gen::emit_drop`]'s `Release` arm reached from inside the walk rather
    /// than at the top of a drop. An element and a field are values, not slots,
    /// so a generic declared release — which solves its type arguments from the
    /// receiver, exactly as a written call does — gets one to read.
    fn call_release(&mut self, v: &str, ty: &Type) -> Result<(), String> {
        let Some(DropKind::Release(f, _)) = self.rel_kind(ty) else {
            return Ok(());
        };
        if self
            .funcs
            .get(f.as_str())
            .is_some_and(|c| !c.type_params.is_empty())
        {
            let ty = vyrn_frontend::types::substitute(ty, self.subst);
            let ll = self.llt(&ty);
            let slot = self.fresh_alloca(&ll);
            self.emit(format!("store {ll} {v}, ptr {slot}"));
            self.scope
                .push(vec![(REL_RECV.to_string(), slot, ty.clone())]);
            let recv = [Expr::Var {
                name: REL_RECV.to_string(),
                line: 0,
            }];
            let r = self.gen_call(&f, &recv);
            self.scope.pop();
            return r.map(|_| ());
        }
        let pty = self
            .param_types
            .get(&f)
            .and_then(|p| p.first())
            .cloned()
            .unwrap_or(Type::Unit);
        let ll = self.llt(&pty);
        self.emit(format!("call void @{}({ll} {v})", fn_sym(&f)));
        Ok(())
    }

    fn deep_release(&mut self, v: &str, ty: &Type) -> Result<(), String> {
        // RFC-0093 M2: the holes belong to the place this call is looking at,
        // and only the record arm below can be told about them. Taking them here
        // is what makes every other arm — an element, a payload, a buffer —
        // start empty, which is right: `own` refuses a hole under any of them.
        let holes = std::mem::take(&mut self.rel_holes);
        // A type that declares its own release keeps it, so the walk CALLS that
        // release rather than reaching past the declaration into its fields —
        // which would reclaim what the declaration says it reclaims, in a
        // different order, and without the print a user `release` may do.
        //
        // It used to return here and call nothing, which was right at the top
        // of a drop (`emit_drop` takes its own `Release` arm) and wrong for
        // every place under one: an element of an `Array<Txn>` and a field of a
        // record both reached this line and were skipped, so `impl Owned for
        // Txn` never ran for a `Txn` inside anything. RFC-0092 M4 is where that
        // is observable — a container carries its element's obligation now, so
        // the compiler demands a discharge the discharge did not perform.
        if matches!(self.rel_kind(ty), Some(DropKind::Release(..))) {
            self.call_release(v, ty)?;
            return self.free_declared_boxes(v, ty);
        }
        // Round twenty-nine: heap-free is not box-free — a type whose only
        // release row is a boxed sum payload still walks.
        if !self.owns_heap(ty) && self.owned.release_kind(ty).is_none() {
            return Ok(());
        }
        match self.resolve(ty) {
            // A `String` buffer allocated inside a `region` belongs to the arena
            // — [`Gen::str_alloc`] routes it there, and `__vyrn_region_exit`
            // hands it back. `own` states the same exception one binding at a
            // time (`Fate::Leaked(Leak::Region)` for `DropKind::FreeStr`), and it
            // can only see the binding's OWN type: the `String` under an
            // `Array<String>`, under a record field, under a `Map` key was
            // allocated by the arena and freed by this walk as well, which is a
            // double free (`rfcs/census-regions.md` defect 1). The key here is
            // the key `heap_alloc` allocates by, so the two sides now partition
            // the same way at every depth.
            Type::Str if self.region_depth == 0 => {
                self.emit(format!("call void @__vyrn_str_free(ptr {v})"));
                Ok(())
            }
            Type::Str => Ok(()),
            // The elements first, then the buffer they live in — the reverse of
            // the order `deep_copy` builds them, and the only order in which the
            // walk may still read the buffer it is about to free.
            //
            // Whether the elements go at all is `own`'s answer, asked rather
            // than re-derived — it carries the stop for a self-referring element
            // type, whose walk has no bottom.
            Type::Array(inner) if matches!(self.rel_kind(ty), Some(DropKind::Deep(_))) => {
                let data = self.fresh_tmp();
                let len = self.fresh_tmp();
                self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {v}, 0"));
                self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {v}, 1"));
                self.release_elems(&data, &len, &inner)?;
                self.emit(format!("call void @__vyrn_free(ptr {data})"));
                Ok(())
            }
            // A `SmallArray<T, N>`'s live slots are its inline block while it
            // fits and its spilled buffer once it does not — the branch
            // `deep_copy` already takes, run backwards (RFC-0092 M3). `data` is
            // null while inline, which `free` refuses.
            Type::SmallArray(inner, n) if matches!(self.rel_kind(ty), Some(DropKind::Deep(_))) => {
                let sa_ll = self.sa_ll(&inner, n);
                let slot = self.fresh_alloca(&sa_ll);
                self.emit(format!("store {sa_ll} {v}, ptr {slot}"));
                let (base, len, _, data) = self.sa_slot_base(&slot, &inner, n);
                self.release_elems(&base, &len, &inner)?;
                self.emit(format!("call void @__vyrn_free(ptr {data})"));
                Ok(())
            }
            // Two parallel buffers. String keys are released per entry; Int64
            // keys (RFC-0117) go with their buffer. The elements first, then
            // the buffers they live in.
            Type::Map(kt, vt) if matches!(self.rel_kind(ty), Some(DropKind::Deep(_))) => {
                // An Int64 key and a packed user key (RFC-0117 M2) both go
                // with their buffer: neither owns heap of its own.
                let heap_keys = !self.key_is_int(&kt) && !self.key_is_pack(&kt);
                let m = "{ ptr, ptr, i64, i64, ptr }";
                let keys = self.fresh_tmp();
                let vals = self.fresh_tmp();
                let len = self.fresh_tmp();
                self.emit(format!("{keys} = extractvalue {m} {v}, 0"));
                self.emit(format!("{vals} = extractvalue {m} {v}, 1"));
                self.emit(format!("{len} = extractvalue {m} {v}, 2"));
                let ix = self.fresh_tmp();
                self.emit(format!("{ix} = extractvalue {m} {v}, 4"));
                if heap_keys {
                    self.release_elems(&keys, &len, &Type::Str)?;
                }
                self.release_elems(&vals, &len, &vt)?;
                self.emit(format!("call void @__vyrn_free(ptr {keys})"));
                self.emit(format!("call void @__vyrn_free(ptr {vals})"));
                self.emit(format!("call void @__vyrn_free(ptr {ix})"));
                Ok(())
            }
            Type::Array(_) | Type::SmallArray(..) | Type::Map(..) => {
                let snap = self.snap_val(v, ty);
                self.free_snap(&snap);
                Ok(())
            }
            Type::Record(_) => {
                let fields = self
                    .record_fields(ty)
                    .ok_or_else(|| format!("a release of a record with no fields: {ty:?}"))?;
                let rll = self.llt(ty);
                for (i, f) in fields.iter().enumerate() {
                    // Kind-driven since round twenty-nine: a heap-free field
                    // whose type still carries a release row is a
                    // boxed-payload sum the walk must reach.
                    if !self.owns_heap(&f.ty) && self.owned.release_kind(&f.ty).is_none() {
                        continue;
                    }
                    // RFC-0093 M2. A `consume` took this field, so it has an
                    // owner already and this walk is not it.
                    if holes.iter().any(|h| *h == f.name) {
                        continue;
                    }
                    let fv = self.fresh_tmp();
                    self.emit(format!("{fv} = extractvalue {rll} {v}, {i}"));
                    self.rel_holes = vyrn_frontend::own::holes_under(&holes, &f.name);
                    self.deep_release(&fv, &f.ty)?;
                }
                Ok(())
            }
            // A fixed `[N x T]` is a value, so the release is unrolled — `N` is a
            // constant and there is no buffer to free, only the slots
            // (RFC-0092 M3). The mirror of `deep_copy`'s own unrolled loop.
            Type::ArrayN(inner, n) => {
                let all = self.llt(ty);
                for i in 0..n {
                    let ev = self.fresh_tmp();
                    self.emit(format!("{ev} = extractvalue {all} {v}, {i}"));
                    self.deep_release(&ev, &inner)?;
                }
                Ok(())
            }
            Type::Option(inner) => self.release_sum(v, &[(Some("1"), *inner)]),
            Type::Result(ok, err) => self.release_sum(v, &[(Some("1"), *ok), (Some("0"), *err)]),
            Type::Enum(vs) => self.release_enum(v, &vs, true),
            // A stored function value is `{ i64 tag, i64 captures }` (RFC-0037).
            // The captures are one heap block, read by value at the construction
            // site, and 0 when there are none — which `free` refuses. Census §16.
            Type::Fn(..) => {
                // The runtime twin walks the block's heap captures before the
                // block goes (RFC-0114 §25 round three): capture is a take,
                // so the block is the OWNER of what it snapshot, and the old
                // shallow free left every heap capture — a String, a nested
                // fn value's own block — with no owner at all.
                let tag = self.fresh_tmp();
                let pay = self.fresh_tmp();
                self.emit(format!("{tag} = extractvalue {{ i64, i64 }} {v}, 0"));
                self.emit(format!("{pay} = extractvalue {{ i64, i64 }} {v}, 1"));
                self.emit(format!("call void @{FNVAL_RELEASE}(i64 {tag}, i64 {pay})"));
                Ok(())
            }
            // A fixed `[N x T]` is a container, so its elements are U4's
            // question. A handle names something somebody else reclaims.
            _ => Ok(()),
        }
    }

    /// Whether an `Option`/`Result` payload of type `ty` is a pointer to a block
    /// rather than the word itself — the question [`decode_payload`] answers by
    /// construction and a release has to ask out loud. Phase 3 measured that the
    /// two encodings coexist: a `String` rides in the word, and a record does not.
    fn payload_boxed(&mut self, ty: &Type) -> bool {
        if matches!(
            self.resolve(ty),
            Type::Int
                | Type::Bool
                | Type::Str
                | Type::Fn(..)
                | Type::Unit
                | Type::Never
                | Type::Param(_)
        ) {
            return false;
        }
        self.llt(ty) != "i64"
    }

    /// The release of an `Option`/`Result`: one payload, selected by the `i1` tag.
    fn release_sum(&mut self, v: &str, arms: &[(Option<&str>, Type)]) -> Result<(), String> {
        let sll = "{ i1, i64, i64 }";
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {sll} {v}, 0"));
        let end_l = self.fresh_label("rel.sum.end");
        for (want, pty) in arms {
            // Round twenty-nine: a payload that owns no heap can still TRAVEL
            // in a box, and the box is the sum's to free.
            if !self.owns_heap(pty) && !self.payload_boxed(pty) {
                continue;
            }
            let hit_l = self.fresh_label("rel.sum.hit");
            let miss_l = self.fresh_label("rel.sum.miss");
            let is = self.fresh_tmp();
            self.emit(format!("{is} = icmp eq i1 {tag}, {}", want.unwrap_or("1")));
            self.emit_term(format!("br i1 {is}, label %{hit_l}, label %{miss_l}"));
            self.emit_label(&hit_l);
            let w0 = self.fresh_tmp();
            let w1 = self.fresh_tmp();
            self.emit(format!("{w0} = extractvalue {sll} {v}, 1"));
            self.emit(format!("{w1} = extractvalue {sll} {v}, 2"));
            let pv = self.decode_payload(&w0, &w1, pty);
            self.deep_release(&pv, pty)?;
            // A payload wider than a word is a pointer to a block the sum owns,
            // exactly as `encode_payload` allocated it.
            if self.payload_boxed(pty) {
                let q = self.fresh_tmp();
                self.emit(format!("{q} = inttoptr i64 {w0} to ptr"));
                self.emit(format!("call void @__vyrn_free(ptr {q})"));
            }
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&miss_l);
        }
        self.emit_term(format!("br label %{end_l}"));
        self.emit_label(&end_l);
        Ok(())
    }

    /// Free the payload BOXES of an enum whose release the type declared, and
    /// nothing else — RFC-0096.
    ///
    /// A declared `release` takes the enum BY VALUE and gives its payloads back
    /// by name. The BLOCK a wide payload travels in is the enum's own
    /// representation: `unbox_payload` loads out of it, no Vyrn surface names it,
    /// and the structural walk is the only thing that ever freed it. So a
    /// declared release leaked one block per boxed payload per value — 48 bytes
    /// a node over a released tree, which is the whole of a leak that looked
    /// steady until it was measured against four times the calls.
    ///
    /// It answers `Ok(())` for everything that is not a user enum, which is
    /// every other declared row: a record, a container and a generic container
    /// carry their storage inline or in a buffer the declaration itself frees.
    fn free_declared_boxes(&mut self, v: &str, ty: &Type) -> Result<(), String> {
        let Type::Enum(vs) = self.resolve(ty) else {
            return Ok(());
        };
        self.release_enum(v, &vs, false)
    }

    /// The release of a user enum: the payload slots of the live variant, and
    /// only the ones whose declared type owns something. A wide payload is BOXED
    /// here where an `Option`'s is not, so the block behind it is freed too —
    /// `unbox_payload` is what says which.
    ///
    /// `payloads` is false for an enum that DECLARED its release: the payload
    /// values are that function's to give back, and only the boxes they
    /// travelled in are left for this walk (RFC-0096).
    fn release_enum(&mut self, v: &str, vs: &[EnumVariant], payloads: bool) -> Result<(), String> {
        let arity = vs.iter().map(|x| x.payload.len()).max().unwrap_or(0);
        let ell = enum_ll(arity);
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ell} {v}, 0"));
        let end_l = self.fresh_label("rel.enum.end");
        for var in vs {
            // A slot is releasable when its VALUE owns heap — or when the
            // slot itself is BOXED, which `unbox_payload`'s criterion decides
            // (any payload that is not an `i64` word travels in a box). The
            // two are different questions: a `Bool` payload owns nothing and
            // is boxed anyway (an i1 is not a word), and gating the walk on
            // ownership alone leaked that box on every release — one 1-byte
            // block per `JBool` in the corpus, exit-residue round seven's
            // smallest specimen.
            if !var
                .payload
                .iter()
                .any(|p| self.owns_heap(p) || self.llt(p) != "i64")
            {
                continue;
            }
            let Some((n, _)) = self.variants.get(&var.name).cloned() else {
                continue;
            };
            let hit_l = self.fresh_label("rel.enum.hit");
            let miss_l = self.fresh_label("rel.enum.miss");
            let is = self.fresh_tmp();
            self.emit(format!("{is} = icmp eq i64 {tag}, {n}"));
            self.emit_term(format!("br i1 {is}, label %{hit_l}, label %{miss_l}"));
            self.emit_label(&hit_l);
            for (j, pty) in var.payload.iter().enumerate() {
                let boxed = self.llt(pty) != "i64";
                if !self.owns_heap(pty) && !boxed {
                    continue;
                }
                let w = self.fresh_tmp();
                self.emit(format!("{w} = extractvalue {ell} {v}, {}", j + 1));
                if payloads && self.owns_heap(pty) {
                    let pv = self.unbox_payload(&w, pty);
                    self.deep_release(&pv, pty)?;
                }
                // A boxed payload's block is the enum's too.
                if boxed {
                    let q = self.fresh_tmp();
                    self.emit(format!("{q} = inttoptr i64 {w} to ptr"));
                    self.emit(format!("call void @__vyrn_free(ptr {q})"));
                }
            }
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&miss_l);
        }
        self.emit_term(format!("br label %{end_l}"));
        self.emit_label(&end_l);
        Ok(())
    }

    /// The copy of an `Option`/`Result`: one payload, selected by the `i1` tag.
    /// `arms` pairs the tag value to test against with the payload types it
    /// carries (one, for both built-in sums).
    fn copy_sum(&mut self, v: &str, arms: &[(Option<&str>, Vec<Type>)]) -> Result<String, String> {
        let sll = "{ i1, i64, i64 }";
        let slot = self.fresh_alloca(sll);
        self.emit(format!("store {sll} {v}, ptr {slot}"));
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {sll} {v}, 0"));
        let end_l = self.fresh_label("cp.sum.end");
        for (want, payload) in arms {
            let pty = &payload[0];
            if !self.owns_heap(pty) {
                continue;
            }
            let hit_l = self.fresh_label("cp.sum.hit");
            let miss_l = self.fresh_label("cp.sum.miss");
            let is = self.fresh_tmp();
            self.emit(format!("{is} = icmp eq i1 {tag}, {}", want.unwrap_or("1")));
            self.emit_term(format!("br i1 {is}, label %{hit_l}, label %{miss_l}"));
            self.emit_label(&hit_l);
            let w0 = self.fresh_tmp();
            let w1 = self.fresh_tmp();
            self.emit(format!("{w0} = extractvalue {sll} {v}, 1"));
            self.emit(format!("{w1} = extractvalue {sll} {v}, 2"));
            let pv = self.decode_payload(&w0, &w1, pty);
            let cv = self.deep_copy(&pv, pty)?;
            let (n0, n1) = self.encode_payload(&cv, pty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {sll} {v}, i64 {n0}, 1"));
            self.emit(format!("{b} = insertvalue {sll} {a}, i64 {n1}, 2"));
            self.emit(format!("store {sll} {b}, ptr {slot}"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&miss_l);
        }
        self.emit_term(format!("br label %{end_l}"));
        self.emit_label(&end_l);
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {sll}, ptr {slot}"));
        Ok(out)
    }

    /// The copy of a user enum: the payload slots of the live variant, and only
    /// the ones whose declared type owns something.
    fn copy_enum(&mut self, v: &str, vs: &[EnumVariant]) -> Result<String, String> {
        let arity = vs.iter().map(|x| x.payload.len()).max().unwrap_or(0);
        let ell = enum_ll(arity);
        let slot = self.fresh_alloca(&ell);
        self.emit(format!("store {ell} {v}, ptr {slot}"));
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ell} {v}, 0"));
        let end_l = self.fresh_label("cp.enum.end");
        for var in vs {
            if !var.payload.iter().any(|p| self.owns_heap(p)) {
                continue;
            }
            let Some((n, _)) = self.variants.get(&var.name).cloned() else {
                continue;
            };
            let hit_l = self.fresh_label("cp.enum.hit");
            let miss_l = self.fresh_label("cp.enum.miss");
            let is = self.fresh_tmp();
            self.emit(format!("{is} = icmp eq i64 {tag}, {n}"));
            self.emit_term(format!("br i1 {is}, label %{hit_l}, label %{miss_l}"));
            self.emit_label(&hit_l);
            let mut cur = v.to_string();
            for (j, pty) in var.payload.iter().enumerate() {
                if !self.owns_heap(pty) {
                    continue;
                }
                let w = self.fresh_tmp();
                self.emit(format!("{w} = extractvalue {ell} {cur}, {}", j + 1));
                let pv = self.unbox_payload(&w, pty);
                let cv = self.deep_copy(&pv, pty)?;
                let nw = self.box_payload(&cv, pty);
                let next = self.fresh_tmp();
                self.emit(format!(
                    "{next} = insertvalue {ell} {cur}, i64 {nw}, {}",
                    j + 1
                ));
                cur = next;
            }
            self.emit(format!("store {ell} {cur}, ptr {slot}"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&miss_l);
        }
        self.emit_term(format!("br label %{end_l}"));
        self.emit_label(&end_l);
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ell}, ptr {slot}"));
        Ok(out)
    }

    /// Lower `SmallArray<T, N>.push(v)` (RFC-0056). Value-threaded: takes the
    /// current SmallArray value `av`, returns the new one. Grows on a push at
    /// `len == cap` — from the inline state it allocates `2N` on the heap and
    /// copies the inline slots out; from the spilled state it reallocs to
    /// `cap*2`. It never un-spills.
    fn gen_smallarray_push(
        &mut self,
        av: &str,
        inner: &Type,
        n: usize,
        val_expr: &Expr,
    ) -> Result<(String, Type), String> {
        let ell = self.llt(inner);
        let sa_ll = self.sa_ll(inner, n);
        // Evaluate + coerce the element (validated element types trap inline).
        self.expect.push(inner.clone());
        let r = self.gen_expr(val_expr);
        self.expect.pop();
        let (v, vty) = r?;
        let (v, _) = self.coerce(v, &vty, inner)?;
        // Spill to a slot so the inline buffer is addressable.
        let slot = self.fresh_alloca(&sa_ll);
        self.emit(format!("store {sa_ll} {av}, ptr {slot}"));
        let hdr = self.fresh_tmp();
        let len = self.fresh_tmp();
        let cap = self.fresh_tmp();
        let data = self.fresh_tmp();
        self.emit(format!("{hdr} = load {sa_ll}, ptr {slot}"));
        self.emit(format!("{len} = extractvalue {sa_ll} {hdr}, 0"));
        self.emit(format!("{cap} = extractvalue {sa_ll} {hdr}, 1"));
        self.emit(format!("{data} = extractvalue {sa_ll} {hdr}, 2"));
        let inl = self.fresh_tmp();
        self.emit(format!(
            "{inl} = getelementptr {sa_ll}, ptr {slot}, i64 0, i32 3, i64 0"
        ));
        let is_inline = self.fresh_tmp();
        self.emit(format!("{is_inline} = icmp eq i64 {cap}, {n}"));
        let esz = self.fresh_tmp();
        self.emit(format!(
            "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
        ));
        let full = self.fresh_tmp();
        self.emit(format!("{full} = icmp eq i64 {len}, {cap}"));
        let grow_l = self.fresh_label("sapush.grow");
        let nogrow_l = self.fresh_label("sapush.nogrow");
        let store_l = self.fresh_label("sapush.store");
        self.emit_term(format!("br i1 {full}, label %{grow_l}, label %{nogrow_l}"));
        // grow: newcap = cap*2 (inline → 2N, spilled → cap*2).
        self.emit_label(&grow_l);
        let newcap = self.fresh_tmp();
        self.emit(format!("{newcap} = mul i64 {cap}, 2"));
        let nb = self.fresh_tmp();
        self.emit(format!("{nb} = mul i64 {newcap}, {esz}"));
        let grow_in_l = self.fresh_label("sapush.grow.inline");
        let grow_sp_l = self.fresh_label("sapush.grow.spill");
        let grow_done_l = self.fresh_label("sapush.grow.done");
        self.emit_term(format!(
            "br i1 {is_inline}, label %{grow_in_l}, label %{grow_sp_l}"
        ));
        // from inline: fresh heap buffer, copy the N inline elements into it.
        self.emit_label(&grow_in_l);
        let ndi = self.fresh_tmp();
        self.emit(format!("{ndi} = call ptr @__vyrn_malloc(i64 {nb})"));
        let cpb = self.fresh_tmp();
        self.emit(format!("{cpb} = mul i64 {len}, {esz}"));
        self.emit(format!(
            "call void @llvm.memcpy.p0.p0.i64(ptr {ndi}, ptr {inl}, i64 {cpb}, i1 false)"
        ));
        self.emit_term(format!("br label %{grow_done_l}"));
        // from spilled: realloc the existing buffer.
        self.emit_label(&grow_sp_l);
        let nds = self.fresh_tmp();
        self.emit(format!(
            "{nds} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"
        ));
        self.emit_term(format!("br label %{grow_done_l}"));
        self.emit_label(&grow_done_l);
        let gnd = self.fresh_tmp();
        self.emit(format!(
            "{gnd} = phi ptr [ {ndi}, %{grow_in_l} ], [ {nds}, %{grow_sp_l} ]"
        ));
        self.emit_term(format!("br label %{store_l}"));
        // nogrow: base is the live buffer; cap/data unchanged.
        self.emit_label(&nogrow_l);
        let ng_base = self.fresh_tmp();
        self.emit(format!(
            "{ng_base} = select i1 {is_inline}, ptr {inl}, ptr {data}"
        ));
        self.emit_term(format!("br label %{store_l}"));
        // store: choose base/cap/data, write the element, bump len, rebuild.
        self.emit_label(&store_l);
        let base = self.fresh_tmp();
        self.emit(format!(
            "{base} = phi ptr [ {gnd}, %{grow_done_l} ], [ {ng_base}, %{nogrow_l} ]"
        ));
        let ncap = self.fresh_tmp();
        self.emit(format!(
            "{ncap} = phi i64 [ {newcap}, %{grow_done_l} ], [ {cap}, %{nogrow_l} ]"
        ));
        let ndata = self.fresh_tmp();
        self.emit(format!(
            "{ndata} = phi ptr [ {gnd}, %{grow_done_l} ], [ {data}, %{nogrow_l} ]"
        ));
        let ep = self.fresh_tmp();
        self.emit(format!("{ep} = getelementptr {ell}, ptr {base}, i64 {len}"));
        self.emit(format!("store {ell} {v}, ptr {ep}"));
        let nl = self.fresh_tmp();
        self.emit(format!("{nl} = add i64 {len}, 1"));
        // Reload (the inline path mutated `slot`) and overwrite the header.
        let cur = self.fresh_tmp();
        self.emit(format!("{cur} = load {sa_ll}, ptr {slot}"));
        let a0 = self.fresh_tmp();
        let a1 = self.fresh_tmp();
        let a2 = self.fresh_tmp();
        self.emit(format!("{a0} = insertvalue {sa_ll} {cur}, i64 {nl}, 0"));
        self.emit(format!("{a1} = insertvalue {sa_ll} {a0}, i64 {ncap}, 1"));
        self.emit(format!("{a2} = insertvalue {sa_ll} {a1}, ptr {ndata}, 2"));
        Ok((a2, Type::SmallArray(Box::new(inner.clone()), n)))
    }

    /// Emit a conditional runtime trap: if `cond` (an i1) is true, print the
    /// message global to **stderr** (matching the interpreter's `error: ...`
    /// channel) and exit(1); otherwise fall through. `prefix` names the labels.
    fn trap_if(&mut self, cond: &str, msg_global: &str, prefix: &str) {
        let trap_l = self.fresh_label(&format!("{prefix}.trap"));
        let ok_l = self.fresh_label(&format!("{prefix}.ok"));
        self.emit_term(format!("br i1 {cond}, label %{trap_l}, label %{ok_l}"));
        self.emit_label(&trap_l);
        self.emit(format!("call void @__vyrn_trap_msg(ptr {msg_global})"));
        self.emit_term("unreachable".into());
        self.emit_label(&ok_l);
    }

    fn fresh_alloca(&mut self, ll: &str) -> String {
        let slot = format!("%spill{}", self.tmp);
        self.tmp += 1;
        self.allocas.push(format!("  {slot} = alloca {ll}"));
        slot
    }

    fn declare(&mut self, name: &str, ty: &Type) -> String {
        let slot = format!("%{}.addr{}", sanitize(name), self.tmp);
        self.tmp += 1;
        let ll = self.llt(ty);
        self.allocas.push(format!("  {slot} = alloca {ll}"));
        self.scope
            .last_mut()
            .unwrap()
            .push((name.to_string(), slot.clone(), ty.clone()));
        slot
    }

    /// The static type of an index receiver, where this emitter can name one
    /// without generating code for it (RFC-0091 M2).
    ///
    /// This backend has no general type-of-expression: it learns a type by
    /// emitting the expression and reading the type back. That is fine for
    /// lowering and useless for *dispatch*, which must choose before it emits.
    /// So this covers the shapes a container receiver actually takes — a
    /// binding, a field of one, an element of one, a call result — and answers
    /// `None` for the rest, which then takes the seeded row exactly as it did
    /// before projections existed.
    /// The `impl Show for T` a value of type `ty` renders through (RFC-0094
    /// M3), or `None` where the language renders it itself.
    /// The key is taken from the SUBSTITUTED type, not the written one: inside a
    /// `<T: Show>` specialization the parameter is still spelled `T` here, and
    /// `T` names no impl. Substituting is what selects the impl per instance,
    /// which is what the checker deferred to this point.
    fn show_dispatch(&self, ty: &Type) -> Option<String> {
        let t = vyrn_frontend::types::substitute(ty, self.subst);
        match vyrn_frontend::types::renders(&self.resolve(&t)) {
            true => None,
            false => vyrn_frontend::types::show_impl(self.impls, &t),
        }
    }

    fn static_ty(&self, e: &Expr) -> Option<Type> {
        match e {
            // The DECLARED type, never its base: an `impl` head names `Window`,
            // and resolving to the record it aliases loses the name the impl is
            // keyed by. Structure is resolved below, where it is destructured.
            Expr::Var { name, .. } => self.lookup(name).map(|(_, t)| t),
            Expr::Field { expr, field, .. } => {
                let base = self.static_ty(expr)?;
                match self.resolve(&base) {
                    Type::Record(fs) => fs.iter().find(|f| &f.name == field).map(|f| f.ty.clone()),
                    _ => None,
                }
            }
            Expr::Call { name, args, .. }
                if (name == vyrn_frontend::project::AT || name == vyrn_frontend::project::ELEM)
                    && args.len() == 2 =>
            {
                let base = self.static_ty(&args[0])?;
                match self.resolve(&base) {
                    Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => Some(*i),
                    // RFC-0123 M3: `a[i]` on a user container is the `at`
                    // projection, and its RAW declared result answers when
                    // that result keys concretely — the chain rule the
                    // checker's `chain_ty` promised every engine keeps.
                    _ => {
                        let f = vyrn_frontend::project::lookup_in(self.impls, &base, "at")?;
                        vyrn_frontend::types::type_key(&f.ret)?;
                        Some(f.ret.clone())
                    }
                }
            }
            Expr::Call { name, args, .. } => self.ret_types.get(name).cloned().or_else(|| {
                // RFC-0123 M3: a named projection call answers the same way
                // `at` does above — its member's raw declared result, when
                // concrete. A generic result has no key without the
                // substitution no probe carries, and stays `None`.
                if args.is_empty() {
                    return None;
                }
                let inner = self.static_ty(&args[0])?;
                let f =
                    vyrn_frontend::project::lookup_in(self.impls, &inner, name).or_else(|| {
                        vyrn_frontend::project::lookup_in(self.impls, &self.resolve(&inner), name)
                    })?;
                vyrn_frontend::types::type_key(&f.ret)?;
                Some(f.ret.clone())
            }),
            _ => None,
        }
    }

    /// The declared type of the binding a drop-stack slot names.
    ///
    /// A slot name is minted once per `declare` and carries a serial, so this
    /// reverse lookup has one answer. It is what a generic declared `release`
    /// solves its type arguments from — the drop stack carries the storage and
    /// the kind, and the concrete type lives with the binding.
    fn slot_ty(&self, slot: &str) -> Option<Type> {
        self.scope
            .iter()
            .rev()
            .flat_map(|f| f.iter().rev())
            .find(|(_, s, _)| s == slot)
            .map(|(_, _, t)| t.clone())
    }

    /// Take `own`'s two answers for the body about to be emitted: WHAT each
    /// binding is, and WHERE and IN WHAT ORDER each exit releases them.
    fn begin_body(&mut self, name: &str) {
        self.droppable = self
            .ownership
            .droppable
            .get(name)
            .cloned()
            .unwrap_or_default();
        self.early = self.ownership.early.get(name).cloned().unwrap_or_default();
        // RFC-0101 M4: the order this body releases in, decided once in
        // `own::place_body` and read here. What used to stand in its place was a
        // stack of scope frames and a boundary index per loop.
        self.placed = match self.ownership.releases.get(name) {
            Some(steps) => vyrn_frontend::own::placed(steps),
            None => HashMap::new(),
        };
        self.drop_slots.clear();
        self.drop_seq = 0;
        self.cursors.clear();
    }

    fn lookup(&self, name: &str) -> Option<(String, Type)> {
        for frame in self.scope.iter().rev() {
            for (n, slot, ty) in frame.iter().rev() {
                if n == name {
                    return Some((slot.clone(), ty.clone()));
                }
            }
        }
        // Fall back to module state (RFC-0013): an LLVM global is itself a
        // pointer, so its symbol works everywhere a slot pointer is used
        // (`load`/`store`/`getelementptr`), giving reads and writes for free.
        self.globals.get(name).cloned()
    }

    fn function(&mut self, f: &Function, sym: &str, out: &mut String) -> Result<(), String> {
        self.fn_ret = f.ret.clone();
        self.cur_fn_name = f.name.clone();
        self.lambda_counter = 0;
        self.begin_body(&f.name);
        // Slot names repeat across functions, and a stale hole would make a walk
        // skip a place this body owns (RFC-0093 M2). A stale skip only ever
        // leaks, but the clear costs one line.
        self.hole_slots.clear();
        self.append_ok = append_candidates(&f.body);
        self.str_append.clear();
        // Module state is an accumulator every body shares, so its flag is seeded
        // here rather than discovered by a `let` this body may not contain.
        for (slot, flag) in &self.gappend {
            self.str_append.insert(slot.clone(), flag.clone());
        }
        self.modify_copyout.clear();
        let ret = self.llt(&f.ret);
        // A `modify` parameter is received by pointer (call-by-value-result).
        let params: Vec<String> = f
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if p.capability == Capability::Modify {
                    format!("ptr %arg{i}")
                } else {
                    format!("{} %arg{i}", self.llt(&p.ty))
                }
            })
            .collect();

        // store each incoming param into a fresh alloca slot
        for (i, p) in f.params.iter().enumerate() {
            let ll = self.llt(&p.ty);
            let slot = self.declare(&p.name, &p.ty);
            if p.capability == Capability::Modify {
                // Copy the pointed-to value in; remember the pointer to copy out.
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load {ll}, ptr %arg{i}"));
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                self.modify_copyout.push((slot, format!("%arg{i}"), ll));
            } else {
                self.emit(format!("store {ll} %arg{i}, ptr {slot}"));
                // RFC-0114: an owned `consume` parameter the body neither
                // moves nor drops is released at exit — `own` gave it a row
                // keyed by the `Param` node, and the placement already put it
                // on the outermost frame.
                if p.capability == Capability::Consume {
                    let key = p as *const Param as usize;
                    if let Some(kind) = self.droppable.get(&key).cloned() {
                        self.register_drop(key, slot.clone(), kind);
                    }
                }
            }
        }

        self.gen_block(&f.body)?;
        // A lowering that reaches an argument node outside [`Gen::gen_call`]
        // would leave its register here, and freeing it in a block that does not
        // dominate the use is worse than not freeing it at all. Nothing in the
        // corpus does — this is the discipline `emit_drop` already states, in
        // one line: a release this cannot place is a leak, never a wrong free.
        self.arg_frees.clear();

        // Ensure the final block is terminated. The checker proves every path
        // returns, so a fall-through tail is dead by construction — but it must
        // still carry a *valid* terminator. `ret <ty> 0` is only legal for
        // integer types (`ret ptr 0` / `ret double 0` are IR syntax errors, and
        // a String-returning fn ending in a returning if/else hits exactly
        // that); `unreachable` is correct for every type.
        if !self.terminated {
            self.emit_modify_copyout();
            if self.llt(&f.ret).as_str() == "void" {
                self.emit_term("ret void".into());
            } else {
                self.emit_term("unreachable".into());
            }
        }

        // `export extern fn` (RFC-0012 M2): the same `define` gains an inline
        // `wasm-export-name` attribute so wasm-ld exports the function under its
        // Vyrn name (not the internal `vyrn_<name>` symbol). The attribute is a
        // GC root, so no `-Wl,--export` flag is needed for the function itself;
        // on native targets LLVM simply ignores the string attribute. Note the
        // String ABI asymmetry vs. an import (M1): an exported fn's `String`
        // parameter is a single `ptr` (the normal lowering) because the JS caller
        // CAN allocate — it grabs `__vyrn_malloc`, copies UTF-8 + a NUL, and
        // passes the pointer. An import can't allocate, so it takes `(ptr, len)`.
        let export_attr = if f.is_export_extern {
            format!(" \"wasm-export-name\"=\"{}\"", f.name)
        } else {
            String::new()
        };
        writeln!(
            out,
            "define {ret} @{sym}({}){} {{",
            params.join(", "),
            export_attr
        )
        .unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        // RFC-0016 addendum (audit A5.3): one frame of the language's call-depth
        // budget, taken here and given back in front of every `ret`. This is the
        // whole instrumentation, and it is at the CALLEE — where the interpreter
        // counts too, so the argument expressions of a call are still at the
        // caller's depth in both. Counting at the call site instead would put
        // `f(g(x))` one level apart between the two engines.
        //
        // `self.body` holds this function's own lines only: a lambda body saves,
        // builds and restores that buffer, and leaves through `lambda_defs`. A
        // lambda has no name to call itself by (RFC-0037), so it cannot recurse
        // without passing through a named function — one of these.
        //
        // ponytail: every function pays, not only the ones that can recurse. The
        // cost is one load, one add and one store on a thread-local global, twice
        // per call, and nothing allocates. If a benchmark ever shows it, the
        // upgrade is to instrument only functions in a call-graph cycle — but
        // the cycle set would then have to be computed identically for the
        // interpreter and both backends, or the three stop counting the same
        // calls, which is the invariant this whole limit exists to hold.
        out.push_str("  call void @__vyrn_call_enter()\n");
        for b in &self.body {
            if b.trim_start().starts_with("ret ") || b.trim_start() == "ret void" {
                out.push_str("  call void @__vyrn_call_exit()\n");
            }
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n");
        Ok(())
    }

    fn gen_block(&mut self, block: &Block) -> Result<(), String> {
        self.scope.push(Vec::new());
        for stmt in &block.stmts {
            if self.terminated {
                break; // remaining statements are unreachable
            }
            self.gen_stmt(stmt)?;
        }
        // Reclaim this block's owned heap temporaries on the fall-through exit.
        // If the block already returned, these are skipped (that path leaks —
        // safe, never a double-free), matching the `region` early-exit rule.
        if !self.terminated {
            self.emit_releases(ExitKind::Block, block as *const Block as usize);
        }
        self.scope.pop();
        Ok(())
    }

    /// Emit the releases the lowering PLACED at one exit — RFC-0101 M4.
    ///
    /// **This function is the whole of the consumption, and what it replaced is
    /// the milestone.** There used to be three: `emit_all_drops` for a `return`,
    /// `emit_drops_above(boundary)` for the drop half of a `break`, and
    /// `emit_loop_exit_cleanup` to pair that with the region exits — each
    /// walking a stack of scope frames from an index this engine derived for
    /// itself, and each asserting the same "innermost frame first, newest
    /// binding first" the direct backend and the interpreter asserted separately
    /// (§1.4). The order is `own::place_body`'s now. This is a lookup and an
    /// encode.
    ///
    /// Nothing is popped, and that is still what makes an early exit safe: the
    /// unwinding `gen_block`s see `terminated` and skip their own fall-through
    /// releases, so nothing is freed twice.
    fn emit_releases(&mut self, exit: ExitKind, at: usize) {
        let steps = self.placed.get(&(exit, at)).cloned().unwrap_or_default();
        // The cursor a `for x in pull()` owns is not a row of `own`'s map
        // (RFC-0075 M2b closes a producer from the loop that made it), so the
        // placement has nothing for it and this engine still holds where it
        // sits. A step registered BEFORE the cursor is a frame outside the loop,
        // so the cursor runs first.
        //
        // Only a FUNCTION exit reaches one. A cursor sits on the loop-variable
        // frame, which no block exit and no construct's own exit is, and which a
        // `break`/`continue` never crosses because the loop it belongs to is the
        // one being left.
        let mut cursors = match exit {
            ExitKind::Return | ExitKind::Try => self.cursors.clone(),
            _ => Vec::new(),
        };
        let mut run: Vec<(String, Option<DropKind>, bool, Option<Vec<String>>)> = Vec::new();
        for (b, holes) in steps {
            let Some(d) = self.drop_slots.get(&b) else {
                continue;
            };
            let (slot, kind, seq, ms) = (d.slot.clone(), d.kind.clone(), d.seq, d.malloc_side);
            while cursors.last().is_some_and(|(_, at)| *at > seq) {
                run.push((cursors.pop().unwrap().0, None, false, None));
            }
            run.push((slot, Some(kind), ms, holes));
        }
        for (slot, _) in cursors.into_iter().rev() {
            run.push((slot, None, false, None));
        }
        for (slot, kind, malloc_side, holes) in run {
            if std::env::var_os("VYRN_STEP_TRACE").is_some() {
                self.emit(format!(
                    "; release-step exit={exit:?} at={at:x} slot={slot} holes={holes:?}"
                ));
            }
            // Round twenty-seven: a malloc-side scrutinee inside a `region` —
            // the walk's region guard (which protects arena storage) stands
            // down for exactly this value, because a callee's allocation was
            // made under the callee's own region-free context.
            let saved = if malloc_side {
                std::mem::replace(&mut self.region_depth, 0)
            } else {
                self.region_depth
            };
            // A row that carries its own hole set walks around exactly
            // those: round fifty-two's pre-take exit walks the WHOLE value
            // (the empty set), and the placer's row walks the rest of what
            // the kernel saw taken at this exit (RFC-0125 M3). The binding's
            // own skip-list is parked around the one emit.
            let saved_holes = holes.map(|h| {
                let prev = self.hole_slots.remove(&slot);
                if !h.is_empty() {
                    self.hole_slots.insert(slot.clone(), h);
                }
                prev
            });
            match kind {
                Some(k) => self.emit_drop(&slot, &k),
                None => self.emit_drop(&slot, &DropKind::CloseStream),
            }
            if let Some(prev) = saved_holes {
                self.hole_slots.remove(&slot);
                if let Some(h) = prev {
                    self.hole_slots.insert(slot.clone(), h);
                }
            }
            self.region_depth = saved;
        }
    }

    /// Say what one owned binding is released WITH. The placement already said
    /// where and in what order.
    fn register_drop(&mut self, key: usize, slot: String, kind: DropKind) {
        let seq = self.drop_seq;
        self.drop_seq += 1;
        let malloc_side = self.plan.malloc_scrutinee(key);
        self.drop_slots.insert(
            key,
            DropSlot {
                slot,
                kind,
                malloc_side,
                seq,
            },
        );
    }

    /// A function exit: the placed releases, then the region stack balanced.
    ///
    /// Every caller is a function exit, and a `return` (or a `?` propagation)
    /// can leave a region the same way it leaves a block.
    /// `__vyrn_region_pop` and not `__vyrn_region_exit`: see [`REGION_RUNTIME`]
    /// for why the returned value forbids the free.
    fn emit_all_drops(&mut self, exit: ExitKind, at: usize) {
        self.emit_all_drops_keeping(exit, at, None)
    }

    /// Round twenty-seven: `keep` is the RETURNED String pointer when a
    /// `return` leaves a region with a `String` in hand — the one value whose
    /// arena block must survive the pop. Every other block of every popped
    /// level is freed (`__vyrn_region_pop_except` compares block bases; a
    /// static or malloc-side `keep` simply matches nothing). A non-String
    /// return keeps today's abandon-all pop: an aggregate can hold several
    /// arena pointers, and freeing around an unknown set is the double free
    /// the partition forbids.
    fn emit_all_drops_keeping(&mut self, exit: ExitKind, at: usize, keep: Option<&str>) {
        self.emit_releases(exit, at);
        for _ in 0..self.region_depth {
            match keep {
                Some(v) => self.emit(format!("call void @__vyrn_region_pop_except(ptr {v})")),
                None => self.emit("call void @__vyrn_region_pop()".into()),
            }
        }
    }

    /// A `break`/`continue` exit: the placed releases, then every region opened
    /// inside the loop body exited (RFC-0060). Emits nothing structural — the
    /// caller adds the branch.
    fn emit_loop_exit_cleanup(&mut self, ctx: &LoopCtx, exit: ExitKind, at: usize) {
        self.emit_releases(exit, at);
        for _ in ctx.region_depth..self.region_depth {
            self.emit("call void @__vyrn_region_exit()".into());
        }
    }

    /// Copy each `modify` parameter's current value back through its incoming
    /// pointer, so mutations are visible to the caller (call-by-value-result).
    /// Emitted before every function exit.
    fn emit_modify_copyout(&mut self) {
        let items = self.modify_copyout.clone();
        for (slot, ptr, ll) in items {
            let c = self.fresh_tmp();
            self.emit(format!("{c} = load {ll}, ptr {slot}"));
            self.emit(format!("store {ll} {c}, ptr {ptr}"));
        }
    }

    /// Reclaim one owned binding: `free` a string buffer, or `release` a cell
    /// (extracting its slot/generation from the reference aggregate).
    fn emit_drop(&mut self, slot: &str, kind: &DropKind) {
        match kind {
            // Inside a `region` the arena owns every `String` this function
            // allocated ([`Gen::heap_alloc`]), so the drop stands aside exactly as
            // [`Gen::deep_release`] and [`Gen::slot_owns`] do. `own` states the
            // rule for the AUTOMATIC row — `Fate::Leaked(Leak::Region)`, so no
            // row reaches here — and says nothing about `drop s`, which mints its
            // own `Fate::Dropped`. `region { let s = a + b  drop s }` therefore
            // freed the block here and again at the closing brace, and the native
            // heap corrupted.
            DropKind::FreeStr if self.region_depth == 0 => {
                // The header says whether there is anything to hand back:
                // `cap == STR_STATIC` is a data-segment literal (RFC-0089 M1a), and
                // `@__vyrn_str_free` returns on it. The block base is the
                // pointer less the header.
                let p = self.fresh_tmp();
                self.emit(format!("{p} = load ptr, ptr {slot}"));
                self.emit(format!("call void @__vyrn_str_free(ptr {p})"));
            }
            DropKind::FreeStr => {}
            DropKind::CloseStream => {
                // RFC-0075 M2b: one call, and the variant is settled inside it.
                // `emit_drop` runs mid-block, immediately before an early `ret`,
                // and M1's pin says the release is IN the block that `ret`
                // terminates — so a branch here would split that block and make
                // the pin's own claim untestable. Since RFC-0090 M3 the callee is
                // one function per element type rather than one for the whole
                // program, because releasing a producer means calling its step
                // and a step is dispatched by element type. The site count did
                // not move; only how many functions the sites name.
                // The loop's header slot is a `fresh_alloca`, not a declared
                // binding, so `slot_ty` cannot answer for it — the element type
                // is recorded where the slot is made.
                let elem = match self.stream_slots.get(slot).cloned().or_else(|| {
                    match self.slot_ty(slot).map(|t| self.resolve(&t)) {
                        Some(Type::Stream(i)) => Some(*i),
                        _ => None,
                    }
                }) {
                    Some(e) => e,
                    // A drop this cannot name is a leak, never a wrong free —
                    // the reason `Deep` and `Release` swallow theirs.
                    None => return,
                };
                let sym = self.stream_closer_sym(&elem);
                self.emit(format!("call void @{sym}(ptr {slot})"));
            }
            DropKind::FreeArr => {
                // Free the array's final backing buffer (field 0).
                let a = self.fresh_tmp();
                let d = self.fresh_tmp();
                self.emit(format!("{a} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                self.emit(format!("{d} = extractvalue {{ ptr, i64, i64 }} {a}, 0"));
                self.emit(format!("call void @__vyrn_free(ptr {d})"));
            }
            DropKind::FreeSmallArr => {
                // A `SmallArray<T, N>` (RFC-0056) is `{ i64 len, i64 cap, ptr
                // data, [N x T] inline }`. Its `data` pointer sits at byte
                // offset 16 (two i64 header words), independent of `N`/`T`. It is
                // null while inline and heap once spilled, so `free(data)` frees
                // iff spilled — `free(null)` is a well-defined no-op. The inline
                // slots need no reclamation (their elements are a safe leak,
                // exactly as for `Array`).
                let p = self.fresh_tmp();
                let d = self.fresh_tmp();
                self.emit(format!("{p} = getelementptr i8, ptr {slot}, i64 16"));
                self.emit(format!("{d} = load ptr, ptr {p}"));
                self.emit(format!("call void @__vyrn_free(ptr {d})"));
            }
            DropKind::FreeMap => {
                // Free all three of the map's final backing buffers (keys,
                // values, the hash index); elements are a safe leak, exactly as
                // for arrays (RFC-0028).
                let a = self.fresh_tmp();
                let k = self.fresh_tmp();
                let v = self.fresh_tmp();
                let ix = self.fresh_tmp();
                self.emit(format!(
                    "{a} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
                ));
                self.emit(format!(
                    "{k} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {a}, 0"
                ));
                self.emit(format!(
                    "{v} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {a}, 1"
                ));
                self.emit(format!(
                    "{ix} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {a}, 4"
                ));
                self.emit(format!("call void @__vyrn_free(ptr {k})"));
                self.emit(format!("call void @__vyrn_free(ptr {v})"));
                self.emit(format!("call void @__vyrn_free(ptr {ix})"));
            }
            // Phase 5: an aggregate owns its places. The walk is the type — and
            // in a generic instantiation the type still carries its parameters,
            // so it is the SOLVED one. `own` decides against the declaration and
            // this emits against the instance, exactly as `rel_kind` does one
            // function down. Without it `llt` answered `void` for a `Param` and
            // the walk emitted `load { void, { i64, i64 } }`, which clang refuses
            // (`examples/generics.vyrn`); it went unseen while only an `Option`
            // and a `Result` reached here, because a generic sum's payload
            // travels as a word.
            DropKind::Deep(ty) => {
                let ty = self
                    .slot_ty(slot)
                    .unwrap_or_else(|| vyrn_frontend::types::substitute(ty, self.subst));
                let ll = self.llt(&ty);
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load {ll}, ptr {slot}"));
                // RFC-0093 M2: the places a take gave away. Empty for every
                // binding nothing took from, which is nearly all of them.
                self.rel_holes = self.hole_slots.get(slot).cloned().unwrap_or_default();
                // `emit_drop` is infallible by signature and this walk is not:
                // a type it cannot name is a leak, never a wrong free. The
                // instructions a failed walk already wrote are neither, so they
                // go back (see `Gen::mark`).
                let mark = self.mark();
                if self.deep_release(&v, &ty).is_err() {
                    self.rewind(mark);
                }
                self.rel_holes.clear();
            }
            // RFC-0086 M1: the type declared `impl Owned`, so its own `release`
            // is what reclaims it. An ordinary call to an ordinary function —
            // the protocol decided WHICH, and this is only the lowering.
            DropKind::Release(f, _) => {
                // A GENERIC declared release (`impl<T> Owned for Slots<T>`)
                // flattens to a generic function, so its symbol depends on the
                // type arguments and its definition has to be asked for. Park
                // the binding under a reserved name and go through the ordinary
                // call path, which solves them from the receiver, mangles the
                // symbol and queues the instance — the same route a written
                // call takes, and the one the direct backend already took.
                // Errors are swallowed for the reason `Deep` swallows them: a
                // drop this cannot emit is a leak, never a wrong free.
                if self
                    .funcs
                    .get(f.as_str())
                    .is_some_and(|c| !c.type_params.is_empty())
                {
                    if let Some(ty) = self.slot_ty(slot) {
                        let f = f.clone();
                        self.scope
                            .push(vec![(REL_RECV.to_string(), slot.to_string(), ty.clone())]);
                        let recv = [Expr::Var {
                            name: REL_RECV.to_string(),
                            line: 0,
                        }];
                        let mark = self.mark();
                        if self.gen_call(&f, &recv).is_err() {
                            self.rewind(mark);
                        }
                        self.scope.pop();
                        let ll = self.llt(&ty);
                        let v = self.fresh_tmp();
                        let mark = self.mark();
                        self.emit(format!("{v} = load {ll}, ptr {slot}"));
                        if self.free_declared_boxes(&v, &ty).is_err() {
                            self.rewind(mark);
                        }
                    }
                    return;
                }
                let pty = self
                    .param_types
                    .get(f)
                    .and_then(|p| p.first())
                    .cloned()
                    .unwrap_or(Type::Unit);
                let ll = self.llt(&pty);
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load {ll}, ptr {slot}"));
                self.emit(format!("call void @{}({ll} {v})", fn_sym(&f)));
                // RFC-0096: the payload boxes are the enum's own storage, and
                // the declaration cannot reach them. Errors swallowed for the
                // reason above: a drop this cannot emit is a leak, never a
                // wrong free — and taken back, for the reason `Deep` takes them
                // back.
                let mark = self.mark();
                if self.free_declared_boxes(&v, &pty).is_err() {
                    self.rewind(mark);
                }
            }
        }
    }

    /// How a value of `ty` is released, with this instantiation's substitution
    /// applied — the same question `own` answered, asked of the same table.
    fn rel_kind(&self, ty: &Type) -> Option<DropKind> {
        self.owned
            .release_kind(&vyrn_frontend::types::substitute(ty, self.subst))
    }

    /// The heap buffers the value in `slot` holds right now, loaded so a store may
    /// replace it before they are handed back.
    ///
    /// A deliberate subset of [`Gen::emit_drop`]. A cell, a stream and a declared
    /// `release` are all observable from inside the language — a stale cell traps
    /// and a user `release` is ordinary Vyrn that may print — and the interpreter
    /// reclaims those from the value a binding took at its `let`, not from the
    /// slot's last one. A store leaves all three alone rather than making the
    /// three engines run different programs.
    fn snap_old(&mut self, slot: &str, ty: &Type) -> Vec<(String, bool)> {
        if self.rel_kind(ty).is_none() {
            return Vec::new();
        }
        let ll = self.llt(ty);
        let v = self.fresh_tmp();
        self.emit(format!("{v} = load {ll}, ptr {slot}"));
        self.snap_val(&v, ty)
    }

    /// Release what the entry at address `ep` holds — a map key or a map value
    /// whose slot is about to be overwritten or shifted away (RFC-0028).
    ///
    /// Deeper than [`Gen::snap_old`], and for one reason: the value is READ out
    /// of its slot first, so the walk is not reading a length the store is in the
    /// middle of replacing. That was the whole argument for the shallow
    /// snapshot, and it does not apply to an entry a map is giving up. The walk
    /// is then exactly the one the map's own drop makes over that entry
    /// (`release_elems` in the `Map` arm of [`Gen::deep_release`]), so an
    /// overwrite and a drop reclaim the same bytes.
    ///
    /// A stream and a declared `release` are the two exceptions, for the reason
    /// [`Gen::snap_old`] states: both are observable from inside the language,
    /// and the interpreter — the oracle — runs neither when a value is replaced.
    fn release_entry(&mut self, ep: &str, ty: &Type) -> Result<(), String> {
        match self.rel_kind(ty) {
            None | Some(DropKind::CloseStream) | Some(DropKind::Release(..)) => Ok(()),
            _ => {
                let ll = self.llt(ty);
                let old = self.fresh_tmp();
                self.emit(format!("{old} = load {ll}, ptr {ep}"));
                self.deep_release(&old, ty)
            }
        }
    }

    /// The same, off a value already in a register — what a record field is.
    fn snap_val(&mut self, v: &str, ty: &Type) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        match self.rel_kind(ty) {
            // A `String` IS its buffer pointer.
            Some(DropKind::FreeStr) => out.push((v.to_string(), true)),
            // An `Array<T>` answers `FreeArr`, or `Deep` where its elements have
            // a release row of their own (RFC-0092 M2). A store hands back the
            // one buffer either way: the elements it held leak, exactly as they
            // did before the row landed, and freeing them here would mean
            // reading a length the store is in the middle of replacing.
            Some(DropKind::FreeArr) => {
                let d = self.fresh_tmp();
                self.emit(format!("{d} = extractvalue {{ ptr, i64, i64 }} {v}, 0"));
                out.push((d, false));
            }
            Some(DropKind::Deep(t)) if matches!(self.resolve(&t), Type::Array(_)) => {
                let d = self.fresh_tmp();
                self.emit(format!("{d} = extractvalue {{ ptr, i64, i64 }} {v}, 0"));
                out.push((d, false));
            }
            // A SUM whose payload travels boxed (round twenty-nine): the box
            // is the sum's own storage, and a store that displaces the sum
            // must free it — selected by tag as pure data flow, so the snap
            // stays straight-line (`free` refuses the null the other tag
            // selects). The payload VALUE keeps the shallow rule: what it
            // owns leaks rather than risk reading through a value the store
            // is replacing.
            Some(DropKind::Deep(t))
                if matches!(self.resolve(&t), Type::Option(_) | Type::Result(..)) =>
            {
                let (box_some, box_zero) = match self.resolve(&t) {
                    Type::Option(p) => (self.payload_boxed(&p), false),
                    Type::Result(a, b) => (self.payload_boxed(&a), self.payload_boxed(&b)),
                    _ => unreachable!(),
                };
                if box_some || box_zero {
                    let sll = "{ i1, i64, i64 }";
                    let tag = self.fresh_tmp();
                    let w = self.fresh_tmp();
                    self.emit(format!("{tag} = extractvalue {sll} {v}, 0"));
                    self.emit(format!("{w} = extractvalue {sll} {v}, 1"));
                    let word = if box_some && box_zero {
                        w
                    } else {
                        let sel = self.fresh_tmp();
                        if box_some {
                            self.emit(format!("{sel} = select i1 {tag}, i64 {w}, i64 0"));
                        } else {
                            self.emit(format!("{sel} = select i1 {tag}, i64 0, i64 {w}"));
                        }
                        sel
                    };
                    let ptr = self.fresh_tmp();
                    self.emit(format!("{ptr} = inttoptr i64 {word} to ptr"));
                    out.push((ptr, false));
                }
            }
            // A RECORD, shallowly (round eighteen): each heap-owning field's
            // buffer pointer, read before the store overwrites the aggregate —
            // the same rule as the arms around it, so elements and boxed
            // payloads still leak rather than risk reading through a value the
            // store is replacing. `Dec { d: Array<Int64> }` reassigned in
            // `parseFloat64`'s halving loop was 360 blocks of exactly this. A
            // declared `release` never reaches here — `rel_kind` answers
            // `Release` for it, not `Deep`.
            Some(DropKind::Deep(_)) if self.record_fields(ty).is_some() => {
                let fields = self.record_fields(ty).expect("guarded");
                let ll = self.llt(ty);
                for (i, f) in fields.iter().enumerate() {
                    // Kind-driven since round twenty-nine: a heap-free field
                    // whose type still has a release row is a boxed-payload
                    // sum, and the sum arm below snapshots its box.
                    if !self.owns_heap(&f.ty) && self.owned.release_kind(&f.ty).is_none() {
                        continue;
                    }
                    let fv = self.fresh_tmp();
                    self.emit(format!("{fv} = extractvalue {ll} {v}, {i}"));
                    let fty = f.ty.clone();
                    let inner = self.snap_val(&fv, &fty);
                    out.extend(inner);
                }
            }
            // `{ i64 len, i64 cap, ptr data, [N x T] inline }` — field 2, null
            // while the array is still inline, which `free` refuses.
            Some(DropKind::FreeSmallArr) => {
                let ll = self.llt(ty);
                let d = self.fresh_tmp();
                self.emit(format!("{d} = extractvalue {ll} {v}, 2"));
                out.push((d, false));
            }
            Some(DropKind::FreeMap) => {
                let m = "{ ptr, ptr, i64, i64, ptr }";
                let k = self.fresh_tmp();
                let vv = self.fresh_tmp();
                let ix = self.fresh_tmp();
                self.emit(format!("{k} = extractvalue {m} {v}, 0"));
                self.emit(format!("{vv} = extractvalue {m} {v}, 1"));
                self.emit(format!("{ix} = extractvalue {m} {v}, 4"));
                out.push((k, false));
                out.push((vv, false));
                out.push((ix, false));
            }
            _ => {}
        }
        out
    }

    /// Hand a snapshot back, after the store that replaced it. Both callees
    /// refuse a null, and `@__vyrn_str_free` refuses a [`STR_STATIC`] literal.
    fn free_snap(&mut self, snap: &[(String, bool)]) {
        for (p, hdr) in snap {
            if *hdr {
                self.emit(format!("call void @__vyrn_str_free(ptr {p})"));
            } else {
                self.emit(format!("call void @__vyrn_free(ptr {p})"));
            }
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let {
                name,
                value,
                ty: decl_ty,
                ..
            } => {
                // Node-address identity — must match `vyrn_frontend::own`, which
                // ran on this same borrowed AST.
                let key = stmt as *const Stmt as usize;
                // RFC-0037: the annotation is the expected type for any lambda
                // literal / bare function name inside the initializer.
                let pushed = decl_ty.is_some();
                if let Some(t) = decl_ty {
                    self.expect.push(t.clone());
                }
                let r = self.gen_expr(value);
                if pushed {
                    self.expect.pop();
                }
                let (v, vty) = r?;
                // Coerce to the annotation if present (record width subtyping).
                let (v, bty) = match decl_ty {
                    Some(t) => self.coerce_flow(v, value, &vty, t)?,
                    None => (v, vty),
                };
                let ll = self.llt(&bty);
                let slot = self.declare(name, &bty);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                // A String accumulator gets its append shadow here, at the one
                // declaration site, and starts every execution of this `let`
                // unowned — including the second trip through an enclosing loop,
                // where the slot has just been re-stored with the initializer.
                //
                // It starts OWNED when this `let` owns its initializer, which is
                // the fact `own` already decided. Starting it unowned abandoned the
                // initializer's buffer at the first append — Phase 4c recorded that
                // leak and this is where it closes. Starting it owned for a binding
                // that names somebody else's storage (`let mut s = r.name` is a
                // borrow, not a move) would free that storage instead, which is why
                // the answer is read rather than assumed.
                //
                // A LITERAL initializer is somebody else's storage too, and the
                // second half of the test is the same one the module-state seed
                // has always carried. `own` says a `let mut acc = ""` is droppable
                // (RFC-0096 M3 defect 3, and it is: the buffer it ENDS on is the
                // loop's), so without this the first append grew a data-segment
                // pointer in place. Being wrong the other way costs one copy.
                if bty == Type::Str && self.append_ok.contains(name.as_str()) {
                    let flag = self.str_append_shadow(&slot);
                    let owns = (self.droppable.contains_key(&key) && !matches!(value, Expr::Str(_)))
                        as i64;
                    self.emit(format!("store i64 {owns}, ptr {flag}"));
                }
                // If ownership analysis proved this heap binding non-escaping,
                // schedule it to be reclaimed when its block exits.
                if let Some(kind) = self.droppable.get(&key).cloned() {
                    // RFC-0093 M2: a take gave one of this binding's places
                    // away, so the walk must not hand it back. The set is
                    // remembered by SLOT, which is what `emit_drop` holds.
                    if let Some(h) = self.holes_map.get(&key) {
                        self.hole_slots.insert(slot.clone(), h.clone());
                    }
                    self.register_drop(key, slot, kind);
                } else if let Some(kind) = self.early.get(&key).cloned() {
                    // Round twenty-one: a MOVED binding whose take runs later
                    // than some early exit. Registering gives the placed rows
                    // a slot to free at those exits; no Block row exists for
                    // it, so nothing runs at fall-through and nothing runs
                    // after the take.
                    self.register_drop(key, slot, kind);
                }
                Ok(())
            }
            Stmt::Assign { name, value, .. } => {
                let (slot, tty) = self
                    .lookup(name)
                    .ok_or_else(|| format!("unbound `{name}`"))?;
                // `s = s + a + b` on an eligible local String: grow the buffer
                // instead of building a new one (see `emit_str_append`). Only
                // outside a `region` — arena memory cannot be `realloc`'d — and
                // only for a slot that owns a shadow, which is exactly a `let`
                // -declared local that `append_candidates` cleared. Operands are
                // evaluated left to right, as the general path would.
                if self.region_depth == 0 && self.str_append.contains_key(&slot) {
                    if let Some(parts) = self_append_spine(name, value) {
                        // The spine handles this store's ownership itself
                        // (§22's own state machine, not yet subsumed by the
                        // plan) — acknowledged so §26's finish check knows
                        // the site was considered, not walked past.
                        let _ = self.plan.store_owned_at(stmt as *const Stmt as usize);
                        self.expect.push(tty.clone());
                        let owned_here = self.plan.store_owned_at(stmt as *const Stmt as usize)
                            && self.region_depth == 0;
                        let mark = self.arg_frees.len();
                        let vals: Result<Vec<String>, String> = parts
                            .iter()
                            .map(|p| self.gen_expr(p).map(|(v, _)| v))
                            .collect();
                        self.expect.pop();
                        // The append COPIES each operand into the accumulator,
                        // so an operand this statement allocated is released
                        // after it (RFC-0096 M3) — `s = s + i.toString()` is
                        // the same `@str` temporary the general `+` path frees,
                        // reached through the fast path instead.
                        for (p, v) in parts.iter().zip(vals?) {
                            self.emit_str_append_owned(&slot, &v, owned_here);
                            self.free_str_temp(p, &v);
                        }
                        // A CALL-producer part (`acc = acc + substring(..)`) is
                        // the other half of the partition: its row is
                        // `Released`, pushed by the `gen_expr` wrapper — and
                        // this fast path was the one consumer with no drain, so
                        // every render loop whose accumulator is returned
                        // leaked one temporary per glyph (exit-residue round
                        // five: herofield's 660, and the emit/render loops of
                        // std/json and std/html behind it).
                        for (v, ty) in self.arg_frees.split_off(mark) {
                            self.free_arg_temp(&v, &ty);
                        }
                        return Ok(());
                    }
                }
                self.expect.push(tty.clone());
                let r = self.gen_expr(value);
                self.expect.pop();
                let (v, vty) = r?;
                let (v, _) = self.coerce(v, &vty, &tty)?;
                // RFC-0089 rule 4: the store releases what the place held. Not when
                // the new value names the place — `a = @push(a, i)` grows the old
                // buffer and hands it back, so freeing it would be a double free.
                // The release runs AFTER the value is built, which is the PR #61
                // sha1 lesson.
                //
                // A STRING `+` IS THE EXCEPTION, and leaving it out was a leak
                // that the append fast path above hid. `__vyrn_str_concat`
                // always calls `__vyrn_str_new` and memcpy's both operands
                // (see the prelude): it cannot hand back either input, so the
                // old buffer is garbage the moment the store lands.
                //
                // What made it invisible: `out = out + s` is caught by the
                // append spine above and never reaches here, so the common
                // shape was fine. Anything the spine declines was not.
                // `out = "x" + out` — a prepend, which no in-place append can
                // serve — leaked 9.9 GB over 50,000 calls of a 200-iteration
                // loop where the append form used 4.2 MB. So did `out = out + s`
                // in a function whose `out` is later consumed into a record,
                // because the spine declines a slot with no shadow.
                let fresh_str = matches!(self.resolve(&tty), Type::Str)
                    && matches!(value, Expr::Binary { op: BinOp::Add, .. });
                // RFC-0114 M2: whether the place is OWNED here is the analysis's
                // per-statement answer (`fold_store_owned`) — not the per-binding
                // `slot_owns`, which abandoned every store of a binding whose
                // value eventually escapes, and released over holes it could not
                // see. The value-side test (`fresh_str` / `mentions_place`)
                // stays: it answers whether the NEW value can alias the old,
                // which is representation knowledge.
                // The plan is asked FIRST: the query records the site as
                // considered (§26's finish check), and a `region`'s arena
                // ownership then gates the release without hiding the site.
                let owned_here = self.plan.store_owned_at(stmt as *const Stmt as usize)
                    && self.region_depth == 0;
                // Round eighteen: a mention that is only ever a read
                // argument to a declared non-lender cannot hand the old value
                // back — `dec = halveBy(dec, m)` — and the plan says which
                // stores those are (`store_fresh_at`).
                let snap = if owned_here
                    && (fresh_str
                        || !vyrn_frontend::movecheck::mentions_place(value, name)
                        || self.plan.store_fresh_at(stmt as *const Stmt as usize))
                {
                    self.snap_old(&slot, &tty)
                } else {
                    Vec::new()
                };
                let ll = self.llt(&tty);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                self.free_snap(&snap);
                self.str_append_reset(&slot);
                Ok(())
            }
            Stmt::SetField {
                name, field, value, ..
            } => {
                let (slot, tty) = self
                    .lookup(name)
                    .ok_or_else(|| format!("unbound `{name}`"))?;
                let fields = self
                    .record_fields(&tty)
                    .ok_or_else(|| format!("`{name}` is not a record"))?;
                let (idx, fty) = fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| &f.name == field)
                    .map(|(i, f)| (i, f.ty.clone()))
                    .ok_or_else(|| format!("no field `{field}`"))?;
                self.expect.push(fty.clone());
                let r = self.gen_expr(value);
                self.expect.pop();
                let (v, vty) = r?;
                let (v, _) = self.coerce(v, &vty, &fty)?;
                // Rebuild the record value with the new field, then store it back.
                let rec_ll = self.llt(&tty);
                let field_ll = self.llt(&fty);
                let cur = self.fresh_tmp();
                let next = self.fresh_tmp();
                self.emit(format!("{cur} = load {rec_ll}, ptr {slot}"));
                // Rule 4 through a field: the record owns what its field holds, so
                // storing over it releases the old one. Census §4's second row.
                // §26 steps 3–4: the plan's per-statement answer replaces the
                // per-binding registry guess (`slot_owns`), queried before the
                // region gate so an arena-owned site still counts as
                // considered. The value-alias guard folded with it.
                let snap = if self.plan.store_owned_at(stmt as *const Stmt as usize)
                    && self.region_depth == 0
                {
                    let old = self.fresh_tmp();
                    self.emit(format!("{old} = extractvalue {rec_ll} {cur}, {idx}"));
                    self.snap_val(&old, &fty)
                } else {
                    Vec::new()
                };
                self.emit(format!(
                    "{next} = insertvalue {rec_ll} {cur}, {field_ll} {v}, {idx}"
                ));
                self.emit(format!("store {rec_ll} {next}, ptr {slot}"));
                self.free_snap(&snap);
                Ok(())
            }
            // `name[index] = value` — in-place element store (RFC-0011). The
            // read path's bounds check + `getelementptr` + `store`, with the
            // value coerced into the element type (validated element types trap
            // inline via `coerce`'s `emit_validation`). No header write-back: the
            // element lives in the shared buffer, whose `{ptr,len,cap}` is
            // unchanged. A fixed `Array<T, N>` stores straight into its stack slot.
            Stmt::IndexSet {
                name, index, value, ..
            } => {
                let (slot, aty) = self
                    .lookup(name)
                    .ok_or_else(|| format!("unbound `{name}`"))?;
                // The store dispatches exactly as the read does (RFC-0091 M2):
                // `a[i] = v` asks the receiver's type for `place atSet`. The
                // seeded row yields `@slot(a, i)` — this binding's own element —
                // and the lowering below is unchanged.
                // A user container's store is its own statement group, lowered
                // by the statements this backend already has.
                if let Some(blk) =
                    vyrn_frontend::project::store_index(self.impls, name, index, value, &aty)?
                {
                    // The projection's own statements decide the release —
                    // acknowledged so §26's finish check knows the site was
                    // considered, not walked past.
                    let _ = self.plan.store_owned_at(stmt as *const Stmt as usize);
                    return self.gen_block(blk);
                }
                let bad_l = self.fresh_label("set.oob");
                let ok_l = self.fresh_label("set.ok");
                match self.resolve(&aty) {
                    Type::Array(inner) => {
                        let elem = *inner;
                        let ell = self.llt(&elem);
                        let (iv, _) = self.gen_expr(index)?;
                        self.expect.push(elem.clone());
                        let r = self.gen_expr(value);
                        self.expect.pop();
                        let (v, vty) = r?;
                        let (v, _) = self.coerce(v, &vty, &elem)?;
                        // The header is loaded only after the index and value
                        // ran: either may `modify` the array, and the bounds
                        // check must trust the post-mutation len.
                        let hdr = self.fresh_tmp();
                        let data = self.fresh_tmp();
                        let len = self.fresh_tmp();
                        self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                        self.emit(format!(
                            "{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"
                        ));
                        self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
                        let oob = self.fresh_tmp();
                        self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                        self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                        self.emit_array_oob_trap(&bad_l, &iv);
                        self.emit_label(&ok_l);
                        let ep = self.fresh_tmp();
                        self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {iv}"));
                        // Rule 4 through an element: the container owns what its
                        // element holds, so storing over it releases the old one.
                        let snap = if self.plan.store_owned_at(stmt as *const Stmt as usize)
                            && self.region_depth == 0
                        {
                            self.snap_old(&ep, &elem)
                        } else {
                            Vec::new()
                        };
                        self.free_snap(&snap);
                        self.emit(format!("store {ell} {v}, ptr {ep}"));
                        Ok(())
                    }
                    // `sa[i] = v` (RFC-0056): store into the live buffer (inline
                    // while `cap == N`, else heap); the header is unchanged.
                    Type::SmallArray(inner, n) => {
                        let elem = *inner;
                        let ell = self.llt(&elem);
                        let (iv, _) = self.gen_expr(index)?;
                        self.expect.push(elem.clone());
                        let r = self.gen_expr(value);
                        self.expect.pop();
                        let (v, vty) = r?;
                        let (v, _) = self.coerce(v, &vty, &elem)?;
                        // Base/len are read only after the index and value ran:
                        // either may `modify` the array, and the bounds check
                        // must trust the post-mutation len.
                        let (base, len, _cap, _data) = self.sa_slot_base(&slot, &elem, n);
                        let oob = self.fresh_tmp();
                        self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                        self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                        self.emit_array_oob_trap(&bad_l, &iv);
                        self.emit_label(&ok_l);
                        let ep = self.fresh_tmp();
                        self.emit(format!("{ep} = getelementptr {ell}, ptr {base}, i64 {iv}"));
                        // Rule 4 through an element, exactly as the `Array` arm
                        // above: the binding owns what its elements hold, so
                        // storing over one releases the old one. Without this,
                        // `sa[i] = other` abandoned the displaced element's
                        // buffer — reclaimed neither here nor at drop, which
                        // releases only the slots' current contents.
                        let snap = if self.plan.store_owned_at(stmt as *const Stmt as usize)
                            && self.region_depth == 0
                        {
                            self.snap_old(&ep, &elem)
                        } else {
                            Vec::new()
                        };
                        self.free_snap(&snap);
                        self.emit(format!("store {ell} {v}, ptr {ep}"));
                        Ok(())
                    }
                    Type::ArrayN(inner, n) => {
                        let elem = *inner;
                        let ell = self.llt(&elem);
                        let aggty = format!("[{n} x {ell}]");
                        let (iv, _) = self.gen_expr(index)?;
                        self.expect.push(elem.clone());
                        let r = self.gen_expr(value);
                        self.expect.pop();
                        let (v, vty) = r?;
                        let (v, _) = self.coerce(v, &vty, &elem)?;
                        let oob = self.fresh_tmp();
                        self.emit(format!("{oob} = icmp uge i64 {iv}, {n}"));
                        self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                        self.emit_array_oob_trap(&bad_l, &iv);
                        self.emit_label(&ok_l);
                        let ep = self.fresh_tmp();
                        self.emit(format!(
                            "{ep} = getelementptr {aggty}, ptr {slot}, i64 0, i64 {iv}"
                        ));
                        // A fixed array's displaced element is not released
                        // here today (a recorded residue, preserved by this
                        // migration) — acknowledged for §26's finish check.
                        let _ = self.plan.store_owned_at(stmt as *const Stmt as usize);
                        self.emit(format!("store {ell} {v}, ptr {ep}"));
                        Ok(())
                    }
                    // `m[k] = v` on a Map (RFC-0028): insert-or-update in place.
                    Type::Map(key, val) => {
                        // A map entry's release is `emit_map_set`'s own two
                        // questions — acknowledged for §26's finish check.
                        let _ = self.plan.store_owned_at(stmt as *const Stmt as usize);
                        let key = *key;
                        let val = *val;
                        let (kv, _) = self.gen_expr(index)?;
                        self.expect.push(val.clone());
                        let r = self.gen_expr(value);
                        self.expect.pop();
                        let (v, vty) = r?;
                        let (v, _) = self.coerce(v, &vty, &val)?;
                        // Rule 4 through an entry. Two questions, not the element
                        // store's three: a map owns its values outright — RFC-0092
                        // M2 removed the shallow views, so the only route into a
                        // value is a store and rule 2 refuses storing a borrow —
                        // so who owns the MAP does not change who owns the value
                        // this store displaces. Nobody can reach it afterwards
                        // whether the binding is dropped at the block, at an
                        // explicit `drop`, or by a caller. What is asked is the
                        // arena (which owns what was allocated inside a `region`)
                        // and aliasing: a new value or a key that names the map
                        // could name the very bytes this frees.
                        let drop_old = self.region_depth == 0
                            && !vyrn_frontend::movecheck::mentions_place(value, name)
                            && !vyrn_frontend::movecheck::mentions_place(index, name);
                        self.emit_map_set(&slot, &kv, &v, &key, &val, drop_old)
                    }
                    other => Err(format!(
                        "`{name}[i] = ..` needs an Array or Map, found {other:?}"
                    )),
                }
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => {
                        let ret_expect = self.fn_ret.clone();
                        self.expect.push(ret_expect);
                        let r = self.gen_expr(e);
                        self.expect.pop();
                        let (v, vty) = r?;
                        let ret = self.fn_ret.clone();
                        let (v, _) = self.coerce_flow(v, e, &vty, &ret)?;
                        let ll = self.llt(&ret);
                        // Free in-scope owned temporaries before leaving (the
                        // return value never aliases one — droppable bindings by
                        // definition do not escape).
                        let keep = (self.region_depth > 0
                            && matches!(self.resolve(&ret), Type::Str))
                        .then_some(v.as_str());
                        self.emit_all_drops_keeping(
                            ExitKind::Return,
                            stmt as *const Stmt as usize,
                            keep,
                        );
                        self.emit_modify_copyout();
                        self.emit_term(format!("ret {ll} {v}"));
                    }
                    None => {
                        self.emit_all_drops(ExitKind::Return, stmt as *const Stmt as usize);
                        self.emit_modify_copyout();
                        self.emit_term("ret void".into());
                    }
                }
                Ok(())
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let (c, _) = self.gen_expr(cond)?;
                // RFC-0114 Rule N: the analysis says one branch consumed a
                // binding the other still holds at the join, where nothing may
                // read it again — so the still-owning edge releases it here.
                // An `if` with no else-block grows one when the implicit edge
                // is the one that owes a release.
                let ers = self
                    .plan
                    .edge_releases_at(stmt as *const Stmt as usize)
                    .cloned()
                    .unwrap_or_default();
                let else_owes = ers.iter().any(|(_, t)| *t == 1);
                let then_l = self.fresh_label("then");
                let end_l = self.fresh_label("endif");
                let else_l = if else_block.is_some() || else_owes {
                    self.fresh_label("else")
                } else {
                    end_l.clone()
                };
                self.emit_term(format!("br i1 {c}, label %{then_l}, label %{else_l}"));

                self.emit_label(&then_l);
                self.gen_block(then_block)?;
                if !self.terminated {
                    self.emit_edge_releases(&ers, 0);
                    self.emit_term(format!("br label %{end_l}"));
                }

                if let Some(eb) = else_block {
                    self.emit_label(&else_l);
                    self.gen_block(eb)?;
                    if !self.terminated {
                        self.emit_edge_releases(&ers, 1);
                        self.emit_term(format!("br label %{end_l}"));
                    }
                } else if else_owes {
                    self.emit_label(&else_l);
                    self.emit_edge_releases(&ers, 1);
                    self.emit_term(format!("br label %{end_l}"));
                }

                self.emit_label(&end_l);
                Ok(())
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                // An OPTIONAL projection as the scrutinee (RFC-0122): no
                // `Option` is built — prologue, one branch on the miss, and
                // the hit arm's binder aliased to the place.
                if self.optional_if_let(pattern, scrutinee, then_block, else_block)? {
                    return Ok(());
                }
                // Evaluate the scrutinee once, test the pattern, and branch to the
                // then-arm (payload bound into fresh locals) or the else-arm
                // (RFC-0060). No `phi` — the arms carry no value (statement form).
                let (sv, sty) = self.gen_expr(scrutinee)?;
                let sr = self.resolve(&sty);
                // Census §14, Phase 10a: a scrutinee that is a TEMPORARY owns
                // what it holds and has no name, so `own` gives the STATEMENT
                // the reclamation row. The value goes into a slot and the slot
                // onto a drop frame of its own, which is what makes the release
                // survive a `return` out of the arm — `emit_all_drops` walks the
                // frames, and this one is on the stack for the whole statement.
                let key = stmt as *const Stmt as usize;
                let scrut_drop = self.droppable.get(&key).cloned();
                if let Some(kind) = scrut_drop {
                    let slot = self.fresh_alloca(&self.llt(&sr).clone());
                    self.emit(format!("store {} {sv}, ptr {slot}", self.llt(&sr)));
                    self.register_drop(key, slot, kind);
                }
                let then_l = self.fresh_label("il.then");
                let end_l = self.fresh_label("il.end");
                let else_l = if else_block.is_some() {
                    self.fresh_label("il.else")
                } else {
                    end_l.clone()
                };
                let cond = self.gen_pattern_test(&sv, &sr, pattern)?;
                self.emit_term(format!("br i1 {cond}, label %{then_l}, label %{else_l}"));

                self.emit_label(&then_l);
                // A scope frame holds the pattern binders (payload borrows, never
                // drop-tracked — like a `match` arm's), wrapping the then-block.
                self.scope.push(Vec::new());
                self.gen_pattern_binds(&sv, &sr, pattern)?;
                self.gen_block(then_block)?;
                self.scope.pop();
                if !self.terminated {
                    self.emit_term(format!("br label %{end_l}"));
                }

                if let Some(eb) = else_block {
                    self.emit_label(&else_l);
                    self.gen_block(eb)?;
                    if !self.terminated {
                        self.emit_term(format!("br label %{end_l}"));
                    }
                }

                self.emit_label(&end_l);
                // The fall-through release. An arm that returned already ran it
                // through `emit_all_drops` and left `terminated` set, so nothing
                // is freed twice — the same rule `gen_block` follows.
                if !self.terminated {
                    self.emit_releases(ExitKind::Scrutinee, key);
                    // A MAP lookup's `Option` box is a fresh allocation even
                    // though `m[k]` spells a place — the `match` path's rule
                    // (round forty-two), on the `if let` spelling mapdemo
                    // uses. The payload shares the map's storage, so only the
                    // box goes back; `None` carries a zero word and `free`
                    // refuses null.
                    let map_lookup = matches!(scrutinee, Expr::Call { name, args, .. }
                        if name == "@at"
                            && args.first().and_then(|a| self.static_ty(a)).is_some_and(
                                |t| matches!(self.resolve(&t), Type::Map(..))));
                    if map_lookup && self.droppable.get(&key).is_none() && self.region_depth == 0 {
                        if let Type::Option(inner) = &sr {
                            if self.payload_boxed(inner) {
                                let w0 = self.fresh_tmp();
                                let q = self.fresh_tmp();
                                self.emit(format!(
                                    "{w0} = extractvalue {{ i1, i64, i64 }} {sv}, 1"
                                ));
                                self.emit(format!("{q} = inttoptr i64 {w0} to ptr"));
                                self.emit(format!("call void @__vyrn_free(ptr {q})"));
                            }
                        }
                    }
                }
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                let cond_l = self.fresh_label("wcond");
                let body_l = self.fresh_label("wbody");
                let end_l = self.fresh_label("wend");
                self.emit_term(format!("br label %{cond_l}"));

                self.emit_label(&cond_l);
                let (c, _) = self.gen_expr(cond)?;
                self.emit_term(format!("br i1 {c}, label %{body_l}, label %{end_l}"));

                self.emit_label(&body_l);
                self.loop_ctx.push(LoopCtx {
                    break_label: end_l.clone(),
                    continue_label: cond_l.clone(),
                    region_depth: self.region_depth,
                });
                self.gen_block(body)?;
                self.loop_ctx.pop();
                if !self.terminated {
                    self.emit_term(format!("br label %{cond_l}"));
                }

                self.emit_label(&end_l);
                Ok(())
            }
            // `break`/`continue` (RFC-0060): reclaim the body scopes exactly as a
            // normal iteration end would (drops + region exits), then branch to
            // the innermost loop's exit / continue target. The checker guarantees
            // a live loop context.
            Stmt::Break { .. } => {
                let ctx = self
                    .loop_ctx
                    .last()
                    .cloned()
                    .ok_or("`break` outside a loop reached codegen")?;
                self.emit_loop_exit_cleanup(&ctx, ExitKind::Break, stmt as *const Stmt as usize);
                self.emit_term(format!("br label %{}", ctx.break_label));
                Ok(())
            }
            Stmt::Continue { .. } => {
                let ctx = self
                    .loop_ctx
                    .last()
                    .cloned()
                    .ok_or("`continue` outside a loop reached codegen")?;
                self.emit_loop_exit_cleanup(&ctx, ExitKind::Continue, stmt as *const Stmt as usize);
                self.emit_term(format!("br label %{}", ctx.continue_label));
                Ok(())
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                ..
            } => {
                // RFC-0091 M3: a user container declares how it is iterated. The
                // desugar is asked for before the iterable is emitted, because
                // dispatch has to choose before anything is written — the same
                // reason `static_ty` exists at all.
                if let Some((size_fn, nth)) = self
                    .static_ty(iter)
                    .and_then(|t| vyrn_frontend::types::iterate_impl(self.impls, &t))
                {
                    let blk = vyrn_frontend::project::iterate_loop(
                        &size_fn, nth, var, iter, body, *line,
                    )?;
                    // RFC-0114 §26: the expansion cloned the body, so the
                    // plan's rows live on nodes this walk will never meet —
                    // the pairs let every query resolve to the original.
                    self.plan
                        .alias_clones(vyrn_frontend::project::iterate_aliases(blk));
                    return self.gen_block(blk);
                }
                // Evaluate the iterable once and snapshot a base element pointer
                // plus a length — matching the interpreter, which iterates a
                // copied element vector. Both array kinds reduce to (base T*, len).
                let (av, aty) = self.gen_expr(iter)?;
                let resolved = self.resolve(&aty);
                // RFC-0075 M2b: a stream is PULLED. It shared the indexed walk
                // below while it was a buffer; it is a producer now, and the
                // difference is the milestone.
                if let Type::Stream(inner) = &resolved {
                    let elem = (**inner).clone();
                    return self.gen_for_stream(var, body, &av, &elem);
                }
                // RFC-0092 M5, census "U4's price": an iterable that is a
                // TEMPORARY owns what it holds and has no name, so `own` gives
                // the STATEMENT the reclamation row — the same row Phase 10a
                // gives an `if let`'s scrutinee, and read the same way. The
                // value goes into a slot and the slot onto a drop frame of its
                // own, which is what makes the release survive a `return` out of
                // the body: `emit_all_drops` walks the frames, and this one is
                // on the stack for the whole statement.
                //
                // The frame is pushed BEFORE the loop's, so `drop_boundary`
                // sits above it and `break`/`continue` leave it alone — both
                // land on code that runs the fall-through release below.
                let key = stmt as *const Stmt as usize;
                let iter_drop = self.droppable.get(&key).cloned();
                if let Some(kind) = iter_drop {
                    let ty = self.llt(&resolved).clone();
                    let slot = self.fresh_alloca(&ty);
                    self.emit(format!("store {ty} {av}, ptr {slot}"));
                    self.register_drop(key, slot, kind);
                }
                // Iterating a String yields each byte as an Int (loaded as i8 and
                // zero-extended); arrays load their element type directly.
                let byte_elem = resolved == Type::Str;
                let elem = match &resolved {
                    Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _) => {
                        (**inner).clone()
                    }
                    Type::Str => Type::Int,
                    other => {
                        return Err(format!(
                            "for-loop needs an Array or String, found {other:?}"
                        ))
                    }
                };
                let ell = self.llt(&elem);
                let (data, len) = match &resolved {
                    Type::Str => {
                        let len = self.str_len(&av);
                        (av.clone(), len)
                    }
                    // A SmallArray (RFC-0056): pick the live base + its length.
                    Type::SmallArray(inner, n) => self.sa_value_base_len(&av, inner, *n),
                    Type::ArrayN(_, n) => {
                        // Fixed array is a value aggregate; spill to the stack and
                        // take a pointer to element 0. Length is the constant N.
                        let aggty = format!("[{n} x {ell}]");
                        let slot = self.fresh_alloca(&aggty);
                        self.emit(format!("store {aggty} {av}, ptr {slot}"));
                        let base = self.fresh_tmp();
                        self.emit(format!(
                            "{base} = getelementptr {aggty}, ptr {slot}, i64 0, i64 0"
                        ));
                        (base, format!("{n}"))
                    }
                    _ => {
                        // Growable array {ptr, i64 len, i64 cap}: data ptr + len.
                        let data = self.fresh_tmp();
                        let len = self.fresh_tmp();
                        self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
                        self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
                        (data, len)
                    }
                };
                let idx = self.fresh_alloca("i64");
                self.emit(format!("store i64 0, ptr {idx}"));
                let cond_l = self.fresh_label("fcond");
                let body_l = self.fresh_label("fbody");
                // `continue` targets the latch (steps the index, then re-tests),
                // so it re-evaluates the loop as a normal iteration end would.
                let latch_l = self.fresh_label("flatch");
                let end_l = self.fresh_label("fend");
                self.emit_term(format!("br label %{cond_l}"));

                // cond: index < length
                self.emit_label(&cond_l);
                let i = self.fresh_tmp();
                let done = self.fresh_tmp();
                self.emit(format!("{i} = load i64, ptr {idx}"));
                self.emit(format!("{done} = icmp uge i64 {i}, {len}"));
                self.emit_term(format!("br i1 {done}, label %{end_l}, label %{body_l}"));

                // body: bind the loop variable to data[index], then run the body.
                self.emit_label(&body_l);
                let bi = self.fresh_tmp();
                let ep = self.fresh_tmp();
                let ev = self.fresh_tmp();
                self.emit(format!("{bi} = load i64, ptr {idx}"));
                if byte_elem {
                    // A string byte: index i8 data, load, zero-extend to i64.
                    let raw = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr i8, ptr {data}, i64 {bi}"));
                    self.emit(format!("{raw} = load i8, ptr {ep}"));
                    self.emit(format!("{ev} = zext i8 {raw} to i64"));
                } else {
                    self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {bi}"));
                    self.emit(format!("{ev} = load {ell}, ptr {ep}"));
                }
                // A scope frame wrapping the body holds the loop variable; the
                // element is a borrow, not an owned allocation, so its drop frame
                // stays empty.
                self.scope.push(Vec::new());
                let vslot = self.declare(var, &elem);
                self.emit(format!("store {ell} {ev}, ptr {vslot}"));
                // RFC-0125 M3: a variable the body drains a field of keeps
                // the rest of its element, and the placer's rows for it —
                // keyed by the variable's spelling, since it has no `let` —
                // release that rest at every exit of the body.
                let vkey = vyrn_frontend::own::for_var_key(var);
                if let Some(kind) = self.droppable.get(&vkey).cloned() {
                    if let Some(h) = self.holes_map.get(&vkey) {
                        self.hole_slots.insert(vslot.clone(), h.clone());
                    }
                    self.register_drop(vkey, vslot.clone(), kind);
                }
                self.loop_ctx.push(LoopCtx {
                    break_label: end_l.clone(),
                    continue_label: latch_l.clone(),
                    region_depth: self.region_depth,
                });
                self.gen_block(body)?;
                self.loop_ctx.pop();
                self.scope.pop();
                if !self.terminated {
                    self.emit_term(format!("br label %{latch_l}"));
                }

                // latch: step the index and re-test (fall-through and `continue`
                // both land here).
                self.emit_label(&latch_l);
                let i2 = self.fresh_tmp();
                let inext = self.fresh_tmp();
                self.emit(format!("{i2} = load i64, ptr {idx}"));
                self.emit(format!("{inext} = add i64 {i2}, 1"));
                self.emit(format!("store i64 {inext}, ptr {idx}"));
                self.emit_term(format!("br label %{cond_l}"));

                self.emit_label(&end_l);
                // The fall-through release (RFC-0092 M5). A body that returned
                // already ran it through `emit_all_drops`; this label is still
                // reached from the condition, so the normal exit runs it once.
                if !self.terminated {
                    self.emit_releases(ExitKind::Scrutinee, key);
                }
                Ok(())
            }
            Stmt::Drop { name, .. } => {
                // Explicit reclamation: free a string, free an array's buffer, or
                // release a reference — the primitives the automatic-drop analysis
                // emits. Ownership analysis escaped `name`, so there is no double
                // free, and move checking forbids using it after this point.
                let (slot, ty) = self
                    .lookup(name)
                    .ok_or_else(|| format!("drop of unbound `{name}`"))?;
                // The same question the automatic block-exit path asks, asked of
                // the same table (RFC-0086 M1). A second copy here is what let
                // the two free different sets.
                // Ask the declared type first (an `impl Owned for Ring` is keyed
                // by the NAME, which resolving away would lose), then its
                // structural form (which is where a generic substitution lands).
                let rty = self.resolve(&ty);
                // RFC-0095 M1. A task is linear and `drop t` is one of its two
                // discharges, so it is emitted here rather than read off
                // `release_kind` — which answers `None` for a `Task`, because an
                // automatic block-exit row would free what the join already
                // freed.
                //
                // Three things happen, in this order. **Wait**, because the
                // worker may still be storing the result into the frame and
                // because the trap protocol says a trapping task prints its line
                // and exits 1 from whichever thread it runs on — a drop that
                // skipped the wait would swallow that. **Release the result by
                // its type**, which is the half that is easy to miss: a dropped
                // `Task<String>` has a String in its frame that the worker
                // allocated and nothing else will ever free. **Then release the
                // task**, which frees the frame, frees the record and closes the
                // handle.
                //
                // The frame pointer IS a slot holding the result — the leading
                // frame field is the result slot — so the release of the result
                // is the ordinary `emit_drop`, with no second walk.
                if let Type::Task(inner) = &rty {
                    let t = self.fresh_tmp();
                    self.emit(format!("{t} = load ptr, ptr {slot}"));
                    let frame = self.fresh_tmp();
                    self.emit(format!("{frame} = call ptr @__vyrn_join(ptr {t})"));
                    if let Some(kind) = self.rel_kind(inner) {
                        self.emit_drop(&frame, &kind);
                    }
                    self.emit(format!("call void @__vyrn_task_release(ptr {t})"));
                    return Ok(());
                }
                // A type with no row releases nothing, and this is not an error:
                // since Phase 8b the checker admits `drop v` where `v: T`, and
                // the instance decides. `Slots<String>` frees a buffer here and
                // `Slots<Person>` emits nothing, because a record is not on the
                // list `own::release_kind` keeps. A concrete `drop` of a type
                // that owns no heap was refused by the checker and never
                // arrives.
                let Some(kind) = self
                    .owned
                    .release_kind(&ty)
                    .or_else(|| self.owned.release_kind(&rty))
                else {
                    return Ok(());
                };
                self.emit_drop(&slot, &kind);
                Ok(())
            }
            Stmt::Expr(e) => {
                let (v, ty) = self.gen_expr(e)?;
                // Round twenty-eight: a statement-position call whose OWNED
                // result nothing binds — freed right after the call
                // (freelist's 100,000 discarded `remove` results).
                if self.plan.discarded_result(stmt as *const Stmt as usize) {
                    let rty = self.resolve(&ty);
                    self.free_arg_temp(&v, &rty);
                }
                Ok(())
            }
            Stmt::Region { body, .. } => {
                // Push an arena frame, run the body, then free everything the
                // region allocated. If the body always returns (terminates the
                // block), the exit call is unreachable and skipped — that path
                // leaks, which is safe (never a use-after-free).
                self.emit("call void @__vyrn_region_enter()".into());
                self.region_depth += 1;
                self.gen_block(body)?;
                self.region_depth -= 1;
                if !self.terminated {
                    self.emit("call void @__vyrn_region_exit()".into());
                }
                Ok(())
            }
        }
    }

    /// Emit code computing `expr`; return (operand, AST type).
    ///
    /// The wrapper keeps ONE fact: whether this expression is a call argument
    /// whose value the CALLER releases once the call is done with it
    /// (`rfcs/census-call-arguments.md`). `own` decided that, per argument node;
    /// this only remembers the register, and [`Gen::gen_call`] frees it after
    /// the call the argument belongs to. Recording it HERE — where the argument
    /// is evaluated — rather than hoisting the argument at the call is what
    /// keeps the evaluation order the one the program wrote.
    ///
    /// Inside a `region` the buffer came from the arena and the region exit
    /// reclaims it, exactly as [`Gen::free_str_temp`] stands aside there.
    fn gen_expr(&mut self, expr: &Expr) -> Result<(String, Type), String> {
        let r = self.gen_expr_inner(expr)?;
        if self.plan.arg_drop(expr as *const Expr as usize) && self.region_depth == 0 {
            self.arg_frees.push((r.0.clone(), r.1.clone()));
        }
        if crate::observe::on() {
            crate::observe::record(
                crate::observe::Site::Native,
                crate::observe::kind_of(expr),
                expr as *const Expr as usize,
                self.subst,
                &r.1,
            );
        }
        Ok(r)
    }

    /// Emit code computing `expr`; return (operand, AST type). The type is
    /// `Type::Unit` for value-less calls (`print`, Unit functions).
    fn gen_expr_inner(&mut self, expr: &Expr) -> Result<(String, Type), String> {
        match expr {
            // RFC-0093: a take is the load the read already emits, without the
            // `deep_copy` call that used to follow it.
            Expr::Consume { place, .. } => self.gen_expr(place),
            Expr::Int(n) => Ok((n.to_string(), Type::Int)),
            // A byte literal (RFC-0057) is an integer literal at the IR level; its
            // value flows through unchanged. The checker has already fixed its
            // type, so the surrounding coercion emits any needed truncation.
            Expr::Byte(b) => Ok((b.to_string(), Type::Int)),
            // LLVM double literals: the hex form encodes the exact bit pattern,
            // avoiding any decimal round-trip mismatch.
            Expr::Float(x) => Ok((format!("0x{:016X}", x.to_bits()), Type::Float)),
            Expr::Bool(b) => Ok(((*b as i64).to_string(), Type::Bool)),
            Expr::Str(s) => {
                let g = self
                    .str_globals
                    .get(s)
                    .ok_or_else(|| "string literal missing from pool".to_string())?;
                Ok((g.clone(), Type::Str))
            }
            Expr::Var { name, .. } => {
                // `None` is a constant Option aggregate, not a variable.
                if name == "None" {
                    return Ok((
                        "{ i1 0, i64 0, i64 0 }".into(),
                        Type::Option(Box::new(Type::Int)),
                    ));
                }
                // A nullary enum variant, e.g. `Empty`.
                if let Some((tag, enum_name)) = self.variants.get(name).cloned() {
                    let arity = self.enum_arity(&enum_name);
                    let ll = enum_ll(arity);
                    let mut cur = "undef".to_string();
                    let t = self.fresh_tmp();
                    self.emit(format!("{t} = insertvalue {ll} {cur}, i64 {tag}, 0"));
                    cur = t;
                    for slot in 1..=arity {
                        let t = self.fresh_tmp();
                        self.emit(format!("{t} = insertvalue {ll} {cur}, i64 0, {slot}"));
                        cur = t;
                    }
                    return Ok((cur, Type::Named(enum_name)));
                }
                let Some((slot, ty)) = self.lookup(name) else {
                    // A `fn`-typed PARAMETER used as a VALUE (RFC-0037 × RFC-0023):
                    // inside a specialized instance the parameter has no slot — it
                    // lives in `fn_bindings` as a known target + capture SSA values.
                    // STORING it (`pend[k] = cb`, `let g = cb`, `return cb`, a
                    // record field, …) materializes the same `{ i64, i64 }`
                    // defunctionalized aggregate a lambda/named source does, so
                    // storing a fn-param never diverges from calling it — for any
                    // signature, scalar or non-scalar payload alike.
                    if let Some(b) = self.fn_bindings.get(name).cloned() {
                        return self.construct_fnval_binding(&b);
                    }
                    // A bare function name in a value position (RFC-0037): an
                    // empty-payload defunctionalization variant.
                    if self.funcs.contains_key(name.as_str()) {
                        return self.construct_fnval_named(name);
                    }
                    return Err(format!("unbound `{name}`"));
                };
                let ll = self.llt(&ty);
                let t = self.fresh_tmp();
                self.emit(format!("{t} = load {ll}, ptr {slot}"));
                Ok((t, ty))
            }
            Expr::Unary { op, expr, .. } => {
                let (v, ty) = self.gen_expr(expr)?;
                let t = self.fresh_tmp();
                match op {
                    UnOp::Neg if matches!(self.resolve(&ty), Type::Float | Type::Float32) => {
                        let f = if self.resolve(&ty) == Type::Float32 {
                            "float"
                        } else {
                            "double"
                        };
                        self.emit(format!("{t} = fneg {f} {v}"))
                    }
                    // `-v` on a vector (RFC-0083 M2) is the same `fneg`, four lanes
                    // wide — a sign-bit flip, which is what makes it different from
                    // `F32x4.splat(0.0) - v`: `0.0 - -0.0` is `+0.0` and loses the
                    // sign a negation keeps.
                    UnOp::Neg if self.resolve(&ty) == Type::F32x4 => {
                        self.emit(format!("{t} = fneg <4 x float> {v}"))
                    }
                    UnOp::Neg if self.resolve(&ty) == Type::F64x2 => {
                        self.emit(format!("{t} = fneg <2 x double> {v}"))
                    }
                    // `~m` complements every lane. The constant is spelled out
                    // rather than written `splat (i32 -1)` because the elementwise
                    // form is what every LLVM this project has built against
                    // accepts, and `-O2` folds them to the same `pcmpeqd`/`pxor`.
                    // `~v` on an `I32x4` is the same instruction on the same
                    // representation — the mask and the integer vector are both
                    // `<4 x i32>`, and `v128.not` on the other backend has no lane
                    // width either.
                    UnOp::BitNot if matches!(self.resolve(&ty), Type::Mask32x4 | Type::I32x4) => {
                        self.emit(format!(
                            "{t} = xor <4 x i32> {v}, <i32 -1, i32 -1, i32 -1, i32 -1>"
                        ))
                    }
                    // The wide mask, two lanes of all-ones.
                    UnOp::BitNot if self.resolve(&ty) == Type::Mask64x2 => {
                        self.emit(format!("{t} = xor <2 x i64> {v}, <i64 -1, i64 -1>"))
                    }
                    // Two's-complement negation, four lanes wide: `-Int32.min` is
                    // `Int32.min`, the same wrap the scalar `sub {w} 0, {v}` below
                    // has. LLVM has no `ineg`.
                    UnOp::Neg if self.resolve(&ty) == Type::I32x4 => {
                        self.emit(format!("{t} = sub <4 x i32> zeroinitializer, {v}"))
                    }
                    UnOp::Neg if matches!(self.resolve(&ty), Type::IntN { .. }) => {
                        let w = self.llt(&ty);
                        self.emit(format!("{t} = sub {w} 0, {v}"))
                    }
                    UnOp::Neg => self.emit(format!("{t} = sub i64 0, {v}")),
                    UnOp::Not => self.emit(format!("{t} = xor i1 {v}, true")),
                    // `~v` = `xor v, -1` at the operand width (RFC-0045): all
                    // ones of the type, so it complements within the width (a
                    // sized integer's `iN`, or `i64` for the literal `Int`).
                    UnOp::BitNot if matches!(self.resolve(&ty), Type::IntN { .. }) => {
                        let w = self.llt(&ty);
                        self.emit(format!("{t} = xor {w} {v}, -1"))
                    }
                    UnOp::BitNot => self.emit(format!("{t} = xor i64 {v}, -1")),
                }
                Ok((t, ty))
            }
            Expr::Binary { op, lhs, rhs, .. } => self.gen_binary(*op, lhs, rhs),
            Expr::Call { name, args, .. } => self.gen_call(name, args),
            Expr::Match {
                scrutinee, arms, ..
            } => self.gen_match(expr as *const Expr as usize, scrutinee, arms),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => self.gen_if_expr(
                expr as *const Expr as usize,
                cond,
                then_branch,
                else_branch.as_deref(),
            ),
            Expr::Try { expr: operand, .. } => self.gen_try(operand, expr as *const Expr as usize),
            Expr::StructLit { name, fields, .. } => self.gen_struct_lit(name, fields),
            Expr::Field {
                expr: fbase, field, ..
            } => {
                let (v, ety) = self.gen_expr(fbase)?;
                // `str.byteLength` is one load from the String header
                // (RFC-0089 M1a; RFC-0058 named it, this made it O(1)).
                // `.length` on a String is rejected by the checker.
                if field == "byteLength" {
                    if let Type::Str = self.resolve(&ety) {
                        let len = self.str_len(&v);
                        // RFC-0114 R1′: an unnamed receiver this frame owns
                        // dies here — the header read was its last observer.
                        if self.plan.receiver_free(expr as *const Expr as usize)
                            && (self.region_depth == 0
                                || self.plan.receiver_malloc_at(expr as *const Expr as usize))
                        {
                            self.emit(format!("call void @__vyrn_str_free(ptr {v})"));
                        }
                        return Ok((len, Type::Int));
                    }
                }
                // `arr.length` is the element count: a constant for a fixed
                // array, field 1 of the `{ptr,len,cap}` triple otherwise.
                if field == "length" {
                    // RFC-0114 R1′ for containers: an unnamed receiver this
                    // frame owns dies after the count is read. `own` admits
                    // only silent kinds into the set, so `free_arg_temp` never
                    // meets a declared release here.
                    let rfree = self.plan.receiver_free(expr as *const Expr as usize)
                        && (self.region_depth == 0
                            || self.plan.receiver_malloc_at(expr as *const Expr as usize));
                    match self.resolve(&ety) {
                        Type::ArrayN(_, n) => return Ok((format!("{n}"), Type::Int)),
                        Type::Array(_) => {
                            let len = self.fresh_tmp();
                            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {v}, 1"));
                            if rfree {
                                self.free_arg_temp(&v, &ety);
                            }
                            return Ok((len, Type::Int));
                        }
                        // `smallArray.length` is field 0 of the SmallArray header
                        // (RFC-0056), the same in the inline and spilled states.
                        Type::SmallArray(inner, n) => {
                            let sa_ll = self.sa_ll(&inner, n);
                            let len = self.fresh_tmp();
                            self.emit(format!("{len} = extractvalue {sa_ll} {v}, 0"));
                            if rfree {
                                self.free_arg_temp(&v, &ety);
                            }
                            return Ok((len, Type::Int));
                        }
                        // `map.length` is the entry count (field 2 of the header).
                        Type::Map(..) => {
                            let len = self.fresh_tmp();
                            self.emit(format!(
                                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {v}, 2"
                            ));
                            if rfree {
                                self.free_arg_temp(&v, &ety);
                            }
                            return Ok((len, Type::Int));
                        }
                        _ => {}
                    }
                }
                let rfields = self
                    .record_fields(&ety)
                    .ok_or_else(|| format!("field access on non-record type"))?;
                let idx = rfields
                    .iter()
                    .position(|f| &f.name == field)
                    .ok_or_else(|| format!("no field `{field}`"))?;
                let fty = rfields[idx].ty.clone();
                let ll = self.llt(&ety);
                let t = self.fresh_tmp();
                self.emit(format!("{t} = extractvalue {ll} {v}, {idx}"));
                // RFC-0114 R1′: a SCALAR field read off an unnamed record this
                // frame owns is the record's last observer — free it whole. A
                // heap field stays out: `names_a_place` made the binding its
                // owner. A `lazy` field stays out too: forcing it reads the
                // record after this point.
                let rh = self.plan.receiver_holes_at(expr as *const Expr as usize);
                if self.plan.receiver_free(expr as *const Expr as usize)
                    && (self.region_depth == 0
                        || self.plan.receiver_malloc_at(expr as *const Expr as usize))
                    && self.owned.release_kind(&fty).is_none()
                    && vyrn_frontend::types::deferred(&fty).is_none()
                {
                    self.free_arg_temp(&v, &ety);
                } else if !rh.is_empty()
                    && self.plan.receiver_free(expr as *const Expr as usize)
                    && (self.region_depth == 0
                        || self.plan.receiver_malloc_at(expr as *const Expr as usize))
                    && !self.terminated
                {
                    // RFC-0125 M3: the read TOOK a heap field (`let sels =
                    // parse(q).sels`), and the placer's row frees the rest of
                    // the receiver around that hole. The value is already in
                    // `t`, so the walk reads nothing the free reaches.
                    if let Some(kind) = self.owned.release_kind(&ety) {
                        let slot = self.fresh_alloca(&ll);
                        self.emit(format!("store {ll} {v}, ptr {slot}"));
                        self.emit_drop_holed(&slot, &kind, rh);
                    }
                }
                // RFC-0085 M4a: reading a `lazy T` field FORCES it — the loaded
                // `{ i64, i64 }` is a stored nullary closure and this is the
                // call. Nothing is cached, so a second read is a second call
                // (see the RFC's "M4a — as landed").
                if let Some(inner) = vyrn_frontend::types::deferred(&fty) {
                    let sig = Type::Fn(Vec::new(), Box::new(inner.clone()));
                    return self.gen_fnval_call(t, &sig, &[]);
                }
                Ok((t, fty))
            }
            Expr::TryConstruct { name, args, .. } => self.gen_try_construct(name, &args[0]),
            // A spawned task (RFC-0025): evaluate the arguments HERE (spawn-site
            // evaluation order is observable and matches the eager interpreter),
            // pack them into a heap frame, and hand the shim a per-spawn-site
            // thunk that runs the isolated callee — on a real thread natively.
            Expr::Spawn { name, args, .. } => self.gen_spawn(name, args),
            Expr::ArrayLit { elems, .. } => {
                // An empty `[]` is a growable empty array — the same `{ptr,len,cap}`
                // triple an empty `[]` produces (the element type is a
                // placeholder; the representation is type-independent and the
                // annotation fixes it).
                if elems.is_empty() {
                    // An empty `[]` against a `SmallArray<T, N>` slot (RFC-0056)
                    // is the empty small-buffer array in the inline state:
                    // `len 0`, `cap N`, `data null`, inline slots dead.
                    if let Some(t) = self.expect.last().cloned() {
                        if let Type::SmallArray(inner, n) = self.resolve(&t) {
                            let ell = self.llt(&inner);
                            return Ok((
                                format!("{{ i64 0, i64 {n}, ptr null, [{n} x {ell}] undef }}"),
                                Type::SmallArray(inner, n),
                            ));
                        }
                    }
                    return Ok((
                        "{ ptr null, i64 0, i64 0 }".into(),
                        Type::Array(Box::new(Type::Int)),
                    ));
                }
                // RFC-0037: the enclosing storage boundary's Array type (if
                // any) supplies each element's expected type, so a lambda
                // literal element knows its signature.
                let elem_expect: Option<Type> =
                    self.expect.last().and_then(|t| match self.resolve(t) {
                        Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => Some(*i),
                        _ => None,
                    });
                let pushed = elem_expect.is_some();
                if let Some(t) = &elem_expect {
                    self.expect.push(t.clone());
                }
                // When the enclosing storage boundary names the element type, the
                // aggregate is built AT that type and every element is coerced into
                // it. Inferring the element type from `elems[0]` instead is wrong in
                // three separate ways, and each one was measured:
                //
                // - A GROWABLE element (an `Array<T>` or `Map<K,V>` — e.g. an
                //   `Array<Array<Int64>>`) must reach its heap representation BEFORE
                //   it enters the outer aggregate, or a nested literal like
                //   `[[1], [2, 3]]` lowers as a fixed 2-D C-array `[2 x [1 x i64]]`,
                //   which is the wrong repr AND fails to build the moment two inner
                //   literals differ in length.
                // - A SIZED-INTEGER element (`Array<UInt8> = [65, 66]`) inferred
                //   `Int` from the literal and emitted `[2 x i64]`, which the
                //   consumer then read at the declared width: `store { ptr, i64,
                //   i64 } %t1` against an `[2 x i64]` for `Array<T>`, and
                //   `extractvalue [2 x i8] %t1` for `SmallArray<T, N>`. Both are
                //   clang errors — `vyrn run` and the direct wasm backend printed
                //   `65`, native did not build at all.
                // - A VALIDATED element (`Array<Age>`, RFC-0020) inferred `Int` too,
                //   and there the failure was SILENT rather than loud: the reshape
                //   below reinterprets the buffer whenever `llt` matches, and
                //   `Age`'s `llt` IS `i64`, so no `where` predicate ran. `[20, 5]`
                //   into an `Array<Age>` trapped under the interpreter and under
                //   wasm and printed `20`/`5` natively.
                //
                // One expected type, coerced element-wise, answers all three: the
                // outer `ArrayN -> Array`/`SmallArray` step then has `fi == ti` and
                // is the pure reshape its comment already claims to be.
                // ...but an element type that IS an unsolved parameter names no
                // type at all. `Deque { front: [2, 1] }` reaches here with
                // `Array<T>` expected and `T` open, and building at that type
                // emits `[2 x void]` — invalid IR. The elements answer for it,
                // and the enclosing literal's `solve_param` reads `T` back off
                // the result. Only a BARE parameter, matching the checker: an
                // `Array<Array<T>>` field is refused there rather than built
                // here, because the inner literal would have to reach its heap
                // representation at a type this pass does not know yet.
                let elem_expect = elem_expect.filter(|t| !matches!(t, Type::Param(_)));
                let build = (|| -> Result<(String, Type), String> {
                    let (ety, first) = if let Some(ety) = elem_expect.clone() {
                        let (v0, v0t) = self.gen_expr(&elems[0])?;
                        let (v0, _) = self.coerce(v0, &v0t, &ety)?;
                        (ety, v0)
                    } else {
                        // Build the [N x T] value aggregate by inserting each element.
                        let (v0, ety) = self.gen_expr(&elems[0])?;
                        (ety, v0)
                    };
                    let ell = self.llt(&ety);
                    let aty = format!("[{} x {ell}]", elems.len());
                    let mut cur = self.fresh_tmp();
                    self.emit(format!("{cur} = insertvalue {aty} undef, {ell} {first}, 0"));
                    for (i, e) in elems.iter().enumerate().skip(1) {
                        let (v, vt) = self.gen_expr(e)?;
                        let (v, _) = self.coerce(v, &vt, &ety)?;
                        let next = self.fresh_tmp();
                        self.emit(format!("{next} = insertvalue {aty} {cur}, {ell} {v}, {i}"));
                        cur = next;
                    }
                    Ok((cur, Type::ArrayN(Box::new(ety), elems.len())))
                })();
                if pushed {
                    self.expect.pop();
                }
                build
            }
            // A map literal (RFC-0028): `[:]` is the empty `{ptr,ptr,len,cap,idx}`
            // (buffers null, value type from context — the representation is
            // type-independent). A non-empty literal builds the map in a temp
            // slot via the same insert-or-update path as `m[k] = v`, so a
            // repeated key updates in place (keeps its slot), matching the
            // interpreter. The value type is inferred from the first value; a
            // validated declared type re-validates through the binding coercion.
            Expr::MapLit { entries, .. } => {
                if entries.is_empty() {
                    // The representation is type-independent — five nulls
                    // whatever the keys — so the TYPE answers the boundary's
                    // expectation where one names a Map. The direct backend
                    // already answered the declared type here, and RFC-0101's
                    // corpus gate holds the two backends to one answer
                    // (RFC-0117 M2 is what made the difference visible: a
                    // user-keyed `[:]` is not `Map<String, Int64>`).
                    let ty = match self.expect.last() {
                        Some(t) if matches!(self.resolve(t), Type::Map(..)) => t.clone(),
                        _ => Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
                    };
                    return Ok(("{ ptr null, ptr null, i64 0, i64 0, ptr null }".into(), ty));
                }
                let slot = self.fresh_alloca("{ ptr, ptr, i64, i64, ptr }");
                self.emit(format!(
                    "store {{ ptr, ptr, i64, i64, ptr }} {{ ptr null, ptr null, i64 0, i64 0, ptr null }}, ptr {slot}"
                ));
                // RFC-0037: derive each value's expected type from the enclosing
                // storage boundary's Map type, if any.
                let val_expect: Option<Type> =
                    self.expect.last().and_then(|t| match self.resolve(t) {
                        Type::Map(_, v) => Some(*v),
                        _ => None,
                    });
                let pushed = val_expect.is_some();
                if let Some(t) = &val_expect {
                    self.expect.push(t.clone());
                }
                // A value type that IS an unsolved parameter names no type (see
                // the array literal above) — the first value answers.
                let val_expect = val_expect.filter(|t| !matches!(t, Type::Param(_)));
                let build = (|| -> Result<(Type, Type), String> {
                    let (kv0, kty0) = self.gen_expr(&entries[0].0)?;
                    // The first key's generated type is the map's key type
                    // (RFC-0117: `String` or `Int64` — the checker made every
                    // key the same one).
                    let kty = kty0;
                    let (v0, vty0) = self.gen_expr(&entries[0].1)?;
                    // Store values at the DECLARED value type when the boundary
                    // supplies one, coercing each into it — otherwise a value that
                    // lowers as a fixed `[N x T]` (a nested array literal like
                    // `[[5],[6,7]]`) would be stored at the wrong width and read
                    // back as a corrupt `{ptr,len,cap}`. No annotation => infer
                    // from the first value (a no-op coercion, byte-identical).
                    let val = val_expect.clone().unwrap_or_else(|| vty0.clone());
                    let (v0, _) = self.coerce(v0, &vty0, &val)?;
                    // A repeated key updates in place, so the value it shadows
                    // has no owner left — `["usd": 1, "usd": 3]`. Inside a
                    // `region` nothing goes back: the arena owns it.
                    let drop_old = self.region_depth == 0;
                    self.emit_map_set(&slot, &kv0, &v0, &kty, &val, drop_old)?;
                    for (ke, ve) in entries.iter().skip(1) {
                        let (kv, _) = self.gen_expr(ke)?;
                        let (v, vt) = self.gen_expr(ve)?;
                        let (v, _) = self.coerce(v, &vt, &val)?;
                        self.emit_map_set(&slot, &kv, &v, &kty, &val, drop_old)?;
                    }
                    Ok((kty, val))
                })();
                if pushed {
                    self.expect.pop();
                }
                let (kty, val) = build?;
                let agg = self.fresh_tmp();
                self.emit(format!(
                    "{agg} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
                ));
                Ok((agg, Type::Map(Box::new(kty), Box::new(val))))
            }
            // A lambda literal in a v1 argument position is monomorphized away
            // at the call site that receives it (RFC-0023); one reaching the
            // general expression path is an RFC-0037 storage source — lift it
            // and construct its defunctionalized enum value, typed by the
            // innermost storage boundary's expected fn type.
            Expr::Lambda { .. } => self.construct_fnval_lambda(expr),
        }
    }

    /// Fallible validated construction `Age?(n)` → `Option<Age>` (`{ i1, i64 }`):
    /// tag is the refinement result, payload is the value.
    ///
    /// `Int64` and `String` are the two bases, which is what the direct backend
    /// already accepted (its rule is "any scalar", and those are the two scalars
    /// a validated type is written over in this corpus). A `String` payload is
    /// its pointer, the same word a `Some(s)` carries — RFC-0098 needed it,
    /// because an option type over `String` is what a command line hands you.
    fn gen_try_construct(&mut self, name: &str, arg: &Expr) -> Result<(String, Type), String> {
        let decl = self
            .types
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown type `{name}`"))?;
        let base = self.resolve(&decl.base);
        if base != Type::Int && base != Type::Str {
            return Err(format!(
                "native fallible construction supports Int64-based and String-based types only (`{name}`); use `vyrn run`"
            ));
        }
        let (v, _) = self.gen_expr(arg)?;
        let base_ll = self.llt(&decl.base);
        let pred_i1 = match self.predicate(&decl)? {
            None => "true".to_string(),
            Some(pred) => {
                self.scope.push(Vec::new());
                let slot = self.declare("value", &decl.base);
                self.emit(format!("store {base_ll} {v}, ptr {slot}"));
                let was = crate::observe::set_ctx("pred");
                let cond = self.gen_expr(pred);
                crate::observe::set_ctx(was);
                let (cond, _) = cond?;
                self.scope.pop();
                cond
            }
        };
        // The payload is one word: the Int itself, or the String's pointer.
        let word = if base == Type::Str {
            let w = self.fresh_tmp();
            self.emit(format!("{w} = ptrtoint ptr {v} to i64"));
            w
        } else {
            v
        };
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {pred_i1}, 0"
        ));
        self.emit(format!(
            "{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {word}, 1"
        ));
        self.emit(format!(
            "{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 0, 2"
        ));
        Ok((c, Type::Option(Box::new(Type::Named(name.to_string())))))
    }

    /// Build a record value (`insertvalue` per field, in declared field order).
    fn gen_struct_lit(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
    ) -> Result<(String, Type), String> {
        // Field types as declared (may contain this type's generic parameters).
        let rfields = self
            .record_fields(&Type::Named(name.to_string()))
            .ok_or_else(|| format!("`{name}` is not a record type"))?;

        // Emit each field value in declared order; infer generic parameters.
        //
        // The type this literal is BUILT FOR comes first, when the site names it.
        // Solving from the field VALUES alone cannot see through a `fn`-typed
        // field: `Deferred<P, T> = { run: fn(P) -> T }` gives the value the still
        // open `fn(P) -> T` as its expected type, the value registers its
        // RFC-0037 variant against that signature, and the dispatcher the call
        // site reaches is keyed on `fn(Int64) -> String` — which has no arm for
        // it. Every carrier of a `fn` under a parameter has the same hole: an
        // `Option<fn(P) -> T>` field, an `Array<fn(P) -> T>` field, a nested
        // generic record. One seed closes all of them, because the parameters are
        // known before the first field is emitted.
        // Substituted, not resolved: `resolve` takes an `App` all the way to the
        // `Record` it stands for, and the arguments are the whole point here.
        let want = self
            .expect
            .last()
            .map(|t| vyrn_frontend::types::substitute(t, self.subst));
        let mut solved = expected_type_args(want.as_ref(), name, self.types.get(name));
        let mut vals: Vec<(String, Type)> = Vec::new();
        for decl_f in &rfields {
            let (_, value_expr) = fields
                .iter()
                .find(|(fname, _)| fname == &decl_f.name)
                .ok_or_else(|| format!("missing field `{}`", decl_f.name))?;
            // RFC-0037: the declared field type (already substituted where the
            // enclosing solve got there) is a lambda field's expected type.
            self.expect
                .push(vyrn_frontend::types::substitute(&decl_f.ty, &solved));
            let r = self.gen_expr(value_expr);
            self.expect.pop();
            let (v, vty) = r?;
            if settles_type_args(value_expr) {
                solve_param(&decl_f.ty, &vty, &mut solved);
            }
            vals.push((v, vty));
        }

        // The concrete result type (generic parameters filled in), by the same
        // shared rule an enum variant's construction uses. `solved` above is the
        // incremental form of it, kept because each field's own type needs it
        // while the fields are still being emitted.
        //
        // The actual types are the DECLARED ones under `solved`, not the values'
        // own. Those two agree everywhere the values are the only source. Where
        // the site's expectation seeded a parameter they can differ — a field
        // given a wider record than the expected instantiation declares — and
        // then the values' answer is the wrong one: each field is inserted at its
        // `solved` type below, so the aggregate must be the same type or the
        // `insertvalue` is invalid IR.
        let result_ty = applied_type(
            self.types.get(name),
            name,
            &rfields.iter().map(|f| f.ty.clone()).collect::<Vec<_>>(),
            &rfields
                .iter()
                .map(|f| vyrn_frontend::types::substitute(&f.ty, &solved))
                .collect::<Vec<_>>(),
        );
        let ll = self.llt(&result_ty);

        let mut cur = "undef".to_string();
        let mut coerced: Vec<(String, String, Type)> = Vec::new();
        for (i, decl_f) in rfields.iter().enumerate() {
            let (v, vty) = vals[i].clone();
            let field_ty = vyrn_frontend::types::substitute(&decl_f.ty, &solved);
            // The field's source expression (for the RFC-0020 containment skip).
            let field_expr = fields
                .iter()
                .find(|(fname, _)| fname == &decl_f.name)
                .map(|(_, e)| e);
            let (v, _) = match field_expr {
                Some(e) => self.coerce_flow(v, e, &vty, &field_ty)?,
                None => self.coerce(v, &vty, &field_ty)?,
            };
            let field_ll = self.llt(&field_ty);
            let ins = self.fresh_tmp();
            self.emit(format!(
                "{ins} = insertvalue {ll} {cur}, {field_ll} {v}, {i}"
            ));
            cur = ins;
            coerced.push((decl_f.name.clone(), v, field_ty));
        }

        // Enforce a cross-field `where` invariant at runtime. As with scalar
        // construction, an all-constant literal is validated by the checker and
        // needs no runtime check.
        if let Some(decl) = self.types.get(name).cloned() {
            if let Some(pred) = self.predicate(&decl)? {
                let all_const = fields
                    .iter()
                    .all(|(_, e)| vyrn_frontend::consteval::eval(e, &HashMap::new()).is_some());
                if !all_const {
                    self.scope.push(Vec::new());
                    for (fname, v, fty) in &coerced {
                        let slot = self.declare(fname, fty);
                        let fll = self.llt(fty);
                        self.emit(format!("store {fll} {v}, ptr {slot}"));
                    }
                    let (cond, _) = self.gen_expr(pred)?;
                    self.scope.pop();
                    let nok = self.fresh_tmp();
                    self.emit(format!("{nok} = xor i1 {cond}, true"));
                    self.trap_if(&nok, &format!("@.trap.verr.{name}"), "rfail");
                }
            }
        }
        Ok((cur, result_ty))
    }

    /// Lower a `match`, releasing the scrutinee where `own` says the match is its
    /// last owner.
    ///
    /// The release is the `if let` release one construct over (`Stmt::IfLet`
    /// above): the scrutinee goes into a slot and the slot onto a drop frame of
    /// its own, so an arm that returns reclaims it through `emit_all_drops` and
    /// the fall-through reclaims it here. A row exists only where nothing took
    /// the scrutinee — an arm that hands its payload out marks the row, and then
    /// the binding the payload flowed into is the one owner there is.
    fn gen_match(
        &mut self,
        key: usize,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> Result<(String, Type), String> {
        let (sv, sty) = self.gen_expr(scrutinee)?;
        let scrut_drop = self.droppable.get(&key).cloned();
        // A CONSUMED scrutinee with no drop row: the arms take the payloads,
        // and nothing else will ever see the enum value again — so the boxes
        // the payloads travelled in are the match's to free, per arm, after
        // extraction (exit-residue round eight: `keyed`'s `match consume
        // node` rebuilt the node from its binders and left all three of
        // `El`'s boxes behind, once per keyed row). Where a drop row EXISTS
        // the fall-through release walks the boxes instead, and freeing them
        // here too would be the double.
        // A MAP lookup's `Option` box is a fresh allocation per call even
        // though `m[k]` spells a place — the payload SHARES the map's value
        // storage, so only the box is the match's to free, which is exactly
        // what `free_boxes` frees (exit-residue round forty-two: one 24-byte
        // box per matched lookup, fieldmut's litMap and mapdemo's tables).
        let map_lookup = matches!(scrutinee, Expr::Call { name, args, .. }
            if name == "@at"
                && args.first().and_then(|a| self.static_ty(a)).is_some_and(
                    |t| matches!(self.resolve(&t), Type::Map(..))));
        let free_boxes = (matches!(scrutinee, Expr::Consume { .. })
            || map_lookup
            || (vyrn_frontend::movecheck::place_path(scrutinee).is_none()
                && vyrn_frontend::movecheck::element_path(scrutinee).is_none())
            // Round twenty-seven: a PLACE scrutinee the fold proved nobody
            // reads after this match — the binding's row is Aliased and never
            // released, the alias owns the payload, and the box is this
            // match's to free.
            || self.plan.match_consumes(key))
            && scrut_drop.is_none()
            // Inside a declared `release` the CALLER walks the boxes after
            // the call (`release_enum`, payloads false) — freeing them here
            // too was a double on every declared-release destructure.
            // Inside a declared release the CALLER walks the RECEIVER's
            // boxes after the call — but only the receiver's. A scrutinee
            // rooted anywhere else (`let n = consume self.next` then `match
            // consume n` — the recursive Chain release, round forty-eight)
            // is this match's to free, exactly as outside (the caller cannot
            // see a local). Rootless scrutinees keep the conservative
            // stand-down.
            && !(self.owned.is_release_fn(&self.cur_fn_name)
                && match scrutinee {
                    Expr::Consume { place, .. } => {
                        vyrn_frontend::movecheck::place_path(place)
                            .is_none_or(|(root, _)| root == "self")
                    }
                    _ => true,
                });
        if let Some(kind) = scrut_drop {
            let ll = self.llt(&self.resolve(&sty)).clone();
            let slot = self.fresh_alloca(&ll);
            self.emit(format!("store {ll} {sv}, ptr {slot}"));
            self.register_drop(key, slot, kind);
        }
        // RFC-0114 Rule N at a match join: the arms that still own what
        // another arm consumed, keyed by this match expression's address.
        let ers = self.plan.edge_releases_at(key).cloned().unwrap_or_default();
        let r = self.gen_match_body_boxed(&sv, &sty, arms, &ers, free_boxes, key);
        if !self.terminated {
            self.emit_releases(ExitKind::Scrutinee, key);
        }
        r
    }

    /// Lower a `match` over an Option/Result to a tag test + `phi`. Payloads are
    /// i64 (native restriction), so bindings are i64 locals. The `Some`/`Ok` arm
    /// has tag 1; the `None`/`Err` arm has tag 0.
    fn gen_match_body_boxed(
        &mut self,
        sv: &str,
        sty: &Type,
        arms: &[MatchArm],
        ers: &[(String, u32)],
        free_boxes: bool,
        key: usize,
    ) -> Result<(String, Type), String> {
        let sv = sv.to_string();
        let sty = sty.clone();
        // A user enum dispatches to the switch-based path.
        if let Type::Enum(evs) = self.resolve(&sty) {
            return self.gen_match_enum(&sv, &evs, arms, ers, free_boxes, key);
        }
        // The payload type carried by each arm: for Option<T> the one-arm binds
        // `T`; for Result<T, E> the one-arm binds `T` and the zero-arm binds `E`.
        let (one_ty, zero_ty) = match self.resolve(&sty) {
            Type::Option(inner) => (*inner, Type::Int),
            Type::Result(ok, err) => (*ok, *err),
            _ => (Type::Int, Type::Int),
        };
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {sv}, 0"));
        let one_l = self.fresh_label("m.one");
        let zero_l = self.fresh_label("m.zero");
        let end_l = self.fresh_label("m.end");
        self.emit_term(format!("br i1 {tag}, label %{one_l}, label %{zero_l}"));

        // tag == 1 arm (Some / Ok)
        self.emit_label(&one_l);
        let one_ix = arms
            .iter()
            .position(|a| pattern_is_one(&a.pattern))
            .unwrap();
        let one_pf = self.plan.arm_payload_free(key, one_ix as u32).cloned();
        let (one_val, one_t) =
            self.gen_arm_body(&sv, &arms[one_ix], &one_ty, free_boxes, one_pf)?;
        if !self.terminated {
            self.emit_edge_releases(ers, one_ix as u32);
        }
        let one_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // tag == 0 arm (None / Err)
        self.emit_label(&zero_l);
        let zero_ix = arms
            .iter()
            .position(|a| !pattern_is_one(&a.pattern))
            .unwrap();
        let zero_pf = self.plan.arm_payload_free(key, zero_ix as u32).cloned();
        let (zero_val, zero_t) =
            self.gen_arm_body(&sv, &arms[zero_ix], &zero_ty, free_boxes, zero_pf)?;
        if !self.terminated {
            self.emit_edge_releases(ers, zero_ix as u32);
        }
        // Any block arm (RFC-0118) makes this a statement match — Unit, so
        // the void path below skips the phi a valueless edge could not feed.
        let ty = if arms.iter().any(|a| matches!(a.body, ArmBody::Block(_))) {
            Type::Unit
        } else {
            join_never(one_t, zero_t)
        };
        let zero_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // merge — a statement-position match with Unit arms (side effects only)
        // has no value to merge, and `phi void` is invalid IR.
        self.emit_label(&end_l);
        let ll = self.llt(&ty);
        if ll == "void" {
            return Ok((void_merge_value(&ty), ty));
        }
        let res = self.fresh_tmp();
        self.emit(format!(
            "{res} = phi {ll} [ {one_val}, %{one_end} ], [ {zero_val}, %{zero_end} ]"
        ));
        Ok((res, ty))
    }

    /// Lower an `if` used as an expression (RFC-0030) to the same branch+`phi`
    /// merge as a two-arm boolean `match`: evaluate the condition, branch to the
    /// taken side (only that branch's code runs), then `phi` the two branch
    /// values at the join. A `void`-typed result (both branches Unit, side
    /// effects only) skips the merge, exactly like `gen_match`. The checker
    /// guarantees `else_branch` is present.
    fn gen_if_expr(
        &mut self,
        key: usize,
        cond: &Expr,
        then_branch: &Expr,
        else_branch: Option<&Expr>,
    ) -> Result<(String, Type), String> {
        let else_branch =
            else_branch.ok_or("internal: `if` expression without `else` reached codegen")?;
        // RFC-0114 Rule N at an `if`-expression join.
        let ers = self.plan.edge_releases_at(key).cloned().unwrap_or_default();
        let (c, _) = self.gen_expr(cond)?;
        let then_l = self.fresh_label("ie.then");
        let else_l = self.fresh_label("ie.else");
        let end_l = self.fresh_label("ie.end");
        self.emit_term(format!("br i1 {c}, label %{then_l}, label %{else_l}"));

        // then branch
        self.emit_label(&then_l);
        let (then_val, then_t) = self.gen_expr(then_branch)?;
        if !self.terminated {
            self.emit_edge_releases(&ers, 0);
        }
        // The predecessor of the join is the CURRENT block — a nested if/match in
        // the branch body may have moved us past `then_l`.
        let then_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // else branch
        self.emit_label(&else_l);
        let (else_val, else_t) = self.gen_expr(else_branch)?;
        if !self.terminated {
            self.emit_edge_releases(&ers, 1);
        }
        let ty = join_never(then_t, else_t);
        let else_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // merge
        self.emit_label(&end_l);
        let ll = self.llt(&ty);
        if ll == "void" {
            return Ok((void_merge_value(&ty), ty));
        }
        let res = self.fresh_tmp();
        self.emit(format!(
            "{res} = phi {ll} [ {then_val}, %{then_end} ], [ {else_val}, %{else_end} ]"
        ));
        Ok((res, ty))
    }

    /// Lower a `match` over a user enum to a `switch` on the tag + `phi`. Payloads
    /// are i64; a binding arm loads the payload as an i64 local.
    fn gen_match_enum(
        &mut self,
        sv: &str,
        evs: &[EnumVariant],
        arms: &[MatchArm],
        ers: &[(String, u32)],
        free_boxes: bool,
        key: usize,
    ) -> Result<(String, Type), String> {
        let arity = evs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
        let ell = enum_ll(arity);
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ell} {sv}, 0"));
        let end_l = self.fresh_label("me.end");
        let default_l = self.fresh_label("me.default");

        // One block per arm; map each arm to its variant's tag index. The
        // refutable-`let` desugar's default arm (RFC-0121) carries no tag: it
        // IS the switch's default block, where an exhaustive match keeps an
        // unreachable one.
        let mut arm_labels: Vec<(Option<usize>, String)> = Vec::new();
        for arm in arms {
            let idx = match &arm.pattern {
                Pattern::Variant(n, _) => Some(
                    evs.iter()
                        .position(|v| &v.name == n)
                        .ok_or_else(|| format!("unknown variant `{n}`"))?,
                ),
                Pattern::Other => None,
                _ => return Err("non-variant pattern in enum match".into()),
            };
            arm_labels.push((idx, self.fresh_label("me.arm")));
        }
        let cases: String = arm_labels
            .iter()
            .filter_map(|(idx, lbl)| Some(format!("i64 {}, label %{lbl}", (*idx)?)))
            .collect::<Vec<_>>()
            .join(" ");
        let switch_default = arm_labels
            .iter()
            .find(|(idx, _)| idx.is_none())
            .map(|(_, lbl)| lbl.clone())
            .unwrap_or_else(|| default_l.clone());
        self.emit_term(format!(
            "switch i64 {tag}, label %{switch_default} [ {cases} ]"
        ));

        let mut incoming: Vec<(String, String)> = Vec::new();
        // Seeded `Never`, not `Unit`: a match whose EVERY arm diverges is itself
        // divergent, and the first arm that answers overwrites this on the next
        // line down. Seeding `Unit` reported "no value" for a match that is in
        // value position, which is how an enclosing `phi` got an empty operand.
        let mut ty = Type::Never;
        for (arm_ix, (arm, (idx, lbl))) in arms.iter().zip(&arm_labels).enumerate() {
            self.emit_label(lbl);
            self.scope.push(Vec::new());
            let mut bind_slots: Vec<(String, String)> = Vec::new();
            if let (Pattern::Variant(_, binds), Some(idx)) = (&arm.pattern, idx) {
                let payload_tys = &evs[*idx].payload;
                for (i, bind) in binds.iter().enumerate() {
                    let pty = payload_tys.get(i).cloned().unwrap_or(Type::Int);
                    let raw = self.fresh_tmp();
                    self.emit(format!("{raw} = extractvalue {ell} {sv}, {}", i + 1));
                    let v = self.unbox_payload(&raw, &pty);
                    let ll = self.llt(&pty);
                    let slot = self.declare(bind, &pty);
                    self.emit(format!("store {ll} {v}, ptr {slot}"));
                    bind_slots.push((bind.clone(), slot.clone()));
                    // A consumed scrutinee's boxes are this match's to give
                    // back once the value is out — see `gen_match`'s note.
                    if free_boxes && v != raw {
                        let q = self.fresh_tmp();
                        self.emit(format!("{q} = inttoptr i64 {raw} to ptr"));
                        self.emit(format!("call void @__vyrn_free(ptr {q})"));
                    }
                }
            }
            let (v, t) = match &arm.body {
                ArmBody::Expr(e) => self.gen_expr(e)?,
                // A block arm (RFC-0118) is its statements and yields nothing;
                // `has_block` below forces the void merge, so the empty value
                // never reaches a `phi`.
                ArmBody::Block(b) => {
                    self.gen_block(b)?;
                    (String::new(), Type::Unit)
                }
            };
            // Reconcile the arms' reported type. All arms share one enum (the
            // checker proved it), but different arms carry different knowledge of
            // its type arguments: a payload-bearing arm that mentions the param
            // (`Loaded(v)` → `LoadResult<StoreFile>`) is fully applied, while a
            // nullary/param-free arm (`Missing`, `Corrupt([Issue])`) resolves the
            // param to `Unit`. Prefer the fully-applied instantiation so a
            // downstream `match` on this expression recovers the concrete payload
            // type instead of the bare `Type::Param` (which lowers to an invalid
            // `alloca void`). Every instantiation shares one LLVM layout, so this
            // never disturbs the `phi` below — only the reported type.
            // A `panic` arm (RFC-0079) reports `Never` and answers nothing: it
            // is `poison` in the `phi`, and the arms that produce a value decide
            // the type.
            if !matches!(t, Type::Never)
                && !(self.ty_is_concrete_app(&ty) && !self.ty_is_concrete_app(&t))
            {
                ty = t;
            }
            // Round forty: the unmoved payload binders the row names — see
            // `gen_arm_body`.
            if let Some(rows) = self.plan.arm_payload_free(key, arm_ix as u32).cloned() {
                for (bind, slot) in &bind_slots {
                    let Some((_, kind, holes)) = rows.iter().find(|(n, _, _)| n == bind) else {
                        continue;
                    };
                    if !self.terminated && self.region_depth == 0 {
                        self.emit_drop_holed(slot, kind, holes.clone());
                    }
                }
            }
            self.scope.pop();
            if !self.terminated {
                self.emit_edge_releases(ers, arm_ix as u32);
            }
            let block = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            incoming.push((v, block));
        }

        // Exhaustiveness is checked, so the default is unreachable.
        self.emit_label(&default_l);
        self.emit_term("unreachable".into());

        self.emit_label(&end_l);
        // Any block arm (RFC-0118) makes this a statement match: whatever the
        // expression arms beside it computed is discarded, the type is Unit,
        // and the void path below skips the phi a valueless edge could not
        // feed.
        if arms.iter().any(|a| matches!(a.body, ArmBody::Block(_))) {
            ty = Type::Unit;
        }
        let ll = self.llt(&ty);
        // Unit-typed arms (side effects only) have no value — `phi void` is
        // invalid IR, so skip the merge entirely.
        if ll == "void" {
            return Ok((void_merge_value(&ty), ty));
        }
        let res = self.fresh_tmp();
        let phi = incoming
            .iter()
            .map(|(v, b)| format!("[ {v}, %{b} ]"))
            .collect::<Vec<_>>()
            .join(", ");
        self.emit(format!("{res} = phi {ll} {phi}"));
        Ok((res, ty))
    }

    /// One element from a stream: `(has, slot)` — an `i1` saying whether there
    /// was one, and the slot it was staged in (RFC-0075 M2c).
    ///
    /// Both readers of a stream go through here — `for … in` below, and `pull`,
    /// which is what a lazy combinator's step is written in terms of. The two
    /// asked the same two questions in two spellings until M2c needed the second
    /// reader; the buffer arm's cursor advance and the producer arm's "a stream
    /// that ended stays ended" latch are exactly the kind of agreement that
    /// stops being true in one of two copies.
    ///
    /// It answers a staged element rather than an `Option<T>` on purpose. An
    /// `Option` payload wider than a word is boxed, so a shared emitter that
    /// answered one would have put a `malloc` in every `for x in fromArray(rs)`
    /// over a record — a per-element box, never reclaimed, in the loop this RFC
    /// exists to keep flat. `pull` builds the `Option` its own signature demands
    /// and pays for it exactly there.
    ///
    /// `sslot` is the header's ADDRESS, because answering writes to it.
    fn emit_stream_next(&mut self, sslot: &str, elem: &Type) -> Result<(String, String), String> {
        let ell = self.llt(elem);
        let optll = self.llt(&Type::Option(Box::new(elem.clone())));
        let stage = self.fresh_alloca(&ell);

        let buf_l = self.fresh_label("nbuf");
        let take_l = self.fresh_label("nbuftake");
        let step_l = self.fresh_label("nstep");
        let call_l = self.fresh_label("ncall");
        let some_l = self.fresh_label("nsome");
        let ended_l = self.fresh_label("nended");
        let empty_l = self.fresh_label("nempty");
        let done_l = self.fresh_label("ndone");

        let fld = |i: usize| format!("i64 0, i32 {i}");
        // Which producer is this? A negative tag is a buffer.
        let tp = self.fresh_tmp();
        let tag = self.fresh_tmp();
        let isbuf = self.fresh_tmp();
        self.emit(format!(
            "{tp} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(2)
        ));
        self.emit(format!("{tag} = load i64, ptr {tp}"));
        self.emit(format!("{isbuf} = icmp slt i64 {tag}, 0"));
        self.emit_term(format!("br i1 {isbuf}, label %{buf_l}, label %{step_l}"));

        // buffer: cursor < len yields data[cursor] and steps the cursor.
        self.emit_label(&buf_l);
        let cp = self.fresh_tmp();
        let lp = self.fresh_tmp();
        let i = self.fresh_tmp();
        let n = self.fresh_tmp();
        let more = self.fresh_tmp();
        self.emit(format!(
            "{cp} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(4)
        ));
        self.emit(format!(
            "{lp} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(1)
        ));
        self.emit(format!("{i} = load i64, ptr {cp}"));
        self.emit(format!("{n} = load i64, ptr {lp}"));
        self.emit(format!("{more} = icmp ult i64 {i}, {n}"));
        self.emit_term(format!("br i1 {more}, label %{take_l}, label %{empty_l}"));
        self.emit_label(&take_l);
        let dp = self.fresh_tmp();
        let data = self.fresh_tmp();
        let ep = self.fresh_tmp();
        let ev = self.fresh_tmp();
        let i1 = self.fresh_tmp();
        self.emit(format!(
            "{dp} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(0)
        ));
        self.emit(format!("{data} = load ptr, ptr {dp}"));
        self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {i}"));
        self.emit(format!("{ev} = load {ell}, ptr {ep}"));
        self.emit(format!("{i1} = add i64 {i}, 1"));
        self.emit(format!("store i64 {i1}, ptr {cp}"));
        self.emit(format!("store {ell} {ev}, ptr {stage}"));
        self.emit_term(format!("br label %{done_l}"));

        // step: once the producer has said `None` it is never asked again — a
        // stream that ended stays ended, on every engine.
        self.emit_label(&step_l);
        let ep2 = self.fresh_tmp();
        let over = self.fresh_tmp();
        let isover = self.fresh_tmp();
        self.emit(format!(
            "{ep2} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(1)
        ));
        self.emit(format!("{over} = load i64, ptr {ep2}"));
        self.emit(format!("{isover} = icmp ne i64 {over}, 0"));
        self.emit_term(format!("br i1 {isover}, label %{empty_l}, label %{call_l}"));

        self.emit_label(&call_l);
        // `tag`/`pay` are an adjacent 8-aligned pair, which is why the fn value
        // loads whole rather than word by word. The cursor's two words are
        // separate arguments (RFC-0090 M3), so they load separately.
        let fp = self.fresh_tmp();
        let cp2 = self.fresh_tmp();
        let gp2 = self.fresh_tmp();
        let fv = self.fresh_tmp();
        let cw = self.fresh_tmp();
        let gw = self.fresh_tmp();
        self.emit(format!(
            "{fp} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(2)
        ));
        self.emit(format!(
            "{cp2} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(4)
        ));
        self.emit(format!(
            "{gp2} = getelementptr {STREAM_LL}, ptr {sslot}, {}",
            fld(5)
        ));
        self.emit(format!("{fv} = load {{ i64, i64 }}, ptr {fp}"));
        self.emit(format!("{cw} = load i64, ptr {cp2}"));
        self.emit(format!("{gw} = load i64, ptr {gp2}"));
        // The dispatcher is keyed on the step's signature, which is a function of
        // the ELEMENT type alone — the reason the cursor is two `Int64`s.
        let sig = self.normalize_sig(&stream_step_sig(elem));
        let sym = self.fnval_dispatcher_sym(&sig);
        let o = self.fresh_tmp();
        self.emit(format!(
            "{o} = call {optll} @{sym}({{ i64, i64 }} {fv}, i64 {cw}, i64 {gw}, i1 0)"
        ));
        let ot = self.fresh_tmp();
        self.emit(format!("{ot} = extractvalue {optll} {o}, 0"));
        self.emit_term(format!("br i1 {ot}, label %{some_l}, label %{ended_l}"));

        self.emit_label(&some_l);
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{w0} = extractvalue {optll} {o}, 1"));
        self.emit(format!("{w1} = extractvalue {optll} {o}, 2"));
        let v = self.decode_payload(&w0, &w1, elem);
        self.emit(format!("store {ell} {v}, ptr {stage}"));
        self.emit_term(format!("br label %{done_l}"));

        self.emit_label(&ended_l);
        self.emit(format!("store i64 1, ptr {ep2}"));
        self.emit_term(format!("br label %{empty_l}"));

        self.emit_label(&empty_l);
        self.emit_term(format!("br label %{done_l}"));

        self.emit_label(&done_l);
        let has = self.fresh_tmp();
        self.emit(format!(
            "{has} = phi i1 [ true, %{take_l} ], [ true, %{some_l} ], [ false, %{empty_l} ]"
        ));
        Ok((has, stage))
    }

    /// `for x in <stream>` — the pull loop (RFC-0075 M2b).
    ///
    /// The shape is `cond → ask → body → latch → cond`, and the difference from
    /// the indexed walk it replaces is that the producer runs inside the loop
    /// rather than before it. That is what makes `take(n)` over an endless feed
    /// allocate n: since M2c `take` is itself a producer, one that stops asking,
    /// and the loop leaves through `fend` the first time it is answered `None`.
    ///
    /// The header is spilled to a slot because answering WRITES it — the buffer
    /// arm advances the cursor, the step arm latches the end — and because the
    /// release then has an address the ordinary drop machinery can reach, which
    /// is how an early `return` out of the body releases it (M1's arrangement,
    /// unchanged apart from the kind).
    fn gen_for_stream(
        &mut self,
        var: &str,
        body: &Block,
        av: &str,
        elem: &Type,
    ) -> Result<(), String> {
        let ell = self.llt(elem);
        let sslot = self.fresh_alloca(STREAM_LL);
        self.stream_slots.insert(sslot.clone(), elem.clone());
        self.emit(format!("store {STREAM_LL} {av}, ptr {sslot}"));

        let cond_l = self.fresh_label("fcond");
        let body_l = self.fresh_label("fbody");
        let latch_l = self.fresh_label("flatch");
        let end_l = self.fresh_label("fend");

        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let (has, stage) = self.emit_stream_next(&sslot, elem)?;
        self.emit_term(format!("br i1 {has}, label %{body_l}, label %{end_l}"));

        // body: identical to the indexed walk's, including the drop frame that
        // carries the release out through an early `return`.
        self.emit_label(&body_l);
        let staged = self.fresh_tmp();
        self.emit(format!("{staged} = load {ell}, ptr {stage}"));
        self.scope.push(Vec::new());
        // The one entry the placement has nothing for — see [`Gen::cursors`].
        self.cursors.push((sslot.clone(), self.drop_seq));
        let vslot = self.declare(var, elem);
        self.emit(format!("store {ell} {staged}, ptr {vslot}"));
        self.loop_ctx.push(LoopCtx {
            break_label: end_l.clone(),
            continue_label: latch_l.clone(),
            region_depth: self.region_depth,
        });
        self.gen_block(body)?;
        self.loop_ctx.pop();
        self.cursors.pop();
        self.scope.pop();
        if !self.terminated {
            self.emit_term(format!("br label %{latch_l}"));
        }

        self.emit_label(&latch_l);
        self.emit_term(format!("br label %{cond_l}"));

        // Normal end and `break` both land here, still owning the stream.
        self.emit_label(&end_l);
        self.emit_drop(&sslot, &DropKind::CloseStream);
        Ok(())
    }

    /// Build a `Stream<T>` header value out of its six words (RFC-0075 M2b).
    /// `data` arrives already typed (`ptr null` or `ptr %t`) because it is the
    /// one field that is not an `i64`.
    fn stream_header(
        &mut self,
        data: &str,
        len: &str,
        tag: &str,
        pay: &str,
        cur: &str,
        gen: &str,
    ) -> String {
        let mut cur_v = "undef".to_string();
        for (i, w) in [
            data.to_string(),
            format!("i64 {len}"),
            format!("i64 {tag}"),
            format!("i64 {pay}"),
            format!("i64 {cur}"),
            format!("i64 {gen}"),
        ]
        .into_iter()
        .enumerate()
        {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = insertvalue {STREAM_LL} {cur_v}, {w}, {i}"));
            cur_v = t;
        }
        cur_v
    }

    /// Encode a payload of type `ty` into the single `i64` slot that *enum*
    /// aggregates carry. Values that fit in a word (`Int`) pass through; wider
    /// values (`Ref`, `String`, records) are boxed on the heap and represented by
    /// their pointer. (The box is not reclaimed — a safe leak.)
    fn box_payload(&mut self, v: &str, ty: &Type) -> String {
        let ll = self.llt(ty);
        if ll == "i64" {
            return v.to_string();
        }
        let size = self.fresh_tmp();
        let p = self.fresh_tmp();
        self.emit(format!(
            "{size} = ptrtoint ptr getelementptr ({ll}, ptr null, i64 1) to i64"
        ));
        self.emit(format!("{p} = call ptr @__vyrn_malloc(i64 {size})"));
        self.emit(format!("store {ll} {v}, ptr {p}"));
        let iv = self.fresh_tmp();
        self.emit(format!("{iv} = ptrtoint ptr {p} to i64"));
        iv
    }

    /// Decode an enum's `i64` payload slot back into a value of type `ty`.
    fn unbox_payload(&mut self, slot: &str, ty: &Type) -> String {
        let ll = self.llt(ty);
        if ll == "i64" {
            return slot.to_string();
        }
        let p = self.fresh_tmp();
        let v = self.fresh_tmp();
        self.emit(format!("{p} = inttoptr i64 {slot} to ptr"));
        self.emit(format!("{v} = load {ll}, ptr {p}"));
        v
    }

    /// Coerce a `Some`/`Ok`/`Err` payload into the type the enclosing expectation
    /// asks for, BEFORE it is encoded into the aggregate's words. The checker
    /// already reports these constructors at the expected payload type (its
    /// `Some` arm returns `Option<want>`, not `Option<typeof x>`), so this is the
    /// runtime half of a coercion the frontend has already accepted — including
    /// the `where` predicate, which is why native alone used to build an
    /// `Option<Age>` out of a runtime `5` (RFC-0082 finding 7). The user-enum
    /// constructor below has always done this against its DECLARED payload types;
    /// the two built-in sums are the ones that never did.
    ///
    /// An unresolved type parameter is left alone, as in the enum path: the
    /// inline-monomorphized payload keeps the argument's own type.
    fn coerce_into_payload(
        &mut self,
        v: String,
        ty: Type,
        want: Option<&Type>,
    ) -> Result<(String, Type), String> {
        match want {
            Some(w) if !matches!(self.resolve(w), Type::Param(_)) => self.coerce(v, &ty, w),
            _ => Ok((v, ty)),
        }
    }

    /// Encode an Option/Result payload into the aggregate's two words `(w0, w1)`.
    /// A `Ref` (two words) fits inline with no heap box; scalars use `w0`; wider
    /// types (records/enums) are boxed and the pointer stored in `w0`.
    fn encode_payload(&mut self, v: &str, ty: &Type) -> (String, String) {
        match self.resolve(ty) {
            Type::Int => (v.to_string(), "0".into()),
            Type::Bool => {
                let w = self.fresh_tmp();
                self.emit(format!("{w} = zext i1 {v} to i64"));
                (w, "0".into())
            }
            Type::Str => {
                let w = self.fresh_tmp();
                self.emit(format!("{w} = ptrtoint ptr {v} to i64"));
                (w, "0".into())
            }
            // A stored function value (RFC-0037) is a two-word
            // `{ i64, i64 }` aggregate — it fits inline with no heap box.
            Type::Fn(..) => {
                let w0 = self.fresh_tmp();
                let w1 = self.fresh_tmp();
                self.emit(format!("{w0} = extractvalue {{ i64, i64 }} {v}, 0"));
                self.emit(format!("{w1} = extractvalue {{ i64, i64 }} {v}, 1"));
                (w0, w1)
            }
            _ => (self.box_payload(v, ty), "0".into()),
        }
    }

    /// Decode two Option/Result payload words back into a value of type `ty`.
    fn decode_payload(&mut self, w0: &str, w1: &str, ty: &Type) -> String {
        match self.resolve(ty) {
            Type::Int => w0.to_string(),
            Type::Bool => {
                let v = self.fresh_tmp();
                self.emit(format!("{v} = trunc i64 {w0} to i1"));
                v
            }
            Type::Str => {
                let v = self.fresh_tmp();
                self.emit(format!("{v} = inttoptr i64 {w0} to ptr"));
                v
            }
            Type::Fn(..) => {
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!(
                    "{a} = insertvalue {{ i64, i64 }} undef, i64 {w0}, 0"
                ));
                self.emit(format!("{b} = insertvalue {{ i64, i64 }} {a}, i64 {w1}, 1"));
                b
            }
            _ => self.unbox_payload(w0, ty),
        }
    }

    /// Emit an arm body, binding the payload (decoded to `payload_ty`) if the
    /// pattern binds a name.
    fn gen_arm_body(
        &mut self,
        sv: &str,
        arm: &MatchArm,
        payload_ty: &Type,
        free_boxes: bool,
        payload_free: Option<Vec<(String, DropKind, Vec<String>)>>,
    ) -> Result<(String, Type), String> {
        self.scope.push(Vec::new());
        let mut bind_slot: Option<(String, String)> = None;
        if let Some(bind) = pattern_binding(&arm.pattern) {
            let w0 = self.fresh_tmp();
            let w1 = self.fresh_tmp();
            self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {sv}, 1"));
            self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {sv}, 2"));
            let v = self.decode_payload(&w0, &w1, payload_ty);
            let ll = self.llt(payload_ty);
            let slot = self.declare(bind, payload_ty);
            self.emit(format!("store {ll} {v}, ptr {slot}"));
            bind_slot = Some((bind.to_string(), slot.clone()));
            // A TEMPORARY scrutinee with no drop row: the boxed payload's
            // block is this match's to give back once the value is out —
            // `readDoc`'s `match parseJson(src)` left one 16-byte Result box
            // per `fromJson` (exit-residue round thirteen; the enum path's
            // twin landed in round eight).
            if free_boxes
                && v != w0
                && !matches!(
                    self.resolve(payload_ty),
                    Type::Bool | Type::Str | Type::Fn(..)
                )
            {
                let q = self.fresh_tmp();
                self.emit(format!("{q} = inttoptr i64 {w0} to ptr"));
                self.emit(format!("call void @__vyrn_free(ptr {q})"));
            }
        }
        let out = match &arm.body {
            ArmBody::Expr(e) => self.gen_expr(e)?,
            // A block arm (RFC-0118): the statements, and no value — the
            // caller forces the void merge whenever one exists.
            ArmBody::Block(b) => {
                self.gen_block(b)?;
                (String::new(), Type::Unit)
            }
        };
        // Round forty: an unmoved payload binder in a match whose sibling
        // arm moved — the row went Moved for the mover's sake, the whole
        // release stood down, and this payload is the arm's to give back
        // once its body is done with it.
        if let (Some(rows), Some((bind, slot))) = (&payload_free, &bind_slot) {
            if let Some((_, kind, holes)) = rows.iter().find(|(n, _, _)| n == bind) {
                if !self.terminated && self.region_depth == 0 {
                    let (slot, kind, holes) = (slot.clone(), kind.clone(), holes.clone());
                    self.emit_drop_holed(&slot, &kind, holes);
                }
            }
        }
        self.scope.pop();
        Ok(out)
    }

    /// Emit the `i1` "does `sv` (of resolved type `sr`) match `pattern`" test for
    /// an `if let`/`while let` (RFC-0060). Shares the tag-extraction shape with
    /// `gen_match`/`gen_match_enum`.
    /// `if let Some(x) = s.tryAt(h)` (RFC-0122): lower an OPTIONAL projection
    /// where it is tested. Answers `false` when the scrutinee is not one, and
    /// the caller keeps the ordinary path.
    fn optional_if_let(
        &mut self,
        pattern: &Pattern,
        scrutinee: &Expr,
        then_block: &vyrn_frontend::ast::Block,
        else_block: &Option<vyrn_frontend::ast::Block>,
    ) -> Result<bool, String> {
        let Expr::Call { name, args, .. } = scrutinee else {
            return Ok(false);
        };
        if args.is_empty()
            || self.funcs.get(name.as_str()).is_some()
            || !self
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            return Ok(false);
        }
        let line = args[0].line();
        let recv = self.static_ty(&args[0]);
        let Some(p) = vyrn_frontend::project::optional_site(
            self.impls,
            recv.as_ref(),
            name,
            &args[0],
            &args[1..],
            line,
        )?
        else {
            return Ok(false);
        };
        let hit_l = self.fresh_label("op.hit");
        let miss_l = self.fresh_label("op.miss");
        let end_l = self.fresh_label("op.end");
        self.scope.push(Vec::new());
        for s in &p.prologue {
            self.gen_stmt(s)?;
        }
        let (mv, _) = self.gen_expr(&p.miss)?;
        self.emit_term(format!("br i1 {mv}, label %{miss_l}, label %{hit_l}"));

        self.emit_label(&hit_l);
        // The hit prologue (RFC-0123 M1): statements that run only when the
        // place exists, ahead of the read.
        for s in &p.hit {
            self.gen_stmt(s)?;
        }
        // The binder aliases the place — a handle copy, never drop-tracked,
        // exactly as a pattern payload binds.
        let (pv, pty) = self.gen_expr(&p.place)?;
        if let Pattern::Some(bind) = pattern {
            let ll = self.llt(&pty);
            let slot = self.declare(bind, &pty);
            self.emit(format!("store {ll} {pv}, ptr {slot}"));
        }
        self.gen_block(then_block)?;
        if !self.terminated {
            self.emit_term(format!("br label %{end_l}"));
        }

        self.emit_label(&miss_l);
        if let Some(eb) = else_block {
            self.gen_block(eb)?;
        }
        if !self.terminated {
            self.emit_term(format!("br label %{end_l}"));
        }
        self.emit_label(&end_l);
        self.scope.pop();
        Ok(true)
    }

    fn gen_pattern_test(
        &mut self,
        sv: &str,
        sr: &Type,
        pattern: &Pattern,
    ) -> Result<String, String> {
        match sr {
            Type::Enum(evs) => {
                let arity = evs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
                let ell = enum_ll(arity);
                let vname = match pattern {
                    Pattern::Variant(n, _) => n,
                    _ => return Err("non-variant pattern on an enum scrutinee".into()),
                };
                let idx = evs
                    .iter()
                    .position(|v| &v.name == vname)
                    .ok_or_else(|| format!("unknown variant `{vname}`"))?;
                let tag = self.fresh_tmp();
                self.emit(format!("{tag} = extractvalue {ell} {sv}, 0"));
                let m = self.fresh_tmp();
                self.emit(format!("{m} = icmp eq i64 {tag}, {idx}"));
                Ok(m)
            }
            Type::Option(_) | Type::Result(..) => {
                let tag = self.fresh_tmp();
                self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {sv}, 0"));
                // `Some`/`Ok` match tag 1; `None`/`Err` match tag 0.
                if pattern_is_one(pattern) {
                    Ok(tag)
                } else {
                    let n = self.fresh_tmp();
                    self.emit(format!("{n} = xor i1 {tag}, true"));
                    Ok(n)
                }
            }
            other => Err(format!(
                "if-let scrutinee is not an Option/Result/enum: {other:?}"
            )),
        }
    }

    /// After a successful `gen_pattern_test`, declare the pattern's binders in the
    /// current scope, decoding each payload from `sv` (RFC-0060).
    fn gen_pattern_binds(&mut self, sv: &str, sr: &Type, pattern: &Pattern) -> Result<(), String> {
        match sr {
            Type::Enum(evs) => {
                if let Pattern::Variant(vname, binds) = pattern {
                    let arity = evs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
                    let ell = enum_ll(arity);
                    let idx = evs
                        .iter()
                        .position(|v| &v.name == vname)
                        .ok_or_else(|| format!("unknown variant `{vname}`"))?;
                    let payload_tys = evs[idx].payload.clone();
                    for (i, bind) in binds.iter().enumerate() {
                        let pty = payload_tys.get(i).cloned().unwrap_or(Type::Int);
                        let raw = self.fresh_tmp();
                        self.emit(format!("{raw} = extractvalue {ell} {sv}, {}", i + 1));
                        let v = self.unbox_payload(&raw, &pty);
                        let ll = self.llt(&pty);
                        let slot = self.declare(bind, &pty);
                        self.emit(format!("store {ll} {v}, ptr {slot}"));
                    }
                }
                Ok(())
            }
            Type::Option(inner) => {
                if let Pattern::Some(bind) = pattern {
                    let pty = (**inner).clone();
                    self.bind_or_payload(sv, bind, &pty);
                }
                Ok(())
            }
            Type::Result(ok, err) => {
                match pattern {
                    Pattern::Ok(bind) => {
                        let pty = (**ok).clone();
                        self.bind_or_payload(sv, bind, &pty);
                    }
                    Pattern::Err(bind) => {
                        let pty = (**err).clone();
                        self.bind_or_payload(sv, bind, &pty);
                    }
                    _ => {}
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Decode an Option/Result payload from `sv`'s two words into a fresh binder
    /// (the `if let` counterpart of a `match` arm's binding).
    fn bind_or_payload(&mut self, sv: &str, bind: &str, pty: &Type) {
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {sv}, 1"));
        self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {sv}, 2"));
        let v = self.decode_payload(&w0, &w1, pty);
        let ll = self.llt(pty);
        let slot = self.declare(bind, pty);
        self.emit(format!("store {ll} {v}, ptr {slot}"));
    }

    /// Lower `expr?`: on `None`/`Err` (tag 0) return the aggregate as the
    /// function's result; otherwise continue with the unwrapped i64 payload.
    fn gen_try(&mut self, expr: &Expr, at: usize) -> Result<(String, Type), String> {
        let (agg, aty) = self.gen_expr(expr)?;
        if !matches!(self.resolve(&aty), Type::Option(_) | Type::Result(..)) {
            let place = vyrn_frontend::movecheck::place_path(expr).is_some()
                || vyrn_frontend::movecheck::element_path(expr).is_some();
            return self.gen_try_fallible(&agg, &aty, at, place);
        }
        // The type unwrapped on the success path.
        let ok_ty = match self.resolve(&aty) {
            Type::Option(inner) => *inner,
            Type::Result(ok, _) => *ok,
            _ => Type::Int,
        };
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {agg}, 0"));
        let ok_l = self.fresh_label("try.ok");
        let prop_l = self.fresh_label("try.prop");
        self.emit_term(format!("br i1 {tag}, label %{ok_l}, label %{prop_l}"));

        // propagate: the enclosing function returns Option/Result ({ i1, i64, i64 }).
        self.emit_label(&prop_l);
        // Free in-scope owned temporaries before the early return, exactly as
        // `return` does (the propagated aggregate never aliases one — a value
        // that escapes into it is not droppable by definition).
        self.emit_all_drops(ExitKind::Try, at);
        self.emit_modify_copyout();
        self.emit_term(format!("ret {{ i1, i64, i64 }} {agg}"));

        self.emit_label(&ok_l);
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {agg}, 1"));
        self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {agg}, 2"));
        let v = self.decode_payload(&w0, &w1, &ok_ty);
        // A boxed success payload (any type wider than a word) travelled in a
        // block the `?` is the last to see WHEN THE OPERAND IS A TEMPORARY:
        // the call's result is consumed here, no row anywhere names it, and
        // the value has just been loaded out. Free the box (exit-residue
        // round eleven: one 16-byte block per `parseValue(p, ..)?` — one per
        // parsed JSON value in the corpus). A PLACE operand (`r?` over a
        // binding) still owns its box — the binding's own release walks it —
        // and on the propagate path the whole aggregate travels on, box and
        // all.
        let operand_is_place = vyrn_frontend::movecheck::place_path(expr).is_some()
            || vyrn_frontend::movecheck::element_path(expr).is_some();
        if !operand_is_place
            && v != w0
            && !matches!(self.resolve(&ok_ty), Type::Bool | Type::Str | Type::Fn(..))
        {
            let q = self.fresh_tmp();
            self.emit(format!("{q} = inttoptr i64 {w0} to ptr"));
            self.emit(format!("call void @__vyrn_free(ptr {q})"));
        }
        Ok((v, ok_ty))
    }

    /// `?` on a type that implements `Fallible` (RFC-0080 M3). The operand is
    /// already evaluated; the shape is the same one `gen_try` writes above — test,
    /// propagate the WHOLE value, otherwise read the success payload — with the
    /// two questions answered by impl methods instead of by field 0 and an
    /// `extractvalue`.
    ///
    /// The value is parked in a scope slot rather than passed as a bare register,
    /// because there is no `Expr` to hand `gen_call` for a register that is
    /// already emitted — the same parking the receiver of a chained protocol call
    /// gets (`gen_call`'s protocol branch, RFC-0084 M2). That is
    /// also what lets the two calls reuse `gen_call` whole, including its generic
    /// path — a generic impl monomorphizes here with nothing new written. The slot
    /// is `declare`d, not registered on `drop_stack`, so the propagated aggregate
    /// is not freed out from under the `ret` (`gen_match_arm` binds payloads the
    /// same way for the same reason).
    fn gen_try_fallible(
        &mut self,
        agg: &str,
        aty: &Type,
        at: usize,
        operand_is_place: bool,
    ) -> Result<(String, Type), String> {
        let concrete = vyrn_frontend::types::substitute(aty, self.subst);
        let key = vyrn_frontend::types::type_key(&concrete)
            .ok_or_else(|| format!("`?` cannot dispatch on {aty:?}"))?;
        let tmp = format!("@try.{}", self.tmp);
        let slot = self.declare(&tmp, aty);
        self.emit(format!("store {} {agg}, ptr {slot}", self.llt(aty)));
        let recv = [Expr::Var { name: tmp, line: 0 }];
        let ask = |m: &str| {
            vyrn_frontend::types::impl_method_name(vyrn_frontend::types::FALLIBLE, &key, m)
        };

        let (ok, _) = self.gen_call(&ask("isSuccess"), &recv)?;
        let ok_l = self.fresh_label("try.ok");
        let prop_l = self.fresh_label("try.prop");
        self.emit_term(format!("br i1 {ok}, label %{ok_l}, label %{prop_l}"));

        self.emit_label(&prop_l);
        self.emit_all_drops(ExitKind::Try, at);
        self.emit_modify_copyout();
        self.emit_term(format!("ret {} {agg}", self.llt(aty)));

        self.emit_label(&ok_l);
        let r = self.gen_call(&ask("success"), &recv)?;
        // `success` COPIES its payload out (rule 3: an owned result contains
        // none of its read arguments), so a TEMPORARY operand is abandoned
        // here whole — box, payload and all (exit-residue round forty-one:
        // one Http per `fetch(code)?`). A place operand's binding still owns
        // it, and a type whose walk could call a declared release keeps its
        // user-visible timing.
        if !operand_is_place && self.region_depth == 0 && !self.owned.reaches_declared(&concrete) {
            if let Some(kind) = self.owned.release_kind(&concrete) {
                let slot = slot.clone();
                self.emit_drop(&slot, &kind);
            }
        }
        Ok(r)
    }

    /// `a + b` on Strings is `@concat` written as an operator, so its operands
    /// are call arguments and take the call-argument rule
    /// (`rfcs/census-call-arguments.md` §9, finding 3): `"n" + label(i)` reaches
    /// this lowering rather than [`Gen::gen_call`], so it was in neither that
    /// census's count nor RFC-0096 M3's operand class, and leaked the same 48
    /// bytes a turn.
    ///
    /// The mark is [`Gen::gen_call`]'s, for its reason: an operand that is
    /// itself a call takes back only what was pushed after its own mark. Every
    /// other operator reaches the drain with nothing pushed — `own` records a
    /// row only where the `+` builds a String.
    fn gen_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<(String, Type), String> {
        let mark = self.arg_frees.len();
        let r = self.gen_binary_inner(op, lhs, rhs);
        for (v, ty) in self.arg_frees.split_off(mark) {
            self.free_arg_temp(&v, &ty);
        }
        r
    }

    fn gen_binary_inner(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<(String, Type), String> {
        // short-circuit logical operators
        if matches!(op, BinOp::And | BinOp::Or) {
            let (l, _) = self.gen_expr(lhs)?;
            let pre = self.cur_block.clone();
            let rhs_l = self.fresh_label("sc.rhs");
            let end_l = self.fresh_label("sc.end");
            match op {
                BinOp::And => self.emit_term(format!("br i1 {l}, label %{rhs_l}, label %{end_l}")),
                BinOp::Or => self.emit_term(format!("br i1 {l}, label %{end_l}, label %{rhs_l}")),
                _ => unreachable!(),
            }
            self.emit_label(&rhs_l);
            let (r, _) = self.gen_expr(rhs)?;
            let rblock = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let t = self.fresh_tmp();
            let short = if op == BinOp::And { "false" } else { "true" };
            self.emit(format!(
                "{t} = phi i1 [ {short}, %{pre} ], [ {r}, %{rblock} ]"
            ));
            return Ok((t, Type::Bool));
        }

        // `s =~ "pat"`: run the pattern's precompiled DFA over the string.
        if op == BinOp::Match {
            let (s, _) = self.gen_expr(lhs)?;
            let pat = match rhs {
                Expr::Str(p) => p,
                _ => return Err("`=~` pattern must be a string literal".to_string()),
            };
            let (table, accept, start) = self
                .regex_globals
                .get(pat)
                .cloned()
                .ok_or_else(|| format!("regex pattern not compiled: {pat}"))?;
            let t = self.fresh_tmp();
            self.emit(format!(
                "{t} = call i1 @__vyrn_regex_run(ptr {s}, ptr {table}, i64 {start}, ptr {accept})"
            ));
            // An allocated left operand is this operator's to free (round
            // thirty) — the run read it, and nothing else ever will.
            self.free_str_temp(lhs, &s);
            return Ok((t, Type::Bool));
        }

        let (mut l, mut lty) = self.gen_expr(lhs)?;
        let (mut r, mut rty) = self.gen_expr(rhs)?;

        // Normalize a mixed Float/Float32 pair: a default `double` literal sibling
        // of a Float32 operand rounds to `float` (fptrunc) so the op runs at single
        // precision. Integer-literal siblings need no such step (LLVM int constants
        // are width-polymorphic; float constants are not).
        if self.resolve(&lty) == Type::Float32 && self.resolve(&rty) == Type::Float {
            let (nr, _) = self.coerce(r, &Type::Float, &Type::Float32)?;
            r = nr;
            rty = Type::Float32;
        } else if self.resolve(&rty) == Type::Float32 && self.resolve(&lty) == Type::Float {
            let (nl, _) = self.coerce(l, &Type::Float, &Type::Float32)?;
            l = nl;
            lty = Type::Float32;
        }

        // String comparison lowers to `strcmp` (contents, not pointers). Its
        // sign is byte-wise lexicographic — each differing byte compared as
        // `unsigned char` — which is exactly the interpreter's `str` byte-order
        // `Ord` (Vyrn strings never contain an interior NUL, so strcmp reads the
        // whole content). Equality tests the result `== 0` / `!= 0`; ordering
        // tests its sign against 0 with a signed `icmp` (`slt`/`sle`/`sgt`/`sge`).
        if matches!(
            op,
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
        ) && self.resolve(&lty) == Type::Str
        {
            let c = self.fresh_tmp();
            self.emit(format!("{c} = call i32 @strcmp(ptr {l}, ptr {r})"));
            // Both operands are read and neither is kept, so one this
            // expression allocated is released here (RFC-0096 M3, the same
            // consumer-site rule the `+` above applies — exit-residue round
            // twelve: `schema(ty, "") == ""` dropped a fresh String per
            // executed GraphQL selection).
            self.free_str_temp(lhs, &l);
            self.free_str_temp(rhs, &r);
            let t = self.fresh_tmp();
            let pred = match op {
                BinOp::Eq => "eq",
                BinOp::NotEq => "ne",
                BinOp::Lt => "slt",
                BinOp::LtEq => "sle",
                BinOp::Gt => "sgt",
                BinOp::GtEq => "sge",
                _ => unreachable!(),
            };
            self.emit(format!("{t} = icmp {pred} i32 {c}, 0"));
            return Ok((t, Type::Bool));
        }

        // `Code + Code` concatenates fragments, origins carried (RFC-0054). Both
        // sides are handles, so the concatenation happens in the host's arena
        // (RFC-0076 M3a) and this is one import call.
        if op == BinOp::Add && matches!(self.resolve(&lty), Type::Named(ref n) if n == "Code") {
            let t = self.fresh_tmp();
            self.emit(format!(
                "{t} = call i64 @__vyrn_code_concat(i64 {l}, i64 {r})"
            ));
            return Ok((t, lty));
        }

        // `a + b` on two Strings is concatenation (replacing `concat`): the same
        // heap allocation, region routing, and drop analysis.
        if op == BinOp::Add && self.resolve(&lty) == Type::Str {
            let buf = self.emit_str_concat(&l, &r);
            // Both halves are copied, so an operand this expression allocated
            // is released here (RFC-0096 M3). The type check is the guard the
            // predicate cannot make for itself: `+` is also integer addition
            // and `Code` concatenation, and both are handled above this arm.
            self.free_str_temp(lhs, &l);
            self.free_str_temp(rhs, &r);
            return Ok((buf, Type::Str));
        }

        // The integer op width: a sized-int operand sets it (a plain-Int literal
        // sibling adopts that width); otherwise `i64`/`i1` from the operand type.
        let numty = if matches!(self.resolve(&lty), Type::IntN { .. }) {
            self.resolve(&lty)
        } else if matches!(self.resolve(&rty), Type::IntN { .. }) {
            self.resolve(&rty)
        } else {
            self.resolve(&lty)
        };
        let ll = self.llt(&numty); // op width for ints (`iN`/`i1`)
        let t = self.fresh_tmp();
        let instr = if matches!(
            self.resolve(&lty),
            Type::Float | Type::Float32 | Type::F32x4 | Type::F64x2
        ) {
            // Floating-point ops (Float64 → `double`, Float32 → `float`). A vector
            // (RFC-0083) rides the same arms: `fadd <4 x float>` is the same
            // instruction over four lanes, and so is `fcmp olt` — which is why the
            // NaN discipline written in the relational comment below is inherited
            // by the mask rather than being decided a second time for it.
            let f = match self.resolve(&lty) {
                Type::Float32 => "float",
                Type::F32x4 => "<4 x float>",
                Type::F64x2 => "<2 x double>",
                _ => "double",
            };
            match op {
                BinOp::Add => format!("{t} = fadd {f} {l}, {r}"),
                BinOp::Sub => format!("{t} = fsub {f} {l}, {r}"),
                BinOp::Mul => format!("{t} = fmul {f} {l}, {r}"),
                BinOp::Div => format!("{t} = fdiv {f} {l}, {r}"),
                // The ordered/unordered choice is IEEE 754's, and it is the same
                // choice wasm's `f64.lt`..`f64.ne` and Rust's `f64` operators make,
                // so all three engines agree arm for arm: the four relational ops
                // and `==` are ORDERED (a NaN operand makes them false), and `!=`
                // is UNORDERED — `NaN != NaN` is TRUE. `one` here read
                // "ordered AND not equal", which made native the only engine
                // printing `0` for `nan != nan` (RFC-0077 M2h measured it).
                BinOp::Lt => format!("{t} = fcmp olt {f} {l}, {r}"),
                BinOp::LtEq => format!("{t} = fcmp ole {f} {l}, {r}"),
                BinOp::Gt => format!("{t} = fcmp ogt {f} {l}, {r}"),
                BinOp::GtEq => format!("{t} = fcmp oge {f} {l}, {r}"),
                BinOp::Eq => format!("{t} = fcmp oeq {f} {l}, {r}"),
                BinOp::NotEq => format!("{t} = fcmp une {f} {l}, {r}"),
                BinOp::Rem
                | BinOp::And
                | BinOp::Or
                | BinOp::Match
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr => {
                    return Err("`%`/`&&`/`||`/`=~`/bitwise ops are not valid on floats".into())
                }
            }
        } else {
            // Integer ops at the operand width (`iN`). Add/Sub/Mul are identical
            // for signed/unsigned (two's complement); Div/Rem and comparisons pick
            // the signed (`sdiv`/`slt`) or unsigned (`udiv`/`ult`) opcode by width.
            let unsigned = matches!(numty, Type::IntN { signed: false, .. });
            // `sdiv`/`udiv`/`srem`/`urem` trap the *process* (SIGFPE/SEH, no
            // message) on a zero divisor, and `sdiv` on MIN / -1. Guard both
            // with the interpreter's exact `error: ...` messages instead.
            if matches!(op, BinOp::Div | BinOp::Rem) {
                let z = self.fresh_tmp();
                self.emit(format!("{z} = icmp eq {ll} {r}, 0"));
                let msg = if op == BinOp::Div {
                    "@.trap.div0"
                } else {
                    "@.trap.rem0"
                };
                self.trap_if(&z, msg, "div.z");
                // Signed `MIN / -1` overflows (no representable quotient) and TRAPS.
                // `MIN % -1 == 0` does NOT trap (RFC-0060) — its `-1`-divisor guard
                // below rewrites the divisor so raw `srem MIN, -1` (UB) never runs.
                if !unsigned && op == BinOp::Div {
                    let bits: u32 = match numty {
                        Type::IntN { bits, .. } => bits.into(),
                        _ => 64,
                    };
                    let min = i64::MIN >> (64 - bits);
                    let lm = self.fresh_tmp();
                    let rm = self.fresh_tmp();
                    let both = self.fresh_tmp();
                    self.emit(format!("{lm} = icmp eq {ll} {l}, {min}"));
                    self.emit(format!("{rm} = icmp eq {ll} {r}, -1"));
                    self.emit(format!("{both} = and i1 {lm}, {rm}"));
                    self.trap_if(&both, "@.trap.divovf", "div.ovf");
                }
            }
            // Shift-amount range check (RFC-0045): a shift by `>= bitwidth`, or a
            // negative amount, traps. One UNSIGNED `>=` test covers both — a
            // negative amount reads as a huge unsigned, so it also fails `< bits`
            // (this exactly mirrors the interpreter's `y < 0 || y >= bits`).
            if matches!(op, BinOp::Shl | BinOp::Shr) {
                let bits: u32 = match numty {
                    Type::IntN { bits, .. } => bits.into(),
                    _ => 64,
                };
                let oor = self.fresh_tmp();
                self.emit(format!("{oor} = icmp uge {ll} {r}, {bits}"));
                self.trap_if(&oor, "@.trap.shift", "shift.oor");
            }
            match op {
                BinOp::Add => format!("{t} = add {ll} {l}, {r}"),
                BinOp::Sub => format!("{t} = sub {ll} {l}, {r}"),
                BinOp::Mul => format!("{t} = mul {ll} {l}, {r}"),
                BinOp::Div if unsigned => format!("{t} = udiv {ll} {l}, {r}"),
                BinOp::Div => format!("{t} = sdiv {ll} {l}, {r}"),
                BinOp::Rem if unsigned => format!("{t} = urem {ll} {l}, {r}"),
                BinOp::Rem => {
                    // Signed remainder: `x % -1 == 0` for every `x`, and raw
                    // `srem x, -1` is UB at `x == MIN`. Rewrite a `-1` divisor to
                    // `1` (whose remainder is always 0) so the result is correct
                    // everywhere and never UB — `MIN % -1` yields 0, no trap
                    // (RFC-0060). The zero-divisor trap above still fires first.
                    let isneg1 = self.fresh_tmp();
                    let safe = self.fresh_tmp();
                    self.emit(format!("{isneg1} = icmp eq {ll} {r}, -1"));
                    self.emit(format!("{safe} = select i1 {isneg1}, {ll} 1, {ll} {r}"));
                    format!("{t} = srem {ll} {l}, {safe}")
                }
                BinOp::Lt if unsigned => format!("{t} = icmp ult {ll} {l}, {r}"),
                BinOp::Lt => format!("{t} = icmp slt {ll} {l}, {r}"),
                BinOp::LtEq if unsigned => format!("{t} = icmp ule {ll} {l}, {r}"),
                BinOp::LtEq => format!("{t} = icmp sle {ll} {l}, {r}"),
                BinOp::Gt if unsigned => format!("{t} = icmp ugt {ll} {l}, {r}"),
                BinOp::Gt => format!("{t} = icmp sgt {ll} {l}, {r}"),
                BinOp::GtEq if unsigned => format!("{t} = icmp uge {ll} {l}, {r}"),
                BinOp::GtEq => format!("{t} = icmp sge {ll} {l}, {r}"),
                BinOp::Eq => format!("{t} = icmp eq {ll} {l}, {r}"),
                BinOp::NotEq => format!("{t} = icmp ne {ll} {l}, {r}"),
                // Bitwise (RFC-0045): and/or/xor directly; `<<` = `shl`; `>>` is
                // `lshr` (logical) on an unsigned operand and `ashr`
                // (arithmetic, sign-extending) on a signed one. The range trap
                // above has already fired for an out-of-range amount.
                BinOp::BitAnd => format!("{t} = and {ll} {l}, {r}"),
                BinOp::BitOr => format!("{t} = or {ll} {l}, {r}"),
                BinOp::BitXor => format!("{t} = xor {ll} {l}, {r}"),
                BinOp::Shl => format!("{t} = shl {ll} {l}, {r}"),
                BinOp::Shr if unsigned => format!("{t} = lshr {ll} {l}, {r}"),
                BinOp::Shr => format!("{t} = ashr {ll} {l}, {r}"),
                BinOp::And | BinOp::Or | BinOp::Match => unreachable!("handled above"),
            }
        };
        self.emit(instr);
        // A vector comparison's `fcmp`/`icmp` yields `<4 x i1>`; a `Mask32x4` IS
        // `<4 x i32>` of all-ones/all-zeros (see `llt_of`), so widen. `sext` and
        // not `zext`: all-ones is what `v128.bitselect` and every mask consumer on
        // the wasm side reads, and having the two backends carry the same bit
        // pattern is what stops a mask from meaning two things.
        //
        // Only a COMPARISON widens. `I32x4`'s `& | ^` are already `<4 x i32>` in
        // and out, which is the whole reason the integer width reaches them
        // without going through a mask at all.
        let cmp = matches!(
            op,
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq
        );
        if matches!(self.resolve(&lty), Type::F32x4 | Type::I32x4) && cmp {
            let m = self.fresh_tmp();
            self.emit(format!("{m} = sext <4 x i1> {t} to <4 x i32>"));
            return Ok((m, Type::Mask32x4));
        }
        // The same widening at the wide lane, into the mask that width yields.
        if self.resolve(&lty) == Type::F64x2 && cmp {
            let m = self.fresh_tmp();
            self.emit(format!("{m} = sext <2 x i1> {t} to <2 x i64>"));
            return Ok((m, Type::Mask64x2));
        }
        let result_ty = match op {
            // Arithmetic and bitwise keep the operand's integer type (Int or
            // IntN); arithmetic also covers Float.
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => numty,
            _ => Type::Bool,
        };
        Ok((t, result_ty))
    }

    /// Lower a numeric conversion to the right LLVM cast: integer resize
    /// (`sext`/`zext`/`trunc`), int↔float (`si/uitofp`, `fpto si/ui`), or
    /// float↔float width change (`fptrunc`/`fpext`).
    fn gen_numeric_conv(
        &mut self,
        v: String,
        from: &Type,
        to: &Type,
    ) -> Result<(String, Type), String> {
        let fr = self.resolve(from);
        let tr = self.resolve(to);
        let fll = self.llt(&fr); // "iN" | "float" | "double"
        let tll = self.llt(&tr);
        if fll == tll {
            return Ok((v, to.clone()));
        }
        // Signedness of the operands drives the widening/conversion opcode:
        // a source `UInt*` zero-extends and uses `uitofp`; an unsigned target
        // float→int uses `fptoui`. `Int`/`Int64` (plain) are signed.
        let from_unsigned = matches!(fr, Type::IntN { signed: false, .. });
        let to_unsigned = matches!(tr, Type::IntN { signed: false, .. });
        let from_float = matches!(fr, Type::Float | Type::Float32);
        let to_float = matches!(tr, Type::Float | Type::Float32);
        let t = self.fresh_tmp();
        match (from_float, to_float) {
            (false, false) => {
                let fw: u32 = fll.trim_start_matches('i').parse().unwrap_or(64);
                let tw: u32 = tll.trim_start_matches('i').parse().unwrap_or(64);
                if tw > fw {
                    let ext = if from_unsigned { "zext" } else { "sext" };
                    self.emit(format!("{t} = {ext} {fll} {v} to {tll}"));
                } else {
                    self.emit(format!("{t} = trunc {fll} {v} to {tll}"));
                }
            }
            (false, true) => {
                let op = if from_unsigned { "uitofp" } else { "sitofp" };
                self.emit(format!("{t} = {op} {fll} {v} to {tll}"));
            }
            // Float→int, in the interpreter's two steps rather than one cast:
            // saturate into 64 bits, then narrow by WRAPPING. That composition
            // is why `UInt8(300.7)` is 44 and not 255 — `300.7 as u64` is 300
            // and `300 as u8` wraps — and a single `fptoui .. to i8` agrees with
            // it only by accident of the host instruction.
            (true, false) => {
                let sfx = if fll == "double" { "f64" } else { "f32" };
                let op = if to_unsigned { "fptoui" } else { "fptosi" };
                let sat = self.fresh_tmp();
                self.emit(format!(
                    "{sat} = call i64 @llvm.{op}.sat.i64.{sfx}({fll} {v})"
                ));
                if tll == "i64" {
                    return Ok((sat, to.clone()));
                }
                self.emit(format!("{t} = trunc i64 {sat} to {tll}"));
            }
            // Float↔Float of different widths (fll != tll guaranteed above):
            // f64→f32 rounds (`fptrunc`), f32→f64 is exact (`fpext`).
            (true, true) => {
                let op = if fll == "double" { "fptrunc" } else { "fpext" };
                self.emit(format!("{t} = {op} {fll} {v} to {tll}"));
            }
        }
        Ok((t, to.clone()))
    }

    /// Emit an array out-of-bounds trap block (`error: array index %lld out of
    /// bounds` to stderr, then `exit(1)`), terminating the current block chain.
    /// Shared by the index read (`at`), the index store (`a[i] = v`), and
    /// `swapRemove` so all three are byte-identical to the interpreter.
    fn emit_array_oob_trap(&mut self, label: &str, iv: &str) {
        self.emit_label(label);
        self.emit(format!(
            "call void @__vyrn_trap_idx(ptr @.trap.aoob, i64 {iv})"
        ));
        self.emit_term("unreachable".into());
    }

    /// Insert-or-update `key`→`v` into the Map whose `{keys,vals,len,cap}` header
    /// lives at alloca `slot` (RFC-0028). A hit overwrites the value in its slot
    /// (order preserved); a miss reserves room (may realloc both buffers),
    /// appends key and value, and bumps the shared length. `val` is the value
    /// type; `v` is already coerced into it.
    ///
    /// **The map takes the key AND the value, so the map releases the key and
    /// the value it does not keep.** `movecheck` refuses a borrowed key, so what
    /// arrives here is always a value this map may own — and the hit path used
    /// to drop both on the floor. `m[k] = c + 1` in a histogram loop leaked one
    /// key per repeat: 200 thousand inserts of one 3-byte key read 10.3 MB peak
    /// before that line and 4.9 MB after. The value half was missed at the time
    /// and is `drop_old` here: `m["k"] = "b"` over `m["k"] = "a"` leaked the
    /// String `"a"` every repeat, 100 thousand of them reading 5.0 MB peak
    /// before this line and 1.3 MB after. The interpreter needed neither,
    /// because its key and its value are both `Rc`.
    ///
    /// `drop_old` is the caller's answer to "may this store release what the
    /// slot holds now" — rule 4's own question, asked exactly as the element
    /// store one screen up asks it (`slot_owns` and `mentions_place`), because a
    /// new value that names the map could name the very bytes this frees.
    /// The two fields a lookup reads on top of `keys` and `len`: the bucket
    /// array and the capacity that sizes it (`cap * 2` buckets). Every
    /// `__vyrn_map_find` call site takes them from the same aggregate it took
    /// the other two from, which is why they are extracted together here.
    /// Whether this map's key type is `Int64` (RFC-0117 M1) — the one question
    /// every key-touching emission branches on.
    fn key_is_int(&self, key_ty: &Type) -> bool {
        vyrn_frontend::types::resolve(key_ty, self.types) == Type::Int
    }

    /// A user-keyed map (RFC-0117 M2): the key resolves to a heapless record
    /// or a fieldless enum, admitted by the checker's key rule.
    fn key_is_pack(&self, key_ty: &Type) -> bool {
        matches!(
            vyrn_frontend::types::resolve(key_ty, self.types),
            Type::Record(_) | Type::Enum(_)
        )
    }

    /// RFC-0117 M2: write a user key value into a ZEROED buffer of its own
    /// layout, field by field — the canonical pack. `memcmp` over the stride
    /// is then field-wise equality, because padding is never anything but
    /// zero. Returns the buffer pointer and the stride SSA.
    fn emit_key_pack(&mut self, v: &str, key_ty: &Type) -> (String, String) {
        let kll = self.llt(key_ty);
        let stride = self.fresh_tmp();
        self.emit(format!(
            "{stride} = ptrtoint ptr getelementptr ({kll}, ptr null, i64 1) to i64"
        ));
        let buf = self.fresh_alloca(&kll);
        self.emit(format!(
            "call void @llvm.memset.p0.i64(ptr {buf}, i8 0, i64 {stride}, i1 false)"
        ));
        self.emit_pack_into(v, key_ty, &buf);
        (buf, stride)
    }

    /// One level of the pack: a record stores each field into its own slot
    /// (recursively), everything else — a scalar, a fieldless enum's tag —
    /// stores its value whole, which carries no padding of its own.
    fn emit_pack_into(&mut self, v: &str, ty: &Type, dst: &str) {
        match vyrn_frontend::types::resolve(ty, self.types) {
            Type::Record(fs) => {
                let ll = self.llt(ty);
                for (i, f) in fs.iter().enumerate() {
                    let fv = self.fresh_tmp();
                    self.emit(format!("{fv} = extractvalue {ll} {v}, {i}"));
                    let fp = self.fresh_tmp();
                    self.emit(format!(
                        "{fp} = getelementptr inbounds {ll}, ptr {dst}, i64 0, i32 {i}"
                    ));
                    self.emit_pack_into(&fv, &f.ty, &fp);
                }
            }
            _ => {
                let ll = self.llt(ty);
                self.emit(format!("store {ll} {v}, ptr {dst}"));
            }
        }
    }

    fn map_index_of(&mut self, agg: &str) -> (String, String) {
        let cap = self.fresh_tmp();
        let ix = self.fresh_tmp();
        self.emit(format!(
            "{cap} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {agg}, 3"
        ));
        self.emit(format!(
            "{ix} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {agg}, 4"
        ));
        (ix, cap)
    }

    fn emit_map_set(
        &mut self,
        slot: &str,
        key: &str,
        v: &str,
        key_ty: &Type,
        val: &Type,
        drop_old: bool,
    ) -> Result<(), String> {
        // An Int64-keyed map (RFC-0117): the key column holds the values
        // themselves, so the probe takes the key by value, an insert stores it,
        // and nothing about a key is ever dup'd or freed.
        let ik = self.key_is_int(key_ty);
        // A user key (RFC-0117 M2) probes and stores by its canonical pack.
        let pk = self.key_is_pack(key_ty);
        let packed = pk.then(|| self.emit_key_pack(key, key_ty));
        let vll = self.llt(val);
        let esz = self.fresh_tmp();
        self.emit(format!(
            "{esz} = ptrtoint ptr getelementptr ({vll}, ptr null, i64 1) to i64"
        ));
        let hdr = self.fresh_tmp();
        let keys = self.fresh_tmp();
        let len = self.fresh_tmp();
        self.emit(format!(
            "{hdr} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
        ));
        self.emit(format!(
            "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 0"
        ));
        self.emit(format!(
            "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 2"
        ));
        let (ix, cap) = self.map_index_of(&hdr);
        let idx = self.fresh_tmp();
        if ik {
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find_i64(ptr {keys}, i64 {len}, i64 {key}, ptr {ix}, i64 {cap})"
            ));
        } else if let Some((kbuf, stride)) = &packed {
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find_pack(ptr {keys}, i64 {len}, ptr {kbuf}, i64 {stride}, ptr {ix}, i64 {cap})"
            ));
        } else {
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {key}, ptr {ix}, i64 {cap})"
            ));
        }
        let found = self.fresh_tmp();
        self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
        let upd_l = self.fresh_label("map.set.upd");
        let ins_l = self.fresh_label("map.set.ins");
        let done_l = self.fresh_label("map.set.done");
        self.emit_term(format!("br i1 {found}, label %{upd_l}, label %{ins_l}"));
        // update: store into the existing value slot.
        self.emit_label(&upd_l);
        let vals0 = self.fresh_tmp();
        self.emit(format!(
            "{vals0} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 1"
        ));
        let ep0 = self.fresh_tmp();
        self.emit(format!(
            "{ep0} = getelementptr {vll}, ptr {vals0}, i64 {idx}"
        ));
        // The value in that slot has no owner left once the store lands, so it
        // goes back first — read out of the slot, then released.
        if drop_old {
            self.release_entry(&ep0, val)?;
        }
        self.emit(format!("store {vll} {v}, ptr {ep0}"));
        // The map already holds an equal key, so this one is surplus. Inside a
        // `region` it came from the arena, which hands it back at the exit —
        // freeing it here would give one block two owners. The same partition
        // `deep_release` draws for a `String`, drawn here too. An `Int64` key
        // owns nothing, so it has no surplus to return.
        if !ik && !pk && self.region_depth == 0 {
            self.emit(format!("call void @__vyrn_str_free(ptr {key})"));
        }
        self.emit_term(format!("br label %{done_l}"));
        // insert: reserve (may realloc both buffers), reload, append, len += 1.
        self.emit_label(&ins_l);
        if let Some((_, stride)) = &packed {
            self.emit(format!(
                "call void @__vyrn_map_reserve_pack(ptr {slot}, i64 {esz}, i64 {stride})"
            ));
        } else {
            let rsv = if ik {
                "__vyrn_map_reserve_i64"
            } else {
                "__vyrn_map_reserve"
            };
            self.emit(format!("call void @{rsv}(ptr {slot}, i64 {esz})"));
        }
        let hdr2 = self.fresh_tmp();
        let keys2 = self.fresh_tmp();
        let vals2 = self.fresh_tmp();
        self.emit(format!(
            "{hdr2} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
        ));
        self.emit(format!(
            "{keys2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 0"
        ));
        self.emit(format!(
            "{vals2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 1"
        ));
        let kep = self.fresh_tmp();
        if ik {
            self.emit(format!("{kep} = getelementptr i64, ptr {keys2}, i64 {len}"));
            self.emit(format!("store i64 {key}, ptr {kep}"));
        } else if let Some((kbuf, stride)) = &packed {
            let off = self.fresh_tmp();
            self.emit(format!("{off} = mul i64 {len}, {stride}"));
            self.emit(format!("{kep} = getelementptr i8, ptr {keys2}, i64 {off}"));
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {kep}, ptr {kbuf}, i64 {stride}, i1 false)"
            ));
        } else {
            self.emit(format!("{kep} = getelementptr ptr, ptr {keys2}, i64 {len}"));
            self.emit(format!("store ptr {key}, ptr {kep}"));
        }
        // The key is in its slot, so the index can record where. `reserve` above
        // grew the bucket array and rebuilt it, so this is the only entry it is
        // missing — and the reason the append stays O(1).
        if let Some((_, stride)) = &packed {
            self.emit(format!(
                "call void @__vyrn_map_index_add_pack(ptr {slot}, i64 {len}, i64 {stride})"
            ));
        } else {
            let iadd = if ik {
                "__vyrn_map_index_add_i64"
            } else {
                "__vyrn_map_index_add"
            };
            self.emit(format!("call void @{iadd}(ptr {slot}, i64 {len})"));
        }
        let vep = self.fresh_tmp();
        self.emit(format!(
            "{vep} = getelementptr {vll}, ptr {vals2}, i64 {len}"
        ));
        self.emit(format!("store {vll} {v}, ptr {vep}"));
        let nl = self.fresh_tmp();
        self.emit(format!("{nl} = add i64 {len}, 1"));
        let lenp = self.fresh_tmp();
        self.emit(format!(
            "{lenp} = getelementptr {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}, i64 0, i32 2"
        ));
        self.emit(format!("store i64 {nl}, ptr {lenp}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        Ok(())
    }

    // ---- higher-order monomorphization (RFC-0023) -----------------------

    /// Emit a call to a function `callee` that takes one or more `fn`-typed
    /// parameters. Each function-value argument is resolved to a target symbol
    /// (a lifted lambda, a named function, or a forwarded parameter) with its
    /// captures materialized HERE (the outer call site — the capture-timing lock);
    /// the callee is specialized per those targets and called directly.
    fn gen_ho_call(
        &mut self,
        callee: &'a Function,
        args: &[Expr],
    ) -> Result<(String, Type), String> {
        let name = callee.name.clone();
        let generic = !callee.type_params.is_empty();
        // The specialization's generic substitution, solved from the ordinary
        // (non-`fn`) arguments first so a `map<T, U>` lambda sees a concrete `T`.
        let mut call_subst: HashMap<String, Type> = HashMap::new();
        // Ordinary argument operands, in parameter order.
        let mut nonfn_ops: Vec<String> = Vec::new();
        // A `fn`-typed argument that is neither a lambda literal nor a bare name
        // is an ORDINARY expression producing a stored function value, and it can
        // have effects. It is evaluated here, in argument order alongside the
        // non-`fn` arguments, because the interpreter evaluates arguments left to
        // right and a value resolved in the second pass would run late. A lambda's
        // captures and a name's load have no effects, so those stay where they are.
        let mut evaluated: HashMap<usize, (String, Type)> = HashMap::new();
        for (i, p) in callee.params.iter().enumerate() {
            if matches!(p.ty, Type::Fn(..)) {
                if !matches!(args[i], Expr::Lambda { .. } | Expr::Var { .. }) {
                    let (v, vty) = self.gen_expr(&args[i])?;
                    evaluated.insert(i, (v, vty));
                }
                continue;
            }
            let frees_mark = self.arg_frees.len();
            let (v, vty) = self.gen_expr(&args[i])?;
            let aty = vyrn_frontend::types::substitute(&vty, self.subst);
            if generic {
                solve_param(&p.ty, &aty, &mut call_subst);
            }
            let was_fixed = matches!(self.resolve(&aty), Type::ArrayN(..) | Type::SmallArray(..));
            let pty = vyrn_frontend::types::substitute(&p.ty, &call_subst);
            let (v, cty) = self.coerce(v, &aty, &pty)?;
            // RFC-0114 §25's heapify row — the ordinary call path's twin: the
            // hook fired on the fixed literal, the coercion allocated the
            // growable triple, so the pushed entry retargets at the product.
            if self.arg_frees.len() > frees_mark
                && was_fixed
                && matches!(self.resolve(&cty), Type::Array(_))
            {
                if let Some(last) = self.arg_frees.last_mut() {
                    *last = (v.clone(), cty.clone());
                }
            }
            nonfn_ops.push(format!("{} {v}", self.llt(&cty)));
        }
        // Resolve each `fn`-typed argument: lift/forward the target and evaluate
        // its captures now.
        let mut bindings: Vec<HoParamBinding> = Vec::new();
        let mut capture_ops: Vec<String> = Vec::new();
        for (i, p) in callee.params.iter().enumerate() {
            let Type::Fn(dptys, dret) = &p.ty else {
                continue;
            };
            // RFC-0071 M2b/M2c: a type parameter may occur ONLY inside a `fn`
            // parameter's own parameter list (`paramQuery(run: fn(P) -> T)`), with
            // no ordinary argument to pin it in pass 1. Solve those from the
            // target's declared parameters, exactly as the return is solved below
            // — otherwise `P` survives as a `Type::Param` into the instance, and a
            // `Type::Param` lowers to `void`: an `alloca void`, a `void` argument,
            // and a dispatcher keyed on a signature no construction registers.
            // The checker's `check_fn_arg` learned the same rule; this is its
            // codegen half.
            if generic {
                let tptys = match evaluated.get(&i) {
                    Some((_, t)) => match self.normalize_sig(t) {
                        Type::Fn(ps, _) => Some(ps),
                        _ => None,
                    },
                    None => self.fn_arg_param_types(&args[i]),
                };
                if let Some(tptys) = tptys {
                    for (d, t) in dptys.iter().zip(&tptys) {
                        solve_param(d, t, &mut call_subst);
                    }
                }
            }
            // The parameter's `fn` type with type parameters filled in from pass 1.
            let ptys: Vec<Type> = dptys
                .iter()
                .map(|t| vyrn_frontend::types::substitute(t, &call_subst))
                .collect();
            let dret_sub = vyrn_frontend::types::substitute(dret, &call_subst);
            let (target_sym, capture_tys, target_ret) = self.resolve_fn_arg(
                &args[i],
                &ptys,
                &dret_sub,
                &mut capture_ops,
                evaluated.remove(&i),
            )?;
            // Solve the outbound generic parameter (`U`) from the target's return.
            if generic {
                solve_param(dret, &target_ret, &mut call_subst);
            }
            bindings.push(HoParamBinding {
                param_name: p.name.clone(),
                target_sym,
                capture_tys,
                param_tys: ptys,
                ret: target_ret,
            });
        }
        // The specialized instance's symbol keys on (callee, type args, targets).
        let type_args: Vec<Type> = callee
            .type_params
            .iter()
            .map(|tp| call_subst.get(tp).cloned().unwrap_or(Type::Unit))
            .collect();
        let mut sym = format!("{}__ho", fn_sym(name.as_str()));
        for ta in &type_args {
            sym.push('_');
            sym.push_str(&mangle_ty(ta));
        }
        for b in &bindings {
            sym.push('_');
            sym.push_str(&sanitize(&b.target_sym));
        }
        // …and the structural identity of exactly what was just spelled, for
        // [`struct_key`]'s reason: `drain_ho` dedups on this symbol, and both
        // halves of the readable spelling are lossy (`mangle_ty` collapses every
        // record to `Rec`; `sanitize` collapses every non-ASCII-alphanumeric).
        let targets: Vec<&str> = bindings.iter().map(|b| b.target_sym.as_str()).collect();
        sym.push_str(&format!("_h{}", struct_key(&(&type_args, &targets))));
        self.ho_instances.push(HoInst {
            sym: sym.clone(),
            name: name.clone(),
            subst: call_subst.clone(),
            bindings,
        });
        // Emit the direct call: ordinary operands, then every capture operand.
        let mut arg_ops = nonfn_ops;
        arg_ops.extend(capture_ops);
        let ret_ty = vyrn_frontend::types::substitute(&callee.ret, &call_subst);
        let retll = self.llt(&ret_ty);
        if retll == "void" {
            self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
            Ok((String::new(), Type::Unit))
        } else {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {retll} @{sym}({})", arg_ops.join(", ")));
            Ok((t, ret_ty))
        }
    }

    /// The DECLARED parameter types of a `fn`-typed argument's target, when the
    /// argument names one: a top-level function, a forwarded `fn` parameter, or a
    /// stored function value. `None` for a lambda literal, whose parameters take
    /// their types from the signature they flow into and so can solve nothing.
    fn fn_arg_param_types(&self, arg: &Expr) -> Option<Vec<Type>> {
        let Expr::Var { name, .. } = arg else {
            return None;
        };
        if let Some(b) = self.fn_bindings.get(name) {
            return Some(b.param_tys.clone());
        }
        if let Some((_, ty)) = self.lookup(name) {
            if let Type::Fn(ptys, _) = self.normalize_sig(&ty) {
                return Some(ptys);
            }
            return None;
        }
        self.param_types.get(name).cloned()
    }

    /// Resolve one `fn`-typed argument to a call target (RFC-0023), emitting any
    /// capture loads into `capture_ops`. Returns (target symbol, capture types,
    /// the target's concrete return type).
    fn resolve_fn_arg(
        &mut self,
        arg: &Expr,
        ptys: &[Type],
        expected_ret: &Type,
        capture_ops: &mut Vec<String>,
        evaluated: Option<(String, Type)>,
    ) -> Result<(String, Vec<Type>, Type), String> {
        match arg {
            Expr::Lambda { params, body, .. } => {
                // Free (captured) locals, in first-seen order.
                let locals: std::collections::HashSet<String> = params.iter().cloned().collect();
                let cap_names = self.lambda_captures(body, locals);
                let mut cap_tys = Vec::new();
                for cn in &cap_names {
                    let (v, cty) = self.emit_capture(cn)?;
                    capture_ops.push(format!("{} {v}", self.llt(&cty)));
                    cap_tys.push(cty);
                }
                // The expected return: concrete for a monomorphic `fn` type, or a
                // type parameter to be inferred from the body.
                let want_ret = if matches!(expected_ret, Type::Param(_)) {
                    None
                } else {
                    Some(expected_ret.clone())
                };
                let (sym, ret) =
                    self.emit_lifted_lambda(params, body, &cap_names, &cap_tys, ptys, want_ret)?;
                Ok((sym, cap_tys, ret))
            }
            Expr::Var { name: vn, .. } => {
                // A pass-through `fn`-typed parameter: forward its target and its
                // captures (this instance's own capture parameters).
                if let Some(b) = self.fn_bindings.get(vn).cloned() {
                    for (ty, v) in &b.captures {
                        capture_ops.push(format!("{} {v}", self.llt(ty)));
                    }
                    let cap_tys = b.captures.iter().map(|(ty, _)| ty.clone()).collect();
                    return Ok((b.target_sym.clone(), cap_tys, b.ret.clone()));
                }
                // A STORED function value (RFC-0037) flowing into a v1 `fn`-typed
                // parameter: the specialized instance receives the `{ i64, i64 }`
                // enum as its capture parameter, and calls to the parameter
                // become direct calls to the signature's dispatcher with the
                // enum prepended — v1's zero-cost path for direct lambda/named
                // arguments is untouched (those never reach this arm).
                if let Some((slot, ty)) = self.lookup(vn) {
                    if let sig @ Type::Fn(..) = self.normalize_sig(&ty) {
                        let Type::Fn(_, ref sret) = sig else {
                            unreachable!()
                        };
                        let v = self.fresh_tmp();
                        self.emit(format!("{v} = load {{ i64, i64 }}, ptr {slot}"));
                        capture_ops.push(format!("{{ i64, i64 }} {v}"));
                        let sym = self.fnval_dispatcher_sym(&sig);
                        let ret = (**sret).clone();
                        return Ok((sym, vec![sig.clone()], ret));
                    }
                }
                // A named top-level function: call it directly, no captures.
                let ret = self.ret_types.get(vn).cloned().unwrap_or(Type::Unit);
                Ok((fn_sym(vn), Vec::new(), ret))
            }
            // Any other expression of `fn` type (RFC-0037): a field read, an
            // element, a call's result. The value it produces is the same
            // `{ i64, i64 }` pair a slot holds, so it takes the arm above: the
            // target is the signature's dispatcher and the enum is the capture.
            // Nothing is copied — a field read is an `extractvalue`, so the
            // capture block stays owned by the place the value was read from,
            // exactly as the slot load leaves it owned by the `let`.
            _ => {
                let Some((v, ty)) = evaluated else {
                    return Err("internal: unexpected `fn`-typed argument".into());
                };
                let sig = self.normalize_sig(&vyrn_frontend::types::substitute(&ty, self.subst));
                let Type::Fn(_, ref sret) = sig else {
                    return Err("internal: unexpected `fn`-typed argument".into());
                };
                capture_ops.push(format!("{{ i64, i64 }} {v}"));
                let sym = self.fnval_dispatcher_sym(&sig);
                let ret = (**sret).clone();
                Ok((sym, vec![sig.clone()], ret))
            }
        }
    }

    /// The captured (free) local variables of a lambda body (RFC-0023), in
    /// first-seen order.
    ///
    /// The walk itself is [`lambda_captures`], shared with the direct wasm
    /// backend: a capture LIST is part of a lifted lambda's signature, so two
    /// backends that disagreed about its length or its order would emit calls
    /// with the wrong number of arguments — the same class of two-sources-of-truth
    /// bug `llt_of` and `predicate_binds` exist to prevent. Only "is this name an
    /// enclosing local?" is per-backend, and it is one closure.
    fn lambda_captures(
        &self,
        body: &LambdaBody,
        locals: std::collections::HashSet<String>,
    ) -> Vec<String> {
        lambda_captures(body, locals, &|name| {
            self.scope
                .iter()
                .any(|f| f.iter().any(|(n, _, _)| n == name))
                || self.fn_bindings.contains_key(name)
        })
    }

    /// Materialize ONE capture of a lambda being lifted, at the literal's site.
    ///
    /// An ordinary local is a load from its slot. A `fn`-typed PARAMETER has no
    /// slot — inside a specialized instance it lives in `fn_bindings` as a target
    /// plus this instance's own capture SSA values — so it becomes the same
    /// `{ i64, i64 }` defunctionalized aggregate storing it anywhere else builds.
    /// The lifted body then receives it as an ordinary `fn`-typed local and calls
    /// it through the signature's dispatcher, which is exactly what a hand-written
    /// `let g: fn(T) -> U = f` was doing at three call sites in std.
    ///
    /// The expected-type stack is cleared across the aggregate's construction: the
    /// type in scope here is the OUTER lambda's signature, and letting it be read
    /// as the capture's own would register the wrong variant.
    fn emit_capture(&mut self, cn: &str) -> Result<(String, Type), String> {
        if let Some((slot, ty)) = self.lookup(cn) {
            let cty = vyrn_frontend::types::substitute(&ty, self.subst);
            let ll = self.llt(&cty);
            let v = self.fresh_tmp();
            self.emit(format!("{v} = load {ll}, ptr {slot}"));
            // The snapshot OWNS its heap (RFC-0114 §25 round three): two
            // lambdas over one binding used to build two blocks holding ONE
            // pointer, which is why the release had to stay shallow and every
            // heap capture leaked. A duplicated capture gives each block its
            // own copy, which is what lets `__vyrn_fnval_release` walk it —
            // and it is the by-value snapshot the capture-timing lock always
            // claimed. The binding itself still answers `Gone::Captured`, so
            // its own value keeps its recorded leak; that row is separate.
            if self.owns_heap(&cty) {
                let v2 = self.deep_copy(&v, &cty)?;
                return Ok((v2, cty));
            }
            return Ok((v, cty));
        }
        if let Some(b) = self.fn_bindings.get(cn).cloned() {
            let saved = std::mem::take(&mut self.expect);
            let r = self.construct_fnval_binding(&b);
            self.expect = saved;
            return r;
        }
        Err(format!("captured `{cn}` not in scope"))
    }
}

/// Deep-normalize a stored-fn signature (RFC-0037) so structurally identical
/// spellings — a `type Transform = fn(Int64) -> Int64` alias, a validated scalar,
/// transformer sugar — register and dispatch as ONE synthesized enum.
/// `Task` interiors are left resolved: they cannot hold fn values, and
/// recursing would cycle.
///
/// Shared with the direct wasm backend, because it decides which constructions a
/// dispatcher covers. Two backends grouping differently would give one of them a
/// dispatcher missing a variant — a defensive trap where a call belongs, reached
/// only by the spelling nobody wrote a test for.
pub(crate) fn normalize_fn_sig(t: &Type, types: &HashMap<String, TypeDecl>) -> Type {
    let norm = |x: &Type| normalize_fn_sig(x, types);
    match vyrn_frontend::types::resolve(t, types) {
        Type::Fn(ps, r) => Type::Fn(ps.iter().map(norm).collect(), Box::new(norm(&r))),
        Type::Array(i) => Type::Array(Box::new(norm(&i))),
        Type::ArrayN(i, n) => Type::ArrayN(Box::new(norm(&i)), n),
        Type::Option(i) => Type::Option(Box::new(norm(&i))),
        Type::Result(a, b) => Type::Result(Box::new(norm(&a)), Box::new(norm(&b))),
        Type::Map(k, v) => Type::Map(Box::new(norm(&k)), Box::new(norm(&v))),
        Type::Record(fs) => Type::Record(
            fs.iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: norm(&f.ty),
                })
                .collect(),
        ),
        other => other,
    }
}

/// The captured (free) local variables of a lambda body (RFC-0023), in
/// first-seen order: names read in the body that are neither the lambda's own
/// parameters/locals nor module state nor functions — i.e. bindings that live in
/// the enclosing local scope, which is what `is_local` answers.
pub(crate) fn lambda_captures(
    body: &LambdaBody,
    locals: std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut locals = locals;
    match body {
        LambdaBody::Expr(e) => captures_of_expr(e, &mut locals, &mut out, &mut seen, is_local),
        LambdaBody::Block(b) => captures_of_block(b, &mut locals, &mut out, &mut seen, is_local),
    }
    out
}

fn captures_of_block(
    b: &Block,
    locals: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let { name, value, .. } => {
                captures_of_expr(value, locals, out, seen, is_local);
                locals.insert(name.clone());
            }
            Stmt::Assign { value, .. }
            | Stmt::SetField { value, .. }
            | Stmt::Expr(value)
            | Stmt::Return {
                value: Some(value), ..
            } => captures_of_expr(value, locals, out, seen, is_local),
            Stmt::IndexSet { index, value, .. } => {
                captures_of_expr(index, locals, out, seen, is_local);
                captures_of_expr(value, locals, out, seen, is_local);
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                captures_of_expr(cond, locals, out, seen, is_local);
                captures_of_block(then_block, &mut locals.clone(), out, seen, is_local);
                if let Some(eb) = else_block {
                    captures_of_block(eb, &mut locals.clone(), out, seen, is_local);
                }
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                captures_of_expr(scrutinee, locals, out, seen, is_local);
                let mut inner = locals.clone();
                for b in vyrn_frontend::movecheck::pattern_bindings(pattern) {
                    inner.insert(b.to_string());
                }
                captures_of_block(then_block, &mut inner, out, seen, is_local);
                if let Some(eb) = else_block {
                    captures_of_block(eb, &mut locals.clone(), out, seen, is_local);
                }
            }
            Stmt::While { cond, body, .. } => {
                captures_of_expr(cond, locals, out, seen, is_local);
                captures_of_block(body, &mut locals.clone(), out, seen, is_local);
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                captures_of_expr(iter, locals, out, seen, is_local);
                let mut inner = locals.clone();
                inner.insert(var.clone());
                captures_of_block(body, &mut inner, out, seen, is_local);
            }
            Stmt::Region { body, .. } => {
                captures_of_block(body, &mut locals.clone(), out, seen, is_local)
            }
            Stmt::Return { value: None, .. }
            | Stmt::Drop { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {}
        }
    }
}

fn captures_of_expr(
    e: &Expr,
    locals: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) {
    match e {
        Expr::Var { name, .. } => {
            if locals.contains(name) || seen.contains(name) {
                return;
            }
            // Only an enclosing LOCAL slot is a capture — module state and
            // functions/variants are reached directly by the lifted function.
            if is_local(name) {
                seen.insert(name.clone());
                out.push(name.clone());
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            captures_of_expr(expr, locals, out, seen, is_local)
        }
        Expr::Binary { lhs, rhs, .. } => {
            captures_of_expr(lhs, locals, out, seen, is_local);
            captures_of_expr(rhs, locals, out, seen, is_local);
        }
        // A CALL captures its callee when the callee names an enclosing local:
        // `|req, ps| run(req)` over a `fn`-typed `run` calls a value, not a
        // symbol, and leaving it out of the capture list lowered it as a direct
        // call to `@vyrn_run` — a name no module defines (the interpreter, which
        // resolves through the environment, ran the same program fine). Nothing
        // else changes: `is_local` is false for a top-level function, so an
        // ordinary call still reaches its symbol with no capture at all.
        Expr::Call { name, args, .. } => {
            if !locals.contains(name) && !seen.contains(name) && is_local(name) {
                seen.insert(name.clone());
                out.push(name.clone());
            }
            for a in args {
                captures_of_expr(a, locals, out, seen, is_local);
            }
        }
        Expr::Spawn { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                captures_of_expr(a, locals, out, seen, is_local);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            captures_of_expr(scrutinee, locals, out, seen, is_local);
            for arm in arms {
                let mut inner = locals.clone();
                for b in vyrn_frontend::pattern_bindings(&arm.pattern) {
                    inner.insert(b.to_string());
                }
                match &arm.body {
                    ArmBody::Expr(e) => captures_of_expr(e, &mut inner, out, seen, is_local),
                    ArmBody::Block(b) => captures_of_block(b, &mut inner, out, seen, is_local),
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            captures_of_expr(cond, locals, out, seen, is_local);
            captures_of_expr(then_branch, locals, out, seen, is_local);
            if let Some(eb) = else_branch {
                captures_of_expr(eb, locals, out, seen, is_local);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                captures_of_expr(v, locals, out, seen, is_local);
            }
        }
        _ => {}
    }
}

impl<'a> Gen<'a> {
    /// Emit a monomorphized top-level function for a lambda literal (RFC-0023):
    /// `@__vyrn_lambda_...(<captures>, <params>) -> <ret>`. Returns (symbol,
    /// concrete return type). The definition is buffered in `lambda_defs` for the
    /// driver to append once (deduped by symbol).
    #[allow(clippy::too_many_arguments)]
    fn emit_lifted_lambda(
        &mut self,
        params: &[String],
        body: &LambdaBody,
        cap_names: &[String],
        cap_tys: &[Type],
        param_tys: &[Type],
        want_ret: Option<Type>,
    ) -> Result<(String, Type), String> {
        // A deterministic, dedup-friendly symbol: enclosing function + ordinal +
        // the concrete capture/param/return shape (so two instantiations of a
        // generic function lift distinct, correctly-typed copies).
        let ordinal = self.lambda_counter;
        self.lambda_counter += 1;
        let mut shape = String::new();
        for t in cap_tys.iter().chain(param_tys.iter()) {
            shape.push_str(&mangle_ty(t));
        }
        if let Some(r) = &want_ret {
            shape.push('R');
            shape.push_str(&mangle_ty(r));
        }
        // The trailing key is what makes the shape an identity rather than a
        // label: `drain_ho` dedups the definition on this symbol, and the
        // readable half is lossy in both of `mangle_ty`'s and `sanitize`'s ways
        // (see [`struct_key`]).
        let sym = format!(
            "__vyrn_lambda_{}_{ordinal}_{shape}_h{}",
            sanitize(&self.cur_fn_name),
            struct_key(&(&self.cur_fn_name, cap_tys, param_tys, &want_ret))
        );

        // Save the current emission state; emit the lambda into fresh buffers.
        let saved_allocas = std::mem::take(&mut self.allocas);
        let saved_body = std::mem::take(&mut self.body);
        let saved_scope = std::mem::replace(&mut self.scope, vec![Vec::new()]);
        let saved_block = std::mem::replace(&mut self.cur_block, "entry".to_string());
        let saved_term = std::mem::replace(&mut self.terminated, false);
        let saved_ret = self.fn_ret.clone();
        let saved_tmp = self.tmp;
        let saved_label = self.label;
        let saved_drop = std::mem::take(&mut self.drop_slots);
        let saved_cursors = std::mem::take(&mut self.cursors);
        // A lifted lambda is a different function: it owns no release rows here
        // (the shell that lowers it has none), so it must not read the enclosing
        // body's placement either.
        let saved_placed = std::mem::take(&mut self.placed);
        let saved_droppable = std::mem::take(&mut self.droppable);
        let saved_modify = std::mem::take(&mut self.modify_copyout);
        let saved_bindings = std::mem::take(&mut self.fn_bindings);
        // The lifted body is a different function: the enclosing one's append
        // candidates say nothing about its locals, and its shadow slots would
        // be allocas of the outer frame.
        let saved_append_ok = std::mem::take(&mut self.append_ok);
        let saved_str_append = std::mem::take(&mut self.str_append);
        // The lifted body is a function that OUTLIVES the lexical site: a
        // lambda written inside `region { .. }` may be called outside any
        // region, so its String allocations must route to `malloc`, not bake in
        // the arena (`str_alloc` compiles the routing in from this flag).
        let saved_region_depth = self.region_depth;
        self.region_depth = 0;
        // The stream/hole bookkeeping is per-function too: `tmp` resets below,
        // so alloca names collide with the enclosing body's — a colliding name
        // would let an outer loop's closer resolve the wrong element type.
        let saved_stream = std::mem::take(&mut self.stream_slots);
        let saved_holes = std::mem::take(&mut self.hole_slots);
        // A lifted body is a different function with a different boundary: the
        // enclosing expression's expected types say nothing about it (and
        // `construct_fnval_lambda` has already taken this stack away across
        // the lift), so it starts empty and re-states its own below.
        let saved_expect = std::mem::take(&mut self.expect);
        self.tmp = 0;
        self.label = 0;

        // Signature: captures first, then the lambda parameters. Each is stored
        // into a fresh alloca slot so the body reads them like ordinary locals.
        let mut sig: Vec<String> = Vec::new();
        let mut argn = 0usize;
        for (cn, cty) in cap_names.iter().zip(cap_tys) {
            let ll = self.llt(cty);
            sig.push(format!("{ll} %arg{argn}"));
            let slot = self.declare(cn, cty);
            self.emit(format!("store {ll} %arg{argn}, ptr {slot}"));
            argn += 1;
        }
        for (pn, pty) in params.iter().zip(param_tys) {
            let ll = self.llt(pty);
            sig.push(format!("{ll} %arg{argn}"));
            let slot = self.declare(pn, pty);
            self.emit(format!("store {ll} %arg{argn}, ptr {slot}"));
            argn += 1;
        }

        // Body: an expression yields the return value; a block returns via `return`.
        let ret_ty = match body {
            LambdaBody::Expr(e) => {
                // The body's value is produced AT the lambda's return type, so
                // the expected type is re-stated here: without the push, a body
                // that is itself a lambda literal sees no expected function
                // signature at all (the stack was taken above) and the inner
                // lift fails with a spurious internal error.
                let pushed = want_ret.is_some();
                if let Some(r) = &want_ret {
                    self.expect.push(r.clone());
                }
                let (v, vty) = self.gen_expr(e)?;
                if pushed {
                    self.expect.pop();
                }
                let ret = want_ret.clone().unwrap_or(vty.clone());
                self.fn_ret = ret.clone();
                if self.llt(&ret) == "void" {
                    self.emit_term("ret void".into());
                } else {
                    let (v, cty) = self.coerce(v, &vty, &ret)?;
                    self.emit_term(format!("ret {} {v}", self.llt(&cty)));
                }
                ret
            }
            LambdaBody::Block(b) => {
                let ret = want_ret.clone().unwrap_or(Type::Unit);
                self.fn_ret = ret.clone();
                self.gen_block(b)?;
                if !self.terminated {
                    if self.llt(&ret) == "void" {
                        self.emit_term("ret void".into());
                    } else {
                        self.emit_term("unreachable".into());
                    }
                }
                ret
            }
        };

        // Assemble the definition.
        let retll = self.llt(&ret_ty);
        let mut def = String::new();
        def.push_str(&format!("define {retll} @{sym}({}) {{\n", sig.join(", ")));
        def.push_str("entry:\n");
        for a in &self.allocas {
            def.push_str(a);
            def.push('\n');
        }
        for b in &self.body {
            def.push_str(b);
            def.push('\n');
        }
        def.push_str("}\n\n");

        // Restore the outer emission state.
        self.allocas = saved_allocas;
        self.body = saved_body;
        self.scope = saved_scope;
        self.cur_block = saved_block;
        self.terminated = saved_term;
        self.fn_ret = saved_ret;
        self.tmp = saved_tmp;
        self.label = saved_label;
        self.drop_slots = saved_drop;
        self.cursors = saved_cursors;
        self.placed = saved_placed;
        self.droppable = saved_droppable;
        self.modify_copyout = saved_modify;
        self.fn_bindings = saved_bindings;
        self.append_ok = saved_append_ok;
        self.str_append = saved_str_append;
        self.stream_slots = saved_stream;
        self.hole_slots = saved_holes;
        self.expect = saved_expect;
        self.region_depth = saved_region_depth;

        self.lambda_defs.push((sym.clone(), def));
        Ok((sym, ret_ty))
    }

    // ---- stored function values by defunctionalization (RFC-0037) --------

    /// Deep-normalize a stored-fn signature so structurally identical spellings
    /// register and dispatch as ONE synthesized enum — [`normalize_fn_sig`],
    /// under this body's substitution.
    fn normalize_sig(&self, t: &Type) -> Type {
        normalize_fn_sig(&vyrn_frontend::types::substitute(t, self.subst), self.types)
    }

    /// The expected fn type currently in scope for a lambda literal / bare
    /// function name: the innermost storage boundary's declared type, resolved.
    fn expected_fn_sig(&self) -> Option<Type> {
        let top = self.expect.last()?;
        match self.resolve(top) {
            t @ Type::Fn(..) => Some(self.normalize_sig(&t)),
            _ => None,
        }
    }

    /// Register a defunctionalization variant (deduped on signature + target)
    /// and return its module-global tag.
    fn register_fnval(
        &mut self,
        sig: &Type,
        target_sym: String,
        cap_tys: Vec<Type>,
        tgt_params: Vec<Type>,
        tgt_ret: Type,
    ) -> i64 {
        if let Some(i) = self
            .fnval_variants
            .iter()
            .position(|v| v.sig == *sig && v.target_sym == target_sym)
        {
            return i as i64;
        }
        self.fnval_variants.push(FnValVariant {
            sig: sig.clone(),
            target_sym,
            cap_tys,
            tgt_params,
            tgt_ret,
        });
        (self.fnval_variants.len() - 1) as i64
    }

    /// Build the `{ i64 tag, i64 payload }` aggregate for a variant.
    fn fnval_aggregate(&mut self, tag: i64, payload: &str) -> String {
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {{ i64, i64 }} undef, i64 {tag}, 0"
        ));
        self.emit(format!(
            "{b} = insertvalue {{ i64, i64 }} {a}, i64 {payload}, 1"
        ));
        b
    }

    /// The same aggregate where the tag is an SSA value rather than a literal —
    /// what a copy of an existing `fn` value rebuilds.
    fn fnval_aggregate_v(&mut self, tag: &str, payload: &str) -> String {
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {{ i64, i64 }} undef, i64 {tag}, 0"
        ));
        self.emit(format!(
            "{b} = insertvalue {{ i64, i64 }} {a}, i64 {payload}, 1"
        ));
        b
    }

    /// Construct a stored function value from a lambda literal (RFC-0037): the
    /// captures are loaded HERE (the literal's evaluation site — RFC-0023's
    /// capture-timing lock, verbatim), packed by value into a malloc'd capture
    /// block, and the body is lifted through the SAME `emit_lifted_lambda` the
    /// v1 argument path uses, typed exactly by the slot's signature.
    fn construct_fnval_lambda(&mut self, expr: &Expr) -> Result<(String, Type), String> {
        let sig = self.expected_fn_sig().ok_or_else(|| {
            "internal: a lambda literal reached codegen without an expected \
             function type (RFC-0037)"
                .to_string()
        })?;
        let Expr::Lambda { params, body, .. } = expr else {
            unreachable!()
        };
        let Type::Fn(ptys, ret) = &sig else {
            unreachable!()
        };
        // Free (captured) locals, in first-seen order (v1's collector).
        let locals: std::collections::HashSet<String> = params.iter().cloned().collect();
        let cap_names = self.lambda_captures(body, locals);
        let mut cap_tys = Vec::new();
        let mut cap_vals = Vec::new();
        for cn in &cap_names {
            let (v, cty) = self.emit_capture(cn)?;
            cap_vals.push(v);
            cap_tys.push(cty);
        }
        // The expected-type stack must not leak into the lifted body (its own
        // storage boundaries push their own types).
        let saved_expect = std::mem::take(&mut self.expect);
        let (sym, _) = self.emit_lifted_lambda(
            params,
            body,
            &cap_names,
            &cap_tys,
            ptys,
            Some((**ret).clone()),
        )?;
        self.expect = saved_expect;
        // Pack the captures into a `{ llt(c0), ... }` block on the heap; an
        // empty capture set is payload 0. The block is never freed — the same
        // safe leak every boxed enum payload already is (own.rs precedent).
        let payload = if cap_tys.is_empty() {
            "0".to_string()
        } else {
            let block_ll = format!(
                "{{ {} }}",
                cap_tys
                    .iter()
                    .map(|t| self.llt(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut cur = "undef".to_string();
            for (i, (v, t)) in cap_vals.iter().zip(&cap_tys).enumerate() {
                let ins = self.fresh_tmp();
                let ll = self.llt(t);
                self.emit(format!(
                    "{ins} = insertvalue {block_ll} {cur}, {ll} {v}, {i}"
                ));
                cur = ins;
            }
            let size = self.fresh_tmp();
            let p = self.fresh_tmp();
            self.emit(format!(
                "{size} = ptrtoint ptr getelementptr ({block_ll}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{p} = call ptr @__vyrn_malloc(i64 {size})"));
            self.emit(format!("store {block_ll} {cur}, ptr {p}"));
            let iv = self.fresh_tmp();
            self.emit(format!("{iv} = ptrtoint ptr {p} to i64"));
            iv
        };
        let tag = self.register_fnval(&sig, sym, cap_tys, ptys.clone(), (**ret).clone());
        Ok((self.fnval_aggregate(tag, &payload), sig))
    }

    /// Construct a stored function value from a bare function name (RFC-0037):
    /// an empty-payload variant calling the function directly. The signature is
    /// the slot's when one is expected, else the function's own.
    fn construct_fnval_named(&mut self, name: &str) -> Result<(String, Type), String> {
        let f = self
            .funcs
            .get(name)
            .copied()
            .ok_or_else(|| format!("unbound `{name}`"))?;
        let tgt_params: Vec<Type> = f.params.iter().map(|p| self.normalize_sig(&p.ty)).collect();
        let tgt_ret = self.normalize_sig(&f.ret);
        let sig = self
            .expected_fn_sig()
            .unwrap_or_else(|| Type::Fn(tgt_params.clone(), Box::new(tgt_ret.clone())));
        let tag = self.register_fnval(&sig, fn_sym(name), Vec::new(), tgt_params, tgt_ret);
        Ok((self.fnval_aggregate(tag, "0"), sig))
    }

    /// Construct a stored function value from a `fn`-typed PARAMETER (RFC-0037 ×
    /// RFC-0023): inside a specialized instance the parameter is defunctionalized
    /// — its direct-call target and its capture SSA values (this instance's own
    /// leading extra parameters) are statically known — so storing it materializes
    /// exactly the `{ i64, i64 }` aggregate a lambda/named source builds. The
    /// captures are boxed by value into a heap block (an empty capture set is
    /// payload 0), identical to `construct_fnval_lambda`; the only difference is
    /// the capture values are already-materialized instance parameters rather than
    /// freshly loaded slots. This makes storing a fn-param behave exactly as
    /// calling it — for any signature, scalar or non-scalar payload alike.
    fn construct_fnval_binding(&mut self, b: &FnBinding) -> Result<(String, Type), String> {
        let sig = self.expected_fn_sig().unwrap_or_else(|| {
            self.normalize_sig(&Type::Fn(b.param_tys.clone(), Box::new(b.ret.clone())))
        });
        let cap_tys: Vec<Type> = b.captures.iter().map(|(t, _)| t.clone()).collect();
        let payload = if cap_tys.is_empty() {
            "0".to_string()
        } else {
            let block_ll = format!(
                "{{ {} }}",
                cap_tys
                    .iter()
                    .map(|t| self.llt(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut cur = "undef".to_string();
            for (i, (t, v)) in b.captures.iter().enumerate() {
                let ins = self.fresh_tmp();
                let ll = self.llt(t);
                self.emit(format!(
                    "{ins} = insertvalue {block_ll} {cur}, {ll} {v}, {i}"
                ));
                cur = ins;
            }
            let size = self.fresh_tmp();
            let p = self.fresh_tmp();
            self.emit(format!(
                "{size} = ptrtoint ptr getelementptr ({block_ll}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{p} = call ptr @__vyrn_malloc(i64 {size})"));
            self.emit(format!("store {block_ll} {cur}, ptr {p}"));
            let iv = self.fresh_tmp();
            self.emit(format!("{iv} = ptrtoint ptr {p} to i64"));
            iv
        };
        let tag = self.register_fnval(
            &sig,
            b.target_sym.clone(),
            cap_tys,
            b.param_tys.clone(),
            b.ret.clone(),
        );
        Ok((self.fnval_aggregate(tag, &payload), sig))
    }

    /// Record that signature `sig` is called through a stored value somewhere,
    /// and return its dispatcher's symbol.
    fn fnval_dispatcher_sym(&mut self, sig: &Type) -> String {
        if !self.fnval_dispatch.iter().any(|s| s == sig) {
            self.fnval_dispatch.push(sig.clone());
        }
        mangle_dispatch_sym(sig)
    }

    /// Record that a `Stream<elem>` is released somewhere, and return the symbol
    /// of the function that releases it (RFC-0090 M3).
    ///
    /// One per element type, synthesized beside the fn-value dispatchers for the
    /// same reason they are: a producer gives its cursor slot back by CALLING its
    /// own step, and a step is dispatched by element type. Registering the step's
    /// signature here is what lets a program that only ever `close`s a stream —
    /// never iterating one — still have a dispatcher to call.
    fn stream_closer_sym(&mut self, elem: &Type) -> String {
        let elem = self.resolve(elem);
        if !self.stream_closers.iter().any(|t| *t == elem) {
            self.stream_closers.push(elem.clone());
        }
        self.fnval_dispatcher_sym(&self.normalize_sig(&stream_step_sig(&elem)).clone());
        stream_close_sym(&elem)
    }

    /// Emit one element type's release (RFC-0075 M2b, re-hosted by RFC-0090 M3).
    ///
    /// A buffer hands back the buffer it was given. A producer asks its own step
    /// to release itself — `closing` is true, the step gives its cursor slot back
    /// to `std/stream`'s slab and, if it is a wrapper, closes its source — and
    /// then the step's own capture block is freed. The walk M2c wrote as a loop
    /// over a chain is therefore ordinary Vyrn recursion now, which is what lets
    /// `movecheck` check it: a wrapper that failed to close its source would not
    /// compile.
    fn emit_stream_closer(&mut self, elem: &Type, out: &mut String) -> Result<(), String> {
        let sig = self.normalize_sig(&stream_step_sig(elem));
        let disp = mangle_dispatch_sym(&sig);
        let optll = self.llt(&Type::Option(Box::new(elem.clone())));
        let sym = stream_close_sym(&self.resolve(elem));
        out.push_str(&format!("define void @{sym}(ptr %s) {{\n"));
        out.push_str("entry:\n");
        out.push_str(&format!(
            "  %tp = getelementptr {STREAM_LL}, ptr %s, i64 0, i32 2\n"
        ));
        out.push_str("  %tag = load i64, ptr %tp\n");
        out.push_str("  %isbuf = icmp slt i64 %tag, 0\n");
        out.push_str("  br i1 %isbuf, label %buf, label %stp\n");
        out.push_str("buf:\n");
        out.push_str("  %d = load ptr, ptr %s\n");
        out.push_str("  call void @__vyrn_free(ptr %d)\n");
        out.push_str("  ret void\n");
        out.push_str("stp:\n");
        out.push_str(&format!(
            "  %fp = getelementptr {STREAM_LL}, ptr %s, i64 0, i32 2\n"
        ));
        out.push_str("  %fv = load { i64, i64 }, ptr %fp\n");
        out.push_str(&format!(
            "  %cp = getelementptr {STREAM_LL}, ptr %s, i64 0, i32 4\n"
        ));
        out.push_str("  %cur = load i64, ptr %cp\n");
        out.push_str(&format!(
            "  %gp = getelementptr {STREAM_LL}, ptr %s, i64 0, i32 5\n"
        ));
        out.push_str("  %gen = load i64, ptr %gp\n");
        out.push_str(&format!(
            "  %r = call {optll} @{disp}({{ i64, i64 }} %fv, i64 %cur, i64 %gen, i1 1)\n"
        ));
        // The step's capture block. A stream owns the fn value it was built
        // with — `fromStep` is where it was constructed — so this is the one
        // place that can hand it back. Deep since RFC-0114 §25 round three:
        // the release twin walks the heap captures before the block goes,
        // and a payload of 0 routes to its default arm harmlessly.
        out.push_str("  %ctag = extractvalue { i64, i64 } %fv, 0\n");
        out.push_str("  %pay = extractvalue { i64, i64 } %fv, 1\n");
        out.push_str(&format!(
            "  call void @{FNVAL_RELEASE}(i64 %ctag, i64 %pay)\n"
        ));
        out.push_str("  ret void\n");
        out.push_str("}\n\n");
        Ok(())
    }

    /// Emit a call through a stored function value: args coerce into the
    /// signature's parameter types, then ONE direct call to the signature's
    /// dispatcher — which itself makes only direct calls (the RFC-0023 IR
    /// invariant holds verbatim; no function pointer exists anywhere).
    fn gen_fnval_call(
        &mut self,
        fnval: String,
        sig: &Type,
        args: &[Expr],
    ) -> Result<(String, Type), String> {
        let sig = self.normalize_sig(sig);
        let Type::Fn(ptys, ret) = &sig else {
            return Err("internal: fn-value call on a non-fn type".into());
        };
        let mut arg_ops = vec![format!("{{ i64, i64 }} {fnval}")];
        for (a, pty) in args.iter().zip(ptys) {
            self.expect.push(pty.clone());
            let r = self.gen_expr(a);
            self.expect.pop();
            let (v, vty) = r?;
            let (v, cty) = self.coerce(v, &vty, pty)?;
            arg_ops.push(format!("{} {v}", self.llt(&cty)));
        }
        let sym = self.fnval_dispatcher_sym(&sig);
        let retll = self.llt(ret);
        if retll == "void" {
            self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
            Ok((String::new(), Type::Unit))
        } else {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {retll} @{sym}({})", arg_ops.join(", ")));
            Ok((t, (**ret).clone()))
        }
    }

    /// Emit one signature's dispatcher (RFC-0037): switch on the tag, unpack
    /// the variant's capture block, and DIRECT-call the target. Every call
    /// names an `@symbol`; the default arm is unreachable by construction
    /// (tags only come from registered constructions) and traps defensively.
    /// Emit the module's one derived copy over the defunctionalized enum
    /// (RFC-0037 × RFC-0089 rule 4, Phase 10b, census §16).
    ///
    /// `x.copy()` of a stored `fn` value has to duplicate the capture block, and
    /// the block's size is a property of the TAG, chosen at run time. Nothing at
    /// the copy site can measure it. The defunctionalizer chose those tags and
    /// knows every one's capture types, so the copy is derived HERE, in one
    /// function per module: a switch from tag to block size, then one `malloc`
    /// and one `memcpy`.
    ///
    /// It is the answer to the objection [`crate::own::owns_heap`] used to carry
    /// — "a capture block's layout is per TAG, so a structural copy has nothing
    /// to measure". The layout is per tag; the copy does not need the layout,
    /// only the size, and the switch supplies it.
    ///
    /// The copy is **shallow**: the block, not what the captures point at. A
    /// deep one would be an alias bug rather than a fix, because two lambdas
    /// over one String already build two blocks holding one pointer — so the
    /// release is shallow for the same reason, and the two stay mirrors.
    ///
    /// `internal`, so a module that never copies a `fn` value loses it entirely.
    fn emit_fnval_copy(&mut self, out: &mut String) -> Result<(), String> {
        use std::fmt::Write as _;
        self.allocas.clear();
        self.body.clear();
        self.tmp = 0;
        self.label = 0;
        self.terminated = false;
        self.cur_block = "entry".into();
        // A variant with no captures carries payload 0 and copies to itself,
        // so only the ones that allocate need an arm — and since RFC-0114
        // §25's round three the arm is DEEP: the block owns its heap
        // captures (capture is a take), so a shallow memcpy left two owners
        // of every captured buffer the moment anything copied a fn value.
        let variants = self.fnval_variants.clone();
        let sized: Vec<(usize, Vec<Type>)> = variants
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.cap_tys.is_empty())
            .map(|(i, v)| (i, v.cap_tys.clone()))
            .collect();
        if sized.is_empty() {
            writeln!(
                out,
                "define internal i64 @{FNVAL_COPY}(i64 %tag, i64 %pay) {{"
            )
            .unwrap();
            out.push_str("entry:\n  ret i64 %pay\n}\n\n");
            return Ok(());
        }
        let arms: Vec<String> = sized
            .iter()
            .map(|(i, _)| format!("i64 {i}, label %cp.v{i}"))
            .collect();
        self.emit_term(format!(
            "switch i64 %tag, label %cp.share [ {} ]",
            arms.join(" ")
        ));
        for (i, caps) in &sized {
            self.emit_label(&format!("cp.v{i}"));
            let block_ll = format!(
                "{{ {} }}",
                caps.iter()
                    .map(|t| self.llt(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let size = self.fresh_tmp();
            let o = self.fresh_tmp();
            let n = self.fresh_tmp();
            self.emit(format!(
                "{size} = ptrtoint ptr getelementptr ({block_ll}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{o} = inttoptr i64 %pay to ptr"));
            self.emit(format!("{n} = call ptr @__vyrn_malloc(i64 {size})"));
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {n}, ptr {o}, i64 {size}, i1 false)"
            ));
            for (j, cty) in caps.iter().enumerate() {
                if !self.owns_heap(cty) {
                    continue;
                }
                let fp = self.fresh_tmp();
                let cll = self.llt(cty);
                self.emit(format!(
                    "{fp} = getelementptr {block_ll}, ptr {n}, i64 0, i32 {j}"
                ));
                let cv = self.fresh_tmp();
                self.emit(format!("{cv} = load {cll}, ptr {fp}"));
                let cv2 = self.deep_copy(&cv, cty)?;
                self.emit(format!("store {cll} {cv2}, ptr {fp}"));
            }
            let r = self.fresh_tmp();
            self.emit(format!("{r} = ptrtoint ptr {n} to i64"));
            self.emit_term(format!("ret i64 {r}"));
        }
        self.emit_label("cp.share");
        self.emit_term("ret i64 %pay".into());
        writeln!(
            out,
            "define internal i64 @{FNVAL_COPY}(i64 %tag, i64 %pay) {{"
        )
        .unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for b in &self.body {
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n\n");
        Ok(())
    }

    /// The release twin (RFC-0114 §25 round three): walk a fn value's heap
    /// captures — a String snapshot, a nested fn value's own block — and then
    /// free the block. One global function, because tags are global; a tag
    /// with no captures routes to the default and releases nothing, which is
    /// also what makes it safe to call with payload 0.
    fn emit_fnval_release(&mut self, out: &mut String) -> Result<(), String> {
        use std::fmt::Write as _;
        self.allocas.clear();
        self.body.clear();
        self.tmp = 0;
        self.label = 0;
        self.terminated = false;
        self.cur_block = "entry".into();
        let variants = self.fnval_variants.clone();
        let sized: Vec<(usize, Vec<Type>)> = variants
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.cap_tys.is_empty())
            .map(|(i, v)| (i, v.cap_tys.clone()))
            .collect();
        if sized.is_empty() {
            writeln!(
                out,
                "define internal void @{FNVAL_RELEASE}(i64 %tag, i64 %pay) {{"
            )
            .unwrap();
            out.push_str("entry:\n  ret void\n}\n\n");
            return Ok(());
        }
        let arms: Vec<String> = sized
            .iter()
            .map(|(i, _)| format!("i64 {i}, label %rl.v{i}"))
            .collect();
        self.emit_term(format!(
            "switch i64 %tag, label %rl.none [ {} ]",
            arms.join(" ")
        ));
        for (i, caps) in &sized {
            self.emit_label(&format!("rl.v{i}"));
            let block_ll = format!(
                "{{ {} }}",
                caps.iter()
                    .map(|t| self.llt(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let o = self.fresh_tmp();
            self.emit(format!("{o} = inttoptr i64 %pay to ptr"));
            for (j, cty) in caps.iter().enumerate() {
                if !self.owns_heap(cty) {
                    continue;
                }
                let fp = self.fresh_tmp();
                let cll = self.llt(cty);
                self.emit(format!(
                    "{fp} = getelementptr {block_ll}, ptr {o}, i64 0, i32 {j}"
                ));
                let cv = self.fresh_tmp();
                self.emit(format!("{cv} = load {cll}, ptr {fp}"));
                self.deep_release(&cv, cty)?;
            }
            self.emit(format!("call void @__vyrn_free(ptr {o})"));
            self.emit_term("ret void".into());
        }
        self.emit_label("rl.none");
        self.emit_term("ret void".into());
        writeln!(
            out,
            "define internal void @{FNVAL_RELEASE}(i64 %tag, i64 %pay) {{"
        )
        .unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for b in &self.body {
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n\n");
        Ok(())
    }

    fn emit_fnval_dispatcher(&mut self, sig: &Type, out: &mut String) -> Result<(), String> {
        let Type::Fn(ptys, ret) = sig else {
            return Err("internal: dispatcher for a non-fn type".into());
        };
        let sym = mangle_dispatch_sym(sig);
        let retll = self.llt(ret);
        self.allocas.clear();
        self.body.clear();
        self.tmp = 0;
        self.label = 0;
        self.terminated = false;
        self.cur_block = "entry".into();
        self.fn_ret = (**ret).clone();

        let mut sig_ll: Vec<String> = vec!["{ i64, i64 } %fv".into()];
        for (i, p) in ptys.iter().enumerate() {
            sig_ll.push(format!("{} %a{i}", self.llt(p)));
        }
        let tag = self.fresh_tmp();
        let pl = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {{ i64, i64 }} %fv, 0"));
        self.emit(format!("{pl} = extractvalue {{ i64, i64 }} %fv, 1"));
        let variants: Vec<(usize, FnValVariant)> = self
            .fnval_variants
            .iter()
            .enumerate()
            .filter(|(_, v)| v.sig == *sig)
            .map(|(i, v)| (i, v.clone()))
            .collect();
        let bad = "fnval.bad".to_string();
        let arms: Vec<String> = variants
            .iter()
            .map(|(i, _)| format!("i64 {i}, label %fnval.v{i}"))
            .collect();
        self.emit_term(format!(
            "switch i64 {tag}, label %{bad} [ {} ]",
            arms.join(" ")
        ));
        for (i, v) in &variants {
            self.emit_label(&format!("fnval.v{i}"));
            let mut ops: Vec<String> = Vec::new();
            // Unpack captures (a lifted lambda's leading parameters).
            if !v.cap_tys.is_empty() {
                let block_ll = format!(
                    "{{ {} }}",
                    v.cap_tys
                        .iter()
                        .map(|t| self.llt(t))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let p = self.fresh_tmp();
                self.emit(format!("{p} = inttoptr i64 {pl} to ptr"));
                let blk = self.fresh_tmp();
                self.emit(format!("{blk} = load {block_ll}, ptr {p}"));
                for (ci, ct) in v.cap_tys.iter().enumerate() {
                    let cv = self.fresh_tmp();
                    self.emit(format!("{cv} = extractvalue {block_ll} {blk}, {ci}"));
                    ops.push(format!("{} {cv}", self.llt(ct)));
                }
            }
            // Ordinary arguments, coerced into the TARGET's own parameter types
            // (a named source's declared types may differ — validated scalars
            // re-validate, records re-layout — mirroring the interpreter's call
            // boundary coercion).
            for (ai, pty) in ptys.iter().enumerate() {
                let tp = v.tgt_params.get(ai).unwrap_or(pty).clone();
                let (cv, cty) = self.coerce(format!("%a{ai}"), pty, &tp)?;
                ops.push(format!("{} {cv}", self.llt(&cty)));
            }
            let tgt_retll = self.llt(&v.tgt_ret);
            if retll == "void" {
                if tgt_retll == "void" {
                    self.emit(format!("call void @{}({})", v.target_sym, ops.join(", ")));
                } else {
                    // A Unit-signature slot may store a value-returning named
                    // function (the result is discarded, like a Unit lambda).
                    let t = self.fresh_tmp();
                    self.emit(format!(
                        "{t} = call {tgt_retll} @{}({})",
                        v.target_sym,
                        ops.join(", ")
                    ));
                }
                self.emit_term("ret void".into());
            } else {
                let t = self.fresh_tmp();
                self.emit(format!(
                    "{t} = call {tgt_retll} @{}({})",
                    v.target_sym,
                    ops.join(", ")
                ));
                let (rv, rty) = self.coerce(t, &v.tgt_ret, ret)?;
                self.emit_term(format!("ret {} {rv}", self.llt(&rty)));
            }
        }
        self.emit_label(&bad);
        self.emit("call void @__vyrn_trap_msg(ptr @.fnval.bad)".into());
        self.emit_term("unreachable".into());

        writeln!(out, "define {retll} @{sym}({}) {{", sig_ll.join(", ")).unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for b in &self.body {
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n\n");
        Ok(())
    }

    /// Emit a specialized instance of a higher-order function (RFC-0023): its
    /// ordinary parameters, then the capture parameters for each `fn`-typed
    /// parameter, with `fn_bindings` wired so calls to those parameters become
    /// direct calls to their targets.
    fn ho_function(&mut self, inst: &HoInst, out: &mut String) -> Result<(), String> {
        let callee: &Function = self.funcs[inst.name.as_str()];
        self.cur_fn_name = inst.name.clone();
        self.lambda_counter = 0;
        self.begin_body(&inst.name);
        self.hole_slots.clear();
        self.append_ok = append_candidates(&callee.body);
        self.str_append.clear();
        self.fn_ret = vyrn_frontend::types::substitute(&callee.ret, &self.subst_clone());
        self.fn_bindings.clear();

        let mut sig: Vec<String> = Vec::new();
        let mut argn = 0usize;
        // Ordinary parameters.
        for p in callee.params.iter() {
            if matches!(p.ty, Type::Fn(..)) {
                continue;
            }
            let ll = self.llt(&p.ty);
            sig.push(format!("{ll} %arg{argn}"));
            let slot = self.declare(&p.name, &p.ty);
            self.emit(format!("store {ll} %arg{argn}, ptr {slot}"));
            argn += 1;
        }
        // Capture parameters + `fn` bindings, in `fn`-parameter order.
        for b in &inst.bindings {
            let mut caps: Vec<(Type, String)> = Vec::new();
            for cty in &b.capture_tys {
                let ll = self.llt(cty);
                sig.push(format!("{ll} %arg{argn}"));
                caps.push((cty.clone(), format!("%arg{argn}")));
                argn += 1;
            }
            self.fn_bindings.insert(
                b.param_name.clone(),
                FnBinding {
                    target_sym: b.target_sym.clone(),
                    captures: caps,
                    param_tys: b.param_tys.clone(),
                    ret: b.ret.clone(),
                },
            );
        }

        self.gen_block(&callee.body)?;
        if !self.terminated {
            if self.llt(&self.fn_ret.clone()) == "void" {
                self.emit_term("ret void".into());
            } else {
                self.emit_term("unreachable".into());
            }
        }

        let retll = self.llt(&self.fn_ret.clone());
        writeln!(out, "define {retll} @{}({}) {{", inst.sym, sig.join(", ")).unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        // Same one-frame-of-the-budget instrumentation [`Gen::function`] emits:
        // a specialization that calls itself re-enters `gen_ho_call` and lands
        // back HERE, so without it a runaway recursion segfaults natively where
        // the interpreter traps with the call-depth error.
        out.push_str("  call void @__vyrn_call_enter()\n");
        for b in &self.body {
            if b.trim_start().starts_with("ret ") || b.trim_start() == "ret void" {
                out.push_str("  call void @__vyrn_call_exit()\n");
            }
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n");
        Ok(())
    }

    /// Clone the current generic substitution (used where an owned copy is needed).
    fn subst_clone(&self) -> HashMap<String, Type> {
        self.subst.clone()
    }

    /// Lower one code-quote operation (RFC-0054) to its `vyrn_gen` host import
    /// (RFC-0076 M3a). A `Code` value is an i64 handle into the host's arena and
    /// nothing here knows what a piece is — which is the point: `render_code`
    /// and the splice table exist once, in the interpreter, and both engines run
    /// that one copy.
    /// A call, then the release of every argument temporary it is finished with.
    ///
    /// The census's rule (`rfcs/census-call-arguments.md` §8): a heap-owning
    /// value the ARGUMENT EXPRESSION built has no binding, so `own` — which keys
    /// every release on a `let` — has nothing to write a row against, and
    /// `width(label(i))` leaked 48 bytes a turn where `let s = label(i)` on the
    /// line above did not. The consumer is the only place that knows the
    /// temporary exists AND knows the callee is done with it, so the release
    /// goes here. Which arguments those are is `own`'s answer and not this
    /// backend's: it stands aside at a `consume` position, at a constructor, at
    /// a position `movecheck::note_retention` recorded, and wherever no
    /// signature is visible.
    ///
    /// The mark is what makes it nest. `f(g(h(x)))` frees `h`'s result at `g`
    /// and `g`'s at `f`, because the inner call takes back only what was pushed
    /// after its own mark.
    fn gen_call(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
        let mark = self.arg_frees.len();
        let r = self.gen_call_inner(name, args);
        for (v, ty) in self.arg_frees.split_off(mark) {
            self.free_arg_temp(&v, &ty);
        }
        r
    }

    /// Release one argument temporary, by its TYPE (RFC-0114 M1).
    ///
    /// The String case is the historical fast path. Everything else spills the
    /// SSA value to a fresh alloca and hands the slot to [`Gen::emit_drop`] —
    /// the block-exit machinery — so there is exactly one spelling of every
    /// release walk and this adapter adds none. The kind comes from the same
    /// `Owned::release_kind` table the analysis consulted when it recorded the
    /// temporary, so the two cannot disagree about what the type owns.
    ///
    /// A block that has already branched takes no more instructions —
    /// `panic("a" + b)` ends its block, and the free would be text after a
    /// terminator. The value is unreachable there anyway.
    fn free_arg_temp(&mut self, v: &str, ty: &Type) {
        if self.terminated {
            return;
        }
        match self.owned.release_kind(ty) {
            Some(DropKind::FreeStr) => {
                self.emit(format!("call void @__vyrn_str_free(ptr {v})"));
            }
            Some(kind) => {
                let ll = self.llt(ty);
                let slot = self.fresh_alloca(&ll);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                self.emit_drop(&slot, &kind);
            }
            // The analysis recorded a temporary whose type owns nothing —
            // nothing to do, and not worth a panic: the record is harmless.
            None => {}
        }
    }

    /// RFC-0114 Rule N: release the bindings the OTHER branch of this `if`
    /// consumed, on the edge where they are still this frame's. `edge` picks
    /// the edge: 0/1 for an `if`'s then/else, the arm's source index for a
    /// `match`. A declared `impl Owned` release is skipped: its body is user
    /// code whose timing all three engines must agree on, and an edge is a
    /// place none of them ran it before — the RFC refuses to normalize it.
    /// Inside a `region` the memory is the arena's, as everywhere else.
    fn emit_edge_releases(&mut self, ers: &[(String, u32)], edge: u32) {
        if self.region_depth != 0 {
            return;
        }
        for (name, t) in ers {
            if *t != edge {
                continue;
            }
            // `d.line` (RFC-0125 M3): the sub-place the other edge took,
            // released here through its own address inside the binding.
            let mut parts = name.split('.');
            let root = parts.next().unwrap_or_default();
            let Some((mut slot, mut ty)) = self.lookup(root) else {
                continue;
            };
            let mut sub = false;
            for f in parts {
                let Some(fields) = self.record_fields(&ty) else {
                    break;
                };
                let Some(idx) = fields.iter().position(|x| x.name == f) else {
                    break;
                };
                let rll = self.llt(&ty);
                let fp = self.fresh_tmp();
                self.emit(format!(
                    "{fp} = getelementptr {rll}, ptr {slot}, i32 0, i32 {idx}"
                ));
                slot = fp;
                ty = fields[idx].ty.clone();
                sub = true;
            }
            let Some(kind) = self.owned.release_kind(&ty) else {
                continue;
            };
            if matches!(kind, DropKind::Release(..)) && !sub {
                continue;
            }
            self.emit_drop(&slot, &kind);
        }
    }

    /// `emit_drop` with a hole set of the row's own (RFC-0125 M3): the arm
    /// table's binder row, whose arm handed part of the binder out.
    fn emit_drop_holed(&mut self, slot: &str, kind: &DropKind, holes: Vec<String>) {
        if holes.is_empty() {
            return self.emit_drop(slot, kind);
        }
        let prev = self.hole_slots.insert(slot.to_string(), holes);
        self.emit_drop(slot, kind);
        match prev {
            Some(h) => self.hole_slots.insert(slot.to_string(), h),
            None => self.hole_slots.remove(slot),
        };
    }

    fn gen_call_inner(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
        // RFC-0078 M4c: a builtin whose implementation IS a Vyrn function lowers as
        // a call to it. The loader injected the module (reserved `$` spellings, so
        // nothing can collide with or capture them) and this emitter holds no
        // implementation of its own — the ~520 lines of hand-written IR the six
        // codecs used to need are gone, and so is the UTF-8 decoder `chars` had.
        if let Some(rt) = vyrn_frontend::loader::routed_builtin(name) {
            if !self.funcs.contains_key(rt) {
                // Loudly, and naming the reason — the same refusal `toJson` makes
                // when its serializer is not in the link (RFC-0078 M2b).
                return Err(format!(
                    "`{name}` is implemented in Vyrn (`{rt}`) and its module is not in the link \
                     — a std root is needed to compile a call to it"
                ));
            }
            return self.gen_call(rt, args);
        }
        // RFC-0094 M3: a type the language cannot render renders itself. `@str`
        // BECOMES the `show` call — its result is the fresh owned String
        // `toString` promises — while `print` and `value` take that String as
        // their argument and keep their one lowering.
        if matches!(name, "print" | "@str" | "value") && args.len() == 1 {
            if let Some(m) = self
                .static_ty(&args[0])
                .and_then(|t| self.show_dispatch(&t))
            {
                if name == "@str" {
                    return self.gen_call(&m, args);
                }
                // `print` of a rendered value: the show's result is a fresh
                // owned String (rule 3) whose whole life is the one write —
                // and the synthesized call node has no plan row, so nothing
                // else would ever free it (round thirty-five, the float
                // print's twin). `value` keeps the plain path: it BOXES the
                // pointer and owns it.
                if name == "print" {
                    let (sv, _) = self.gen_call(&m, args)?;
                    self.emit(format!(
                        "call i32 (ptr, ...) @printf(ptr @.fmt.s, ptr {sv})"
                    ));
                    self.emit(format!("call void @__vyrn_str_free(ptr {sv})"));
                    return Ok(("".into(), Type::Unit));
                }
                let rendered = [Expr::Call {
                    name: m,
                    args: args.to_vec(),
                    line: 0,
                }];
                return self.gen_call(name, &rendered);
            }
        }
        // Calling a `fn`-typed parameter inside a specialized instance (RFC-0023):
        // a direct call to the monomorphized target with the captured values (this
        // instance's own extra parameters) prepended. No function pointer exists.
        if let Some(b) = self.fn_bindings.get(name).cloned() {
            let mut arg_ops: Vec<String> = b
                .captures
                .iter()
                .map(|(ty, v)| format!("{} {v}", self.llt(ty)))
                .collect();
            for (i, a) in args.iter().enumerate() {
                let (v, vty) = self.gen_expr(a)?;
                let (v, cty) = match b.param_tys.get(i) {
                    Some(p) => self.coerce(v, &vty, p)?,
                    None => (v, vty),
                };
                arg_ops.push(format!("{} {v}", self.llt(&cty)));
            }
            let retll = self.llt(&b.ret);
            return if retll == "void" {
                self.emit(format!(
                    "call void @{}({})",
                    b.target_sym,
                    arg_ops.join(", ")
                ));
                Ok((String::new(), Type::Unit))
            } else {
                let t = self.fresh_tmp();
                self.emit(format!(
                    "{t} = call {retll} @{}({})",
                    b.target_sym,
                    arg_ops.join(", ")
                ));
                Ok((t, b.ret.clone()))
            };
        }
        // A call through a stored fn-typed binding (RFC-0037): load the enum
        // value and dispatch through the signature's synthesized dispatcher —
        // one direct call; the switch + direct calls live inside it.
        if let Some((slot, ty)) = self.lookup(name) {
            let rty = self.resolve(&ty);
            if matches!(rty, Type::Fn(..)) {
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load {{ i64, i64 }}, ptr {slot}"));
                return self.gen_fnval_call(v, &rty, args);
            }
        }
        // A call to a function that takes `fn`-typed parameters (RFC-0023): resolve
        // each function-value argument, specialize the callee per those targets, and
        // emit a direct call to the specialized instance with captures appended.
        if let Some(callee) = self.funcs.get(name).copied() {
            if !callee.is_extern && callee.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
                return self.gen_ho_call(callee, args);
            }
        }
        // `schemaOf(TypeName)` reflects a type at compile time — build its Schema
        // literal from the type declaration and lower that (identical to interp).
        if name == "schemaOf" {
            let sl = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => {
                    vyrn_frontend::types::schema_struct_lit(&self.types[tn])
                }
                _ => return Err("`schemaOf` needs a declared type name".to_string()),
            };
            return self.gen_expr(&sl);
        }
        // `jsonSchema(TypeName)` renders the type as a JSON Schema string at compile
        // time — the same string the interpreter builds (seeded into the pool by
        // `collect_strings_expr`), so parity holds.
        if name == "jsonSchema" {
            let json = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => {
                    vyrn_frontend::types::json_schema_string(&self.types[tn], self.types)
                }
                _ => return Err("`jsonSchema` needs a declared type name".to_string()),
            };
            return self.gen_expr(&Expr::Str(json));
        }
        // `toJson(x)` (RFC-0078 M2b): the type-directed half is a shared AST
        // builder, and the serializer is `std/json`'s `emit` — Vyrn, injected into
        // the link. So there is no encoder here at all: the call becomes an
        // ordinary expression and lowers like any other. This is what `schemaOf`
        // does one size up, and it is why the direct backend needed no lowering of
        // its own.
        if name == "toJson" {
            let line = Expr::line(&args[0]);
            // Lowered here rather than inside the built expression, so the argument
            // is evaluated exactly once and its static type comes from the lowering
            // that already computes it (this backend has no type-only peek). The
            // binding's name has a `$` in it, so it cannot shadow anything a
            // program can spell.
            let (v, vty) = self.gen_expr(&args[0])?;
            let enc = vyrn_frontend::jsonenc::enc_name(&vty);
            if !self.funcs.contains_key(enc.as_str()) {
                return Err(format!(
                    "`toJson` on `{vty}`: the JSON runtime is not linked. It is injected \
                     into any program that mentions `toJson` (RFC-0078 M2b), so this is a \
                     program built without a std root, or one whose argument type the \
                     checker did not see"
                ));
            }
            let ll = self.llt(&vty);
            let slot = self.declare("json$arg", &vty);
            self.emit(format!("store {ll} {v}, ptr {slot}"));
            let arg = Expr::Var {
                name: "json$arg".to_string(),
                line,
            };
            let e = vyrn_frontend::jsonenc::encode_expr(arg, &vty, line);
            return self.gen_expr(&e);
        }
        // `fromJson(TypeName, s)` (RFC-0078 M3): the type-directed half is a shared
        // AST builder and the reader is `std/jsonread` — Vyrn, injected into the
        // link. So there is no decoder here at all, and no `__vyrn_vj_*` DOM: the
        // call becomes an ordinary expression and lowers like any other.
        if name == "fromJson" {
            let line = Expr::line(&args[1]);
            let tn = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => tn.clone(),
                _ => return Err("`fromJson` needs a declared type name".to_string()),
            };
            let target = Type::Named(tn);
            let top = vyrn_frontend::jsondec::top_name(&target);
            if !self.funcs.contains_key(top.as_str()) {
                return Err(format!(
                    "`fromJson` into `{target}`: the JSON runtime is not linked. It is                      injected into any program that mentions `fromJson` (RFC-0078 M3), so                      this is a program built without a std root"
                ));
            }
            let e = vyrn_frontend::jsondec::decode_expr(&target, args[1].clone(), line);
            // RFC-0114 §26: the rewrite EMBEDS a clone of the payload
            // argument — the direct backend's twin comment. Scoped: the tree
            // dies with this call, and a stale alias would fire on whatever
            // later node reuses the address.
            let mark = self.plan.alias_scope();
            let mut pairs = Vec::new();
            vyrn_frontend::ast::alias_embedded(&e, &args[1], &mut pairs);
            self.plan.alias_clones_scoped(&pairs);
            let r = self.gen_expr(&e);
            self.plan.alias_unwind(mark);
            return r;
        }
        // Numeric conversion `Int32(x)`, `Float64(x)`, ...
        if let Some(target) = vyrn_frontend::types::numeric_conv_target(name) {
            if args.len() == 1 {
                let (v, sty) = self.gen_expr(&args[0])?;
                return self.gen_numeric_conv(v, &sty, &target);
            }
        }
        // Vector construction, splat and lane read (RFC-0083 M1). Construction is
        // four `insertelement`s into a `zeroinitializer` rather than into `undef`:
        // every lane IS written, so `undef` would be equivalent, and a start that
        // is a defined value is one less thing a reader has to prove.
        if matches!(
            name,
            "F32x4"
                | "@f32x4Splat"
                | "I32x4"
                | "@i32x4Splat"
                | "F64x2"
                | "@f64x2Splat"
                | "@lane"
                | "@replaceLane"
        ) {
            // The width these share, and the three spellings it settles: the
            // vector's IR type, its lane's, and how many lanes there are. M3's
            // whole shape was the first two; M4 added the third, because
            // `insertelement` and `extractelement` do not care which width they
            // are in but a SPLAT has to know how many times to write.
            let wide = name.starts_with("@f64x2") || name == "F64x2";
            let int = name.starts_with("@i32x4") || name == "I32x4";
            let (vec_ty, vt, lt, lane_ty, n) = if int {
                (Type::I32x4, "<4 x i32>", "i32", INT32, 4)
            } else if wide {
                (Type::F64x2, "<2 x double>", "double", Type::Float, 2)
            } else {
                (Type::F32x4, "<4 x float>", "float", Type::Float32, 4)
            };
            // The two lane accessors read their WIDTH off the receiver rather than
            // off the name, because they are value methods and one arm serves
            // every width.
            let of_recv = |ty: &Type| -> (&'static str, &'static str, Type, i64) {
                match ty {
                    Type::I32x4 => ("<4 x i32>", "i32", INT32, 4),
                    Type::F64x2 => ("<2 x double>", "double", Type::Float, 2),
                    Type::Mask64x2 => ("<2 x i64>", "i64", Type::Bool, 2),
                    Type::Mask32x4 => ("<4 x i32>", "i32", Type::Bool, 4),
                    _ => ("<4 x float>", "float", Type::Float32, 4),
                }
            };
            if name == "@replaceLane" {
                let (v, vty) = self.gen_expr(&args[0])?;
                let (vt, lt, lane_ty, lanes) = of_recv(&self.resolve(&vty));
                let k = vyrn_frontend::types::const_lane(&args[1], lanes)
                    .ok_or("a lane index must be a compile-time constant")?;
                let (x, xt) = self.gen_expr(&args[2])?;
                let (x, _) = self.coerce(x, &xt, &lane_ty)?;
                let t = self.fresh_tmp();
                self.emit(format!("{t} = insertelement {vt} {v}, {lt} {x}, i32 {k}"));
                return Ok((t, self.resolve(&vty)));
            }
            if name == "@lane" {
                let (v, vty) = self.gen_expr(&args[0])?;
                let (vt, lt, out, lanes) = of_recv(&self.resolve(&vty));
                // Proven constant and in range by the checker, so no bounds check
                // is emitted here and none is missing.
                let k = vyrn_frontend::types::const_lane(&args[1], lanes)
                    .ok_or("a lane index must be a compile-time constant")?;
                let t = self.fresh_tmp();
                // A mask lane is `0` or `-1`; `Bool` is `i1`, so the read is an
                // extract plus a test against zero rather than a truncation —
                // `trunc i32 -1 to i1` is `true` only because the low bit is set,
                // which is an accident of the encoding rather than its meaning.
                if out == Type::Bool {
                    let e = self.fresh_tmp();
                    self.emit(format!("{e} = extractelement {vt} {v}, i32 {k}"));
                    self.emit(format!("{t} = icmp ne {lt} {e}, 0"));
                    return Ok((t, Type::Bool));
                }
                self.emit(format!("{t} = extractelement {vt} {v}, i32 {k}"));
                return Ok((t, out));
            }
            let mut lanes = Vec::new();
            for a in args {
                let (v, ty) = self.gen_expr(a)?;
                let (v, _) = self.coerce(v, &ty, &lane_ty)?;
                lanes.push(v);
            }
            // A splat writes the one value into every lane. `shufflevector`
            // would be the idiom; the `insertelement` chain is the same
            // instruction sequence after `-O3` and shares this path instead of
            // forking it.
            if lanes.len() == 1 {
                lanes = vec![lanes[0].clone(); n];
            }
            let mut acc = "zeroinitializer".to_string();
            for (i, l) in lanes.iter().enumerate() {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = insertelement {vt} {acc}, {lt} {l}, i32 {i}"));
                acc = t;
            }
            return Ok((acc, vec_ty));
        }
        // `m.anyTrue()` / `m.allTrue()` (RFC-0083 M2). `icmp ne` first, then an
        // or/and reduction: the same "test each lane against zero" the mask lane
        // read above does, for the same reason — the all-ones encoding is how the
        // mask is stored, not what it means, and a `bitcast` to `i128` compared
        // against `-1` would be reading the storage. `-O2` folds both spellings to
        // the same `movmskps`, so the readable one costs nothing.
        if matches!(name, "@anyTrue" | "@allTrue") {
            let (mv, mty) = self.gen_expr(&args[0])?;
            // Which mask decides the vector width and the reduction's suffix, and
            // nothing else: the "test each lane against zero, then fold" shape is
            // the same one at both.
            let (vt, v) = if self.resolve(&mty) == Type::Mask64x2 {
                ("<2 x i64>", "v2i1")
            } else {
                ("<4 x i32>", "v4i1")
            };
            let ne = self.fresh_tmp();
            let t = self.fresh_tmp();
            self.emit(format!("{ne} = icmp ne {vt} {mv}, zeroinitializer"));
            let op = if name == "@anyTrue" { "or" } else { "and" };
            self.emit(format!(
                "{t} = call i1 @llvm.vector.reduce.{op}.{v}(<{} x i1> {ne})",
                if v == "v2i1" { 2 } else { 4 }
            ));
            return Ok((t, Type::Bool));
        }
        // `F32x4.min`/`max`/`sqrt` and the four roundings (RFC-0083 M2/M3). The
        // intrinsics are declared once in the prologue, where the choice of
        // `llvm.minimum` over `llvm.minnum` is argued. (`@f32x4Abs` was here as
        // `llvm.fabs.v4f32` and was deleted in M4 — 1.00x native, 1.07x wasm once
        // the Vyrn version has no helper call in it.)
        if matches!(
            name,
            "@f32x4Min"
                | "@f32x4Max"
                | "@f32x4Sqrt"
                | "@f32x4Ceil"
                | "@f32x4Floor"
                | "@f32x4Trunc"
                | "@f32x4Nearest"
        ) {
            let (a, _) = self.gen_expr(&args[0])?;
            let t = self.fresh_tmp();
            let (f, rest) = match name {
                "@f32x4Min" => ("llvm.minimum.v4f32", true),
                "@f32x4Max" => ("llvm.maximum.v4f32", true),
                "@f32x4Ceil" => ("llvm.ceil.v4f32", false),
                "@f32x4Floor" => ("llvm.floor.v4f32", false),
                "@f32x4Trunc" => ("llvm.trunc.v4f32", false),
                // `llvm.rint`, which is roundTiesToEven under the default rounding
                // mode; see the declaration for why not `llvm.roundeven`.
                "@f32x4Nearest" => ("llvm.rint.v4f32", false),
                _ => ("llvm.sqrt.v4f32", false),
            };
            let second = if rest {
                let (b, _) = self.gen_expr(&args[1])?;
                format!(", <4 x float> {b}")
            } else {
                String::new()
            };
            self.emit(format!(
                "{t} = call <4 x float> @{f}(<4 x float> {a}{second})"
            ));
            return Ok((t, Type::F32x4));
        }
        // The wide width's three (RFC-0083 M4), the same intrinsics at `v2f64`.
        // `llvm.minimum` and not `llvm.minnum` for the reason declared in the
        // prologue — the rule does not change with the lane width, and neither
        // does the way native would drift from wasm if it were left to a default.
        if matches!(name, "@f64x2Min" | "@f64x2Max" | "@f64x2Sqrt") {
            let (a, _) = self.gen_expr(&args[0])?;
            let t = self.fresh_tmp();
            let (f, rest) = match name {
                "@f64x2Min" => ("llvm.minimum.v2f64", true),
                "@f64x2Max" => ("llvm.maximum.v2f64", true),
                _ => ("llvm.sqrt.v2f64", false),
            };
            let second = if rest {
                let (b, _) = self.gen_expr(&args[1])?;
                format!(", <2 x double> {b}")
            } else {
                String::new()
            };
            self.emit(format!(
                "{t} = call <2 x double> @{f}(<2 x double> {a}{second})"
            ));
            return Ok((t, Type::F64x2));
        }
        // (`@i32x4Min`/`Max`/`Abs` were here, as `llvm.smin`/`smax`/`abs.v4i32`,
        // and were deleted on their measurement — LLVM compiles the Vyrn
        // `if a < b` into the same `pminsd`, so the intrinsic bought 1.0x. See the
        // refusal in `checker.rs`'s `vector_call` and RFC-0083's M3 note.)
        //
        // `F32x4.load(xs, i)` / `F32x4.store(xs, i, v)` (RFC-0083 M2) — sixteen
        // bytes at element `i` of an `Array<Float32>`, behind ONE bounds check
        // rather than four. That amortisation is the milestone's point; the
        // structural pin in `vyrn-cli/tests/simd.rs` counts the branch.
        if matches!(
            name,
            "@f32x4Load"
                | "@f32x4Store"
                | "@i32x4Load"
                | "@i32x4Store"
                | "@f64x2Load"
                | "@f64x2Store"
        ) {
            // Element type, vector type and SPAN; the check, the address
            // arithmetic and the trap are identical, which is why the widths share
            // the arm. The span is the only thing M4 added: two `Float64` lanes
            // read two elements, so the limit is `len - 2` and the trap names
            // `i + 1`. The address arithmetic needs no change at all — a
            // `getelementptr` over `double` steps 8 bytes because the element type
            // says so.
            let (et, vt, vec_ty, span) = if name.starts_with("@i32x4") {
                ("i32", "<4 x i32>", Type::I32x4, 4)
            } else if name.starts_with("@f64x2") {
                ("double", "<2 x double>", Type::F64x2, 2)
            } else {
                ("float", "<4 x float>", Type::F32x4, 4)
            };
            let (av, _) = self.gen_expr(&args[0])?;
            let (iv, _) = self.gen_expr(&args[1])?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
            // SIGNED, and against `len - 4` rather than `i + 4` against `len`.
            // The unsigned single-compare trick the scalar path uses does not
            // survive a span: `i + 4` wraps for a huge `i` and lets the load
            // through, while `len - 4` cannot wrap because `len >= 0`. Two
            // compares, one branch — still once for the whole vector.
            let lo = self.fresh_tmp();
            let lim = self.fresh_tmp();
            let hi = self.fresh_tmp();
            let bad = self.fresh_tmp();
            self.emit(format!("{lo} = icmp slt i64 {iv}, 0"));
            self.emit(format!("{lim} = sub nsw i64 {len}, {span}"));
            self.emit(format!("{hi} = icmp sgt i64 {iv}, {lim}"));
            self.emit(format!("{bad} = or i1 {lo}, {hi}"));
            let bad_l = self.fresh_label("vec.oob");
            let ok_l = self.fresh_label("vec.ok");
            self.emit_term(format!("br i1 {bad}, label %{bad_l}, label %{ok_l}"));
            self.emit_label(&bad_l);
            // The reported index is the first lane of `i..i+3` actually out of
            // range — naming `i` would name an in-range element whenever only the
            // tail overruns. Computed in the trap block, so the hot path pays
            // nothing for the nicety.
            let hi3 = self.fresh_tmp();
            let k = self.fresh_tmp();
            self.emit(format!("{hi3} = add i64 {iv}, {}", span - 1));
            self.emit(format!("{k} = select i1 {lo}, i64 {iv}, i64 {hi3}"));
            self.emit(format!(
                "call void @__vyrn_trap_idx(ptr @.trap.aoob, i64 {k})"
            ));
            self.emit_term("unreachable".into());
            self.emit_label(&ok_l);
            let ep = self.fresh_tmp();
            self.emit(format!("{ep} = getelementptr {et}, ptr {data}, i64 {iv}"));
            // `align 4`, not the vector's natural 16: the buffer is an array of
            // elements, so nothing guarantees more, and claiming 16 would be a
            // promise the allocator never made. Understating it is always legal,
            // which is why the wide width does not need its own number here.
            if name.ends_with("Load") {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = load {vt}, ptr {ep}, align 4"));
                return Ok((t, vec_ty));
            }
            let (v, _) = self.gen_expr(&args[2])?;
            self.emit(format!("store {vt} {v}, ptr {ep}, align 4"));
            return Ok(("undef".to_string(), Type::Unit));
        }
        // `blackBox(v)` (RFC-0055): identity with an optimizer-opacity guarantee.
        // The *semantics* — the value is used and its result is unknowable, so the
        // work producing it survives and can't be constant-folded — are what matter;
        // the instruction sequence is free. For a register-class value an identity
        // inline-asm ties the output to the input (`"=r,0"`), the classic
        // divan/criterion `black_box`. For an aggregate (record/array/Ref/…) that a
        // single register can't express, we round-trip through an entry-block slot
        // with a `~{memory}` clobber: the store (the work) can't be dead and the
        // reload can't be folded. Only valid inside `bench`/`test` bodies (the
        // checker enforces placement); by then benches are lifted to ordinary
        // functions, so this lowers like any other call.
        if name == "blackBox" {
            let (v, ty) = self.gen_expr(&args[0])?;
            let llty = self.llt(&ty);
            if llty == "void" {
                // Unit: no value to hold; a bare memory clobber keeps ordering.
                self.emit("call void asm sideeffect \"\", \"~{memory}\"()".to_string());
                return Ok((String::new(), Type::Unit));
            }
            if llty.starts_with('{') || llty.starts_with('[') {
                let slot = self.fresh_alloca(&llty);
                self.emit(format!("store {llty} {v}, ptr {slot}"));
                self.emit(format!(
                    "call void asm sideeffect \"\", \"r,~{{memory}}\"(ptr {slot})"
                ));
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {llty}, ptr {slot}"));
                return Ok((out, ty));
            }
            let out = self.fresh_tmp();
            self.emit(format!(
                "{out} = call {llty} asm sideeffect \"\", \"=r,0\"({llty} {v})"
            ));
            return Ok((out, ty));
        }
        // RFC-0079: `panic(msg)` — the message, then `exit(1)`, which is the
        // trap path [`Emitter::trap_if`] takes minus the branch. The block ends
        // in `unreachable` and a fresh (dead) one opens, so whatever the caller
        // does with the "value" — `phi` it into a join, coerce it, drop it —
        // lands in code no execution reaches. `poison` is that value: valid at
        // every LLVM type, which is what makes a panicking `match` arm need no
        // special case in the merge.
        if vyrn_frontend::ast::is_panic(name) {
            let (v, _) = self.gen_expr(&args[0])?;
            // Census U5: the site is a pooled string literal the loader stamped,
            // so it costs one more operand on the call and nothing per site in
            // code. `null` is a program that reached a backend without the
            // loader — only this crate's own tests do — and the cold tail
            // branches on it rather than a second tail existing.
            let at = match args.get(1) {
                Some(a) => self.gen_expr(a)?.0,
                None => "null".to_string(),
            };
            self.emit(format!("call void @__vyrn_panic(ptr {v}, ptr {at})"));
            self.emit_term("unreachable".into());
            let dead = self.fresh_label("panic.dead");
            self.emit_label(&dead);
            return Ok(("poison".to_string(), Type::Never));
        }
        // RFC-0074 M3a: see `@.trap.serve`. The argument is deliberately NOT
        // generated — the producer it names cannot be pulled here, so evaluating
        // it would only run a step whose values nothing can read.
        if name == "serveStream" {
            self.emit("call void @__vyrn_trap_msg(ptr @.trap.serve)".into());
            self.emit_term("unreachable".into());
            let dead = self.fresh_label("serve.dead");
            self.emit_label(&dead);
            return Ok(("poison".to_string(), Type::Never));
        }
        if name == "print" {
            let (v, ty) = self.gen_expr(&args[0])?;
            match self.resolve(&ty) {
                Type::Bool => {
                    // select the "true"/"false" format string, matching interp
                    let fmt = self.fresh_tmp();
                    self.emit(format!(
                        "{fmt} = select i1 {v}, ptr @.fmt.true, ptr @.fmt.false"
                    ));
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr {fmt})"));
                }
                Type::Str => {
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr @.fmt.s, ptr {v})"));
                }
                // A float prints as its `@str` and a `%s\n`, because `print` and
                // interpolation must spell one value one way and there is now one
                // implementation to spell it with (RFC-0081 M2). The `malloc` this
                // costs that `printf("%f")` did not is real and was measured: on
                // 200,000 `print`s of a float it is inside run-to-run noise, the
                // write being what that program is actually doing.
                ref f @ (Type::Float | Type::Float32) => {
                    let s = self.gen_f64_str(&v, f)?;
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr @.fmt.s, ptr {s})"));
                    // The rendered value is `gen_f64_str`'s fresh allocation and
                    // the printf was its whole life — one block per float print,
                    // 81 of simd's 81 residue rows (exit-residue round
                    // seventeen).
                    self.emit(format!("call void @__vyrn_str_free(ptr {s})"));
                }
                // A signed sized int sign-extends to i64 and prints with `%lld`;
                // an unsigned one zero-extends and prints with `%llu` — same digits
                // the interpreter prints from its logical value. A 64-bit value is
                // already `i64`, so no extension is emitted (it would be invalid).
                Type::IntN { bits, signed } => {
                    let fmt = if signed { "@.fmt.d" } else { "@.fmt.u" };
                    let w = if bits == 64 {
                        v
                    } else {
                        let ext = if signed { "sext" } else { "zext" };
                        let t = self.fresh_tmp();
                        self.emit(format!("{t} = {ext} i{bits} {v} to i64"));
                        t
                    };
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr {fmt}, i64 {w})"));
                }
                _ => {
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr @.fmt.d, i64 {v})"));
                }
            }
            return Ok(("".into(), Type::Unit));
        }

        // logger(String) -> Logger: the handle is its name pointer (RFC-0008).
        if name == "logger" {
            let (v, _) = self.gen_expr(&args[0])?;
            return Ok((v, Type::Logger));
        }
        // Log methods write `[LEVEL] name: msg\n` to stderr via fprintf. Kept off
        // stdout so program output and diagnostics are separable.
        if vyrn_frontend::ast::is_log_level(name) {
            // Evaluate both args regardless (their side effects must match the
            // interpreter, which also evaluates them), but emit the write only
            // when the level meets the configured threshold (RFC-0008).
            let (logv, _) = self.gen_expr(&args[0])?;
            let (msgv, _) = self.gen_expr(&args[1])?;
            if log_level_ordinal(name).unwrap_or(0) >= self.log_level {
                let lvl = format!("@.lvl.{name}");
                let stream = self.fresh_tmp();
                match &self.log_sink {
                    // Stream handles come from the portable C shim.
                    LogSink::Stderr => self.emit(format!("{stream} = call ptr @__vyrn_stderr()")),
                    LogSink::Stdout => self.emit(format!("{stream} = call ptr @__vyrn_stdout()")),
                    // The file is opened once in `@main` (below).
                    LogSink::File(_) => {
                        self.emit(format!("{stream} = load ptr, ptr @__vyrn_log_file"))
                    }
                }
                // A failed fopen left the handle null: fprintf(NULL, …) would
                // crash, so the record is skipped and the program degrades
                // silently, as the interpreter does.
                if matches!(self.log_sink, LogSink::File(_)) {
                    let open_l = self.fresh_label("log.open");
                    let done_l = self.fresh_label("log.done");
                    let live = self.fresh_tmp();
                    self.emit(format!("{live} = icmp ne ptr {stream}, null"));
                    self.emit_term(format!("br i1 {live}, label %{open_l}, label %{done_l}"));
                    self.emit_label(&open_l);
                    self.emit(format!(
                        "call i32 (ptr, ptr, ...) @fprintf(ptr {stream}, ptr @.fmt.log, ptr {lvl}, ptr {logv}, ptr {msgv})"
                    ));
                    self.emit_term(format!("br label %{done_l}"));
                    self.emit_label(&done_l);
                } else {
                    self.emit(format!(
                        "call i32 (ptr, ptr, ...) @fprintf(ptr {stream}, ptr @.fmt.log, ptr {lvl}, ptr {logv}, ptr {msgv})"
                    ));
                }
            }
            return Ok(("".into(), Type::Unit));
        }

        // (`len(String)` was removed; a String's byte length is the `.length`
        // field, lowered at `Expr::Field` via `@__vyrn_strlen`.)
        // (The six text encodings are `std/codecs` — RFC-0078 M4c. They were the
        // only builtins with no C shim at all: ~520 lines of hand-written IR here,
        // routed above and deleted from `ENCODING_RUNTIME`.)
        // The IEEE-754 bit views (RFC-0078 M4a): a `bitcast`, which costs no
        // instruction at all — the value is already in the right 64 bits and
        // only the register class changes.
        if matches!(name, "floatBits" | "floatFromBits") {
            let (v, _) = self.gen_expr(&args[0])?;
            let t = self.fresh_tmp();
            let (fll, tll, ty) = if name == "floatBits" {
                (
                    "double",
                    "i64",
                    Type::IntN {
                        bits: 64,
                        signed: false,
                    },
                )
            } else {
                ("i64", "double", Type::Float)
            };
            self.emit(format!("{t} = bitcast {fll} {v} to {tll}"));
            return Ok((t, ty));
        }
        // bytes(s): a string's raw UTF-8 bytes as an Array<UInt8> (i8 stride —
        // RFC-0014 M2). The VIEW, which is irreducible: `std/codecs`, `std/text`
        // and `std/strpred` are all written on it. (`chars` used to share this arm
        // and is now `std/text`'s `charsV` — RFC-0078 M4c.)
        if matches!(name, "bytes") {
            let (v, _) = self.gen_expr(&args[0])?;
            let elem = Type::IntN {
                bits: 8,
                signed: false,
            };
            let t = self.fresh_tmp();
            if args.len() == 3 {
                // The range form (RFC-0113). Bounds are checked HERE rather than
                // in the helper, because the trap needs the offset that was
                // wrong and the helper has already turned the pair into a
                // length. `soob` is the wording `s[i]` uses, so the catalogue
                // does not grow.
                let (a, _) = self.gen_expr(&args[1])?;
                let (b, _) = self.gen_expr(&args[2])?;
                let len = self.fresh_tmp();
                self.emit(format!("{len} = call i64 @__vyrn_str_len(ptr {v})"));
                let lo_bad = self.fresh_tmp();
                let ord_bad = self.fresh_tmp();
                let hi_bad = self.fresh_tmp();
                let bad1 = self.fresh_tmp();
                let bad = self.fresh_tmp();
                self.emit(format!("{lo_bad} = icmp slt i64 {a}, 0"));
                self.emit(format!("{ord_bad} = icmp slt i64 {b}, {a}"));
                self.emit(format!("{hi_bad} = icmp sgt i64 {b}, {len}"));
                self.emit(format!("{bad1} = or i1 {lo_bad}, {ord_bad}"));
                self.emit(format!("{bad} = or i1 {bad1}, {hi_bad}"));
                let trap_l = self.fresh_label("byr.trap");
                let ok_l = self.fresh_label("byr.ok");
                self.emit_term(format!("br i1 {bad}, label %{trap_l}, label %{ok_l}"));
                self.emit_label(&trap_l);
                // The offset the interpreter names: the low one when it is
                // negative or out of order, otherwise the high one.
                let which = self.fresh_tmp();
                let pick = self.fresh_tmp();
                self.emit(format!("{which} = or i1 {lo_bad}, {ord_bad}"));
                self.emit(format!("{pick} = select i1 {which}, i64 {a}, i64 {b}"));
                self.emit(format!(
                    "call void @__vyrn_trap_idx(ptr @.trap.soob, i64 {pick})"
                ));
                self.emit_term("unreachable".into());
                self.emit_label(&ok_l);
                self.emit(format!(
                    "{t} = call {{ ptr, i64, i64 }} @__vyrn_str_bytes_range(ptr {v}, i64 {a}, i64 {b})"
                ));
            } else {
                self.emit(format!(
                    "{t} = call {{ ptr, i64, i64 }} @__vyrn_str_bytes(ptr {v})"
                ));
            }
            return Ok((t, Type::Array(Box::new(elem))));
        }

        // ---- input I/O (RFC-0014) -----------------------------------------
        // Effects like `print`: the C shim does the syscalls; the IR builds the
        // canonical error payloads (via `@__vyrn_read_err`/`@__vyrn_write_err`
        // and the `@.io.*` globals) so the wording lives in ONE place.
        if name == "args" {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {{ ptr, i64, i64 }} @__vyrn_args()"));
            return Ok((t, Type::Array(Box::new(Type::Str))));
        }
        if name == "readLine" {
            // ptr = __vyrn_read_line(&len): NULL at EOF (or an embedded NUL —
            // unrepresentable in a NUL-terminated String). A non-NULL line is
            // UTF-8-validated with the shared DFA; invalid reads as None too,
            // exactly like the interpreter's `String::from_utf8` failure.
            let lenp = self.fresh_alloca("i64");
            let p = self.fresh_tmp();
            self.emit(format!("{p} = call ptr @__vyrn_read_line(ptr {lenp})"));
            let isnull = self.fresh_tmp();
            self.emit(format!("{isnull} = icmp eq ptr {p}, null"));
            let none_l = self.fresh_label("rl.none");
            let chk_l = self.fresh_label("rl.chk");
            let bad_l = self.fresh_label("rl.bad");
            let ok_l = self.fresh_label("rl.ok");
            let end_l = self.fresh_label("rl.end");
            self.emit_term(format!("br i1 {isnull}, label %{none_l}, label %{chk_l}"));
            self.emit_label(&chk_l);
            let len = self.fresh_tmp();
            let valid = self.fresh_tmp();
            self.emit(format!("{len} = load i64, ptr {lenp}"));
            self.emit(format!(
                "{valid} = call i1 @__vyrn_utf8valid(ptr {p}, i64 {len})"
            ));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{bad_l}"));
            self.emit_label(&bad_l);
            self.emit(format!("call void @__vyrn_str_free(ptr {p})"));
            self.emit_term(format!("br label %{none_l}"));
            self.emit_label(&none_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let w0 = self.fresh_tmp();
            let s0 = self.fresh_tmp();
            let s1 = self.fresh_tmp();
            let s2 = self.fresh_tmp();
            self.emit(format!("{w0} = ptrtoint ptr {p} to i64"));
            self.emit(format!(
                "{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"
            ));
            self.emit(format!(
                "{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {{ i1 0, i64 0, i64 0 }}, %{none_l} ], \
                 [ {s2}, %{ok_l} ]"
            ));
            return Ok((r, Type::Option(Box::new(Type::Str))));
        }
        // `listDir`/`moduleInterface` (RFC-0021) are interpreter/generation-time
        // builtins. `moduleInterface` is compile-time reflection (it never has a
        // runtime value); `listDir`'s primary role is generation-time directory
        // enumeration (mediated through the loader's resolver). Neither has a
        // native/wasm lowering in v1 — a program that reaches one at runtime gets
        // a clear compile error rather than a link failure.
        // `lineAt(bytes, off)` / `colAt(bytes, off)`: the buffer is `{ ptr, i64,
        // i64 }` and `UInt8` is i8-stride, so the data pointer and length go
        // straight to a C helper.
        //
        // The interpreter memoizes a line-start table per buffer (a scanner asks
        // once per node, and counting from byte 0 each time is quadratic); the
        // native helper counts directly. Same answer, which is what parity
        // requires — the cache is an optimization, not a semantic.
        //
        // These need a lowering at all because a library that calls them is
        // COMPILED as a module: `std/vyx` reaches them only from generator code,
        // but codegen emits every function in a linked module regardless. That is
        // why `listDir`/`moduleInterface` can stay comptime-only — they appear
        // directly inside `gen fn` bodies — and these cannot.
        if name == "lineAt" || name == "colAt" {
            let (av, _) = self.gen_expr(&args[0])?;
            let (ov, _) = self.gen_expr(&args[1])?;
            let dptr = self.fresh_tmp();
            let dlen = self.fresh_tmp();
            let r = self.fresh_tmp();
            self.emit(format!("{dptr} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{dlen} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
            let helper = if name == "lineAt" {
                "__vyrn_line_at"
            } else {
                "__vyrn_col_at"
            };
            self.emit(format!(
                "{r} = call i64 @{helper}(ptr {dptr}, i64 {dlen}, i64 {ov})"
            ));
            return Ok((r, Type::Int));
        }
        if name == "listDir" {
            // The language gives `listDir` no runtime meaning (RFC-0021). RFC-0076
            // M2 lowered it behind `emit_gen_host` for the generation engine, and
            // M7 moved that to the direct backend, so this emitter is back to the
            // one thing it ever had to say about it — and the direct backend says
            // it in the same words, out of the same constant.
            return Err(crate::LIST_DIR_NO_LOWERING.to_string());
        }
        if name == "listDirKinds" {
            return Err(crate::LIST_DIR_KINDS_NO_LOWERING.to_string());
        }
        if name == "moduleInterface" {
            return Err(
                "`moduleInterface` is compile-time reflection (RFC-0021) — it is only available \
                 during generation, never at runtime"
                    .to_string(),
            );
        }
        // `contractOf(Name)` is the same kind of thing on the *expectation* side
        // (RFC-0071): a module contract is comptime-only and nothing about it
        // survives into the emitted module, so there is nothing to lower.
        if name == "contractOf" {
            return Err(
                "`contractOf` is compile-time reflection (RFC-0071) — a module contract is only \
                 available during generation, never at runtime"
                    .to_string(),
            );
        }
        if name == "readFile" {
            // status = __vyrn_read_file(path, &buf, &len): 0 ok / 1 io / 3 NUL,
            // then the shared UTF-8 DFA decides status 2. The Err payload is
            // rendered by @__vyrn_read_err from the status.
            let (path, _) = self.gen_expr(&args[0])?;
            let outp = self.fresh_alloca("ptr");
            let lenp = self.fresh_alloca("i64");
            let st = self.fresh_tmp();
            self.emit(format!(
                "{st} = call i32 @__vyrn_read_file(ptr {path}, ptr {outp}, ptr {lenp})"
            ));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let entry_b = self.cur_block.clone();
            let chk_l = self.fresh_label("rf.chk");
            let badutf_l = self.fresh_label("rf.badutf");
            let err_l = self.fresh_label("rf.err");
            let ok_l = self.fresh_label("rf.ok");
            let end_l = self.fresh_label("rf.end");
            self.emit_term(format!("br i1 {isok}, label %{chk_l}, label %{err_l}"));
            self.emit_label(&chk_l);
            let buf = self.fresh_tmp();
            let len = self.fresh_tmp();
            let valid = self.fresh_tmp();
            self.emit(format!("{buf} = load ptr, ptr {outp}"));
            self.emit(format!("{len} = load i64, ptr {lenp}"));
            self.emit(format!(
                "{valid} = call i1 @__vyrn_utf8valid(ptr {buf}, i64 {len})"
            ));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{badutf_l}"));
            self.emit_label(&badutf_l);
            self.emit(format!("call void @__vyrn_str_free(ptr {buf})"));
            self.emit_term(format!("br label %{err_l}"));
            self.emit_label(&err_l);
            let stphi = self.fresh_tmp();
            self.emit(format!(
                "{stphi} = phi i32 [ {st}, %{entry_b} ], [ 2, %{badutf_l} ]"
            ));
            let msg = self.fresh_tmp();
            self.emit(format!(
                "{msg} = call ptr @__vyrn_read_err(ptr {path}, i32 {stphi})"
            ));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let ow = self.fresh_tmp();
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{ow} = ptrtoint ptr {buf} to i64"));
            self.emit(format!(
                "{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {ow}, 1"
            ));
            self.emit(format!(
                "{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], [ {o2}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Str), Box::new(Type::Str))));
        }
        // RFC-0111: the byte sink. Same status protocol as `writeFile` and the
        // same error renderer, so the message is byte-identical to the other
        // engines'. The length is passed because the buffer may hold NULs.
        if name == "writeFileBytes" {
            let (path, _) = self.gen_expr(&args[0])?;
            let (arr, _) = self.gen_expr(&args[1])?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {arr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {arr}, 1"));
            let st = self.fresh_tmp();
            self.emit(format!(
                "{st} = call i32 @__vyrn_write_file_bytes(ptr {path}, ptr {data}, i64 {len})"
            ));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let ok_l = self.fresh_label("wfb.ok");
            let err_l = self.fresh_label("wfb.err");
            let end_l = self.fresh_label("wfb.end");
            self.emit_term(format!("br i1 {isok}, label %{ok_l}, label %{err_l}"));
            self.emit_label(&err_l);
            let msg = self.fresh_tmp();
            self.emit(format!("{msg} = call ptr @__vyrn_write_err(ptr {path})"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], \
                 [ {{ i1 1, i64 1, i64 0 }}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Bool), Box::new(Type::Str))));
        }
        // RFC-0111: `print` for bytes. No status to check — the shim answers
        // nothing, for the reason `print` answers nothing.
        if name == "writeStdout" {
            let (arr, _) = self.gen_expr(&args[0])?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {arr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {arr}, 1"));
            self.emit(format!(
                "call void @__vyrn_write_stdout(ptr {data}, i64 {len})"
            ));
            return Ok(("undef".to_string(), Type::Unit));
        }
        if name == "writeFile" {
            let (path, _) = self.gen_expr(&args[0])?;
            let (contents, _) = self.gen_expr(&args[1])?;
            let st = self.fresh_tmp();
            self.emit(format!(
                "{st} = call i32 @__vyrn_write_file(ptr {path}, ptr {contents})"
            ));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let ok_l = self.fresh_label("wf.ok");
            let err_l = self.fresh_label("wf.err");
            let end_l = self.fresh_label("wf.end");
            self.emit_term(format!("br i1 {isok}, label %{ok_l}, label %{err_l}"));
            self.emit_label(&err_l);
            let msg = self.fresh_tmp();
            self.emit(format!("{msg} = call ptr @__vyrn_write_err(ptr {path})"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            // Ok(true): tag 1, payload word0 = 1 (Bool true zext).
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], \
                 [ {{ i1 1, i64 1, i64 0 }}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Bool), Box::new(Type::Str))));
        }
        // RFC-0044: atomic overwrite (`__vyrn_rename_file`, status 0 ok / 1 io /
        // 2 cross-device). The message is rendered by `@__vyrn_rename_err` from
        // `to` + status, reusing the canonical `@.io.*` wording (a distinct
        // `@.io.xdeverr` for the cross-device case), so it is byte-identical to
        // the interpreter's.
        if name == "renameFile" {
            let (from, _) = self.gen_expr(&args[0])?;
            let (to, _) = self.gen_expr(&args[1])?;
            let st = self.fresh_tmp();
            self.emit(format!(
                "{st} = call i32 @__vyrn_rename_file(ptr {from}, ptr {to})"
            ));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let ok_l = self.fresh_label("rn.ok");
            let err_l = self.fresh_label("rn.err");
            let end_l = self.fresh_label("rn.end");
            self.emit_term(format!("br i1 {isok}, label %{ok_l}, label %{err_l}"));
            self.emit_label(&err_l);
            let msg = self.fresh_tmp();
            self.emit(format!(
                "{msg} = call ptr @__vyrn_rename_err(ptr {to}, i32 {st})"
            ));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], \
                 [ {{ i1 1, i64 1, i64 0 }}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Bool), Box::new(Type::Str))));
        }
        // RFC-0044: flush a file to stable storage (`__vyrn_fsync_file`, 0 ok /
        // 1 io). The error reuses the write-error renderer (fsync is a durability
        // step of writing).
        if name == "fsyncFile" {
            let (path, _) = self.gen_expr(&args[0])?;
            let st = self.fresh_tmp();
            self.emit(format!("{st} = call i32 @__vyrn_fsync_file(ptr {path})"));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let ok_l = self.fresh_label("fs.ok");
            let err_l = self.fresh_label("fs.err");
            let end_l = self.fresh_label("fs.end");
            self.emit_term(format!("br i1 {isok}, label %{ok_l}, label %{err_l}"));
            self.emit_label(&err_l);
            let msg = self.fresh_tmp();
            self.emit(format!("{msg} = call ptr @__vyrn_write_err(ptr {path})"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], \
                 [ {{ i1 1, i64 1, i64 0 }}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Bool), Box::new(Type::Str))));
        }
        if name == "readFileBytes" {
            // Binary read (M2): no UTF-8/NUL rules — the whole point of bytes.
            let (path, _) = self.gen_expr(&args[0])?;
            let outp = self.fresh_alloca("ptr");
            let lenp = self.fresh_alloca("i64");
            let st = self.fresh_tmp();
            self.emit(format!(
                "{st} = call i32 @__vyrn_read_file_bytes(ptr {path}, ptr {outp}, ptr {lenp})"
            ));
            let isok = self.fresh_tmp();
            self.emit(format!("{isok} = icmp eq i32 {st}, 0"));
            let ok_l = self.fresh_label("rfb.ok");
            let err_l = self.fresh_label("rfb.err");
            let end_l = self.fresh_label("rfb.end");
            self.emit_term(format!("br i1 {isok}, label %{ok_l}, label %{err_l}"));
            self.emit_label(&err_l);
            let msg = self.fresh_tmp();
            // status is always 1 (io) here — reuse the read-error renderer.
            self.emit(format!(
                "{msg} = call ptr @__vyrn_read_err(ptr {path}, i32 1)"
            ));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            let err_end = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            // Build the Array<UInt8> triple {buf, len, len}, box it (an Array is
            // wider than the two payload words), and wrap in Ok.
            let buf = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{buf} = load ptr, ptr {outp}"));
            self.emit(format!("{len} = load i64, ptr {lenp}"));
            let a0 = self.fresh_tmp();
            let a1 = self.fresh_tmp();
            let a2 = self.fresh_tmp();
            self.emit(format!(
                "{a0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"
            ));
            self.emit(format!(
                "{a1} = insertvalue {{ ptr, i64, i64 }} {a0}, i64 {len}, 1"
            ));
            self.emit(format!(
                "{a2} = insertvalue {{ ptr, i64, i64 }} {a1}, i64 {len}, 2"
            ));
            let elem_ty = Type::Array(Box::new(Type::IntN {
                bits: 8,
                signed: false,
            }));
            let (w0, w1) = self.encode_payload(&a2, &elem_ty);
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!(
                "{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {w0}, 1"
            ));
            self.emit(format!(
                "{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 {w1}, 2"
            ));
            let ok_end = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_end} ], [ {o2}, %{ok_end} ]"
            ));
            return Ok((r, Type::Result(Box::new(elem_ty), Box::new(Type::Str))));
        }
        if name == "stringFromBytes" {
            // Copy the bytes into a fresh NUL-terminated buffer (null result =
            // an embedded NUL byte), then UTF-8-validate with the shared DFA.
            // The fixed error payloads are strcpy'd to the heap so an Err string
            // is always owned storage, like every other I/O error payload.
            let (arr, _) = self.gen_expr(&args[0])?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {arr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {arr}, 1"));
            let buf = self.fresh_tmp();
            self.emit(format!(
                "{buf} = call ptr @__vyrn_bytes_dup(ptr {data}, i64 {len})"
            ));
            let isnull = self.fresh_tmp();
            self.emit(format!("{isnull} = icmp eq ptr {buf}, null"));
            let nul_l = self.fresh_label("sfb.nul");
            let chk_l = self.fresh_label("sfb.chk");
            let badutf_l = self.fresh_label("sfb.badutf");
            let err_l = self.fresh_label("sfb.err");
            let ok_l = self.fresh_label("sfb.ok");
            let end_l = self.fresh_label("sfb.end");
            self.emit_term(format!("br i1 {isnull}, label %{nul_l}, label %{chk_l}"));
            self.emit_label(&nul_l);
            self.emit_term(format!("br label %{err_l}"));
            self.emit_label(&chk_l);
            let valid = self.fresh_tmp();
            self.emit(format!(
                "{valid} = call i1 @__vyrn_utf8valid(ptr {buf}, i64 {len})"
            ));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{badutf_l}"));
            self.emit_label(&badutf_l);
            self.emit(format!("call void @__vyrn_str_free(ptr {buf})"));
            self.emit_term(format!("br label %{err_l}"));
            self.emit_label(&err_l);
            let src = self.fresh_tmp();
            self.emit(format!(
                "{src} = phi ptr [ @.io.bnul, %{nul_l} ], [ @.io.butf8, %{badutf_l} ]"
            ));
            // `@.io.bnul` / `@.io.butf8` are raw C strings in the data segment
            // (a `snprintf` format elsewhere), not `String` values, so the length
            // is still a scan. What it fills is a headered String.
            let mlen = self.fresh_tmp();
            self.emit(format!("{mlen} = call i64 @__vyrn_strlen(ptr {src})"));
            let msg = self.str_alloc(&mlen, &mlen);
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {msg}, ptr {src}, i64 {mlen}, i1 false)"
            ));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!(
                "{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"
            ));
            self.emit(format!(
                "{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"
            ));
            self.emit(format!(
                "{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let ow = self.fresh_tmp();
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{ow} = ptrtoint ptr {buf} to i64"));
            self.emit(format!(
                "{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {ow}, 1"
            ));
            self.emit(format!(
                "{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], [ {o2}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Str), Box::new(Type::Str))));
        }
        // (`contains`, `startsWith` and `endsWith` are `std/strpred` — RFC-0078
        // M4c. They were `strstr` and two `strncmp` shapes here, ~50 lines with a
        // `phi` in one of them, and are now routed at the top of `gen_call`.
        // `@charCount` is `std/text`'s `charCountV` for the same reason and by the
        // same mechanism — it was one `call i64 @__vyrn_charcount` here.)
        // (`slice` was here too — RFC-0079 M3, and it was the biggest of these:
        // a bounds test, an open-coded continuation-byte pair at each cut point,
        // two trap globals and an arena-aware copy, none of which the interpreter
        // or the direct backend could share a line of. It is `std/strpred`'s
        // `sliceV` now, routed at the top of `gen_call`, and the range check
        // exists once.)
        // concat(String, String) -> String. Heap-allocated. Routing is decided
        // lexically: inside a `region` the buffer is drawn from the arena (freed
        // when the region exits); outside, it comes from `malloc` and is freed by
        // ownership analysis if it doesn't escape, else leaked. The two paths are
        // mutually exclusive, so no buffer is ever freed twice.
        if name == "@concat" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let buf = self.emit_str_concat(&a, &b);
            // The fresh buffer holds a copy of both halves, so a half the
            // expression itself allocated is finished with (RFC-0096 M3). This
            // is the interpolation spine: `"a\{x}b\{y}"` folds left into nested
            // `@concat`s, so every hole's `@str` and every inner join is freed
            // here by the `@concat` above it.
            self.free_str_temp(&args[0], &a);
            self.free_str_temp(&args[1], &b);
            return Ok((buf, Type::Str));
        }

        // str(Int) -> String: format into a fresh 24-byte buffer (enough for any
        // i64). Routed like `concat` (arena inside a region, else malloc).
        if name == "@str" {
            // Render a scalar to a fresh, owned heap String (Int / Bool / String).
            let (v, ty) = self.gen_expr(&args[0])?;
            match self.resolve(&ty) {
                Type::Int => {
                    let buf = self.str_alloc("0", "24");
                    let n = self.fresh_tmp();
                    let n64 = self.fresh_tmp();
                    self.emit(format!(
                        "{n} = call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 25, ptr @.fmt.ld, i64 {v})"
                    ));
                    self.emit(format!("{n64} = sext i32 {n} to i64"));
                    self.emit(format!(
                        "call void @__vyrn_str_setlen(ptr {buf}, i64 {n64})"
                    ));
                    return Ok((buf, Type::Str));
                }
                // A sized int widens to i64 (sext signed, zext unsigned; a 64-bit
                // value is used as-is) and formats with %lld / %llu — same digits
                // the interpreter renders.
                Type::IntN { bits, signed } => {
                    let fmt = if signed { "@.fmt.ld" } else { "@.fmt.lu" };
                    let w = if bits == 64 {
                        v
                    } else {
                        let ext = if signed { "sext" } else { "zext" };
                        let t = self.fresh_tmp();
                        self.emit(format!("{t} = {ext} i{bits} {v} to i64"));
                        t
                    };
                    let buf = self.str_alloc("0", "24");
                    let n = self.fresh_tmp();
                    let n64 = self.fresh_tmp();
                    self.emit(format!(
                        "{n} = call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 25, ptr {fmt}, i64 {w})"
                    ));
                    self.emit(format!("{n64} = sext i32 {n} to i64"));
                    self.emit(format!(
                        "call void @__vyrn_str_setlen(ptr {buf}, i64 {n64})"
                    ));
                    return Ok((buf, Type::Str));
                }
                // (`%f` was selected here — a `select` on `fcmp uno` between the
                // literal `NaN` and the format string, because UCRT's `%f` says
                // `-nan(ind)` and the interpreter says `NaN`. RFC-0081 M2 routed
                // the float case to `std/num`'s `f64Str`: the six places were
                // three algorithms that had to agree byte for byte, and `printf`
                // was measured at 240 ns against the Vyrn version's 750 — a 3x on
                // a microbenchmark and nothing observable in a program.)
                ref f @ (Type::Float | Type::Float32) => {
                    let s = self.gen_f64_str(&v, f)?;
                    return Ok((s, Type::Str));
                }
                Type::Bool => {
                    // Copy "true"/"false" into a fresh buffer so the result owns
                    // its storage (a global pointer must never be freed).
                    let src = self.fresh_tmp();
                    let n = self.fresh_tmp();
                    self.emit(format!(
                        "{src} = select i1 {v}, ptr @.str.true, ptr @.str.false"
                    ));
                    self.emit(format!("{n} = select i1 {v}, i64 4, i64 5"));
                    let buf = self.str_alloc(&n, &n);
                    self.emit(format!(
                        "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {src}, i64 {n}, i1 false)"
                    ));
                    return Ok((buf, Type::Str));
                }
                Type::Str => {
                    // strdup: copy so the rendered value is independently owned.
                    let len = self.str_len(&v);
                    let buf = self.str_alloc(&len, &len);
                    self.emit(format!(
                        "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {v}, i64 {len}, i1 false)"
                    ));
                    // The copy is why an argument this expression allocated is
                    // finished with (RFC-0096 M3). `"\{a + b}"` leaked that
                    // buffer for as long as this arm has copied.
                    self.free_str_temp(&args[0], &v);
                    return Ok((buf, Type::Str));
                }
                other => return Err(format!("`str` cannot render {other:?}")),
            }
        }
        // parse(String) -> Option<Int>: optional '-', then digits, all consumed;
        // otherwise None. Overflow wraps (matches the interpreter).
        if name == "parse" {
            let (s, _) = self.gen_expr(&args[0])?;
            let c0 = self.fresh_tmp();
            let isneg = self.fresh_tmp();
            let off = self.fresh_tmp();
            let p0 = self.fresh_tmp();
            let first = self.fresh_tmp();
            let hasdigit = self.fresh_tmp();
            self.emit(format!("{c0} = load i8, ptr {s}"));
            self.emit(format!("{isneg} = icmp eq i8 {c0}, 45"));
            self.emit(format!("{off} = zext i1 {isneg} to i64"));
            self.emit(format!("{p0} = getelementptr i8, ptr {s}, i64 {off}"));
            self.emit(format!("{first} = load i8, ptr {p0}"));
            self.emit(format!("{hasdigit} = icmp ne i8 {first}, 0"));
            let pre = self.cur_block.clone();
            let loop_l = self.fresh_label("parse.loop");
            let digit_l = self.fresh_label("parse.digit");
            let cont_l = self.fresh_label("parse.cont");
            let done_l = self.fresh_label("parse.done");
            let fail_l = self.fresh_label("parse.fail");
            let build_l = self.fresh_label("parse.build");
            self.emit_term(format!("br label %{loop_l}"));
            // loop: walk characters, accumulating.
            self.emit_label(&loop_l);
            let p = self.fresh_tmp();
            let acc = self.fresh_tmp();
            self.emit(format!(
                "{p} = phi ptr [ {p0}, %{pre} ], [ {{PNEXT}}, %{cont_l} ]"
            ));
            self.emit(format!(
                "{acc} = phi i64 [ 0, %{pre} ], [ {{ACCN}}, %{cont_l} ]"
            ));
            let ch = self.fresh_tmp();
            let isnull = self.fresh_tmp();
            self.emit(format!("{ch} = load i8, ptr {p}"));
            self.emit(format!("{isnull} = icmp eq i8 {ch}, 0"));
            self.emit_term(format!("br i1 {isnull}, label %{done_l}, label %{digit_l}"));
            // digit: is it 0-9?
            self.emit_label(&digit_l);
            let ge0 = self.fresh_tmp();
            let le9 = self.fresh_tmp();
            let isdig = self.fresh_tmp();
            self.emit(format!("{ge0} = icmp uge i8 {ch}, 48"));
            self.emit(format!("{le9} = icmp ule i8 {ch}, 57"));
            self.emit(format!("{isdig} = and i1 {ge0}, {le9}"));
            self.emit_term(format!("br i1 {isdig}, label %{cont_l}, label %{fail_l}"));
            // cont: acc = acc*10 + digit; advance.
            self.emit_label(&cont_l);
            let d = self.fresh_tmp();
            let d64 = self.fresh_tmp();
            let m = self.fresh_tmp();
            let accn = self.fresh_tmp();
            let pnext = self.fresh_tmp();
            self.emit(format!("{d} = sub i8 {ch}, 48"));
            self.emit(format!("{d64} = zext i8 {d} to i64"));
            self.emit(format!("{m} = mul i64 {acc}, 10"));
            self.emit(format!("{accn} = add i64 {m}, {d64}"));
            self.emit(format!("{pnext} = getelementptr i8, ptr {p}, i64 1"));
            self.emit_term(format!("br label %{loop_l}"));
            // done: reached NUL; apply sign.
            self.emit_label(&done_l);
            let negval = self.fresh_tmp();
            let val = self.fresh_tmp();
            self.emit(format!("{negval} = sub i64 0, {acc}"));
            self.emit(format!(
                "{val} = select i1 {isneg}, i64 {negval}, i64 {acc}"
            ));
            self.emit_term(format!("br label %{build_l}"));
            // fail: a non-digit character.
            self.emit_label(&fail_l);
            self.emit_term(format!("br label %{build_l}"));
            // build the Option<Int>.
            self.emit_label(&build_l);
            let tag = self.fresh_tmp();
            let v = self.fresh_tmp();
            self.emit(format!(
                "{tag} = phi i1 [ {hasdigit}, %{done_l} ], [ false, %{fail_l} ]"
            ));
            self.emit(format!(
                "{v} = phi i64 [ {val}, %{done_l} ], [ 0, %{fail_l} ]"
            ));
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!(
                "{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 {tag}, 0"
            ));
            self.emit(format!(
                "{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {v}, 1"
            ));
            self.emit(format!(
                "{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"
            ));
            // Backpatch the loop phis' back-edge values (emitted before cont).
            for line in self.body.iter_mut() {
                if line.contains("{PNEXT}") {
                    *line = line.replace("{PNEXT}", &pnext);
                }
                if line.contains("{ACCN}") {
                    *line = line.replace("{ACCN}", &accn);
                }
            }
            return Ok((o2, Type::Option(Box::new(Type::Int))));
        }

        // `Some(x)` / `Ok(x)` / `Err(e)` — build a { i1 tag, i64 payload } value.
        // Growable arrays. An `Array<T>` is { ptr data, i64 len, i64 cap }; used
        // linearly (`push` returns the updated triple, reallocating on growth).
        // `xs.reserve(n)` (RFC-0115): make room for `n` more elements, in one
        // realloc, and hand the (possibly moved) buffer back. A `need` already
        // inside `cap` passes the triple through untouched.
        // `xs.clear()` (RFC-0115 addendum): the same triple with its length
        // zeroed — the buffer and its capacity are kept for the next fill.
        if name == "@clear" {
            let (av, aty) = self.gen_expr(&args[0])?;
            if !matches!(self.resolve(&aty), Type::Array(_)) {
                return Err("clear on a non-Array value".into());
            }
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = insertvalue {{ ptr, i64, i64 }} {av}, i64 0, 1"
            ));
            return Ok((r, aty));
        }
        if name == "@reserve" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                _ => return Err("reserve on a non-Array value".into()),
            };
            let ell = self.llt(&elem);
            let (nv, _) = self.gen_expr(&args[1])?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            let cap = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
            self.emit(format!("{cap} = extractvalue {{ ptr, i64, i64 }} {av}, 2"));
            let need = self.fresh_tmp();
            let fits = self.fresh_tmp();
            self.emit(format!("{need} = add i64 {len}, {nv}"));
            self.emit(format!("{fits} = icmp sle i64 {need}, {cap}"));
            let grow_l = self.fresh_label("rsv.grow");
            let ready_l = self.fresh_label("rsv.ready");
            let pre = self.cur_block.clone();
            self.emit_term(format!("br i1 {fits}, label %{ready_l}, label %{grow_l}"));
            self.emit_label(&grow_l);
            let esz = self.fresh_tmp();
            let nb = self.fresh_tmp();
            let nd = self.fresh_tmp();
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{nb} = mul i64 {need}, {esz}"));
            self.emit(format!(
                "{nd} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"
            ));
            self.emit_term(format!("br label %{ready_l}"));
            self.emit_label(&ready_l);
            let pdata = self.fresh_tmp();
            let pcap = self.fresh_tmp();
            self.emit(format!(
                "{pdata} = phi ptr [ {data}, %{pre} ], [ {nd}, %{grow_l} ]"
            ));
            self.emit(format!(
                "{pcap} = phi i64 [ {cap}, %{pre} ], [ {need}, %{grow_l} ]"
            ));
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            let r3 = self.fresh_tmp();
            self.emit(format!(
                "{r1} = insertvalue {{ ptr, i64, i64 }} undef, ptr {pdata}, 0"
            ));
            self.emit(format!(
                "{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {len}, 1"
            ));
            self.emit(format!(
                "{r3} = insertvalue {{ ptr, i64, i64 }} {r2}, i64 {pcap}, 2"
            ));
            return Ok((r3, Type::Array(Box::new(elem))));
        }
        // `m.tally(k, n)` (RFC-0116): insert-or-add, ONE probe — the fusion a
        // read-then-store cannot compose. The callee never takes the key: a
        // hit touches nothing (the free audit caught the first draft freeing
        // a key the argument machinery also frees), a miss stores a COPY.
        // Values are Int64 (the checker pinned them), so the displaced value
        // releases nothing.
        if name == "@tally" {
            let (mv, mty) = self.gen_expr(&args[0])?;
            // The map's key type picks the probe family (RFC-0117): an Int64
            // key is passed by value, stored by value, and never copied or
            // freed.
            let (ik, pk_ty) = match vyrn_frontend::types::resolve(&mty, self.types) {
                Type::Map(k, _) => (
                    self.key_is_int(&k),
                    self.key_is_pack(&k).then(|| (*k).clone()),
                ),
                _ => (false, None),
            };
            let (kv, _) = self.gen_expr(&args[1])?;
            let packed = pk_ty.map(|kt| self.emit_key_pack(&kv, &kt));
            let (nv, _) = self.gen_expr(&args[2])?;
            let slot = self.fresh_alloca("{ ptr, ptr, i64, i64, ptr }");
            self.emit(format!(
                "store {{ ptr, ptr, i64, i64, ptr }} {mv}, ptr {slot}"
            ));
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 0"
            ));
            self.emit(format!(
                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 2"
            ));
            let (ix, cap) = self.map_index_of(&mv);
            let idx = self.fresh_tmp();
            if ik {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_i64(ptr {keys}, i64 {len}, i64 {kv}, ptr {ix}, i64 {cap})"
                ));
            } else if let Some((kbuf, stride)) = &packed {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_pack(ptr {keys}, i64 {len}, ptr {kbuf}, i64 {stride}, ptr {ix}, i64 {cap})"
                ));
            } else {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {kv}, ptr {ix}, i64 {cap})"
                ));
            }
            let found = self.fresh_tmp();
            self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
            let upd_l = self.fresh_label("tally.upd");
            let ins_l = self.fresh_label("tally.ins");
            let done_l = self.fresh_label("tally.done");
            self.emit_term(format!("br i1 {found}, label %{upd_l}, label %{ins_l}"));
            self.emit_label(&upd_l);
            let vals0 = self.fresh_tmp();
            self.emit(format!(
                "{vals0} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 1"
            ));
            let ep0 = self.fresh_tmp();
            let old = self.fresh_tmp();
            let newv = self.fresh_tmp();
            self.emit(format!("{ep0} = getelementptr i64, ptr {vals0}, i64 {idx}"));
            self.emit(format!("{old} = load i64, ptr {ep0}"));
            self.emit(format!("{newv} = add i64 {old}, {nv}"));
            self.emit(format!("store i64 {newv}, ptr {ep0}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&ins_l);
            if let Some((_, stride)) = &packed {
                self.emit(format!(
                    "call void @__vyrn_map_reserve_pack(ptr {slot}, i64 8, i64 {stride})"
                ));
            } else {
                let rsv = if ik {
                    "__vyrn_map_reserve_i64"
                } else {
                    "__vyrn_map_reserve"
                };
                self.emit(format!("call void @{rsv}(ptr {slot}, i64 8)"));
            }
            let hdr2 = self.fresh_tmp();
            let keys2 = self.fresh_tmp();
            let vals2 = self.fresh_tmp();
            self.emit(format!(
                "{hdr2} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
            ));
            self.emit(format!(
                "{keys2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 0"
            ));
            self.emit(format!(
                "{vals2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 1"
            ));
            let kep = self.fresh_tmp();
            if ik {
                self.emit(format!("{kep} = getelementptr i64, ptr {keys2}, i64 {len}"));
                self.emit(format!("store i64 {kv}, ptr {kep}"));
            } else if let Some((kbuf, stride)) = &packed {
                let off = self.fresh_tmp();
                self.emit(format!("{off} = mul i64 {len}, {stride}"));
                self.emit(format!("{kep} = getelementptr i8, ptr {keys2}, i64 {off}"));
                self.emit(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr {kep}, ptr {kbuf}, i64 {stride}, i1 false)"
                ));
            } else {
                let kcopy = self.deep_copy(&kv, &Type::Str)?;
                self.emit(format!("{kep} = getelementptr ptr, ptr {keys2}, i64 {len}"));
                self.emit(format!("store ptr {kcopy}, ptr {kep}"));
            }
            if let Some((_, stride)) = &packed {
                self.emit(format!(
                    "call void @__vyrn_map_index_add_pack(ptr {slot}, i64 {len}, i64 {stride})"
                ));
            } else {
                let iadd = if ik {
                    "__vyrn_map_index_add_i64"
                } else {
                    "__vyrn_map_index_add"
                };
                self.emit(format!("call void @{iadd}(ptr {slot}, i64 {len})"));
            }
            let vep = self.fresh_tmp();
            self.emit(format!("{vep} = getelementptr i64, ptr {vals2}, i64 {len}"));
            self.emit(format!("store i64 {nv}, ptr {vep}"));
            let nl = self.fresh_tmp();
            let lenp = self.fresh_tmp();
            self.emit(format!("{nl} = add i64 {len}, 1"));
            self.emit(format!(
                "{lenp} = getelementptr {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}, i64 0, i32 2"
            ));
            self.emit(format!("store i64 {nl}, ptr {lenp}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&done_l);
            let out = self.fresh_tmp();
            self.emit(format!(
                "{out} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
            ));
            return Ok((out, mty));
        }
        // `m.tallyBytes(w, n)` (RFC-0116): `tally` keyed by raw bytes. The HIT
        // path — the hot one in a counting loop — compares the bytes where they
        // lie: no String, no UTF-8 validation, no allocation. Only a MISS
        // builds the key, and bytes that are not a String trap there.
        if name == "@tallyBytes" {
            let (mv, mty) = self.gen_expr(&args[0])?;
            let (wv, _) = self.gen_expr(&args[1])?;
            let (nv, _) = self.gen_expr(&args[2])?;
            let slot = self.fresh_alloca("{ ptr, ptr, i64, i64, ptr }");
            self.emit(format!(
                "store {{ ptr, ptr, i64, i64, ptr }} {mv}, ptr {slot}"
            ));
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 0"
            ));
            self.emit(format!(
                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 2"
            ));
            let (ix, cap) = self.map_index_of(&mv);
            let wdata = self.fresh_tmp();
            let wlen = self.fresh_tmp();
            self.emit(format!(
                "{wdata} = extractvalue {{ ptr, i64, i64 }} {wv}, 0"
            ));
            self.emit(format!("{wlen} = extractvalue {{ ptr, i64, i64 }} {wv}, 1"));
            let idx = self.fresh_tmp();
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find_bytes(ptr {keys}, i64 {len}, ptr {wdata}, i64 {wlen}, ptr {ix}, i64 {cap})"
            ));
            let found = self.fresh_tmp();
            self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
            let upd_l = self.fresh_label("tbyt.upd");
            let mk_l = self.fresh_label("tbyt.mk");
            let bad_l = self.fresh_label("tbyt.bad");
            let chk_l = self.fresh_label("tbyt.chk");
            let ins_l = self.fresh_label("tbyt.ins");
            let done_l = self.fresh_label("tbyt.done");
            self.emit_term(format!("br i1 {found}, label %{upd_l}, label %{mk_l}"));
            self.emit_label(&upd_l);
            let vals0 = self.fresh_tmp();
            self.emit(format!(
                "{vals0} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 1"
            ));
            let ep0 = self.fresh_tmp();
            let old = self.fresh_tmp();
            let newv = self.fresh_tmp();
            self.emit(format!("{ep0} = getelementptr i64, ptr {vals0}, i64 {idx}"));
            self.emit(format!("{old} = load i64, ptr {ep0}"));
            self.emit(format!("{newv} = add i64 {old}, {nv}"));
            self.emit(format!("store i64 {newv}, ptr {ep0}"));
            self.emit_term(format!("br label %{done_l}"));
            // miss: the key exists from here on. `bytes_dup` answers null for an
            // embedded NUL; the DFA answers for the rest; either way the trap.
            self.emit_label(&mk_l);
            let buf = self.fresh_tmp();
            self.emit(format!(
                "{buf} = call ptr @__vyrn_bytes_dup(ptr {wdata}, i64 {wlen})"
            ));
            let isnull = self.fresh_tmp();
            self.emit(format!("{isnull} = icmp eq ptr {buf}, null"));
            self.emit_term(format!("br i1 {isnull}, label %{bad_l}, label %{chk_l}"));
            self.emit_label(&chk_l);
            let valid = self.fresh_tmp();
            self.emit(format!(
                "{valid} = call i1 @__vyrn_utf8valid(ptr {buf}, i64 {wlen})"
            ));
            self.emit_term(format!("br i1 {valid}, label %{ins_l}, label %{bad_l}"));
            self.emit_label(&bad_l);
            self.emit("call void @__vyrn_trap_msg(ptr @.trap.tbytes)".into());
            self.emit_term("unreachable".into());
            self.emit_label(&ins_l);
            self.emit(format!("call void @__vyrn_map_reserve(ptr {slot}, i64 8)"));
            let hdr2 = self.fresh_tmp();
            let keys2 = self.fresh_tmp();
            let vals2 = self.fresh_tmp();
            self.emit(format!(
                "{hdr2} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
            ));
            self.emit(format!(
                "{keys2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 0"
            ));
            self.emit(format!(
                "{vals2} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr2}, 1"
            ));
            let kep = self.fresh_tmp();
            self.emit(format!("{kep} = getelementptr ptr, ptr {keys2}, i64 {len}"));
            self.emit(format!("store ptr {buf}, ptr {kep}"));
            self.emit(format!(
                "call void @__vyrn_map_index_add(ptr {slot}, i64 {len})"
            ));
            let vep = self.fresh_tmp();
            self.emit(format!("{vep} = getelementptr i64, ptr {vals2}, i64 {len}"));
            self.emit(format!("store i64 {nv}, ptr {vep}"));
            let nl = self.fresh_tmp();
            let lenp = self.fresh_tmp();
            self.emit(format!("{nl} = add i64 {len}, 1"));
            self.emit(format!(
                "{lenp} = getelementptr {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}, i64 0, i32 2"
            ));
            self.emit(format!("store i64 {nl}, ptr {lenp}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&done_l);
            let out = self.fresh_tmp();
            self.emit(format!(
                "{out} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
            ));
            return Ok((out, mty));
        }
        // `dst.copyFrom(src)` (RFC-0115): the receiver's buffer, the source's
        // elements, one memcpy. Grows only when the source is longer; a
        // self-copy moves zero bytes (a `select` on the data pointers), which
        // sidesteps the same-pointer memcpy nobody defines.
        if name == "@copyFrom" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                _ => return Err("copyFrom on a non-Array value".into()),
            };
            let ell = self.llt(&elem);
            let (xv, _) = self.gen_expr(&args[1])?;
            let data = self.fresh_tmp();
            let cap = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{cap} = extractvalue {{ ptr, i64, i64 }} {av}, 2"));
            let xdata = self.fresh_tmp();
            let xlen = self.fresh_tmp();
            self.emit(format!(
                "{xdata} = extractvalue {{ ptr, i64, i64 }} {xv}, 0"
            ));
            self.emit(format!("{xlen} = extractvalue {{ ptr, i64, i64 }} {xv}, 1"));
            let fits = self.fresh_tmp();
            self.emit(format!("{fits} = icmp sle i64 {xlen}, {cap}"));
            let grow_l = self.fresh_label("cpf.grow");
            let ready_l = self.fresh_label("cpf.ready");
            let pre = self.cur_block.clone();
            self.emit_term(format!("br i1 {fits}, label %{ready_l}, label %{grow_l}"));
            self.emit_label(&grow_l);
            let esz = self.fresh_tmp();
            let nb = self.fresh_tmp();
            let nd = self.fresh_tmp();
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{nb} = mul i64 {xlen}, {esz}"));
            self.emit(format!(
                "{nd} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"
            ));
            self.emit_term(format!("br label %{ready_l}"));
            self.emit_label(&ready_l);
            let pdata = self.fresh_tmp();
            let pcap = self.fresh_tmp();
            self.emit(format!(
                "{pdata} = phi ptr [ {data}, %{pre} ], [ {nd}, %{grow_l} ]"
            ));
            self.emit(format!(
                "{pcap} = phi i64 [ {cap}, %{pre} ], [ {xlen}, %{grow_l} ]"
            ));
            let same = self.fresh_tmp();
            let esz2 = self.fresh_tmp();
            let raw = self.fresh_tmp();
            let bytes = self.fresh_tmp();
            self.emit(format!("{same} = icmp eq ptr {xdata}, {pdata}"));
            self.emit(format!(
                "{esz2} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{raw} = mul i64 {xlen}, {esz2}"));
            self.emit(format!("{bytes} = select i1 {same}, i64 0, i64 {raw}"));
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {pdata}, ptr {xdata}, i64 {bytes}, i1 false)"
            ));
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            let r3 = self.fresh_tmp();
            self.emit(format!(
                "{r1} = insertvalue {{ ptr, i64, i64 }} undef, ptr {pdata}, 0"
            ));
            self.emit(format!(
                "{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {xlen}, 1"
            ));
            self.emit(format!(
                "{r3} = insertvalue {{ ptr, i64, i64 }} {r2}, i64 {pcap}, 2"
            ));
            return Ok((r3, Type::Array(Box::new(elem))));
        }
        // `xs.append(ys)` (RFC-0115): grow once to `max(need, cap*2)`, then one
        // memcpy of the source's elements — the checker held the element type
        // to heapless ones, so bytes ARE the elements. A self-append reads the
        // source from the reallocated buffer: `select` keeps the old source
        // pointer only when it was a different array's.
        if name == "@append" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                _ => return Err("append on a non-Array value".into()),
            };
            let ell = self.llt(&elem);
            let (xv, _) = self.gen_expr(&args[1])?;
            // The same post-evaluation header re-read `push` does: evaluating
            // the source may have grown the receiver through a `modify` call.
            let hdr = match &args[0] {
                Expr::Var { name, .. } if self.lookup(name).is_some() => {
                    let (slot, _) = self.lookup(name).unwrap();
                    let fresh = self.fresh_tmp();
                    self.emit(format!("{fresh} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                    fresh
                }
                _ => av.clone(),
            };
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            let cap = self.fresh_tmp();
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
            self.emit(format!("{cap} = extractvalue {{ ptr, i64, i64 }} {hdr}, 2"));
            let xdata = self.fresh_tmp();
            let xlen = self.fresh_tmp();
            self.emit(format!(
                "{xdata} = extractvalue {{ ptr, i64, i64 }} {xv}, 0"
            ));
            self.emit(format!("{xlen} = extractvalue {{ ptr, i64, i64 }} {xv}, 1"));
            let need = self.fresh_tmp();
            let fits = self.fresh_tmp();
            self.emit(format!("{need} = add i64 {len}, {xlen}"));
            self.emit(format!("{fits} = icmp sle i64 {need}, {cap}"));
            let grow_l = self.fresh_label("app.grow");
            let ready_l = self.fresh_label("app.ready");
            let pre = self.cur_block.clone();
            self.emit_term(format!("br i1 {fits}, label %{ready_l}, label %{grow_l}"));
            self.emit_label(&grow_l);
            let dbl = self.fresh_tmp();
            let over = self.fresh_tmp();
            let nc = self.fresh_tmp();
            let esz = self.fresh_tmp();
            let nb = self.fresh_tmp();
            let nd = self.fresh_tmp();
            self.emit(format!("{dbl} = mul i64 {cap}, 2"));
            self.emit(format!("{over} = icmp sgt i64 {need}, {dbl}"));
            self.emit(format!("{nc} = select i1 {over}, i64 {need}, i64 {dbl}"));
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{nb} = mul i64 {nc}, {esz}"));
            self.emit(format!(
                "{nd} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"
            ));
            self.emit_term(format!("br label %{ready_l}"));
            self.emit_label(&ready_l);
            let pdata = self.fresh_tmp();
            let pcap = self.fresh_tmp();
            self.emit(format!(
                "{pdata} = phi ptr [ {data}, %{pre} ], [ {nd}, %{grow_l} ]"
            ));
            self.emit(format!(
                "{pcap} = phi i64 [ {cap}, %{pre} ], [ {nc}, %{grow_l} ]"
            ));
            let same = self.fresh_tmp();
            let src = self.fresh_tmp();
            let dst = self.fresh_tmp();
            let esz2 = self.fresh_tmp();
            let bytes = self.fresh_tmp();
            self.emit(format!("{same} = icmp eq ptr {xdata}, {data}"));
            self.emit(format!(
                "{src} = select i1 {same}, ptr {pdata}, ptr {xdata}"
            ));
            self.emit(format!(
                "{dst} = getelementptr {ell}, ptr {pdata}, i64 {len}"
            ));
            self.emit(format!(
                "{esz2} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{bytes} = mul i64 {xlen}, {esz2}"));
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)"
            ));
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            let r3 = self.fresh_tmp();
            self.emit(format!(
                "{r1} = insertvalue {{ ptr, i64, i64 }} undef, ptr {pdata}, 0"
            ));
            self.emit(format!(
                "{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {need}, 1"
            ));
            self.emit(format!(
                "{r3} = insertvalue {{ ptr, i64, i64 }} {r2}, i64 {pcap}, 2"
            ));
            return Ok((r3, Type::Array(Box::new(elem))));
        }
        if name == "@push" {
            let (av, aty) = self.gen_expr(&args[0])?;
            // `SmallArray<T, N>.push(v)` (RFC-0056): store into the live buffer
            // (inline while `cap == N`, else heap). A push at `len == cap`
            // grows — from inline it allocates `2N` and copies the inline slots
            // out; from a spilled buffer it reallocs to `cap*2`. It never
            // un-spills. Returns the whole (possibly reshaped) SmallArray value.
            if let Type::SmallArray(inner, n) = self.resolve(&aty) {
                return self.gen_smallarray_push(&av, &inner, n, &args[1]);
            }
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                _ => return Err("push on a non-Array value".into()),
            };
            let ell = self.llt(&elem);
            // RFC-0037: the element type is a pushed lambda's expected type.
            self.expect.push(elem.clone());
            let r = self.gen_expr(&args[1]);
            self.expect.pop();
            let (v, vty) = r?;
            let (v, _) = self.coerce(v, &vty, &elem)?;
            // The header is read only AFTER the element ran: the element
            // expression may `modify` the receiver (`a.push(takeLast(a))`),
            // and the push must trust the post-mutation len/cap the way the
            // interpreter reads the live slot. A receiver that is not a plain
            // variable cannot be aliased by the element expression, so its
            // snapshot in `av` is still current.
            let hdr = match &args[0] {
                Expr::Var { name, .. } if self.lookup(name).is_some() => {
                    let (slot, _) = self.lookup(name).unwrap();
                    let fresh = self.fresh_tmp();
                    self.emit(format!("{fresh} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                    fresh
                }
                _ => av.clone(),
            };
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            let cap = self.fresh_tmp();
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
            self.emit(format!("{cap} = extractvalue {{ ptr, i64, i64 }} {hdr}, 2"));
            let full = self.fresh_tmp();
            self.emit(format!("{full} = icmp eq i64 {len}, {cap}"));
            let grow_l = self.fresh_label("push.grow");
            let ready_l = self.fresh_label("push.ready");
            let pre = self.cur_block.clone();
            self.emit_term(format!("br i1 {full}, label %{grow_l}, label %{ready_l}"));
            // grow: newcap = cap==0 ? 4 : cap*2; realloc.
            self.emit_label(&grow_l);
            let capzero = self.fresh_tmp();
            let dbl = self.fresh_tmp();
            let nc = self.fresh_tmp();
            let esz = self.fresh_tmp();
            let nb = self.fresh_tmp();
            let nd = self.fresh_tmp();
            self.emit(format!("{capzero} = icmp eq i64 {cap}, 0"));
            self.emit(format!("{dbl} = mul i64 {cap}, 2"));
            self.emit(format!("{nc} = select i1 {capzero}, i64 4, i64 {dbl}"));
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{nb} = mul i64 {nc}, {esz}"));
            self.emit(format!(
                "{nd} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"
            ));
            self.emit_term(format!("br label %{ready_l}"));
            // ready: choose data/cap, store the new element, rebuild the triple.
            self.emit_label(&ready_l);
            let d = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!(
                "{d} = phi ptr [ {data}, %{pre} ], [ {nd}, %{grow_l} ]"
            ));
            self.emit(format!(
                "{c} = phi i64 [ {cap}, %{pre} ], [ {nc}, %{grow_l} ]"
            ));
            let ep = self.fresh_tmp();
            self.emit(format!("{ep} = getelementptr {ell}, ptr {d}, i64 {len}"));
            self.emit(format!("store {ell} {v}, ptr {ep}"));
            let nl = self.fresh_tmp();
            self.emit(format!("{nl} = add i64 {len}, 1"));
            let r0 = self.fresh_tmp();
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            self.emit(format!(
                "{r0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {d}, 0"
            ));
            self.emit(format!(
                "{r1} = insertvalue {{ ptr, i64, i64 }} {r0}, i64 {nl}, 1"
            ));
            self.emit(format!(
                "{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {c}, 2"
            ));
            return Ok((r2, Type::Array(Box::new(elem))));
        }
        // `a[i]` is the DISPATCH site (RFC-0091 M2), not a lowering. It asks
        // the receiver's type for a `place at` projection and inlines its body
        // here. Every builtin container takes the seeded row, whose body is
        // `yield @slot(self, i)` — so the element lowering below is reached
        // through the same table a user container reaches its own through, and
        // the emitted IR is the same text it was when this block was named
        // `at`.
        if name == vyrn_frontend::project::AT && args.len() == 2 {
            let line = args[0].line();
            let recv = self.static_ty(&args[0]);
            // `None` is the seeded row, and the element lowering below reads the
            // ORIGINAL nodes; `project::site` is where that is decided, once.
            let Some(p) = vyrn_frontend::project::site(
                self.impls,
                recv.as_ref(),
                "at",
                &args[0],
                &args[1..],
                line,
            )?
            else {
                return self.gen_call(vyrn_frontend::project::ELEM, args);
            };
            for s in &p.prologue {
                self.gen_stmt(s)?;
            }
            return self.gen_expr(&p.place);
        }
        // RFC-0120: a named projection dispatches here exactly as `a[i]` does —
        // the same table, its own method name. Reached only for a call the
        // checker resolved to a projection (a function of the same name wins
        // there, so it wins here too via the `funcs` guard).
        if !args.is_empty()
            && self.funcs.get(name).is_none()
            && self
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            let line = args[0].line();
            let recv = self.static_ty(&args[0]);
            if let Some(p) = vyrn_frontend::project::site(
                self.impls,
                recv.as_ref(),
                name,
                &args[0],
                &args[1..],
                line,
            )? {
                for s in &p.prologue {
                    self.gen_stmt(s)?;
                }
                return self.gen_expr(&p.place);
            }
        }
        if name == vyrn_frontend::project::ELEM {
            let (av, aty) = self.gen_expr(&args[0])?;
            let (iv, _) = self.gen_expr(&args[1])?;
            let bad_l = self.fresh_label("at.oob");
            let ok_l = self.fresh_label("at.ok");
            // The trap message carries the offending index and goes to stderr,
            // byte-identical to the interpreter's `error: ... index {i} out of
            // bounds`. Strings pick the "string index" wording.
            let emit_trap = |g: &mut Self, fmt: &str| {
                g.emit_label(&bad_l);
                g.emit(format!("call void @__vyrn_trap_idx(ptr {fmt}, i64 {iv})"));
                g.emit_term("unreachable".into());
            };
            match self.resolve(&aty) {
                Type::Array(inner) => {
                    let elem = *inner;
                    let ell = self.llt(&elem);
                    let data = self.fresh_tmp();
                    let len = self.fresh_tmp();
                    self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
                    self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.aoob");
                    self.emit_label(&ok_l);
                    let ep = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {iv}"));
                    self.emit(format!("{v} = load {ell}, ptr {ep}"));
                    return Ok((v, elem));
                }
                // `SmallArray<T, N>[i]` (RFC-0056): pick the live base (inline
                // while `cap == N`, else heap), bounds-check against `len`, load.
                Type::SmallArray(inner, n) => {
                    let elem = *inner;
                    let ell = self.llt(&elem);
                    // Read straight from the binding's slot when the receiver is
                    // a plain variable — no need to spill the whole value (incl.
                    // the inline buffer) to a temp on every access, so a
                    // read-heavy `xs[i]` loop pays only the state branch.
                    let (base, len) = match &args[0] {
                        Expr::Var { name, .. } if self.lookup(name).is_some() => {
                            let (slot, _) = self.lookup(name).unwrap();
                            let (base, len, _c, _d) = self.sa_slot_base(&slot, &elem, n);
                            (base, len)
                        }
                        _ => self.sa_value_base_len(&av, &elem, n),
                    };
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.aoob");
                    self.emit_label(&ok_l);
                    let ep = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr {ell}, ptr {base}, i64 {iv}"));
                    self.emit(format!("{v} = load {ell}, ptr {ep}"));
                    return Ok((v, elem));
                }
                Type::ArrayN(inner, n) => {
                    // Fixed array: index it in memory. Bounds are the constant N.
                    let elem = *inner;
                    let ell = self.llt(&elem);
                    let aggty = format!("[{n} x {ell}]");
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {n}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.aoob");
                    self.emit_label(&ok_l);
                    // Read through the receiver's own storage when it has some.
                    // `getelementptr` cannot index an SSA aggregate by a dynamic
                    // index, so the value form has to reach memory first, and a
                    // fresh slot means copying all N elements per read. The
                    // binding's slot is already that memory.
                    let slot = match self.fixed_place(&args[0], &aggty) {
                        Some(p) => p,
                        None => {
                            let s = self.fresh_alloca(&aggty);
                            self.emit(format!("store {aggty} {av}, ptr {s}"));
                            s
                        }
                    };
                    let ep = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!(
                        "{ep} = getelementptr {aggty}, ptr {slot}, i64 0, i64 {iv}"
                    ));
                    self.emit(format!("{v} = load {ell}, ptr {ep}"));
                    return Ok((v, elem));
                }
                // `s[i]` on a String: bounds-check against the header length,
                // then load the byte as a `UInt8` (RFC-0022) — an `i8` SSA value,
                // the same representation as an element of `bytes(s)`, no
                // zero-extension.
                Type::Str => {
                    let len = self.str_len(&av);
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.soob");
                    self.emit_label(&ok_l);
                    let ep = self.fresh_tmp();
                    let byte = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr i8, ptr {av}, i64 {iv}"));
                    self.emit(format!("{byte} = load i8, ptr {ep}"));
                    return Ok((
                        byte,
                        Type::IntN {
                            bits: 8,
                            signed: false,
                        },
                    ));
                }
                // `m[k]` on a Map (RFC-0028): hashed probe → `Option<V>`
                // (`None` on a miss, never a trap). `iv` is the key — a `ptr`
                // for a String key, the `i64` itself for an Int64 one
                // (RFC-0117).
                Type::Map(key, val) => {
                    let ik = self.key_is_int(&key);
                    let packed = self
                        .key_is_pack(&key)
                        .then(|| self.emit_key_pack(&iv, &key));
                    let val = *val;
                    let vll = self.llt(&val);
                    let keys = self.fresh_tmp();
                    let vals = self.fresh_tmp();
                    let len = self.fresh_tmp();
                    self.emit(format!(
                        "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {av}, 0"
                    ));
                    self.emit(format!(
                        "{vals} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {av}, 1"
                    ));
                    self.emit(format!(
                        "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {av}, 2"
                    ));
                    let (ix, cap) = self.map_index_of(&av);
                    let idx = self.fresh_tmp();
                    if ik {
                        self.emit(format!(
                            "{idx} = call i64 @__vyrn_map_find_i64(ptr {keys}, i64 {len}, i64 {iv}, ptr {ix}, i64 {cap})"
                        ));
                    } else if let Some((kbuf, stride)) = &packed {
                        self.emit(format!(
                            "{idx} = call i64 @__vyrn_map_find_pack(ptr {keys}, i64 {len}, ptr {kbuf}, i64 {stride}, ptr {ix}, i64 {cap})"
                        ));
                    } else {
                        self.emit(format!(
                            "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {iv}, ptr {ix}, i64 {cap})"
                        ));
                    }
                    let found = self.fresh_tmp();
                    self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
                    let some_l = self.fresh_label("map.at.some");
                    let none_l = self.fresh_label("map.at.none");
                    let end_l = self.fresh_label("map.at.end");
                    self.emit_term(format!("br i1 {found}, label %{some_l}, label %{none_l}"));
                    self.emit_label(&some_l);
                    let ep = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr {vll}, ptr {vals}, i64 {idx}"));
                    self.emit(format!("{v} = load {vll} , ptr {ep}"));
                    let (w0, w1) = self.encode_payload(&v, &val);
                    let s0 = self.fresh_tmp();
                    let s1 = self.fresh_tmp();
                    let s2 = self.fresh_tmp();
                    self.emit(format!(
                        "{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
                    ));
                    self.emit(format!(
                        "{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"
                    ));
                    self.emit(format!(
                        "{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 {w1}, 2"
                    ));
                    let some_end = self.cur_block.clone();
                    self.emit_term(format!("br label %{end_l}"));
                    self.emit_label(&none_l);
                    self.emit_term(format!("br label %{end_l}"));
                    self.emit_label(&end_l);
                    let r = self.fresh_tmp();
                    self.emit(format!(
                        "{r} = phi {{ i1, i64, i64 }} [ {s2}, %{some_end} ], \
                         [ {{ i1 0, i64 0, i64 0 }}, %{none_l} ]"
                    ));
                    return Ok((r, Type::Option(Box::new(val))));
                }
                _ => return Err("at on a non-Array value".into()),
            }
        }
        // RFC-0075 M2b: `close(s)` is the explicit half of the disposal
        // obligation. Which of the two producers a stream holds is a runtime tag,
        // so the teardown is still the runtime's branch on it — one call here,
        // whatever the stream turns out to be.
        if name == "close" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&aty) {
                Type::Stream(i) => *i,
                other => return Err(format!("close of non-Stream {other:?}")),
            };
            let s = self.fresh_alloca(STREAM_LL);
            self.emit(format!("store {STREAM_LL} {av}, ptr {s}"));
            let sym = self.stream_closer_sym(&elem);
            self.emit(format!("call void @{sym}(ptr {s})"));
            return Ok((String::new(), Type::Unit));
        }
        // `fromArray(xs)` hands the array's buffer to a stream: the three words
        // move across, the read cursor starts at 0, and the producer tag is -1,
        // which is what says "buffer" for the rest of this stream's life.
        if name == "fromArray" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let inner = match self.resolve(&aty) {
                Type::Array(i) => *i,
                other => return Err(format!("fromArray of non-Array {other:?}")),
            };
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
            let sv = self.stream_header(&format!("ptr {data}"), &len, "-1", "0", "0", "0");
            return Ok((sv, Type::Stream(Box::new(inner))));
        }
        // `fromStep(slot, gen, step)` (RFC-0075 M2b, re-hosted by RFC-0090 M3)
        // is the producer that is not a buffer. The cursor arrives from the
        // caller — `std/stream` minted it out of its own `Slots` — and the step
        // goes in the header as an RFC-0037 function value, which is the whole
        // trick: the step's own closed enum carries the site identity, so nothing
        // about it has to show up in `Stream<T>`.
        if name == "fromStep" {
            let (slot, sty) = self.gen_expr(&args[0])?;
            let (slot, _) = self.coerce(slot, &sty, &Type::Int)?;
            let (gen, gty) = self.gen_expr(&args[1])?;
            let (gen, _) = self.coerce(gen, &gty, &Type::Int)?;
            let (fv, fty) = self.gen_expr(&args[2])?;
            let sig = self.normalize_sig(&fty);
            let elem = match &sig {
                Type::Fn(_, r) => match self.resolve(r) {
                    Type::Option(i) => *i,
                    other => return Err(format!("fromStep step returns {other:?}")),
                },
                other => return Err(format!("fromStep of non-fn {other:?}")),
            };
            // The loop rebuilds this signature from the element type alone, so a
            // step registered under any other spelling would dispatch through a
            // table it is not in. Refuse rather than miscompile.
            if sig != self.normalize_sig(&stream_step_sig(&elem)) {
                return Err(format!("fromStep step of type {sig}"));
            }
            let tag = self.fresh_tmp();
            let pay = self.fresh_tmp();
            self.emit(format!("{tag} = extractvalue {{ i64, i64 }} {fv}, 0"));
            self.emit(format!("{pay} = extractvalue {{ i64, i64 }} {fv}, 1"));
            let sv = self.stream_header("ptr null", "0", &tag, &pay, &slot, &gen);
            return Ok((sv, Type::Stream(Box::new(elem))));
        }
        // `boxStream(s)` (RFC-0090 M3) moves a stream into one heap box and
        // answers its address. A `Stream<T>` may not be a field of anything (M1
        // refuses it, because a field erases the obligation), so a lazy
        // combinator's source lives here and `std/stream` keeps the address in
        // its cursor slot.
        if name == "boxStream" {
            let (av, _) = self.gen_expr(&args[0])?;
            let boxll = format!("{{ i64, {STREAM_LL} }}");
            let boxed = self.fresh_tmp();
            self.emit(format!(
                "{boxed} = call ptr @__vyrn_malloc(i64 ptrtoint (ptr getelementptr ({boxll}, ptr null, i64 1) to i64))"
            ));
            self.emit(format!("store i64 3735928559, ptr {boxed}"));
            let sp = self.fresh_tmp();
            self.emit(format!("{sp} = getelementptr i8, ptr {boxed}, i64 8"));
            self.emit(format!("store {STREAM_LL} {av}, ptr {sp}"));
            let a = self.fresh_tmp();
            self.emit(format!("{a} = ptrtoint ptr {boxed} to i64"));
            return Ok((a, Type::Int));
        }
        // `unboxStream(a)` takes the stream back out and frees the box. The magic word
        // is cleared first, so unboxing one address twice is the trap rather
        // than a second owner of one stream.
        if name == "unboxStream" {
            let elem = match self.expect.last().map(|t| self.resolve(t)) {
                Some(Type::Stream(i)) => *i,
                other => {
                    return Err(format!(
                        "`unboxStream` needs a `Stream<T>` context, found {other:?}"
                    ))
                }
            };
            let (av, aty) = self.gen_expr(&args[0])?;
            let (av, _) = self.coerce(av, &aty, &Type::Int)?;
            let d = self.fresh_tmp();
            self.emit(format!("{d} = call ptr @__vyrn_stream_box(i64 {av})"));
            let sv = self.fresh_tmp();
            self.emit(format!("{sv} = load {STREAM_LL}, ptr {d}"));
            let base = self.fresh_tmp();
            self.emit(format!("{base} = getelementptr i8, ptr {d}, i64 -8"));
            self.emit(format!("store i64 0, ptr {base}"));
            self.emit(format!("call void @__vyrn_free(ptr {base})"));
            return Ok((sv, Type::Stream(Box::new(elem))));
        }
        // `pullAt(a)` (RFC-0075 M2c) — one element from the stream in that box,
        // which is the whole of what a wrapper's step can do that an ordinary
        // producer's cannot. The element type is the annotation's: an address is
        // an `Int64` whatever it addresses, so the call carries nothing to infer
        // from (the checker says the same thing in its own words).
        if name == "pullAt" {
            let elem = match self.expect.last().map(|t| self.resolve(t)) {
                Some(Type::Option(i)) => *i,
                other => {
                    return Err(format!(
                        "`pullAt` needs an `Option<T>` context, found {other:?}"
                    ))
                }
            };
            let (av, aty) = self.gen_expr(&args[0])?;
            let (av, _) = self.coerce(av, &aty, &Type::Int)?;
            let sp = self.fresh_tmp();
            self.emit(format!("{sp} = call ptr @__vyrn_stream_box(i64 {av})"));
            let (has, stage) = self.emit_stream_next(&sp, &elem)?;
            let ell = self.llt(&elem);
            let optll = self.llt(&Type::Option(Box::new(elem.clone())));
            let some_l = self.fresh_label("psome");
            let none_l = self.fresh_label("pnone");
            let join_l = self.fresh_label("pjoin");
            self.emit_term(format!("br i1 {has}, label %{some_l}, label %{none_l}"));
            self.emit_label(&some_l);
            let ev = self.fresh_tmp();
            self.emit(format!("{ev} = load {ell}, ptr {stage}"));
            let (w0, w1) = self.encode_payload(&ev, &elem);
            let a = self.fresh_tmp();
            let bb = self.fresh_tmp();
            let cc = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {optll} undef, i1 1, 0"));
            self.emit(format!("{bb} = insertvalue {optll} {a}, i64 {w0}, 1"));
            self.emit(format!("{cc} = insertvalue {optll} {bb}, i64 {w1}, 2"));
            self.emit_term(format!("br label %{join_l}"));
            self.emit_label(&none_l);
            let nv = self.fresh_tmp();
            self.emit(format!("{nv} = insertvalue {optll} undef, i1 0, 0"));
            self.emit_term(format!("br label %{join_l}"));
            self.emit_label(&join_l);
            let res = self.fresh_tmp();
            self.emit(format!(
                "{res} = phi {optll} [ {cc}, %{some_l} ], [ {nv}, %{none_l} ]"
            ));
            return Ok((res, Type::Option(Box::new(elem))));
        }
        // `a.pop()` (RFC-0011) — remove and return the last element as
        // `Option<T>`. Loads the `{ptr,len,cap}` header from the binding's slot;
        // on `len == 0` yields `None`, otherwise loads element `len-1`, writes
        // the decremented header back, and wraps the element in `Some`. Never
        // traps. No new runtime function — all inline.
        if name == "@pop" {
            let recv = match &args[0] {
                Expr::Var { name, .. } => name.clone(),
                _ => return Err("`pop` needs a plain array variable".into()),
            };
            let (slot, aty) = self
                .lookup(&recv)
                .ok_or_else(|| format!("unbound `{recv}`"))?;
            // `SmallArray<T, N>.pop()` (RFC-0056): slot-based. Never un-spills —
            // just decrement `len` (field 0); the base is the live buffer.
            if let Type::SmallArray(inner, n) = self.resolve(&aty) {
                let elem = *inner;
                let ell = self.llt(&elem);
                let sa_ll = self.sa_ll(&elem, n);
                let (base, len, _cap, _data) = self.sa_slot_base(&slot, &elem, n);
                let empty = self.fresh_tmp();
                self.emit(format!("{empty} = icmp eq i64 {len}, 0"));
                let none_l = self.fresh_label("sapop.none");
                let some_l = self.fresh_label("sapop.some");
                let end_l = self.fresh_label("sapop.end");
                self.emit_term(format!("br i1 {empty}, label %{none_l}, label %{some_l}"));
                self.emit_label(&none_l);
                self.emit_term(format!("br label %{end_l}"));
                self.emit_label(&some_l);
                let nl = self.fresh_tmp();
                self.emit(format!("{nl} = sub i64 {len}, 1"));
                let ep = self.fresh_tmp();
                let v = self.fresh_tmp();
                self.emit(format!("{ep} = getelementptr {ell}, ptr {base}, i64 {nl}"));
                self.emit(format!("{v} = load {ell}, ptr {ep}"));
                let lenp = self.fresh_tmp();
                self.emit(format!(
                    "{lenp} = getelementptr {sa_ll}, ptr {slot}, i64 0, i32 0"
                ));
                self.emit(format!("store i64 {nl}, ptr {lenp}"));
                let (w0, w1) = self.encode_payload(&v, &elem);
                let s0 = self.fresh_tmp();
                let s1 = self.fresh_tmp();
                let s2 = self.fresh_tmp();
                self.emit(format!(
                    "{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
                ));
                self.emit(format!(
                    "{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"
                ));
                self.emit(format!(
                    "{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 {w1}, 2"
                ));
                let some_end = self.cur_block.clone();
                self.emit_term(format!("br label %{end_l}"));
                self.emit_label(&end_l);
                let r = self.fresh_tmp();
                self.emit(format!(
                    "{r} = phi {{ i1, i64, i64 }} [ {{ i1 0, i64 0, i64 0 }}, %{none_l} ], \
                     [ {s2}, %{some_end} ]"
                ));
                return Ok((r, Type::Option(Box::new(elem))));
            }
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                other => return Err(format!("`pop` needs an Array, found {other:?}")),
            };
            let ell = self.llt(&elem);
            let hdr = self.fresh_tmp();
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
            let empty = self.fresh_tmp();
            self.emit(format!("{empty} = icmp eq i64 {len}, 0"));
            let none_l = self.fresh_label("pop.none");
            let some_l = self.fresh_label("pop.some");
            let end_l = self.fresh_label("pop.end");
            self.emit_term(format!("br i1 {empty}, label %{none_l}, label %{some_l}"));
            // none: yield the empty Option aggregate.
            self.emit_label(&none_l);
            self.emit_term(format!("br label %{end_l}"));
            // some: load the last element, shrink the header, wrap in Some.
            self.emit_label(&some_l);
            let nl = self.fresh_tmp();
            self.emit(format!("{nl} = sub i64 {len}, 1"));
            let ep = self.fresh_tmp();
            let v = self.fresh_tmp();
            self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {nl}"));
            self.emit(format!("{v} = load {ell}, ptr {ep}"));
            let nh = self.fresh_tmp();
            self.emit(format!(
                "{nh} = insertvalue {{ ptr, i64, i64 }} {hdr}, i64 {nl}, 1"
            ));
            self.emit(format!("store {{ ptr, i64, i64 }} {nh}, ptr {slot}"));
            let (w0, w1) = self.encode_payload(&v, &elem);
            let s0 = self.fresh_tmp();
            let s1 = self.fresh_tmp();
            let s2 = self.fresh_tmp();
            self.emit(format!(
                "{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"
            ));
            self.emit(format!(
                "{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 {w1}, 2"
            ));
            let some_end = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            // merge: None aggregate from the empty path, Some from the other.
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {{ i1 0, i64 0, i64 0 }}, %{none_l} ], \
                 [ {s2}, %{some_end} ]"
            ));
            return Ok((r, Type::Option(Box::new(elem))));
        }
        // `a.swapRemove(i)` (RFC-0011) — bounds-check `i`, load element `i`
        // (the return value), move the last element into slot `i`, decrement the
        // header, write it back. O(1), unordered. Traps out-of-bounds with the
        // read path's wording. No new runtime function.
        if name == "@swapRemove" {
            let recv = match &args[0] {
                Expr::Var { name, .. } => name.clone(),
                _ => return Err("`swapRemove` needs a plain array variable".into()),
            };
            let (slot, aty) = self
                .lookup(&recv)
                .ok_or_else(|| format!("unbound `{recv}`"))?;
            // `SmallArray<T, N>.swapRemove(i)` (RFC-0056): slot-based, on the
            // live buffer; move the last element into slot `i`, shrink `len`.
            if let Type::SmallArray(inner, n) = self.resolve(&aty) {
                let elem = *inner;
                let ell = self.llt(&elem);
                let sa_ll = self.sa_ll(&elem, n);
                // The index is evaluated BEFORE the base/len are read: it may
                // `modify` the array, and the bounds check must trust the
                // post-mutation len.
                let (iv, _) = self.gen_expr(&args[1])?;
                let (base, len, _cap, _data) = self.sa_slot_base(&slot, &elem, n);
                let bad_l = self.fresh_label("saswap.oob");
                let ok_l = self.fresh_label("saswap.ok");
                let oob = self.fresh_tmp();
                self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                self.emit_array_oob_trap(&bad_l, &iv);
                self.emit_label(&ok_l);
                let nl = self.fresh_tmp();
                self.emit(format!("{nl} = sub i64 {len}, 1"));
                let ip = self.fresh_tmp();
                let v = self.fresh_tmp();
                self.emit(format!("{ip} = getelementptr {ell}, ptr {base}, i64 {iv}"));
                self.emit(format!("{v} = load {ell}, ptr {ip}"));
                let lp = self.fresh_tmp();
                let last = self.fresh_tmp();
                self.emit(format!("{lp} = getelementptr {ell}, ptr {base}, i64 {nl}"));
                self.emit(format!("{last} = load {ell}, ptr {lp}"));
                self.emit(format!("store {ell} {last}, ptr {ip}"));
                let lenp = self.fresh_tmp();
                self.emit(format!(
                    "{lenp} = getelementptr {sa_ll}, ptr {slot}, i64 0, i32 0"
                ));
                self.emit(format!("store i64 {nl}, ptr {lenp}"));
                return Ok((v, elem));
            }
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                other => return Err(format!("`swapRemove` needs an Array, found {other:?}")),
            };
            let ell = self.llt(&elem);
            // The index is evaluated BEFORE the header is loaded: it may
            // `modify` the array, and the bounds check must trust the
            // post-mutation len.
            let (iv, _) = self.gen_expr(&args[1])?;
            let hdr = self.fresh_tmp();
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
            self.emit(format!(
                "{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"
            ));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
            let bad_l = self.fresh_label("swap.oob");
            let ok_l = self.fresh_label("swap.ok");
            let oob = self.fresh_tmp();
            self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
            self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
            self.emit_array_oob_trap(&bad_l, &iv);
            self.emit_label(&ok_l);
            let nl = self.fresh_tmp();
            self.emit(format!("{nl} = sub i64 {len}, 1"));
            let ip = self.fresh_tmp();
            let v = self.fresh_tmp();
            self.emit(format!("{ip} = getelementptr {ell}, ptr {data}, i64 {iv}"));
            self.emit(format!("{v} = load {ell}, ptr {ip}"));
            let lp = self.fresh_tmp();
            let last = self.fresh_tmp();
            self.emit(format!("{lp} = getelementptr {ell}, ptr {data}, i64 {nl}"));
            self.emit(format!("{last} = load {ell}, ptr {lp}"));
            self.emit(format!("store {ell} {last}, ptr {ip}"));
            let nh = self.fresh_tmp();
            self.emit(format!(
                "{nh} = insertvalue {{ ptr, i64, i64 }} {hdr}, i64 {nl}, 1"
            ));
            self.emit(format!("store {{ ptr, i64, i64 }} {nh}, ptr {slot}"));
            return Ok((v, elem));
        }
        // `xs.toArray()` (RFC-0056) — copy a `SmallArray<T, N>`'s live elements
        // into a fresh heap buffer and wrap it in the growable `{ptr,len,cap}`
        // triple. The one explicit conversion; the interpreter's copy is the
        // identity (both share `Val::Array`).
        //
        // The result is a fresh `Array<T>` and an array owns its elements
        // (RFC-0092 M2), so the words it copies are given their own heap. Before
        // M2 this handed back the receiver's element POINTERS and the census
        // counted it as one of the three view constructors.
        if name == "@toArray" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let inner = match self.resolve(&aty) {
                Type::SmallArray(inner, _) => *inner,
                // A plain `Array<T>` receiver. Handing the triple straight back
                // named ONE buffer from two owned bindings, and both are
                // released — a double free of the buffer before M2 and of every
                // element after it. The comment here already said "a defensive
                // copy-out"; M2 is where the code says it too.
                Type::Array(inner) => {
                    let c = self.deep_copy(&av, &Type::Array(inner.clone()))?;
                    return Ok((c, Type::Array(inner)));
                }
                other => return Err(format!("`toArray` needs a SmallArray, found {other:?}")),
            };
            let n = match self.resolve(&aty) {
                Type::SmallArray(_, n) => n,
                _ => unreachable!(),
            };
            let ell = self.llt(&inner);
            let (base, len) = self.sa_value_base_len(&av, &inner, n);
            let esz = self.fresh_tmp();
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"
            ));
            let nb = self.fresh_tmp();
            self.emit(format!("{nb} = mul i64 {len}, {esz}"));
            let buf = self.fresh_tmp();
            self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {nb})"));
            self.emit(format!(
                "call void @llvm.memcpy.p0.p0.i64(ptr {buf}, ptr {base}, i64 {nb}, i1 false)"
            ));
            self.copy_elems(&buf, &len, &inner)?;
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!(
                "{a} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"
            ));
            self.emit(format!(
                "{b} = insertvalue {{ ptr, i64, i64 }} {a}, i64 {len}, 1"
            ));
            self.emit(format!(
                "{c} = insertvalue {{ ptr, i64, i64 }} {b}, i64 {len}, 2"
            ));
            return Ok((c, Type::Array(Box::new(inner))));
        }
        // `x.copy()` (RFC-0089 M1b) — a value of the receiver's type that shares
        // no heap with it. The reported type is the receiver's own, so a copy of
        // a validated `type Email = String` is still an `Email`.
        if name == "@copy" {
            // RFC-0091 M1: a type that declares `impl Copy for T` says what
            // duplicating it means, so the call goes there instead. The
            // receiver's type is named before it is emitted, exactly as the
            // `place at` dispatch names it.
            if let Some(m) = self
                .static_ty(&args[0])
                .and_then(|t| vyrn_frontend::types::copy_impl(self.impls, &t))
            {
                return self.gen_call(&m, args);
            }
            let (v, ty) = self.gen_expr(&args[0])?;
            let c = self.deep_copy(&v, &ty)?;
            return Ok((c, ty));
        }
        // `m.has(k)` (RFC-0028) — membership test → i1. Read-only; the receiver
        // is any Map-typed expression (an SSA aggregate).
        if name == "@has" {
            let (mv, mty) = self.gen_expr(&args[0])?;
            let (ik, pk_ty) = match self.resolve(&mty) {
                Type::Map(k, _) => (
                    self.key_is_int(&k),
                    self.key_is_pack(&k).then(|| (*k).clone()),
                ),
                _ => (false, None),
            };
            let (kv, _) = self.gen_expr(&args[1])?;
            let packed = pk_ty.map(|kt| self.emit_key_pack(&kv, &kt));
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 0"
            ));
            self.emit(format!(
                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 2"
            ));
            let (ix, cap) = self.map_index_of(&mv);
            let idx = self.fresh_tmp();
            if ik {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_i64(ptr {keys}, i64 {len}, i64 {kv}, ptr {ix}, i64 {cap})"
                ));
            } else if let Some((kbuf, stride)) = &packed {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_pack(ptr {keys}, i64 {len}, ptr {kbuf}, i64 {stride}, ptr {ix}, i64 {cap})"
                ));
            } else {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {kv}, ptr {ix}, i64 {cap})"
                ));
            }
            let found = self.fresh_tmp();
            self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
            return Ok((found, Type::Bool));
        }
        // `m.remove(k)` (RFC-0028) — remove the entry (order-preserving shift of
        // the survivors), return whether it was present. Mutates the binding.
        if name == "@remove" {
            let recv = match &args[0] {
                Expr::Var { name, .. } => name.clone(),
                _ => return Err("`remove` needs a plain map variable".into()),
            };
            let (slot, aty) = self
                .lookup(&recv)
                .ok_or_else(|| format!("unbound `{recv}`"))?;
            let (key_t, val) = match self.resolve(&aty) {
                Type::Map(k, v) => (*k, *v),
                other => return Err(format!("`remove` needs a Map, found {other:?}")),
            };
            let ik = self.key_is_int(&key_t);
            let vll = self.llt(&val);
            let esz = self.fresh_tmp();
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({vll}, ptr null, i64 1) to i64"
            ));
            let hdr = self.fresh_tmp();
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            let (kv, _) = self.gen_expr(&args[1])?;
            let packed = self
                .key_is_pack(&key_t)
                .then(|| self.emit_key_pack(&kv, &key_t));
            self.emit(format!(
                "{hdr} = load {{ ptr, ptr, i64, i64, ptr }}, ptr {slot}"
            ));
            self.emit(format!(
                "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 0"
            ));
            self.emit(format!(
                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 2"
            ));
            let (ix, cap) = self.map_index_of(&hdr);
            let idx = self.fresh_tmp();
            if ik {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_i64(ptr {keys}, i64 {len}, i64 {kv}, ptr {ix}, i64 {cap})"
                ));
            } else if let Some((kbuf, stride)) = &packed {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find_pack(ptr {keys}, i64 {len}, ptr {kbuf}, i64 {stride}, ptr {ix}, i64 {cap})"
                ));
            } else {
                self.emit(format!(
                    "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {kv}, ptr {ix}, i64 {cap})"
                ));
            }
            let found = self.fresh_tmp();
            self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
            let do_l = self.fresh_label("map.rm.do");
            let end_l = self.fresh_label("map.rm.end");
            self.emit_term(format!("br i1 {found}, label %{do_l}, label %{end_l}"));
            self.emit_label(&do_l);
            // The map took the key and the value, so the map hands both back
            // when the entry goes — and this is the only place that can, because
            // `__vyrn_map_remove_at` is handed two strides and no types. Read
            // out of their slots BEFORE the shift moves the survivors over them.
            // Only the arena is asked: an entry a `remove` drops is unreachable
            // afterwards whoever owns the map, and nothing aliases it (RFC-0092
            // M2 made `keys()` copy).
            if self.region_depth == 0 {
                let vals = self.fresh_tmp();
                self.emit(format!(
                    "{vals} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {hdr}, 1"
                ));
                // An Int64 key owns nothing to hand back (RFC-0117), and a
                // packed user key (M2) is heapless by the checker rule.
                if !ik && packed.is_none() {
                    let kp = self.fresh_tmp();
                    self.emit(format!("{kp} = getelementptr ptr, ptr {keys}, i64 {idx}"));
                    self.release_entry(&kp, &Type::Str)?;
                }
                let vp = self.fresh_tmp();
                self.emit(format!("{vp} = getelementptr {vll}, ptr {vals}, i64 {idx}"));
                self.release_entry(&vp, &val)?;
            }
            if let Some((_, stride)) = &packed {
                self.emit(format!(
                    "call void @__vyrn_map_remove_at_pack(ptr {slot}, i64 {idx}, i64 {esz}, i64 {stride})"
                ));
            } else {
                let rmat = if ik {
                    "__vyrn_map_remove_at_i64"
                } else {
                    "__vyrn_map_remove_at"
                };
                self.emit(format!(
                    "call void @{rmat}(ptr {slot}, i64 {idx}, i64 {esz})"
                ));
            }
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            return Ok((found, Type::Bool));
        }
        // `m.keys()` (RFC-0028) — a fresh snapshot `Array<String>` in insertion
        // order. Copies the key pointers into a new buffer (cap = len), and
        // since RFC-0092 M2 the KEYS as well: the snapshot is an `Array<String>`
        // and an array owns its elements, so a snapshot of the map's own
        // pointers would be freed twice. The interpreter has copied each key
        // since RFC-0028 (`Rc::new(k.clone())`), so this is the two compiling
        // backends catching up with the oracle, not a new cost in the model.
        if name == "@keys" {
            let (mv, mty) = self.gen_expr(&args[0])?;
            let key_t = match self.resolve(&mty) {
                Type::Map(k, _) => *k,
                _ => Type::Str,
            };
            let ik = self.key_is_int(&key_t);
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!(
                "{keys} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 0"
            ));
            self.emit(format!(
                "{len} = extractvalue {{ ptr, ptr, i64, i64, ptr }} {mv}, 2"
            ));
            let buf = self.fresh_tmp();
            if self.key_is_pack(&key_t) {
                // Packed keys copy by bytes; the pack IS the value layout
                // (fields at their own offsets), so the buffer is already a
                // valid Array<K> of heapless elements (RFC-0117 M2).
                let kll = self.llt(&key_t);
                let stride = self.fresh_tmp();
                self.emit(format!(
                    "{stride} = ptrtoint ptr getelementptr ({kll}, ptr null, i64 1) to i64"
                ));
                self.emit(format!(
                    "{buf} = call ptr @__vyrn_map_keys_copy_pack(ptr {keys}, i64 {len}, i64 {stride})"
                ));
            } else if ik {
                // Int64 keys copy by value — the snapshot IS the copy, and
                // there is nothing per-element to duplicate (RFC-0117).
                self.emit(format!(
                    "{buf} = call ptr @__vyrn_map_keys_copy_i64(ptr {keys}, i64 {len})"
                ));
            } else {
                self.emit(format!(
                    "{buf} = call ptr @__vyrn_map_keys_copy(ptr {keys}, i64 {len})"
                ));
                self.copy_elems(&buf, &len, &Type::Str)?;
            }
            let r0 = self.fresh_tmp();
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            self.emit(format!(
                "{r0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"
            ));
            self.emit(format!(
                "{r1} = insertvalue {{ ptr, i64, i64 }} {r0}, i64 {len}, 1"
            ));
            self.emit(format!(
                "{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {len}, 2"
            ));
            return Ok((r2, Type::Array(Box::new(key_t))));
        }
        // value(x) -> Value: box a scalar into the built-in `Value` enum, using the
        // same payload encoding as any enum variant (so `match` decodes it).
        if name == "value" {
            let (v, ty) = self.gen_expr(&args[0])?;
            let vname = match self.resolve(&ty) {
                Type::Int => "IntVal",
                Type::Bool => "BoolVal",
                Type::Str => "StrVal",
                other => return Err(format!("`value` cannot box {other:?}")),
            };
            let (tag, enum_name) = self
                .variants
                .get(vname)
                .cloned()
                .ok_or_else(|| "built-in `Value` enum is not registered".to_string())?;
            let ll = enum_ll(self.enum_arity(&enum_name));
            let payload = self.box_payload(&v, &ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {ll} undef, i64 {tag}, 0"));
            self.emit(format!("{b} = insertvalue {ll} {a}, i64 {payload}, 1"));
            return Ok((b, Type::Named(enum_name)));
        }
        // list(Array<T, N>) -> Array<T>: copy the fixed value aggregate into a
        // heap buffer and wrap it as a growable `{ ptr, len, cap }` triple.
        if name == "@list" {
            let (v, ty) = self.gen_expr(&args[0])?;
            match self.resolve(&ty) {
                Type::Array(inner) => return Ok((v, Type::Array(inner))), // already growable
                Type::ArrayN(inner, _) => {
                    let (triple, out) = self.array_n_to_heap(&v, &inner, &ty)?;
                    return Ok((triple, out));
                }
                other => return Err(format!("`@list` needs an Array, found {other:?}")),
            }
        }
        // @join (`t.join()`), RFC-0025: block until the task completes, then
        // load its result from the frame's leading slot.
        //
        // Since RFC-0095 M1 the join CONSUMES the task, so this is the one join
        // and the task's storage goes back here: the result is loaded out of the
        // frame first, then `__vyrn_task_release` frees the frame, frees the
        // record and closes the event handle. The order is the whole safety
        // argument — `__vyrn_join` answers with the frame's ADDRESS, so the load
        // has to happen before the free, and a second `t.join()` is refused at
        // compile time rather than reading freed memory.
        //
        // The result is a VALUE in a register after the load, which is what makes
        // the free safe for a `Task<String>` too: the frame held the pointer, and
        // the caller now owns the buffer it points at.
        if name == "@join" {
            let (v, ty) = self.gen_expr(&args[0])?;
            let inner = match self.resolve(&ty) {
                Type::Task(inner) => *inner,
                // Defensive: a non-Task operand is already the value (the
                // checker never lets this through; keep the old identity).
                other => return Ok((v, other)),
            };
            let frame = self.fresh_tmp();
            self.emit(format!("{frame} = call ptr @__vyrn_join(ptr {v})"));
            let retll = self.llt(&inner);
            if retll == "void" {
                self.emit(format!("call void @__vyrn_task_release(ptr {v})"));
                return Ok((String::new(), Type::Unit));
            }
            let t = self.fresh_tmp();
            self.emit(format!("{t} = load {retll}, ptr {frame}"));
            self.emit(format!("call void @__vyrn_task_release(ptr {v})"));
            return Ok((t, inner));
        }

        // `Some(x)` — the payload may be any type (boxed if wider than a word),
        // so the Option is `Option<typeof x>`.
        if name == "Some" {
            // RFC-0037: an enclosing `Option<T>` expectation types the payload
            // (a lambda literal payload needs its fn signature).
            let payload_expect: Option<Type> =
                self.expect.last().and_then(|t| match self.resolve(t) {
                    Type::Option(i) => Some(*i),
                    _ => None,
                });
            let pushed = payload_expect.is_some();
            if let Some(t) = &payload_expect {
                self.expect.push(t.clone());
            }
            let r = self.gen_expr(&args[0]);
            if pushed {
                self.expect.pop();
            }
            let (v, ty) = r?;
            let (v, ty) = self.coerce_into_payload(v, ty, payload_expect.as_ref())?;
            let (w0, w1) = self.encode_payload(&v, &ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!(
                "{a} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"
            ));
            self.emit(format!(
                "{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"
            ));
            self.emit(format!(
                "{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"
            ));
            return Ok((c, Type::Option(Box::new(ty))));
        }
        // `Ok(x)` / `Err(e)` — the payload may be any type (encoded like Some).
        // The *other* type parameter is unknown at the constructor (a placeholder
        // `Int`); `match`/`?` decode by the scrutinee's real `Result<T, E>` type.
        if let Some(tag) = match name {
            "Ok" => Some(1),
            "Err" => Some(0),
            _ => None,
        } {
            // RFC-0037: an enclosing `Result<T, E>` expectation types the arm.
            let payload_expect: Option<Type> =
                self.expect.last().and_then(|t| match self.resolve(t) {
                    Type::Result(ok, err) => Some(if name == "Ok" { *ok } else { *err }),
                    _ => None,
                });
            let pushed = payload_expect.is_some();
            if let Some(t) = &payload_expect {
                self.expect.push(t.clone());
            }
            let r = self.gen_expr(&args[0]);
            if pushed {
                self.expect.pop();
            }
            let (v, ty) = r?;
            let (v, ty) = self.coerce_into_payload(v, ty, payload_expect.as_ref())?;
            let (w0, w1) = self.encode_payload(&v, &ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!(
                "{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {tag}, 0"
            ));
            self.emit(format!(
                "{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"
            ));
            self.emit(format!(
                "{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"
            ));
            let out = if name == "Ok" {
                Type::Result(Box::new(ty), Box::new(Type::Int))
            } else {
                Type::Result(Box::new(Type::Int), Box::new(ty))
            };
            return Ok((c, out));
        }

        // enum variant with payload(s): `Circle(x)`, `Rect(w, h)`
        if let Some((tag, enum_name)) = self.variants.get(name).cloned() {
            let arity = self.enum_arity(&enum_name);
            let ll = enum_ll(arity);
            // The variant's DECLARED payload types. Each argument is coerced into
            // its declared type *before* boxing, so the boxed representation is
            // exactly the one `match` unboxes. This is load-bearing for wide
            // values whose literal form differs from their declared form: an
            // array literal is a fixed `[N x T]` value, but a declared
            // `Array<T>` payload is the growable `{ptr,len,cap}` triple — box the
            // former and unboxStream the latter and the raw elements are reinterpreted
            // as a header (the RFC-0026 corruption bug). A generic variant whose
            // payload is still an unresolved type parameter keeps the argument's
            // own type (the inline-monomorphized path).
            let decl_payload: Vec<Type> = match self.types.get(&enum_name).map(|d| d.base.clone()) {
                Some(Type::Enum(vs)) => vs
                    .iter()
                    .find(|v| v.name == name)
                    .map(|v| v.payload.clone())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            // gen each payload, coercing to its declared type, boxing any wider
            // than a word.
            let mut payloads = Vec::new();
            let mut arg_tys = Vec::new();
            for (i, a) in args.iter().enumerate() {
                // RFC-0037: the declared payload type is the expected type for
                // a lambda-literal payload.
                let pushed = matches!(decl_payload.get(i),
                    Some(dt) if !matches!(self.resolve(dt), Type::Param(_)));
                if pushed {
                    self.expect.push(decl_payload[i].clone());
                }
                let r = self.gen_expr(a);
                if pushed {
                    self.expect.pop();
                }
                let (v, ty) = r?;
                let (v, ty) = match decl_payload.get(i) {
                    Some(dt) if !matches!(self.resolve(dt), Type::Param(_)) => {
                        self.coerce(v, &ty, dt)?
                    }
                    _ => (v, ty),
                };
                arg_tys.push(ty.clone());
                payloads.push(self.box_payload(&v, &ty));
            }
            let mut cur = "undef".to_string();
            let t = self.fresh_tmp();
            self.emit(format!("{t} = insertvalue {ll} {cur}, i64 {tag}, 0"));
            cur = t;
            for slot in 1..=arity {
                let val = payloads
                    .get(slot - 1)
                    .cloned()
                    .unwrap_or_else(|| "0".into());
                let t = self.fresh_tmp();
                self.emit(format!("{t} = insertvalue {ll} {cur}, i64 {val}, {slot}"));
                cur = t;
            }
            let applied = self.applied_enum_type(&enum_name, name, &arg_tys);
            return Ok((cur, applied));
        }

        // construction of a validated type: `Age(expr)`
        if let Some(decl) = self.types.get(name).cloned() {
            // A record name here would be a struct literal (handled elsewhere);
            // only validated scalars reach construction.
            if matches!(decl.base, Type::Record(_)) {
                return Err(format!("`{name}` is a record type; use `{name} {{ .. }}`"));
            }
            return self.gen_construction(&decl, &args[0]);
        }

        // Protocol-method dispatch (RFC-0002 §5): resolve `m(recv, ..)` to the
        // impl for the receiver's concrete type (after monomorphization), then
        // emit a call to that mangled impl function.
        if let Some(proto) = self.protocol_methods.get(name).cloned() {
            // The receiver's static type, and the argument list the mangled call
            // gets. A variable answers from `lookup` and travels unchanged — it
            // has to, since a `modify self` is passed as the caller's own slot.
            //
            // Anything else (`get(..).cacheFor(3600)`, RFC-0084 M2) is emitted
            // HERE, exactly once, and parked in a slot the mangled call reads as
            // an ordinary variable. `gen_call` re-generates its argument list, so
            // handing it the receiver expression a second time would evaluate the
            // receiver twice and run its effects twice with it. The binding's name
            // has an `@` in it, so no program can spell or shadow it — the
            // same parking `gen_try_fallible` does with an already-emitted value.
            let (recv_ty, args) = match args.first() {
                Some(Expr::Var { name: v, .. }) => (
                    self.lookup(v)
                        .map(|(_, t)| t)
                        .ok_or_else(|| format!("unbound receiver `{v}`"))?,
                    args.to_vec(),
                ),
                Some(recv) => {
                    let line = Expr::line(recv);
                    let (v, ty) = self.gen_expr(recv)?;
                    let ll = self.llt(&ty);
                    let name = format!("@recv.{}", self.tmp);
                    let slot = self.declare(&name, &ty);
                    self.emit(format!("store {ll} {v}, ptr {slot}"));
                    let mut rest = args.to_vec();
                    rest[0] = Expr::Var { name, line };
                    (ty, rest)
                }
                None => return Err(format!("protocol method `{name}` has no receiver")),
            };
            // Substitute generic params (monomorphization) but keep named types,
            // so an enum receiver keys on its name rather than its aggregate.
            let concrete = vyrn_frontend::types::substitute(&recv_ty, self.subst);
            let key = vyrn_frontend::types::type_key(&concrete)
                .ok_or_else(|| format!("cannot dispatch `{name}` on {recv_ty:?}"))?;
            let mangled = vyrn_frontend::types::impl_method_name(&proto, &key, name);
            return self.gen_call(&mangled, &args);
        }

        // `extern` call (RFC-0012): emit the real host call. This is the one
        // call whose behavior differs by target — the shared IR carries the
        // import, and the C trap stub (native) vs the `vyrn` namespace (wasm)
        // decides what it does. String args cross as `(ptr, len)`.
        if let Some(callee) = self.funcs.get(name).copied() {
            if callee.is_extern {
                return self.gen_extern_call(callee, args);
            }
        }

        // Generic callee: solve its type arguments (concrete, under our subst),
        // mangle its symbol, and register the instantiation to emit later.
        let callee = self.funcs.get(name).copied();
        let is_generic = callee.map(|c| !c.type_params.is_empty()).unwrap_or(false);
        if is_generic {
            let callee = callee.unwrap();
            // The concrete type of each argument (parameters substituted away).
            //
            // A `modify` parameter crosses as the ADDRESS of the caller's
            // binding, exactly as it does in the ordinary call below. The
            // definition emits `ptr %argN` for one whether or not the function
            // is generic, so a generic call that handed over the value disagreed
            // with the callee's own ABI and the program read a record out of a
            // pointer-sized register. No corpus function was both generic and
            // `modify`, so nothing caught it until `Slots<T>` was both.
            let mut arg_tys = Vec::new();
            let mut arg_vals = Vec::new();
            let mut arg_ptrs: Vec<Option<String>> = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if callee.params.get(i).map(|p| p.capability) == Some(Capability::Modify) {
                    if let Expr::Var { name: vn, .. } = a {
                        if let Some((slot, ty)) = self.lookup(vn) {
                            arg_tys.push(vyrn_frontend::types::substitute(&ty, self.subst));
                            arg_vals.push(String::new());
                            arg_ptrs.push(Some(slot));
                            continue;
                        }
                    }
                    return Err(format!("`modify` argument to `{name}` must be a variable"));
                }
                let (v, vty) = self.gen_expr(a)?;
                arg_tys.push(vyrn_frontend::types::substitute(&vty, self.subst));
                arg_vals.push(v);
                arg_ptrs.push(None);
            }
            // Bind each type parameter from the matching argument. An unsolved
            // one becomes `Unit` here — see `solve_type_args`.
            let want = self
                .expect
                .last()
                .map(|t| vyrn_frontend::types::substitute(t, self.subst));
            let (call_subst, solved) = solve_with_expected(
                &callee.type_params,
                &callee
                    .params
                    .iter()
                    .map(|p| p.ty.clone())
                    .collect::<Vec<_>>(),
                &arg_tys,
                &callee.ret,
                want.as_ref(),
            );
            let type_args: Vec<Type> = solved
                .into_iter()
                .map(|t| t.unwrap_or(Type::Unit))
                .collect();
            let sym = mangle_name(name, &type_args);

            // Coerce args to their (substituted) parameter types.
            let mut arg_ops = Vec::new();
            for (((p, v), aty), slot) in callee
                .params
                .iter()
                .zip(arg_vals)
                .zip(&arg_tys)
                .zip(&arg_ptrs)
            {
                if let Some(slot) = slot {
                    arg_ops.push(format!("ptr {slot}"));
                    continue;
                }
                let pty = vyrn_frontend::types::substitute(&p.ty, &call_subst);
                let (v, cty) = self.coerce(v, aty, &pty)?;
                arg_ops.push(format!("{} {v}", self.llt(&cty)));
            }
            self.instantiations.push((name.to_string(), type_args));

            let ret_ty = vyrn_frontend::types::substitute(&callee.ret, &call_subst);
            let retll = self.llt(&ret_ty);
            return if retll == "void" {
                self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
                Ok(("".into(), Type::Unit))
            } else {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = call {retll} @{sym}({})", arg_ops.join(", ")));
                Ok((t, ret_ty))
            };
        }

        // Ordinary call: coerce each argument to its parameter type.
        let params = self.param_types.get(name).cloned().unwrap_or_default();
        let caps = self.param_caps.get(name).cloned().unwrap_or_default();
        let mut arg_ops = Vec::new();
        for (i, a) in args.iter().enumerate() {
            // A `modify` parameter is passed by reference: hand over the caller's
            // slot pointer (the checker guaranteed the argument is a mut variable).
            if caps.get(i) == Some(&Capability::Modify) {
                if let Expr::Var { name: vn, .. } = a {
                    if let Some((slot, _)) = self.lookup(vn) {
                        arg_ops.push(format!("ptr {slot}"));
                        continue;
                    }
                }
                return Err(format!("`modify` argument to `{name}` must be a variable"));
            }
            // RFC-0037: the declared parameter type is the expected type for a
            // lambda-carrying argument (e.g. `takes(Some(|x| x))`).
            let pushed = params.get(i).is_some();
            if let Some(p) = params.get(i) {
                self.expect.push(p.clone());
            }
            let frees_mark = self.arg_frees.len();
            let r = self.gen_expr(a);
            if pushed {
                self.expect.pop();
            }
            let (v, vty) = r?;
            let was_fixed = matches!(self.resolve(&vty), Type::ArrayN(..) | Type::SmallArray(..));
            let (v, pty) = match params.get(i) {
                Some(p) => self.coerce_flow(v, a, &vty, p)?,
                None => (v, vty),
            };
            // RFC-0114 §25's heapify row: an array-literal argument the plan
            // recorded is a temporary the CALLER releases — but the hook in
            // `gen_expr` fired before the coercion, on the fixed VALUE the
            // literal is, and a fixed value owns nothing to free. What the
            // coercion allocated is the growable triple in `v` now, so the
            // entry the hook pushed is retargeted at the coerced product.
            if self.arg_frees.len() > frees_mark
                && was_fixed
                && matches!(self.resolve(&pty), Type::Array(_))
            {
                if let Some(last) = self.arg_frees.last_mut() {
                    *last = (v.clone(), pty.clone());
                }
            }
            arg_ops.push(format!("{} {v}", self.llt(&pty)));
        }
        let sym = fn_sym(name);
        let ret = self.ret_types.get(name).cloned().unwrap_or(Type::Int);
        let retll = self.llt(&ret);
        if retll == "void" {
            self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
            Ok(("".into(), Type::Unit))
        } else {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {retll} @{sym}({})", arg_ops.join(", ")));
            Ok((t, ret))
        }
    }

    /// Lower `spawn f(args)` (RFC-0025) to real-thread machinery in the shim.
    ///
    /// The spawn site knows the concrete callee (spawn is monomorphic — `f` is
    /// named statically), so: evaluate + coerce every argument NOW (the eager
    /// interpreter's evaluation order), pack them into a malloc'd frame whose
    /// leading slot is the result, synthesize a per-callee thunk
    /// `void @__vyrn_task_<sym>(ptr %frame)` that loads the arguments back and
    /// calls the callee DIRECTLY, and emit
    /// `call ptr @__vyrn_spawn(ptr @thunk, ptr %frame)`.
    ///
    /// The thunk symbol handed to the shim is a function pointer at the C
    /// boundary ONLY — no Vyrn-level function value exists, every emitted
    /// `call` still names an `@symbol` (the RFC-0023 invariant), and the wasm
    /// module gains no indirect-call table entry from Vyrn code (the shim's
    /// inline `thunk(frame)` is C, compiled per target). The thunk is keyed by
    /// the callee's mangled symbol: its content is a pure function of that
    /// symbol, so spawn sites of the same callee share one thunk (deduped by
    /// the `lambda_defs` driver).
    fn gen_spawn(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
        let (sym, arg_vals, arg_tys, ret_ty) = self.prep_spawn_target(name, args)?;
        let retll = self.llt(&ret_ty);
        // Copy every heap-owning argument OUT into fresh `malloc`'d storage
        // before it is stored into the frame. The frame is heap and outlives
        // this block, but a value's BUFFER does not follow the frame: inside a
        // `region` a String's bytes come from the arena, and
        // `__vyrn_region_exit` frees every block at the closing brace — while
        // the worker may only run after it. The interpreter hands the callee
        // its own value, so the frame holds an owned copy and never a pointer
        // into storage this block is about to reclaim. `deep_copy` is that
        // walk; the region flag is forced to 0 around it so a String's fresh
        // buffer is `malloc`'d rather than routed into the very arena the exit
        // frees (`str_alloc` compiles the routing in from this flag — the same
        // rule the lifted-lambda body compiles in).
        let saved_region_depth = self.region_depth;
        self.region_depth = 0;
        let mut owned_vals: Vec<String> = Vec::with_capacity(arg_vals.len());
        for ((_, v), ty) in arg_vals.iter().zip(&arg_tys) {
            owned_vals.push(self.deep_copy(v, ty)?);
        }
        self.region_depth = saved_region_depth;
        // Frame layout: { result, args... } — result first so `join` loads it
        // straight off the frame pointer. A Unit task has no result slot.
        let mut fields: Vec<String> = Vec::new();
        if retll != "void" {
            fields.push(retll.clone());
        }
        for (ll, _) in &arg_vals {
            fields.push(ll.clone());
        }
        let frame_ty = format!("{{ {} }}", fields.join(", "));
        let frame = self.fresh_tmp();
        self.emit(format!(
            "{frame} = call ptr @__vyrn_malloc(i64 ptrtoint (ptr getelementptr \
             ({frame_ty}, ptr null, i32 1) to i64))"
        ));
        let base = usize::from(retll != "void");
        for (i, ((ll, _), cv)) in arg_vals.iter().zip(&owned_vals).enumerate() {
            let p = self.fresh_tmp();
            self.emit(format!(
                "{p} = getelementptr {frame_ty}, ptr {frame}, i32 0, i32 {}",
                base + i
            ));
            self.emit(format!("store {ll} {cv}, ptr {p}"));
        }

        let tsym = format!("__vyrn_task_{sym}");
        let mut def = String::new();
        def.push_str(&format!("define void @{tsym}(ptr %frame) {{\nentry:\n"));
        let mut ops: Vec<String> = Vec::new();
        for (i, (ll, _)) in arg_vals.iter().enumerate() {
            def.push_str(&format!(
                "  %p{i} = getelementptr {frame_ty}, ptr %frame, i32 0, i32 {}\n",
                base + i
            ));
            def.push_str(&format!("  %a{i} = load {ll}, ptr %p{i}\n"));
            ops.push(format!("{ll} %a{i}"));
        }
        if retll == "void" {
            def.push_str(&format!("  call void @{sym}({})\n", ops.join(", ")));
        } else {
            def.push_str(&format!("  %r = call {retll} @{sym}({})\n", ops.join(", ")));
            def.push_str(&format!("  store {retll} %r, ptr %frame\n"));
        }
        def.push_str("  ret void\n}\n\n");
        self.lambda_defs.push((tsym.clone(), def));

        let t = self.fresh_tmp();
        self.emit(format!(
            "{t} = call ptr @__vyrn_spawn(ptr @{tsym}, ptr {frame})"
        ));
        Ok((t, Type::Task(Box::new(ret_ty))))
    }

    /// Resolve a spawn callee exactly as `gen_call` would: evaluate and coerce
    /// each argument to its (substituted) parameter type, solve + register a
    /// generic instantiation when the callee is generic, and return the callee
    /// symbol, the `(llvm type, value)` argument pairs, each argument's concrete
    /// parameter type, and the concrete return type. `modify` parameters and
    /// externs cannot appear — the checker only admits isolated (spawn-safe)
    /// callees.
    fn prep_spawn_target(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<(String, Vec<(String, String)>, Vec<Type>, Type), String> {
        let callee = self.funcs.get(name).copied();
        if let Some(callee) = callee.filter(|c| !c.type_params.is_empty()) {
            // Generic callee — mirror gen_call's instantiation solving.
            let mut arg_tys = Vec::new();
            let mut arg_vals = Vec::new();
            for a in args {
                let (v, vty) = self.gen_expr(a)?;
                arg_tys.push(vyrn_frontend::types::substitute(&vty, self.subst));
                arg_vals.push(v);
            }
            let want = self
                .expect
                .last()
                .map(|t| vyrn_frontend::types::substitute(t, self.subst));
            let (call_subst, solved) = solve_with_expected(
                &callee.type_params,
                &callee
                    .params
                    .iter()
                    .map(|p| p.ty.clone())
                    .collect::<Vec<_>>(),
                &arg_tys,
                &callee.ret,
                want.as_ref(),
            );
            let type_args: Vec<Type> = solved
                .into_iter()
                .map(|t| t.unwrap_or(Type::Unit))
                .collect();
            let sym = mangle_name(name, &type_args);
            let mut pairs = Vec::new();
            let mut ptys: Vec<Type> = Vec::new();
            for ((p, v), aty) in callee.params.iter().zip(arg_vals).zip(&arg_tys) {
                let pty = vyrn_frontend::types::substitute(&p.ty, &call_subst);
                let (v, cty) = self.coerce(v, aty, &pty)?;
                ptys.push(cty.clone());
                pairs.push((self.llt(&cty), v));
            }
            self.instantiations.push((name.to_string(), type_args));
            let ret_ty = vyrn_frontend::types::substitute(&callee.ret, &call_subst);
            return Ok((sym, pairs, ptys, ret_ty));
        }
        let params = self.param_types.get(name).cloned().unwrap_or_default();
        let mut pairs = Vec::new();
        let mut ptys: Vec<Type> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let (v, vty) = self.gen_expr(a)?;
            let (v, pty) = match params.get(i) {
                Some(p) => self.coerce_flow(v, a, &vty, p)?,
                None => (v, vty.clone()),
            };
            ptys.push(pty.clone());
            pairs.push((self.llt(&pty), v));
        }
        let sym = fn_sym(name);
        let ret = self.ret_types.get(name).cloned().unwrap_or(Type::Int);
        Ok((sym, pairs, ptys, ret))
    }

    /// Emit a real call to an `extern` import (RFC-0012). Each argument is
    /// coerced to its declared parameter type, then to the ABI value type; a
    /// `String` crosses as a `(ptr, strlen)` pair. The result is converted from
    /// the ABI type back to the value's Vyrn representation. The callee symbol
    /// (`@__vyrn_extern_<name>`) resolves to the host import (wasm) or the linked
    /// C trap stub (native) — the IR is identical either way.
    fn gen_extern_call(&mut self, f: &Function, args: &[Expr]) -> Result<(String, Type), String> {
        let mut arg_ops = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let (v, vty) = self.gen_expr(a)?;
            let pty = f.params[i].ty.clone();
            let (v, cty) = self.coerce(v, &vty, &pty)?;
            if matches!(self.resolve(&cty), Type::Str) {
                // String → (ptr, len): the callee decodes UTF-8 from linear
                // memory (strings are immutable, so decode-on-cross is safe).
                let len = self.str_len(&v);
                arg_ops.push(format!("ptr {v}"));
                arg_ops.push(format!("i64 {len}"));
            } else {
                let (abi_v, abi_ll) = self.to_extern_abi(&v, &cty);
                arg_ops.push(format!("{abi_ll} {abi_v}"));
            }
        }
        let sym = host_boundary_extern(&f.name)
            .map(str::to_string)
            .unwrap_or_else(|| extern_symbol(&f.name));
        let ret_ll = extern_abi_ll(&f.ret);
        if ret_ll == "void" {
            self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
            Ok((String::new(), Type::Unit))
        } else {
            let raw = self.fresh_tmp();
            self.emit(format!(
                "{raw} = call {ret_ll} @{sym}({})",
                arg_ops.join(", ")
            ));
            let v = self.from_extern_abi(&raw, &f.ret);
            Ok((v, f.ret.clone()))
        }
    }

    /// Widen a value from its native representation to the extern ABI value type
    /// (RFC-0012): `Bool` (`i1`) and sub-word ints extend to `i32`; `Int64`/`f64`/
    /// `f32`/`ptr` pass through. Returns `(operand, ABI llvm type)`.
    fn to_extern_abi(&mut self, v: &str, ty: &Type) -> (String, &'static str) {
        match self.resolve(ty) {
            Type::Bool => {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = zext i1 {v} to i32"));
                (t, "i32")
            }
            Type::IntN { bits: 64, .. } => (v.to_string(), "i64"),
            Type::IntN { bits: 32, .. } => (v.to_string(), "i32"),
            Type::IntN { bits, signed } => {
                let op = if signed { "sext" } else { "zext" };
                let t = self.fresh_tmp();
                self.emit(format!("{t} = {op} i{bits} {v} to i32"));
                (t, "i32")
            }
            Type::Float => (v.to_string(), "double"),
            Type::Float32 => (v.to_string(), "float"),
            Type::Str => (v.to_string(), "ptr"),
            // Int64 and anything else the checker admitted.
            _ => (v.to_string(), "i64"),
        }
    }

    /// Narrow an extern ABI result back to the value's native representation
    /// (inverse of [`to_extern_abi`]): `i32`→`i1` for `Bool`, `i32`→`iN` for a
    /// sub-word int; others pass through.
    fn from_extern_abi(&mut self, raw: &str, ty: &Type) -> String {
        match self.resolve(ty) {
            Type::Bool => {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = trunc i32 {raw} to i1"));
                t
            }
            Type::IntN { bits: 64, .. } | Type::IntN { bits: 32, .. } | Type::Int => {
                raw.to_string()
            }
            Type::IntN { bits, .. } => {
                let t = self.fresh_tmp();
                self.emit(format!("{t} = trunc i32 {raw} to i{bits}"));
                t
            }
            _ => raw.to_string(),
        }
    }

    /// Emit a validated-type construction. A compile-time-constant argument (the
    /// checker proved it valid) erases to the value; otherwise emit a runtime
    /// predicate check that prints and `exit(1)`s on failure.
    fn gen_construction(&mut self, decl: &TypeDecl, arg: &Expr) -> Result<(String, Type), String> {
        let named = Type::Named(decl.name.clone());
        let (v, _) = self.gen_expr(arg)?;
        // A constant was already proven by the checker (a violation is a
        // compile error), so only dynamic values pay for a runtime check.
        let is_const = vyrn_frontend::consteval::eval(arg, &HashMap::new()).is_some();
        if !is_const {
            self.emit_validation(decl, &v)?;
        }
        Ok((v, named))
    }

    /// Emit the inline runtime check that a value satisfies `decl`'s `where`
    /// predicate, trapping with the canonical per-type message otherwise. A
    /// scalar base binds `value`; a record base binds every field (the
    /// cross-field predicate references them by name). Shared by explicit
    /// construction (`Age(n)`) and every automatic-validation coercion.
    fn emit_validation(&mut self, decl: &TypeDecl, v: &str) -> Result<(), String> {
        if decl.predicate.is_none() {
            return Ok(());
        }
        let cond = self.emit_predicate_cond(decl, v)?;
        let nok = self.fresh_tmp();
        self.emit(format!("{nok} = xor i1 {cond}, true"));
        self.trap_if(&nok, &format!("@.trap.verr.{}", decl.name), "vfail");
        Ok(())
    }

    /// The program's OWN `where` predicate node for `decl` — see [`Gen::decls`].
    ///
    /// `Ok(None)` is a type with no refinement. Every `decl` that reaches this
    /// emitter was cloned out of [`Gen::types`], which is `decl_map`'s copy of
    /// this list, so a decl that HAS a predicate and is not in the list is a
    /// decl the program does not hold — a refusal rather than a silently
    /// skipped validation.
    fn predicate(&self, decl: &TypeDecl) -> Result<Option<&'a Expr>, String> {
        if decl.predicate.is_none() {
            return Ok(None);
        }
        self.decls
            .iter()
            .find(|d| d.name == decl.name && d.base == decl.base)
            .and_then(|d| d.predicate.as_ref())
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "a `where` clause on `{}`, which is not one of the program's own                      type declarations",
                    decl.name
                )
            })
    }

    /// Lower a refined type's `where` predicate to an `i1` (true = holds),
    /// binding the value under check as [`predicate_binds`] says to. This is the
    /// ONE place a predicate is lowered to LLVM — the trap path
    /// (`emit_validation`) and the JSON decode `validate` path (RFC-0018) both
    /// derive from it, so the two never drift.
    fn emit_predicate_cond(&mut self, decl: &TypeDecl, v: &str) -> Result<String, String> {
        let pred = self.predicate(decl)?.expect("predicate present");
        let rec_ll = self.llt(&decl.base);
        self.scope.push(Vec::new());
        for (name, ty, field) in predicate_binds(decl) {
            let val = match field {
                Some(i) => {
                    let ext = self.fresh_tmp();
                    self.emit(format!("{ext} = extractvalue {rec_ll} {v}, {i}"));
                    ext
                }
                None => v.to_string(),
            };
            let slot = self.declare(&name, &ty);
            let ll = self.llt(&ty);
            self.emit(format!("store {ll} {val}, ptr {slot}"));
        }
        let was = crate::observe::set_ctx("pred");
        let cond = self.gen_expr(pred);
        crate::observe::set_ctx(was);
        let (cond, _) = cond?;
        self.scope.pop();
        Ok(cond)
    }
}

/// Whether a pattern matches the tag-1 variant (`Some`/`Ok`). Only used on the
/// Option/Result path; user-enum variants go through `gen_match_enum`.
fn pattern_is_one(p: &Pattern) -> bool {
    matches!(p, Pattern::Some(_) | Pattern::Ok(_) | Pattern::Success(_))
}

/// The name a pattern binds its payload to, if any.
fn pattern_binding(p: &Pattern) -> Option<&str> {
    match p {
        Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => Some(b),
        // `??`'s pair (RFC-0079). `Failure` binds on the `Option` path too, where
        // the payload type is the `Type::Int` placeholder `gen_match` passes for
        // a tag-0 `Option` arm: a dead `alloca`+`store` of a word nothing reads,
        // which is cheaper than teaching this type-free helper about types.
        Pattern::Success(b) | Pattern::Failure(b) => Some(b),
        // Variants route through gen_match_enum, not this Option/Result helper.
        Pattern::Variant(_, b) => b.first().map(|s| s.as_str()),
        Pattern::None | Pattern::Other => None,
    }
}

/// LLVM byte-string escaping: printable ASCII as-is, everything else `\NN`,
/// plus a trailing NUL. Returns (escaped, total byte length).
/// The wording both compiling backends print for `serveStream` (RFC-0074 M3a).
/// One constant so the two engines cannot drift, which is the rule every trap
/// message in this project follows.
pub(crate) fn serve_stream_trap() -> String {
    vyrn_frontend::trap::line(vyrn_frontend::trap::SERVE_STREAM)
}

/// A static `String` value in the data segment: the `{ i64 len, i64 cap }` header
/// (RFC-0089 M1a) followed by the NUL-terminated bytes. `cap` is [`STR_STATIC`],
/// the runtime's word for "never `realloc`, never free" — `@__vyrn_str_free`
/// reads it and returns, so a drop site needs no static/heap analysis of its own.
fn static_str_global(name: &str, s: &str) -> String {
    let (escaped, len) = llvm_str(s);
    format!(
        "{name} = private unnamed_addr constant {{ i64, i64, [{len} x i8] }} \
         {{ i64 {}, i64 {STR_STATIC}, [{len} x i8] c\"{escaped}\" }}, align 8\n",
        s.len()
    )
}

/// The `String` value of a global emitted by [`static_str_global`] — a constant
/// `getelementptr` past the header, usable anywhere a `ptr` operand is.
fn static_str_ptr(name: &str, s: &str) -> String {
    let len = s.len() + 1;
    format!("getelementptr inbounds ({{ i64, i64, [{len} x i8] }}, ptr {name}, i64 0, i32 2)")
}

/// One `@.trap.*` / `@.fmt.*` global holding a wording from
/// [`vyrn_frontend::trap`] — RFC-0101 M5.
///
/// The array length used to be hand-counted beside each literal (`[25 x i8]`
/// against `"error: division by zero\0A\00"`), which is the second copy of the
/// message: a reworded trap needed the number recounted, and nothing checked it.
/// `llvm_str` already measures what it escapes, so this takes the wording and
/// writes both.
fn trap_global(name: &str, msg: &str) -> String {
    let (escaped, len) = llvm_str(msg);
    format!("{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n")
}

fn llvm_str(s: &str) -> (String, usize) {
    let mut out = String::new();
    for b in s.bytes() {
        if (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\{b:02X}"));
        }
    }
    out.push_str("\\00");
    (out, s.len() + 1)
}

/// Björn Höhrmann's UTF-8 validation DFA table: 256 byte-class entries followed
/// by a 108-entry (9 states × 12 classes) transition table. State 0 is ACCEPT,
/// 12 is REJECT. Used by `@__vyrn_utf8valid` so the native decoders reject exactly
/// what Rust's `String::from_utf8` rejects (overlong forms, surrogates, > U+10FFFF).
///
/// Shared with the direct wasm backend (RFC-0077 M2g), which puts the same bytes
/// in a data segment and walks them with the same two loads. A second table would
/// have been a second answer to "is this valid UTF-8", free to drift by a byte.
///
/// The table below is his, byte for byte, and it is the one piece of third-party
/// code in this repository. His terms are MIT and they require the notice to
/// travel with every copy, including the binaries this emits it into:
///
/// ```text
/// Copyright (c) 2008-2009 Bjoern Hoehrmann <bjoern@hoehrmann.de>
/// See http://bjoern.hoehrmann.de/utf-8/decoder/dfa/ for details.
/// ```
///
/// The full permission notice is in `THIRD-PARTY-NOTICES.md` at the repository
/// root, which the release archive ships.
pub(crate) fn utf8d_table() -> Vec<u8> {
    let mut t = vec![0u8; 256];
    for b in 0x80..=0x8F {
        t[b] = 1;
    }
    for b in 0x90..=0x9F {
        t[b] = 9;
    }
    for b in 0xA0..=0xBF {
        t[b] = 7;
    }
    t[0xC0] = 8;
    t[0xC1] = 8;
    for b in 0xC2..=0xDF {
        t[b] = 2;
    }
    t[0xE0] = 10;
    for b in 0xE1..=0xEC {
        t[b] = 3;
    }
    t[0xED] = 4;
    t[0xEE] = 3;
    t[0xEF] = 3;
    t[0xF0] = 11;
    for b in 0xF1..=0xF3 {
        t[b] = 6;
    }
    t[0xF4] = 5;
    for b in 0xF5..=0xFF {
        t[b] = 8;
    }
    #[rustfmt::skip]
    let trans: [u8; 108] = [
        0,12,24,36,60,96,84,12,12,12,48,72,
        12,12,12,12,12,12,12,12,12,12,12,12,
        12, 0,12,12,12,12,12, 0,12, 0,12,12,
        12,24,12,12,12,12,12,24,12,24,12,12,
        12,12,12,12,12,12,12,24,12,12,12,12,
        12,24,12,12,12,12,12,12,12,24,12,12,
        12,12,12,12,12,12,12,36,12,36,12,12,
        12,36,12,12,12,12,12,36,12,36,12,12,
        12,36,12,12,12,12,12,12,12,12,12,12,
    ];
    t.extend_from_slice(&trans);
    t
}

/// If `value` is `name + e1 + e2 + …` — a `+` chain whose left spine bottoms
/// out in a bare `name` — the appended operands in written order. The chain
/// matters: `out + a + ", "` parses as `Add(Add(Var(out), a), ", ")`, so the
/// accumulator sits at the far end of the spine, not under the top `+`.
fn self_append_spine<'e>(name: &str, value: &'e Expr) -> Option<Vec<&'e Expr>> {
    let mut parts: Vec<&Expr> = Vec::new();
    let mut cur = value;
    while let Expr::Binary {
        op: BinOp::Add,
        lhs,
        rhs,
        ..
    } = cur
    {
        parts.push(rhs);
        cur = lhs;
    }
    match cur {
        Expr::Var { name: n, .. } if n == name && !parts.is_empty() => {
            parts.reverse();
            Some(parts)
        }
        _ => None,
    }
}

/// The local `String` accumulators of one function body that `s = s + …` may
/// grow IN PLACE (see `Gen::emit_str_append`).
///
/// In-place growth reallocates, so every other holder of the old pointer is
/// invalidated — `let copy = out` before an append must keep reading "a". The
/// interpreter is safe here because `Rc::make_mut` clones a shared buffer;
/// native code has no refcount, so eligibility is decided statically and the
/// rule is a WHITELIST: a name qualifies only if every occurrence of it in the
/// function is a use that provably cannot retain the pointer — the root of a
/// self-append, a `.field` read (a String's fields are byte/char counts),
/// an operand of the interpolation desugar (which copies), or a tail
/// `return`. Anything else — another `let`, any user call, a record
/// field, an array element, a lambda body, an unrecognized builtin — bans the
/// name. An unknown callee is therefore ineligible by construction, so a new
/// retaining builtin cannot silently make this unsound.
fn append_candidates(body: &Block) -> std::collections::HashSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    scan_append_block(body, &mut targets, &mut banned, false);
    targets.retain(|n| !banned.contains(n));
    targets
}

/// The **module-state** `String` accumulators of a whole program (census P1).
///
/// The same whitelist, read over every body instead of one, because a global is
/// reachable from all of them: a name qualifies when some body grows it with
/// `g = g + …` and NO body puts a pointer to it anywhere that could outlive the
/// grow. `let t = g` is one of the things that bans a name, which is exactly the
/// aliasing guard a global needs and a local already had.
///
/// P1 measured what not having this costs: 4.92 s and 12.2 GB to build a 160 KB
/// string, against 0.095 s for the identical local. The global did not qualify
/// for one reason — the whitelist read one body — and every server that
/// accumulates a response body is a module-state accumulator.
///
/// A body that binds the name LOCALLY votes on neither side, because inside it
/// the name is not the global. Without that filter one `let out` among the
/// hundreds of linked `std/` functions disqualifies a module-state `out`, and the
/// first measurement of this pass hit exactly that.
/// The result is a `BTreeSet` and not a `HashSet` because one caller ITERATES
/// it: the direct backend reserves an ownership word per accumulator, and a
/// reservation is an address baked into every `i32.const` that reads or writes
/// it — and it shifts every later reservation, so the whole static map moves.
/// `RandomState` is seeded per process, so two accumulators were a coin flip and
/// three built six different modules from one source. Sorted here rather than at
/// that loop, because the next caller to iterate it would have to know.
pub(crate) fn global_append_candidates(program: &Program) -> std::collections::BTreeSet<String> {
    let mut targets = std::collections::HashSet::new();
    let mut banned = std::collections::HashSet::new();
    let mut one = |body: &Block, params: &[Param]| {
        let (mut t, mut ban) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        scan_append_block(body, &mut t, &mut ban, false);
        let mut shadowed: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        bound_names(body, &mut shadowed);
        targets.extend(t.into_iter().filter(|n| !shadowed.contains(n)));
        banned.extend(ban.into_iter().filter(|n| !shadowed.contains(n)));
    };
    for f in &program.functions {
        one(&f.body, &f.params);
    }
    for t in &program.tests {
        one(&t.body, &[]);
    }
    for bn in &program.benches {
        one(&bn.body, &[]);
    }
    // A global's own initializer runs once and cannot append, but a name it reads
    // is a name held somewhere this walk should see.
    for g in &program.globals {
        ban_append_expr(&g.init, &mut banned, false);
    }
    targets.retain(|n| !banned.contains(n));
    targets.retain(|n| program.globals.iter().any(|g| &g.name == n));
    targets.into_iter().collect()
}

/// Every name a block binds anywhere inside it — `let`s, loop variables, pattern
/// binders and lambda parameters. Over-collecting is safe here: the only use is
/// to decide that a body is talking about its own name rather than about module
/// state, and an extra name only costs a global the in-place append path.
fn bound_names(b: &Block, out: &mut std::collections::HashSet<String>) {
    fn in_expr(e: &Expr, out: &mut std::collections::HashSet<String>) {
        match e {
            Expr::Lambda { params, body, .. } => {
                out.extend(params.iter().cloned());
                match body {
                    LambdaBody::Expr(inner) => in_expr(inner, out),
                    LambdaBody::Block(blk) => bound_names(blk, out),
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                in_expr(scrutinee, out);
                for a in arms {
                    out.extend(pattern_names(&a.pattern));
                    match &a.body {
                        ArmBody::Expr(e) => in_expr(e, out),
                        ArmBody::Block(b) => bound_names(b, out),
                    }
                }
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                in_expr(expr, out)
            }
            Expr::Consume { place, .. } => in_expr(place, out),
            Expr::Binary { lhs, rhs, .. } => {
                in_expr(lhs, out);
                in_expr(rhs, out);
            }
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => args.iter().for_each(|a| in_expr(a, out)),
            Expr::StructLit { fields, .. } => fields.iter().for_each(|(_, v)| in_expr(v, out)),
            Expr::MapLit { entries, .. } => entries.iter().for_each(|(k, v)| {
                in_expr(k, out);
                in_expr(v, out);
            }),
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                in_expr(cond, out);
                in_expr(then_branch, out);
                if let Some(eb) = else_branch {
                    in_expr(eb, out);
                }
            }
            Expr::Int(_)
            | Expr::Byte(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Var { .. } => {}
        }
    }
    for s in &b.stmts {
        match s {
            Stmt::Let { name, value, .. } => {
                out.insert(name.clone());
                in_expr(value, out);
            }
            Stmt::Assign { value, .. } | Stmt::SetField { value, .. } | Stmt::Expr(value) => {
                in_expr(value, out)
            }
            Stmt::IndexSet { index, value, .. } => {
                in_expr(index, out);
                in_expr(value, out);
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    in_expr(e, out);
                }
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                in_expr(cond, out);
                bound_names(then_block, out);
                if let Some(eb) = else_block {
                    bound_names(eb, out);
                }
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                out.extend(pattern_names(pattern));
                in_expr(scrutinee, out);
                bound_names(then_block, out);
                if let Some(eb) = else_block {
                    bound_names(eb, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                in_expr(cond, out);
                bound_names(body, out);
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                out.insert(var.clone());
                in_expr(iter, out);
                bound_names(body, out);
            }
            Stmt::Region { body, .. } => bound_names(body, out),
            Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

/// The names a refutable pattern binds.
fn pattern_names(p: &Pattern) -> Vec<String> {
    match p {
        Pattern::Some(n)
        | Pattern::Ok(n)
        | Pattern::Err(n)
        | Pattern::Success(n)
        | Pattern::Failure(n) => vec![n.clone()],
        Pattern::Variant(_, ns) => ns.clone(),
        Pattern::None | Pattern::Other => Vec::new(),
    }
}

/// Walk a block collecting append targets and banned names. `strict` marks a
/// lambda body: everything inside one is banned outright, because a capture
/// copies the pointer into a value that outlives the append.
fn scan_append_block(
    b: &Block,
    targets: &mut std::collections::HashSet<String>,
    banned: &mut std::collections::HashSet<String>,
    strict: bool,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } => ban_append_expr(value, banned, strict),
            Stmt::Assign { name, value, .. } => match self_append_spine(name, value) {
                Some(parts) if !strict => {
                    targets.insert(name.clone());
                    // The accumulator may not appear on the right as well:
                    // `out = out + out` would read a buffer the realloc moved.
                    for p in parts {
                        ban_append_expr(p, banned, strict);
                    }
                }
                _ => ban_append_expr(value, banned, strict),
            },
            Stmt::SetField { value, .. } | Stmt::Expr(value) => {
                ban_append_expr(value, banned, strict)
            }
            Stmt::IndexSet { index, value, .. } => {
                ban_append_expr(index, banned, strict);
                ban_append_expr(value, banned, strict);
            }
            // Returning the accumulator hands off the buffer at the point the
            // frame dies — nothing can append after it.
            Stmt::Return { value: Some(e), .. } => ban_append_read(e, banned, strict),
            // `drop s` frees the buffer; leave that path on the general lowering.
            Stmt::Drop { name, .. } => {
                banned.insert(name.clone());
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                ban_append_expr(scrutinee, banned, strict);
                scan_append_block(then_block, targets, banned, strict);
                if let Some(eb) = else_block {
                    scan_append_block(eb, targets, banned, strict);
                }
            }
            Stmt::While { cond, body, .. } => {
                ban_append_expr(cond, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::ForIn { iter, body, .. } => {
                ban_append_expr(iter, banned, strict);
                scan_append_block(body, targets, banned, strict);
            }
            Stmt::Region { body, .. } => scan_append_block(body, targets, banned, strict),
            Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

/// A position that does not retain its operand: a bare variable there is fine.
fn ban_append_read(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    if strict || !matches!(e, Expr::Var { .. }) {
        ban_append_expr(e, banned, strict);
    }
}

/// Does `op` leave one of its operands holding a String pointer it did not
/// copy? If so, an accumulator there could be left pointing at a buffer a later
/// in-place append `realloc`'d away, and the name must be banned.
///
/// Today the answer is no for all nineteen, so the guard in `ban_append_expr`
/// looks vacuous — it is not, and this function is why it may not be deleted:
/// the match is exhaustive so a new operator cannot be added without deciding,
/// and a String-borrowing operator (a `slice`-like `..`, say) would flip its
/// arm and re-ban its operands with no other change. Getting this wrong is a
/// use-after-free, so the decision is recorded per operator with its lowering:
///
/// - `+` on two Strings — `emit_str_concat`: `malloc(la+lb+1)` then
///   `strcpy`/`strcat`. A fresh buffer, which is exactly why `@concat` (what
///   `"\{out}]"` desugars to) is already whitelisted above. `Code + Code` takes
///   two arena handles, not pointers, and a `Code` name never owns a shadow.
/// - `== != < <= > >=` on two Strings — one `strcmp` and an `icmp`, result `i1`.
/// - `=~` — `@__vyrn_regex_run(ptr s, …)`, result `i1`; the right operand must
///   be a literal pattern, so only the left can even be an accumulator.
/// - `- * / % && || & | ^ << >>` — `binop_type` refuses a `String` operand
///   outright (arithmetic and bitwise need matching numerics, `&&`/`||` need
///   `Bool`), so no String reaches these lowerings at all.
///
/// A `<T: Ord>` operand monomorphized to `String` reaches the same lowerings
/// through the same operators, so the list covers generics too.
fn binop_retains_str(op: BinOp) -> bool {
    match op {
        BinOp::Add
        | BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::LtEq
        | BinOp::Gt
        | BinOp::GtEq
        | BinOp::Match => false,
        BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Rem
        | BinOp::And
        | BinOp::Or
        | BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::Shl
        | BinOp::Shr => false,
    }
}

/// Ban every variable `e` mentions in a position that might retain it. The
/// match is exhaustive on purpose: a new `Expr` variant must be classified
/// rather than silently fall into a permissive default.
fn ban_append_expr(e: &Expr, banned: &mut std::collections::HashSet<String>, strict: bool) {
    match e {
        Expr::Var { name, .. } => {
            banned.insert(name.clone());
        }
        // A String's fields are `byteLength`/`charCount` — an Int, not a borrow.
        Expr::Field { expr, .. } => ban_append_read(expr, banned, strict),
        // The two copying builtins the interpolation desugar emits: `@str`
        // strdups its argument, `@concat` builds a fresh buffer from both
        // halves. Only these two, because the lexer cannot produce a leading
        // `@` — no local binding can shadow the name and turn the call into a
        // dispatch through a stored function value that keeps what it is given.
        // (`print` is spellable, and `let print = f` does exactly that.)
        Expr::Call { name, args, .. } if matches!(name.as_str(), "@str" | "@concat") => {
            for a in args {
                ban_append_read(a, banned, strict);
            }
        }
        Expr::Call { args, .. }
        | Expr::Spawn { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                ban_append_expr(a, banned, strict);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } => ban_append_expr(expr, banned, strict),
        // A take hands the stored place the buffer itself, so the root is
        // banned exactly as a bare mention is.
        Expr::Consume { place, .. } => ban_append_expr(place, banned, strict),
        // An operator's operands are a retaining position only if the LOWERING
        // keeps the pointer. `binop_retains_str` is the decision, exhaustive on
        // `BinOp` so a new operator cannot be added without making one.
        //
        // Banning every operand was the whole of `toJson`'s O(N²): `return out +
        // "]"` at the end of `std/json`'s `emitArr` disqualified `out`, so every
        // element re-`malloc`'d and re-copied the entire result so far (and
        // leaked the previous buffer, which is why 50k records OOM'd on 2.5 MB
        // of output). 80k `Int64` natively: 23.5 s before, 12 ms after. Forty
        // more `return acc + "…"` sites across `std/` were in the same trap.
        Expr::Binary { op, lhs, rhs, .. } => {
            if binop_retains_str(*op) {
                ban_append_expr(lhs, banned, strict);
                ban_append_expr(rhs, banned, strict);
            } else {
                ban_append_read(lhs, banned, strict);
                ban_append_read(rhs, banned, strict);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            ban_append_expr(scrutinee, banned, strict);
            for arm in arms {
                match &arm.body {
                    ArmBody::Expr(e) => ban_append_expr(e, banned, strict),
                    // A block arm (RFC-0118): the throwaway target set means a
                    // self-append inside it keeps the copying path — correct,
                    // just not upgraded; the in-place path can follow demand.
                    ArmBody::Block(b) => {
                        scan_append_block(b, &mut std::collections::HashSet::new(), banned, strict)
                    }
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            ban_append_expr(cond, banned, strict);
            ban_append_expr(then_branch, banned, strict);
            if let Some(eb) = else_branch {
                ban_append_expr(eb, banned, strict);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                ban_append_expr(v, banned, strict);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                ban_append_expr(k, banned, strict);
                ban_append_expr(v, banned, strict);
            }
        }
        // A capture outlives the append, so nothing a lambda touches is eligible.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => ban_append_expr(e, banned, true),
            LambdaBody::Block(b) => {
                scan_append_block(b, &mut std::collections::HashSet::new(), banned, true)
            }
        },
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

/// Collect distinct `=~` pattern literals (first-seen order) from a block.
fn collect_regex_block(b: &Block, out: &mut Vec<String>) {
    for s in &b.stmts {
        collect_regex_stmt(s, out);
    }
}

fn collect_regex_stmt(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
            collect_regex_expr(value, out)
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_regex_expr(e, out);
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_regex_expr(cond, out);
            collect_regex_block(then_block, out);
            if let Some(eb) = else_block {
                collect_regex_block(eb, out);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            collect_regex_expr(scrutinee, out);
            collect_regex_block(then_block, out);
            if let Some(eb) = else_block {
                collect_regex_block(eb, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_regex_expr(cond, out);
            collect_regex_block(body, out);
        }
        Stmt::ForIn { iter, body, .. } => {
            collect_regex_expr(iter, out);
            collect_regex_block(body, out);
        }
        Stmt::IndexSet { index, value, .. } => {
            collect_regex_expr(index, out);
            collect_regex_expr(value, out);
        }
        Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => collect_regex_expr(e, out),
        Stmt::Region { body, .. } => collect_regex_block(body, out),
    }
}

fn collect_regex_expr(e: &Expr, out: &mut Vec<String>) {
    match e {
        // A `s =~ "pat"` node contributes its literal pattern.
        Expr::Binary {
            op: BinOp::Match,
            lhs,
            rhs,
            ..
        } => {
            collect_regex_expr(lhs, out);
            if let Expr::Str(pat) = &**rhs {
                if !out.contains(pat) {
                    out.push(pat.clone());
                }
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_regex_expr(lhs, out);
            collect_regex_expr(rhs, out);
        }
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            collect_regex_expr(expr, out)
        }
        Expr::Consume { place, .. } => collect_regex_expr(place, out),
        Expr::Call { args, .. } | Expr::TryConstruct { args, .. } | Expr::Spawn { args, .. } => {
            for a in args {
                collect_regex_expr(a, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_regex_expr(scrutinee, out);
            for a in arms {
                match &a.body {
                    ArmBody::Expr(e) => collect_regex_expr(e, out),
                    ArmBody::Block(b) => collect_regex_block(b, out),
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_regex_expr(cond, out);
            collect_regex_expr(then_branch, out);
            if let Some(eb) = else_branch {
                collect_regex_expr(eb, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                collect_regex_expr(v, out);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for e in elems {
                collect_regex_expr(e, out);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                collect_regex_expr(k, out);
                collect_regex_expr(v, out);
            }
        }
        // A `=~` pattern inside a lambda body (RFC-0023) must be pooled too.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => collect_regex_expr(e2, out),
            LambdaBody::Block(b) => collect_regex_block(b, out),
        },
        Expr::Int(_)
        | Expr::Byte(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Var { .. } => {}
    }
}

/// Collect distinct string-literal contents (first-seen order) from a block. The
/// `types` map lets `schemaOf`/`jsonSchema` seed their compile-time-computed strings.
fn collect_strings_block(b: &Block, out: &mut Vec<String>, types: &HashMap<String, TypeDecl>) {
    for s in &b.stmts {
        collect_strings_stmt(s, out, types);
    }
}

fn collect_strings_stmt(s: &Stmt, out: &mut Vec<String>, types: &HashMap<String, TypeDecl>) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
            collect_strings_expr(value, out, types)
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_strings_expr(e, out, types);
            }
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_strings_expr(cond, out, types);
            collect_strings_block(then_block, out, types);
            if let Some(eb) = else_block {
                collect_strings_block(eb, out, types);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            collect_strings_expr(scrutinee, out, types);
            collect_strings_block(then_block, out, types);
            if let Some(eb) = else_block {
                collect_strings_block(eb, out, types);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_strings_expr(cond, out, types);
            collect_strings_block(body, out, types);
        }
        Stmt::ForIn { iter, body, .. } => {
            collect_strings_expr(iter, out, types);
            collect_strings_block(body, out, types);
        }
        Stmt::IndexSet { index, value, .. } => {
            collect_strings_expr(index, out, types);
            collect_strings_expr(value, out, types);
        }
        Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Expr(e) => collect_strings_expr(e, out, types),
        Stmt::Region { body, .. } => collect_strings_block(body, out, types),
    }
}

fn collect_strings_expr(e: &Expr, out: &mut Vec<String>, types: &HashMap<String, TypeDecl>) {
    match e {
        Expr::Str(s) => {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Var { .. } => {}
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            collect_strings_expr(expr, out, types)
        }
        Expr::Consume { place, .. } => collect_strings_expr(place, out, types),
        Expr::Binary { lhs, rhs, .. } => {
            collect_strings_expr(lhs, out, types);
            collect_strings_expr(rhs, out, types);
        }
        Expr::Call { name, args, .. } => {
            // `schemaOf` lowers to a `Schema` literal carrying synthetic string
            // literals (the type's name, base spelling, doc, pattern); walk the
            // exact expression the code generator will emit so every one of
            // them lands in the pool.
            if name == "schemaOf" {
                if let Some(Expr::Var { name: tn, .. }) = args.first() {
                    if let Some(decl) = types.get(tn) {
                        let sl = vyrn_frontend::types::schema_struct_lit(decl);
                        collect_strings_expr(&sl, out, types);
                    }
                }
            }
            // `jsonSchema(TypeName)` lowers to a single computed JSON string literal;
            // seed the exact string the code generator will emit (see `gen_call`).
            if name == "jsonSchema" {
                if let Some(Expr::Var { name: tn, .. }) = args.first() {
                    if let Some(decl) = types.get(tn) {
                        let js = vyrn_frontend::types::json_schema_string(decl, types);
                        if !out.contains(&js) {
                            out.push(js);
                        }
                    }
                }
            }
            for a in args {
                collect_strings_expr(a, out, types);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_strings_expr(scrutinee, out, types);
            for a in arms {
                match &a.body {
                    ArmBody::Expr(e) => collect_strings_expr(e, out, types),
                    ArmBody::Block(b) => collect_strings_block(b, out, types),
                }
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_strings_expr(cond, out, types);
            collect_strings_expr(then_branch, out, types);
            if let Some(eb) = else_branch {
                collect_strings_expr(eb, out, types);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                collect_strings_expr(v, out, types);
            }
        }
        Expr::TryConstruct { args, .. } => {
            for a in args {
                collect_strings_expr(a, out, types);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for e in elems {
                collect_strings_expr(e, out, types);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                collect_strings_expr(k, out, types);
                collect_strings_expr(v, out, types);
            }
        }
        Expr::Spawn { args, .. } => {
            for e in args {
                collect_strings_expr(e, out, types);
            }
        }
        // String literals inside a lambda body (RFC-0023) join the module's
        // string pool so the monomorphized lambda function can reference them.
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e2) => collect_strings_expr(e2, out, types),
            LambdaBody::Block(b) => collect_strings_block(b, out, types),
        },
    }
}

/// The type arguments a construction or call site instantiates a generic with:
/// `declared` are the parametric types (a function's parameters, an enum
/// variant's payloads, a record's fields), `actual` the concrete types supplied
/// for them. Each declared type parameter comes back `Some` if the match fixed
/// it and `None` if nothing did.
///
/// This is **the rule**, shared rather than reimplemented (RFC-0077 M2e). Two
/// backends solving one site differently would specialize *different* functions
/// for the same source, and nothing in this repo compares the two backends'
/// instantiation sets — it is a disagreement no test would name.
///
/// What each backend does with a `None` is its own business, which is why this
/// reports rather than decides. The LLVM emitter has always substituted `Unit`
/// and let it lower to `void`; the direct backend refuses, because a `void` in a
/// wasm signature is not a diagnostic, it is a different function.
/// The type arguments of a generic call, with any parameter the arguments left
/// open taken from the type the call site expects.
///
/// `fn newSlots<T>() -> Slots<T>` has nothing to read its `T` off: the empty
/// container carries no element. The checker answers from the expected type, so
/// this must answer the same way or the two disagree about which instance the
/// program calls.
pub(crate) fn solve_with_expected(
    type_params: &[String],
    params: &[Type],
    arg_tys: &[Type],
    ret: &Type,
    expected: Option<&Type>,
) -> (HashMap<String, Type>, Vec<Option<Type>>) {
    let (mut subst, solved) = solve_type_args(type_params, params, arg_tys);
    if !solved.iter().any(|t| t.is_none()) {
        return (subst, solved);
    }
    let Some(want) = expected else {
        return (subst, solved);
    };
    let (from_ret, ret_solved) = solve_type_args(
        type_params,
        std::slice::from_ref(ret),
        std::slice::from_ref(want),
    );
    for (tp, t) in from_ret {
        subst.entry(tp).or_insert(t);
    }
    let solved = solved
        .into_iter()
        .zip(ret_solved)
        .map(|(a, b)| a.or(b))
        .collect();
    (subst, solved)
}

pub(crate) fn solve_type_args(
    type_params: &[String],
    declared: &[Type],
    actual: &[Type],
) -> (HashMap<String, Type>, Vec<Option<Type>>) {
    let mut subst: HashMap<String, Type> = HashMap::new();
    for (d, a) in declared.iter().zip(actual) {
        solve_param(d, a, &mut subst);
    }
    let args = type_params.iter().map(|p| subst.get(p).cloned()).collect();
    (subst, args)
}

/// The concrete type a construction site of `name` produces: the bare
/// [`Type::Named`] when the declaration takes no parameters, and otherwise a
/// [`Type::App`] with each parameter solved from what was supplied. An unsolved
/// parameter becomes `Unit`, which is what this has always done for enums.
pub(crate) fn applied_type(
    decl: Option<&TypeDecl>,
    name: &str,
    declared: &[Type],
    actual: &[Type],
) -> Type {
    let named = || Type::Named(name.to_string());
    let Some(decl) = decl.filter(|d| !d.type_params.is_empty()) else {
        return named();
    };
    let (_, args) = solve_type_args(&decl.type_params, declared, actual);
    Type::App(
        name.to_string(),
        args.into_iter().map(|a| a.unwrap_or(Type::Unit)).collect(),
    )
}

/// The type arguments a construction site's EXPECTED type already names.
///
/// [`applied_type`] reads a generic's arguments off what is supplied — a record's
/// field values, a variant's payload. That is the only source when there is no
/// other, and it is enough for a field whose value carries its own type. It is
/// NOT enough for a field that carries a `fn` (RFC-0037): a stored `fn` value
/// registers its dispatch variant against the type it is being built FOR, so if
/// that type is still `fn(P) -> T` when the value is built, the variant lands
/// under a signature no dispatcher covers.
///
/// The site's own expectation knows the answer before any field is read. This
/// takes the arguments it names, and only the ones it settles: a `Unit`
/// placeholder or an open `Param` says nothing, so the value-side solve keeps
/// those.
///
/// Shared with the direct wasm backend, for [`applied_type`]'s reason — two
/// backends seeding one construction differently would build two different
/// types for one literal.
/// Whether a field's value can settle a type parameter at all.
///
/// An empty `[]`/`[:]` reports a PLACEHOLDER element type — the representation
/// is type-independent, so the element type is picked rather than known. Letting
/// it settle the record's parameter binds the placeholder: `Deque { back: ["z"],
/// front: [] }` solved `T = Int64` from the empty `front` (emitted first, in
/// DECLARED order) and then stored a String pointer into an `i64` element. It
/// settles nothing; a later field, or the site's expectation, answers. The
/// checker reaches the same conclusion by a different road — there `[]` reports
/// `Array<T>`, and a parameter bound to itself is dropped.
///
/// Shared with the direct wasm backend for [`expected_type_args`]'s reason.
pub(crate) fn settles_type_args(e: &Expr) -> bool {
    !matches!(e, Expr::ArrayLit { elems, .. } if elems.is_empty())
        && !matches!(e, Expr::MapLit { entries, .. } if entries.is_empty())
}

pub(crate) fn expected_type_args(
    expected: Option<&Type>,
    name: &str,
    decl: Option<&TypeDecl>,
) -> HashMap<String, Type> {
    let Some(Type::App(en, args)) = expected else {
        return HashMap::new();
    };
    let Some(decl) = decl.filter(|d| en == name && d.type_params.len() == args.len()) else {
        return HashMap::new();
    };
    decl.type_params
        .iter()
        .zip(args)
        .filter(|(_, a)| !matches!(a, Type::Unit | Type::Param(_)))
        .map(|(p, a)| (p.clone(), a.clone()))
        .collect()
}

/// Whether `t` is a generic instantiation whose every type argument is known —
/// no `Unit` placeholder [`applied_type`] put there, no unresolved `Param`.
///
/// **The arm-reconciliation rule** (RFC-0077 M2m). Every arm of a `match` yields
/// the same enum, but they do not all know its type arguments: an arm whose
/// payload mentions the parameter fixes it (`Held(c)` is a `Crate<Cargo>`), and a
/// param-free one cannot (`Empty` is a `Crate<Unit>`). Preferring the applied
/// answer is what lets a downstream `match` recover the concrete payload instead
/// of a bare `Type::Param` — which the textual backend lowers to an invalid
/// `alloca void` and the direct one refuses as "a conversion from `Cargo` to `T`".
///
/// Shared rather than spelled twice, for `solve_type_args`'s reason: two backends
/// preferring different arms would report two different types for one expression,
/// and the enum's LAYOUT is the same either way (`enum_ll` is arity-wide), so the
/// disagreement would surface as a payload encoded one way and read the other.
/// The type a two-arm join carries when one arm may be a `panic` (RFC-0079).
///
/// `Never` names an arm that left through `unreachable` and reached the merge
/// only as a `poison` incoming, so it contributes no type — the other arm
/// answers. Both arms `Never` keeps `Never`, whose `llt` is `void`, and the
/// merge's existing "nothing to `phi`" case takes it from there.
fn join_never(a: Type, b: Type) -> Type {
    if matches!(a, Type::Never) {
        b
    } else {
        a
    }
}

/// What a merge whose LLVM type is `void` hands back to whatever encloses it.
///
/// `Unit` has nothing to hand back and never did. `Never` — every arm diverged —
/// is different in exactly one way that matters: the merge is still in VALUE
/// position, so an enclosing `phi` wants an operand for it. `poison` is that
/// operand, valid at every LLVM type, and it is the same answer a single `panic`
/// arm already gives (RFC-0079 M1). Returning the empty string here instead
/// emitted `phi ptr [ %t12, %a ], [ , %b ]`, which clang rejects — found by
/// `std/strings`'s `substring`, whose `Err` arm is a nested `match` with a `panic`
/// in BOTH of its arms. M1 pinned every join shape with the panic not taken; a
/// join with no surviving arm at all was the shape it did not have.
fn void_merge_value(ty: &Type) -> String {
    if matches!(ty, Type::Never) {
        "poison".to_string()
    } else {
        String::new()
    }
}

pub(crate) fn ty_is_concrete_app(t: &Type, resolve: &dyn Fn(&Type) -> Type) -> bool {
    matches!(t, Type::App(_, args)
        if !args.is_empty()
            && args.iter().all(|a| !matches!(resolve(a), Type::Unit | Type::Param(_))))
}

/// The structural identity of whatever a symbol stands for, as 16 hex
/// characters — the type arguments of an instantiation, a stored-fn signature,
/// the capture/parameter/return shape of a lifted lambda.
///
/// **Why a symbol needs one at all.** [`mangle_ty`] is a READABLE spelling and
/// it is not injective: `Option<Int64>` and a user type named `OptInt64` both
/// mangle to `OptInt64`, every structural record mangles to `Rec`, every
/// `Omit`/`Pick`/`Merge`/`Partial` to `Xf`, and `App`/`Fn` concatenate their
/// arguments with no separator. Each symbol below is BOTH the name of the
/// emitted `define` and the key its worklist dedups on, so a collision skips
/// the second body and points both call sites at the first — `vyrn check`
/// prints `ok`, the interpreter and the wasm backend answer correctly, and
/// native reads one instantiation's value through another's body. (LLVM does
/// not catch it: a `call` carries its own function type, so two calls of
/// different types to one `define` assemble without a diagnostic.)
///
/// Appending this rather than escaping the readable form: escaping cannot help
/// `Record`/`Enum`/`Xf` at all — those collapse a whole field list to three
/// characters, and spelling them out injectively is how a symbol becomes 400
/// characters long. A hash of the WHOLE identity reduces injectivity to one
/// claim about SHA-256, covers every variant at once, and leaves the readable
/// prefix byte-for-byte what it was, which is what `emit-ir` output, crash
/// dumps and linker errors are read for.
///
/// The derived `Debug` form is the structural serialization, it is not a stable
/// ABI, and 64 bits is a birthday bound — the whole argument, and the pin that
/// holds it, live on [`vyrn_frontend::types::struct_key`], which is now the ONE
/// definition. The JSON codec's synthesized encoder and decoder names took this
/// same decision in a second crate, byte-identically and twice more; they read
/// that function too.
fn struct_key(x: &impl std::fmt::Debug) -> String {
    vyrn_frontend::types::struct_key(x)
}

/// The mangled LLVM symbol for a generic instantiation, e.g.
/// `vyrn_id__Int_h4d1f…`: the readable mangle of the type arguments, then their
/// structural identity (see [`struct_key`], which is where the second half is
/// argued).
fn mangle_name(name: &str, type_args: &[Type]) -> String {
    let parts: Vec<String> = type_args.iter().map(mangle_ty).collect();
    format!(
        "{}__{}_h{}",
        fn_sym(name),
        parts.join("_"),
        struct_key(&type_args)
    )
}

/// The release function for a `Stream<elem>` (RFC-0090 M3). One spelling rather
/// than two: the site that names it and the site that defines it were separate
/// `format!`s of the same string, which is a divergence waiting for a mangle
/// change — and this one is a mangle change.
fn stream_close_sym(elem: &Type) -> String {
    format!(
        "__vyrn_stream_close_{}_h{}",
        mangle_ty(elem),
        struct_key(elem)
    )
}

/// The dispatcher symbol for a stored-fn signature (RFC-0037).
fn mangle_dispatch_sym(sig: &Type) -> String {
    format!(
        "__vyrn_fndispatch_{}_h{}",
        sanitize(&mangle_ty(sig)),
        struct_key(sig)
    )
}

fn mangle_ty(t: &Type) -> String {
    match t {
        Type::Int => "Int64".into(),
        Type::IntN { bits, signed } => format!("{}Int{bits}", if *signed { "" } else { "U" }),
        Type::Float => "Float64".into(),
        Type::Float32 => "Float32".into(),
        Type::Bool => "Bool".into(),
        Type::Str => "Str".into(),
        Type::Unit => "Unit".into(),
        Type::Named(n) => sanitize(n),
        Type::Option(inner) => format!("Opt{}", mangle_ty(inner)),
        Type::Result(a, b) => format!("Res{}{}", mangle_ty(a), mangle_ty(b)),
        Type::Record(_) => "Rec".into(),
        Type::Enum(_) => "Enum".into(),
        Type::App(n, args) => {
            format!(
                "{}{}",
                sanitize(n),
                args.iter().map(mangle_ty).collect::<String>()
            )
        }
        Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..) => "Xf".into(),
        Type::Param(p) => sanitize(p),
        Type::Array(inner) => format!("Arr{}", mangle_ty(inner)),
        // Distinct from `Arr` even though the layout is identical: a generic
        // instantiated at `Stream<T>` must not share a symbol with one at
        // `Array<T>`, or the two bodies would be the same code under one name.
        Type::Stream(inner) => format!("Strm{}", mangle_ty(inner)),
        Type::ArrayN(inner, n) => format!("Arr{n}{}", mangle_ty(inner)),
        // RFC-0056: key on BOTH the element type and the inline capacity, so
        // `SmallArray<Int64, 4>` and `SmallArray<Int64, 8>` are distinct
        // monomorphizations.
        Type::SmallArray(inner, n) => format!("SmArr{n}{}", mangle_ty(inner)),
        Type::ConstInt(n) => format!("N{n}"),
        Type::Map(k, v) => format!("Map{}{}", mangle_ty(k), mangle_ty(v)),
        Type::Task(inner) => format!("Task{}", mangle_ty(inner)),
        Type::Logger => "Logger".into(),
        Type::F32x4 => "F32x4".into(),
        Type::I32x4 => "I32x4".into(),
        Type::Mask32x4 => "Mask32x4".into(),
        Type::F64x2 => "F64x2".into(),
        Type::Mask64x2 => "Mask64x2".into(),
        // A function-value type (RFC-0023) mangles by shape — used only when a
        // generic instance's own type argument mentions one (rare); the
        // higher-order specialization keys are formed separately.
        Type::Fn(ps, r) => format!(
            "Fn{}R{}",
            ps.iter().map(mangle_ty).collect::<String>(),
            mangle_ty(r)
        ),
        // A `lazy T` field (RFC-0085 M4a) mangles as what it IS — the nullary
        // closure — so a generic instantiated at one cannot key differently
        // from an instantiation at its representation.
        Type::Lazy(inner) => format!("FnR{}", mangle_ty(inner)),
        // Neither is a type a monomorphization can be keyed on: `Err` is the
        // checker's recovery sentinel and never reaches codegen, and `Never`
        // (RFC-0079) is unspellable in a signature, so no type argument is one.
        Type::Never => "Never".into(),
        Type::Err => "Err".into(),
    }
}

/// The declaration whose `where` predicate a value flowing from `from` into `to`
/// must satisfy, if any (RFC-0003's automatic validation).
///
/// The ONE copy of that decision, for the reason [`llt_of`] is the one copy of
/// the shape rules: RFC-0077's direct wasm backend asks the same question, and a
/// second spelling of it would fork the semantics silently — a flow one backend
/// checks and the other does not is a wrong program on exactly one target.
///
/// The exactly-same named type is not a boundary crossing: it was checked when
/// it was built, so re-running the predicate would be work that cannot fail.
///
/// One exemption is deliberately NOT here, because it needs the expression
/// rather than the two types: [`vyrn_frontend::finite::string_flow_proven`],
/// RFC-0020's containment proof. Both backends call that function themselves on
/// the same AST — the consteval precedent — so it is single-sourced too, just
/// one layer out.
pub(crate) fn validation_required<'t>(
    from: &Type,
    to: &Type,
    types: &'t HashMap<String, TypeDecl>,
) -> Option<&'t TypeDecl> {
    let Type::Named(n) = to else { return None };
    if from == to {
        return None;
    }
    types.get(n).filter(|d| d.predicate.is_some())
}

/// One rung of the boundary ladder — RFC-0101 §1.5's shadow.
///
/// A `coerce` is where a value crosses into a declared type, and each engine
/// writes the decision as a ladder of guarded rungs: 146 lines in the textual
/// emitter, 198 in the direct one, and §1.5 measured them as ONE decision until
/// M6's second phase read them against each other and found two — the same first
/// two rungs, differently ordered middles, one rung each the other lacks, and
/// opposite ends. This is the vocabulary that lets a gate say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// A `Never` (RFC-0079) reaching a boundary: the `panic` already left, so
    /// there is no value to reconcile.
    Never,
    /// A refined named type: coerce into the base, then run its `where`
    /// predicate. The base crossing is a rung of its own, at its own pair.
    Validate,
    /// A function value between `fn`-typed spellings: a re-tag, no instruction.
    FnRetag,
    /// A fixed array whose ELEMENT type changes: element by element, so each
    /// element crosses its own boundary (and validates, if it has one).
    Elementwise,
    /// A fixed array into the growable `{ptr,len,cap}` triple.
    Heapify,
    /// A fixed array into a `SmallArray`'s inline buffer.
    Inline,
    /// An integer resize: truncate, extend, or renormalise.
    Resize,
    /// Across the int/float line, or between the two float widths.
    FloatCross,
    /// A record used as a differently shaped record: rebuilt field by field.
    Rebuild,
    /// The bits are already right.
    Identity,
    /// No rung handles this pair.
    Refuse,
}

/// The rung a value crossing from `from` into `to` takes — the PLAN both
/// compiled backends are held to (RFC-0101 §1.5).
///
/// **Two engines, not three** (§2.4 row 4): the interpreter's `coerce` takes a
/// value and a target and has no `from` at all, so it cannot be held to a plan
/// keyed on a pair until it runs the form.
///
/// **The order is this function's, and it is where the rules come from.** The
/// two ladders agree on the first two rungs and order their middles differently;
/// a middle rung's guard is disjoint from the others', so the order is
/// observable at exactly one place — an integer pair whose two spellings share
/// one LLVM shape (`i8` for `Int8` and `UInt8` alike), which is why the resize
/// comes before [`Rung::Identity`] here and in the direct backend. Every
/// remaining difference is one engine taking a rung the other does not have; the
/// corpus gate names each one as a rule rather than hiding it in a plan that
/// splits the difference.
///
/// It is beside [`validation_required`] and [`llt_of`] rather than in
/// `vyrn-lower` because it is made OF them: this is where the two shared
/// codegen decisions already live, and a plan keyed on a pair needs no site.
pub fn coerce_plan(from: &Type, to: &Type, types: &HashMap<String, TypeDecl>) -> Rung {
    let (rf, rt) = (
        vyrn_frontend::types::resolve(from, types),
        vyrn_frontend::types::resolve(to, types),
    );
    // `Int` and `Int64` are one type, and `IntN` carries its own width and
    // signedness — so "the same integer" is a comparison of resolved spellings,
    // and a pair that IS the same integer needs no rung at all.
    let num = |t: &Type| matches!(t, Type::Int | Type::IntN { .. });
    let flt = |t: &Type| matches!(t, Type::Float | Type::Float32);
    if matches!(from, Type::Never) {
        return Rung::Never;
    }
    if validation_required(from, to, types).is_some() {
        return Rung::Validate;
    }
    if num(&rf) && num(&rt) && rf != rt {
        return Rung::Resize;
    }
    if (flt(&rf) || flt(&rt)) && (num(&rf) || num(&rt) || flt(&rf) && flt(&rt)) && rf != rt {
        return Rung::FloatCross;
    }
    if matches!(rf, Type::Fn(..)) && matches!(rt, Type::Fn(..)) {
        return Rung::FnRetag;
    }
    match (&rf, &rt) {
        (Type::ArrayN(fi, fnn), Type::ArrayN(ti, tn)) if fi != ti && fnn == tn => {
            return Rung::Elementwise
        }
        (Type::ArrayN(fi, _), Type::Array(ti))
            if fi == ti || llt_of(fi, types) == llt_of(ti, types) =>
        {
            return Rung::Heapify
        }
        (Type::ArrayN(fi, len), Type::SmallArray(ti, n))
            if llt_of(fi, types) == llt_of(ti, types) && len <= n =>
        {
            return Rung::Inline
        }
        _ => {}
    }
    if llt_of(from, types) == llt_of(to, types) {
        return Rung::Identity;
    }
    match (
        vyrn_frontend::types::record_fields(&rf, types),
        vyrn_frontend::types::record_fields(&rt, types),
    ) {
        (Some(_), Some(_)) => Rung::Rebuild,
        _ => Rung::Refuse,
    }
}

/// The message a `where` violation prints. A record base gets the cross-field
/// wording, because what violated it is not one value.
///
/// Byte-identical across interp, native and wasm — parity compares stderr — so
/// it is built here rather than spelled at each of the places that trap.
pub(crate) fn validation_message(decl: &TypeDecl) -> String {
    vyrn_frontend::trap::line(&vyrn_frontend::trap::validation_of(decl))
}

/// What a `where` predicate has in scope, re-exported from the frontend.
///
/// It moved there in RFC-0078 M3: the JSON decode path now synthesizes a
/// `Bool`-returning Vyrn function whose PARAMETERS are this structure, and the
/// frontend is the only crate both it and the two lowerings can see. This file had
/// three copies of the walk before RFC-0077 M2d wanted a fourth; it has none now.
pub(crate) use vyrn_frontend::types::predicate_binds;

/// The LLVM shape of a Vyrn type: the ONE match that turns a type into a memory
/// layout, so `Gen::llt` and RFC-0077's direct wasm backend cannot come to
/// different conclusions about the same value. `layout::of_ll` parses what this
/// prints, which is what keeps size and offset arithmetic downstream of lowering
/// rather than beside it.
///
/// `ty` is resolved here but NOT substituted — a caller inside a monomorphized
/// body substitutes first (`Gen::llt` does), because only it knows which
/// instantiation it is in.
pub(crate) fn llt_of(ty: &Type, types: &HashMap<String, TypeDecl>) -> String {
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Int => "i64".into(),
        Type::IntN { bits, .. } => format!("i{bits}"),
        Type::Float => "double".into(),
        Type::Float32 => "float".into(),
        // RFC-0083: LLVM's own vector type, so `fadd <4 x float>` is one
        // instruction and the register allocator puts it in an xmm. The direct
        // wasm backend reads this same spelling back out to reach `v128`, which is
        // why the vector lives here rather than in a table of its own.
        Type::F32x4 => "<4 x float>".into(),
        // M3's integer width, sharing the mask's spelling: `<4 x i32>` is what a
        // v128 of four 32-bit lanes IS on both backends, and the two are different
        // Vyrn TYPES rather than different representations — which is exactly why
        // an `I32x4` comparison can produce a `Mask32x4` without a conversion.
        Type::I32x4 => "<4 x i32>".into(),
        // A mask is `<4 x i32>` of all-ones/all-zeros, not the `<4 x i1>` an
        // `fcmp` actually produces. `<4 x i1>` is a legal IR type but a strange
        // ABI one — it is passed as a packed `i4` in places — and a mask crosses
        // function boundaries here like any other value. The `sext`/`trunc` pair
        // that costs is folded away by `-O2` at every use, which was checked.
        Type::Mask32x4 => "<4 x i32>".into(),
        // M4's wide float width and its own mask, on the same two rules: the
        // vector is LLVM's own so the arithmetic is one instruction, and the mask
        // is all-ones/all-zeros at the LANE width — `<2 x i64>` and not `<2 x i1>`
        // — so the two backends carry the same bit pattern here as they do at 32.
        Type::F64x2 => "<2 x double>".into(),
        Type::Mask64x2 => "<2 x i64>".into(),
        Type::Bool => "i1".into(),
        Type::Str => "ptr".into(),
        // `Never` (RFC-0079) carries no value, so it lowers like `Unit`: a
        // statement-position `panic` has nothing to drop, and a `void` join is
        // already the "no value to merge" case both merges test for.
        Type::Unit | Type::Never => "void".into(),
        // Option/Result both lower to { tag, payload }; payload is i64.
        // { tag, word0, word1 } — two payload words so a `Ref` (which is two
        // words) fits inline without a heap box.
        Type::Option(_) | Type::Result(..) => "{ i1, i64, i64 }".into(),
        // A growable array is { ptr data, i64 len, i64 cap }.
        Type::Array(_) => "{ ptr, i64, i64 }".into(),
        // A `Stream<T>` (RFC-0075 M2b) is a tagged header over two producers,
        // and this is the line M2 said would change:
        //
        //   { ptr data, i64 len, i64 tag, i64 pay, i64 cur, i64 gen }
        //
        // `tag` is the discriminant. Negative means a BUFFER — `data`/`len` are
        // the array `fromArray` was handed and `cur` is how far the consumer has
        // read. Non-negative means a STEP: `tag`/`pay` ARE an RFC-0037 function
        // value, `cur`/`gen` ARE the `Ref<Int64>` cursor cell it is called with,
        // `len` is 0 until the step answers `None` and 1 after, and `data` is
        // null.
        //
        // The two-word pairs are adjacent and 8-aligned on purpose: `{ i64, i64 }`
        // is exactly what a fn value and a `Ref` each lower to, so `&s + 16` and
        // `&s + 32` ARE those values and neither backend has to reassemble one to
        // make the call.
        //
        // Overlaid rather than a union of the widest variant because the two
        // shapes are 3 and 5 words and a discriminated 6-word header costs less
        // than a heap box plus an indirection on every `next`. Nothing reads a
        // field whose variant it has not tested.
        Type::Stream(_) => "{ ptr, i64, i64, i64, i64, i64 }".into(),
        // A `Map<String, V>` (RFC-0028) is two parallel growable buffers
        // sharing one length/capacity, plus the hash index over them:
        // { ptr keys, ptr values, i64 len, i64 cap, ptr idx }. Keys are `ptr`
        // (String); values are `llt(V)`-stride; `idx` is `cap * 2` buckets of
        // i64, and the shim's `map_hash`/`map_slot` are what read it.
        Type::Map(..) => "{ ptr, ptr, i64, i64, ptr }".into(),
        // A fixed-size array lowers to the LLVM value aggregate [N x T].
        Type::ArrayN(inner, n) => format!("[{n} x {}]", llt_of(&inner, types)),
        // A small-buffer array (RFC-0056) lowers to
        // `{ i64 len, i64 cap, ptr data, [N x T] inline }` — `cap` is the
        // state discriminant (`cap == N` inline; `cap > N` spilled onto
        // `data`). Every element access branches on it to pick the base.
        Type::SmallArray(inner, n) => {
            format!("{{ i64, i64, ptr, [{n} x {}] }}", llt_of(&inner, types))
        }
        // A task handle (RFC-0025) is an opaque `ptr` to the shim's task
        // record (thread handle + heap frame); `t.join()` blocks on it and
        // loads the result from the frame's leading slot.
        Type::Task(_) => "ptr".into(),
        // A logger handle is a `ptr` to its name string.
        Type::Logger => "ptr".into(),
        Type::Record(fields) => {
            let inner: Vec<String> = fields.iter().map(|f| llt_of(&f.ty, types)).collect();
            format!("{{ {} }}", inner.join(", "))
        }
        // A user enum is { i64 tag, i64 payload0, ... } — one payload slot per
        // the widest variant (payloads are i64 in native).
        Type::Enum(vs) => {
            let arity = vs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
            enum_ll(arity)
        }
        // RFC-0076 M3a: on the generator-host path `Code` (RFC-0054) is an
        // opaque i64 HANDLE into the host's piece arena — the one `Named`
        // that survives `resolve` undeclared. i64 is also what makes it
        // travel for free: `box_payload` passes an i64 through, so a `Code`
        // in an Option/Array needs no case of its own.
        Type::Named(ref n) if n == "Code" && gen_host() => "i64".into(),
        // Unreachable after `resolve` (Named/App/transformers/params reduced away).
        Type::Named(_)
        | Type::App(..)
        | Type::Omit(..)
        | Type::Pick(..)
        | Type::Merge(..)
        | Type::Partial(..)
        | Type::Param(_) => "void".into(),
        // A bare integer type argument (RFC-0056) never stands alone as a
        // runtime type — `SmallArray` consumes it before lowering.
        Type::ConstInt(_) => "void".into(),
        // A stored function value (RFC-0037) is a synthesized closed enum:
        // `{ i64 tag, i64 payload }` — tag selects the source (one variant
        // per named function / lifted lambda), payload is 0 or a pointer to
        // the malloc'd capture block. v1 `fn`-typed PARAMETERS never reach
        // `llt` (they monomorphize away before lowering).
        Type::Fn(..) => "{ i64, i64 }".into(),
        // Unreachable: `resolve` (above) answers `Fn([], T)` for a `lazy T`
        // field, which is exactly the point — the deferral has no layout of its
        // own (RFC-0085 M4a).
        Type::Lazy(_) => "{ i64, i64 }".into(),
        // `Err` is the checker's recovery sentinel; a program with any `Err`
        // already has diagnostics and never reaches codegen. Lower to void
        // as a defensive fallback (never observed in practice).
        Type::Err => "void".into(),
    }
}

/// The LLVM aggregate type for an enum with `arity` payload slots:
/// `{ i64 }` (tag only) for arity 0, `{ i64, i64 }` for arity 1, and so on.
fn enum_ll(arity: usize) -> String {
    let mut s = String::from("{ i64");
    for _ in 0..arity {
        s.push_str(", i64");
    }
    s.push_str(" }");
    s
}

/// Make an identifier safe to embed in an LLVM local name.
///
/// Unquoted LLVM identifiers are ASCII-only (`[A-Za-z$._0-9]`), but the lexer
/// accepts Unicode alphabetic identifiers — so a non-ASCII letter or digit is
/// escaped as `_uXXXX_` (its code point in hex) instead of passing through and
/// producing IR that clang/llc reject with a parse error. Other characters
/// collapse to `_`, as before.
fn sanitize(name: &str) -> String {
    // `$` passes through: the runtime-module reserved spellings (`json$emit`,
    // `num$f64Str`) are built on it — the loader's own defence, and a character
    // every object format here (LLVM IR, wasm) accepts in a symbol. Everything
    // non-ASCII still escapes; the pre-fix hazard was `fn héllo`, not `$`.
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            out.push(c);
        } else if c.is_alphanumeric() {
            out.push_str(&format!("_u{:04X}_", c as u32));
        } else {
            out.push('_');
        }
    }
    out
}

/// The LLVM symbol for a top-level Vyrn function: `vyrn_<name>`, sanitized.
///
/// One spelling rather than seven: the `define` and every call site, fnval
/// variant and mangle that names a top-level function go through here, so a
/// non-ASCII identifier escapes identically on both sides of a call and no
/// raw `format!("vyrn_{name}")` can drift back in. See [`sanitize`].
fn fn_sym(name: &str) -> String {
    format!("vyrn_{}", sanitize(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::check;

    // ---- the layout engine's link to lowering (RFC-0077 M0) -------------

    /// [`layout::SHAPES`] is what the clang comparison is run against, so it is
    /// only worth anything if it is what `llt` prints. Assert that here — in the
    /// one place `Gen` is reachable — so a new case in `llt` cannot quietly
    /// escape the layout check that stands between it and a silent miscompile.
    #[test]
    fn llt_prints_the_shapes_the_layout_engine_was_verified_on() {
        let (rt, pt, pc, ty, va, sg, sb, fs, dm, hm, rg) = Default::default();
        let ow = vyrn_frontend::own::Owned::default();
        let pl = vyrn_frontend::own::ReleasePlan::default();
        let g = Gen::new(
            &rt,
            &pt,
            &pc,
            &ty,
            &va,
            &sg,
            &sb,
            &fs,
            &dm,
            &hm,
            &ow,
            &rg,
            &[],
            &pl,
            &[],
        );
        let rec = |fs: &[Type]| {
            Type::Record(
                fs.iter()
                    .enumerate()
                    .map(|(i, t)| Field {
                        name: format!("f{i}"),
                        ty: t.clone(),
                    })
                    .collect(),
            )
        };
        let i8t = Type::IntN {
            bits: 8,
            signed: false,
        };
        let cases: &[(&str, Type)] = &[
            ("Int64", Type::Int),
            (
                "Int32",
                Type::IntN {
                    bits: 32,
                    signed: true,
                },
            ),
            (
                "Int16",
                Type::IntN {
                    bits: 16,
                    signed: true,
                },
            ),
            ("Int8", i8t.clone()),
            ("Bool", Type::Bool),
            ("Float64", Type::Float),
            ("Float32", Type::Float32),
            ("String", Type::Str),
            ("Option/Result", Type::Option(Box::new(Type::Int))),
            (
                "Option/Result",
                Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
            ),
            ("Array", Type::Array(Box::new(Type::Str))),
            ("Map", Type::Map(Box::new(Type::Str), Box::new(Type::Int))),
            ("Fn", Type::Fn(Vec::new(), Box::new(Type::Int))),
            ("RecordEmpty", rec(&[])),
            (
                "RecordMixed",
                rec(&[Type::Bool, Type::Str, Type::Int, i8t.clone(), Type::Float]),
            ),
            ("ArrayN_i64", Type::ArrayN(Box::new(Type::Int), 4)),
            ("ArrayN_i8", Type::ArrayN(Box::new(i8t.clone()), 3)),
            ("SmallArray_i64", Type::SmallArray(Box::new(Type::Int), 4)),
            ("SmallArray_i8", Type::SmallArray(Box::new(i8t), 3)),
            ("SmallArray_str", Type::SmallArray(Box::new(Type::Str), 2)),
        ];
        for (name, ty) in cases {
            let want = layout::SHAPES
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} missing from layout::SHAPES"))
                .1;
            assert_eq!(&g.llt(ty), want, "llt({name}) drifted from layout::SHAPES");
        }
        // The enum arities, which come from the same helper `llt` calls.
        for (name, arity) in [("Enum0", 0), ("Enum1", 1), ("Enum3", 3)] {
            let want = layout::SHAPES.iter().find(|(n, _)| *n == name).unwrap().1;
            assert_eq!(enum_ll(arity), want, "enum_ll({arity}) drifted");
        }
    }

    /// One `Type` per variant of the type enum, held complete by a match the
    /// compiler will not let go stale.
    ///
    /// The match computes nothing. Its only job is to fail to compile when a
    /// variant is added, which is what makes the list below a list rather than a
    /// guess — the hand-written `cases` above is exactly what happens without
    /// one, and it had been missing `Stream` and every vector for as long as
    /// they had existed. Both halves live on the type itself
    /// ([`Type::VARIANTS`] / [`Type::variant_name`]), because `vyrn-frontend`'s
    /// wire-form coverage test asks the same question of the same list.
    fn layout_seeds() -> Vec<Type> {
        let b = |t: Type| Box::new(t);
        vec![
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: false,
            },
            Type::IntN {
                bits: 16,
                signed: true,
            },
            Type::IntN {
                bits: 32,
                signed: true,
            },
            Type::Float,
            Type::Float32,
            Type::F32x4,
            Type::I32x4,
            Type::F64x2,
            Type::Mask32x4,
            Type::Mask64x2,
            Type::Bool,
            Type::Str,
            Type::Unit,
            Type::Named("Nowhere".into()),
            Type::Option(b(Type::Int)),
            Type::Result(b(Type::Int), b(Type::Str)),
            Type::Record(Vec::new()),
            Type::Omit(b(Type::Record(Vec::new())), vec!["f".into()]),
            Type::Pick(b(Type::Record(Vec::new())), vec!["f".into()]),
            Type::Merge(b(Type::Record(Vec::new())), b(Type::Record(Vec::new()))),
            Type::Partial(b(Type::Record(Vec::new()))),
            Type::Enum(Vec::new()),
            Type::Param("T".into()),
            Type::App("Box".into(), vec![Type::Int]),
            Type::Array(b(Type::Int)),
            Type::ArrayN(b(Type::Int), 4),
            Type::SmallArray(b(Type::Int), 3),
            Type::ConstInt(8),
            Type::Map(b(Type::Str), b(Type::Int)),
            Type::Stream(b(Type::Int)),
            Type::Task(b(Type::Int)),
            Type::Logger,
            Type::Fn(vec![Type::Int], b(Type::Unit)),
            Type::Lazy(b(Type::Int)),
            Type::Never,
            Type::Err,
        ]
    }

    /// The wrappers [`grow`] has none of, because the mangle it was written for
    /// collapses a record whole and layout does not.
    ///
    /// Three per type: a one-field record, which shows the member's own
    /// alignment; an `i8`-then-`t` record, which shows the HOLE in front of it,
    /// where a wrong alignment becomes a wrong offset; and a one-payload enum.
    fn in_records(ts: &[Type]) -> Vec<Type> {
        let field = |n: &str, x: &Type| Field {
            name: n.into(),
            ty: x.clone(),
        };
        let byte = Type::IntN {
            bits: 8,
            signed: true,
        };
        ts.iter()
            .flat_map(|t| {
                [
                    Type::Record(vec![field("a", t)]),
                    Type::Record(vec![field("n", &byte), field("a", t)]),
                    Type::Enum(vec![EnumVariant {
                        name: "V".into(),
                        payload: vec![t.clone()],
                    }]),
                ]
            })
            .collect()
    }

    /// The leaf spellings in an `llt` string: each scalar word, and each whole
    /// `<N x T>` vector. The `{ }`, `[N x` and `,` scaffolding is structure, not
    /// a leaf — and a vector is a leaf rather than structure because it is the
    /// unit `of_ll` has to know a size for.
    fn atoms(ll: &str, out: &mut std::collections::BTreeSet<String>) {
        fn words(s: &str, out: &mut std::collections::BTreeSet<String>) {
            for w in s.split(|c: char| !c.is_ascii_alphanumeric()) {
                // Skip the counts and the `x` that separates them from the
                // element: both are grammar, and neither is a shape.
                if !w.is_empty() && w != "x" && !w.bytes().all(|c| c.is_ascii_digit()) {
                    out.insert(w.to_string());
                }
            }
        }
        let mut rest = ll;
        while let Some(a) = rest.find('<') {
            let b = a + rest[a..].find('>').expect("a vector spelling closes");
            words(&rest[..a], out);
            out.insert(rest[a..=b].to_string());
            rest = &rest[b + 1..];
        }
        words(rest, out);
    }

    /// The guard the hand-written list above cannot be.
    ///
    /// [`layout::SHAPES`] claims to be the emitter's whole type universe, and
    /// the check next to it walks a list a human typed — so `Stream` and `Ref`
    /// sat in `SHAPES` with no case in the test, and RFC-0083's four vector
    /// spellings were printed by `llt`, refused by `of_ll`, and never once
    /// compared against clang. A list cannot guard a list.
    ///
    /// So derive the cases, on PR #165's generator itself. [`layout_seeds`] is
    /// one `Type` per variant of the type enum, held complete by
    /// [`Type::variant_name`] — an exhaustive match, so a new variant is a COMPILE
    /// error here before it is a missing layout in front of a user. [`grow`],
    /// which the mangle-injectivity test already owns, composes those through
    /// every container constructor twice, and [`in_records`] adds the two
    /// wrappers a mangle does not care about and a layout does. That is a few
    /// thousand type trees, and two things hold over all of them:
    ///
    /// 1. every one has a layout — a shape `llt` can print and `of_ll` cannot
    ///    parse is this test failing, not a build dying at the user;
    /// 2. every leaf spelling that appears also appears in `SHAPES`, so a new
    ///    case in `llt` cannot escape the clang comparison.
    ///
    /// # What it cannot derive
    ///
    /// The reverse direction is leaf-wise — no DEAD spelling in `SHAPES` —
    /// rather than "every row is generated", and that is a real limit rather
    /// than an oversight. Most rows are hand-built PADDING probes:
    /// `RecordNested`, `SmallArray_i8`, `RecordOfVector` are chosen because
    /// clang and the engine could plausibly disagree about them, and no
    /// enumeration of the type enum produces those exact trees. Nor are the
    /// NAMES derivable: `Ref` is `{ i64, i64 }`, the same string a stored `fn`
    /// prints, and nothing in the type enum spells a `Ref` at all.
    #[test]
    fn llt_prints_every_shape_the_layout_engine_was_verified_on() {
        let types = HashMap::new();
        let seeds = layout_seeds();
        // The lock, in two halves. `variant_name`'s match is exhaustive, so a
        // new variant of the type enum stops this file compiling; `VARIANTS` is
        // the same list as a value, so a variant that is named but never seeded
        // stops the test passing. Neither half alone is a guard.
        let seeded: std::collections::BTreeSet<&str> =
            seeds.iter().map(|t| t.variant_name()).collect();
        for v in Type::VARIANTS {
            assert!(seeded.contains(v), "no seed for Type::{v}");
        }
        assert!(
            seeded.iter().all(|s| Type::VARIANTS.contains(s)),
            "a seed names a variant Type::VARIANTS does not list"
        );
        let d1 = grow(&seeds, &seeds[..8]);
        let d2 = grow(&d1[..200], &d1[..20]);
        let all: Vec<Type> = seeds
            .iter()
            .chain(d1.iter())
            .chain(d2.iter())
            .chain(in_records(&seeds).iter())
            .chain(in_records(&d1[..60]).iter())
            .cloned()
            .collect();

        let mut printed = std::collections::BTreeSet::new();
        for ty in &all {
            let ll = llt_of(ty, &types);
            let l = layout::of_ll(&ll)
                .unwrap_or_else(|e| panic!("llt({ty}) = {ll}, which has no layout: {e}"));
            assert!(l.align.is_power_of_two(), "{ll}: align {}", l.align);
            assert_eq!(l.size % l.align, 0, "{ll}: size {} is not padded", l.size);
            atoms(&ll, &mut printed);
        }

        let mut covered = std::collections::BTreeSet::new();
        for (_, ll) in layout::SHAPES {
            atoms(ll, &mut covered);
        }
        // `void` is the one leaf that cannot join them, and the reason is C's,
        // not this crate's: `sizeof(void)` is a GNU extension answering 1 where
        // the engine answers 0, so a `void` row would make the clang comparison
        // disagree about a shape that has no bytes and never occupies any. It is
        // what `llt` prints for `Unit`, `Never`, and every type that resolved
        // away — none of which can be a member of anything.
        printed.remove("void");
        covered.remove("void");
        let missing: Vec<_> = printed.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "{} type trees print {missing:?}, which layout::SHAPES does not cover — \
             so clang is never asked about it",
            all.len()
        );
        let dead: Vec<_> = covered.difference(&printed).collect();
        assert!(
            dead.is_empty(),
            "layout::SHAPES spells {dead:?}, which `llt` no longer prints"
        );
        assert!(all.len() > 4_000, "the corpus shrank to {}", all.len());
    }

    // ---- RFC-0086: the shapes `solve_param` cannot descend into ---------

    /// Every `.vyrn` file under `rel`, relative to the repository root.
    fn corpus(rel: &str, out: &mut Vec<std::path::PathBuf>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "vyrn") {
                    out.push(p);
                }
            }
        }
    }

    /// The constructors [`solve_param`] descends through. A [`Type::Param`]
    /// reached by any other route is a parameter the solver cannot bind.
    fn unreachable_params(t: &Type, under: Option<&'static str>, out: &mut Vec<&'static str>) {
        let go = unreachable_params;
        match t {
            // The root arm: a bare `Param` binds whatever it faces.
            Type::Param(_) => {
                if let Some(k) = under {
                    out.push(k);
                }
            }
            Type::Option(a)
            | Type::Array(a)
            | Type::ArrayN(a, _)
            | Type::SmallArray(a, _)
            | Type::Stream(a) => go(a, under, out),
            Type::Result(a, b) | Type::Map(a, b) => {
                go(a, under, out);
                go(b, under, out);
            }
            Type::App(_, args) => {
                for a in args {
                    go(a, under, out);
                }
            }
            Type::Fn(ps, r) => {
                for p in ps {
                    go(p, under, out);
                }
                go(r, under, out);
            }
            // Everything below carries a type and has no arm.
            Type::Record(fs) => {
                for f in fs {
                    go(&f.ty, under.or(Some("Record")), out);
                }
            }
            Type::Enum(vs) => {
                for v in vs {
                    for p in &v.payload {
                        go(p, under.or(Some("Enum")), out);
                    }
                }
            }
            Type::Lazy(a) => go(a, under.or(Some("Lazy")), out),
            Type::Task(a) => go(a, under.or(Some("Task")), out),
            Type::Partial(a) | Type::Omit(a, _) | Type::Pick(a, _) => {
                go(a, under.or(Some("Omit/Pick/Partial")), out)
            }
            Type::Merge(a, b) => {
                go(a, under.or(Some("Merge")), out);
                go(b, under.or(Some("Merge")), out);
            }
            _ => {}
        }
    }

    /// RFC-0086's last open list: how many places in the corpus hand
    /// [`solve_param`] a declared type whose type parameter sits under a
    /// constructor the match has no arm for.
    ///
    /// It counts the four positions the solver is actually called from — a
    /// generic function's parameters and return, a generic record declaration's
    /// fields, a generic enum's variant payloads, and a generic impl head. Each
    /// of those types is a *root* the solver receives, so a `Param` directly at
    /// the root is fine; only one buried under an unhandled constructor is a
    /// site the solver walks past.
    ///
    /// It parses each file ALONE — no loader, no linking — like the RFC-0089
    /// corpus measurements. A declared type is written where it is declared, so
    /// linking would add no site this misses.
    ///
    /// Ignored by default: it reads the repository, so it is a measurement, not
    /// a unit test. Run it with
    /// `cargo test -p vyrn-codegen --lib rfc0086 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rfc0086_unsolvable_parameter_positions_over_the_corpus() {
        let mut files = Vec::new();
        corpus("examples", &mut files);
        corpus("std", &mut files);
        files.sort();

        let mut by_kind: std::collections::BTreeMap<&'static str, usize> = Default::default();
        let mut rows: Vec<String> = Vec::new();
        let (mut parsed, mut roots) = (0, 0);
        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(tokens) = vyrn_frontend::lexer::lex(&src) else {
                continue;
            };
            let (program, errs) = vyrn_frontend::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // (what it is, the generic's name, the line, the root types)
            let mut sites: Vec<(&str, String, usize, Vec<Type>)> = Vec::new();
            for f in &program.functions {
                if f.type_params.is_empty() {
                    continue;
                }
                let mut ts: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
                ts.push(f.ret.clone());
                sites.push(("fn", f.name.clone(), f.line, ts));
            }
            for d in &program.type_decls {
                if d.type_params.is_empty() {
                    continue;
                }
                let ts = match &d.base {
                    Type::Record(fs) => fs.iter().map(|f| f.ty.clone()).collect(),
                    Type::Enum(vs) => vs.iter().flat_map(|v| v.payload.clone()).collect(),
                    other => vec![other.clone()],
                };
                sites.push(("type", d.name.clone(), d.line, ts));
            }
            for i in &program.impls {
                if i.type_params.is_empty() {
                    continue;
                }
                sites.push(("impl", i.protocol.clone(), i.line, vec![i.ty.clone()]));
            }
            for (kind, who, line, ts) in sites {
                for t in &ts {
                    roots += 1;
                    let mut hits = Vec::new();
                    unreachable_params(t, None, &mut hits);
                    for h in hits {
                        *by_kind.entry(h).or_default() += 1;
                        rows.push(format!("{name}:{line} {kind} `{who}` — {t} under {h}"));
                    }
                }
            }
        }

        println!(
            "corpus: {} files ({parsed} parsed), {roots} declared root types",
            files.len()
        );
        println!("parameters `solve_param` cannot reach: {}", rows.len());
        for (k, c) in &by_kind {
            println!("  {k:>18}: {c}");
        }
        for r in &rows {
            println!("    {r}");
        }
    }

    /// The four arms the census found no victim for, exercised directly.
    ///
    /// They are dead behind the checker today (see the test below), so nothing
    /// else would notice them being wrong. This is what says they are right.
    #[test]
    fn the_filled_arms_bind_a_parameter_the_fall_through_walked_past() {
        let t = || Type::Param("T".into());
        let fld = |n: &str, ty: Type| Field { name: n.into(), ty };
        let var = |n: &str, payload: Vec<Type>| EnumVariant {
            name: n.into(),
            payload,
        };
        let solved = |p: Type, a: Type| {
            let mut s = HashMap::new();
            solve_param(&p, &a, &mut s);
            s.get("T").cloned()
        };

        // A record matches by field NAME, and a wider argument is still a match.
        assert_eq!(
            solved(
                Type::Record(vec![fld("v", t())]),
                Type::Record(vec![fld("other", Type::Bool), fld("v", Type::Str)]),
            ),
            Some(Type::Str),
        );
        // An enum matches by variant NAME, then payload-wise.
        assert_eq!(
            solved(
                Type::Enum(vec![var("Empty", vec![]), var("W", vec![t()])]),
                Type::Enum(vec![var("W", vec![Type::Int]), var("Empty", vec![])]),
            ),
            Some(Type::Int),
        );
        // `lazy T` in either spelling — RFC-0085 M4a says they are one type.
        assert_eq!(
            solved(Type::Lazy(Box::new(t())), Type::Lazy(Box::new(Type::Float))),
            Some(Type::Float),
        );
        assert_eq!(
            solved(
                Type::Lazy(Box::new(t())),
                Type::Fn(vec![], Box::new(Type::Float))
            ),
            Some(Type::Float),
        );
        assert_eq!(
            solved(Type::Task(Box::new(t())), Type::Task(Box::new(Type::Bool))),
            Some(Type::Bool),
        );
        // A different constructor still binds nothing — the rule's second half.
        assert_eq!(solved(Type::Task(Box::new(t())), Type::Int), None);
    }

    /// **Why the arms above are dead, and why filling them changed no program.**
    ///
    /// RFC-0086 deferred this list because "filling in `Lazy`/`Record`/`Enum`/
    /// `Task` turns some silent `Unit` into a real type and some refusal into a
    /// compile". Neither happens, and the reason is that the CHECKER refuses all
    /// four shapes before codegen is asked: `Checker::unify` has the same list,
    /// and its fall-through is a diagnostic rather than a substitution. So
    /// `solve_param` never faced one, the corpus census counts zero, and no
    /// program's meaning moved.
    ///
    /// If any of these ever starts checking, this test fails and says so, and
    /// the arms it unblocks are already written and already tested.
    #[test]
    fn the_checker_refuses_every_shape_the_fall_through_used_to_swallow() {
        let cases: &[(&str, &str)] = &[
            // A structural record parameter naming the type parameter.
            (
                "Record",
                "type Box<T> = { value: T }\n\
                 fn unwrap<T>(b: { value: T }) -> T { return b.value }\n\
                 fn main() -> Int64 { let n = Box { value: 7 }\n return unwrap(n) }",
            ),
            // An enum variant whose payload is a record naming the parameter.
            (
                "Enum",
                "type Cell = { v: Int64 }\n\
                 type Wrap<T> = | Empty | W({ v: T })\n\
                 fn main() -> Int64 { let c = Cell { v: 7 }\n let w = W(c)\n return 0 }",
            ),
            // A generic record with a `lazy` field.
            (
                "Lazy",
                "type Holder<T> = { body: lazy T }\n\
                 fn seven() -> Int64 { return 7 }\n\
                 fn main() -> Int64 { let h: Holder<Int64> = Holder { body: () -> seven() }\n\
                 return h.body }",
            ),
            // A `Task<T>` parameter.
            (
                "Task",
                "fn slow(n: Int64) -> Int64 { return n * 2 }\n\
                 fn await<T>(t: Task<T>) -> T { return t.join() }\n\
                 fn main() -> Int64 { let t = spawn slow(21)\n return await(t) }",
            ),
        ];
        for (what, src) in cases {
            assert!(
                check(src).is_err(),
                "{what}: the checker accepted this, so `solve_param` now faces it — \
                 see the arms above and RFC-0086's last open list"
            );
        }
    }

    // ---- blackBox / benchmarking barrier (RFC-0055) ---------------------

    #[test]
    fn black_box_lowers_to_an_optimizer_barrier_that_survives_in_ir() {
        // `blackBox` is checker-gated to bench/test bodies, but codegen (which
        // never re-checks the bench-transformed program) lowers it unconditionally.
        // Parse-without-check so we can put it in an ordinary function, then assert
        // the emitted IR retains the barrier — the deterministic form of "the
        // benched work is not deleted" (RFC-0055 §Verification 3).
        let src = "fn work(n: Int64) -> Int64 { return blackBox(n) }\n\
                   fn main() -> Int64 { return work(5) }";
        let toks = vyrn_frontend::lexer::lex(src).unwrap();
        let program = vyrn_frontend::parser::parse(toks).unwrap();
        let ir = emit(&program).unwrap();
        assert!(
            ir.contains("asm sideeffect"),
            "blackBox must emit a barrier:\n{ir}"
        );
        // A register-class value uses the identity-asm tie (`"=r,0"`), so the
        // optimizer treats the result as an unknown function of the input.
        assert!(
            ir.contains("\"=r,0\""),
            "register-class blackBox tie missing:\n{ir}"
        );
    }

    #[test]
    fn black_box_on_an_aggregate_uses_a_memory_clobber() {
        // A value a single register can't hold (an array) round-trips through a
        // slot with a `~{memory}` clobber instead of the `=r,0` tie.
        let src = "fn work(xs: Array<Int64>) -> Array<Int64> { return blackBox(xs) }\n\
                   fn main() -> Int64 { let a = work([1, 2, 3]) return 0 }";
        let toks = vyrn_frontend::lexer::lex(src).unwrap();
        let program = vyrn_frontend::parser::parse(toks).unwrap();
        let ir = emit(&program).unwrap();
        assert!(
            ir.contains("~{memory}"),
            "aggregate blackBox needs a memory clobber:\n{ir}"
        );
    }

    #[test]
    fn emits_module_with_main_wrapper() {
        let program = check("fn main() -> Int64 { let x = 2 + 3; print(x); return x; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("define i64 @vyrn_main("));
        assert!(ir.contains("define i32 @vyrn_entry()"));
        assert!(ir.contains("@printf"));
        assert!(ir.contains("add i64"));
    }

    // ---- payload enums on the wire (RFC-0024) ---------------------------

    /// A payload enum earns NO codec IR in either direction, and the call site
    /// refuses by name when the runtime is not linked.
    ///
    /// This test used to pin `@__vyrn_dec_Shape` and its `@__vyrn_vj_at_or_null`
    /// tuple read. RFC-0078 M2b retired the encode half and M3 the decode half:
    /// `fromJson` is now `std/jsonread` plus a per-type walk generated as Vyrn, so
    /// this file writes no codec IR at all and holds no DOM. What is pinned is the
    /// absence, plus the refusal — a single-source program has no resolver, so the
    /// runtime module cannot be injected into it, and codegen must say so rather
    /// than emit a call to a function nobody defined. The bytes are pinned three
    /// ways by `examples/jsondecbytes.vyrn` instead.
    #[test]
    fn payload_enum_gets_no_codec_ir_and_fromjson_refuses_by_name() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Nothing                    fn g(s: String) -> Validation<Shape> { return fromJson(Shape, s) }                    fn main() -> Int64 { return 0 }";
        let err = emit(&check(src).unwrap()).unwrap_err();
        assert!(
            err.contains("`fromJson` into `Shape`"),
            "names the type: {err}"
        );
        assert!(err.contains("RFC-0078 M3"), "names the reason: {err}");
    }

    /// A payload-LESS enum earns no codec function and no variant-name table.
    ///
    /// This used to pin the inline string encoding a nullary enum got inside
    /// `emit_encode`, whose O(1) name lookup read `@.enumnames.Role`. RFC-0078 M2b
    /// moved that encoding into a synthesized Vyrn `match` whose nullary arm is an
    /// ordinary `JStr("Guest")` over the string pool — which left the table with no
    /// reader at all, so it went too. What is pinned now is its absence.
    #[test]
    fn pure_nullary_enum_gets_no_codec_ir() {
        let src = "type Role = | Guest | Admin \
                   fn f(r: Role) -> Int64 { return match r { Guest => 0, Admin => 1 } } \
                   fn main() -> Int64 { return f(Guest) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            !ir.contains("@__vyrn_enc_Role("),
            "no codec fn for a nullary enum:\n{ir}"
        );
        assert!(
            !ir.contains("@.enumnames."),
            "the name table had one reader:\n{ir}"
        );
    }

    // ---- function values (RFC-0023) -------------------------------------

    const HO: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n\
         fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
         fn main() -> Int64 {\n\
             let a = twice([1, 2, 3], x -> x * 2)\n\
             let off = 10\n\
             let b = twice([1, 2, 3], x -> x + off)\n\
             let c = twice([1, 2, 3], dbl)\n\
             return 0 }";

    #[test]
    fn lambdas_monomorphize_with_no_indirect_calls() {
        let ir = emit(&check(HO).unwrap()).unwrap();
        // Each lambda literal is lifted to its own top-level function...
        assert!(
            ir.contains("@__vyrn_lambda_main_"),
            "lifted lambda missing:\n{ir}"
        );
        // ...and `twice` is specialized per target (three distinct instances).
        assert!(
            ir.matches("@vyrn_twice__ho").count() >= 3,
            "specializations missing:\n{ir}"
        );
        // The unspecialized `twice` shell is NEVER emitted (it has a `fn` param).
        assert!(
            !ir.contains("define { ptr, i64, i64 } @vyrn_twice("),
            "shell emitted:\n{ir}"
        );
        // Critically: no indirect calls anywhere — every `call` names a `@symbol`.
        for line in ir.lines() {
            let t = line.trim_start();
            if t.contains(" = call ") || t.starts_with("call ") {
                assert!(
                    t.contains("@"),
                    "indirect (function-pointer) call emitted:\n  {line}"
                );
            }
        }
    }

    #[test]
    fn a_captured_lambda_takes_a_capture_parameter() {
        let ir = emit(&check(HO).unwrap()).unwrap();
        // `|x| x + off` lifts to a two-parameter function (the capture, then x).
        // The readable shape is pinned; the trailing `_h<key>` is not, because
        // it is the structural identity ([`struct_key`]) and asserting it would
        // pin a hash rather than the thing this test is about.
        let def = ir
            .lines()
            .find(|l| {
                l.starts_with("define") && l.contains("@__vyrn_lambda_main_1_Int64Int64RInt64_h")
            })
            .unwrap_or_else(|| panic!("the captured lambda was not lifted:\n{ir}"));
        assert!(
            def.contains("(i64 %arg0, i64 %arg1)"),
            "captured lambda should take (capture, param):\n  {def}"
        );
    }

    // ---- stored function values (RFC-0037) --------------------------------

    /// A storage-heavy module: stored lambdas (with and without captures) in
    /// lets/arrays/records/Option/module state, a named source, composition,
    /// a stored value passed into a v1 `fn`-typed parameter — as a binding, as a
    /// record FIELD, as an array ELEMENT and as a call's RESULT — and calls
    /// through every storage form.
    ///
    /// A GENERIC record built by literal is here too, in each shape that carries
    /// a `fn` under a type parameter. Those solve their parameters from the type
    /// the literal is built for, and a wrong solve there registers a variant no
    /// dispatcher covers — which is a trap, not a pointer, but the same
    /// dispatcher is what keeps the module pointer-free.
    const STORED: &str = "type M = fn(Int64) -> Int64\n\
        type Ops = { plus: M, minus: M }\n\
        type Def<P, T> = { run: fn(P) -> T }\n\
        type Many<P, T> = { runs: Array<fn(P) -> T> }\n\
        type Maybe<P, T> = { run: Option<fn(P) -> T> }\n\
        type Outer<P, T> = { inner: Def<P, T> }\n\
        let mut chain: Array<M> = []\n\
        fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
        fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
            let mut out: Array<Int64> = []\n\
            for x in xs { out.push(f(x)) }\n\
            return out }\n\
        fn makeAdder(n: Int64) -> M { return x -> x + n }\n\
        fn main() -> Int64 {\n\
            let g: fn(Int64) -> Int64 = x -> x * 2\n\
            let h = g\n\
            let named = dbl\n\
            chain.push(x -> x + 1)\n\
            chain.push(dbl)\n\
            let ops = Ops { plus: x -> x + 10, minus: x -> x - 10 }\n\
            let p = ops.plus\n\
            let o: Option<M> = Some(makeAdder(5))\n\
            let q = match o { Some(f) => f(1), None => 0 }\n\
            let m = chain[0]\n\
            let ys = twice([1, 2], h)\n\
            let zs = twice([1, 2], ops.minus)\n\
            let ws = twice([1, 2], chain[1])\n\
            let vs = twice([1, 2], makeAdder(7))\n\
            let d: Def<Int64, Int64> = Def { run: dbl }\n\
            let dl: Def<Int64, Int64> = Def { run: x -> x + 3 }\n\
            let many: Many<Int64, Int64> = Many { runs: [dbl] }\n\
            let maybe: Maybe<Int64, Int64> = Maybe { run: Some(dbl) }\n\
            let outer: Outer<Int64, Int64> = Outer { inner: Def { run: dbl } }\n\
            let dr = d.run\n\
            let dlr = dl.run\n\
            let mr = many.runs[0]\n\
            let mbr = match maybe.run { Some(f) => f(1), None => 0 }\n\
            let or = outer.inner.run\n\
            return h(1) + named(2) + p(3) + q + m(4) + ys[0] + zs[0] + ws[0] + vs[0]\n\
                + dr(1) + dlr(1) + mr(1) + mbr + or(1) }";

    #[test]
    fn stored_fn_values_lower_with_no_indirect_calls() {
        let ir = emit(&check(STORED).unwrap()).unwrap();
        // A dispatcher exists for the stored signature, and its calls are all
        // direct. CRITICALLY: no indirect calls anywhere in the module — every
        // `call` names an `@symbol` (the RFC-0023 invariant, verbatim), and no
        // function's ADDRESS is ever taken (no `ptr @vyrn_` operand outside a
        // direct call — the wasm backend therefore emits no table/elem entry).
        assert!(
            ir.contains("@__vyrn_fndispatch_"),
            "dispatcher missing:\n{ir}"
        );
        for line in ir.lines() {
            let t = line.trim_start();
            if t.contains(" = call ") || t.starts_with("call ") {
                assert!(t.contains('@'), "indirect call emitted:\n  {line}");
            }
        }
        // No function-pointer materialization: `@vyrn_...` / lambda symbols
        // appear only immediately after `call <ty> ` or in a `define`.
        for line in ir.lines() {
            let t = line.trim_start();
            if t.starts_with("define ") || t.starts_with("declare ") {
                continue;
            }
            if let Some(i) = t.find("@vyrn_") {
                let before = &t[..i];
                assert!(
                    before.trim_end().ends_with("call") || before.contains(" call "),
                    "function symbol used outside a direct call:\n  {line}"
                );
            }
        }
    }

    #[test]
    fn stored_fn_value_is_a_two_word_enum() {
        let ir = emit(&check(STORED).unwrap()).unwrap();
        // Construction: `{ i64 tag, i64 payload }` aggregates built by
        // insertvalue; a captureless source has payload 0 and a capturing
        // lambda mallocs its capture block.
        assert!(
            ir.contains("insertvalue { i64, i64 } undef, i64 0, 0")
                || ir.contains("insertvalue { i64, i64 } undef, i64 1, 0"),
            "tagged construction missing:\n{ir}"
        );
        // makeAdder's `|x| x + n` captures `n` — a malloc'd single-field block.
        assert!(
            ir.contains("store { i64 }"),
            "capture block store missing:\n{ir}"
        );
    }

    #[test]
    fn stored_value_into_v1_param_keeps_direct_lambda_path_zero_cost() {
        let ir = emit(&check(STORED).unwrap()).unwrap();
        // `twice(.., h)` with a STORED value specializes an instance whose
        // capture parameter is the `{ i64, i64 }` enum, dispatched inside.
        assert!(
            ir.contains("@vyrn_twice__ho") && ir.contains("{ i64, i64 } %arg"),
            "stored-value specialization missing:\n{ir}"
        );
        // And a DIRECT lambda argument still monomorphizes with no enum: the
        // v1 corpus path emits a lifted-lambda call with plain params.
        let v1 = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
            let mut out: Array<Int64> = []\n\
            for x in xs { out.push(f(x)) }\n\
            return out }\n\
            fn main() -> Int64 { let ys = twice([1], x -> x * 2)  return ys[0] }";
        let ir1 = emit(&check(v1).unwrap()).unwrap();
        let ho_takes_enum = ir1
            .lines()
            .any(|l| l.contains("@vyrn_twice__ho") && l.contains("{ i64, i64 }"));
        assert!(
            !ir1.contains("fndispatch") && !ho_takes_enum,
            "v1 direct-lambda path must stay enum-free:\n{ir1}"
        );
    }

    #[test]
    fn generic_bodies_store_fn_values_per_instantiation() {
        // A stored fn type mentioning `T` resolves per instantiation: each
        // concrete signature gets its own dispatcher, all calls direct.
        let src = "fn relay<T>(x: T) -> T {\n\
             let f: fn(T) -> T = v -> v\n\
             return f(x) }\n\
             fn main() -> Int64 {\n\
             let n = relay(41)\n\
             let s = relay(\"ok\")\n\
             if s == \"ok\" { return n + 1 }\n\
             return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let dispatchers: std::collections::HashSet<&str> = ir
            .lines()
            .filter(|l| l.starts_with("define") && l.contains("@__vyrn_fndispatch_"))
            .collect();
        assert_eq!(
            dispatchers.len(),
            2,
            "one dispatcher per concrete sig:\n{ir}"
        );
        for line in ir.lines() {
            let t = line.trim_start();
            if t.contains(" = call ") || t.starts_with("call ") {
                assert!(t.contains('@'), "indirect call emitted:\n  {line}");
            }
        }
    }

    #[test]
    fn module_state_of_fn_type_initializes_and_reassigns() {
        let src = "let mut cur: fn(Int64) -> Int64 = x -> x + 1\n\
             fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let before = cur(10)  cur = dbl\n\
             return before + cur(10) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The global lowers to the two-word enum, initialized by the lambda
        // variant and reassigned to the named variant.
        assert!(
            ir.contains("@g.cur = internal global { i64, i64 } zeroinitializer"),
            "fn-typed module state lowering missing:\n{ir}"
        );
        assert!(
            ir.contains("@__vyrn_fndispatch_"),
            "dispatcher missing:\n{ir}"
        );
    }

    #[test]
    fn fnval_variant_tags_are_deterministic() {
        let a = emit(&check(STORED).unwrap()).unwrap();
        let b = emit(&check(STORED).unwrap()).unwrap();
        assert_eq!(a, b, "emission must be deterministic");
    }

    // ---- worker threads (RFC-0025) ---------------------------------------

    const SPAWNY: &str = "fn fib(n: Int64) -> Int64 { \
                              if n < 2 { return n } \
                              return fib(n - 1) + fib(n - 2) } \
                          fn main() -> Int64 { \
                              let a = spawn fib(10) \
                              let b = spawn fib(11) \
                              return a.join() + b.join() - fib(12) }";

    #[test]
    fn spawn_lowers_to_shim_threads_with_a_per_callee_thunk() {
        let ir = emit(&check(SPAWNY).unwrap()).unwrap();
        // The spawn site: a heap frame plus the thunk SYMBOL into the shim.
        assert!(
            ir.contains("call ptr @__vyrn_spawn(ptr @__vyrn_task_vyrn_fib, ptr"),
            "spawn call missing:\n{ir}"
        );
        // ONE thunk per callee — both spawn sites share it (deduped) — and it
        // calls the task function directly, then stores into the result slot.
        assert_eq!(
            ir.matches("define void @__vyrn_task_vyrn_fib(ptr %frame)")
                .count(),
            1,
            "expected exactly one shared thunk:\n{ir}"
        );
        assert!(ir.contains("%r = call i64 @vyrn_fib(i64 %a0)"), "{ir}");
        assert!(ir.contains("store i64 %r, ptr %frame"), "{ir}");
        // join blocks through the shim and loads the result from the frame.
        assert!(
            ir.contains("call ptr @__vyrn_join(ptr"),
            "join missing:\n{ir}"
        );
    }

    #[test]
    fn region_arena_is_thread_local() {
        // Isolated tasks may use `region { .. }`; with tasks on real threads
        // the arena stack must be per-thread (single-threaded targets lower
        // TLS to plain globals, so the shared IR is unaffected there).
        let ir = emit(&check(SPAWNY).unwrap()).unwrap();
        assert!(
            ir.contains("@__vyrn_region_sp = thread_local global i64 0"),
            "{ir}"
        );
        assert!(
            ir.contains(&format!(
                "@__vyrn_region_blocks = thread_local global [{} x ptr] zeroinitializer",
                vyrn_frontend::interp::REGION_MAX
            )),
            "{ir}"
        );
    }

    #[test]
    fn spawn_ir_has_no_indirect_calls() {
        // The RFC-0023 invariant survives RFC-0025: the thunk symbol passed to
        // `__vyrn_spawn` sits in ARGUMENT position (a C-boundary detail, not a
        // Vyrn-level function value); every emitted `call` still names @symbol.
        let ir = emit(&check(SPAWNY).unwrap()).unwrap();
        for line in ir.lines() {
            let t = line.trim_start();
            if t.contains(" = call ") || t.starts_with("call ") {
                assert!(
                    t.contains("@"),
                    "indirect (function-pointer) call emitted:\n  {line}"
                );
            }
        }
    }

    // ---- round-two audit fixes -------------------------------------------

    /// A non-ASCII identifier reaches the IR escaped (`_uXXXX_`), never raw:
    /// unquoted LLVM identifiers are ASCII-only, and a raw `é` produced a
    /// module clang/llc reject with a parse error while the interpreter ran
    /// the same program.
    #[test]
    fn a_non_ascii_name_is_escaped_in_the_symbol() {
        let src = "fn héllo(n: Int64) -> Int64 { return n } \
                   fn main() -> Int64 { return héllo(1) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("define i64 @vyrn_h_u00E9_llo("),
            "escaped define missing:\n{ir}"
        );
        assert!(ir.contains("call i64 @vyrn_h_u00E9_llo(i64"), "{ir}");
        assert!(!ir.contains("@vyrn_héllo"), "raw name in the IR: {ir}");
    }

    /// A lifted lambda is generated at ITS OWN expected signature, not the
    /// ambient one: the expect stack is taken across the lift and restored
    /// after, so consecutive lifts cannot see each other's (or the caller's)
    /// expectation. RFC-0037 defers fn-types that RETURN fn-values, so the
    /// reachable shape is two annotated lambdas with different signatures in
    /// one block — the second lift must not answer the first's expectation.
    #[test]
    fn a_lifted_lambda_body_sees_the_expected_signature() {
        // A lifted lambda is generated at ITS OWN expected signature: the
        // expect stack is taken across the lift and restored after, so two
        // consecutive lifts cannot answer each other's expectation. The two
        // signatures differ in PARAMETER, which is what the bodies are typed
        // from; RFC-0037 defers the fn-returning-fn shape outright.
        let src =
            "fn main() -> Int64 {                    let a: fn(Int64) -> Int64 = x -> x + 1; \
                   let b: fn(String) -> Int64 = s -> s.byteLength; \
                   return a(1) + b(\"ab\") }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Two lifted lambdas, two signatures.
        assert_eq!(ir.matches("@__vyrn_lambda_").count() >= 4, true, "{ir}");
        assert!(ir.contains("define i64 @__vyrn_lambda_"), "{ir}");
    }

    /// A task frame outlives its spawning block, so a String argument whose
    /// bytes are arena-owned must be copied into `malloc`'d storage at the
    /// spawn: the region exit frees the arena before the worker may run. Each
    /// spawn site now costs two `__vyrn_malloc`s in main — the frame AND the
    /// argument copy — where only the frame used to be.
    #[test]
    fn spawn_arguments_are_copied_out_of_the_region() {
        let src = "fn work(s: String) -> Int64 { return s.byteLength } \
                   fn main() -> Int64 { \
                   let mut t: Task<Int64> = spawn work(\"\") \
                   region { t = spawn work(\"a\" + \"b\") } return t.join() }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let start = ir.find("define i64 @vyrn_main(").expect("main present");
        let body = &ir[start..];
        let body = &body[..body.find("\n}\n").expect("unterminated body")];
        assert_eq!(
            body.matches("call ptr @__vyrn_malloc").count(),
            4,
            "argument copied out beside each frame:\n{body}"
        );
    }

    // ---- input I/O (RFC-0014) -------------------------------------------

    #[test]
    fn read_file_lowers_to_shim_call_with_canonical_messages() {
        let src = "fn main() -> Int64 { \
                       let r = readFile(\"cfg.txt\") \
                       return match r { Ok(s) => s.byteLength, Err(e) => e.byteLength } }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The shim primitive plus the single-source canonical error strings.
        assert!(ir.contains("call i32 @__vyrn_read_file(ptr"), "{ir}");
        assert!(ir.contains("@__vyrn_read_err"), "{ir}");
        assert!(ir.contains("c\"cannot read `%s`\\00\""), "{ir}");
        assert!(ir.contains("c\"`%s` is not valid UTF-8\\00\""), "{ir}");
        assert!(ir.contains("c\"`%s` contains a NUL byte\\00\""), "{ir}");
        // The UTF-8 validation reuses the shared DFA.
        assert!(ir.contains("call i1 @__vyrn_utf8valid(ptr"), "{ir}");
    }

    #[test]
    fn write_file_lowers_to_shim_call_with_canonical_message() {
        let src = "fn main() -> Int64 { \
                       let w = writeFile(\"o.txt\", \"x\") \
                       return match w { Ok(b) => 0, Err(e) => e.byteLength } }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i32 @__vyrn_write_file(ptr"), "{ir}");
        assert!(ir.contains("c\"cannot write `%s`\\00\""), "{ir}");
    }

    #[test]
    fn args_and_read_line_lower_to_runtime_calls() {
        let src = "fn main() -> Int64 { \
                       let a = args() \
                       let l = readLine() \
                       let n = match l { Some(s) => s.byteLength, None => 0 } \
                       return a.length + n }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call { ptr, i64, i64 } @__vyrn_args()"), "{ir}");
        assert!(ir.contains("call ptr @__vyrn_read_line(ptr"), "{ir}");
    }

    #[test]
    fn bytes_array_uses_i8_stride() {
        // RFC-0014 M2: Array<UInt8> elements are one byte, not eight — the
        // indexed read must load an `i8` through an i8-typed gep.
        let src = "fn main() -> Int64 { \
                       let b = bytes(\"hi\") \
                       let x = b[0] \
                       return b.length }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("getelementptr i8, ptr"), "{ir}");
        assert!(ir.contains("load i8, ptr"), "{ir}");
    }

    #[test]
    fn string_from_bytes_validates_and_pins_error_strings() {
        let src = "fn main() -> Int64 { \
                       let r = stringFromBytes(bytes(\"hi\")) \
                       return match r { Ok(s) => s.byteLength, Err(e) => e.byteLength } }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vyrn_bytes_dup(ptr"), "{ir}");
        assert!(ir.contains("c\"bytes contain a NUL byte\\00\""), "{ir}");
        assert!(ir.contains("c\"bytes are not valid UTF-8\\00\""), "{ir}");
    }

    #[test]
    fn implicit_coercion_into_validated_type_emits_check() {
        // A dynamic raw Int64 argument flowing into an `Age` parameter runs
        // the predicate inline and traps through the per-type message.
        let src = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { let mut x = 30 x = x - 1 return g(x) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.trap.verr.Age"), "coercion validates: {ir}");
    }

    #[test]
    fn same_named_type_coercion_emits_no_double_check() {
        // Passing an already-Age value to an Age parameter re-checks nothing.
        let src = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn h(a: Age) -> Int64 { return g(a) } \
                   fn main() -> Int64 { return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Only the (elided-const) explicit paths exist: no vfail label at all,
        // since no dynamic coercion crosses a type boundary.
        assert!(!ir.contains("vfail"), "no redundant checks: {ir}");
    }

    #[test]
    fn division_emits_stderr_trap_guards() {
        // `/` guards divisor-zero and MIN/-1 with the interpreter's exact
        // `error: ...` message on stderr (a bare sdiv would SEH-crash silently).
        let program = check("fn main() -> Int64 { let mut d = 3; return 10 / d; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("@.trap.div0"), "zero guard: {ir}");
        assert!(ir.contains("@.trap.divovf"), "MIN/-1 guard: {ir}");
        assert!(ir.contains("icmp eq i64"), "guard compare: {ir}");
        assert!(ir.contains("@fputs"), "stderr write: {ir}");
    }

    #[test]
    fn remainder_guards_zero_but_not_min_overflow() {
        // `%` guards divisor-zero (its own `rem0` message) and rewrites a `-1`
        // divisor via `select` so `MIN % -1` yields 0 with NO trap (RFC-0060) —
        // it must NOT emit the div-overflow trap that `/` does.
        let rem = check("fn main() -> Int64 { let mut d = 3; return 10 % d; }").unwrap();
        let rem_ir = emit(&rem).unwrap();
        // `%` uses its own zero-trap message and the `select`-based `-1` guard.
        assert!(rem_ir.contains("br i1 %"), "zero guard branch: {rem_ir}");
        assert!(rem_ir.contains("select i1"), "-1 divisor guard: {rem_ir}");
        assert!(rem_ir.contains("srem i64"), "signed remainder: {rem_ir}");
        // Unlike `/`, `%` never branches to the div-overflow trap. `/` compares
        // the DIVIDEND to i64::MIN; `%` never emits that MIN compare.
        let div = check("fn main() -> Int64 { let mut d = 3; return 10 / d; }").unwrap();
        let div_ir = emit(&div).unwrap();
        assert!(
            div_ir.contains("-9223372036854775808"),
            "`/` compares dividend to MIN: {div_ir}"
        );
        assert!(
            !rem_ir.contains("-9223372036854775808"),
            "`%` emits no MIN overflow compare: {rem_ir}"
        );
    }

    #[test]
    fn break_and_continue_run_scope_drops_before_branching() {
        // A loop body that owns a heap string (`"x" + "y"`) drops it on EVERY
        // exit path (RFC-0060): the fall-through, the `continue`, and the final
        // loop exit each free it — so the free count exceeds the single
        // fall-through free a drop-less break/continue would emit.
        let cont = check(
            "fn main() -> Int64 { let mut i = 0 \
             while i < 3 { let s = \"x\" + \"y\" i = i + 1 if i == 1 { continue } } return 0 }",
        )
        .unwrap();
        let ir = emit(&cont).unwrap();
        let frees = ir.matches("call void @__vyrn_free(").count();
        assert!(
            frees >= 2,
            "continue path must also drop `s`: {frees} frees"
        );

        // A `break` past an owned local drops it on the break edge too.
        let brk = check(
            "fn main() -> Int64 { \
             while true { let s = \"a\" + \"b\" break } return 0 }",
        )
        .unwrap();
        let bir = emit(&brk).unwrap();
        assert!(
            bir.contains("call void @__vyrn_free("),
            "break must drop the body's owned string: {bir}"
        );
    }

    #[test]
    fn if_let_lowers_to_a_tag_test_and_payload_bind_no_phi() {
        // `if let Some(v) = e { .. }` extracts the tag, branches, and binds the
        // payload — a statement form, so no `phi` merge (RFC-0060).
        let p = check(
            "fn f() -> Option<Int64> { return Some(3) } \
             fn main() -> Int64 { if let Some(v) = f() { return v } return 0 }",
        )
        .unwrap();
        let ir = emit(&p).unwrap();
        assert!(ir.contains("il.then"), "if-let then block: {ir}");
        assert!(
            ir.contains("extractvalue { i1, i64, i64 }"),
            "tag/payload extraction: {ir}"
        );
    }

    #[test]
    fn continue_in_a_for_targets_a_latch_that_steps_the_index() {
        // `continue` in a `for` branches to the latch block (which increments the
        // index and re-tests) — not straight back to the condition (RFC-0060).
        let p = check(
            "fn main() -> Int64 { let mut n = 0 \
             for i in [0, 1, 2] { if i == 1 { continue } n = n + 1 } return n }",
        )
        .unwrap();
        let ir = emit(&p).unwrap();
        assert!(ir.contains("flatch"), "for-loop emits a latch block: {ir}");
    }

    #[test]
    fn a_float_with_no_formatter_in_the_link_refuses_by_name() {
        // This test used to pin the `fcmp uno` that selected a literal `NaN` over
        // UCRT's `-nan(ind)`. RFC-0081 M2 took that selection out: a float prints
        // through `std/num`'s `f64Str`, which spells the three non-finite words
        // itself. `check` links no module, so what is left to pin here is the
        // refusal — which names the function rather than leaving an undefined
        // symbol for the linker to report.
        let program = check("fn main() -> Int64 { print(0.0 / 0.0); return 0; }").unwrap();
        let err = emit(&program).unwrap_err();
        assert!(err.contains("num$f64Str"), "names the formatter: {err}");
    }

    #[test]
    fn dead_tail_of_nonint_fn_is_unreachable_not_ret_zero() {
        // A String-returning fn whose branches both return leaves a dead final
        // block; `ret ptr 0` there is invalid IR — it must be `unreachable`.
        let program = check(
            "fn pick(b: Bool) -> String { if b { return \"yes\" } else { return \"no\" } } \
             fn main() -> Int64 { print(pick(true)); return 0; }",
        )
        .unwrap();
        let ir = emit(&program).unwrap();
        assert!(!ir.contains("ret ptr 0"), "invalid dead default:\n{ir}");
        assert!(ir.contains("unreachable"));
    }

    #[test]
    fn unit_match_arms_emit_no_phi_void() {
        // A statement-position match whose arms are side-effecting prints has
        // Unit type; `phi void` is invalid IR, so no merge value is built.
        let program = check(
            "fn main() -> Int64 { let o = Some(4); \
             match o { Some(x) => print(x), None => print(0) } \
             return 0; }",
        )
        .unwrap();
        let ir = emit(&program).unwrap();
        assert!(!ir.contains("phi void"), "invalid phi:\n{ir}");
    }

    #[test]
    fn predicate_string_literals_reach_the_pool() {
        // A literal that appears ONLY in a type's `where` predicate must still
        // be emitted as a string global (the predicate is lowered inline at
        // construction sites).
        let program = check(
            "type Name = String where value == \"root\" \
             fn main() -> Int64 { let n = Name(\"root\"); print(n); return 0; }",
        )
        .unwrap();
        let ir = emit(&program).unwrap();
        assert!(
            ir.contains("c\"root\\00\""),
            "predicate literal missing from pool:\n{ir}"
        );
    }

    #[test]
    fn short_circuit_uses_phi() {
        let program =
            check("fn main() -> Int64 { if true && false { return 1; } return 0; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("phi i1"), "{ir}");
    }

    #[test]
    fn logging_lowers_to_fprintf_stderr() {
        // `log.info(..)` emits an fprintf to stderr (via the shim) with
        // the level-name global.
        let src = "fn main() -> Int64 { let log = logger(\"m\"); log.info(\"hi\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_stderr()"), "stderr handle: {ir}");
        assert!(ir.contains("@fprintf"), "fprintf: {ir}");
        assert!(ir.contains("@.lvl.info"), "level name global: {ir}");
    }

    #[test]
    fn stdout_sink_selects_stream_1() {
        let src = "logging { sink: stdout } \
                   fn main() -> Int64 { let l = logger(\"m\"); l.error(\"x\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_stdout()"), "stdout via the shim: {ir}");
    }

    #[test]
    fn file_sink_opens_and_closes_in_main() {
        let src = "logging { sink: file(\"a.log\") } \
                   fn main() -> Int64 { let l = logger(\"m\"); l.error(\"x\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@fopen(ptr @.logpath"), "opens the file: {ir}");
        assert!(ir.contains("@fclose"), "closes the file: {ir}");
        assert!(
            ir.contains("load ptr, ptr @__vyrn_log_file"),
            "logs to the file handle: {ir}"
        );
    }

    #[test]
    fn log_calls_below_threshold_emit_no_write() {
        // With `level: warn`, a `debug` call must not emit an fprintf, but a
        // `warn` call must. (Args are still evaluated — see the interpreter.)
        let src = "logging { level: warn } \
                   fn main() -> Int64 { let log = logger(\"m\"); \
                   log.debug(\"lo\"); log.warn(\"hi\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The level-name globals are always declared; check the fprintf *use*
        // (`@.fmt.log, ptr @.lvl.<level>`) instead.
        assert!(
            ir.contains("@.fmt.log, ptr @.lvl.warn"),
            "warn should emit: {ir}"
        );
        assert!(
            !ir.contains("@.fmt.log, ptr @.lvl.debug"),
            "debug should be filtered out: {ir}"
        );
    }

    #[test]
    fn tagged_template_lowers_to_tag_call_with_arrays() {
        // `sql"a\{x}b"` -> `sql(list([..]), list([value(x)]))`; the value is boxed
        // into the `Value` enum aggregate and the arrays are built on the heap.
        let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { return 0; } \
                   fn main() -> Int64 { let x = 5; return sql\"a\\{x}b\"; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @vyrn_sql("), "calls the tag: {ir}");
        // Two heap buffers (parts + values) are allocated for the growable arrays.
        assert!(
            ir.contains("insertvalue { ptr, i64, i64 }"),
            "builds arrays: {ir}"
        );
    }

    #[test]
    fn string_interpolation_lowers_to_str_and_concat() {
        // `"n=\{n}"` desugars to `concat("n=", str(n))`; `str(Bool)` selects the
        // no-newline global and copies it into a fresh buffer.
        let src = "fn main() -> Int64 { let n = 7; let ok = true; \
                   let s = \"n=\\{n} ok=\\{ok}\"; return s.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@__vyrn_snprintf"),
            "str(Int64) -> snprintf: {ir}"
        );
        assert!(
            ir.contains("select i1"),
            "str(Bool) -> select true/false: {ir}"
        );
        assert!(ir.contains("@strcpy"), "bool/str render copies: {ir}");
        assert!(ir.contains("@.str.true"), "no-newline bool global: {ir}");
    }

    #[test]
    fn string_plus_lowers_to_concat_runtime() {
        // `a + b` on Strings emits the same strlen/strcpy/strcat sequence `concat`
        // used, and `x.toString()` renders via snprintf.
        let src = "fn main() -> Int64 { let a = \"x\"; let n = (5).toString() + a; return n.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_strlen"), "concat length: {ir}");
        assert!(
            ir.contains("@strcpy") && ir.contains("@strcat"),
            "concat copy: {ir}"
        );
        assert!(
            ir.contains("@__vyrn_snprintf"),
            "toString(Int) -> snprintf: {ir}"
        );
    }

    #[test]
    fn string_accumulator_appends_in_place() {
        // `out = out + piece` on a local String whose every other use is a
        // non-retaining read grows one buffer (`realloc` + `memcpy`) instead of
        // allocating a fresh one per iteration.
        let src = "fn main() -> Int64 { let mut out = \"\"; let mut i = 0; \
                   while i < 4 { out = out + \"x\"; i = i + 1; } \
                   print(\"\\{out}\"); return out.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("app.own"),
            "takes ownership of the buffer once: {ir}"
        );
        assert!(
            ir.contains("call ptr @__vyrn_realloc"),
            "grows in place: {ir}"
        );
        assert!(
            ir.contains("call void @llvm.memcpy.p0.p0.i64"),
            "copies the operand in: {ir}"
        );
    }

    #[test]
    fn string_accumulator_not_appended_when_aliased_or_in_region() {
        // The ALIAS half of this test is gone with RFC-0089 rule 1. `let copy =
        // out` moves, so a program that reads `out` afterward does not compile,
        // and the named fix `out.copy()` gives `copy` a buffer of its own —
        // which is the condition the in-place append needed all along. The
        // whitelist row survives for `Ref`, the one aliasing the language keeps.
        //
        // A user call may store what it is handed, so it still disqualifies.
        let escaped = "fn keep(s: String) -> Int64 { return s.byteLength; } \
                       fn main() -> Int64 { let mut out = \"a\"; out = out + \"b\"; \
                       return keep(out); }";
        let ir = emit(&check(escaped).unwrap()).unwrap();
        assert!(
            !ir.contains("app.own"),
            "escaping accumulator stays copying: {ir}"
        );
        // Region memory comes from an arena that cannot be `realloc`'d.
        let regioned = "fn main() -> Int64 { region { let mut out = \"a\"; \
                        out = out + \"b\"; print(\"\\{out}\"); } return 0; }";
        let ir = emit(&check(regioned).unwrap()).unwrap();
        assert!(
            !ir.contains("app.own"),
            "region accumulator stays copying: {ir}"
        );
    }

    /// The `emitArr` shape — accumulate in a loop, then `return out + "]"`.
    /// Banning both operands of every `+` disqualified `out` on that last line,
    /// which is where `toJson`'s O(N²) came from: each iteration re-copied the
    /// whole result. The pin is the COUNT of copying concats, not a duration:
    /// exactly one, the tail, so the copying work cannot scale with the loop.
    #[test]
    fn accumulator_returned_through_a_concat_still_appends_in_place() {
        let src = "fn build(n: Int64) -> String { let mut out = \"[\"; let mut i = 0; \
                   while i < n { out = out + \",\"; i = i + 1; } return out + \"]\"; } \
                   fn main() -> Int64 { return build(3).byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("app.own"),
            "the loop must take the append path: {ir}"
        );
        assert_eq!(
            ir.matches("call ptr @__vyrn_str_concat(").count(),
            1,
            "only the tail `out + \"]\"` may copy; a second means the loop is \
             copying again and the complexity class regressed: {ir}"
        );
    }

    /// Every operator `binop_retains_str` calls non-retaining, in the position
    /// that matters — reading the accumulator — must leave it eligible. The
    /// exhaustive match in that function is what forces a NEW operator to be
    /// classified; this is what stops an existing one being reclassified by
    /// accident. `=~` takes the accumulator on the left only (its right operand
    /// must be a literal pattern).
    #[test]
    fn non_retaining_operators_do_not_disqualify_an_accumulator() {
        for read in [
            "out + \"]\"",
            "if out == \"x\" { 1 } else { 0 }",
            "if out != \"x\" { 1 } else { 0 }",
            "if out < \"x\" { 1 } else { 0 }",
            "if out <= \"x\" { 1 } else { 0 }",
            "if out > \"x\" { 1 } else { 0 }",
            "if out >= \"x\" { 1 } else { 0 }",
            "if out =~ \"a*\" { 1 } else { 0 }",
        ] {
            let ret = if read.starts_with("out +") {
                format!("return ({read}).byteLength;")
            } else {
                format!("return {read};")
            };
            let src = format!(
                "fn main() -> Int64 {{ let mut out = \"a\"; let mut i = 0; \
                 while i < 3 {{ out = out + \"x\"; i = i + 1; }} {ret} }}"
            );
            let ir = emit(&check(&src).unwrap()).unwrap();
            assert!(ir.contains("app.own"), "`{read}` must stay eligible: {ir}");
        }
    }

    /// The in-place path must not change reclamation. Since RFC-0114 M2 the
    /// copying lowering releases each replaced value at the store, so it
    /// carries exactly ONE free the in-place lowering has no counterpart for:
    /// the in-place buffer is reused, there is no replaced value. Everything
    /// else — the temporaries, the caller's frees — must be identical, so the
    /// assertion is `copying == appending + 1` and nothing looser.
    #[test]
    fn the_append_path_frees_exactly_what_the_copying_path_frees() {
        let body = |extra: &str| {
            format!(
                "fn keep(s: String) -> Int64 {{ return s.byteLength; }} \
                 fn build(n: Int64) -> String {{ let mut out = \"[\"; let mut i = 0; \
                 {extra} while i < n {{ out = out + \",\"; i = i + 1; }} return out + \"]\"; }} \
                 fn main() -> Int64 {{ let a = \"x\"; let b = \"y\"; let s = a + b; \
                 return build(s.byteLength).byteLength; }}"
            )
        };
        let appending = emit(&check(&body("")).unwrap()).unwrap();
        // Handing `out` to a user call is the whitelist's own ban, so this is the
        // same function lowered the old way. It used to say `let alias = out`,
        // which RFC-0089 rule 1 now refuses.
        let copying = emit(&check(&body("let n0 = keep(out);")).unwrap()).unwrap();
        assert!(
            appending.contains("app.own") && !copying.contains("app.own"),
            "setup"
        );
        assert_eq!(
            free_calls(&copying),
            free_calls(&appending),
            // Equal since exit-residue round fifteen: the in-place path's
            // take-ownership copy frees the buffer it copied out of when the
            // plan proved the place owns it — one store-side free each,
            // differently placed. (On this fixture's literal init the take's
            // free is a runtime no-op — `str_free` skips a static — but the
            // instruction is emitted, and the count is what this pin reads.)
            "one store-side free each; any other difference is a \
             reclamation change:\n{appending}\n=== copying ===\n{copying}"
        );
    }

    #[test]
    fn contextual_array_literal_lowers_to_heap_triple() {
        // A literal in an `Array<T>` slot is malloc'd into the `{ptr,len,cap}`
        // triple (like `list([..])`), then `.length` reads field 1.
        let src = "fn main() -> Int64 { let a: Array<Int64> = [1, 2, 3]; return a.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vyrn_malloc"), "heap copy: {ir}");
        assert!(
            ir.contains("insertvalue { ptr, i64, i64 }"),
            "growable triple: {ir}"
        );
    }

    #[test]
    fn numeric_conversions_lower_to_casts() {
        // No `print` of a float: since RFC-0081 M2 that is a call into `std/num`,
        // which a bare `check` does not link. The conversions are what this pins.
        let src = "fn main() -> Int64 { let f = 3.5; let n = Int64(f); \
                   let g = Float64(n); let s = Int32(5000000000); \
                   if g > 0.0 { return Int64(s); } return Int64(s); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Saturating, not bare: a plain `fptosi` is poison out of range, and the
        // interpreter's `as` saturates (RFC-0078 M4a).
        assert!(
            ir.contains("@llvm.fptosi.sat.i64.f64(double"),
            "float→int: {ir}"
        );
        assert!(ir.contains("sitofp i64"), "int→float: {ir}");
        assert!(
            ir.contains("trunc i64") && ir.contains("to i32"),
            "int→i32: {ir}"
        );
        assert!(
            ir.contains("sext i32 ") && ir.contains("to i64"),
            "i32→int: {ir}"
        );
    }

    #[test]
    fn sized_ints_lower_to_width_ops() {
        let src = "fn main() -> Int64 { let a: Int32 = 5; let b: Int32 = 3; \
                   let c = a + b; print(c); if c > 0 { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("add i32"), "i32 add: {ir}");
        assert!(ir.contains("icmp sgt i32"), "i32 compare: {ir}");
        // Literals coerce into i32 slots via trunc.
        assert!(
            ir.contains("trunc i64") && ir.contains("to i32"),
            "literal→i32: {ir}"
        );
        // print sign-extends back to i64.
        assert!(ir.contains("sext i32"), "print sext: {ir}");
    }

    #[test]
    fn unsigned_ints_lower_to_unsigned_ops() {
        let src = "fn main() -> Int64 { let a: UInt32 = 10; let b: UInt32 = 3; \
                   let q = a / b; print(q); if a > b { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("udiv i32"), "unsigned divide: {ir}");
        assert!(ir.contains("icmp ugt i32"), "unsigned compare: {ir}");
        // print zero-extends (not sign-extends) and uses the %llu format.
        assert!(
            ir.contains("zext i32") && ir.contains("@.fmt.u"),
            "unsigned print: {ir}"
        );
    }

    #[test]
    fn uint64_prints_without_extension() {
        // A 64-bit value is already i64; no zext/sext is emitted before print.
        let src = "fn main() -> Int64 { let n: UInt64 = 42; print(n); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.fmt.u"), "uses %llu: {ir}");
        assert!(!ir.contains("zext i64 %"), "no i64→i64 extension: {ir}");
    }

    #[test]
    fn floats_lower_to_double_ops() {
        let src = "fn main() -> Int64 { let a = 1.5; let b = 2.0; \
                   if a * b > 2.0 { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("fmul double"), "float multiply: {ir}");
        assert!(ir.contains("fcmp ogt double"), "float compare: {ir}");
        // Literals use the exact hex-bit form.
        assert!(ir.contains("0x3FF8000000000000"), "1.5 as hex double: {ir}");
    }

    #[test]
    fn float32_lowers_to_single_precision_ops() {
        let src = "fn main() -> Int64 { let a: Float32 = 1.5; let b: Float32 = 2.5; \
                   let c = a + b; let w = Float64(c); if c > 0.0 { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("fadd float"), "f32 add: {ir}");
        assert!(ir.contains("fcmp ogt float"), "f32 compare: {ir}");
        // Literals round into f32 slots via fptrunc, and widening goes back the
        // other way. (`print(c)` used to be the widening here; it is a call into
        // `std/num` since RFC-0081 M2 and a bare `check` links no module, so the
        // explicit `Float64(c)` — the same `fpext` — stands in for it.)
        assert!(
            ir.contains("fptrunc double") && ir.contains("to float"),
            "literal→f32: {ir}"
        );
        assert!(
            ir.contains("fpext float") && ir.contains("to double"),
            "widening fpext: {ir}"
        );
    }

    #[test]
    fn float32_conversions_use_fptrunc_and_fpext() {
        let widen = "fn main() -> Int64 { let x: Float32 = 1.5; let d = Float64(x); \
                     if d > 0.0 { return 1; } return 0; }";
        assert!(
            emit(&check(widen).unwrap())
                .unwrap()
                .contains("fpext float"),
            "f32→f64"
        );
        let narrow = "fn main() -> Int64 { let d = 1.5; let x = Float32(d); \
                      if x > 0.0 { return 1; } return 0; }";
        assert!(
            emit(&check(narrow).unwrap())
                .unwrap()
                .contains("fptrunc double"),
            "f64→f32"
        );
    }

    #[test]
    fn exit_code_is_masked_to_low_byte() {
        // `@main` masks vyrn_main's return so it matches the interpreter's
        // `code & 0xff` on values > 255 (POSIX exit convention).
        let ir = emit(&check("fn main() -> Int64 { return 285; }").unwrap()).unwrap();
        assert!(ir.contains("and i64 %r, 255"), "{ir}");
    }

    #[test]
    fn drop_of_array_frees_its_buffer() {
        // `drop a` on a growable array frees its backing buffer (an extra free
        // beyond the region-runtime baseline). It is the only way to reclaim one
        // early — the `afree` builtin that used to be the other way is gone.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(1); \
                   drop a; return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The buffer pointer is aggregate field 0.
        assert!(ir.contains("extractvalue { ptr, i64, i64 }"), "{ir}");
        assert!(free_calls(&ir) >= 1, "expected a free from `drop`: {ir}");
    }

    #[test]
    fn length_and_index_surface_lower_to_a_load_and_a_checked_read() {
        // `a.length` -> extractvalue field 1; `a[i]` -> bounds-checked `@at`.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(5); \
                   return a.length + a[0]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("extractvalue { ptr, i64, i64 }"),
            "length -> extractvalue: {ir}"
        );
        assert!(ir.contains("icmp uge i64"), "index -> bounds check: {ir}");
    }

    #[test]
    fn for_loop_lowers_to_indexed_walk() {
        // A `for` over a growable array reads the length once and walks it with a
        // bounds-comparison branch, accumulating into the total.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(3); a.push(4); \
                   let mut s = 0; for x in a { s = s + x; } return s; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("fcond"),
            "expected a for-loop condition block: {ir}"
        );
        assert!(ir.contains("fbody"), "expected a for-loop body block: {ir}");
        assert!(
            ir.contains("icmp uge i64"),
            "expected the length bound check: {ir}"
        );
        // The element type is Int, so the per-iteration element load is `i64`.
        assert!(ir.contains("load i64"), "expected an element load: {ir}");
    }

    // The always-present runtime contributes exactly RUNTIME_FREES release
    // occurrences: three `@free`s in the arena — its blocks and its side vector
    // in `__vyrn_region_exit`, the vector alone in `__vyrn_region_pop` — one
    // inside `@__vyrn_str_free` itself, and one `@__vyrn_str_free` in the
    // `__vyrn_bytes_dup` NUL path. An *auto*-free is a release beyond that
    // baseline.
    //
    // It was 3 while the arena chained its blocks through a trailer: the chain
    // was in the blocks, so the walk needed no allocation of its own and `pop`
    // had nothing to release. The vector is the arena's own bookkeeping, so it
    // is freed on both ways out (RFC-0096's addendum).
    //
    // It was 6 until RFC-0090 M3, which took `__vyrn_stream_close` out of the
    // always-present prelude: a release is one function per element type now,
    // emitted only for a program that has a stream to release, and it carries
    // STREAM_CLOSER_FREES of its own.
    //
    // Both spellings count. A `String` drop is `@__vyrn_str_free` since
    // RFC-0089 M1a — it reads the header cap and returns on a static — and the
    // tests below ask how many bindings are reclaimed, not which call does it.
    //
    // Up from 5 in exit-residue round twenty-seven:
    // `__vyrn_region_pop_except` carries two more — the non-escaping blocks
    // and the side vector.
    const RUNTIME_FREES: usize = 7;

    /// What one element type's release costs in `free`s: a buffer stream's data,
    /// and a producer's step capture block.
    // Down from 2 since RFC-0114 §25 round three: the step's capture block
    // is handed to `__vyrn_fnval_release` now (whose stub, in a program with
    // no fn-value constructions, contains no free of its own), so the closer
    // text carries one raw free — the buffer arm's.
    const STREAM_CLOSER_FREES: usize = 1;
    fn free_calls(ir: &str) -> usize {
        ir.matches("call void @__vyrn_free(ptr").count()
            + ir.matches("call void @__vyrn_str_free(ptr").count()
    }

    #[test]
    fn non_escaping_temporary_is_freed() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let n = s.byteLength; return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            free_calls(&ir) > RUNTIME_FREES,
            "expected an auto-free beyond the runtime: {ir}"
        );
    }

    /// RFC-0096 M3 — the temporary INSIDE the expression, which no binding
    /// names. Two frees: the `@str` result the concatenation copied out of, and
    /// the binding that holds the concatenation.
    ///
    /// This is the node-free half of the `exprTemporary` memory row. That row
    /// measures the direct backend and this one reads the textual backend's IR,
    /// so neither engine can start leaking on its own.
    #[test]
    fn a_temporary_inside_an_expression_is_freed() {
        let src = "fn main() -> Int64 { let n = 1; let s = \"n\" + n.toString(); \
                   return s.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 2,
            "the `@str` temporary and the binding: {ir}"
        );
    }

    /// The same, through the interpolation spine.
    ///
    /// `"a\{n}b\{n}c"` folds left into FOUR `@concat`s over the five pieces, so
    /// six buffers are allocated and one survives: two `@str` holes, three
    /// inner joins, and the binding. The three literal pieces allocate nothing
    /// and are not freed — this is the count that says the rule reads the
    /// EXPRESSION rather than the type.
    #[test]
    fn every_interpolation_hole_is_freed() {
        let src = "fn main() -> Int64 { let n = 1; let s = \"a\\{n}b\\{n}c\"; \
                   return s.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 6,
            "two `@str` holes, three inner joins, and the binding: {ir}"
        );
    }

    #[test]
    fn an_alias_is_freed_once_and_only_once() {
        // `let t = s` MOVES the buffer under RFC-0089 rule 1: `t` owns it and `s`
        // no longer does. One buffer, one free — the pair used to leak, because
        // this pass could not tell an alias from a move.
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let t = s; return t.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 1,
            "exactly one auto-free is expected: {ir}"
        );
    }

    #[test]
    fn a_captured_binding_reclaims_beside_its_snapshot() {
        // A stored closure holds the buffer by value (RFC-0037) and can outlive
        // this block, so nothing here may release the STRING — census §16.
        //
        // Since Phase 10b the closure's own capture block is released; since
        // RFC-0114 §25 round three the capture is a DUPLICATE the block owns
        // (two lambdas over one `s` used to share one pointer, which is why
        // the release had to stay shallow), and `__vyrn_fnval_release` walks
        // it before the block goes. Round fifty-seven closed the last third:
        // the snapshot being the block's OWN copy is exactly what makes `s`
        // the frame's to release again, so the three frees below are the
        // copied String, the block, and the binding's own buffer at exit.
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let f: fn(Int64) -> Int64 = x -> x + s.byteLength; \
                   return f(1); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 3,
            "the copied capture, the block, and the binding itself: {ir}"
        );
    }

    // ---- RFC-0075: a stream's release runs on every exit path ---------------

    /// A producer whose buffer is a real heap allocation, so the release below is
    /// a real `free` rather than a no-op on a static.
    const FEED: &str = "fn feed() -> Stream<Int64> { let mut xs: Array<Int64> = [] \
                        xs.push(1) return fromArray(xs) }";

    /// The block a label introduces, up to the next label. Used instead of a bare
    /// free COUNT because a count cannot tell "released on the break path" from
    /// "released somewhere else in the function" — which is the entire question.
    /// The first label with this prefix. A prefix rather than the exact
    /// `fend.9`: the numbering moves whenever the emitter's label counter does,
    /// which it did in M2c when the loop and `pull` became one asker.
    fn label_like(ir: &str, prefix: &str) -> String {
        ir.lines()
            .find(|l| l.starts_with(prefix) && l.ends_with(':'))
            .map(|l| l.trim_end_matches(':').to_string())
            .unwrap_or_else(|| panic!("no `{prefix}` label in IR"))
    }

    fn block_at<'a>(ir: &'a str, label: &str) -> &'a str {
        let start = ir.find(&format!("\n{label}:\n")).expect("label not in IR");
        let rest = &ir[start + 1..];
        let end = rest[label.len() + 2..]
            .find("\n}")
            .map(|i| i + label.len() + 2)
            .unwrap_or(rest.len());
        &rest[..end]
    }

    /// Every stream release is one call to the variant-aware runtime helper
    /// (RFC-0075 M2b). Counting THESE rather than `free`s is the re-count M2's
    /// price list asked for: a buffer stream's `free` now happens inside the
    /// helper, so a `free` at the call site would mean something had gone back
    /// to reclaiming a stream as if it were an array.
    fn stream_closes(ir: &str) -> usize {
        ir.matches("call void @__vyrn_stream_close_").count()
    }

    #[test]
    fn stream_for_loop_releases_on_normal_exit() {
        let src =
            format!("{FEED} fn main() -> Int64 {{ for p in feed() {{ print(p) }} return 0 }}");
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            1,
            "the loop should release the stream exactly once: {ir}"
        );
        // And nothing frees a stream's buffer at the call site any more — which
        // of the two producers it holds is not knowable there. The only frees
        // beyond the runtime's are the element's own release function.
        assert_eq!(free_calls(&ir), RUNTIME_FREES + STREAM_CLOSER_FREES, "{ir}");
        // The release is at the loop's exit block, which is where the
        // zero-element, the exhausted-buffer and the `None` paths all land.
        assert!(
            block_at(&ir, &label_like(&ir, "fend")).contains("call void @__vyrn_stream_close_"),
            "the release belongs in the loop's end block: {ir}"
        );
    }

    #[test]
    fn stream_for_loop_releases_on_break() {
        // RFC-0060 made `break` drop what a normal iteration end would; the stream
        // is not in a drop frame of its own, so this checks the other half — that
        // `break` branches to the block holding the release rather than past it.
        let src = format!(
            "{FEED} fn main() -> Int64 {{ for p in feed() {{ print(p) break }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            1,
            "one release, on the one path out: {ir}"
        );
        let fend = label_like(&ir, "fend");
        let end = block_at(&ir, &fend);
        assert!(end.contains("call void @__vyrn_stream_close_"), "{ir}");
        assert!(
            ir.contains(&format!("br label %{fend}")),
            "`break` must branch to the releasing block: {ir}"
        );
    }

    #[test]
    fn stream_for_loop_releases_on_early_return() {
        // The discriminating count: a `return` from inside the body leaves through
        // `emit_all_drops`, which walks the loop-variable frame the stream was put
        // in — so the function carries TWO releases, one per exit path, and never
        // both on one path.
        let src = format!(
            "{FEED} fn main() -> Int64 {{ for p in feed() {{ if p == 1 {{ return 7 }} }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            2,
            "the early-return path needs its own release: {ir}"
        );
        // The basic block the early `ret` terminates, from its label onward.
        let upto = &ir[..ir.find("ret i64 7").expect("the early return is in the IR")];
        let then = &upto[upto.rfind(":\n").expect("a label precedes the early ret")..];
        assert!(
            then.contains("call void @__vyrn_stream_close_"),
            "the release must precede the early `ret`: {ir}"
        );
    }

    #[test]
    fn close_releases_a_stream_once() {
        let src = format!("{FEED} fn main() -> Int64 {{ let s = feed() close(s) return 0 }}");
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            1,
            "`close` releases it, and nothing releases it a second time: {ir}"
        );
        assert_eq!(free_calls(&ir), RUNTIME_FREES + STREAM_CLOSER_FREES, "{ir}");
    }

    #[test]
    fn a_stepped_stream_calls_its_producer_from_inside_the_loop() {
        // The milestone in one assertion. `fromStep` builds a producer-tagged
        // header and the loop dispatches through the step's signature, so the step
        // runs once per iteration — which is what makes an endless feed a program
        // rather than a hang. Under M1 there was no call at all: the buffer had to
        // exist before the loop started.
        let src = "fn tick(s: Int64, g: Int64, c: Bool) -> Option<Int64> { \
                   if c { return None } return Some(s) } \
                   fn main() -> Int64 { for v in fromStep(0, 1, tick) { print(v) break } return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            block_at(&ir, &label_like(&ir, "ncall"))
                .contains("call { i1, i64, i64 } @__vyrn_fndispatch_"),
            "the step must be called from inside the loop: {ir}"
        );
        assert_eq!(stream_closes(&ir), 1, "{ir}");
    }

    /// RFC-0075 M2's combinator, spelled locally: it takes the obligation in as a
    /// parameter, discharges it with its own `for … in`, and hands back a new
    /// stream. std/stream's `map`/`filter`/`take`/`merge` are all this shape.
    const TWICE: &str = "fn twice(s: Stream<Int64>) -> Stream<Int64> { \
                         let mut out: Array<Int64> = [] \
                         for x in s { out.push(x * 2) } return fromArray(out) }";

    #[test]
    fn stream_combinator_chain_releases_once_per_stream() {
        // Two streams exist — the one `feed` made and the one `twice` made — so
        // two releases, each in the function that owns its stream. Counted rather
        // than assumed: "it composes" is exactly the claim a hole hides behind.
        let src = format!(
            "{FEED} {TWICE} fn main() -> Int64 {{ for p in twice(feed()) {{ print(p) }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            2,
            "one release per stream, never two on one path: {ir}"
        );
    }

    #[test]
    fn stream_combinator_chain_releases_on_break_and_early_return() {
        // `break` out of the chain's consumer: still one release per stream.
        let src = format!(
            "{FEED} {TWICE} fn main() -> Int64 {{ for p in twice(feed()) \
             {{ print(p) break }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(stream_closes(&ir), 2, "break path: {ir}");

        // An early `return` leaves through `emit_all_drops`, so `main` carries a
        // second release — one per exit path, and the early one must precede the
        // `ret` rather than follow it into unreachable code.
        let src = format!(
            "{FEED} {TWICE} fn main() -> Int64 {{ for p in twice(feed()) \
             {{ if p == 2 {{ return 7 }} }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            3,
            "the early-return path needs its own release: {ir}"
        );
        // The basic block the early `ret` terminates, from its label onward.
        let upto = &ir[..ir.find("ret i64 7").expect("the early return is in the IR")];
        let blk = &upto[upto.rfind(":\n").expect("a label precedes the early ret")..];
        assert!(
            blk.contains("call void @__vyrn_stream_close_"),
            "the release must precede the early `ret`: {ir}"
        );
    }

    /// RFC-0075 M2c's combinator, spelled locally, as `std/stream`'s `map` is:
    /// no `for … in` at all, a step that reads its source with `pullAt`, and a
    /// wrapper that owns the box that source moved into.
    ///
    /// Since RFC-0090 M3 the step also RELEASES: `closing` is true exactly once,
    /// and the source comes back out of its box and is closed by an ordinary
    /// `close`. That is the walk M2c wrote inside the runtime, moved to where
    /// `movecheck` can see it. The cursor words are 0 here because this
    /// combinator keeps its state in the capture rather than in a slab —
    /// `std/stream` mints a real cursor, and a step that never reads one does
    /// not need it to be real.
    const LMAP: &str = "fn lmap(s: Stream<Int64>, f: fn(Int64) -> Int64) -> Stream<Int64> { \
                        let a = boxStream(s) \
                        let g: fn(Int64) -> Int64 = f \
                        let step: fn(Int64, Int64, Bool) -> Option<Int64> = (sl, gn, cl) -> { \
                        if cl { let src: Stream<Int64> = unboxStream(a) close(src) return None } \
                        let x: Option<Int64> = pullAt(a) \
                        if let Some(v) = x { return Some(g(v)) } return None } \
                        return fromStep(0, 0, step) }";

    #[test]
    fn a_lazy_combinator_releases_at_one_site_and_walks_the_rest() {
        // The M3 re-count, against M2c's table. An eager `map` had a `for … in`
        // of its own, so a chain of two streams was two release sites, one per
        // owning function. M2c made the second one a WALK inside the runtime, so
        // the count fell to one. RFC-0090 M3 puts it back at two, and the second
        // is better than the one it replaces: the walk is now `close(src)` in the
        // wrapper's own step, ordinary Vyrn that `movecheck` checks. A wrapper
        // that failed to close its source would not compile, where a walk that
        // stopped one stream early only leaked.
        //
        // Still one release per stream, counted at run time by
        // `examples/streamlazy.vyrn`: 30 000 cycles of a three-deep chain, whose
        // cursor slots come back to `std/stream`'s slab or do not.
        let src = format!(
            "{FEED} {LMAP} fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             fn main() -> Int64 {{ for p in lmap(feed(), double) {{ print(p) }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            2,
            "one release site per stream: the consumer's, and the wrapper step's own: {ir}"
        );
        // And `lmap` itself releases nothing — it has no stream to release, which
        // is the whole difference from the eager version. Its STEP does, and the
        // step is a different function.
        let body = ir
            .split("\ndefine ")
            .find(|d| d.lines().next().is_some_and(|l| l.contains("@vyrn_lmap")))
            .expect("lmap is in the IR");
        assert!(
            !body.contains("call void @__vyrn_stream_close_"),
            "a lazy combinator consumes nothing, so it releases nothing: {ir}"
        );
        // The wrapper's source lives in a box it holds the address of, and
        // nothing in the compiler holds it any more (RFC-0090 M3).
        assert!(
            ir.contains("call ptr @__vyrn_stream_box(i64"),
            "the source goes in a box the step reads: {ir}"
        );
    }

    #[test]
    fn a_lazy_chain_keeps_the_early_return_and_break_counts() {
        // M1's numbers, unchanged by laziness: `break` leaves through the block
        // that releases, and an early `return` carries its own release. Each
        // count is one higher than M2c's, and it is always the same one — the
        // step's `close(src)`, which no path through `main` can add or lose.
        let src = format!(
            "{FEED} {LMAP} fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             fn main() -> Int64 {{ for p in lmap(feed(), double) {{ print(p) break }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(stream_closes(&ir), 2, "break path: {ir}");

        let src = format!(
            "{FEED} {LMAP} fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             fn main() -> Int64 {{ for p in lmap(feed(), double) \
             {{ if p == 2 {{ return 7 }} }} return 0 }}"
        );
        let ir = emit(&check(&src).unwrap()).unwrap();
        assert_eq!(
            stream_closes(&ir),
            3,
            "the early-return path needs its own release: {ir}"
        );
    }

    #[test]
    fn pull_traps_on_an_address_with_no_stream_in_it() {
        // `pullAt` is a builtin and an address is an ordinary `Int64`, so nothing
        // stops a program from calling it on a number. The box's magic word is
        // what makes that a trap with the interpreter's own wording rather than a
        // read of whatever happens to be at that address.
        let src = "fn main() -> Int64 { let x: Option<Int64> = pullAt(24) return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vyrn_stream_box(i64"), "{ir}");
        assert!(ir.contains("call void @__vyrn_stream_nobox()"), "{ir}");
        assert!(
            ir.contains("error: no stream in this box"),
            "the wording is the interpreter's: {ir}"
        );
    }

    #[test]
    fn caller_frees_owned_transfer_result() {
        // `make` returns a fresh owned String; `main` must free the result it
        // receives, but `make` must NOT free what it moves out.
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let g = make(a, b); return g.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The runtime frees + exactly one auto-free (in `main`, for `g`).
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 1,
            "caller should free the owned result once: {ir}"
        );
    }

    #[test]
    fn region_brackets_body_with_enter_and_exit() {
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; let mut n = 0; \
                       region { let s = a + b; n = s.byteLength; } \
                       return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @__vyrn_region_enter()"), "{ir}");
        assert!(ir.contains("call void @__vyrn_region_exit()"), "{ir}");
        // concat routes through the arena at runtime.
        assert!(ir.contains("@__vyrn_region_alloc"), "{ir}");
        assert!(ir.contains("load i64, ptr @__vyrn_region_sp"), "{ir}");
    }

    // The runtime preamble contains a fixed number of trap sites; a validation
    // check is one *beyond* that baseline.
    /// Trap sites in `ir`. Phase 8d made a trap one call to the shared cold
    /// tail, so this counts the tail's call sites where it once counted `@exit`.
    fn exit_calls(ir: &str) -> usize {
        ir.matches("call void @__vyrn_trap_msg(").count()
    }
    fn exit_baseline() -> usize {
        exit_calls(&emit(&check("fn main() -> Int64 { return 0; }").unwrap()).unwrap())
    }

    #[test]
    fn const_construction_has_no_runtime_check() {
        // A compile-time-constant construction erases to the value (RFC-0003).
        let src = "type Age = Int64 where value >= 18; \
                   fn main() -> Int64 { let a = Age(25); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            exit_calls(&ir),
            exit_baseline(),
            "const construction should not emit a runtime check: {ir}"
        );
    }

    /// RFC-0090 phase 8d. A trap site is ONE call to a shared `noreturn cold`
    /// tail. It was three inline calls, and LLVM's inliner charges for each, so
    /// a guard no program takes made the function around it too expensive to
    /// inline — which is what `std/slots`' `place at` paid at every access.
    /// Both halves are pinned: the tail is shared, and it is marked cold.
    #[test]
    fn a_trap_site_is_one_cold_call() {
        let src = "fn pick(xs: Array<Int64>, i: Int64) -> Int64 { \
                   if i < 0 { panic(\"negative\") } \
                   return xs[i] } \
                   fn main() -> Int64 { return pick([1, 2], 0) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let body = {
            let s = ir
                .find("define i64 @vyrn_pick(")
                .expect("no `pick` in the IR");
            let e = s + ir[s..].find("\n}\n").expect("unterminated body");
            &ir[s..e]
        };
        // The index trap and the panic are each one call, and neither spells the
        // tail out where it stands.
        assert!(
            body.contains("call void @__vyrn_panic(")
                && body.contains("call void @__vyrn_trap_idx(ptr @.trap.aoob,"),
            "both traps must route through the shared tail:\n{body}"
        );
        for inline in ["@__vyrn_stderr", "@fprintf", "@fputs", "@exit"] {
            assert!(
                !body.contains(inline),
                "`{inline}` is emitted inline at a trap site again:\n{body}"
            );
        }
        // `cold` is what keeps the call out of the inliner's cost for the
        // caller; `noreturn` is what lets the block after it stay unreachable.
        for tail in ["@__vyrn_trap_msg", "@__vyrn_trap_idx", "@__vyrn_panic"] {
            let d = format!("define internal void {tail}(");
            let s = ir
                .find(&d)
                .unwrap_or_else(|| panic!("no `{tail}` definition"));
            let head = &ir[s..s + ir[s..].find('{').unwrap()];
            assert!(
                head.contains("noreturn") && head.contains("cold"),
                "`{tail}` must stay `noreturn cold`: {head}"
            );
        }
    }

    #[test]
    fn runtime_construction_emits_check() {
        // A non-constant construction (through a parameter) is checked at runtime.
        let src = "type Age = Int64 where value >= 18; \
                   fn mk(n: Int64) -> Age { return Age(n); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            exit_calls(&ir) > exit_baseline(),
            "expected a runtime check: {ir}"
        );
        assert!(ir.contains("@.trap.verr.Age"), "{ir}");
    }

    #[test]
    fn string_byte_length_reads_the_header() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s.byteLength; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // One load from the String header, not a scan (RFC-0089 M1a). Asserted
        // inside `@main` because the module's own runtime still scans raw C
        // strings, so a module-wide `strlen` search proves nothing.
        let body = &ir[ir.find("define i64 @vyrn_main").expect("main is emitted")..];
        let body = &body[..body
            .find(
                "
}",
            )
            .expect("main ends")];
        assert!(
            body.contains("call i64 @__vyrn_str_len"),
            "byteLength reads the header: {body}"
        );
        assert!(
            !body.contains("@__vyrn_strlen"),
            "and does not scan: {body}"
        );
    }

    // (`string_char_count_lowers_to_charcount_shim` pinned
    // `call i64 @__vyrn_charcount`, which is no longer emitted: RFC-0078's census
    // found `charCount` the one builtin with no justification for being one, and it
    // is `std/text`'s `charCountV`. Its witness moved to
    // `a_routed_builtin_without_its_module_refuses_by_name` with the other ten.
    // `string_byte_length_reads_the_header` above is the contrast that matters:
    // `byteLength` is a VIEW and stays.)

    #[test]
    fn string_index_lowers_to_byte_load() {
        // `s[i]` is a `UInt8` (RFC-0022): the byte loads as `i8` and stays
        // `i8` (no zero-extension) — an explicit `Int64(..)` is what widens it.
        let src = "fn main() -> Int64 { let s = \"hi\"; return Int64(s[0]); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("load i8"), "loads a byte: {ir}");
        assert!(
            ir.contains("zext i8") && ir.contains("to i64"),
            "Int64(..) widens: {ir}"
        );
        assert!(ir.contains("@.trap.soob"), "bounds-checked: {ir}");
    }

    /// RFC-0079 M3: `slice_lowers_with_both_traps` pinned the lowering that is
    /// gone — two trap globals, a continuation-byte mask at each cut point and a
    /// `@__vyrn_bytecopy` call. What replaces it is the ABSENCE, checked on a
    /// module that reaches the string runtime, because a deleted lowering whose
    /// dead globals stay behind is the shape RFC-0078 exists to catch.
    ///
    /// Since RFC-0094 M2 the refusal for `slice` is the ordinary one every moved
    /// name gets — the checker does not resolve it and says which module has it.
    /// `a_moved_builtin_names_the_module_it_moved_to` in `vyrn-frontend` is that
    /// half.
    #[test]
    fn the_slice_lowering_and_its_traps_are_gone() {
        let ir =
            emit(&check("fn main() -> Int64 { return bytes(\"hi\").length }").unwrap()).unwrap();
        assert!(
            !ir.contains("@.trap.sliceoob"),
            "the out-of-range trap survived: {ir}"
        );
        assert!(
            !ir.contains("@.trap.slicesplit"),
            "the mid-codepoint trap survived: {ir}"
        );
        assert!(
            !ir.contains("@__vyrn_bytecopy"),
            "the copy helper survived: {ir}"
        );
    }

    /// RFC-0078 M4c deleted the lowerings for `@__vyrn_hex_encode`,
    /// `@__vyrn_str_chars` and `@strstr`, and pinned the ABSENCE with a
    /// refusal-by-name at emit. RFC-0094 M2 moved that refusal one phase earlier
    /// for ten of the eleven: they are declarations now, so a bare source with no
    /// resolver does not resolve the NAME and never reaches the emitter.
    ///
    /// One row still routes. `s.charCount()` is method-only, so the AST call name
    /// is `@charCount`, which no import can bring into scope — and its seam is
    /// still the emitter's. Loudly rather than silently: an emitter that dropped
    /// the call, or emitted one to a function nobody defines, is the failure mode
    /// this pins against.
    #[test]
    fn the_one_routed_builtin_without_its_module_refuses_by_name() {
        let src = "fn main() -> Int64 { return \"hi\".charCount() }";
        let e = emit(&check(src).unwrap()).unwrap_err();
        assert!(
            e.contains("@charCount") && e.contains("text$charCountV"),
            "{e}"
        );
    }

    /// The ten that moved, refused at the CHECK instead — and the emitter is not
    /// reached, which is the point: there is no lowering left to drop.
    #[test]
    fn a_moved_builtin_never_reaches_the_emitter() {
        for (src, want) in [
            (
                "fn main() -> Int64 { let a = hexEncode(\"x\"); return 0 }",
                "`hexEncode` is `std/codecs`'s",
            ),
            (
                "fn main() -> Int64 { return chars(\"hi\").length }",
                "`chars` is `std/text`'s",
            ),
            (
                "fn f(s: String) -> Bool { return contains(s, \"x\") } \
                 fn main() -> Int64 { return 0 }",
                "`contains` is `std/strpred`'s",
            ),
        ] {
            let e = check(src).unwrap_err();
            assert!(e.contains(want), "{e}");
        }
    }

    /// `bytes` did NOT move, and neither did the UTF-8 validator it shares with
    /// `stringFromBytes` — both are the irreducible VIEW the Vyrn implementations are
    /// written on, so both are still emitted. The codecs' own IR, by contrast, is
    /// gone from the module rather than merely unreferenced.
    #[test]
    fn the_byte_view_and_the_validator_stay_in_the_runtime() {
        let ir =
            emit(&check("fn main() -> Int64 { return bytes(\"hi\").length }").unwrap()).unwrap();
        assert!(
            ir.contains("call { ptr, i64, i64 } @__vyrn_str_bytes"),
            "bytes → helper: {ir}"
        );
        assert!(
            ir.contains("@__vyrn_str_bytes(ptr %s)"),
            "helper emitted: {ir}"
        );
        assert!(ir.contains("@__vyrn_utf8valid"), "validator: {ir}");
        assert!(ir.contains("@__vyrn_utf8d = private"), "DFA table: {ir}");
        for dead in [
            "@__vyrn_hex_encode",
            "@__vyrn_b64_encode",
            "@__vyrn_url_encode",
            "@__vyrn_hex_decode",
            "@__vyrn_hexdigit",
            "@__vyrn_hexval",
            "@__vyrn_b64alpha",
            "@__vyrn_str_chars",
            "@strstr",
        ] {
            assert!(
                !ir.contains(dead),
                "`{dead}` should have gone with M4c: {ir}"
            );
        }
    }

    #[test]
    fn validated_string_runtime_check_reads_the_header() {
        // A non-constant String construction checks `value.byteLength` — a
        // header load since RFC-0089 M1a — and traps through the same
        // validation-error path.
        let src = "type Name = String where value.byteLength >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("call i64 @__vyrn_str_len"),
            "refinement reads the header: {ir}"
        );
        assert!(ir.contains("@.trap.verr.Name"), "refinement traps: {ir}");
    }

    #[test]
    fn cross_field_record_emits_runtime_check() {
        let src = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.trap.verr.R"), "cross-field traps: {ir}");
    }

    #[test]
    fn regex_match_lowers_to_dfa_runner() {
        let src = "fn f(s: String) -> Bool { return s =~ \"[a-z]+\"; } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("call i1 @__vyrn_regex_run"),
            "calls the runner: {ir}"
        );
        assert!(
            ir.contains("@.rx.0.table"),
            "emits a transition table: {ir}"
        );
        assert!(
            ir.contains("@.rx.0.accept"),
            "emits an accepting array: {ir}"
        );
    }

    #[test]
    fn option_match_lowers_to_aggregate_and_phi() {
        let src = "fn f() -> Option<Int64> { return Some(7); } \
                   fn main() -> Int64 { return match f() { Some(x) => x, None => 0 }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("insertvalue { i1, i64, i64 }"),
            "Some should build an aggregate: {ir}"
        );
        assert!(
            ir.contains("extractvalue { i1, i64, i64 }"),
            "match should extract: {ir}"
        );
        assert!(
            ir.contains("phi i64"),
            "match should merge with a phi: {ir}"
        );
    }

    #[test]
    fn enum_array_payload_boxes_growable_triple() {
        // RFC-0026 regression: an `Array<T>` payload is a fat `{ptr,len,cap}`
        // value, three words wide. The array *literal* is a fixed `[N x T]`, so
        // construction must reshape it into the growable triple before boxing
        // the payload — otherwise `match` unboxes the raw elements as a header
        // and the length is garbage. The tell is that the boxed payload is a
        // `{ ptr, i64, i64 }` triple (built via the ArrayN→Array copy), and the
        // arm loads one back.
        let src = "type R = | A(Int64) | B(Array<Int64>); \
                   fn mk() -> R { return B([1, 2, 3]); } \
                   fn main() -> Int64 { return match mk() { A(n) => n, B(xs) => xs.length }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("store { ptr, i64, i64 }"),
            "the boxed Array payload must be the growable triple, not the raw \
             `[N x T]` literal:\n{ir}"
        );
        assert!(
            ir.contains("load { ptr, i64, i64 }"),
            "match must unboxStream the payload as the growable triple:\n{ir}"
        );
    }

    #[test]
    fn result_array_payload_is_boxed_in_the_target_representation() {
        // RFC-0026 regression for the built-in sum types: `Ok([..])` writes an
        // array literal, a fixed `[N x T]` value, into a `Result` whose declared
        // payload is the growable `Array<T>`. Box the literal as it stands and
        // `match` decodes it at the wrong width — the raw elements read as a
        // `{ptr,len,cap}` header.
        //
        // This used to be repaired after the fact, by branching on the tag at the
        // coercion into the return type and re-materializing the arm
        // (`rebox_sum`). RFC-0082 made the constructor coerce its payload into
        // the expected type before boxing — it had to, so that a validated
        // payload runs its predicate — and that reshapes the literal at the
        // source, so the repair became unreachable and went. The invariant is the
        // same one and the tell is the same one `enum_array_payload_boxes_
        // growable_triple` uses: the boxed payload is the growable triple, and
        // the arm loads one back. The absence of the branch is asserted too,
        // because a `rebox` reappearing would mean construction had stopped
        // reshaping and something downstream was papering over it.
        let src = "fn load() -> Result<Array<Int64>, String> { return Ok([1, 2, 3]); } \
                   fn main() -> Int64 { return match load() { Ok(xs) => xs.length, Err(e) => 0 }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("store { ptr, i64, i64 }"),
            "the boxed Ok payload must be the growable triple, not the raw \
             `[N x T]` literal:\n{ir}"
        );
        assert!(
            ir.contains("load { ptr, i64, i64 }"),
            "the Ok arm must unboxStream the payload as the growable triple:\n{ir}"
        );
        assert!(
            !ir.contains("rebox."),
            "the payload is reshaped at construction, not repaired after it:\n{ir}"
        );
    }

    #[test]
    fn generic_record_monomorphizes_by_layout() {
        let src = "type Box<T> = { value: T }; \
                   fn main() -> Int64 { let a = Box { value: 5 }; let b = Box { value: true }; \
                                      if b.value { return a.value; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("insertvalue { i64 }"),
            "Box<Int64> is a 1x i64 struct:\n{ir}"
        );
        assert!(
            ir.contains("insertvalue { i1 }"),
            "Box<Bool> is a 1x i1 struct:\n{ir}"
        );
    }

    #[test]
    fn generic_monomorphizes_per_type() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn main() -> Int64 { print(id(\"s\")); return id(1); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("define i64 @vyrn_id__Int"),
            "Int64 instance:\n{ir}"
        );
        assert!(
            ir.contains("define ptr @vyrn_id__Str"),
            "Str instance:\n{ir}"
        );
        assert!(
            !ir.contains("@vyrn_id("),
            "no un-instantiated generic body:\n{ir}"
        );
    }

    // ---- the symbol is an identity, not a label -------------------------

    /// One of every shape [`mangle_ty`] has an arm for, plus the names a user
    /// can write to imitate one.
    ///
    /// The imitations are the point. Nothing stops a program declaring
    /// `type OptInt64 = { a: Int64 }`, `type ArrInt64`, `type Rec`, `type Enum`,
    /// `type Xf`, or `type Int8Int64` — and each of those is the exact string a
    /// built-in constructor produces, so each is a pair the readable mangle
    /// cannot tell apart. The punctuated names are `sanitize`'s own collapse:
    /// it maps every non-alphanumeric to `_`, so three distinct module-qualified
    /// names arrive as one.
    fn seed_types() -> Vec<Type> {
        let f = |n: &str, t: Type| Field {
            name: n.to_string(),
            ty: t,
        };
        vec![
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: true,
            },
            Type::IntN {
                bits: 8,
                signed: false,
            },
            Type::IntN {
                bits: 64,
                signed: false,
            },
            Type::Float,
            Type::Float32,
            Type::Bool,
            Type::Str,
            Type::Unit,
            Type::Logger,
            Type::F32x4,
            Type::I32x4,
            Type::ConstInt(4),
            Type::ConstInt(8),
            // The imitations.
            Type::Named("OptInt64".into()),
            Type::Named("ArrInt64".into()),
            Type::Named("ResInt64Str".into()),
            Type::Named("Int8Int64".into()),
            Type::Named("Rec".into()),
            Type::Named("Enum".into()),
            Type::Named("Xf".into()),
            Type::Named("Arr4Int64".into()),
            Type::Named("a.b".into()),
            Type::Named("a_b".into()),
            Type::Named("a$b".into()),
            // Protocol-bounded parameters mangle as their own name, and a user
            // type may be spelled like one.
            Type::Param("T".into()),
            Type::Param("U".into()),
            Type::Named("T".into()),
            // Structural shapes, which the readable mangle collapses whole.
            Type::Record(vec![]),
            Type::Record(vec![f("a", Type::Int)]),
            Type::Record(vec![f("b", Type::Int)]),
            Type::Record(vec![f("a", Type::Str)]),
            Type::Record(vec![f("a", Type::Int), f("b", Type::Int)]),
            Type::Enum(vec![]),
            Type::Enum(vec![EnumVariant {
                name: "A".into(),
                payload: vec![],
            }]),
            Type::Enum(vec![EnumVariant {
                name: "A".into(),
                payload: vec![Type::Int],
            }]),
            Type::Omit(Box::new(Type::Named("R".into())), vec!["a".into()]),
            Type::Omit(Box::new(Type::Named("R".into())), vec!["b".into()]),
            Type::Pick(Box::new(Type::Named("R".into())), vec!["a".into()]),
            Type::Partial(Box::new(Type::Named("R".into()))),
            Type::Merge(
                Box::new(Type::Named("R".into())),
                Box::new(Type::Named("S".into())),
            ),
        ]
    }

    /// Every composite shape, over the types it is given: containers, both
    /// generic applications, function types of three arities, and the sized
    /// containers at two capacities.
    fn grow(base: &[Type], pairs: &[Type]) -> Vec<Type> {
        let mut out = Vec::new();
        for t in base {
            let b = || Box::new(t.clone());
            out.extend([
                Type::Option(b()),
                Type::Array(b()),
                Type::Stream(b()),
                Type::Task(b()),
                Type::Lazy(b()),
                Type::ArrayN(b(), 4),
                Type::ArrayN(b(), 8),
                Type::SmallArray(b(), 4),
                Type::SmallArray(b(), 8),
                Type::App("P".into(), vec![t.clone()]),
                Type::App("Q".into(), vec![t.clone()]),
                Type::Fn(vec![], b()),
                Type::Fn(vec![t.clone()], Box::new(Type::Unit)),
            ]);
        }
        for a in pairs {
            for c in pairs {
                out.extend([
                    Type::Result(Box::new(a.clone()), Box::new(c.clone())),
                    Type::Map(Box::new(a.clone()), Box::new(c.clone())),
                    Type::App("P".into(), vec![a.clone(), c.clone()]),
                    Type::Fn(vec![a.clone(), c.clone()], Box::new(Type::Unit)),
                    Type::Fn(vec![a.clone()], Box::new(c.clone())),
                ]);
            }
        }
        out
    }

    /// No two distinct types produce one instantiation symbol.
    ///
    /// The claim [`struct_key`] exists for, checked over generated type trees
    /// rather than the handful of pairs anyone thought to write down: every
    /// shape the mangle has an arm for, one level of nesting over all of them
    /// and two levels over a slice, generic applications at both arities, and
    /// user-declared names spelled exactly like the strings the built-in
    /// constructors produce. A bucket holding two distinct types is the defect
    /// — the driver dedups on this string, so it would emit one body and call it
    /// from both sites.
    ///
    /// The arity rows are separate because a symbol carries a LIST of type
    /// arguments and the readable half joins them with a separator `sanitize`
    /// can also produce.
    #[test]
    fn a_mangled_symbol_is_injective_over_generated_types() {
        let seeds = seed_types();
        let pairs = &seeds[..8];
        let d1 = grow(&seeds, pairs);
        let d2 = grow(&d1[..40], &d1[..6]);
        let universe: Vec<Type> = seeds
            .iter()
            .chain(d1.iter())
            .chain(d2.iter())
            .cloned()
            .collect();

        // The hazard is real before it is ruled out: if no two members of the
        // universe shared a readable mangle, the rows below would pass on the
        // unfixed code and prove nothing.
        assert_eq!(
            mangle_ty(&Type::Option(Box::new(Type::Int))),
            mangle_ty(&Type::Named("OptInt64".into())),
            "the generator no longer covers a pair the readable mangle collapses"
        );

        // Every argument LIST a symbol can stand for: one per type, and — for
        // arity, which the readable half joins with a separator `sanitize` can
        // also produce — every ordered pair over a slice of them.
        let mut lists: Vec<Vec<Type>> = universe.iter().map(|t| vec![t.clone()]).collect();
        for a in &universe[..60] {
            for b in &universe[..60] {
                lists.push(vec![a.clone(), b.clone()]);
            }
        }

        let mut seen: HashMap<String, Vec<Type>> = HashMap::new();
        for args in &lists {
            let sym = mangle_name("f", args);
            if let Some(prev) = seen.insert(sym.clone(), args.clone()) {
                assert_eq!(
                    &prev, args,
                    "two distinct instantiations share the symbol `{sym}`: the \
                     driver emits one body and calls it from both sites"
                );
            }
        }
        assert!(
            lists.len() > 5_000,
            "only {} symbols generated; the coverage shrank",
            lists.len()
        );
    }

    #[test]
    fn string_lowers_to_global_and_strcmp() {
        let src = "fn main() -> Int64 { if \"a\" == \"a\" { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.str.0 = private"), "string global:\n{ir}");
        assert!(ir.contains("call i32 @strcmp"), "== uses strcmp:\n{ir}");
    }

    #[test]
    fn string_ordering_lowers_to_strcmp_sign() {
        // RFC-0022: `<` on Strings is `strcmp(..) slt 0` (byte-wise sign test).
        let src = "fn main() -> Int64 { if \"a\" < \"b\" { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("call i32 @strcmp"),
            "ordering uses strcmp:\n{ir}"
        );
        assert!(
            ir.contains("icmp slt i32"),
            "signed sign-test against 0:\n{ir}"
        );
    }

    #[test]
    fn enum_match_lowers_to_switch() {
        let src = "type E = | A(Int64) | B(Int64) | C; \
                   fn f(e: E) -> Int64 { return match e { A(x) => x, B(y) => y, C => 0 }; } \
                   fn main() -> Int64 { return f(A(5)); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("switch i64"), "enum match uses a switch:\n{ir}");
        assert!(
            ir.contains("@vyrn_f({ i64, i64 }"),
            "enum lowers to a 2-word aggregate:\n{ir}"
        );
        assert!(
            ir.contains("insertvalue { i64, i64 } undef, i64 0"),
            "variant A has tag 0:\n{ir}"
        );
    }

    #[test]
    fn omit_transformer_lowers_to_narrower_struct() {
        let src =
            "type User = { id: Int64, name: Int64, pw: Int64 }; type Public = Omit<User, pw>; \
                   fn f(p: Public) -> Int64 { return p.name; } \
                   fn main() -> Int64 { let u = User { id: 1, name: 2, pw: 3 }; return f(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Public resolves to a 2-field struct; User is 3 fields; coercion happens.
        assert!(ir.contains("@vyrn_f({ i64, i64 }"), "Public layout: {ir}");
        assert!(
            ir.contains("insertvalue { i64, i64, i64 }"),
            "User is 3 fields: {ir}"
        );
    }

    #[test]
    fn record_width_subtyping_coerces() {
        let src = "type Named = { name: Int64 }; type User = { name: Int64, age: Int64 }; \
                   fn greet(w: Named) -> Int64 { return w.name; } \
                   fn main() -> Int64 { let u = User { name: 7, age: 30 }; return greet(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // greet takes a 1-field record; User is a 2-field record.
        assert!(
            ir.contains("@vyrn_greet({ i64 }"),
            "greet param layout: {ir}"
        );
        assert!(
            ir.contains("insertvalue { i64, i64 }"),
            "User is built: {ir}"
        );
        // width-subtyping coercion: rebuild a { i64 } from the User's `name`.
        assert!(
            ir.contains("insertvalue { i64 } undef"),
            "coercion to Named: {ir}"
        );
        // field access lowers to extractvalue.
        assert!(ir.contains("extractvalue { i64 }"), "field access: {ir}");
    }

    #[test]
    fn question_mark_lowers_to_early_return() {
        let src = "fn f() -> Result<Int64, Int64> { return Ok(1); } \
                   fn g() -> Result<Int64, Int64> { let x = f()?; return Ok(x); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // `?` tests the tag and returns the aggregate on the propagate path.
        assert!(
            ir.contains("try.prop"),
            "? should have a propagate block: {ir}"
        );
        assert!(
            ir.contains("ret { i1, i64, i64 }"),
            "? should propagate the aggregate: {ir}"
        );
    }

    #[test]
    fn question_mark_frees_owned_locals_on_propagate() {
        // `s` is an owned, non-escaping heap string alive across the `?`; the
        // propagate path must free it exactly like `return` does (previously
        // it leaked every owned local on the early exit).
        let src = "fn f() -> Result<Int64, Int64> { return Ok(1); } \
                   fn g() -> Result<Int64, Int64> { \
                       let s = \"a\" + \"b\"; \
                       let x = f()?; \
                       let n = s.byteLength; \
                       return Ok(x + n); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let prop = ir.find("try.prop").expect("propagate block present");
        let ret = prop
            + ir[prop..]
                .find("ret { i1, i64, i64 }")
                .expect("propagate returns");
        assert!(
            ir[prop..ret].contains("call void @__vyrn_str_free(ptr"),
            "owned string must be freed on the propagate path:\n{}",
            &ir[prop..ret]
        );
    }

    #[test]
    fn region_enter_traps_past_the_nesting_limit() {
        // The arena stack is a fixed [REGION_MAX x ptr]; entering one region
        // past it must trap (stderr + exit 1), not write past the global.
        //
        // Read from the constant, never spelled: this file used to write the
        // number in five places and the test in two more, and a limit a test
        // pins by hand is a limit the test can outlive.
        let n = vyrn_frontend::interp::REGION_MAX;
        let src = "fn main() -> Int64 { region { } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains(&format!("error: region nesting exceeds {n}")),
            "region-depth trap message present: {ir}"
        );
        assert!(
            ir.contains(&format!("%over = icmp uge i64 %sp, {n}")),
            "region_enter bounds-checks the stack pointer: {ir}"
        );
    }

    #[test]
    fn extern_fn_emits_wasm_import_declaration() {
        // RFC-0012: a body-less `extern fn` becomes a `declare` carrying the
        // wasm-import attributes (namespace `vyrn`, field = the Vyrn name) on
        // the prefixed symbol; a String parameter flattens to a (ptr, i64)
        // pair; the call site passes the pointer plus a computed length.
        let src = "extern fn jsLog(msg: String) \
                   extern fn jsAdd(a: Int64, b: Int64) -> Int64 \
                   fn main() -> Int64 { jsLog(\"hi\"); return jsAdd(1, 2); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("declare void @__vyrn_extern_jsLog(ptr, i64)"),
            "String param flattens to (ptr, i64): {ir}"
        );
        assert!(
            ir.contains("declare i64 @__vyrn_extern_jsAdd(i64, i64)"),
            "scalar extern declared with ABI types: {ir}"
        );
        assert!(
            ir.contains("\"wasm-import-module\"=\"vyrn\"")
                && ir.contains("\"wasm-import-name\"=\"jsLog\""),
            "wasm import attributes present: {ir}"
        );
        assert!(
            ir.contains("call i64 @__vyrn_extern_jsAdd(i64 1, i64 2)"),
            "extern call emitted at the use site: {ir}"
        );
    }

    #[test]
    fn export_extern_emits_a_normal_define_with_the_export_attribute() {
        // RFC-0012 M2: an `export extern fn` is a normal `define` under the
        // internal `vyrn_<name>` symbol, carrying an inline `wasm-export-name`
        // attribute so wasm-ld exports it under the bare Vyrn name. A `String`
        // parameter is a SINGLE `ptr` (not the import's (ptr,len) pair) — the JS
        // caller allocates the buffer, so decode-side length is a NUL scan.
        let src = "export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b } \
                   export extern fn greet(name: String) -> String { return name.copy() } \
                   fn main() -> Int64 { return vyrnAdd(1, 2) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains(
                "define i64 @vyrn_vyrnAdd(i64 %arg0, i64 %arg1) \"wasm-export-name\"=\"vyrnAdd\" {"
            ),
            "scalar export extern is a normal define with the export attr: {ir}"
        );
        assert!(
            ir.contains("define ptr @vyrn_greet(ptr %arg0) \"wasm-export-name\"=\"greet\" {"),
            "String param/return are single ptrs; export attr present: {ir}"
        );
        // It is NOT a body-less import: no declare, no import attributes for it.
        assert!(
            !ir.contains("@__vyrn_extern_vyrnAdd"),
            "an export extern is not a wasm import: {ir}"
        );
        // A plain fn keeps no export attribute.
        assert!(
            ir.contains("define i64 @vyrn_main(")
                && !ir.contains("@vyrn_main() \"wasm-export-name\""),
            "a plain fn is not exported: {ir}"
        );
    }

    #[test]
    fn mut_array_is_auto_freed() {
        // No explicit `drop`, yet the non-escaping mutable array is freed at
        // scope end (inferred by the ownership analysis).
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[0]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("call void @__vyrn_free(ptr"),
            "expected an automatic free: {ir}"
        );
    }

    #[test]
    fn option_holds_a_two_word_payload_inline() {
        // A stored `fn` value (RFC-0037) is `{ i64 tag, i64 captures }` and fits
        // inline in the widened Option aggregate — no box. `Ref` was the other
        // two-word case until RFC-0090 M4 deleted it, so this is the whole of
        // what `payload_boxed` still answers `false` for above one word.
        let src = "type Bump = fn(Int64) -> Int64
                   fn main() -> Int64 { let n = 7 let f: Bump = x -> x + n
                   let o: Option<Bump> = Some(f)
                   return match o { Some(g) => g(1), None => 0 } }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The Option is the three-word aggregate, and the fn value is rebuilt
        // inline on match (insertvalue into { i64, i64 }) rather than loaded from
        // a box.
        assert!(
            ir.contains("insertvalue { i1, i64, i64 }"),
            "widened aggregate: {ir}"
        );
        assert!(
            ir.contains("insertvalue { i64, i64 }"),
            "fn value rebuilt inline: {ir}"
        );
    }

    #[test]
    fn bool_returning_call_is_typed_i1() {
        // Regression: a call to a Bool-returning function must be typed i1 at the
        // call site (not i64), or branching on it produces invalid IR.
        let src = "fn t() -> Bool { return true; } \
                   fn main() -> Int64 { if t() { return 1; } return 0; }";
        let program = check(src).unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("call i1 @vyrn_t()"), "{ir}");
        // and the branch consumes an i1, never an i64 call result
        assert!(!ir.contains("call i64 @vyrn_t()"), "{ir}");
    }

    // ---- in-place array mutation (RFC-0011) -----------------------------

    #[test]
    fn index_store_emits_bounds_check_and_store() {
        // `a[i] = v` is the read path's bounds check plus a `getelementptr`+`store`
        // into the shared buffer; it reuses the array OOB trap global.
        let src =
            "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[1] = 9; return a[1]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@.trap.aoob"),
            "reuses the array OOB trap: {ir}"
        );
        assert!(ir.contains("icmp uge i64"), "bounds compare: {ir}");
        assert!(ir.contains("store i64 9"), "element store: {ir}");
    }

    #[test]
    fn index_store_validated_element_emits_check() {
        // A dynamic value stored into an `Array<Age>` element validates inline.
        let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut a: Array<Age> = [Age(20)]; \
                   let mut n = 30; a[0] = n; return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@.trap.verr.Age"),
            "element store validates: {ir}"
        );
    }

    #[test]
    fn pop_emits_none_some_branches_and_writeback() {
        // `pop` len-checks, builds a None/Some aggregate via a phi, and writes
        // the decremented header back to the array slot.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let p = match a.pop() { Some(x) => x, None => -1 }; return p; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("phi { i1, i64, i64 }"), "None/Some merge: {ir}");
        assert!(
            ir.contains("insertvalue { ptr, i64, i64 }"),
            "header write-back: {ir}"
        );
        assert!(ir.contains("sub i64"), "length decrement: {ir}");
    }

    #[test]
    fn swapremove_emits_bounds_check_and_swap() {
        // `swapRemove` bounds-checks, loads element i (the result), moves the last
        // element into slot i, and writes the shrunk header back.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g = a.swapRemove(0); return g; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@.trap.aoob"),
            "reuses the array OOB trap: {ir}"
        );
        assert!(
            ir.contains("insertvalue { ptr, i64, i64 }"),
            "header write-back: {ir}"
        );
        assert!(ir.contains("sub i64"), "length decrement: {ir}");
    }

    // ---- module state (RFC-0013) ---------------------------------------

    #[test]
    fn globals_emit_declaration_and_init_before_main() {
        let src = "let mut hits: Int64 = 0 \
                   let banner = \"hi\" \
                   fn bump() -> Int64 { hits = hits + 1 return hits } \
                   fn main() -> Int64 { return bump() }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // One internal global per binding, zero-initialized.
        assert!(
            ir.contains("@g.hits = internal global i64 zeroinitializer"),
            "{ir}"
        );
        assert!(
            ir.contains("@g.banner = internal global ptr zeroinitializer"),
            "{ir}"
        );
        // A synthesized init function, called from `vyrn_entry` before main.
        assert!(
            ir.contains("define internal void @__vyrn_globals_init()"),
            "{ir}"
        );
        let init_at = ir
            .find("call void @__vyrn_globals_init()")
            .expect("init call");
        let main_at = ir.find("call i64 @vyrn_main()").expect("main call");
        assert!(init_at < main_at, "init must run before main");
        // Reads and writes go through the global.
        assert!(
            ir.contains("load i64, ptr @g.hits"),
            "read through global: {ir}"
        );
        assert!(ir.contains("store i64 %"), "write through global: {ir}");
    }

    #[test]
    fn validated_global_store_emits_inline_validation() {
        // A non-constant store into a validated global runs the predicate inline
        // and traps through the per-type message.
        let src = "type Age = Int64 where value >= 18 \
                   let mut a: Age = Age(20) \
                   fn setAge(n: Int64) -> Int64 { a = n return 0 } \
                   fn main() -> Int64 { return setAge(30) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@.trap.verr.Age"),
            "per-type validation trap: {ir}"
        );
        assert!(
            ir.contains("store i64 %") && ir.contains("@g.a"),
            "store through global: {ir}"
        );
    }

    // ---- RFC-0020 M1: interpolation containment erases the runtime check ----

    #[test]
    fn proven_interpolation_emits_no_validation() {
        // `"nav.\{s}.label"` with s: Section is provably a TransKey, so the
        // containment proof erases the runtime validation entirely — no per-type
        // trap for TransKey is emitted at the `t(..)` argument boundary.
        let src =
            "type TransKey = String where value =~ \"nav\\\\.(home|about|settings)\\\\.label\" \
                   type Section = String where value =~ \"home|about|settings\" \
                   fn t(key: TransKey) -> Int64 { return 0 } \
                   fn main() -> Int64 { let s: Section = \"home\" return t(\"nav.\\{s}.label\") }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The per-type message global is always defined; a *check* is an `fputs`
        // of it in a trap block. A proven flow emits none.
        assert!(
            !ir.contains("@.trap.verr.TransKey, ptr"),
            "proven interpolation must emit NO TransKey validation: {ir}"
        );
    }

    #[test]
    fn nonfinite_hole_interpolation_still_validates_at_runtime() {
        // A plain-String hole is not finite, so containment does not apply and
        // the ordinary runtime validation for TransKey IS emitted.
        let src =
            "type TransKey = String where value =~ \"nav\\\\.(home|about|settings)\\\\.label\" \
                   fn t(key: TransKey) -> Int64 { return 0 } \
                   fn build(x: String) -> Int64 { return t(\"nav.\\{x}.label\") } \
                   fn main() -> Int64 { return build(\"home\") }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@__vyrn_trap_msg(ptr @.trap.verr.TransKey)"),
            "a non-finite hole must keep the runtime validation: {ir}"
        );
    }

    #[test]
    fn finite_var_contained_emits_no_validation() {
        // A Narrow value flowing into a Wide param where L(Narrow) ⊆ L(Wide) is
        // proven — no runtime check emitted.
        let src = "type Wide = String where value =~ \"a|b|c\" \
                   type Narrow = String where value =~ \"a|b\" \
                   fn want(x: Wide) -> Int64 { return 0 } \
                   fn main() -> Int64 { let n: Narrow = \"a\" return want(n) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            !ir.contains("@.trap.verr.Wide, ptr"),
            "contained finite var needs no check: {ir}"
        );
    }

    /// A read of `Array<T, N>` indexes the storage the receiver already has.
    /// It used to copy all N elements to a fresh slot first, because
    /// `getelementptr` cannot index an SSA aggregate by a dynamic index — 128
    /// bytes per read at N = 16, and 8x the whole loop. A receiver with no
    /// address keeps the copy, which is the value form's own cost.
    #[test]
    fn a_fixed_array_read_indexes_the_receivers_own_storage() {
        let src = "type Cells = { at: Array<Int64, 4> } \
                   fn mk() -> Array<Int64, 4> { return [1, 2, 3, 4] } \
                   fn local(i: Int64) -> Int64 { let a: Array<Int64, 4> = [1, 2, 3, 4] return a[i] } \
                   fn field(i: Int64) -> Int64 { let c = Cells { at: [1, 2, 3, 4] } return c.at[i] } \
                   fn call(i: Int64) -> Int64 { return mk()[i] } \
                   fn main() -> Int64 { return local(0) + field(1) + call(2) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let stores = ir.matches("store [4 x i64] ").count();
        // Two: the literal that initializes `a`, and the one spill `call` still
        // needs. `field`'s literal is stored as part of the record and `mk`
        // returns its value, so neither is one of these.
        assert_eq!(stores, 2, "no aggregate store may be per-read: {ir}");
        assert!(
            ir.contains("getelementptr [4 x i64], ptr %a.addr"),
            "a binding is indexed through its own slot: {ir}"
        );
        assert!(
            ir.contains("getelementptr [4 x i64], ptr %spill"),
            "a receiver with no address is still spilled once: {ir}"
        );
    }

    // ---- SmallArray<T, N> (RFC-0056) --------------------------------------

    #[test]
    fn smallarray_lowers_to_inline_struct() {
        // The type lowers to `{ i64 len, i64 cap, ptr data, [N x T] inline }`.
        let src = "fn main() -> Int64 { let mut xs: SmallArray<Int64, 4> = []  \
                   xs.push(1)  return xs.length }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("{ i64, i64, ptr, [4 x i64] }"),
            "SmallArray struct layout: {ir}"
        );
    }

    #[test]
    fn smallarray_push_has_the_spill_path() {
        // A push must branch on fullness and, from the inline state, allocate a
        // fresh heap buffer and `memcpy` the inline slots into it.
        let src = "fn main() -> Int64 { let mut xs: SmallArray<Int64, 4> = []  \
                   xs.push(1)  return xs.length }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("sapush.grow"), "spill/grow branch: {ir}");
        assert!(
            ir.contains("call ptr @__vyrn_malloc"),
            "inline spill allocates: {ir}"
        );
        assert!(
            ir.contains("@llvm.memcpy.p0.p0.i64"),
            "spill copies inline slots: {ir}"
        );
        assert!(
            ir.contains("call ptr @__vyrn_realloc"),
            "spilled grow reallocs: {ir}"
        );
    }

    #[test]
    fn smallarray_drop_frees_the_data_field_once() {
        // `drop xs` frees the SmallArray's `data` pointer (byte offset 16),
        // which is null while inline (a no-op) and heap once spilled — exactly
        // one auto-free beyond the runtime baseline, in either state.
        let src = "fn main() -> Int64 { let mut xs: SmallArray<Int64, 4> = []  \
                   xs.push(1)  let n = xs.length  drop xs  return n }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 1,
            "a SmallArray drop frees its data field exactly once: {ir}"
        );
        assert!(
            ir.contains("getelementptr i8, ptr %xs"),
            "drop addresses the data field by byte offset: {ir}"
        );
    }

    #[test]
    fn smallarray_auto_drop_at_scope_end_balances() {
        // No explicit `drop`: a SmallArray received from an owned-returning
        // function is reclaimed once at scope end (ownership schedules
        // `FreeSmallArr`) — the same discipline as an owned `Array`.
        let src = "fn make() -> SmallArray<Int64, 4> { \
                     let mut xs: SmallArray<Int64, 4> = []  xs.push(1)  return xs } \
                   fn main() -> Int64 { let xs = make()  return xs.length }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 1,
            "auto-drop frees the SmallArray data once: {ir}"
        );
    }

    #[test]
    fn coexisting_smallarray_capacities_lower_distinctly() {
        // `SmallArray<Int64, 4>` and `SmallArray<Int64, 8>` are separate types,
        // so both inline aggregate shapes appear in the module.
        let src = "fn main() -> Int64 { \
                   let mut a: SmallArray<Int64, 4> = []  a.push(1)  \
                   let mut b: SmallArray<Int64, 8> = []  b.push(2)  \
                   let n = a.length + b.length  drop a  drop b  return n }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("[4 x i64]"), "N=4 inline buffer: {ir}");
        assert!(ir.contains("[8 x i64]"), "N=8 inline buffer: {ir}");
        // Two SmallArray drops → two auto-frees beyond the runtime baseline.
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES + 2,
            "two drops, two frees: {ir}"
        );
    }

    #[test]
    fn push_rereads_the_header_after_the_element_expression() {
        // `a.push(takeLast(a))`: the element expression `modify`-mutates the
        // receiver (takeLast pops), so the push must read the header AFTER it
        // ran — the interpreter reads the live slot, and native must publish
        // the same len. The old order snapshotted the header first and grew
        // off the stale length.
        let src = "fn takeLast(xs: modify Array<Int64>) -> Int64 { \
                   return match xs.pop() { Some(x) => x, None => 0 } } \
                   fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30] \
                   a.push(takeLast(a)) return a.length }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let start = ir.find("define i64 @vyrn_main(").expect("main present");
        let body = &ir[start..];
        let elem = body.find("@vyrn_takeLast").expect("element call emitted");
        assert!(
            body[elem..].contains("load { ptr, i64, i64 }, ptr %a.addr"),
            "the header is re-read from the binding's slot after the element \
             expression ran:\n{body}"
        );
    }

    #[test]
    fn lifted_lambda_does_not_bake_in_the_enclosing_region() {
        // A lifted lambda is a function of its own: even when it is written
        // lexically inside `region { .. }`, its String allocations must route
        // to malloc, not bake in the arena at lift time — the arena dies with
        // the region, and (since the checker now refuses storing region-heap
        // values into bindings that outlive one) nothing pins the lambda's
        // lifetime to the region it was written in.
        let src = "fn main() -> Int64 { \
                   region { let g: fn(Int64) -> String = y -> \"b\" + y.toString() \
                   print(g(1)) } return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let mut checked = 0;
        let mut rest = ir.as_str();
        while let Some(pos) = rest.find("define") {
            let end = rest[pos..]
                .find("\n}\n")
                .map(|e| pos + e)
                .unwrap_or(rest.len());
            let def = &rest[pos..end];
            if def.contains("@__vyrn_lambda_") && def.contains("@__vyrn_str_concat") {
                checked += 1;
                assert!(
                    !def.contains("@__vyrn_region_alloc"),
                    "lambda baked in arena routing:\n{def}"
                );
                assert!(def.contains("@__vyrn_malloc"), "heap allocation:\n{def}");
            }
            rest = &rest[end..];
        }
        assert!(checked > 0, "expected a concatenating lifted lambda");
    }

    #[test]
    fn ho_specializations_pay_the_call_depth_budget() {
        // A specialization that recurses through its own `fn` parameter lands
        // back in the same define, so it needs the same enter/exit hooks
        // [`Gen::function`] emits — otherwise runaway recursion segfaults
        // natively where the interpreter traps.
        let src = "fn twice(x: Int64, g: fn(Int64) -> Int64) -> Int64 { return g(g(x)) } \
                   fn inc(x: Int64) -> Int64 { return x + 1 } \
                   fn main() -> Int64 { return twice(1, inc) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let start = ir
            .find("define i64 @vyrn_twice__ho")
            .expect("specialization present");
        let end = ir[start..].find("\n}\n").expect("definition closes");
        let def = &ir[start..start + end];
        assert!(
            def.contains("call void @__vyrn_call_enter()"),
            "prologue takes a frame of the budget:\n{def}"
        );
        assert!(
            def.contains("call void @__vyrn_call_exit()"),
            "the ret gives the frame back:\n{def}"
        );
    }
}
