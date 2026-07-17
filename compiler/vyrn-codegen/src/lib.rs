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
//!
//! The Inkwell (in-memory LLVM) backend in the excluded `vyrn-codegen-llvm`
//! crate will eventually replace this; both must agree with the interpreter.

use std::collections::HashMap;
use std::fmt::Write;

use vyrn_frontend::ast::*;
use vyrn_frontend::own::DropKind;

/// LLVM IR for the region/arena runtime (see the preamble comment in `emit`).
///
/// The arena stack is `thread_local` (RFC-0025): `region { .. }` is memory
/// management, not an effect, so an isolated task may use it — and with tasks
/// on real OS threads a shared stack would race. Per-thread stacks keep every
/// region block self-contained on its own thread. On single-threaded targets
/// (wasm32-wasip1) LLVM lowers TLS to ordinary globals, so the shared IR is
/// unchanged in behavior there.
const REGION_RUNTIME: &str = "\
@__vyrn_region_sp = thread_local global i64 0
@__vyrn_region_heads = thread_local global [64 x ptr] zeroinitializer
@.trap.regiondepth = private unnamed_addr constant [34 x i8] c\"error: region nesting exceeds 64\\0A\\00\"

define void @__vyrn_region_enter() {
entry:
  %sp = load i64, ptr @__vyrn_region_sp
  %over = icmp sge i64 %sp, 64
  br i1 %over, label %trap, label %ok
trap:
  %e = call ptr @__vyrn_stderr()
  %w = call i32 @fputs(ptr @.trap.regiondepth, ptr %e)
  call void @exit(i32 1)
  unreachable
ok:
  %slot = getelementptr [64 x ptr], ptr @__vyrn_region_heads, i64 0, i64 %sp
  store ptr null, ptr %slot
  %sp1 = add i64 %sp, 1
  store i64 %sp1, ptr @__vyrn_region_sp
  ret void
}

define ptr @__vyrn_region_alloc(i64 %n) {
entry:
  %tot = add i64 %n, 8
  %raw = call ptr @__vyrn_malloc(i64 %tot)
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  %slot = getelementptr [64 x ptr], ptr @__vyrn_region_heads, i64 0, i64 %idx
  %prev = load ptr, ptr %slot
  store ptr %prev, ptr %raw
  store ptr %raw, ptr %slot
  %user = getelementptr i8, ptr %raw, i64 8
  ret ptr %user
}

define void @__vyrn_region_exit() {
entry:
  %sp = load i64, ptr @__vyrn_region_sp
  %idx = sub i64 %sp, 1
  store i64 %idx, ptr @__vyrn_region_sp
  %slot = getelementptr [64 x ptr], ptr @__vyrn_region_heads, i64 0, i64 %idx
  %head = load ptr, ptr %slot
  br label %loop
loop:
  %cur = phi ptr [ %head, %entry ], [ %next, %body ]
  %isnull = icmp eq ptr %cur, null
  br i1 %isnull, label %done, label %body
body:
  %next = load ptr, ptr %cur
  call void @free(ptr %cur)
  br label %loop
done:
  ret void
}

";

/// LLVM IR for the generational-reference cell slab (RFC-0004 §4, Path B).
/// A fixed slab of 65536 `Int` cells, each with a generation counter. Allocation
/// hands out `{ slot, generation }`; every access checks the reference's captured
/// generation against the slot's current one and traps on a mismatch, so a
/// reference used after `release` fails a cheap check instead of dangling. A
/// released slot is reused with a bumped generation, invalidating old references.
const CELL_RUNTIME: &str = "\
@__vyrn_cell_gen = global [65536 x i64] zeroinitializer
@__vyrn_cell_ptr_arr = global [65536 x ptr] zeroinitializer
@__vyrn_cell_top = global i64 0
@__vyrn_cell_free = global [65536 x i64] zeroinitializer
@__vyrn_cell_freetop = global i64 0
@.fmt.uaf = private unnamed_addr constant [37 x i8] c\"error: reference used after release\\0A\\00\"
@.fmt.oom = private unnamed_addr constant [31 x i8] c\"error: out of reference cells\\0A\\00\"

define void @__vyrn_cell_trap() {
entry:
  %e = call ptr @__vyrn_stderr()
  %r = call i32 @fputs(ptr @.fmt.uaf, ptr %e)
  call void @exit(i32 1)
  unreachable
}

define i64 @__vyrn_cell_alloc(ptr %p) {
entry:
  %ft = load i64, ptr @__vyrn_cell_freetop
  %hasfree = icmp sgt i64 %ft, 0
  br i1 %hasfree, label %reuse, label %fresh
reuse:
  %ft1 = sub i64 %ft, 1
  store i64 %ft1, ptr @__vyrn_cell_freetop
  %fp = getelementptr [65536 x i64], ptr @__vyrn_cell_free, i64 0, i64 %ft1
  %rslot = load i64, ptr %fp
  br label %done
fresh:
  %top = load i64, ptr @__vyrn_cell_top
  %oob = icmp sge i64 %top, 65536
  br i1 %oob, label %overflow, label %ok
overflow:
  %eo = call ptr @__vyrn_stderr()
  %ro = call i32 @fputs(ptr @.fmt.oom, ptr %eo)
  call void @exit(i32 1)
  unreachable
ok:
  %top1 = add i64 %top, 1
  store i64 %top1, ptr @__vyrn_cell_top
  br label %done
done:
  %slot = phi i64 [ %rslot, %reuse ], [ %top, %ok ]
  %pp = getelementptr [65536 x ptr], ptr @__vyrn_cell_ptr_arr, i64 0, i64 %slot
  store ptr %p, ptr %pp
  ret i64 %slot
}

define i64 @__vyrn_cell_getgen(i64 %slot) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vyrn_cell_gen, i64 0, i64 %slot
  %g = load i64, ptr %gp
  ret i64 %g
}

define ptr @__vyrn_cell_ptr(i64 %slot) {
entry:
  %pp = getelementptr [65536 x ptr], ptr @__vyrn_cell_ptr_arr, i64 0, i64 %slot
  %p = load ptr, ptr %pp
  ret ptr %p
}

define void @__vyrn_cell_check(i64 %slot, i64 %gen) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vyrn_cell_gen, i64 0, i64 %slot
  %cur = load i64, ptr %gp
  %ok = icmp eq i64 %cur, %gen
  br i1 %ok, label %pass, label %fail
fail:
  call void @__vyrn_cell_trap()
  unreachable
pass:
  ret void
}

define void @__vyrn_cell_release_slot(i64 %slot) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vyrn_cell_gen, i64 0, i64 %slot
  %g = load i64, ptr %gp
  %g1 = add i64 %g, 1
  store i64 %g1, ptr %gp
  %ft = load i64, ptr @__vyrn_cell_freetop
  %fp = getelementptr [65536 x i64], ptr @__vyrn_cell_free, i64 0, i64 %ft
  store i64 %slot, ptr %fp
  %ft1 = add i64 %ft, 1
  store i64 %ft1, ptr @__vyrn_cell_freetop
  ret void
}

";

/// Text-encoding runtime (hex / base64 / url) plus the shared helpers: a strict
/// UTF-8 validator (Björn Höhrmann's DFA — matches Rust's `from_utf8`) used by the
/// decoders, and hex-digit conversions. The `@__vyrn_utf8d` and `@__vyrn_b64alpha`
/// tables are emitted separately (generated in `emit`). Decoders return the
/// Option aggregate `{ i1 tag, i64 word0, i64 word1 }` (word0 = `ptrtoint` of the
/// result string on `Some`; all-zero on `None`).
const ENCODING_RUNTIME: &str = "\
define i8 @__vyrn_hexdigit(i8 %n) {
  %lt = icmp ult i8 %n, 10
  %d0 = add i8 %n, 48
  %da = add i8 %n, 87
  %r = select i1 %lt, i8 %d0, i8 %da
  ret i8 %r
}

define i8 @__vyrn_hexdigit_uc(i8 %n) {
  %lt = icmp ult i8 %n, 10
  %d0 = add i8 %n, 48
  %da = add i8 %n, 55
  %r = select i1 %lt, i8 %d0, i8 %da
  ret i8 %r
}

define i32 @__vyrn_hexval(i8 %c) {
  %cz = zext i8 %c to i32
  %d0 = icmp uge i32 %cz, 48
  %d9 = icmp ule i32 %cz, 57
  %isd = and i1 %d0, %d9
  %la = icmp uge i32 %cz, 97
  %lf = icmp ule i32 %cz, 102
  %isl = and i1 %la, %lf
  %ua = icmp uge i32 %cz, 65
  %uf = icmp ule i32 %cz, 70
  %isu = and i1 %ua, %uf
  %vd = sub i32 %cz, 48
  %vl = sub i32 %cz, 87
  %vu = sub i32 %cz, 55
  %r1 = select i1 %isd, i32 %vd, i32 -1
  %r2 = select i1 %isl, i32 %vl, i32 %r1
  %r3 = select i1 %isu, i32 %vu, i32 %r2
  ret i32 %r3
}

define i1 @__vyrn_utf8valid(ptr %s, i64 %len) {
entry:
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %st = phi i64 [ 0, %entry ], [ %st2, %body ]
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

define ptr @__vyrn_hex_encode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %outlen = mul i64 %len, 2
  %sz = add i64 %outlen, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %fin, label %body
body:
  %bp = getelementptr i8, ptr %s, i64 %i
  %b = load i8, ptr %bp
  %hi = lshr i8 %b, 4
  %lo = and i8 %b, 15
  %hc = call i8 @__vyrn_hexdigit(i8 %hi)
  %lc = call i8 @__vyrn_hexdigit(i8 %lo)
  %o = mul i64 %i, 2
  %op0 = getelementptr i8, ptr %out, i64 %o
  store i8 %hc, ptr %op0
  %o1 = add i64 %o, 1
  %op1 = getelementptr i8, ptr %out, i64 %o1
  store i8 %lc, ptr %op1
  %i2 = add i64 %i, 1
  br label %loop
fin:
  %ep = getelementptr i8, ptr %out, i64 %outlen
  store i8 0, ptr %ep
  ret ptr %out
}

define {i1, i64, i64} @__vyrn_hex_decode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %odd = and i64 %len, 1
  %isodd = icmp ne i64 %odd, 0
  br i1 %isodd, label %none, label %ok0
ok0:
  %outlen = lshr i64 %len, 1
  %sz = add i64 %outlen, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %ok0 ], [ %i2, %cont ]
  %done = icmp uge i64 %i, %outlen
  br i1 %done, label %valid, label %body
body:
  %hidx = mul i64 %i, 2
  %lidx = add i64 %hidx, 1
  %hip = getelementptr i8, ptr %s, i64 %hidx
  %hc = load i8, ptr %hip
  %lop = getelementptr i8, ptr %s, i64 %lidx
  %lc = load i8, ptr %lop
  %hv = call i32 @__vyrn_hexval(i8 %hc)
  %lv = call i32 @__vyrn_hexval(i8 %lc)
  %hbad = icmp slt i32 %hv, 0
  %lbad = icmp slt i32 %lv, 0
  %bad = or i1 %hbad, %lbad
  br i1 %bad, label %none, label %cont
cont:
  %hv8 = trunc i32 %hv to i8
  %lv8 = trunc i32 %lv to i8
  %hsh = shl i8 %hv8, 4
  %byte = or i8 %hsh, %lv8
  %op = getelementptr i8, ptr %out, i64 %i
  store i8 %byte, ptr %op
  %i2 = add i64 %i, 1
  br label %loop
valid:
  %ep = getelementptr i8, ptr %out, i64 %outlen
  store i8 0, ptr %ep
  %v = call i1 @__vyrn_utf8valid(ptr %out, i64 %outlen)
  br i1 %v, label %some, label %none
some:
  %w0 = ptrtoint ptr %out to i64
  %s0 = insertvalue {i1, i64, i64} undef, i1 1, 0
  %s1 = insertvalue {i1, i64, i64} %s0, i64 %w0, 1
  %s2 = insertvalue {i1, i64, i64} %s1, i64 0, 2
  ret {i1, i64, i64} %s2
none:
  %n0 = insertvalue {i1, i64, i64} undef, i1 0, 0
  %n1 = insertvalue {i1, i64, i64} %n0, i64 0, 1
  %n2 = insertvalue {i1, i64, i64} %n1, i64 0, 2
  ret {i1, i64, i64} %n2
}

define ptr @__vyrn_url_encode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %cap = mul i64 %len, 3
  %sz = add i64 %cap, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %cont ]
  %o = phi i64 [ 0, %entry ], [ %o2, %cont ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %fin, label %body
body:
  %bp = getelementptr i8, ptr %s, i64 %i
  %b = load i8, ptr %bp
  %bz = zext i8 %b to i32
  %alnum_l = icmp uge i32 %bz, 97
  %alnum_lh = icmp ule i32 %bz, 122
  %isl = and i1 %alnum_l, %alnum_lh
  %alnum_u = icmp uge i32 %bz, 65
  %alnum_uh = icmp ule i32 %bz, 90
  %isu = and i1 %alnum_u, %alnum_uh
  %dig_l = icmp uge i32 %bz, 48
  %dig_h = icmp ule i32 %bz, 57
  %isdig = and i1 %dig_l, %dig_h
  %isdash = icmp eq i32 %bz, 45
  %isund = icmp eq i32 %bz, 95
  %isdot = icmp eq i32 %bz, 46
  %istil = icmp eq i32 %bz, 126
  %u1 = or i1 %isl, %isu
  %u2 = or i1 %u1, %isdig
  %u3 = or i1 %u2, %isdash
  %u4 = or i1 %u3, %isund
  %u5 = or i1 %u4, %isdot
  %unres = or i1 %u5, %istil
  br i1 %unres, label %plain, label %pct
plain:
  %pp = getelementptr i8, ptr %out, i64 %o
  store i8 %b, ptr %pp
  %op1 = add i64 %o, 1
  br label %cont
pct:
  %hi = lshr i8 %b, 4
  %lo = and i8 %b, 15
  %hc = call i8 @__vyrn_hexdigit_uc(i8 %hi)
  %lc = call i8 @__vyrn_hexdigit_uc(i8 %lo)
  %p0 = getelementptr i8, ptr %out, i64 %o
  store i8 37, ptr %p0
  %o_1 = add i64 %o, 1
  %p1 = getelementptr i8, ptr %out, i64 %o_1
  store i8 %hc, ptr %p1
  %o_2 = add i64 %o, 2
  %p2 = getelementptr i8, ptr %out, i64 %o_2
  store i8 %lc, ptr %p2
  %op3 = add i64 %o, 3
  br label %cont
cont:
  %o2 = phi i64 [ %op1, %plain ], [ %op3, %pct ]
  %i2 = add i64 %i, 1
  br label %loop
fin:
  %ep = getelementptr i8, ptr %out, i64 %o
  store i8 0, ptr %ep
  ret ptr %out
}

define {i1, i64, i64} @__vyrn_url_decode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %sz = add i64 %len, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %inext, %cont ]
  %o = phi i64 [ 0, %entry ], [ %onext, %cont ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %valid, label %body
body:
  %bp = getelementptr i8, ptr %s, i64 %i
  %b = load i8, ptr %bp
  %ispct = icmp eq i8 %b, 37
  br i1 %ispct, label %pct, label %plain
plain:
  %pp = getelementptr i8, ptr %out, i64 %o
  store i8 %b, ptr %pp
  %o_p = add i64 %o, 1
  %i_p = add i64 %i, 1
  br label %cont
pct:
  %i1 = add i64 %i, 1
  %i2 = add i64 %i, 2
  %room = icmp ult i64 %i2, %len
  br i1 %room, label %pctok, label %none
pctok:
  %hip = getelementptr i8, ptr %s, i64 %i1
  %hc = load i8, ptr %hip
  %lop = getelementptr i8, ptr %s, i64 %i2
  %lc = load i8, ptr %lop
  %hv = call i32 @__vyrn_hexval(i8 %hc)
  %lv = call i32 @__vyrn_hexval(i8 %lc)
  %hbad = icmp slt i32 %hv, 0
  %lbad = icmp slt i32 %lv, 0
  %bad = or i1 %hbad, %lbad
  br i1 %bad, label %none, label %pctstore
pctstore:
  %hv8 = trunc i32 %hv to i8
  %lv8 = trunc i32 %lv to i8
  %hsh = shl i8 %hv8, 4
  %byte = or i8 %hsh, %lv8
  %pp2 = getelementptr i8, ptr %out, i64 %o
  store i8 %byte, ptr %pp2
  %o_pc = add i64 %o, 1
  %i_pc = add i64 %i, 3
  br label %cont
cont:
  %onext = phi i64 [ %o_p, %plain ], [ %o_pc, %pctstore ]
  %inext = phi i64 [ %i_p, %plain ], [ %i_pc, %pctstore ]
  br label %loop
valid:
  %ep = getelementptr i8, ptr %out, i64 %o
  store i8 0, ptr %ep
  %v = call i1 @__vyrn_utf8valid(ptr %out, i64 %o)
  br i1 %v, label %some, label %none
some:
  %w0 = ptrtoint ptr %out to i64
  %s0 = insertvalue {i1, i64, i64} undef, i1 1, 0
  %s1 = insertvalue {i1, i64, i64} %s0, i64 %w0, 1
  %s2 = insertvalue {i1, i64, i64} %s1, i64 0, 2
  ret {i1, i64, i64} %s2
none:
  %n0 = insertvalue {i1, i64, i64} undef, i1 0, 0
  %n1 = insertvalue {i1, i64, i64} %n0, i64 0, 1
  %n2 = insertvalue {i1, i64, i64} %n1, i64 0, 2
  ret {i1, i64, i64} %n2
}

define i8 @__vyrn_b64char(i64 %idx) {
  %p = getelementptr i8, ptr @__vyrn_b64alpha, i64 %idx
  %c = load i8, ptr %p
  ret i8 %c
}

define ptr @__vyrn_b64_encode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %p2 = add i64 %len, 2
  %grp = udiv i64 %p2, 3
  %outlen = mul i64 %grp, 4
  %sz = add i64 %outlen, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i3, %body ]
  %o = phi i64 [ 0, %entry ], [ %o4, %body ]
  %rem = sub i64 %len, %i
  %has3 = icmp uge i64 %rem, 3
  br i1 %has3, label %body, label %tail
body:
  %b0p = getelementptr i8, ptr %s, i64 %i
  %b0 = load i8, ptr %b0p
  %i1 = add i64 %i, 1
  %b1p = getelementptr i8, ptr %s, i64 %i1
  %b1 = load i8, ptr %b1p
  %i2 = add i64 %i, 2
  %b2p = getelementptr i8, ptr %s, i64 %i2
  %b2 = load i8, ptr %b2p
  %z0 = zext i8 %b0 to i64
  %z1 = zext i8 %b1 to i64
  %z2 = zext i8 %b2 to i64
  %s0 = shl i64 %z0, 16
  %s1 = shl i64 %z1, 8
  %n01 = or i64 %s0, %s1
  %n = or i64 %n01, %z2
  %d0 = lshr i64 %n, 18
  %d0m = and i64 %d0, 63
  %d1 = lshr i64 %n, 12
  %d1m = and i64 %d1, 63
  %d2 = lshr i64 %n, 6
  %d2m = and i64 %d2, 63
  %d3m = and i64 %n, 63
  %c0 = call i8 @__vyrn_b64char(i64 %d0m)
  %c1 = call i8 @__vyrn_b64char(i64 %d1m)
  %c2 = call i8 @__vyrn_b64char(i64 %d2m)
  %c3 = call i8 @__vyrn_b64char(i64 %d3m)
  %o0p = getelementptr i8, ptr %out, i64 %o
  store i8 %c0, ptr %o0p
  %oo1 = add i64 %o, 1
  %o1p = getelementptr i8, ptr %out, i64 %oo1
  store i8 %c1, ptr %o1p
  %oo2 = add i64 %o, 2
  %o2p = getelementptr i8, ptr %out, i64 %oo2
  store i8 %c2, ptr %o2p
  %oo3 = add i64 %o, 3
  %o3p = getelementptr i8, ptr %out, i64 %oo3
  store i8 %c3, ptr %o3p
  %i3 = add i64 %i, 3
  %o4 = add i64 %o, 4
  br label %loop
tail:
  %is1 = icmp eq i64 %rem, 1
  br i1 %is1, label %one, label %tail2
one:
  %t0p = getelementptr i8, ptr %s, i64 %i
  %t0 = load i8, ptr %t0p
  %tz0 = zext i8 %t0 to i64
  %tn = shl i64 %tz0, 16
  %e0 = lshr i64 %tn, 18
  %e0m = and i64 %e0, 63
  %e1 = lshr i64 %tn, 12
  %e1m = and i64 %e1, 63
  %ec0 = call i8 @__vyrn_b64char(i64 %e0m)
  %ec1 = call i8 @__vyrn_b64char(i64 %e1m)
  %e0p = getelementptr i8, ptr %out, i64 %o
  store i8 %ec0, ptr %e0p
  %eo1 = add i64 %o, 1
  %e1p = getelementptr i8, ptr %out, i64 %eo1
  store i8 %ec1, ptr %e1p
  %eo2 = add i64 %o, 2
  %e2p = getelementptr i8, ptr %out, i64 %eo2
  store i8 61, ptr %e2p
  %eo3 = add i64 %o, 3
  %e3p = getelementptr i8, ptr %out, i64 %eo3
  store i8 61, ptr %e3p
  br label %fin
tail2:
  %is2 = icmp eq i64 %rem, 2
  br i1 %is2, label %two, label %fin
two:
  %f0p = getelementptr i8, ptr %s, i64 %i
  %f0 = load i8, ptr %f0p
  %fi1 = add i64 %i, 1
  %f1p = getelementptr i8, ptr %s, i64 %fi1
  %f1 = load i8, ptr %f1p
  %fz0 = zext i8 %f0 to i64
  %fz1 = zext i8 %f1 to i64
  %fs0 = shl i64 %fz0, 16
  %fs1 = shl i64 %fz1, 8
  %fn = or i64 %fs0, %fs1
  %g0 = lshr i64 %fn, 18
  %g0m = and i64 %g0, 63
  %g1 = lshr i64 %fn, 12
  %g1m = and i64 %g1, 63
  %g2 = lshr i64 %fn, 6
  %g2m = and i64 %g2, 63
  %gc0 = call i8 @__vyrn_b64char(i64 %g0m)
  %gc1 = call i8 @__vyrn_b64char(i64 %g1m)
  %gc2 = call i8 @__vyrn_b64char(i64 %g2m)
  %g0p = getelementptr i8, ptr %out, i64 %o
  store i8 %gc0, ptr %g0p
  %go1 = add i64 %o, 1
  %g1p = getelementptr i8, ptr %out, i64 %go1
  store i8 %gc1, ptr %g1p
  %go2 = add i64 %o, 2
  %g2p = getelementptr i8, ptr %out, i64 %go2
  store i8 %gc2, ptr %g2p
  %go3 = add i64 %o, 3
  %g3p = getelementptr i8, ptr %out, i64 %go3
  store i8 61, ptr %g3p
  br label %fin
fin:
  %ep = getelementptr i8, ptr %out, i64 %outlen
  store i8 0, ptr %ep
  ret ptr %out
}

define i32 @__vyrn_b64val(i8 %c) {
  %cz = zext i8 %c to i32
  %ua = icmp uge i32 %cz, 65
  %uz = icmp ule i32 %cz, 90
  %isu = and i1 %ua, %uz
  %la = icmp uge i32 %cz, 97
  %lz = icmp ule i32 %cz, 122
  %isl = and i1 %la, %lz
  %da = icmp uge i32 %cz, 48
  %dz = icmp ule i32 %cz, 57
  %isd = and i1 %da, %dz
  %isp = icmp eq i32 %cz, 43
  %iss = icmp eq i32 %cz, 47
  %vu = sub i32 %cz, 65
  %vl = sub i32 %cz, 71
  %vd = add i32 %cz, 4
  %r1 = select i1 %isu, i32 %vu, i32 -1
  %r2 = select i1 %isl, i32 %vl, i32 %r1
  %r3 = select i1 %isd, i32 %vd, i32 %r2
  %r4 = select i1 %isp, i32 62, i32 %r3
  %r5 = select i1 %iss, i32 63, i32 %r4
  ret i32 %r5
}

define {i1, i64, i64} @__vyrn_b64_decode(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %m4 = and i64 %len, 3
  %notmul4 = icmp ne i64 %m4, 0
  %empty = icmp eq i64 %len, 0
  br i1 %notmul4, label %none, label %ok0
ok0:
  %cap = mul i64 %len, 1
  %sz = add i64 %cap, 1
  %out = call ptr @__vyrn_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %ok0 ], [ %i4, %store ]
  %o = phi i64 [ 0, %ok0 ], [ %onext, %store ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %valid, label %body
body:
  %c0p = getelementptr i8, ptr %s, i64 %i
  %c0 = load i8, ptr %c0p
  %ci1 = add i64 %i, 1
  %c1p = getelementptr i8, ptr %s, i64 %ci1
  %c1 = load i8, ptr %c1p
  %ci2 = add i64 %i, 2
  %c2p = getelementptr i8, ptr %s, i64 %ci2
  %c2 = load i8, ptr %c2p
  %ci3 = add i64 %i, 3
  %c3p = getelementptr i8, ptr %s, i64 %ci3
  %c3 = load i8, ptr %c3p
  %isLast4 = add i64 %i, 4
  %islast = icmp eq i64 %isLast4, %len
  %pad2 = icmp eq i8 %c2, 61
  %pad3 = icmp eq i8 %c3, 61
  %anypad = or i1 %pad2, %pad3
  %padnotlast = and i1 %anypad, %islast
  %padbad1 = xor i1 %islast, true
  %badpadpos = and i1 %anypad, %padbad1
  br i1 %badpadpos, label %none, label %chkpad
chkpad:
  %pad2only = and i1 %pad2, %pad3
  %pad2butnot3 = xor i1 %pad3, true
  %illegal = and i1 %pad2, %pad2butnot3
  br i1 %illegal, label %none, label %vals
vals:
  %v0 = call i32 @__vyrn_b64val(i8 %c0)
  %v1 = call i32 @__vyrn_b64val(i8 %c1)
  %v2raw = call i32 @__vyrn_b64val(i8 %c2)
  %v3raw = call i32 @__vyrn_b64val(i8 %c3)
  %v2 = select i1 %pad2, i32 0, i32 %v2raw
  %v3 = select i1 %pad3, i32 0, i32 %v3raw
  %b0bad = icmp slt i32 %v0, 0
  %b1bad = icmp slt i32 %v1, 0
  %b2bad = icmp slt i32 %v2, 0
  %b3bad = icmp slt i32 %v3, 0
  %e01 = or i1 %b0bad, %b1bad
  %e23 = or i1 %b2bad, %b3bad
  %anybad = or i1 %e01, %e23
  br i1 %anybad, label %none, label %store
store:
  %z0 = zext i32 %v0 to i64
  %z1 = zext i32 %v1 to i64
  %z2 = zext i32 %v2 to i64
  %z3 = zext i32 %v3 to i64
  %sh0 = shl i64 %z0, 18
  %sh1 = shl i64 %z1, 12
  %sh2 = shl i64 %z2, 6
  %n01 = or i64 %sh0, %sh1
  %n012 = or i64 %n01, %sh2
  %n = or i64 %n012, %z3
  %ob0 = lshr i64 %n, 16
  %ob0t = trunc i64 %ob0 to i8
  %op0 = getelementptr i8, ptr %out, i64 %o
  store i8 %ob0t, ptr %op0
  %o1 = add i64 %o, 1
  %ob1 = lshr i64 %n, 8
  %ob1t = trunc i64 %ob1 to i8
  %op1 = getelementptr i8, ptr %out, i64 %o1
  store i8 %ob1t, ptr %op1
  %o2 = add i64 %o, 2
  %ob2t = trunc i64 %n to i8
  %op2 = getelementptr i8, ptr %out, i64 %o2
  store i8 %ob2t, ptr %op2
  %keep1 = xor i1 %pad3, true
  %keep1n = zext i1 %keep1 to i64
  %keep2 = xor i1 %pad2, true
  %keep2n = zext i1 %keep2 to i64
  %oplus = add i64 %o, 1
  %oplus2 = add i64 %oplus, %keep1n
  %onext = add i64 %oplus2, %keep2n
  %i4 = add i64 %i, 4
  br label %loop
valid:
  %ep = getelementptr i8, ptr %out, i64 %o
  store i8 0, ptr %ep
  %v = call i1 @__vyrn_utf8valid(ptr %out, i64 %o)
  br i1 %v, label %some, label %none
some:
  %w0 = ptrtoint ptr %out to i64
  %s0 = insertvalue {i1, i64, i64} undef, i1 1, 0
  %s1 = insertvalue {i1, i64, i64} %s0, i64 %w0, 1
  %s2 = insertvalue {i1, i64, i64} %s1, i64 0, 2
  ret {i1, i64, i64} %s2
none:
  %n0 = insertvalue {i1, i64, i64} undef, i1 0, 0
  %n1 = insertvalue {i1, i64, i64} %n0, i64 0, 1
  %n2 = insertvalue {i1, i64, i64} %n1, i64 0, 2
  ret {i1, i64, i64} %n2
}

";

/// `bytes(s)` / `chars(s)`: build an `Array<UInt8>` ({ptr,len,cap}, i8 stride —
/// RFC-0014 M2) of a string's raw UTF-8 bytes, or an `Array<Int>` of its decoded
/// Unicode code points (a two-pass UTF-8 decode — count leaders, then decode
/// each 1–4 byte sequence).
const STRING_RUNTIME: &str = "\
define {ptr, i64, i64} @__vyrn_str_bytes(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  %data = call ptr @__vyrn_malloc(i64 %len)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %ret, label %body
body:
  %sp = getelementptr i8, ptr %s, i64 %i
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

define {ptr, i64, i64} @__vyrn_str_chars(ptr %s) {
entry:
  %len = call i64 @__vyrn_strlen(ptr %s)
  br label %cloop
cloop:
  %ci = phi i64 [ 0, %entry ], [ %ci2, %cbody ]
  %cn = phi i64 [ 0, %entry ], [ %cn2, %cbody ]
  %cdone = icmp uge i64 %ci, %len
  br i1 %cdone, label %alloc, label %cbody
cbody:
  %cbp = getelementptr i8, ptr %s, i64 %ci
  %cb = load i8, ptr %cbp
  %cmask = and i8 %cb, -64
  %iscont = icmp eq i8 %cmask, -128
  %inc = select i1 %iscont, i64 0, i64 1
  %cn2 = add i64 %cn, %inc
  %ci2 = add i64 %ci, 1
  br label %cloop
alloc:
  %sz = mul i64 %cn, 8
  %data = call ptr @__vyrn_malloc(i64 %sz)
  br label %dloop
dloop:
  %di = phi i64 [ 0, %alloc ], [ %di2, %store ]
  %dj = phi i64 [ 0, %alloc ], [ %dj2, %store ]
  %ddone = icmp uge i64 %di, %len
  br i1 %ddone, label %ret, label %dbody
dbody:
  %b0p = getelementptr i8, ptr %s, i64 %di
  %b0 = load i8, ptr %b0p
  %b0z = zext i8 %b0 to i64
  %c1 = icmp ult i64 %b0z, 128
  br i1 %c1, label %L1, label %m2
L1:
  br label %have
m2:
  %c2 = icmp ult i64 %b0z, 224
  br i1 %c2, label %L2, label %m3
L2:
  %cp2 = and i64 %b0z, 31
  br label %have
m3:
  %c3 = icmp ult i64 %b0z, 240
  br i1 %c3, label %L3, label %L4
L3:
  %cp3 = and i64 %b0z, 15
  br label %have
L4:
  %cp4 = and i64 %b0z, 7
  br label %have
have:
  %L = phi i64 [ 1, %L1 ], [ 2, %L2 ], [ 3, %L3 ], [ 4, %L4 ]
  %cp0 = phi i64 [ %b0z, %L1 ], [ %cp2, %L2 ], [ %cp3, %L3 ], [ %cp4, %L4 ]
  br label %kloop
kloop:
  %k = phi i64 [ 1, %have ], [ %k2, %kbody ]
  %cp = phi i64 [ %cp0, %have ], [ %cpn, %kbody ]
  %kdone = icmp uge i64 %k, %L
  br i1 %kdone, label %store, label %kbody
kbody:
  %ki = add i64 %di, %k
  %kp = getelementptr i8, ptr %s, i64 %ki
  %kb = load i8, ptr %kp
  %kbz = zext i8 %kb to i64
  %kbits = and i64 %kbz, 63
  %cpsh = shl i64 %cp, 6
  %cpn = or i64 %cpsh, %kbits
  %k2 = add i64 %k, 1
  br label %kloop
store:
  %dp = getelementptr i64, ptr %data, i64 %dj
  store i64 %cp, ptr %dp
  %dj2 = add i64 %dj, 1
  %di2 = add i64 %di, %L
  br label %dloop
ret:
  %r0 = insertvalue {ptr, i64, i64} undef, ptr %data, 0
  %r1 = insertvalue {ptr, i64, i64} %r0, i64 %cn, 1
  %r2 = insertvalue {ptr, i64, i64} %r1, i64 %cn, 2
  ret {ptr, i64, i64} %r2
}

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
  %dp = getelementptr ptr, ptr %data, i64 %i
  store ptr %s, ptr %dp
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
  %plen = call i64 @__vyrn_strlen(ptr %path)
  %bsz = add i64 %plen, 40
  %buf = call ptr @__vyrn_malloc(i64 %bsz)
  call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr %buf, i64 %bsz, ptr %fmt, ptr %path)
  ret ptr %buf
}

define ptr @__vyrn_write_err(ptr %path) {
entry:
  %plen = call i64 @__vyrn_strlen(ptr %path)
  %bsz = add i64 %plen, 40
  %buf = call ptr @__vyrn_malloc(i64 %bsz)
  call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr %buf, i64 %bsz, ptr @.io.writeerr, ptr %path)
  ret ptr %buf
}

define ptr @__vyrn_bytes_dup(ptr %data, i64 %len) {
entry:
  %bsz = add i64 %len, 1
  %buf = call ptr @__vyrn_malloc(i64 %bsz)
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
  call void @free(ptr %buf)
  ret ptr null
ok:
  %tp = getelementptr i8, ptr %buf, i64 %len
  store i8 0, ptr %tp
  ret ptr %buf
}

";

/// The private LLVM symbol for an `extern` import (RFC-0012). Prefixed so it
/// cannot collide with a real C symbol on the native target: the generated C
/// trap stub defines exactly this name, and the wasm import name is carried
/// separately by the `wasm-import-name` attribute (the raw Vyrn name).
fn extern_symbol(name: &str) -> String {
    format!("__vyrn_extern_{name}")
}

/// The extern (JS-boundary) ABI value type for one primitive, per the RFC-0012
/// table: `Int64`/`i64`, sized ints ≤32-bit widen to `i32`, `Bool` is `i32`,
/// floats stay `double`/`float`, `String` returns as a bare `ptr`, `Unit` is a
/// missing result. `String` *parameters* are handled separately (they cross as
/// a `(ptr, len)` pair). The checker guarantees no other type reaches here.
fn extern_abi_ll(ty: &Type) -> &'static str {
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

/// Emit a complete LLVM IR module for `program`.
/// The RFC-0037 stage gate: stored function values are checked and executable
/// under the interpreter, but their defunctionalized lowering has not landed in
/// this backend yet. Fail loudly rather than miscompile.
fn rfc0037_gate(what: &str) -> String {
    format!(
        "{what}: stored function values (RFC-0037) are not yet supported by the \
         native/wasm backends — run with `vyrn run` (interpreter) while the \
         defunctionalized lowering lands"
    )
}

/// Whether a declared type transitively mentions a function type (RFC-0037
/// stage gate). Named references don't recurse — the alias's own declaration
/// is scanned directly, so any fn-typed storage roots at a flagged site.
fn mentions_fn_type(ty: &Type) -> bool {
    match ty {
        Type::Fn(..) => true,
        Type::Option(i)
        | Type::Array(i)
        | Type::ArrayN(i, _)
        | Type::Ref(i)
        | Type::Task(i)
        | Type::Partial(i)
        | Type::Omit(i, _)
        | Type::Pick(i, _) => mentions_fn_type(i),
        Type::Result(a, b) | Type::Map(a, b) | Type::Merge(a, b) => {
            mentions_fn_type(a) || mentions_fn_type(b)
        }
        Type::Record(fs) => fs.iter().any(|f| mentions_fn_type(&f.ty)),
        Type::Enum(vs) => vs.iter().any(|v| v.payload.iter().any(mentions_fn_type)),
        Type::App(_, args) => args.iter().any(mentions_fn_type),
        _ => false,
    }
}

/// Scan for RFC-0037 storage-of-function-values the checker now accepts but
/// this backend cannot lower yet: fn types in type declarations, module state,
/// function returns, and `let` annotations. (Bare named-function values and
/// lambda literals in storage positions are caught at their emission sites.)
fn rfc0037_stage_scan(program: &Program) -> Result<(), String> {
    for t in &program.type_decls {
        if mentions_fn_type(&t.base) {
            return Err(rfc0037_gate(&format!("type `{}`", t.name)));
        }
    }
    for g in &program.globals {
        if g.ty.as_ref().is_some_and(mentions_fn_type) {
            return Err(rfc0037_gate(&format!("module state `{}`", g.name)));
        }
    }
    fn scan_block(b: &Block, fname: &str) -> Result<(), String> {
        for s in &b.stmts {
            match s {
                Stmt::Let { name, ty, .. } => {
                    if ty.as_ref().is_some_and(mentions_fn_type) {
                        return Err(rfc0037_gate(&format!("`let {name}` in `{fname}`")));
                    }
                }
                Stmt::If { then_block, else_block, .. } => {
                    scan_block(then_block, fname)?;
                    if let Some(eb) = else_block {
                        scan_block(eb, fname)?;
                    }
                }
                Stmt::While { body, .. }
                | Stmt::ForIn { body, .. }
                | Stmt::Region { body, .. } => scan_block(body, fname)?,
                _ => {}
            }
        }
        Ok(())
    }
    for f in &program.functions {
        // v1 `fn`-typed PARAMETERS stay on the monomorphization path — only a
        // fn-typed RETURN is a storage position this backend cannot lower yet.
        if mentions_fn_type(&f.ret) {
            return Err(rfc0037_gate(&format!("the return type of `{}`", f.name)));
        }
        scan_block(&f.body, &f.name)?;
    }
    for t in &program.tests {
        scan_block(&t.body, &t.name)?;
    }
    Ok(())
}

pub fn emit(program: &Program) -> Result<String, String> {
    rfc0037_stage_scan(program)?;
    let mut out = String::new();
    // module preamble: printf/abort + format strings (opaque-pointer style)
    out.push_str("; Vyrn v0.1 — generated LLVM IR (target: LLVM 15+)\n");
    out.push_str("declare i32 @printf(ptr, ...)\n");
    // exit() (not abort()) so stdio buffers flush and the exit code is a clean 1,
    // matching the interpreter.
    out.push_str("declare void @exit(i32)\n");
    out.push_str("declare i32 @strcmp(ptr, ptr)\n");
    out.push_str("declare i32 @__vyrn_strncmp(ptr, ptr, i64)\n");
    out.push_str("declare ptr @strstr(ptr, ptr)\n");
    // Heap + string runtime (dynamic strings). Allocations are not yet freed —
    // the reclamation strategy is RFC-0004's open question.
    out.push_str("declare i64 @__vyrn_strlen(ptr)\n");
    out.push_str("declare ptr @__vyrn_malloc(i64)\n");
    out.push_str("declare ptr @__vyrn_realloc(ptr, i64)\n");
    out.push_str("declare void @free(ptr)\n");
    out.push_str("declare ptr @strcpy(ptr, ptr)\n");
    out.push_str("declare ptr @strcat(ptr, ptr)\n");
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
    // JSON codec runtime (RFC-0018): the DOM builders/accessors, the parser,
    // the canonical encoder, and the decode-side issue accumulator live in the
    // C shim; the per-type encode/decode logic is generated as IR below.
    out.push_str("declare ptr @__vyrn_vj_obj()\n");
    out.push_str("declare ptr @__vyrn_vj_arr()\n");
    out.push_str("declare ptr @__vyrn_vj_null()\n");
    out.push_str("declare ptr @__vyrn_vj_bool(i1)\n");
    out.push_str("declare ptr @__vyrn_vj_int(i64)\n");
    out.push_str("declare ptr @__vyrn_vj_uint(i64)\n");
    out.push_str("declare ptr @__vyrn_vj_float(double)\n");
    out.push_str("declare ptr @__vyrn_vj_str(ptr)\n");
    out.push_str("declare void @__vyrn_vj_push(ptr, ptr)\n");
    out.push_str("declare void @__vyrn_vj_set(ptr, ptr, ptr)\n");
    out.push_str("declare ptr @__vyrn_vj_encode(ptr)\n");
    out.push_str("declare ptr @__vyrn_json_parse(ptr, ptr)\n");
    out.push_str("declare i32 @__vyrn_vj_kind(ptr)\n");
    out.push_str("declare i32 @__vyrn_vj_bool_get(ptr)\n");
    out.push_str("declare ptr @__vyrn_vj_get(ptr, ptr)\n");
    out.push_str("declare i64 @__vyrn_vj_len(ptr)\n");
    out.push_str("declare ptr @__vyrn_vj_at(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_vj_at_or_null(ptr, i64)\n");
    out.push_str("declare i64 @__vyrn_vj_obj_len(ptr)\n");
    out.push_str("declare ptr @__vyrn_vj_obj_key(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_vj_obj_at(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_vj_str_get(ptr)\n");
    // Map<String, V> runtime (RFC-0028).
    out.push_str("declare i64 @__vyrn_map_find(ptr, i64, ptr)\n");
    out.push_str("declare void @__vyrn_map_reserve(ptr, i64)\n");
    out.push_str("declare void @__vyrn_map_remove_at(ptr, i64, i64)\n");
    out.push_str("declare ptr @__vyrn_map_keys_copy(ptr, i64)\n");
    out.push_str("declare i32 @__vyrn_vj_asint(ptr, i32, i32, ptr)\n");
    out.push_str("declare double @__vyrn_vj_asfloat(ptr)\n");
    out.push_str("declare ptr @__vyrn_json_type_msg(ptr, i32)\n");
    out.push_str("declare ptr @__vyrn_json_field_path(ptr, ptr)\n");
    out.push_str("declare ptr @__vyrn_json_index_path(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_issues_new()\n");
    out.push_str("declare void @__vyrn_issues_push(ptr, ptr, ptr, ptr)\n");
    out.push_str("declare i64 @__vyrn_issues_len(ptr)\n");
    out.push_str("declare ptr @__vyrn_issue_key(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_issue_path(ptr, i64)\n");
    out.push_str("declare ptr @__vyrn_issue_msg(ptr, i64)\n");
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
    for (i, f) in program.functions.iter().filter(|f| f.is_extern).enumerate() {
        let ret = extern_abi_ll(&f.ret);
        let params = extern_decl_params(f);
        let grp = 100 + i; // arbitrary, distinct ids; no other groups in this IR
        out.push_str(&format!("declare {ret} @{}({params}) #{grp}\n", extern_symbol(&f.name)));
        extern_attr_groups.push_str(&format!(
            "attributes #{grp} = {{ \"wasm-import-module\"=\"vyrn\" \"wasm-import-name\"=\"{}\" }}\n",
            f.name
        ));
    }
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
    out.push_str(
        "@.trap.aoob = private unnamed_addr constant [39 x i8] \
         c\"error: array index %lld out of bounds\\0A\\00\"\n",
    );
    out.push_str(
        "@.trap.soob = private unnamed_addr constant [40 x i8] \
         c\"error: string index %lld out of bounds\\0A\\00\"\n\n",
    );

    // ---- region / arena runtime (RFC-0004 §4) ---------------------------
    // A `region { .. }` block gives heap allocations a deterministic lifetime:
    // everything allocated while the region is on the stack is freed when the
    // block exits. Implementation: a stack (max depth 64; entering a 65th
    // nested region traps, and the interpreter enforces the same bound) of
    // singly-linked allocation lists. Each region allocation reserves 8 extra
    // header bytes holding the "next" link; `exit` walks the list and frees it.
    // `concat` routes through the arena at runtime when a region is active.
    out.push_str(REGION_RUNTIME);

    // ---- generational-reference cell slab (RFC-0004 §4, Path B) ----------
    // Backs `cell`/`get`/`set`/`release`: a stale reference is caught by a
    // generation check instead of dangling.
    out.push_str(CELL_RUNTIME);
    out.push_str(STRING_RUNTIME);
    // Encoding tables + runtime (hex/base64/url + the UTF-8 validator DFA).
    let utf8d = utf8d_table();
    let table_body = utf8d.iter().map(|b| format!("i8 {b}")).collect::<Vec<_>>().join(", ");
    out.push_str(&format!(
        "@__vyrn_utf8d = private unnamed_addr constant [364 x i8] [{table_body}]\n"
    ));
    out.push_str(
        "@__vyrn_b64alpha = private unnamed_addr constant [64 x i8] \
         c\"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/\"\n",
    );
    out.push_str(ENCODING_RUNTIME);
    out.push_str(REGEX_RUNTIME);
    // `%lld\n` for i64 — `%ld` would be 32-bit under the Windows/MSVC ABI where
    // `long` is 32 bits, truncating full 64-bit values; `long long` is 64-bit.
    out.push_str("@.fmt.d = private unnamed_addr constant [6 x i8] c\"%lld\\0A\\00\"\n");
    // `%llu\n` for printing unsigned sized ints (UInt8..64) — zero-extended to u64.
    out.push_str("@.fmt.u = private unnamed_addr constant [6 x i8] c\"%llu\\0A\\00\"\n");
    // `%f\n` for printing Float64 (printf's default precision is 6, matching interp).
    out.push_str("@.fmt.f = private unnamed_addr constant [4 x i8] c\"%f\\0A\\00\"\n");
    // No-newline variants used by `str(..)` (interpolation renders without \n):
    // %lld for signed ints, %llu for unsigned, %f for Float (6-decimal, matches interp).
    out.push_str("@.fmt.ld = private unnamed_addr constant [5 x i8] c\"%lld\\00\"\n");
    out.push_str("@.fmt.lu = private unnamed_addr constant [5 x i8] c\"%llu\\00\"\n");
    out.push_str("@.fmt.lf = private unnamed_addr constant [3 x i8] c\"%f\\00\"\n");
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
        let msg = if matches!(t.base, Type::Record(_)) {
            format!("error: validation failed: `{}` violates its `where` clause\n", t.name)
        } else {
            format!("error: validation failed for `{}`\n", t.name)
        };
        let (escaped, len) = llvm_str(&msg);
        out.push_str(&format!(
            "@.trap.verr.{} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n",
            t.name
        ));
    }
    // Division trap messages — byte-identical to the interpreter's errors as
    // rendered by the CLI (`error: {msg}` on stderr, exit 1).
    out.push_str(
        "@.trap.div0 = private unnamed_addr constant [25 x i8] c\"error: division by zero\\0A\\00\"\n",
    );
    out.push_str(
        "@.trap.rem0 = private unnamed_addr constant [26 x i8] c\"error: remainder by zero\\0A\\00\"\n",
    );
    out.push_str(
        "@.trap.divovf = private unnamed_addr constant [37 x i8] \
         c\"error: integer overflow in division\\0A\\00\"\n",
    );
    // NaN renders as `NaN` (the interpreter's Rust formatting); UCRT's %f
    // would print `-nan(ind)`.
    out.push_str("@.fmt.nan = private unnamed_addr constant [5 x i8] c\"NaN\\0A\\00\"\n");
    out.push_str("@.str.nan = private unnamed_addr constant [4 x i8] c\"NaN\\00\"\n");

    // Input-I/O error wording (RFC-0014): canonical Vyrn strings, NEVER OS text,
    // so every backend produces byte-identical `Err` payloads. `%s` is the path;
    // the message is built at runtime (`@__vyrn_read_err`/`@__vyrn_write_err`).
    // These are payload strings (no trailing newline — unlike the trap globals).
    for (name, msg) in [
        ("@.io.readerr", "cannot read `%s`"),
        ("@.io.writeerr", "cannot write `%s`"),
        ("@.io.utf8err", "`%s` is not valid UTF-8"),
        ("@.io.nulerr", "`%s` contains a NUL byte"),
        // Byte-bridge errors (M2, no path): fixed payloads for `stringFromBytes`.
        ("@.io.bnul", "bytes contain a NUL byte"),
        ("@.io.butf8", "bytes are not valid UTF-8"),
    ] {
        let (escaped, len) = llvm_str(msg);
        out.push_str(&format!(
            "{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n"
        ));
    }
    out.push_str(IO_RUNTIME);

    // Emit one global per distinct string literal; map content -> global name.
    // Built before string collection so `jsonSchema`/`schemaOf` can seed their
    // compile-time-computed strings into the pool.
    let type_map: HashMap<String, TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
    let mut str_globals: HashMap<String, String> = HashMap::new();
    let mut literals: Vec<String> = Vec::new();
    for f in &program.functions {
        collect_strings_block(&f.body, &mut literals, &type_map);
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
    // JSON codec (RFC-0018): every constant the generated encode/decode
    // functions reference — field keys, enum variant names, `expected <what>`
    // phrases, `json.missing`/`validate` messages, and the fixed Issue keys —
    // must be in the pool before the functions are emitted.
    for s in collect_codec_strings(program, &type_map) {
        if !literals.contains(&s) {
            literals.push(s);
        }
    }
    for (i, s) in literals.iter().enumerate() {
        let name = format!("@.str.{i}");
        let (escaped, len) = llvm_str(s);
        out.push_str(&format!(
            "{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n"
        ));
        str_globals.insert(s.clone(), name);
    }
    // JSON codec (RFC-0018): a per-enum table of variant-name string pointers,
    // indexed by tag, so `toJson` on an enum reads its name in O(1).
    for t in &program.type_decls {
        if let Type::Enum(vs) = &vyrn_frontend::types::resolve(&Type::Named(t.name.clone()), &type_map)
        {
            if vyrn_frontend::codec::encodable(&Type::Named(t.name.clone()), &type_map).is_err() {
                continue;
            }
            let elems: Vec<String> = vs
                .iter()
                .map(|v| format!("ptr {}", str_globals[&v.name]))
                .collect();
            out.push_str(&format!(
                "@.enumnames.{} = private unnamed_addr constant [{} x ptr] [{}]\n",
                t.name,
                vs.len(),
                elems.join(", ")
            ));
        }
    }
    out.push('\n');

    // Compile every distinct `=~` pattern to a DFA and emit its transition table
    // and accepting-state array as globals (the runner `@__vyrn_regex_run` walks
    // them). The map lets `gen_binary` find a pattern's globals at the use site.
    let mut regex_patterns: Vec<String> = Vec::new();
    for f in &program.functions {
        collect_regex_block(&f.body, &mut regex_patterns);
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
        let table_body =
            dfa.table.iter().map(|n| format!("i32 {n}")).collect::<Vec<_>>().join(", ");
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
    let ret_types: HashMap<String, Type> =
        program.functions.iter().map(|f| (f.name.clone(), f.ret.clone())).collect();
    let param_types: HashMap<String, Vec<Type>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.ty.clone()).collect()))
        .collect();
    let param_caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.capability).collect()))
        .collect();
    // Validated-type + record declarations, for construction, Named→base
    // resolution, and record layout.
    let types: HashMap<String, TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
    // Enum variant -> (tag index, enum name), for construction.
    let mut variants: HashMap<String, (i64, String)> = HashMap::new();
    for t in &program.type_decls {
        if let Type::Enum(vs) = &t.base {
            for (i, v) in vs.iter().enumerate() {
                variants.insert(v.name.clone(), (i as i64, t.name.clone()));
            }
        }
    }

    let funcs: HashMap<String, &Function> =
        program.functions.iter().map(|f| (f.name.clone(), f)).collect();
    let empty_subst: HashMap<String, Type> = HashMap::new();

    // Monomorphization worklist. Non-generic functions are emitted once; generic
    // functions are emitted once per distinct instantiation reachable from them.
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: Vec<(String, Vec<Type>)> = Vec::new();
    // Higher-order specialization worklist and lifted-lambda dedup set (RFC-0023).
    let mut ho_queue: Vec<HoInst> = Vec::new();
    let mut lambda_emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    let enqueue = |emitted: &std::collections::HashSet<String>,
                   queue: &mut Vec<(String, Vec<Type>)>,
                   insts: Vec<(String, Vec<Type>)>| {
        for (n, args) in insts {
            let m = mangle_name(&n, &args);
            if !emitted.contains(&m) && !queue.iter().any(|(qn, qa)| mangle_name(qn, qa) == m) {
                queue.push((n, args));
            }
        }
    };

    // Whole-program ownership: which functions return owned heap values, and
    // which `let` bindings each function must free at block exit (RFC-0004 §4).
    let ownership = vyrn_frontend::own::analyze(program);
    let droppable_map = &ownership.droppable;

    let protocol_methods: HashMap<String, String> = program
        .protocols
        .iter()
        .flat_map(|p| p.methods.iter().map(|m| (m.name.clone(), p.name.clone())))
        .collect();

    // ---- module state (RFC-0013) ----------------------------------------
    // One LLVM global per binding (`@g.<name>`, `zeroinitializer`), plus a
    // synthesized `@__vyrn_globals_init()` that runs every initializer's stores
    // in declaration order (heap-valued inits — arrays, strings — work because
    // this runs at runtime). It is called from `vyrn_entry` BEFORE `main`. Reads
    // and writes elsewhere resolve through `globals_map` via `Gen::lookup`.
    let mut globals_map: HashMap<String, (String, Type)> = HashMap::new();
    let mut globals_init_ir = String::new();
    if !program.globals.is_empty() {
        let mut gi = Gen::new(
            &ret_types, &param_types, &param_caps, &types, &variants, &str_globals, &empty_subst,
            &funcs, droppable_map, &regex_globals,
        );
        gi.log_level = program.log_level;
        gi.log_sink = program.log_sink.clone();
        gi.protocol_methods = protocol_methods.clone();
        let mut decls = String::new();
        for g in &program.globals {
            let (v, vty) = gi.gen_expr(&g.init)?;
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
        enqueue(&emitted, &mut queue, insts);
        drain_ho(&mut gi, &mut out, &mut ho_queue, &mut lambda_emitted);
    }

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
        // A function that takes `fn`-typed parameters (RFC-0023) has no first-order
        // definition — it exists only as monomorphized specializations, emitted on
        // demand from the higher-order worklist. Skip its (unspecializable) shell.
        if f.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
            continue;
        }
        let sym = if f.name == "main" { "vyrn_main".to_string() } else { format!("vyrn_{}", f.name) };
        let mut gen = Gen::new(
            &ret_types, &param_types, &param_caps, &types, &variants, &str_globals, &empty_subst,
            &funcs, droppable_map, &regex_globals,
        );
        gen.log_level = program.log_level;
        gen.log_sink = program.log_sink.clone();
        gen.protocol_methods = protocol_methods.clone();
        gen.globals = globals_map.clone();
        gen.function(f, &sym, &mut out)?;
        out.push('\n');
        let insts = std::mem::take(&mut gen.instantiations);
        enqueue(&emitted, &mut queue, insts);
        drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
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
            let subst: HashMap<String, Type> =
                f.type_params.iter().cloned().zip(type_args.iter().cloned()).collect();
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals, &subst,
                &funcs, droppable_map, &regex_globals,
            );
            gen.log_level = program.log_level;
            gen.log_sink = program.log_sink.clone();
            gen.protocol_methods = protocol_methods.clone();
            gen.globals = globals_map.clone();
            gen.function(f, &sym, &mut out)?;
            out.push('\n');
            let insts = std::mem::take(&mut gen.instantiations);
            enqueue(&emitted, &mut queue, insts);
            drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
            continue;
        }
        if let Some(inst) = ho_queue.pop() {
            if !emitted.insert(inst.sym.clone()) {
                continue;
            }
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals, &inst.subst,
                &funcs, droppable_map, &regex_globals,
            );
            gen.log_level = program.log_level;
            gen.log_sink = program.log_sink.clone();
            gen.protocol_methods = protocol_methods.clone();
            gen.globals = globals_map.clone();
            gen.ho_function(&inst, &mut out)?;
            out.push('\n');
            let insts = std::mem::take(&mut gen.instantiations);
            enqueue(&emitted, &mut queue, insts);
            drain_ho(&mut gen, &mut out, &mut ho_queue, &mut lambda_emitted);
            continue;
        }
        break;
    }

    // JSON codec (RFC-0018): a per-record-type encoder/decoder, synthesized
    // like `emit_validation`. Generated for every codable named record type so
    // nested/recursive references resolve to a call (textual order is
    // immaterial to LLVM). Enums, options, arrays, and validated scalars are
    // handled inline by the emitters, so they need no standalone function.
    for t in &program.type_decls {
        let named = Type::Named(t.name.clone());
        if !matches!(vyrn_frontend::types::resolve(&named, &types), Type::Record(_)) {
            continue;
        }
        let decl = types[&t.name].clone();
        // Encoder: `@__vyrn_enc_T({llt} %v) -> ptr`.
        if vyrn_frontend::codec::encodable(&named, &types).is_ok() {
            let fields = match vyrn_frontend::types::resolve(&named, &types) {
                Type::Record(fs) => fs,
                _ => unreachable!(),
            };
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals,
                &empty_subst, &funcs, droppable_map, &regex_globals,
            );
            gen.protocol_methods = protocol_methods.clone();
            let ll = gen.llt(&named);
            let obj = gen.fresh_tmp();
            gen.emit(format!("{obj} = call ptr @__vyrn_vj_obj()"));
            for (i, f) in fields.iter().enumerate() {
                let fv = gen.fresh_tmp();
                gen.emit(format!("{fv} = extractvalue {ll} %arg0, {i}"));
                gen.emit_encode_field(&obj, &fv, f)?;
            }
            gen.emit_term(format!("ret ptr {obj}"));
            writeln!(out, "define ptr @__vyrn_enc_{}({ll} %arg0) {{", t.name).unwrap();
            out.push_str("entry:\n");
            for a in &gen.allocas {
                out.push_str(a);
                out.push('\n');
            }
            for b in &gen.body {
                out.push_str(b);
                out.push('\n');
            }
            out.push_str("}\n\n");
        }
        // Decoder: `@__vyrn_dec_T(ptr %vj, ptr %path, ptr %issues) -> {llt}`.
        if vyrn_frontend::codec::decodable(&named, &types).is_ok() {
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals,
                &empty_subst, &funcs, droppable_map, &regex_globals,
            );
            gen.protocol_methods = protocol_methods.clone();
            let ll = gen.llt(&named);
            let rec_decl = TypeDecl {
                base: vyrn_frontend::types::resolve(&named, &types),
                ..decl.clone()
            };
            let r = gen.emit_decode_record_body("%arg0", "%arg1", "%arg2", &rec_decl)?;
            gen.emit_term(format!("ret {ll} {r}"));
            writeln!(
                out,
                "define {ll} @__vyrn_dec_{}(ptr %arg0, ptr %arg1, ptr %arg2) {{",
                t.name
            )
            .unwrap();
            out.push_str("entry:\n");
            for a in &gen.allocas {
                out.push_str(a);
                out.push('\n');
            }
            for b in &gen.body {
                out.push_str(b);
                out.push('\n');
            }
            out.push_str("}\n\n");
        }
    }

    // JSON codec v2 (RFC-0024): a per-enum encode/decode function for every
    // codable named PAYLOAD enum, so a self-referential payload (through a
    // record/array/its own type) resolves to a call — the record precedent.
    // Pure-nullary enums keep their inline string encoding (no function).
    for t in &program.type_decls {
        if !t.type_params.is_empty() {
            continue; // generic enums monomorphize at concrete use sites (inline)
        }
        let named = Type::Named(t.name.clone());
        let vs = match vyrn_frontend::types::resolve(&named, &types) {
            Type::Enum(vs) if vs.iter().any(|v| !v.payload.is_empty()) => vs,
            _ => continue,
        };
        // Encoder: `@__vyrn_enc_T({ll} %arg0) -> ptr`.
        if vyrn_frontend::codec::encodable(&named, &types).is_ok() {
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals,
                &empty_subst, &funcs, droppable_map, &regex_globals,
            );
            gen.protocol_methods = protocol_methods.clone();
            let ll = gen.llt(&named);
            let r = gen.emit_encode_enum_body("%arg0", &vs, &ll)?;
            gen.emit_term(format!("ret ptr {r}"));
            writeln!(out, "define ptr @__vyrn_enc_{}({ll} %arg0) {{", t.name).unwrap();
            out.push_str("entry:\n");
            for a in &gen.allocas {
                out.push_str(a);
                out.push('\n');
            }
            for b in &gen.body {
                out.push_str(b);
                out.push('\n');
            }
            out.push_str("}\n\n");
        }
        // Decoder: `@__vyrn_dec_T(ptr %vj, ptr %path, ptr %issues) -> {ll}`.
        if vyrn_frontend::codec::decodable(&named, &types).is_ok() {
            let mut gen = Gen::new(
                &ret_types, &param_types, &param_caps, &types, &variants, &str_globals,
                &empty_subst, &funcs, droppable_map, &regex_globals,
            );
            gen.protocol_methods = protocol_methods.clone();
            let ll = gen.llt(&named);
            let expected = vyrn_frontend::codec::enum_expected(&vs);
            let r = gen.emit_decode_enum_payload("%arg0", "%arg1", "%arg2", &vs, &ll, &expected)?;
            gen.emit_term(format!("ret {ll} {r}"));
            writeln!(
                out,
                "define {ll} @__vyrn_dec_{}(ptr %arg0, ptr %arg1, ptr %arg2) {{",
                t.name
            )
            .unwrap();
            out.push_str("entry:\n");
            for a in &gen.allocas {
                out.push_str(a);
                out.push('\n');
            }
            for b in &gen.body {
                out.push_str(b);
                out.push('\n');
            }
            out.push_str("}\n\n");
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
    if file_sink {
        out.push_str("  %lfc = load ptr, ptr @__vyrn_log_file\n");
        out.push_str("  %ignore = call i32 @fclose(ptr %lfc)\n");
    }
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
    /// Per-function droppable maps for the whole program (looked up per emit).
    droppable_map: &'a HashMap<String, HashMap<usize, DropKind>>,
    /// Per-block stack of (slot register, kind) to reclaim when the block exits.
    drop_stack: Vec<Vec<(String, DropKind)>>,
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
        droppable_map: &'a HashMap<String, HashMap<usize, DropKind>>,
        regex_globals: &'a HashMap<String, (String, String, u32)>,
    ) -> Self {
        Gen {
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
            variants,
            str_globals,
            subst,
            funcs,
            instantiations: Vec::new(),
            region_depth: 0,
            droppable: HashMap::new(),
            droppable_map,
            drop_stack: Vec::new(),
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

    /// The LLVM type string for `ty`. Records lower to a `{ .. }` literal struct.
    fn llt(&self, ty: &Type) -> String {
        match self.resolve(ty) {
            Type::Int => "i64".into(),
            Type::IntN { bits, .. } => format!("i{bits}"),
            Type::Float => "double".into(),
            Type::Float32 => "float".into(),
            Type::Bool => "i1".into(),
            Type::Str => "ptr".into(),
            Type::Unit => "void".into(),
            // Option/Result both lower to { tag, payload }; payload is i64.
            // { tag, word0, word1 } — two payload words so a `Ref` (which is two
            // words) fits inline without a heap box.
            Type::Option(_) | Type::Result(..) => "{ i1, i64, i64 }".into(),
            // A generational reference is { i64 slot, i64 generation } for any T
            // (the payload is boxed), so it is a fixed-size handle.
            Type::Ref(_) => "{ i64, i64 }".into(),
            // A growable array is { ptr data, i64 len, i64 cap }.
            Type::Array(_) => "{ ptr, i64, i64 }".into(),
            // A `Map<String, V>` (RFC-0028) is two parallel growable buffers
            // sharing one length/capacity: { ptr keys, ptr values, i64 len,
            // i64 cap }. Keys are `ptr` (String); values are `llt(V)`-stride.
            Type::Map(..) => "{ ptr, ptr, i64, i64 }".into(),
            // A fixed-size array lowers to the LLVM value aggregate [N x T].
            Type::ArrayN(inner, n) => format!("[{n} x {}]", self.llt(&inner)),
            // A task handle (RFC-0025) is an opaque `ptr` to the shim's task
            // record (thread handle + heap frame); `t.join()` blocks on it and
            // loads the result from the frame's leading slot.
            Type::Task(_) => "ptr".into(),
            // A logger handle is a `ptr` to its name string.
            Type::Logger => "ptr".into(),
            Type::Record(fields) => {
                let inner: Vec<String> = fields.iter().map(|f| self.llt(&f.ty)).collect();
                format!("{{ {} }}", inner.join(", "))
            }
            // A user enum is { i64 tag, i64 payload0, ... } — one payload slot per
            // the widest variant (payloads are i64 in native).
            Type::Enum(vs) => {
                let arity = vs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
                enum_ll(arity)
            }
            // Unreachable after `resolve` (Named/App/transformers/params reduced away).
            Type::Named(_) | Type::App(..) | Type::Omit(..) | Type::Pick(..) | Type::Merge(..)
            | Type::Partial(..) | Type::Param(_) => "void".into(),
            // A function-value type (RFC-0023) is monomorphized away — no function
            // value is ever a runtime value, so this has no lowering. It is only
            // ever a parameter marker; `llt` is never asked for a real one.
            Type::Fn(..) => "void".into(),
            // `Err` is the checker's recovery sentinel; a program with any `Err`
            // already has diagnostics and never reaches codegen. Lower to void
            // as a defensive fallback (never observed in practice).
            Type::Err => "void".into(),
        }
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
            if let Type::Named(n) = to {
                if let Some(decl) = self.types.get(n).cloned() {
                    if decl.predicate.is_some() {
                        let (v, _) = self.coerce(op, from, &decl.base)?;
                        return Ok((v, to.clone()));
                    }
                }
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
        // AUTOMATIC VALIDATION: a value flowing into a predicated named type
        // coerces to its base, then runs the `where` predicate inline and traps
        // with the canonical message — mirroring the interpreter's `coerce`.
        // The exact same named type skips the check (it was validated when it
        // was constructed/coerced originally).
        if let Type::Named(n) = to {
            if from != to {
                if let Some(decl) = self.types.get(n).cloned() {
                    if decl.predicate.is_some() {
                        let (v, _) = self.coerce(op, from, &decl.base)?;
                        self.emit_validation(&decl, &v)?;
                        return Ok((v, to.clone()));
                    }
                }
            }
        }
        // An Option/Result whose payload representation changed: an array
        // literal is boxed by `Some`/`Ok`/`Err` as a fixed `[N x T]` value, but
        // the target payload is a growable `Array<T>` (e.g. `Ok([1,2,3])`
        // returned as `Result<Array<Int64>, E>`). The boxed bytes must be
        // re-materialized in the target representation, or a later `match`/`?`
        // decodes them at the wrong width (the raw elements read as a
        // `{ptr,len,cap}` header). Branch on the tag and rebuild only the arm
        // whose payload actually reshapes; the other arm — including the
        // placeholder type the constructor supplies for the unused side — keeps
        // its words untouched. (Enum construction fixes this at the source; this
        // covers the two built-in sum types, whose target is only known at the
        // outer flow.)
        {
            let (rf, rt) = (self.resolve(from), self.resolve(to));
            let arms: Option<((Type, Type), Option<(Type, Type)>)> = match (&rf, &rt) {
                (Type::Option(fa), Type::Option(ta)) => {
                    Some((((**fa).clone(), (**ta).clone()), None))
                }
                (Type::Result(fo, fe), Type::Result(to_ok, te)) => Some((
                    ((**fo).clone(), (**to_ok).clone()),
                    Some(((**fe).clone(), (**te).clone())),
                )),
                _ => None,
            };
            if let Some((one, zero)) = arms {
                let reshapes = |c: &Self, f: &Type, t: &Type| {
                    matches!((c.resolve(f), c.resolve(t)), (Type::ArrayN(..), Type::Array(_)))
                };
                let needs = reshapes(self, &one.0, &one.1)
                    || zero.as_ref().is_some_and(|(f, t)| reshapes(self, f, t));
                if needs {
                    let v = self.rebox_sum(&op, &one, zero.as_ref())?;
                    return Ok((v, to.clone()));
                }
            }
        }
        // Fixed arrays coerce element-wise (unrolled), so `[x, y]` flowing into
        // an `Array<Age, 2>` validates every element.
        if let (Type::ArrayN(fi, fnn), Type::ArrayN(ti, tn)) =
            (&self.resolve(from), &self.resolve(to))
        {
            if fi != ti && fnn == tn {
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
                    self.emit(format!("{ins} = insertvalue {to_ll} {cur}, {tell} {cv}, {i}"));
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
                if fi == ti {
                    let inner = (**fi).clone();
                    let (triple, _) = self.array_n_to_heap(&op, &inner, &rf)?;
                    return Ok((triple, to.clone()));
                }
            }
        }
        // A plain integer flowing into a sized-integer slot truncates to `iN`
        // (matching the interpreter's `wrap_intn`). Same-width is a no-op.
        if let Type::IntN { bits, .. } = self.resolve(to) {
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
            let t = self.fresh_tmp();
            self.emit(format!("{t} = fptrunc double {op} to float"));
            return Ok((t, to.clone()));
        }
        if let (Some(ff), Some(tf)) = (self.record_fields(from), self.record_fields(to)) {
            if ff == tf {
                return Ok((op, to.clone()));
            }
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
                self.emit(format!("{ins} = insertvalue {to_ll} {cur}, {field_ll} {fv}, {i}"));
                cur = ins;
            }
            return Ok((cur, to.clone()));
        }
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
    fn heap_alloc(&mut self, size: &str) -> String {
        let buf = self.fresh_tmp();
        if self.region_depth > 0 {
            self.emit(format!("{buf} = call ptr @__vyrn_region_alloc(i64 {size})"));
        } else {
            self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {size})"));
        }
        buf
    }

    /// Concatenate two `String` pointers into a fresh, NUL-terminated buffer.
    /// Shared by the `@concat` builtin (interpolation) and the `a + b` operator
    /// lowering. Routing is lexical: inside a `region` the buffer is drawn from
    /// the arena (freed at region exit); outside, from `malloc` (freed by
    /// ownership analysis if it does not escape, else leaked). The two paths are
    /// mutually exclusive, so no buffer is ever freed twice.
    fn emit_str_concat(&mut self, a: &str, b: &str) -> String {
        let la = self.fresh_tmp();
        let lb = self.fresh_tmp();
        self.emit(format!("{la} = call i64 @__vyrn_strlen(ptr {a})"));
        self.emit(format!("{lb} = call i64 @__vyrn_strlen(ptr {b})"));
        let sum = self.fresh_tmp();
        let tot = self.fresh_tmp();
        self.emit(format!("{sum} = add i64 {la}, {lb}"));
        self.emit(format!("{tot} = add i64 {sum}, 1"));
        let buf = self.heap_alloc(&tot);
        self.emit(format!("call ptr @strcpy(ptr {buf}, ptr {a})"));
        self.emit(format!("call ptr @strcat(ptr {buf}, ptr {b})"));
        buf
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
        self.emit(format!("{a} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"));
        self.emit(format!("{b} = insertvalue {{ ptr, i64, i64 }} {a}, i64 {n}, 1"));
        self.emit(format!("{c} = insertvalue {{ ptr, i64, i64 }} {b}, i64 {n}, 2"));
        Ok((c, Type::Array(Box::new(inner.clone()))))
    }

    /// Emit a conditional runtime trap: if `cond` (an i1) is true, print the
    /// message global to **stderr** (matching the interpreter's `error: ...`
    /// channel) and exit(1); otherwise fall through. `prefix` names the labels.
    fn trap_if(&mut self, cond: &str, msg_global: &str, prefix: &str) {
        let trap_l = self.fresh_label(&format!("{prefix}.trap"));
        let ok_l = self.fresh_label(&format!("{prefix}.ok"));
        self.emit_term(format!("br i1 {cond}, label %{trap_l}, label %{ok_l}"));
        self.emit_label(&trap_l);
        let e = self.fresh_tmp();
        self.emit(format!("{e} = call ptr @__vyrn_stderr()"));
        self.emit(format!("call i32 @fputs(ptr {msg_global}, ptr {e})"));
        self.emit("call void @exit(i32 1)".into());
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
        self.droppable = self.droppable_map.get(&f.name).cloned().unwrap_or_default();
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
            }
        }

        self.gen_block(&f.body)?;

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
        writeln!(out, "define {ret} @{sym}({}){} {{", params.join(", "), export_attr).unwrap();
        out.push_str("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for b in &self.body {
            out.push_str(b);
            out.push('\n');
        }
        out.push_str("}\n");
        Ok(())
    }

    fn gen_block(&mut self, block: &Block) -> Result<(), String> {
        self.scope.push(Vec::new());
        self.drop_stack.push(Vec::new());
        for stmt in &block.stmts {
            if self.terminated {
                break; // remaining statements are unreachable
            }
            self.gen_stmt(stmt)?;
        }
        // Reclaim this block's owned heap temporaries on the fall-through exit.
        // If the block already returned, these are skipped (that path leaks —
        // safe, never a double-free), matching the `region` early-exit rule.
        let drops = self.drop_stack.pop().unwrap();
        if !self.terminated {
            for (slot, kind) in drops.iter().rev() {
                self.emit_drop(slot, *kind);
            }
        }
        self.scope.pop();
        Ok(())
    }

    /// Free every owned heap temporary currently in scope (innermost block
    /// first), without popping the drop frames — used before an early `return`.
    /// The frames stay in place so the unwinding `gen_block`s see `terminated`
    /// and skip their own fall-through frees, so nothing is freed twice.
    fn emit_all_drops(&mut self) {
        let frames: Vec<Vec<(String, DropKind)>> = self.drop_stack.iter().rev().cloned().collect();
        for frame in frames {
            for (slot, kind) in frame.iter().rev() {
                self.emit_drop(slot, *kind);
            }
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
    fn emit_drop(&mut self, slot: &str, kind: DropKind) {
        match kind {
            DropKind::FreeStr => {
                let p = self.fresh_tmp();
                self.emit(format!("{p} = load ptr, ptr {slot}"));
                self.emit(format!("call void @free(ptr {p})"));
            }
            DropKind::ReleaseRef => {
                // Auto-release: validate, free the boxed payload, invalidate slot.
                let r = self.fresh_tmp();
                let s = self.fresh_tmp();
                let g = self.fresh_tmp();
                let p = self.fresh_tmp();
                self.emit(format!("{r} = load {{ i64, i64 }}, ptr {slot}"));
                self.emit(format!("{s} = extractvalue {{ i64, i64 }} {r}, 0"));
                self.emit(format!("{g} = extractvalue {{ i64, i64 }} {r}, 1"));
                self.emit(format!("call void @__vyrn_cell_check(i64 {s}, i64 {g})"));
                self.emit(format!("{p} = call ptr @__vyrn_cell_ptr(i64 {s})"));
                self.emit(format!("call void @free(ptr {p})"));
                self.emit(format!("call void @__vyrn_cell_release_slot(i64 {s})"));
            }
            DropKind::AfreeArr => {
                // Auto-afree: free the array's final backing buffer (field 0).
                let a = self.fresh_tmp();
                let d = self.fresh_tmp();
                self.emit(format!("{a} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                self.emit(format!("{d} = extractvalue {{ ptr, i64, i64 }} {a}, 0"));
                self.emit(format!("call void @free(ptr {d})"));
            }
            DropKind::FreeMap => {
                // Free both of the map's final backing buffers (keys, values);
                // elements are a safe leak, exactly as for arrays (RFC-0028).
                let a = self.fresh_tmp();
                let k = self.fresh_tmp();
                let v = self.fresh_tmp();
                self.emit(format!("{a} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
                self.emit(format!("{k} = extractvalue {{ ptr, ptr, i64, i64 }} {a}, 0"));
                self.emit(format!("{v} = extractvalue {{ ptr, ptr, i64, i64 }} {a}, 1"));
                self.emit(format!("call void @free(ptr {k})"));
                self.emit(format!("call void @free(ptr {v})"));
            }
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, value, ty: decl_ty, .. } => {
                // Node-address identity — must match `vyrn_frontend::own`, which
                // ran on this same borrowed AST.
                let key = stmt as *const Stmt as usize;
                let (v, vty) = self.gen_expr(value)?;
                // Coerce to the annotation if present (record width subtyping).
                let (v, bty) = match decl_ty {
                    Some(t) => self.coerce_flow(v, value, &vty, t)?,
                    None => (v, vty),
                };
                let ll = self.llt(&bty);
                let slot = self.declare(name, &bty);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                // If ownership analysis proved this heap binding non-escaping,
                // schedule it to be reclaimed when its block exits.
                if let Some(&kind) = self.droppable.get(&key) {
                    self.drop_stack.last_mut().unwrap().push((slot, kind));
                }
                Ok(())
            }
            Stmt::Assign { name, value, .. } => {
                let (v, vty) = self.gen_expr(value)?;
                let (slot, tty) = self.lookup(name).ok_or_else(|| format!("unbound `{name}`"))?;
                let (v, _) = self.coerce(v, &vty, &tty)?;
                let ll = self.llt(&tty);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                Ok(())
            }
            Stmt::SetField { name, field, value, .. } => {
                let (slot, tty) = self.lookup(name).ok_or_else(|| format!("unbound `{name}`"))?;
                let fields = self
                    .record_fields(&tty)
                    .ok_or_else(|| format!("`{name}` is not a record"))?;
                let (idx, fty) = fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| &f.name == field)
                    .map(|(i, f)| (i, f.ty.clone()))
                    .ok_or_else(|| format!("no field `{field}`"))?;
                let (v, vty) = self.gen_expr(value)?;
                let (v, _) = self.coerce(v, &vty, &fty)?;
                // Rebuild the record value with the new field, then store it back.
                let rec_ll = self.llt(&tty);
                let field_ll = self.llt(&fty);
                let cur = self.fresh_tmp();
                let next = self.fresh_tmp();
                self.emit(format!("{cur} = load {rec_ll}, ptr {slot}"));
                self.emit(format!("{next} = insertvalue {rec_ll} {cur}, {field_ll} {v}, {idx}"));
                self.emit(format!("store {rec_ll} {next}, ptr {slot}"));
                Ok(())
            }
            // `name[index] = value` — in-place element store (RFC-0011). The
            // read path's bounds check + `getelementptr` + `store`, with the
            // value coerced into the element type (validated element types trap
            // inline via `coerce`'s `emit_validation`). No header write-back: the
            // element lives in the shared buffer, whose `{ptr,len,cap}` is
            // unchanged. A fixed `Array<T, N>` stores straight into its stack slot.
            Stmt::IndexSet { name, index, value, .. } => {
                let (slot, aty) = self.lookup(name).ok_or_else(|| format!("unbound `{name}`"))?;
                let bad_l = self.fresh_label("set.oob");
                let ok_l = self.fresh_label("set.ok");
                match self.resolve(&aty) {
                    Type::Array(inner) => {
                        let elem = *inner;
                        let ell = self.llt(&elem);
                        let hdr = self.fresh_tmp();
                        let data = self.fresh_tmp();
                        let len = self.fresh_tmp();
                        self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                        self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"));
                        self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
                        let (iv, _) = self.gen_expr(index)?;
                        let (v, vty) = self.gen_expr(value)?;
                        let (v, _) = self.coerce(v, &vty, &elem)?;
                        let oob = self.fresh_tmp();
                        self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                        self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                        self.emit_array_oob_trap(&bad_l, &iv);
                        self.emit_label(&ok_l);
                        let ep = self.fresh_tmp();
                        self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {iv}"));
                        self.emit(format!("store {ell} {v}, ptr {ep}"));
                        Ok(())
                    }
                    Type::ArrayN(inner, n) => {
                        let elem = *inner;
                        let ell = self.llt(&elem);
                        let aggty = format!("[{n} x {ell}]");
                        let (iv, _) = self.gen_expr(index)?;
                        let (v, vty) = self.gen_expr(value)?;
                        let (v, _) = self.coerce(v, &vty, &elem)?;
                        let oob = self.fresh_tmp();
                        self.emit(format!("{oob} = icmp uge i64 {iv}, {n}"));
                        self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                        self.emit_array_oob_trap(&bad_l, &iv);
                        self.emit_label(&ok_l);
                        let ep = self.fresh_tmp();
                        self.emit(format!("{ep} = getelementptr {aggty}, ptr {slot}, i64 0, i64 {iv}"));
                        self.emit(format!("store {ell} {v}, ptr {ep}"));
                        Ok(())
                    }
                    // `m[k] = v` on a Map (RFC-0028): insert-or-update in place.
                    Type::Map(_, val) => {
                        let val = *val;
                        let (kv, _) = self.gen_expr(index)?;
                        let (v, vty) = self.gen_expr(value)?;
                        let (v, _) = self.coerce(v, &vty, &val)?;
                        self.emit_map_set(&slot, &kv, &v, &val);
                        Ok(())
                    }
                    other => Err(format!("`{name}[i] = ..` needs an Array or Map, found {other:?}")),
                }
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => {
                        let (v, vty) = self.gen_expr(e)?;
                        let ret = self.fn_ret.clone();
                        let (v, _) = self.coerce_flow(v, e, &vty, &ret)?;
                        let ll = self.llt(&ret);
                        // Free in-scope owned temporaries before leaving (the
                        // return value never aliases one — droppable bindings by
                        // definition do not escape).
                        self.emit_all_drops();
                        self.emit_modify_copyout();
                        self.emit_term(format!("ret {ll} {v}"));
                    }
                    None => {
                        self.emit_all_drops();
                        self.emit_modify_copyout();
                        self.emit_term("ret void".into());
                    }
                }
                Ok(())
            }
            Stmt::If { cond, then_block, else_block, .. } => {
                let (c, _) = self.gen_expr(cond)?;
                let then_l = self.fresh_label("then");
                let end_l = self.fresh_label("endif");
                let else_l = if else_block.is_some() {
                    self.fresh_label("else")
                } else {
                    end_l.clone()
                };
                self.emit_term(format!("br i1 {c}, label %{then_l}, label %{else_l}"));

                self.emit_label(&then_l);
                self.gen_block(then_block)?;
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
                self.gen_block(body)?;
                if !self.terminated {
                    self.emit_term(format!("br label %{cond_l}"));
                }

                self.emit_label(&end_l);
                Ok(())
            }
            Stmt::ForIn { var, iter, body, .. } => {
                // Evaluate the iterable once and snapshot a base element pointer
                // plus a length — matching the interpreter, which iterates a
                // copied element vector. Both array kinds reduce to (base T*, len).
                let (av, aty) = self.gen_expr(iter)?;
                let resolved = self.resolve(&aty);
                // Iterating a String yields each byte as an Int (loaded as i8 and
                // zero-extended); arrays load their element type directly.
                let byte_elem = resolved == Type::Str;
                let elem = match &resolved {
                    Type::Array(inner) | Type::ArrayN(inner, _) => (**inner).clone(),
                    Type::Str => Type::Int,
                    other => return Err(format!("for-loop needs an Array or String, found {other:?}")),
                };
                let ell = self.llt(&elem);
                let (data, len) = match &resolved {
                    Type::Str => {
                        let len = self.fresh_tmp();
                        self.emit(format!("{len} = call i64 @__vyrn_strlen(ptr {av})"));
                        (av.clone(), len)
                    }
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
                self.drop_stack.push(Vec::new());
                let vslot = self.declare(var, &elem);
                self.emit(format!("store {ell} {ev}, ptr {vslot}"));
                self.gen_block(body)?;
                self.drop_stack.pop();
                self.scope.pop();
                if !self.terminated {
                    let i2 = self.fresh_tmp();
                    let inext = self.fresh_tmp();
                    self.emit(format!("{i2} = load i64, ptr {idx}"));
                    self.emit(format!("{inext} = add i64 {i2}, 1"));
                    self.emit(format!("store i64 {inext}, ptr {idx}"));
                    self.emit_term(format!("br label %{cond_l}"));
                }

                self.emit_label(&end_l);
                Ok(())
            }
            Stmt::Drop { name, .. } => {
                // Explicit reclamation: free a string, afree an array, or release
                // a reference — reusing the primitives the automatic-drop analysis
                // emits. Ownership analysis escaped `name`, so there is no double
                // free, and move checking forbids using it after this point.
                let (slot, ty) =
                    self.lookup(name).ok_or_else(|| format!("drop of unbound `{name}`"))?;
                let kind = match self.resolve(&ty) {
                    Type::Str => DropKind::FreeStr,
                    Type::Array(_) => DropKind::AfreeArr,
                    Type::Map(..) => DropKind::FreeMap,
                    Type::Ref(_) => DropKind::ReleaseRef,
                    other => return Err(format!("cannot drop non-heap value of type {other:?}")),
                };
                self.emit_drop(&slot, kind);
                Ok(())
            }
            Stmt::Expr(e) => {
                self.gen_expr(e)?;
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

    /// Emit code computing `expr`; return (operand, AST type). The type is
    /// `Type::Unit` for value-less calls (`print`, Unit functions).
    fn gen_expr(&mut self, expr: &Expr) -> Result<(String, Type), String> {
        match expr {
            Expr::Int(n) => Ok((n.to_string(), Type::Int)),
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
                    // A bare function name in a value position (RFC-0037).
                    if self.funcs.contains_key(name.as_str()) {
                        return Err(rfc0037_gate(&format!("`{name}` used as a function value")));
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
                        let f = if self.resolve(&ty) == Type::Float32 { "float" } else { "double" };
                        self.emit(format!("{t} = fneg {f} {v}"))
                    }
                    UnOp::Neg if matches!(self.resolve(&ty), Type::IntN { .. }) => {
                        let w = self.llt(&ty);
                        self.emit(format!("{t} = sub {w} 0, {v}"))
                    }
                    UnOp::Neg => self.emit(format!("{t} = sub i64 0, {v}")),
                    UnOp::Not => self.emit(format!("{t} = xor i1 {v}, true")),
                }
                Ok((t, ty))
            }
            Expr::Binary { op, lhs, rhs, .. } => self.gen_binary(*op, lhs, rhs),
            Expr::Call { name, args, .. } => self.gen_call(name, args),
            Expr::Match { scrutinee, arms, .. } => self.gen_match(scrutinee, arms),
            Expr::IfExpr { cond, then_branch, else_branch, .. } => {
                self.gen_if_expr(cond, then_branch, else_branch.as_deref())
            }
            Expr::Try { expr, .. } => self.gen_try(expr),
            Expr::StructLit { name, fields, .. } => self.gen_struct_lit(name, fields),
            Expr::Field { expr, field, .. } => {
                let (v, ety) = self.gen_expr(expr)?;
                // `arr.length` is the element count (sugar for `alen`): a constant
                // for a fixed array, field 1 of the `{ptr,len,cap}` triple otherwise.
                if field == "length" {
                    match self.resolve(&ety) {
                        Type::ArrayN(_, n) => return Ok((format!("{n}"), Type::Int)),
                        Type::Array(_) => {
                            let len = self.fresh_tmp();
                            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {v}, 1"));
                            return Ok((len, Type::Int));
                        }
                        // `map.length` is the entry count (field 2 of the header).
                        Type::Map(..) => {
                            let len = self.fresh_tmp();
                            self.emit(format!(
                                "{len} = extractvalue {{ ptr, ptr, i64, i64 }} {v}, 2"
                            ));
                            return Ok((len, Type::Int));
                        }
                        // `str.length` is the byte length via `strlen`.
                        Type::Str => {
                            let len = self.fresh_tmp();
                            self.emit(format!("{len} = call i64 @__vyrn_strlen(ptr {v})"));
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
                // triple `array()` produces (the element type is placeholder; the
                // representation is type-independent and the annotation fixes it).
                if elems.is_empty() {
                    return Ok((
                        "{ ptr null, i64 0, i64 0 }".into(),
                        Type::Array(Box::new(Type::Int)),
                    ));
                }
                // Build the [N x T] value aggregate by inserting each element.
                let (v0, ety) = self.gen_expr(&elems[0])?;
                let ell = self.llt(&ety);
                let aty = format!("[{} x {ell}]", elems.len());
                let mut cur = self.fresh_tmp();
                self.emit(format!("{cur} = insertvalue {aty} undef, {ell} {v0}, 0"));
                for (i, e) in elems.iter().enumerate().skip(1) {
                    let (v, vt) = self.gen_expr(e)?;
                    let (v, _) = self.coerce(v, &vt, &ety)?;
                    let next = self.fresh_tmp();
                    self.emit(format!("{next} = insertvalue {aty} {cur}, {ell} {v}, {i}"));
                    cur = next;
                }
                Ok((cur, Type::ArrayN(Box::new(ety), elems.len())))
            }
            // A map literal (RFC-0028): `[:]` is the empty `{ptr,ptr,len,cap}`
            // (buffers null, value type from context — the representation is
            // type-independent). A non-empty literal builds the map in a temp
            // slot via the same insert-or-update path as `m[k] = v`, so a
            // repeated key updates in place (keeps its slot), matching the
            // interpreter. The value type is inferred from the first value; a
            // validated declared type re-validates through the binding coercion.
            Expr::MapLit { entries, .. } => {
                if entries.is_empty() {
                    return Ok((
                        "{ ptr null, ptr null, i64 0, i64 0 }".into(),
                        Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
                    ));
                }
                let slot = self.fresh_alloca("{ ptr, ptr, i64, i64 }");
                self.emit(format!(
                    "store {{ ptr, ptr, i64, i64 }} {{ ptr null, ptr null, i64 0, i64 0 }}, ptr {slot}"
                ));
                let (kv0, _) = self.gen_expr(&entries[0].0)?;
                let (v0, vty0) = self.gen_expr(&entries[0].1)?;
                let val = vty0;
                self.emit_map_set(&slot, &kv0, &v0, &val);
                for (ke, ve) in entries.iter().skip(1) {
                    let (kv, _) = self.gen_expr(ke)?;
                    let (v, vt) = self.gen_expr(ve)?;
                    let (v, _) = self.coerce(v, &vt, &val)?;
                    self.emit_map_set(&slot, &kv, &v, &val);
                }
                let agg = self.fresh_tmp();
                self.emit(format!("{agg} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
                Ok((agg, Type::Map(Box::new(Type::Str), Box::new(val))))
            }
            // A lambda literal in a v1 argument position is monomorphized away
            // at the call site that receives it (RFC-0023); one reaching the
            // general expression path is an RFC-0037 storage source this
            // backend cannot lower yet.
            Expr::Lambda { .. } => {
                Err(rfc0037_gate("a lambda literal in a storage position"))
            }
        }
    }

    /// Fallible validated construction `Age?(n)` → `Option<Age>` (`{ i1, i64 }`):
    /// tag is the refinement result, payload is the value.
    fn gen_try_construct(&mut self, name: &str, arg: &Expr) -> Result<(String, Type), String> {
        let decl = self
            .types
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown type `{name}`"))?;
        if self.resolve(&decl.base) != Type::Int {
            return Err(format!(
                "native fallible construction supports Int64-based types only (`{name}`); use `vyrn run`"
            ));
        }
        let (v, _) = self.gen_expr(arg)?;
        let pred_i1 = match &decl.predicate {
            None => "true".to_string(),
            Some(pred) => {
                self.scope.push(Vec::new());
                let slot = self.declare("value", &decl.base);
                self.emit(format!("store i64 {v}, ptr {slot}"));
                let (cond, _) = self.gen_expr(pred)?;
                self.scope.pop();
                cond
            }
        };
        // The payload is a validated Int (Age → Int), so it lives in word 0.
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {pred_i1}, 0"));
        self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {v}, 1"));
        self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 0, 2"));
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
        let type_params = self.types.get(name).map(|d| d.type_params.clone()).unwrap_or_default();

        // Emit each field value in declared order; infer generic parameters.
        let mut solved: HashMap<String, Type> = HashMap::new();
        let mut vals: Vec<(String, Type)> = Vec::new();
        for decl_f in &rfields {
            let (_, value_expr) = fields
                .iter()
                .find(|(fname, _)| fname == &decl_f.name)
                .ok_or_else(|| format!("missing field `{}`", decl_f.name))?;
            let (v, vty) = self.gen_expr(value_expr)?;
            solve_param(&decl_f.ty, &vty, &mut solved);
            vals.push((v, vty));
        }

        // The concrete result type (generic parameters filled in).
        let result_ty = if type_params.is_empty() {
            Type::Named(name.to_string())
        } else {
            let args = type_params
                .iter()
                .map(|tp| solved.get(tp).cloned().unwrap_or(Type::Unit))
                .collect();
            Type::App(name.to_string(), args)
        };
        let ll = self.llt(&result_ty);

        let mut cur = "undef".to_string();
        let mut coerced: Vec<(String, String, Type)> = Vec::new();
        for (i, decl_f) in rfields.iter().enumerate() {
            let (v, vty) = vals[i].clone();
            let field_ty = vyrn_frontend::types::substitute(&decl_f.ty, &solved);
            // The field's source expression (for the RFC-0020 containment skip).
            let field_expr = fields.iter().find(|(fname, _)| fname == &decl_f.name).map(|(_, e)| e);
            let (v, _) = match field_expr {
                Some(e) => self.coerce_flow(v, e, &vty, &field_ty)?,
                None => self.coerce(v, &vty, &field_ty)?,
            };
            let field_ll = self.llt(&field_ty);
            let ins = self.fresh_tmp();
            self.emit(format!("{ins} = insertvalue {ll} {cur}, {field_ll} {v}, {i}"));
            cur = ins;
            coerced.push((decl_f.name.clone(), v, field_ty));
        }

        // Enforce a cross-field `where` invariant at runtime. As with scalar
        // construction, an all-constant literal is validated by the checker and
        // needs no runtime check.
        if let Some(decl) = self.types.get(name).cloned() {
            if let Some(pred) = &decl.predicate {
                let all_const = fields.iter().all(|(_, e)| {
                    vyrn_frontend::consteval::eval(e, &HashMap::new()).is_some()
                });
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

    /// Lower a `match` over an Option/Result to a tag test + `phi`. Payloads are
    /// i64 (native restriction), so bindings are i64 locals. The `Some`/`Ok` arm
    /// has tag 1; the `None`/`Err` arm has tag 0.
    fn gen_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<(String, Type), String> {
        let (sv, sty) = self.gen_expr(scrutinee)?;
        // A user enum dispatches to the switch-based path.
        if let Type::Enum(evs) = self.resolve(&sty) {
            return self.gen_match_enum(&sv, &evs, arms);
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
        let one_arm = arms.iter().find(|a| pattern_is_one(&a.pattern)).unwrap();
        let (one_val, ty) = self.gen_arm_body(&sv, one_arm, &one_ty)?;
        let one_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // tag == 0 arm (None / Err)
        self.emit_label(&zero_l);
        let zero_arm = arms.iter().find(|a| !pattern_is_one(&a.pattern)).unwrap();
        let (zero_val, _) = self.gen_arm_body(&sv, zero_arm, &zero_ty)?;
        let zero_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // merge — a statement-position match with Unit arms (side effects only)
        // has no value to merge, and `phi void` is invalid IR.
        self.emit_label(&end_l);
        let ll = self.llt(&ty);
        if ll == "void" {
            return Ok((String::new(), ty));
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
        cond: &Expr,
        then_branch: &Expr,
        else_branch: Option<&Expr>,
    ) -> Result<(String, Type), String> {
        let else_branch = else_branch
            .ok_or("internal: `if` expression without `else` reached codegen")?;
        let (c, _) = self.gen_expr(cond)?;
        let then_l = self.fresh_label("ie.then");
        let else_l = self.fresh_label("ie.else");
        let end_l = self.fresh_label("ie.end");
        self.emit_term(format!("br i1 {c}, label %{then_l}, label %{else_l}"));

        // then branch
        self.emit_label(&then_l);
        let (then_val, ty) = self.gen_expr(then_branch)?;
        // The predecessor of the join is the CURRENT block — a nested if/match in
        // the branch body may have moved us past `then_l`.
        let then_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // else branch
        self.emit_label(&else_l);
        let (else_val, _) = self.gen_expr(else_branch)?;
        let else_end = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        // merge
        self.emit_label(&end_l);
        let ll = self.llt(&ty);
        if ll == "void" {
            return Ok((String::new(), ty));
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
    ) -> Result<(String, Type), String> {
        let arity = evs.iter().map(|v| v.payload.len()).max().unwrap_or(0);
        let ell = enum_ll(arity);
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ell} {sv}, 0"));
        let end_l = self.fresh_label("me.end");
        let default_l = self.fresh_label("me.default");

        // One block per arm; map each arm to its variant's tag index.
        let mut arm_labels: Vec<(usize, String)> = Vec::new();
        for arm in arms {
            let vname = match &arm.pattern {
                Pattern::Variant(n, _) => n,
                _ => return Err("non-variant pattern in enum match".into()),
            };
            let idx = evs
                .iter()
                .position(|v| &v.name == vname)
                .ok_or_else(|| format!("unknown variant `{vname}`"))?;
            arm_labels.push((idx, self.fresh_label("me.arm")));
        }
        let cases: String = arm_labels
            .iter()
            .map(|(idx, lbl)| format!("i64 {idx}, label %{lbl}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.emit_term(format!("switch i64 {tag}, label %{default_l} [ {cases} ]"));

        let mut incoming: Vec<(String, String)> = Vec::new();
        let mut ty = Type::Unit;
        for (arm, (idx, lbl)) in arms.iter().zip(&arm_labels) {
            self.emit_label(lbl);
            self.scope.push(Vec::new());
            if let Pattern::Variant(_, binds) = &arm.pattern {
                let payload_tys = &evs[*idx].payload;
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
            let (v, t) = self.gen_expr(&arm.body)?;
            ty = t;
            self.scope.pop();
            let block = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            incoming.push((v, block));
        }

        // Exhaustiveness is checked, so the default is unreachable.
        self.emit_label(&default_l);
        self.emit_term("unreachable".into());

        self.emit_label(&end_l);
        let ll = self.llt(&ty);
        // Unit-typed arms (side effects only) have no value — `phi void` is
        // invalid IR, so skip the merge entirely.
        if ll == "void" {
            return Ok((String::new(), ty));
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
        self.emit(format!("{size} = ptrtoint ptr getelementptr ({ll}, ptr null, i64 1) to i64"));
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
            Type::Ref(_) => {
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
            Type::Ref(_) => {
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!("{a} = insertvalue {{ i64, i64 }} undef, i64 {w0}, 0"));
                self.emit(format!("{b} = insertvalue {{ i64, i64 }} {a}, i64 {w1}, 1"));
                b
            }
            _ => self.unbox_payload(w0, ty),
        }
    }

    /// Rebuild an Option/Result aggregate `{i1,i64,i64}` so each arm's payload
    /// is re-encoded from its old declared type into the new one. `one` is the
    /// tag-1 (`Some`/`Ok`) payload; `zero` the tag-0 (`Err`) payload, absent for
    /// `Option` (its `None` arm carries nothing). The tag is a single bit, so
    /// this is always a two-way branch; each arm is rebuilt only when its
    /// representation actually reshapes (see [`Self::rebuild_arm`]).
    fn rebox_sum(
        &mut self,
        agg: &str,
        one: &(Type, Type),
        zero: Option<&(Type, Type)>,
    ) -> Result<String, String> {
        let tagv = self.fresh_tmp();
        self.emit(format!("{tagv} = extractvalue {{ i1, i64, i64 }} {agg}, 0"));
        let one_l = self.fresh_label("rebox.one");
        let zero_l = self.fresh_label("rebox.zero");
        let end_l = self.fresh_label("rebox.end");
        self.emit_term(format!("br i1 {tagv}, label %{one_l}, label %{zero_l}"));

        self.emit_label(&one_l);
        let one_v = self.rebuild_arm(agg, 1, &one.0, &one.1)?;
        let one_b = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        self.emit_label(&zero_l);
        let zero_v = match zero {
            Some((f, t)) => self.rebuild_arm(agg, 0, f, t)?,
            None => agg.to_string(),
        };
        let zero_b = self.cur_block.clone();
        self.emit_term(format!("br label %{end_l}"));

        self.emit_label(&end_l);
        let res = self.fresh_tmp();
        self.emit(format!(
            "{res} = phi {{ i1, i64, i64 }} [ {one_v}, %{one_b} ], [ {zero_v}, %{zero_b} ]"
        ));
        Ok(res)
    }

    /// Re-encode one Option/Result arm's payload from `from` to `to`. When the
    /// representation is unchanged — including the constructor's placeholder
    /// type on the arm this value never actually is — the aggregate is returned
    /// untouched, so no bogus scalar⇄heap coercion is emitted on a dead arm.
    /// Only the `ArrayN → Array` reshape (the sole shape an array *literal* takes
    /// before coercion) is materialized.
    fn rebuild_arm(
        &mut self,
        agg: &str,
        tag: i64,
        from: &Type,
        to: &Type,
    ) -> Result<String, String> {
        if !matches!((self.resolve(from), self.resolve(to)), (Type::ArrayN(..), Type::Array(_))) {
            return Ok(agg.to_string());
        }
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {agg}, 1"));
        self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {agg}, 2"));
        let v = self.decode_payload(&w0, &w1, from);
        let (cv, _) = self.coerce(v, from, to)?;
        let (nw0, nw1) = self.encode_payload(&cv, to);
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {tag}, 0"));
        self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {nw0}, 1"));
        self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {nw1}, 2"));
        Ok(c)
    }

    /// Emit an arm body, binding the payload (decoded to `payload_ty`) if the
    /// pattern binds a name.
    fn gen_arm_body(
        &mut self,
        sv: &str,
        arm: &MatchArm,
        payload_ty: &Type,
    ) -> Result<(String, Type), String> {
        self.scope.push(Vec::new());
        if let Some(bind) = pattern_binding(&arm.pattern) {
            let w0 = self.fresh_tmp();
            let w1 = self.fresh_tmp();
            self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {sv}, 1"));
            self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {sv}, 2"));
            let v = self.decode_payload(&w0, &w1, payload_ty);
            let ll = self.llt(payload_ty);
            let slot = self.declare(bind, payload_ty);
            self.emit(format!("store {ll} {v}, ptr {slot}"));
        }
        let out = self.gen_expr(&arm.body)?;
        self.scope.pop();
        Ok(out)
    }

    /// Lower `expr?`: on `None`/`Err` (tag 0) return the aggregate as the
    /// function's result; otherwise continue with the unwrapped i64 payload.
    fn gen_try(&mut self, expr: &Expr) -> Result<(String, Type), String> {
        let (agg, aty) = self.gen_expr(expr)?;
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
        self.emit_all_drops();
        self.emit_modify_copyout();
        self.emit_term(format!("ret {{ i1, i64, i64 }} {agg}"));

        self.emit_label(&ok_l);
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {agg}, 1"));
        self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {agg}, 2"));
        let v = self.decode_payload(&w0, &w1, &ok_ty);
        Ok((v, ok_ty))
    }

    fn gen_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<(String, Type), String> {
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
            self.emit(format!("{t} = phi i1 [ {short}, %{pre} ], [ {r}, %{rblock} ]"));
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
        if matches!(op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
            && self.resolve(&lty) == Type::Str
        {
            let c = self.fresh_tmp();
            self.emit(format!("{c} = call i32 @strcmp(ptr {l}, ptr {r})"));
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

        // `a + b` on two Strings is concatenation (replacing `concat`): the same
        // heap allocation, region routing, and drop analysis.
        if op == BinOp::Add && self.resolve(&lty) == Type::Str {
            let buf = self.emit_str_concat(&l, &r);
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
        let instr = if matches!(self.resolve(&lty), Type::Float | Type::Float32) {
            // Floating-point ops (Float64 → `double`, Float32 → `float`).
            let f = if self.resolve(&lty) == Type::Float32 { "float" } else { "double" };
            match op {
                BinOp::Add => format!("{t} = fadd {f} {l}, {r}"),
                BinOp::Sub => format!("{t} = fsub {f} {l}, {r}"),
                BinOp::Mul => format!("{t} = fmul {f} {l}, {r}"),
                BinOp::Div => format!("{t} = fdiv {f} {l}, {r}"),
                BinOp::Lt => format!("{t} = fcmp olt {f} {l}, {r}"),
                BinOp::LtEq => format!("{t} = fcmp ole {f} {l}, {r}"),
                BinOp::Gt => format!("{t} = fcmp ogt {f} {l}, {r}"),
                BinOp::GtEq => format!("{t} = fcmp oge {f} {l}, {r}"),
                BinOp::Eq => format!("{t} = fcmp oeq {f} {l}, {r}"),
                BinOp::NotEq => format!("{t} = fcmp one {f} {l}, {r}"),
                BinOp::Rem | BinOp::And | BinOp::Or | BinOp::Match => {
                    return Err("`%`/`&&`/`||`/`=~` are not valid on Float64".into())
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
                let msg = if op == BinOp::Div { "@.trap.div0" } else { "@.trap.rem0" };
                self.trap_if(&z, msg, "div.z");
                if !unsigned {
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
            match op {
                BinOp::Add => format!("{t} = add {ll} {l}, {r}"),
                BinOp::Sub => format!("{t} = sub {ll} {l}, {r}"),
                BinOp::Mul => format!("{t} = mul {ll} {l}, {r}"),
                BinOp::Div if unsigned => format!("{t} = udiv {ll} {l}, {r}"),
                BinOp::Div => format!("{t} = sdiv {ll} {l}, {r}"),
                BinOp::Rem if unsigned => format!("{t} = urem {ll} {l}, {r}"),
                BinOp::Rem => format!("{t} = srem {ll} {l}, {r}"),
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
                BinOp::And | BinOp::Or | BinOp::Match => unreachable!("handled above"),
            }
        };
        self.emit(instr);
        let result_ty = match op {
            // Arithmetic keeps the operand's numeric type (Int, Float, or IntN).
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => numty,
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
            (true, false) => {
                let op = if to_unsigned { "fptoui" } else { "fptosi" };
                self.emit(format!("{t} = {op} {fll} {v} to {tll}"));
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
        let e = self.fresh_tmp();
        self.emit(format!("{e} = call ptr @__vyrn_stderr()"));
        self.emit(format!(
            "call i32 (ptr, ptr, ...) @fprintf(ptr {e}, ptr @.trap.aoob, i64 {iv})"
        ));
        self.emit("call void @exit(i32 1)".into());
        self.emit_term("unreachable".into());
    }

    /// Insert-or-update `key`→`v` into the Map whose `{keys,vals,len,cap}` header
    /// lives at alloca `slot` (RFC-0028). A hit overwrites the value in its slot
    /// (order preserved); a miss reserves room (may realloc both buffers),
    /// appends key and value, and bumps the shared length. `val` is the value
    /// type; `v` is already coerced into it.
    fn emit_map_set(&mut self, slot: &str, key: &str, v: &str, val: &Type) {
        let vll = self.llt(val);
        let esz = self.fresh_tmp();
        self.emit(format!(
            "{esz} = ptrtoint ptr getelementptr ({vll}, ptr null, i64 1) to i64"
        ));
        let hdr = self.fresh_tmp();
        let keys = self.fresh_tmp();
        let len = self.fresh_tmp();
        self.emit(format!("{hdr} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
        self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 0"));
        self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 2"));
        let idx = self.fresh_tmp();
        self.emit(format!(
            "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {key})"
        ));
        let found = self.fresh_tmp();
        self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
        let upd_l = self.fresh_label("map.set.upd");
        let ins_l = self.fresh_label("map.set.ins");
        let done_l = self.fresh_label("map.set.done");
        self.emit_term(format!("br i1 {found}, label %{upd_l}, label %{ins_l}"));
        // update: store into the existing value slot.
        self.emit_label(&upd_l);
        let vals0 = self.fresh_tmp();
        self.emit(format!("{vals0} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 1"));
        let ep0 = self.fresh_tmp();
        self.emit(format!("{ep0} = getelementptr {vll}, ptr {vals0}, i64 {idx}"));
        self.emit(format!("store {vll} {v}, ptr {ep0}"));
        self.emit_term(format!("br label %{done_l}"));
        // insert: reserve (may realloc both buffers), reload, append, len += 1.
        self.emit_label(&ins_l);
        self.emit(format!("call void @__vyrn_map_reserve(ptr {slot}, i64 {esz})"));
        let hdr2 = self.fresh_tmp();
        let keys2 = self.fresh_tmp();
        let vals2 = self.fresh_tmp();
        self.emit(format!("{hdr2} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
        self.emit(format!("{keys2} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr2}, 0"));
        self.emit(format!("{vals2} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr2}, 1"));
        let kep = self.fresh_tmp();
        self.emit(format!("{kep} = getelementptr ptr, ptr {keys2}, i64 {len}"));
        self.emit(format!("store ptr {key}, ptr {kep}"));
        let vep = self.fresh_tmp();
        self.emit(format!("{vep} = getelementptr {vll}, ptr {vals2}, i64 {len}"));
        self.emit(format!("store {vll} {v}, ptr {vep}"));
        let nl = self.fresh_tmp();
        self.emit(format!("{nl} = add i64 {len}, 1"));
        let lenp = self.fresh_tmp();
        self.emit(format!(
            "{lenp} = getelementptr {{ ptr, ptr, i64, i64 }}, ptr {slot}, i64 0, i32 2"
        ));
        self.emit(format!("store i64 {nl}, ptr {lenp}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
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
        for (i, p) in callee.params.iter().enumerate() {
            if matches!(p.ty, Type::Fn(..)) {
                continue;
            }
            let (v, vty) = self.gen_expr(&args[i])?;
            let aty = vyrn_frontend::types::substitute(&vty, self.subst);
            if generic {
                solve_param(&p.ty, &aty, &mut call_subst);
            }
            let pty = vyrn_frontend::types::substitute(&p.ty, &call_subst);
            let (v, cty) = self.coerce(v, &aty, &pty)?;
            nonfn_ops.push(format!("{} {v}", self.llt(&cty)));
        }
        // Resolve each `fn`-typed argument: lift/forward the target and evaluate
        // its captures now.
        let mut bindings: Vec<HoParamBinding> = Vec::new();
        let mut capture_ops: Vec<String> = Vec::new();
        for (i, p) in callee.params.iter().enumerate() {
            let Type::Fn(dptys, dret) = &p.ty else { continue };
            // The parameter's `fn` type with type parameters filled in from pass 1.
            let ptys: Vec<Type> = dptys
                .iter()
                .map(|t| vyrn_frontend::types::substitute(t, &call_subst))
                .collect();
            let dret_sub = vyrn_frontend::types::substitute(dret, &call_subst);
            let (target_sym, capture_tys, target_ret) =
                self.resolve_fn_arg(&args[i], &ptys, &dret_sub, &mut capture_ops)?;
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
        let mut sym = format!("vyrn_{name}__ho");
        for ta in &type_args {
            sym.push('_');
            sym.push_str(&mangle_ty(ta));
        }
        for b in &bindings {
            sym.push('_');
            sym.push_str(&sanitize(&b.target_sym));
        }
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

    /// Resolve one `fn`-typed argument to a call target (RFC-0023), emitting any
    /// capture loads into `capture_ops`. Returns (target symbol, capture types,
    /// the target's concrete return type).
    fn resolve_fn_arg(
        &mut self,
        arg: &Expr,
        ptys: &[Type],
        expected_ret: &Type,
        capture_ops: &mut Vec<String>,
    ) -> Result<(String, Vec<Type>, Type), String> {
        match arg {
            Expr::Lambda { params, body, .. } => {
                // Free (captured) locals, in first-seen order.
                let locals: std::collections::HashSet<String> = params.iter().cloned().collect();
                let cap_names = self.lambda_captures(body, locals);
                let mut cap_tys = Vec::new();
                for cn in &cap_names {
                    let (slot, ty) = self
                        .lookup(cn)
                        .ok_or_else(|| format!("captured `{cn}` not in scope"))?;
                    let cty = vyrn_frontend::types::substitute(&ty, self.subst);
                    let ll = self.llt(&cty);
                    let v = self.fresh_tmp();
                    self.emit(format!("{v} = load {ll}, ptr {slot}"));
                    capture_ops.push(format!("{ll} {v}"));
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
                // A named top-level function: call it directly, no captures.
                let ret = self.ret_types.get(vn).cloned().unwrap_or(Type::Unit);
                Ok((format!("vyrn_{vn}"), Vec::new(), ret))
            }
            _ => Err("internal: unexpected `fn`-typed argument".into()),
        }
    }

    /// The captured (free) local variables of a lambda body (RFC-0023), in
    /// first-seen order: names read in the body that are neither the lambda's own
    /// parameters/locals nor module state nor functions — i.e. bindings that live
    /// in the enclosing local scope.
    fn lambda_captures(
        &self,
        body: &LambdaBody,
        locals: std::collections::HashSet<String>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut locals = locals;
        match body {
            LambdaBody::Expr(e) => self.captures_of_expr(e, &mut locals, &mut out, &mut seen),
            LambdaBody::Block(b) => self.captures_of_block(b, &mut locals, &mut out, &mut seen),
        }
        out
    }

    fn captures_of_block(
        &self,
        b: &Block,
        locals: &mut std::collections::HashSet<String>,
        out: &mut Vec<String>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        for s in &b.stmts {
            match s {
                Stmt::Let { name, value, .. } => {
                    self.captures_of_expr(value, locals, out, seen);
                    locals.insert(name.clone());
                }
                Stmt::Assign { value, .. }
                | Stmt::SetField { value, .. }
                | Stmt::Expr(value)
                | Stmt::Return { value: Some(value), .. } => {
                    self.captures_of_expr(value, locals, out, seen)
                }
                Stmt::IndexSet { index, value, .. } => {
                    self.captures_of_expr(index, locals, out, seen);
                    self.captures_of_expr(value, locals, out, seen);
                }
                Stmt::If { cond, then_block, else_block, .. } => {
                    self.captures_of_expr(cond, locals, out, seen);
                    self.captures_of_block(then_block, &mut locals.clone(), out, seen);
                    if let Some(eb) = else_block {
                        self.captures_of_block(eb, &mut locals.clone(), out, seen);
                    }
                }
                Stmt::While { cond, body, .. } => {
                    self.captures_of_expr(cond, locals, out, seen);
                    self.captures_of_block(body, &mut locals.clone(), out, seen);
                }
                Stmt::ForIn { var, iter, body, .. } => {
                    self.captures_of_expr(iter, locals, out, seen);
                    let mut inner = locals.clone();
                    inner.insert(var.clone());
                    self.captures_of_block(body, &mut inner, out, seen);
                }
                Stmt::Region { body, .. } => {
                    self.captures_of_block(body, &mut locals.clone(), out, seen)
                }
                Stmt::Return { value: None, .. } | Stmt::Drop { .. } => {}
            }
        }
    }

    fn captures_of_expr(
        &self,
        e: &Expr,
        locals: &mut std::collections::HashSet<String>,
        out: &mut Vec<String>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        match e {
            Expr::Var { name, .. } => {
                if locals.contains(name) || seen.contains(name) {
                    return;
                }
                // Only an enclosing LOCAL slot is a capture — module state and
                // functions/variants are reached directly by the lifted function.
                let is_local = self.scope.iter().any(|f| f.iter().any(|(n, _, _)| n == name));
                if is_local {
                    seen.insert(name.clone());
                    out.push(name.clone());
                }
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                self.captures_of_expr(expr, locals, out, seen)
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.captures_of_expr(lhs, locals, out, seen);
                self.captures_of_expr(rhs, locals, out, seen);
            }
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => {
                for a in args {
                    self.captures_of_expr(a, locals, out, seen);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.captures_of_expr(scrutinee, locals, out, seen);
                for arm in arms {
                    let mut inner = locals.clone();
                    for b in vyrn_frontend::pattern_bindings(&arm.pattern) {
                        inner.insert(b.to_string());
                    }
                    self.captures_of_expr(&arm.body, &mut inner, out, seen);
                }
            }
            Expr::IfExpr { cond, then_branch, else_branch, .. } => {
                self.captures_of_expr(cond, locals, out, seen);
                self.captures_of_expr(then_branch, locals, out, seen);
                if let Some(eb) = else_branch {
                    self.captures_of_expr(eb, locals, out, seen);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.captures_of_expr(v, locals, out, seen);
                }
            }
            _ => {}
        }
    }

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
        let sym = format!("__vyrn_lambda_{}_{ordinal}_{shape}", sanitize(&self.cur_fn_name));

        // Save the current emission state; emit the lambda into fresh buffers.
        let saved_allocas = std::mem::take(&mut self.allocas);
        let saved_body = std::mem::take(&mut self.body);
        let saved_scope = std::mem::replace(&mut self.scope, vec![Vec::new()]);
        let saved_block = std::mem::replace(&mut self.cur_block, "entry".to_string());
        let saved_term = std::mem::replace(&mut self.terminated, false);
        let saved_ret = self.fn_ret.clone();
        let saved_tmp = self.tmp;
        let saved_label = self.label;
        let saved_drop = std::mem::take(&mut self.drop_stack);
        let saved_droppable = std::mem::take(&mut self.droppable);
        let saved_modify = std::mem::take(&mut self.modify_copyout);
        let saved_bindings = std::mem::take(&mut self.fn_bindings);
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
                let (v, vty) = self.gen_expr(e)?;
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
        self.drop_stack = saved_drop;
        self.droppable = saved_droppable;
        self.modify_copyout = saved_modify;
        self.fn_bindings = saved_bindings;

        self.lambda_defs.push((sym.clone(), def));
        Ok((sym, ret_ty))
    }

    /// Emit a specialized instance of a higher-order function (RFC-0023): its
    /// ordinary parameters, then the capture parameters for each `fn`-typed
    /// parameter, with `fn_bindings` wired so calls to those parameters become
    /// direct calls to their targets.
    fn ho_function(&mut self, inst: &HoInst, out: &mut String) -> Result<(), String> {
        let callee: &Function = self.funcs[inst.name.as_str()];
        self.cur_fn_name = inst.name.clone();
        self.lambda_counter = 0;
        self.droppable = self.droppable_map.get(&inst.name).cloned().unwrap_or_default();
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
        for b in &self.body {
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

    fn gen_call(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
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
                self.emit(format!("call void @{}({})", b.target_sym, arg_ops.join(", ")));
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
        // A call through a stored fn-typed binding (RFC-0037) — not yet lowered.
        if let Some((_, ty)) = self.lookup(name) {
            if matches!(self.resolve(&ty), Type::Fn(..)) {
                return Err(rfc0037_gate(&format!("a call through the stored value `{name}`")));
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
        // `toJson(x)` (RFC-0018): encode `x` into a JSON DOM node, then render it
        // canonically via the shim's stringifier (which owns escaping + number
        // formatting, so the bytes match the interpreter's `scalar_to_string`).
        if name == "toJson" {
            let (v, vty) = self.gen_expr(&args[0])?;
            let node = self.emit_encode(&v, &vty)?;
            let s = self.fresh_tmp();
            self.emit(format!("{s} = call ptr @__vyrn_vj_encode(ptr {node})"));
            return Ok((s, Type::Str));
        }
        // `fromJson(TypeName, s)` (RFC-0018): parse, decode, and package into a
        // `Validation<T>` — `Valid(T)` if no Issue accumulated, else
        // `Invalid([Issue])` built from the shim's issue list.
        if name == "fromJson" {
            let tn = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => tn.clone(),
                _ => return Err("`fromJson` needs a declared type name".to_string()),
            };
            let (s, _) = self.gen_expr(&args[1])?;
            return self.gen_from_json(&tn, &s);
        }
        // Numeric conversion `Int32(x)`, `Float64(x)`, ...
        if let Some(target) = vyrn_frontend::types::numeric_conv_target(name) {
            if args.len() == 1 {
                let (v, sty) = self.gen_expr(&args[0])?;
                return self.gen_numeric_conv(v, &sty, &target);
            }
        }
        if name == "print" {
            let (v, ty) = self.gen_expr(&args[0])?;
            match self.resolve(&ty) {
                Type::Bool => {
                    // select the "true"/"false" format string, matching interp
                    let fmt = self.fresh_tmp();
                    self.emit(format!("{fmt} = select i1 {v}, ptr @.fmt.true, ptr @.fmt.false"));
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr {fmt})"));
                }
                Type::Str => {
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr @.fmt.s, ptr {v})"));
                }
                // Float: 6-decimal `%f\n`, matching the interpreter's `{:.6}`.
                // NaN is special-cased: UCRT's %f renders it `-nan(ind)` while
                // the interpreter prints `NaN` — select the literal instead
                // (`fcmp uno x, x` is true exactly for NaN; printf ignores the
                // unused vararg).
                Type::Float => {
                    let nan = self.fresh_tmp();
                    let fmt = self.fresh_tmp();
                    self.emit(format!("{nan} = fcmp uno double {v}, {v}"));
                    self.emit(format!("{fmt} = select i1 {nan}, ptr @.fmt.nan, ptr @.fmt.f"));
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr {fmt}, double {v})"));
                }
                // Float32 promotes to `double` for printf's varargs (C default
                // argument promotion), then prints with the same `%f\n`.
                Type::Float32 => {
                    let d = self.fresh_tmp();
                    self.emit(format!("{d} = fpext float {v} to double"));
                    let nan = self.fresh_tmp();
                    let fmt = self.fresh_tmp();
                    self.emit(format!("{nan} = fcmp uno double {d}, {d}"));
                    self.emit(format!("{fmt} = select i1 {nan}, ptr @.fmt.nan, ptr @.fmt.f"));
                    self.emit(format!("call i32 (ptr, ...) @printf(ptr {fmt}, double {d})"));
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
        if matches!(name, "trace" | "debug" | "info" | "warn" | "error") {
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
                    LogSink::Stderr => {
                        self.emit(format!("{stream} = call ptr @__vyrn_stderr()"))
                    }
                    LogSink::Stdout => {
                        self.emit(format!("{stream} = call ptr @__vyrn_stdout()"))
                    }
                    // The file is opened once in `@main` (below).
                    LogSink::File(_) => {
                        self.emit(format!("{stream} = load ptr, ptr @__vyrn_log_file"))
                    }
                }
                self.emit(format!(
                    "call i32 (ptr, ptr, ...) @fprintf(ptr {stream}, ptr @.fmt.log, ptr {lvl}, ptr {logv}, ptr {msgv})"
                ));
            }
            return Ok(("".into(), Type::Unit));
        }

        // (`len(String)` was removed; a String's byte length is the `.length`
        // field, lowered at `Expr::Field` via `@__vyrn_strlen`.)
        // Text encodings. Encoders return a fresh String; decoders return the
        // Option<String> aggregate (runtime helpers do the work + UTF-8 checking).
        if matches!(name, "hexEncode" | "base64Encode" | "urlEncode") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper = match name {
                "hexEncode" => "@__vyrn_hex_encode",
                "base64Encode" => "@__vyrn_b64_encode",
                _ => "@__vyrn_url_encode",
            };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call ptr {helper}(ptr {v})"));
            return Ok((t, Type::Str));
        }
        if matches!(name, "hexDecode" | "base64Decode" | "urlDecode") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper = match name {
                "hexDecode" => "@__vyrn_hex_decode",
                "base64Decode" => "@__vyrn_b64_decode",
                _ => "@__vyrn_url_decode",
            };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {{ i1, i64, i64 }} {helper}(ptr {v})"));
            return Ok((t, Type::Option(Box::new(Type::Str))));
        }
        // bytes(s) / chars(s): decode a string into an Array<UInt8> of bytes
        // (i8 stride — RFC-0014 M2) or an Array<Int> of Unicode code points
        // (runtime helpers do the UTF-8 work).
        if matches!(name, "bytes" | "chars") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper =
                if name == "bytes" { "@__vyrn_str_bytes" } else { "@__vyrn_str_chars" };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {{ ptr, i64, i64 }} {helper}(ptr {v})"));
            let elem = if name == "bytes" {
                Type::IntN { bits: 8, signed: false }
            } else {
                Type::Int
            };
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
            self.emit(format!("{valid} = call i1 @__vyrn_utf8valid(ptr {p}, i64 {len})"));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{bad_l}"));
            self.emit_label(&bad_l);
            self.emit(format!("call void @free(ptr {p})"));
            self.emit_term(format!("br label %{none_l}"));
            self.emit_label(&none_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let w0 = self.fresh_tmp();
            let s0 = self.fresh_tmp();
            let s1 = self.fresh_tmp();
            let s2 = self.fresh_tmp();
            self.emit(format!("{w0} = ptrtoint ptr {p} to i64"));
            self.emit(format!("{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"));
            self.emit(format!("{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 0, 2"));
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
        if name == "listDir" {
            return Err(format!(
                "`listDir` runs in the interpreter / at generation time (RFC-0021); it has no \
                 native or wasm lowering in v1 — use it in a `gen fn` or under `vyrn run`"
            ));
        }
        if name == "moduleInterface" {
            return Err(
                "`moduleInterface` is compile-time reflection (RFC-0021) — it is only available \
                 during generation, never at runtime"
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
            self.emit(format!("{valid} = call i1 @__vyrn_utf8valid(ptr {buf}, i64 {len})"));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{badutf_l}"));
            self.emit_label(&badutf_l);
            self.emit(format!("call void @free(ptr {buf})"));
            self.emit_term(format!("br label %{err_l}"));
            self.emit_label(&err_l);
            let stphi = self.fresh_tmp();
            self.emit(format!(
                "{stphi} = phi i32 [ {st}, %{entry_b} ], [ 2, %{badutf_l} ]"
            ));
            let msg = self.fresh_tmp();
            self.emit(format!("{msg} = call ptr @__vyrn_read_err(ptr {path}, i32 {stphi})"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!("{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"));
            self.emit(format!("{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"));
            self.emit(format!("{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let ow = self.fresh_tmp();
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{ow} = ptrtoint ptr {buf} to i64"));
            self.emit(format!("{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {ow}, 1"));
            self.emit(format!("{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], [ {o2}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Str), Box::new(Type::Str))));
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
            self.emit(format!("{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"));
            self.emit(format!("{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"));
            self.emit(format!("{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"));
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
            self.emit(format!("{msg} = call ptr @__vyrn_read_err(ptr {path}, i32 1)"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!("{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"));
            self.emit(format!("{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"));
            self.emit(format!("{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"));
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
            self.emit(format!("{a0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"));
            self.emit(format!("{a1} = insertvalue {{ ptr, i64, i64 }} {a0}, i64 {len}, 1"));
            self.emit(format!("{a2} = insertvalue {{ ptr, i64, i64 }} {a1}, i64 {len}, 2"));
            let elem_ty = Type::Array(Box::new(Type::IntN { bits: 8, signed: false }));
            let (w0, w1) = self.encode_payload(&a2, &elem_ty);
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {w0}, 1"));
            self.emit(format!("{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 {w1}, 2"));
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
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {arr}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {arr}, 1"));
            let buf = self.fresh_tmp();
            self.emit(format!("{buf} = call ptr @__vyrn_bytes_dup(ptr {data}, i64 {len})"));
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
            self.emit(format!("{valid} = call i1 @__vyrn_utf8valid(ptr {buf}, i64 {len})"));
            self.emit_term(format!("br i1 {valid}, label %{ok_l}, label %{badutf_l}"));
            self.emit_label(&badutf_l);
            self.emit(format!("call void @free(ptr {buf})"));
            self.emit_term(format!("br label %{err_l}"));
            self.emit_label(&err_l);
            let src = self.fresh_tmp();
            self.emit(format!(
                "{src} = phi ptr [ @.io.bnul, %{nul_l} ], [ @.io.butf8, %{badutf_l} ]"
            ));
            let mlen = self.fresh_tmp();
            let msz = self.fresh_tmp();
            self.emit(format!("{mlen} = call i64 @__vyrn_strlen(ptr {src})"));
            self.emit(format!("{msz} = add i64 {mlen}, 1"));
            let msg = self.fresh_tmp();
            self.emit(format!("{msg} = call ptr @__vyrn_malloc(i64 {msz})"));
            self.emit(format!("call ptr @strcpy(ptr {msg}, ptr {src})"));
            let ew = self.fresh_tmp();
            let e0 = self.fresh_tmp();
            let e1 = self.fresh_tmp();
            let e2 = self.fresh_tmp();
            self.emit(format!("{ew} = ptrtoint ptr {msg} to i64"));
            self.emit(format!("{e0} = insertvalue {{ i1, i64, i64 }} undef, i1 0, 0"));
            self.emit(format!("{e1} = insertvalue {{ i1, i64, i64 }} {e0}, i64 {ew}, 1"));
            self.emit(format!("{e2} = insertvalue {{ i1, i64, i64 }} {e1}, i64 0, 2"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&ok_l);
            let ow = self.fresh_tmp();
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{ow} = ptrtoint ptr {buf} to i64"));
            self.emit(format!("{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {ow}, 1"));
            self.emit(format!("{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!(
                "{r} = phi {{ i1, i64, i64 }} [ {e2}, %{err_l} ], [ {o2}, %{ok_l} ]"
            ));
            return Ok((r, Type::Result(Box::new(Type::Str), Box::new(Type::Str))));
        }
        // contains(a, b): strstr(a, b) != null.
        if name == "contains" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let p = self.fresh_tmp();
            let r = self.fresh_tmp();
            self.emit(format!("{p} = call ptr @strstr(ptr {a}, ptr {b})"));
            self.emit(format!("{r} = icmp ne ptr {p}, null"));
            return Ok((r, Type::Bool));
        }
        // startsWith(a, b): strncmp(a, b, strlen(b)) == 0.
        if name == "startsWith" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let lb = self.fresh_tmp();
            let c = self.fresh_tmp();
            let r = self.fresh_tmp();
            self.emit(format!("{lb} = call i64 @__vyrn_strlen(ptr {b})"));
            self.emit(format!("{c} = call i32 @__vyrn_strncmp(ptr {a}, ptr {b}, i64 {lb})"));
            self.emit(format!("{r} = icmp eq i32 {c}, 0"));
            return Ok((r, Type::Bool));
        }
        // endsWith(a, b): b fits in a AND strncmp(a + (|a|-|b|), b, |b|) == 0.
        if name == "endsWith" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let la = self.fresh_tmp();
            let lb = self.fresh_tmp();
            self.emit(format!("{la} = call i64 @__vyrn_strlen(ptr {a})"));
            self.emit(format!("{lb} = call i64 @__vyrn_strlen(ptr {b})"));
            let fits = self.fresh_tmp();
            self.emit(format!("{fits} = icmp uge i64 {la}, {lb}"));
            let cmp_l = self.fresh_label("ew.cmp");
            let no_l = self.fresh_label("ew.no");
            let end_l = self.fresh_label("ew.end");
            self.emit_term(format!("br i1 {fits}, label %{cmp_l}, label %{no_l}"));
            self.emit_label(&cmp_l);
            let off = self.fresh_tmp();
            let p = self.fresh_tmp();
            let c = self.fresh_tmp();
            let eq = self.fresh_tmp();
            self.emit(format!("{off} = sub i64 {la}, {lb}"));
            self.emit(format!("{p} = getelementptr i8, ptr {a}, i64 {off}"));
            self.emit(format!("{c} = call i32 @__vyrn_strncmp(ptr {p}, ptr {b}, i64 {lb})"));
            self.emit(format!("{eq} = icmp eq i32 {c}, 0"));
            let cmp_end = self.cur_block.clone();
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&no_l);
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            let r = self.fresh_tmp();
            self.emit(format!("{r} = phi i1 [ {eq}, %{cmp_end} ], [ false, %{no_l} ]"));
            return Ok((r, Type::Bool));
        }
        // concat(String, String) -> String. Heap-allocated. Routing is decided
        // lexically: inside a `region` the buffer is drawn from the arena (freed
        // when the region exits); outside, it comes from `malloc` and is freed by
        // ownership analysis if it doesn't escape, else leaked. The two paths are
        // mutually exclusive, so no buffer is ever freed twice.
        if name == "@concat" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let buf = self.emit_str_concat(&a, &b);
            return Ok((buf, Type::Str));
        }

        // str(Int) -> String: format into a fresh 24-byte buffer (enough for any
        // i64). Routed like `concat` (arena inside a region, else malloc).
        if name == "@str" {
            // Render a scalar to a fresh, owned heap String (Int / Bool / String).
            let (v, ty) = self.gen_expr(&args[0])?;
            match self.resolve(&ty) {
                Type::Int => {
                    let buf = self.heap_alloc("24");
                    self.emit(format!(
                        "call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 24, ptr @.fmt.ld, i64 {v})"
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
                    let buf = self.heap_alloc("24");
                    self.emit(format!(
                        "call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 24, ptr {fmt}, i64 {w})"
                    ));
                    return Ok((buf, Type::Str));
                }
                // Float renders with %f (6 decimals). A 512-byte buffer covers the
                // widest magnitude (~1e308 → ~320 chars). NaN selects a literal
                // "NaN" format (UCRT %f would render `-nan(ind)`; the interp
                // prints `NaN`) — snprintf ignores the unused vararg.
                Type::Float => {
                    let nan = self.fresh_tmp();
                    let fmt = self.fresh_tmp();
                    self.emit(format!("{nan} = fcmp uno double {v}, {v}"));
                    self.emit(format!("{fmt} = select i1 {nan}, ptr @.str.nan, ptr @.fmt.lf"));
                    let buf = self.heap_alloc("512");
                    self.emit(format!(
                        "call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 512, ptr {fmt}, double {v})"
                    ));
                    return Ok((buf, Type::Str));
                }
                // Float32 promotes to `double` (varargs), then renders like Float.
                Type::Float32 => {
                    let d = self.fresh_tmp();
                    self.emit(format!("{d} = fpext float {v} to double"));
                    let nan = self.fresh_tmp();
                    let fmt = self.fresh_tmp();
                    self.emit(format!("{nan} = fcmp uno double {d}, {d}"));
                    self.emit(format!("{fmt} = select i1 {nan}, ptr @.str.nan, ptr @.fmt.lf"));
                    let buf = self.heap_alloc("512");
                    self.emit(format!(
                        "call i32 (ptr, i64, ptr, ...) @__vyrn_snprintf(ptr {buf}, i64 512, ptr {fmt}, double {d})"
                    ));
                    return Ok((buf, Type::Str));
                }
                Type::Bool => {
                    // Copy "true"/"false" into a fresh buffer so the result owns
                    // its storage (a global pointer must never be freed).
                    let src = self.fresh_tmp();
                    self.emit(format!(
                        "{src} = select i1 {v}, ptr @.str.true, ptr @.str.false"
                    ));
                    let buf = self.heap_alloc("6");
                    self.emit(format!("call ptr @strcpy(ptr {buf}, ptr {src})"));
                    return Ok((buf, Type::Str));
                }
                Type::Str => {
                    // strdup: copy so the rendered value is independently owned.
                    let len = self.fresh_tmp();
                    let sz = self.fresh_tmp();
                    self.emit(format!("{len} = call i64 @__vyrn_strlen(ptr {v})"));
                    self.emit(format!("{sz} = add i64 {len}, 1"));
                    let buf = self.heap_alloc(&sz);
                    self.emit(format!("call ptr @strcpy(ptr {buf}, ptr {v})"));
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
            self.emit(format!("{p} = phi ptr [ {p0}, %{pre} ], [ {{PNEXT}}, %{cont_l} ]"));
            self.emit(format!("{acc} = phi i64 [ 0, %{pre} ], [ {{ACCN}}, %{cont_l} ]"));
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
            self.emit(format!("{val} = select i1 {isneg}, i64 {negval}, i64 {acc}"));
            self.emit_term(format!("br label %{build_l}"));
            // fail: a non-digit character.
            self.emit_label(&fail_l);
            self.emit_term(format!("br label %{build_l}"));
            // build the Option<Int>.
            self.emit_label(&build_l);
            let tag = self.fresh_tmp();
            let v = self.fresh_tmp();
            self.emit(format!("{tag} = phi i1 [ {hasdigit}, %{done_l} ], [ false, %{fail_l} ]"));
            self.emit(format!("{v} = phi i64 [ {val}, %{done_l} ], [ 0, %{fail_l} ]"));
            let o0 = self.fresh_tmp();
            let o1 = self.fresh_tmp();
            let o2 = self.fresh_tmp();
            self.emit(format!("{o0} = insertvalue {{ i1, i64, i64 }} undef, i1 {tag}, 0"));
            self.emit(format!("{o1} = insertvalue {{ i1, i64, i64 }} {o0}, i64 {v}, 1"));
            self.emit(format!("{o2} = insertvalue {{ i1, i64, i64 }} {o1}, i64 0, 2"));
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

        // Generational references (RFC-0004 §4, Path B). A `Ref<T>` is
        // { i64 slot, i64 generation }; the payload is a boxed `T` held by the
        // slab, and every access checks the generation against the slot's.
        if name == "cell" {
            let (v, vty) = self.gen_expr(&args[0])?;
            let ll = self.llt(&vty);
            // Box the value: allocate sizeof(T), store it, register the pointer.
            let size = self.fresh_tmp();
            let payload = self.fresh_tmp();
            self.emit(format!(
                "{size} = ptrtoint ptr getelementptr ({ll}, ptr null, i64 1) to i64"
            ));
            self.emit(format!("{payload} = call ptr @__vyrn_malloc(i64 {size})"));
            self.emit(format!("store {ll} {v}, ptr {payload}"));
            let slot = self.fresh_tmp();
            self.emit(format!("{slot} = call i64 @__vyrn_cell_alloc(ptr {payload})"));
            let g = self.fresh_tmp();
            self.emit(format!("{g} = call i64 @__vyrn_cell_getgen(i64 {slot})"));
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {{ i64, i64 }} undef, i64 {slot}, 0"));
            self.emit(format!("{b} = insertvalue {{ i64, i64 }} {a}, i64 {g}, 1"));
            return Ok((b, Type::Ref(Box::new(vty))));
        }
        if name == "get" || name == "set" || name == "release" {
            let (r, rty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&rty) {
                Type::Ref(inner) => *inner,
                _ => return Err(format!("`{name}` on a non-Ref value")),
            };
            let slot = self.fresh_tmp();
            let g = self.fresh_tmp();
            self.emit(format!("{slot} = extractvalue {{ i64, i64 }} {r}, 0"));
            self.emit(format!("{g} = extractvalue {{ i64, i64 }} {r}, 1"));
            // Every access first validates the generation (traps on a stale ref).
            self.emit(format!("call void @__vyrn_cell_check(i64 {slot}, i64 {g})"));
            let payload = self.fresh_tmp();
            self.emit(format!("{payload} = call ptr @__vyrn_cell_ptr(i64 {slot})"));
            let ll = self.llt(&elem);
            match name {
                "get" => {
                    let v = self.fresh_tmp();
                    self.emit(format!("{v} = load {ll}, ptr {payload}"));
                    return Ok((v, elem));
                }
                "set" => {
                    let (v, vty) = self.gen_expr(&args[1])?;
                    let (v, _) = self.coerce(v, &vty, &elem)?;
                    self.emit(format!("store {ll} {v}, ptr {payload}"));
                    return Ok((String::new(), Type::Unit));
                }
                _ => {
                    // release: free the boxed payload and invalidate the slot.
                    self.emit(format!("call void @free(ptr {payload})"));
                    self.emit(format!("call void @__vyrn_cell_release_slot(i64 {slot})"));
                    return Ok((String::new(), Type::Unit));
                }
            }
        }

        // `Some(x)` / `Ok(x)` / `Err(e)` — build a { i1 tag, i64 payload } value.
        // Growable arrays. An `Array<T>` is { ptr data, i64 len, i64 cap }; used
        // linearly (`push` returns the updated triple, reallocating on growth).
        if name == "array" {
            return Ok(("{ ptr null, i64 0, i64 0 }".into(), Type::Array(Box::new(Type::Int))));
        }
        if name == "push" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                _ => return Err("push on a non-Array value".into()),
            };
            let ell = self.llt(&elem);
            let (v, vty) = self.gen_expr(&args[1])?;
            let (v, _) = self.coerce(v, &vty, &elem)?;
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            let cap = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
            self.emit(format!("{cap} = extractvalue {{ ptr, i64, i64 }} {av}, 2"));
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
            self.emit(format!("{esz} = ptrtoint ptr getelementptr ({ell}, ptr null, i64 1) to i64"));
            self.emit(format!("{nb} = mul i64 {nc}, {esz}"));
            self.emit(format!("{nd} = call ptr @__vyrn_realloc(ptr {data}, i64 {nb})"));
            self.emit_term(format!("br label %{ready_l}"));
            // ready: choose data/cap, store the new element, rebuild the triple.
            self.emit_label(&ready_l);
            let d = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!("{d} = phi ptr [ {data}, %{pre} ], [ {nd}, %{grow_l} ]"));
            self.emit(format!("{c} = phi i64 [ {cap}, %{pre} ], [ {nc}, %{grow_l} ]"));
            let ep = self.fresh_tmp();
            self.emit(format!("{ep} = getelementptr {ell}, ptr {d}, i64 {len}"));
            self.emit(format!("store {ell} {v}, ptr {ep}"));
            let nl = self.fresh_tmp();
            self.emit(format!("{nl} = add i64 {len}, 1"));
            let r0 = self.fresh_tmp();
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            self.emit(format!("{r0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {d}, 0"));
            self.emit(format!("{r1} = insertvalue {{ ptr, i64, i64 }} {r0}, i64 {nl}, 1"));
            self.emit(format!("{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {c}, 2"));
            return Ok((r2, Type::Array(Box::new(elem))));
        }
        if name == "at" {
            let (av, aty) = self.gen_expr(&args[0])?;
            let (iv, _) = self.gen_expr(&args[1])?;
            let bad_l = self.fresh_label("at.oob");
            let ok_l = self.fresh_label("at.ok");
            // The trap message carries the offending index and goes to stderr,
            // byte-identical to the interpreter's `error: ... index {i} out of
            // bounds`. Strings pick the "string index" wording.
            let emit_trap = |g: &mut Self, fmt: &str| {
                g.emit_label(&bad_l);
                let e = g.fresh_tmp();
                g.emit(format!("{e} = call ptr @__vyrn_stderr()"));
                g.emit(format!(
                    "call i32 (ptr, ptr, ...) @fprintf(ptr {e}, ptr {fmt}, i64 {iv})"
                ));
                g.emit("call void @exit(i32 1)".into());
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
                Type::ArrayN(inner, n) => {
                    // Fixed array: store the value aggregate to the stack, then
                    // index it. Bounds are the constant N.
                    let elem = *inner;
                    let ell = self.llt(&elem);
                    let aggty = format!("[{n} x {ell}]");
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {n}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.aoob");
                    self.emit_label(&ok_l);
                    let slot = self.fresh_alloca(&aggty);
                    self.emit(format!("store {aggty} {av}, ptr {slot}"));
                    let ep = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr {aggty}, ptr {slot}, i64 0, i64 {iv}"));
                    self.emit(format!("{v} = load {ell}, ptr {ep}"));
                    return Ok((v, elem));
                }
                // `s[i]` on a String: bounds-check against strlen, then load the
                // byte as a `UInt8` (RFC-0022) — an `i8` SSA value, the same
                // representation as an element of `bytes(s)`, no zero-extension.
                Type::Str => {
                    let len = self.fresh_tmp();
                    self.emit(format!("{len} = call i64 @__vyrn_strlen(ptr {av})"));
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.soob");
                    self.emit_label(&ok_l);
                    let ep = self.fresh_tmp();
                    let byte = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr i8, ptr {av}, i64 {iv}"));
                    self.emit(format!("{byte} = load i8, ptr {ep}"));
                    return Ok((byte, Type::IntN { bits: 8, signed: false }));
                }
                // `m[k]` on a Map (RFC-0028): linear key scan → `Option<V>`
                // (`None` on a miss, never a trap). `iv` is the key `ptr`.
                Type::Map(_, val) => {
                    let val = *val;
                    let vll = self.llt(&val);
                    let keys = self.fresh_tmp();
                    let vals = self.fresh_tmp();
                    let len = self.fresh_tmp();
                    self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {av}, 0"));
                    self.emit(format!("{vals} = extractvalue {{ ptr, ptr, i64, i64 }} {av}, 1"));
                    self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {av}, 2"));
                    let idx = self.fresh_tmp();
                    self.emit(format!(
                        "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {iv})"
                    ));
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
                    self.emit(format!("{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
                    self.emit(format!("{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"));
                    self.emit(format!("{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 {w1}, 2"));
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
        if name == "alen" {
            let (av, aty) = self.gen_expr(&args[0])?;
            match self.resolve(&aty) {
                // Fixed array: the length is the constant N.
                Type::ArrayN(_, n) => return Ok((format!("{n}"), Type::Int)),
                _ => {
                    let len = self.fresh_tmp();
                    self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {av}, 1"));
                    return Ok((len, Type::Int));
                }
            }
        }
        if name == "afree" {
            let (av, _) = self.gen_expr(&args[0])?;
            let data = self.fresh_tmp();
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {av}, 0"));
            self.emit(format!("call void @free(ptr {data})"));
            return Ok((String::new(), Type::Unit));
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
            let (slot, aty) = self.lookup(&recv).ok_or_else(|| format!("unbound `{recv}`"))?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                other => return Err(format!("`pop` needs an Array, found {other:?}")),
            };
            let ell = self.llt(&elem);
            let hdr = self.fresh_tmp();
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"));
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
            self.emit(format!("{nh} = insertvalue {{ ptr, i64, i64 }} {hdr}, i64 {nl}, 1"));
            self.emit(format!("store {{ ptr, i64, i64 }} {nh}, ptr {slot}"));
            let (w0, w1) = self.encode_payload(&v, &elem);
            let s0 = self.fresh_tmp();
            let s1 = self.fresh_tmp();
            let s2 = self.fresh_tmp();
            self.emit(format!("{s0} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{s1} = insertvalue {{ i1, i64, i64 }} {s0}, i64 {w0}, 1"));
            self.emit(format!("{s2} = insertvalue {{ i1, i64, i64 }} {s1}, i64 {w1}, 2"));
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
            let (slot, aty) = self.lookup(&recv).ok_or_else(|| format!("unbound `{recv}`"))?;
            let elem = match self.resolve(&aty) {
                Type::Array(inner) => *inner,
                other => return Err(format!("`swapRemove` needs an Array, found {other:?}")),
            };
            let ell = self.llt(&elem);
            let hdr = self.fresh_tmp();
            let data = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{hdr} = load {{ ptr, i64, i64 }}, ptr {slot}"));
            self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {hdr}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {hdr}, 1"));
            let (iv, _) = self.gen_expr(&args[1])?;
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
            self.emit(format!("{nh} = insertvalue {{ ptr, i64, i64 }} {hdr}, i64 {nl}, 1"));
            self.emit(format!("store {{ ptr, i64, i64 }} {nh}, ptr {slot}"));
            return Ok((v, elem));
        }
        // `m.has(k)` (RFC-0028) — membership test → i1. Read-only; the receiver
        // is any Map-typed expression (an SSA aggregate).
        if name == "@has" {
            let (mv, _) = self.gen_expr(&args[0])?;
            let (kv, _) = self.gen_expr(&args[1])?;
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {mv}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {mv}, 2"));
            let idx = self.fresh_tmp();
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {kv})"
            ));
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
            let (slot, aty) = self.lookup(&recv).ok_or_else(|| format!("unbound `{recv}`"))?;
            let val = match self.resolve(&aty) {
                Type::Map(_, v) => *v,
                other => return Err(format!("`remove` needs a Map, found {other:?}")),
            };
            let vll = self.llt(&val);
            let esz = self.fresh_tmp();
            self.emit(format!(
                "{esz} = ptrtoint ptr getelementptr ({vll}, ptr null, i64 1) to i64"
            ));
            let hdr = self.fresh_tmp();
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            let (kv, _) = self.gen_expr(&args[1])?;
            self.emit(format!("{hdr} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
            self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 2"));
            let idx = self.fresh_tmp();
            self.emit(format!(
                "{idx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {kv})"
            ));
            let found = self.fresh_tmp();
            self.emit(format!("{found} = icmp sge i64 {idx}, 0"));
            let do_l = self.fresh_label("map.rm.do");
            let end_l = self.fresh_label("map.rm.end");
            self.emit_term(format!("br i1 {found}, label %{do_l}, label %{end_l}"));
            self.emit_label(&do_l);
            self.emit(format!(
                "call void @__vyrn_map_remove_at(ptr {slot}, i64 {idx}, i64 {esz})"
            ));
            self.emit_term(format!("br label %{end_l}"));
            self.emit_label(&end_l);
            return Ok((found, Type::Bool));
        }
        // `m.keys()` (RFC-0028) — a fresh snapshot `Array<String>` in insertion
        // order. Copies the key pointers into a new buffer (cap = len); the map
        // may then be mutated without disturbing the snapshot.
        if name == "@keys" {
            let (mv, _) = self.gen_expr(&args[0])?;
            let keys = self.fresh_tmp();
            let len = self.fresh_tmp();
            self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {mv}, 0"));
            self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {mv}, 2"));
            let buf = self.fresh_tmp();
            self.emit(format!("{buf} = call ptr @__vyrn_map_keys_copy(ptr {keys}, i64 {len})"));
            let r0 = self.fresh_tmp();
            let r1 = self.fresh_tmp();
            let r2 = self.fresh_tmp();
            self.emit(format!("{r0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"));
            self.emit(format!("{r1} = insertvalue {{ ptr, i64, i64 }} {r0}, i64 {len}, 1"));
            self.emit(format!("{r2} = insertvalue {{ ptr, i64, i64 }} {r1}, i64 {len}, 2"));
            return Ok((r2, Type::Array(Box::new(Type::Str))));
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
        // load its result from the frame's leading slot. Idempotent — a task
        // may be joined more than once (the shim only waits the first time and
        // the frame is never freed), exactly like the eager value semantics.
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
                return Ok((String::new(), Type::Unit));
            }
            let t = self.fresh_tmp();
            self.emit(format!("{t} = load {retll}, ptr {frame}"));
            return Ok((t, inner));
        }

        // `Some(x)` — the payload may be any type (boxed if wider than a word),
        // so the Option is `Option<typeof x>`.
        if name == "Some" {
            let (v, ty) = self.gen_expr(&args[0])?;
            let (w0, w1) = self.encode_payload(&v, &ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
            self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"));
            self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"));
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
            let (v, ty) = self.gen_expr(&args[0])?;
            let (w0, w1) = self.encode_payload(&v, &ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {tag}, 0"));
            self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"));
            self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"));
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
            // former and unbox the latter and the raw elements are reinterpreted
            // as a header (the RFC-0026 corruption bug). A generic variant whose
            // payload is still an unresolved type parameter keeps the argument's
            // own type (the inline-monomorphized path).
            let decl_payload: Vec<Type> =
                match self.types.get(&enum_name).map(|d| d.base.clone()) {
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
            for (i, a) in args.iter().enumerate() {
                let (v, ty) = self.gen_expr(a)?;
                let (v, ty) = match decl_payload.get(i) {
                    Some(dt) if !matches!(self.resolve(dt), Type::Param(_)) => {
                        self.coerce(v, &ty, dt)?
                    }
                    _ => (v, ty),
                };
                payloads.push(self.box_payload(&v, &ty));
            }
            let mut cur = "undef".to_string();
            let t = self.fresh_tmp();
            self.emit(format!("{t} = insertvalue {ll} {cur}, i64 {tag}, 0"));
            cur = t;
            for slot in 1..=arity {
                let val = payloads.get(slot - 1).cloned().unwrap_or_else(|| "0".into());
                let t = self.fresh_tmp();
                self.emit(format!("{t} = insertvalue {ll} {cur}, i64 {val}, {slot}"));
                cur = t;
            }
            return Ok((cur, Type::Named(enum_name)));
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
            let recv_ty = match args.first() {
                Some(Expr::Var { name: v, .. }) => self
                    .lookup(v)
                    .map(|(_, t)| t)
                    .ok_or_else(|| format!("unbound receiver `{v}`"))?,
                _ => {
                    return Err(format!(
                        "protocol method `{name}` must be called on a variable in this backend"
                    ))
                }
            };
            // Substitute generic params (monomorphization) but keep named types,
            // so an enum receiver keys on its name rather than its aggregate.
            let concrete = vyrn_frontend::types::substitute(&recv_ty, self.subst);
            let key = vyrn_frontend::types::type_key(&concrete)
                .ok_or_else(|| format!("cannot dispatch `{name}` on {recv_ty:?}"))?;
            let mangled = vyrn_frontend::types::impl_method_name(&proto, &key, name);
            return self.gen_call(&mangled, args);
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
            let mut arg_tys = Vec::new();
            let mut arg_vals = Vec::new();
            for a in args {
                let (v, vty) = self.gen_expr(a)?;
                arg_tys.push(vyrn_frontend::types::substitute(&vty, self.subst));
                arg_vals.push(v);
            }
            // Bind each type parameter from the matching argument.
            let mut call_subst: HashMap<String, Type> = HashMap::new();
            for (p, aty) in callee.params.iter().zip(&arg_tys) {
                solve_param(&p.ty, aty, &mut call_subst);
            }
            let type_args: Vec<Type> = callee
                .type_params
                .iter()
                .map(|tp| call_subst.get(tp).cloned().unwrap_or(Type::Unit))
                .collect();
            let sym = mangle_name(name, &type_args);

            // Coerce args to their (substituted) parameter types.
            let mut arg_ops = Vec::new();
            for ((p, v), aty) in callee.params.iter().zip(arg_vals).zip(&arg_tys) {
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
            let (v, vty) = self.gen_expr(a)?;
            let (v, pty) = match params.get(i) {
                Some(p) => self.coerce_flow(v, a, &vty, p)?,
                None => (v, vty),
            };
            arg_ops.push(format!("{} {v}", self.llt(&pty)));
        }
        let sym = format!("vyrn_{name}");
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
        let (sym, arg_vals, ret_ty) = self.prep_spawn_target(name, args)?;
        let retll = self.llt(&ret_ty);
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
        for (i, (ll, v)) in arg_vals.iter().enumerate() {
            let p = self.fresh_tmp();
            self.emit(format!(
                "{p} = getelementptr {frame_ty}, ptr {frame}, i32 0, i32 {}",
                base + i
            ));
            self.emit(format!("store {ll} {v}, ptr {p}"));
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
        self.emit(format!("{t} = call ptr @__vyrn_spawn(ptr @{tsym}, ptr {frame})"));
        Ok((t, Type::Task(Box::new(ret_ty))))
    }

    /// Resolve a spawn callee exactly as `gen_call` would: evaluate and coerce
    /// each argument to its (substituted) parameter type, solve + register a
    /// generic instantiation when the callee is generic, and return the callee
    /// symbol, the `(llvm type, value)` argument pairs, and the concrete return
    /// type. `modify` parameters and externs cannot appear — the checker only
    /// admits isolated (spawn-safe) callees.
    fn prep_spawn_target(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<(String, Vec<(String, String)>, Type), String> {
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
            let mut call_subst: HashMap<String, Type> = HashMap::new();
            for (p, aty) in callee.params.iter().zip(&arg_tys) {
                solve_param(&p.ty, aty, &mut call_subst);
            }
            let type_args: Vec<Type> = callee
                .type_params
                .iter()
                .map(|tp| call_subst.get(tp).cloned().unwrap_or(Type::Unit))
                .collect();
            let sym = mangle_name(name, &type_args);
            let mut pairs = Vec::new();
            for ((p, v), aty) in callee.params.iter().zip(arg_vals).zip(&arg_tys) {
                let pty = vyrn_frontend::types::substitute(&p.ty, &call_subst);
                let (v, cty) = self.coerce(v, aty, &pty)?;
                pairs.push((self.llt(&cty), v));
            }
            self.instantiations.push((name.to_string(), type_args));
            let ret_ty = vyrn_frontend::types::substitute(&callee.ret, &call_subst);
            return Ok((sym, pairs, ret_ty));
        }
        // Ordinary callee.
        let params = self.param_types.get(name).cloned().unwrap_or_default();
        let mut pairs = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let (v, vty) = self.gen_expr(a)?;
            let (v, pty) = match params.get(i) {
                Some(p) => self.coerce_flow(v, a, &vty, p)?,
                None => (v, vty),
            };
            pairs.push((self.llt(&pty), v));
        }
        let sym = format!("vyrn_{name}");
        let ret = self.ret_types.get(name).cloned().unwrap_or(Type::Int);
        Ok((sym, pairs, ret))
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
                let len = self.fresh_tmp();
                self.emit(format!("{len} = call i64 @__vyrn_strlen(ptr {v})"));
                arg_ops.push(format!("ptr {v}"));
                arg_ops.push(format!("i64 {len}"));
            } else {
                let (abi_v, abi_ll) = self.to_extern_abi(&v, &cty);
                arg_ops.push(format!("{abi_ll} {abi_v}"));
            }
        }
        let sym = extern_symbol(&f.name);
        let ret_ll = extern_abi_ll(&f.ret);
        if ret_ll == "void" {
            self.emit(format!("call void @{sym}({})", arg_ops.join(", ")));
            Ok((String::new(), Type::Unit))
        } else {
            let raw = self.fresh_tmp();
            self.emit(format!("{raw} = call {ret_ll} @{sym}({})", arg_ops.join(", ")));
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
        let Some(pred) = decl.predicate.clone() else { return Ok(()) };
        self.scope.push(Vec::new());
        if let Type::Record(fields) = &decl.base.clone() {
            let rec_ll = self.llt(&decl.base);
            for (i, f) in fields.iter().enumerate() {
                let ext = self.fresh_tmp();
                self.emit(format!("{ext} = extractvalue {rec_ll} {v}, {i}"));
                let slot = self.declare(&f.name, &f.ty);
                let fll = self.llt(&f.ty);
                self.emit(format!("store {fll} {ext}, ptr {slot}"));
            }
        } else {
            let base_ll = self.llt(&decl.base);
            let slot = self.declare("value", &decl.base);
            self.emit(format!("store {base_ll} {v}, ptr {slot}"));
        }
        let (cond, _) = self.gen_expr(&pred)?;
        self.scope.pop();
        let nok = self.fresh_tmp();
        self.emit(format!("{nok} = xor i1 {cond}, true"));
        self.trap_if(&nok, &format!("@.trap.verr.{}", decl.name), "vfail");
        Ok(())
    }

    /// Lower a refined type's `where` predicate to an `i1` (true = holds),
    /// binding the value under check: a record base binds each field name; a
    /// scalar base binds `value`. This is the ONE place a predicate is lowered
    /// — both the trap path (`emit_validation`) and the JSON decode `validate`
    /// path (RFC-0018) derive from it, so the two never drift.
    fn emit_predicate_cond(&mut self, decl: &TypeDecl, v: &str) -> Result<String, String> {
        let pred = decl.predicate.clone().expect("predicate present");
        self.scope.push(Vec::new());
        if let Type::Record(fields) = &decl.base.clone() {
            let rec_ll = self.llt(&decl.base);
            for (i, f) in fields.iter().enumerate() {
                let ext = self.fresh_tmp();
                self.emit(format!("{ext} = extractvalue {rec_ll} {v}, {i}"));
                let slot = self.declare(&f.name, &f.ty);
                let fll = self.llt(&f.ty);
                self.emit(format!("store {fll} {ext}, ptr {slot}"));
            }
        } else {
            let base_ll = self.llt(&decl.base);
            let slot = self.declare("value", &decl.base);
            self.emit(format!("store {base_ll} {v}, ptr {slot}"));
        }
        let (cond, _) = self.gen_expr(&pred)?;
        self.scope.pop();
        Ok(cond)
    }

    // ---- JSON codec (RFC-0018): per-type encode/decode IR ---------------
    // The parity-critical string work (number formatting, escaping, parse
    // errors, message assembly) lives in the C shim; these emitters walk the
    // static type, producing the DOM (encode) or consuming it (decode) and
    // accumulating `Issue`s through the shim's accumulator.

    /// A pooled string-literal global's symbol (every codec constant is seeded
    /// into the pool up front by `collect_codec_strings`).
    fn str_g(&self, s: &str) -> Result<String, String> {
        self.str_globals.get(s).cloned().ok_or_else(|| format!("codec string not pooled: {s:?}"))
    }

    /// Widen a sized integer to `i64` for the `vj_int`/`vj_uint` builders.
    fn widen_i64(&mut self, val: &str, bits: u8, signed: bool) -> String {
        if bits >= 64 {
            return val.to_string();
        }
        let t = self.fresh_tmp();
        let ext = if signed { "sext" } else { "zext" };
        self.emit(format!("{t} = {ext} i{bits} {val} to i64"));
        t
    }

    /// Emit IR that encodes a value `val` of type `ty` into a JSON DOM node
    /// (`VJ*`), returning the register holding it.
    fn emit_encode(&mut self, val: &str, ty: &Type) -> Result<String, String> {
        // A named record routes to its generated encoder (recursion-safe); a
        // named enum reads its variant name from the per-enum name table.
        if let Type::Named(n) = ty {
            match self.resolve(ty) {
                Type::Record(_) if self.types.contains_key(n) => {
                    let ll = self.llt(ty);
                    let r = self.fresh_tmp();
                    self.emit(format!("{r} = call ptr @__vyrn_enc_{n}({ll} {val})"));
                    return Ok(r);
                }
                Type::Enum(vs) => {
                    // A pure-nullary enum reads its variant name from the table in
                    // O(1) — byte-identical to the payload-less RFC-0018 encoding.
                    // A payload-bearing enum routes to its generated encoder (a tag
                    // switch), so a self-referential payload resolves to a call.
                    if vs.iter().all(|v| v.payload.is_empty()) {
                        let ll = self.llt(ty);
                        let tag = self.fresh_tmp();
                        self.emit(format!("{tag} = extractvalue {ll} {val}, 0"));
                        let gep = self.fresh_tmp();
                        let count = vs.len();
                        self.emit(format!(
                            "{gep} = getelementptr [{count} x ptr], ptr @.enumnames.{n}, i64 0, i64 {tag}"
                        ));
                        let name = self.fresh_tmp();
                        self.emit(format!("{name} = load ptr, ptr {gep}"));
                        let r = self.fresh_tmp();
                        self.emit(format!("{r} = call ptr @__vyrn_vj_str(ptr {name})"));
                        return Ok(r);
                    }
                    let ll = self.llt(ty);
                    let r = self.fresh_tmp();
                    self.emit(format!("{r} = call ptr @__vyrn_enc_{n}({ll} {val})"));
                    return Ok(r);
                }
                _ => {}
            }
        }
        match self.resolve(ty) {
            Type::Int => {
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_int(i64 {val})"));
                Ok(r)
            }
            Type::IntN { bits, signed } => {
                let w = self.widen_i64(val, bits, signed);
                let fname = if signed { "int" } else { "uint" };
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_{fname}(i64 {w})"));
                Ok(r)
            }
            Type::Float => {
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_float(double {val})"));
                Ok(r)
            }
            Type::Float32 => {
                let d = self.fresh_tmp();
                self.emit(format!("{d} = fpext float {val} to double"));
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_float(double {d})"));
                Ok(r)
            }
            Type::Bool => {
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_bool(i1 {val})"));
                Ok(r)
            }
            Type::Str => {
                let r = self.fresh_tmp();
                self.emit(format!("{r} = call ptr @__vyrn_vj_str(ptr {val})"));
                Ok(r)
            }
            Type::Enum(vs) => {
                // Anonymous enum (rare): encode inline via a tag switch (RFC-0024
                // wire tagging — nullary bare string, payload object/array).
                let ll = self.llt(ty);
                self.emit_encode_enum_body(val, &vs, &ll)
            }
            Type::Result(t, e) => self.emit_encode_result(val, &t, &e),
            Type::Record(fields) => {
                // Anonymous record: build the object inline (no recursion risk).
                let ll = self.llt(ty);
                let obj = self.fresh_tmp();
                self.emit(format!("{obj} = call ptr @__vyrn_vj_obj()"));
                for (i, f) in fields.iter().enumerate() {
                    let fv = self.fresh_tmp();
                    self.emit(format!("{fv} = extractvalue {ll} {val}, {i}"));
                    self.emit_encode_field(&obj, &fv, f)?;
                }
                Ok(obj)
            }
            Type::Option(inner) => {
                // A bare Option: Some -> encode the payload, None -> `null`.
                let tag = self.fresh_tmp();
                self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {val}, 0"));
                let slot = self.fresh_alloca("ptr");
                let some_l = self.fresh_label("enc.opt.some");
                let none_l = self.fresh_label("enc.opt.none");
                let done_l = self.fresh_label("enc.opt.done");
                self.emit_term(format!("br i1 {tag}, label %{some_l}, label %{none_l}"));
                self.emit_label(&some_l);
                let w0 = self.fresh_tmp();
                let w1 = self.fresh_tmp();
                self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {val}, 1"));
                self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {val}, 2"));
                let iv = self.decode_payload(&w0, &w1, &inner);
                let c = self.emit_encode(&iv, &inner)?;
                self.emit(format!("store ptr {c}, ptr {slot}"));
                self.emit_term(format!("br label %{done_l}"));
                self.emit_label(&none_l);
                let nul = self.fresh_tmp();
                self.emit(format!("{nul} = call ptr @__vyrn_vj_null()"));
                self.emit(format!("store ptr {nul}, ptr {slot}"));
                self.emit_term(format!("br label %{done_l}"));
                self.emit_label(&done_l);
                let r = self.fresh_tmp();
                self.emit(format!("{r} = load ptr, ptr {slot}"));
                Ok(r)
            }
            Type::Array(inner) => {
                let ell = self.llt(&inner);
                let data = self.fresh_tmp();
                let len = self.fresh_tmp();
                self.emit(format!("{data} = extractvalue {{ ptr, i64, i64 }} {val}, 0"));
                self.emit(format!("{len} = extractvalue {{ ptr, i64, i64 }} {val}, 1"));
                let arr = self.fresh_tmp();
                self.emit(format!("{arr} = call ptr @__vyrn_vj_arr()"));
                let idx = self.fresh_alloca("i64");
                self.emit(format!("store i64 0, ptr {idx}"));
                let cond_l = self.fresh_label("enc.arr.cond");
                let body_l = self.fresh_label("enc.arr.body");
                let done_l = self.fresh_label("enc.arr.done");
                self.emit_term(format!("br label %{cond_l}"));
                self.emit_label(&cond_l);
                let i = self.fresh_tmp();
                self.emit(format!("{i} = load i64, ptr {idx}"));
                let more = self.fresh_tmp();
                self.emit(format!("{more} = icmp slt i64 {i}, {len}"));
                self.emit_term(format!("br i1 {more}, label %{body_l}, label %{done_l}"));
                self.emit_label(&body_l);
                let ep = self.fresh_tmp();
                self.emit(format!("{ep} = getelementptr {ell}, ptr {data}, i64 {i}"));
                let ev = self.fresh_tmp();
                self.emit(format!("{ev} = load {ell}, ptr {ep}"));
                let c = self.emit_encode(&ev, &inner)?;
                self.emit(format!("call void @__vyrn_vj_push(ptr {arr}, ptr {c})"));
                let ni = self.fresh_tmp();
                self.emit(format!("{ni} = add i64 {i}, 1"));
                self.emit(format!("store i64 {ni}, ptr {idx}"));
                self.emit_term(format!("br label %{cond_l}"));
                self.emit_label(&done_l);
                Ok(arr)
            }
            // A `Map<String, V>` (RFC-0028) encodes as a JSON object: keys in
            // insertion order, each value via V's codec.
            Type::Map(_, mv) => {
                let vll = self.llt(&mv);
                let keys = self.fresh_tmp();
                let vals = self.fresh_tmp();
                let len = self.fresh_tmp();
                self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {val}, 0"));
                self.emit(format!("{vals} = extractvalue {{ ptr, ptr, i64, i64 }} {val}, 1"));
                self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {val}, 2"));
                let obj = self.fresh_tmp();
                self.emit(format!("{obj} = call ptr @__vyrn_vj_obj()"));
                let idx = self.fresh_alloca("i64");
                self.emit(format!("store i64 0, ptr {idx}"));
                let cond_l = self.fresh_label("enc.map.cond");
                let body_l = self.fresh_label("enc.map.body");
                let done_l = self.fresh_label("enc.map.done");
                self.emit_term(format!("br label %{cond_l}"));
                self.emit_label(&cond_l);
                let i = self.fresh_tmp();
                self.emit(format!("{i} = load i64, ptr {idx}"));
                let more = self.fresh_tmp();
                self.emit(format!("{more} = icmp slt i64 {i}, {len}"));
                self.emit_term(format!("br i1 {more}, label %{body_l}, label %{done_l}"));
                self.emit_label(&body_l);
                let kp = self.fresh_tmp();
                let k = self.fresh_tmp();
                self.emit(format!("{kp} = getelementptr ptr, ptr {keys}, i64 {i}"));
                self.emit(format!("{k} = load ptr, ptr {kp}"));
                let vp = self.fresh_tmp();
                let ev = self.fresh_tmp();
                self.emit(format!("{vp} = getelementptr {vll}, ptr {vals}, i64 {i}"));
                self.emit(format!("{ev} = load {vll}, ptr {vp}"));
                let c = self.emit_encode(&ev, &mv)?;
                self.emit(format!("call void @__vyrn_vj_set(ptr {obj}, ptr {k}, ptr {c})"));
                let ni = self.fresh_tmp();
                self.emit(format!("{ni} = add i64 {i}, 1"));
                self.emit(format!("store i64 {ni}, ptr {idx}"));
                self.emit_term(format!("br label %{cond_l}"));
                self.emit_label(&done_l);
                Ok(obj)
            }
            Type::ArrayN(inner, n) => {
                let ell = self.llt(&inner);
                let aggty = format!("[{n} x {ell}]");
                let arr = self.fresh_tmp();
                self.emit(format!("{arr} = call ptr @__vyrn_vj_arr()"));
                for i in 0..n {
                    let ev = self.fresh_tmp();
                    self.emit(format!("{ev} = extractvalue {aggty} {val}, {i}"));
                    let c = self.emit_encode(&ev, &inner)?;
                    self.emit(format!("call void @__vyrn_vj_push(ptr {arr}, ptr {c})"));
                }
                Ok(arr)
            }
            other => Err(format!("toJson: cannot encode {other:?}")),
        }
    }

    /// Encode one enum/Result variant to its RFC-0024 wire form, returning the
    /// `VJ*` register: nullary is a bare string; a single payload is
    /// `{"Tag":<value>}`; two or more is `{"Tag":[v1,...]}`.
    fn emit_encode_wire_variant(
        &mut self,
        name: &str,
        payloads: &[(String, Type)],
    ) -> Result<String, String> {
        let nameg = self.str_g(name)?;
        if payloads.is_empty() {
            let r = self.fresh_tmp();
            self.emit(format!("{r} = call ptr @__vyrn_vj_str(ptr {nameg})"));
            return Ok(r);
        }
        let obj = self.fresh_tmp();
        self.emit(format!("{obj} = call ptr @__vyrn_vj_obj()"));
        if payloads.len() == 1 {
            let (pv, pty) = &payloads[0];
            let c = self.emit_encode(pv, pty)?;
            self.emit(format!("call void @__vyrn_vj_set(ptr {obj}, ptr {nameg}, ptr {c})"));
        } else {
            let arr = self.fresh_tmp();
            self.emit(format!("{arr} = call ptr @__vyrn_vj_arr()"));
            for (pv, pty) in payloads {
                let c = self.emit_encode(pv, pty)?;
                self.emit(format!("call void @__vyrn_vj_push(ptr {arr}, ptr {c})"));
            }
            self.emit(format!("call void @__vyrn_vj_set(ptr {obj}, ptr {nameg}, ptr {arr})"));
        }
        Ok(obj)
    }

    /// Encode a payload enum aggregate `val` (type `ll`) by switching on its tag
    /// and emitting each variant's wire form. Returns the `VJ*` register.
    fn emit_encode_enum_body(
        &mut self,
        val: &str,
        vs: &[EnumVariant],
        ll: &str,
    ) -> Result<String, String> {
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ll} {val}, 0"));
        let slot = self.fresh_alloca("ptr");
        let done = self.fresh_label("enc.enum.done");
        let dflt = self.fresh_label("enc.enum.dflt");
        let mut cases = Vec::new();
        let mut labels = Vec::new();
        for i in 0..vs.len() {
            let l = self.fresh_label("enc.enum.case");
            cases.push(format!("i64 {i}, label %{l}"));
            labels.push(l);
        }
        self.emit_term(format!("switch i64 {tag}, label %{dflt} [ {} ]", cases.join(" ")));
        for (i, l) in labels.iter().enumerate() {
            self.emit_label(l);
            let v = &vs[i];
            let mut payloads = Vec::new();
            for (pi, pty) in v.payload.iter().enumerate() {
                let raw = self.fresh_tmp();
                self.emit(format!("{raw} = extractvalue {ll} {val}, {}", pi + 1));
                let pv = self.unbox_payload(&raw, pty);
                payloads.push((pv, pty.clone()));
            }
            let c = self.emit_encode_wire_variant(&v.name, &payloads)?;
            self.emit(format!("store ptr {c}, ptr {slot}"));
            self.emit_term(format!("br label %{done}"));
        }
        self.emit_label(&dflt);
        let nul = self.fresh_tmp();
        self.emit(format!("{nul} = call ptr @__vyrn_vj_null()"));
        self.emit(format!("store ptr {nul}, ptr {slot}"));
        self.emit_term(format!("br label %{done}"));
        self.emit_label(&done);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load ptr, ptr {slot}"));
        Ok(r)
    }

    /// Encode a `Result<T, E>` value `val` (`{ i1, i64, i64 }`) as `{"Ok":<T>}` /
    /// `{"Err":<E>}` (RFC-0024).
    fn emit_encode_result(&mut self, val: &str, t: &Type, e: &Type) -> Result<String, String> {
        let tag = self.fresh_tmp();
        let w0 = self.fresh_tmp();
        let w1 = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {val}, 0"));
        self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {val}, 1"));
        self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {val}, 2"));
        let slot = self.fresh_alloca("ptr");
        let ok_l = self.fresh_label("enc.res.ok");
        let err_l = self.fresh_label("enc.res.err");
        let done_l = self.fresh_label("enc.res.done");
        self.emit_term(format!("br i1 {tag}, label %{ok_l}, label %{err_l}"));
        self.emit_label(&ok_l);
        let okv = self.decode_payload(&w0, &w1, t);
        let okc = self.emit_encode_wire_variant("Ok", &[(okv, t.clone())])?;
        self.emit(format!("store ptr {okc}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&err_l);
        let errv = self.decode_payload(&w0, &w1, e);
        let errc = self.emit_encode_wire_variant("Err", &[(errv, e.clone())])?;
        self.emit(format!("store ptr {errc}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load ptr, ptr {slot}"));
        Ok(r)
    }

    /// Encode one record field into `obj`, honoring the None-field omission for
    /// an `Option`-typed field (Some -> set the decoded payload; None -> omit).
    fn emit_encode_field(&mut self, obj: &str, fv: &str, f: &Field) -> Result<(), String> {
        let key = self.str_g(&f.name)?;
        if let Type::Option(inner) = self.resolve(&f.ty) {
            let tag = self.fresh_tmp();
            self.emit(format!("{tag} = extractvalue {{ i1, i64, i64 }} {fv}, 0"));
            let set_l = self.fresh_label("enc.fld.set");
            let skip_l = self.fresh_label("enc.fld.skip");
            self.emit_term(format!("br i1 {tag}, label %{set_l}, label %{skip_l}"));
            self.emit_label(&set_l);
            let w0 = self.fresh_tmp();
            let w1 = self.fresh_tmp();
            self.emit(format!("{w0} = extractvalue {{ i1, i64, i64 }} {fv}, 1"));
            self.emit(format!("{w1} = extractvalue {{ i1, i64, i64 }} {fv}, 2"));
            let iv = self.decode_payload(&w0, &w1, &inner);
            let c = self.emit_encode(&iv, &inner)?;
            self.emit(format!("call void @__vyrn_vj_set(ptr {obj}, ptr {key}, ptr {c})"));
            self.emit_term(format!("br label %{skip_l}"));
            self.emit_label(&skip_l);
        } else {
            let c = self.emit_encode(fv, &f.ty)?;
            self.emit(format!("call void @__vyrn_vj_set(ptr {obj}, ptr {key}, ptr {c})"));
        }
        Ok(())
    }

    /// Push a decode `Issue` into the shim's accumulator with a constant key and
    /// message and a (runtime) path register.
    fn push_issue(&mut self, issues: &str, key: &str, path: &str, msg: &str) -> Result<(), String> {
        let kg = self.str_g(key)?;
        let mg = self.str_g(msg)?;
        self.emit(format!(
            "call void @__vyrn_issues_push(ptr {issues}, ptr {kg}, ptr {path}, ptr {mg})"
        ));
        Ok(())
    }

    /// Push a `json.type` Issue whose message (`expected X, found <kind>`) is
    /// assembled at runtime from a constant `expected` phrase and the node kind.
    fn push_type_issue(
        &mut self,
        issues: &str,
        path: &str,
        expected: &str,
        kind: &str,
    ) -> Result<(), String> {
        let kg = self.str_g("json.type")?;
        let eg = self.str_g(expected)?;
        let msg = self.fresh_tmp();
        self.emit(format!("{msg} = call ptr @__vyrn_json_type_msg(ptr {eg}, i32 {kind})"));
        self.emit(format!(
            "call void @__vyrn_issues_push(ptr {issues}, ptr {kg}, ptr {path}, ptr {msg})"
        ));
        Ok(())
    }

    /// Emit IR that decodes DOM node `vj` (assumed non-null) into a value of
    /// type `ty`, accumulating Issues under `path`. Returns the value register.
    fn emit_decode(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        ty: &Type,
    ) -> Result<String, String> {
        // A named record routes to its generated decoder; a named validated
        // scalar decodes its base then runs the `where` clause (accumulating a
        // `validate` Issue) — but only if the base decoded cleanly.
        if let Type::Named(n) = ty {
            match self.resolve(ty) {
                Type::Record(_) if self.types.contains_key(n) => {
                    let ll = self.llt(ty);
                    let r = self.fresh_tmp();
                    self.emit(format!(
                        "{r} = call {ll} @__vyrn_dec_{n}(ptr {vj}, ptr {path}, ptr {issues})"
                    ));
                    return Ok(r);
                }
                Type::Enum(vs) => {
                    // A pure-nullary enum decodes inline (string matching,
                    // byte-identical to RFC-0018). A payload enum routes to its
                    // generated decoder so self-referential payloads call back.
                    if vs.iter().all(|v| v.payload.is_empty()) {
                        return self.emit_decode_enum(vj, path, issues, ty);
                    }
                    let ll = self.llt(ty);
                    let r = self.fresh_tmp();
                    self.emit(format!(
                        "{r} = call {ll} @__vyrn_dec_{n}(ptr {vj}, ptr {path}, ptr {issues})"
                    ));
                    return Ok(r);
                }
                _ => {
                    let decl = self.types.get(n).cloned().unwrap();
                    let before = self.fresh_tmp();
                    self.emit(format!("{before} = call i64 @__vyrn_issues_len(ptr {issues})"));
                    let base_val = self.emit_decode(vj, path, issues, &decl.base)?;
                    if decl.predicate.is_some() {
                        self.emit_validate_check(issues, path, &decl, &base_val, &before)?;
                    }
                    return Ok(base_val);
                }
            }
        }
        match self.resolve(ty) {
            Type::Int => self.emit_decode_int(vj, path, issues, 64, true),
            Type::IntN { bits, signed } => self.emit_decode_int(vj, path, issues, bits, signed),
            Type::Float => self.emit_decode_float(vj, path, issues, false),
            Type::Float32 => self.emit_decode_float(vj, path, issues, true),
            Type::Bool => self.emit_decode_bool(vj, path, issues),
            Type::Str => self.emit_decode_str(vj, path, issues),
            Type::Enum(_) => self.emit_decode_enum(vj, path, issues, ty),
            Type::Result(t, e) => self.emit_decode_result(vj, path, issues, &t, &e),
            Type::Option(inner) => self.emit_decode_option(vj, path, issues, &inner),
            Type::Array(inner) => self.emit_decode_array(vj, path, issues, &inner),
            Type::Map(_, val) => self.emit_decode_map(vj, path, issues, &val),
            Type::Record(_) => {
                // Anonymous record decode target: inline via a temporary decl.
                let tmp = TypeDecl {
                    name: String::new(),
                    exported: false,
                    module: None,
                    doc: None,
                    type_params: Vec::new(),
                    base: self.resolve(ty),
                    predicate: None,
                    line: 0,
                };
                self.emit_decode_record_body(vj, path, issues, &tmp)
            }
            other => Err(format!("fromJson: cannot decode {other:?}")),
        }
    }

    /// After a validated type's base decoded, run its predicate and push a
    /// `validate` Issue if it is false — but skip the check when the base decode
    /// already accumulated an Issue (mirrors the interpreter's `?`-guarded walk).
    fn emit_validate_check(
        &mut self,
        issues: &str,
        path: &str,
        decl: &TypeDecl,
        base_val: &str,
        before: &str,
    ) -> Result<(), String> {
        let after = self.fresh_tmp();
        self.emit(format!("{after} = call i64 @__vyrn_issues_len(ptr {issues})"));
        let clean = self.fresh_tmp();
        self.emit(format!("{clean} = icmp eq i64 {before}, {after}"));
        let chk_l = self.fresh_label("dec.val.chk");
        let done_l = self.fresh_label("dec.val.done");
        self.emit_term(format!("br i1 {clean}, label %{chk_l}, label %{done_l}"));
        self.emit_label(&chk_l);
        let cond = self.emit_predicate_cond(decl, base_val)?;
        let bad = self.fresh_tmp();
        self.emit(format!("{bad} = xor i1 {cond}, true"));
        let push_l = self.fresh_label("dec.val.push");
        self.emit_term(format!("br i1 {bad}, label %{push_l}, label %{done_l}"));
        self.emit_label(&push_l);
        let msg = vyrn_frontend::codec::validate_message(decl);
        self.push_issue(issues, "validate", path, &msg)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        Ok(())
    }

    fn emit_decode_int(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        bits: u8,
        signed: bool,
    ) -> Result<String, String> {
        let outp = self.fresh_alloca("i64");
        let sflag = if signed { 1 } else { 0 };
        let rc = self.fresh_tmp();
        self.emit(format!(
            "{rc} = call i32 @__vyrn_vj_asint(ptr {vj}, i32 {bits}, i32 {sflag}, ptr {outp})"
        ));
        let bad = self.fresh_tmp();
        self.emit(format!("{bad} = icmp ne i32 {rc}, 0"));
        let ill = if bits >= 64 { "i64".to_string() } else { format!("i{bits}") };
        let slot = self.fresh_alloca(&ill);
        self.emit(format!("store {ill} 0, ptr {slot}"));
        let bad_l = self.fresh_label("dec.int.bad");
        let ok_l = self.fresh_label("dec.int.ok");
        let done_l = self.fresh_label("dec.int.done");
        self.emit_term(format!("br i1 {bad}, label %{bad_l}, label %{ok_l}"));
        self.emit_label(&bad_l);
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        self.push_type_issue(issues, path, "integer", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let v = self.fresh_tmp();
        self.emit(format!("{v} = load i64, ptr {outp}"));
        let stored = if bits >= 64 {
            v.clone()
        } else {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = trunc i64 {v} to i{bits}"));
            t
        };
        self.emit(format!("store {ill} {stored}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {ill}, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_float(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        single: bool,
    ) -> Result<String, String> {
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isnum = self.fresh_tmp();
        self.emit(format!("{isnum} = icmp eq i32 {kind}, 2"));
        let ell = if single { "float" } else { "double" };
        let slot = self.fresh_alloca(ell);
        self.emit(format!("store {ell} 0.0, ptr {slot}"));
        let ok_l = self.fresh_label("dec.flt.ok");
        let bad_l = self.fresh_label("dec.flt.bad");
        let done_l = self.fresh_label("dec.flt.done");
        self.emit_term(format!("br i1 {isnum}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "number", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let d = self.fresh_tmp();
        self.emit(format!("{d} = call double @__vyrn_vj_asfloat(ptr {vj})"));
        let stored = if single {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = fptrunc double {d} to float"));
            t
        } else {
            d.clone()
        };
        self.emit(format!("store {ell} {stored}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {ell}, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_bool(&mut self, vj: &str, path: &str, issues: &str) -> Result<String, String> {
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isbool = self.fresh_tmp();
        self.emit(format!("{isbool} = icmp eq i32 {kind}, 1"));
        let slot = self.fresh_alloca("i1");
        self.emit(format!("store i1 false, ptr {slot}"));
        let ok_l = self.fresh_label("dec.bool.ok");
        let bad_l = self.fresh_label("dec.bool.bad");
        let done_l = self.fresh_label("dec.bool.done");
        self.emit_term(format!("br i1 {isbool}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "boolean", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let b = self.fresh_tmp();
        self.emit(format!("{b} = call i32 @__vyrn_vj_bool_get(ptr {vj})"));
        let bt = self.fresh_tmp();
        self.emit(format!("{bt} = icmp ne i32 {b}, 0"));
        self.emit(format!("store i1 {bt}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load i1, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_str(&mut self, vj: &str, path: &str, issues: &str) -> Result<String, String> {
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isstr = self.fresh_tmp();
        self.emit(format!("{isstr} = icmp eq i32 {kind}, 3"));
        let slot = self.fresh_alloca("ptr");
        self.emit(format!("store ptr null, ptr {slot}"));
        let ok_l = self.fresh_label("dec.str.ok");
        let bad_l = self.fresh_label("dec.str.bad");
        let done_l = self.fresh_label("dec.str.done");
        self.emit_term(format!("br i1 {isstr}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "string", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let s = self.fresh_tmp();
        self.emit(format!("{s} = call ptr @__vyrn_vj_str_get(ptr {vj})"));
        self.emit(format!("store ptr {s}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load ptr, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_enum(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        ty: &Type,
    ) -> Result<String, String> {
        let vs = match self.resolve(ty) {
            Type::Enum(vs) => vs,
            _ => return Err("emit_decode_enum on non-enum".to_string()),
        };
        let ll = self.llt(ty);
        let expected = vyrn_frontend::codec::enum_expected(&vs);
        // A payload enum has the richer RFC-0024 wire form (object tags); a
        // pure-nullary enum is the byte-identical RFC-0018 string form.
        if vs.iter().any(|v| !v.payload.is_empty()) {
            return self.emit_decode_enum_payload(vj, path, issues, &vs, &ll, &expected);
        }
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isstr = self.fresh_tmp();
        self.emit(format!("{isstr} = icmp eq i32 {kind}, 3"));
        let slot = self.fresh_alloca(&ll);
        self.emit(format!("store {ll} zeroinitializer, ptr {slot}"));
        let str_l = self.fresh_label("dec.enum.str");
        let notstr_l = self.fresh_label("dec.enum.notstr");
        let done_l = self.fresh_label("dec.enum.done");
        self.emit_term(format!("br i1 {isstr}, label %{str_l}, label %{notstr_l}"));
        self.emit_label(&notstr_l);
        self.push_type_issue(issues, path, &expected, &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&str_l);
        let s = self.fresh_tmp();
        self.emit(format!("{s} = call ptr @__vyrn_vj_str_get(ptr {vj})"));
        // Sequential strcmp against each variant name.
        for (i, v) in vs.iter().enumerate() {
            let g = self.str_g(&v.name)?;
            let cmp = self.fresh_tmp();
            self.emit(format!("{cmp} = call i32 @strcmp(ptr {s}, ptr {g})"));
            let eq = self.fresh_tmp();
            self.emit(format!("{eq} = icmp eq i32 {cmp}, 0"));
            let hit_l = self.fresh_label("dec.enum.hit");
            let next_l = self.fresh_label("dec.enum.next");
            self.emit_term(format!("br i1 {eq}, label %{hit_l}, label %{next_l}"));
            self.emit_label(&hit_l);
            let e = self.fresh_tmp();
            self.emit(format!("{e} = insertvalue {ll} zeroinitializer, i64 {i}, 0"));
            self.emit(format!("store {ll} {e}, ptr {slot}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&next_l);
        }
        // No variant matched.
        self.push_type_issue(issues, path, &expected, &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {ll}, ptr {slot}"));
        Ok(r)
    }

    /// The RFC-0024 payload-enum decoder: a bare string is a nullary variant; a
    /// one-key object `{"Tag":..}` names a payload variant (single payload direct,
    /// tuple payload as an array). Any other shape / unknown key is the locked
    /// expected-one-of `json.type` Issue. Returns the enum aggregate register.
    fn emit_decode_enum_payload(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        vs: &[EnumVariant],
        ll: &str,
        expected: &str,
    ) -> Result<String, String> {
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let slot = self.fresh_alloca(ll);
        self.emit(format!("store {ll} zeroinitializer, ptr {slot}"));
        let isstr = self.fresh_tmp();
        self.emit(format!("{isstr} = icmp eq i32 {kind}, 3"));
        let str_l = self.fresh_label("dec.penum.str");
        let notstr_l = self.fresh_label("dec.penum.notstr");
        let obj_l = self.fresh_label("dec.penum.obj");
        let key_l = self.fresh_label("dec.penum.key");
        let mismatch_l = self.fresh_label("dec.penum.mismatch");
        let done_l = self.fresh_label("dec.penum.done");
        self.emit_term(format!("br i1 {isstr}, label %{str_l}, label %{notstr_l}"));
        // Shared mismatch: `expected one of ..., found <kind>` (kind is still live).
        self.emit_label(&mismatch_l);
        self.push_type_issue(issues, path, expected, &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        // String → a nullary variant.
        self.emit_label(&str_l);
        let s = self.fresh_tmp();
        self.emit(format!("{s} = call ptr @__vyrn_vj_str_get(ptr {vj})"));
        for (i, v) in vs.iter().enumerate() {
            if !v.payload.is_empty() {
                continue; // a payload variant as a bare string is a mismatch
            }
            let g = self.str_g(&v.name)?;
            let cmp = self.fresh_tmp();
            self.emit(format!("{cmp} = call i32 @strcmp(ptr {s}, ptr {g})"));
            let eq = self.fresh_tmp();
            self.emit(format!("{eq} = icmp eq i32 {cmp}, 0"));
            let hit_l = self.fresh_label("dec.penum.shit");
            let next_l = self.fresh_label("dec.penum.snext");
            self.emit_term(format!("br i1 {eq}, label %{hit_l}, label %{next_l}"));
            self.emit_label(&hit_l);
            let e = self.fresh_tmp();
            self.emit(format!("{e} = insertvalue {ll} zeroinitializer, i64 {i}, 0"));
            self.emit(format!("store {ll} {e}, ptr {slot}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&next_l);
        }
        self.emit_term(format!("br label %{mismatch_l}"));
        // Not a string → must be a single-key object.
        self.emit_label(&notstr_l);
        let isobj = self.fresh_tmp();
        self.emit(format!("{isobj} = icmp eq i32 {kind}, 5"));
        self.emit_term(format!("br i1 {isobj}, label %{obj_l}, label %{mismatch_l}"));
        self.emit_label(&obj_l);
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_vj_obj_len(ptr {vj})"));
        let one = self.fresh_tmp();
        self.emit(format!("{one} = icmp eq i64 {n}, 1"));
        self.emit_term(format!("br i1 {one}, label %{key_l}, label %{mismatch_l}"));
        self.emit_label(&key_l);
        let key = self.fresh_tmp();
        self.emit(format!("{key} = call ptr @__vyrn_vj_obj_key(ptr {vj}, i64 0)"));
        let valj = self.fresh_tmp();
        self.emit(format!("{valj} = call ptr @__vyrn_vj_obj_at(ptr {vj}, i64 0)"));
        for (i, v) in vs.iter().enumerate() {
            if v.payload.is_empty() {
                continue; // a nullary variant spelled as an object is a mismatch
            }
            let g = self.str_g(&v.name)?;
            let cmp = self.fresh_tmp();
            self.emit(format!("{cmp} = call i32 @strcmp(ptr {key}, ptr {g})"));
            let eq = self.fresh_tmp();
            self.emit(format!("{eq} = icmp eq i32 {cmp}, 0"));
            let hit_l = self.fresh_label("dec.penum.phit");
            let next_l = self.fresh_label("dec.penum.pnext");
            self.emit_term(format!("br i1 {eq}, label %{hit_l}, label %{next_l}"));
            self.emit_label(&hit_l);
            let child = self.fresh_tmp();
            self.emit(format!("{child} = call ptr @__vyrn_json_field_path(ptr {path}, ptr {g})"));
            self.emit_decode_enum_payload_arm(&valj, &child, issues, i, &v.payload, ll, slot.as_str(), &done_l)?;
            self.emit_label(&next_l);
        }
        self.emit_term(format!("br label %{mismatch_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {ll}, ptr {slot}"));
        Ok(r)
    }

    /// Decode the payload(s) of a matched payload variant (`tag` index `idx`) from
    /// `valj` at `child` path, build the enum aggregate, store it, and jump to
    /// `done_l`. A single payload decodes directly; two or more read a JSON array
    /// positionally (missing slots decode from `null`, a length mismatch surfaces
    /// as the element's own type Issue at `child[i]`).
    #[allow(clippy::too_many_arguments)]
    fn emit_decode_enum_payload_arm(
        &mut self,
        valj: &str,
        child: &str,
        issues: &str,
        idx: usize,
        payload: &[Type],
        ll: &str,
        slot: &str,
        done_l: &str,
    ) -> Result<(), String> {
        if payload.len() == 1 {
            let iv = self.emit_decode(valj, child, issues, &payload[0])?;
            let bx = self.box_payload(&iv, &payload[0]);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {ll} zeroinitializer, i64 {idx}, 0"));
            self.emit(format!("{b} = insertvalue {ll} {a}, i64 {bx}, 1"));
            self.emit(format!("store {ll} {b}, ptr {slot}"));
            self.emit_term(format!("br label %{done_l}"));
            return Ok(());
        }
        // Tuple payload: the value must be a JSON array.
        let kv = self.fresh_tmp();
        self.emit(format!("{kv} = call i32 @__vyrn_vj_kind(ptr {valj})"));
        let isarr = self.fresh_tmp();
        self.emit(format!("{isarr} = icmp eq i32 {kv}, 4"));
        let ok_l = self.fresh_label("dec.penum.tok");
        let bad_l = self.fresh_label("dec.penum.tbad");
        self.emit_term(format!("br i1 {isarr}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, child, "array", &kv)?;
        let z = self.fresh_tmp();
        self.emit(format!("{z} = insertvalue {ll} zeroinitializer, i64 {idx}, 0"));
        self.emit(format!("store {ll} {z}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let mut cur = self.fresh_tmp();
        self.emit(format!("{cur} = insertvalue {ll} zeroinitializer, i64 {idx}, 0"));
        for (j, pty) in payload.iter().enumerate() {
            let node = self.fresh_tmp();
            self.emit(format!("{node} = call ptr @__vyrn_vj_at_or_null(ptr {valj}, i64 {j})"));
            let ipath = self.fresh_tmp();
            self.emit(format!("{ipath} = call ptr @__vyrn_json_index_path(ptr {child}, i64 {j})"));
            let iv = self.emit_decode(&node, &ipath, issues, pty)?;
            let bx = self.box_payload(&iv, pty);
            let next = self.fresh_tmp();
            self.emit(format!("{next} = insertvalue {ll} {cur}, i64 {bx}, {}", j + 1));
            cur = next;
        }
        self.emit(format!("store {ll} {cur}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        Ok(())
    }

    /// Decode a `Result<T, E>` from `{"Ok":<T>}` / `{"Err":<E>}` (RFC-0024) into a
    /// `{ i1, i64, i64 }` aggregate. Any other shape is the locked expected-one-of
    /// (`one of \`Ok\`, \`Err\``) `json.type` Issue.
    fn emit_decode_result(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        t: &Type,
        e: &Type,
    ) -> Result<String, String> {
        let expected = vyrn_frontend::codec::result_expected();
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let slot = self.fresh_alloca("{ i1, i64, i64 }");
        self.emit(format!("store {{ i1, i64, i64 }} zeroinitializer, ptr {slot}"));
        let isobj = self.fresh_tmp();
        self.emit(format!("{isobj} = icmp eq i32 {kind}, 5"));
        let obj_l = self.fresh_label("dec.res.obj");
        let key_l = self.fresh_label("dec.res.key");
        let mismatch_l = self.fresh_label("dec.res.mismatch");
        let done_l = self.fresh_label("dec.res.done");
        self.emit_term(format!("br i1 {isobj}, label %{obj_l}, label %{mismatch_l}"));
        self.emit_label(&mismatch_l);
        self.push_type_issue(issues, path, &expected, &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&obj_l);
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_vj_obj_len(ptr {vj})"));
        let one = self.fresh_tmp();
        self.emit(format!("{one} = icmp eq i64 {n}, 1"));
        self.emit_term(format!("br i1 {one}, label %{key_l}, label %{mismatch_l}"));
        self.emit_label(&key_l);
        let key = self.fresh_tmp();
        self.emit(format!("{key} = call ptr @__vyrn_vj_obj_key(ptr {vj}, i64 0)"));
        let valj = self.fresh_tmp();
        self.emit(format!("{valj} = call ptr @__vyrn_vj_obj_at(ptr {vj}, i64 0)"));
        // Two arms: `Ok` (tag 1) and `Err` (tag 0).
        for (is_ok, name, arm_ty) in [(1, "Ok", t), (0, "Err", e)] {
            let g = self.str_g(name)?;
            let cmp = self.fresh_tmp();
            self.emit(format!("{cmp} = call i32 @strcmp(ptr {key}, ptr {g})"));
            let eq = self.fresh_tmp();
            self.emit(format!("{eq} = icmp eq i32 {cmp}, 0"));
            let hit_l = self.fresh_label("dec.res.hit");
            let next_l = self.fresh_label("dec.res.next");
            self.emit_term(format!("br i1 {eq}, label %{hit_l}, label %{next_l}"));
            self.emit_label(&hit_l);
            let child = self.fresh_tmp();
            self.emit(format!("{child} = call ptr @__vyrn_json_field_path(ptr {path}, ptr {g})"));
            let iv = self.emit_decode(&valj, &child, issues, arm_ty)?;
            let (w0, w1) = self.encode_payload(&iv, arm_ty);
            let a = self.fresh_tmp();
            let b = self.fresh_tmp();
            let c = self.fresh_tmp();
            self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 {is_ok}, 0"));
            self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"));
            self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"));
            self.emit(format!("store {{ i1, i64, i64 }} {c}, ptr {slot}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&next_l);
        }
        self.emit_term(format!("br label %{mismatch_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {{ i1, i64, i64 }}, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_option(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        inner: &Type,
    ) -> Result<String, String> {
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isnull = self.fresh_tmp();
        self.emit(format!("{isnull} = icmp eq i32 {kind}, 0"));
        let slot = self.fresh_alloca("{ i1, i64, i64 }");
        let none_l = self.fresh_label("dec.opt.none");
        let some_l = self.fresh_label("dec.opt.some");
        let done_l = self.fresh_label("dec.opt.done");
        self.emit_term(format!("br i1 {isnull}, label %{none_l}, label %{some_l}"));
        self.emit_label(&none_l);
        self.emit(format!("store {{ i1, i64, i64 }} {{ i1 0, i64 0, i64 0 }}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&some_l);
        let iv = self.emit_decode(vj, path, issues, inner)?;
        let (w0, w1) = self.encode_payload(&iv, inner);
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!("{a} = insertvalue {{ i1, i64, i64 }} undef, i1 1, 0"));
        self.emit(format!("{b} = insertvalue {{ i1, i64, i64 }} {a}, i64 {w0}, 1"));
        self.emit(format!("{c} = insertvalue {{ i1, i64, i64 }} {b}, i64 {w1}, 2"));
        self.emit(format!("store {{ i1, i64, i64 }} {c}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {{ i1, i64, i64 }}, ptr {slot}"));
        Ok(r)
    }

    fn emit_decode_array(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        inner: &Type,
    ) -> Result<String, String> {
        let ell = self.llt(inner);
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isarr = self.fresh_tmp();
        self.emit(format!("{isarr} = icmp eq i32 {kind}, 4"));
        let slot = self.fresh_alloca("{ ptr, i64, i64 }");
        self.emit(format!(
            "store {{ ptr, i64, i64 }} {{ ptr null, i64 0, i64 0 }}, ptr {slot}"
        ));
        let ok_l = self.fresh_label("dec.arr.ok");
        let bad_l = self.fresh_label("dec.arr.bad");
        let done_l = self.fresh_label("dec.arr.done");
        self.emit_term(format!("br i1 {isarr}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "array", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_vj_len(ptr {vj})"));
        // buffer = n * sizeof(elem)
        let szp = self.fresh_tmp();
        let sz = self.fresh_tmp();
        self.emit(format!("{szp} = getelementptr {ell}, ptr null, i64 {n}"));
        self.emit(format!("{sz} = ptrtoint ptr {szp} to i64"));
        let buf = self.fresh_tmp();
        self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {sz})"));
        let idx = self.fresh_alloca("i64");
        self.emit(format!("store i64 0, ptr {idx}"));
        let cond_l = self.fresh_label("dec.arr.cond");
        let body_l = self.fresh_label("dec.arr.body");
        let fill_l = self.fresh_label("dec.arr.fill");
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let i = self.fresh_tmp();
        self.emit(format!("{i} = load i64, ptr {idx}"));
        let more = self.fresh_tmp();
        self.emit(format!("{more} = icmp slt i64 {i}, {n}"));
        self.emit_term(format!("br i1 {more}, label %{body_l}, label %{fill_l}"));
        self.emit_label(&body_l);
        let node = self.fresh_tmp();
        self.emit(format!("{node} = call ptr @__vyrn_vj_at(ptr {vj}, i64 {i})"));
        let childpath = self.fresh_tmp();
        self.emit(format!(
            "{childpath} = call ptr @__vyrn_json_index_path(ptr {path}, i64 {i})"
        ));
        let ev = self.emit_decode(&node, &childpath, issues, inner)?;
        let ep = self.fresh_tmp();
        self.emit(format!("{ep} = getelementptr {ell}, ptr {buf}, i64 {i}"));
        self.emit(format!("store {ell} {ev}, ptr {ep}"));
        let ni = self.fresh_tmp();
        self.emit(format!("{ni} = add i64 {i}, 1"));
        self.emit(format!("store i64 {ni}, ptr {idx}"));
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&fill_l);
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        let c = self.fresh_tmp();
        self.emit(format!("{a} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"));
        self.emit(format!("{b} = insertvalue {{ ptr, i64, i64 }} {a}, i64 {n}, 1"));
        self.emit(format!("{c} = insertvalue {{ ptr, i64, i64 }} {b}, i64 {n}, 2"));
        self.emit(format!("store {{ ptr, i64, i64 }} {c}, ptr {slot}"));
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {{ ptr, i64, i64 }}, ptr {slot}"));
        Ok(r)
    }

    /// Decode a JSON object into a `Map<String, V>` (RFC-0028): document order
    /// becomes insertion order; each value validates as `V` at path
    /// `field.<key>`. Duplicate keys mirror the record decoder's first-wins
    /// policy (`__vyrn_vj_get`): a repeated key is skipped. Returns the map
    /// aggregate `{ ptr keys, ptr vals, i64 len, i64 cap }`.
    fn emit_decode_map(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        val: &Type,
    ) -> Result<String, String> {
        let vll = self.llt(val);
        let esz = self.fresh_tmp();
        self.emit(format!(
            "{esz} = ptrtoint ptr getelementptr ({vll}, ptr null, i64 1) to i64"
        ));
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isobj = self.fresh_tmp();
        self.emit(format!("{isobj} = icmp eq i32 {kind}, 5"));
        let slot = self.fresh_alloca("{ ptr, ptr, i64, i64 }");
        self.emit(format!(
            "store {{ ptr, ptr, i64, i64 }} {{ ptr null, ptr null, i64 0, i64 0 }}, ptr {slot}"
        ));
        let ok_l = self.fresh_label("dec.map.ok");
        let bad_l = self.fresh_label("dec.map.bad");
        let done_l = self.fresh_label("dec.map.done");
        self.emit_term(format!("br i1 {isobj}, label %{ok_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "object", &kind)?;
        self.emit_term(format!("br label %{done_l}"));
        self.emit_label(&ok_l);
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_vj_obj_len(ptr {vj})"));
        let idx = self.fresh_alloca("i64");
        self.emit(format!("store i64 0, ptr {idx}"));
        let cond_l = self.fresh_label("dec.map.cond");
        let body_l = self.fresh_label("dec.map.body");
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let i = self.fresh_tmp();
        self.emit(format!("{i} = load i64, ptr {idx}"));
        let more = self.fresh_tmp();
        self.emit(format!("{more} = icmp slt i64 {i}, {n}"));
        self.emit_term(format!("br i1 {more}, label %{body_l}, label %{done_l}"));
        self.emit_label(&body_l);
        let key = self.fresh_tmp();
        self.emit(format!("{key} = call ptr @__vyrn_vj_obj_key(ptr {vj}, i64 {i})"));
        let node = self.fresh_tmp();
        self.emit(format!("{node} = call ptr @__vyrn_vj_obj_at(ptr {vj}, i64 {i})"));
        let childpath = self.fresh_tmp();
        self.emit(format!(
            "{childpath} = call ptr @__vyrn_json_field_path(ptr {path}, ptr {key})"
        ));
        let ev = self.emit_decode(&node, &childpath, issues, val)?;
        // First-wins: only append when the key is not already present.
        let hdr = self.fresh_tmp();
        let keys = self.fresh_tmp();
        let len = self.fresh_tmp();
        self.emit(format!("{hdr} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
        self.emit(format!("{keys} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 0"));
        self.emit(format!("{len} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr}, 2"));
        let fidx = self.fresh_tmp();
        self.emit(format!(
            "{fidx} = call i64 @__vyrn_map_find(ptr {keys}, i64 {len}, ptr {key})"
        ));
        let absent = self.fresh_tmp();
        self.emit(format!("{absent} = icmp slt i64 {fidx}, 0"));
        let ins_l = self.fresh_label("dec.map.ins");
        let next_l = self.fresh_label("dec.map.next");
        self.emit_term(format!("br i1 {absent}, label %{ins_l}, label %{next_l}"));
        self.emit_label(&ins_l);
        self.emit(format!("call void @__vyrn_map_reserve(ptr {slot}, i64 {esz})"));
        let hdr2 = self.fresh_tmp();
        let keys2 = self.fresh_tmp();
        let vals2 = self.fresh_tmp();
        self.emit(format!("{hdr2} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
        self.emit(format!("{keys2} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr2}, 0"));
        self.emit(format!("{vals2} = extractvalue {{ ptr, ptr, i64, i64 }} {hdr2}, 1"));
        let kep = self.fresh_tmp();
        self.emit(format!("{kep} = getelementptr ptr, ptr {keys2}, i64 {len}"));
        self.emit(format!("store ptr {key}, ptr {kep}"));
        let vep = self.fresh_tmp();
        self.emit(format!("{vep} = getelementptr {vll}, ptr {vals2}, i64 {len}"));
        self.emit(format!("store {vll} {ev}, ptr {vep}"));
        let nl = self.fresh_tmp();
        self.emit(format!("{nl} = add i64 {len}, 1"));
        let lenp = self.fresh_tmp();
        self.emit(format!(
            "{lenp} = getelementptr {{ ptr, ptr, i64, i64 }}, ptr {slot}, i64 0, i32 2"
        ));
        self.emit(format!("store i64 {nl}, ptr {lenp}"));
        self.emit_term(format!("br label %{next_l}"));
        self.emit_label(&next_l);
        let ni = self.fresh_tmp();
        self.emit(format!("{ni} = add i64 {i}, 1"));
        self.emit(format!("store i64 {ni}, ptr {idx}"));
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {{ ptr, ptr, i64, i64 }}, ptr {slot}"));
        Ok(r)
    }

    /// The body of a record decoder: check the node is an object, decode each
    /// field (honoring Option absent-or-null and required-field-missing), then
    /// run the record's cross-field `where` clause if all fields decoded
    /// cleanly. `decl.base` must be a `Record`. Returns the record value.
    fn emit_decode_record_body(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        decl: &TypeDecl,
    ) -> Result<String, String> {
        let fields = match &decl.base {
            Type::Record(fs) => fs.clone(),
            _ => return Err("emit_decode_record_body on non-record".to_string()),
        };
        let ll = self.llt(&decl.base);
        let res = self.fresh_alloca(&ll);
        self.emit(format!("store {ll} zeroinitializer, ptr {res}"));
        let before = self.fresh_tmp();
        self.emit(format!("{before} = call i64 @__vyrn_issues_len(ptr {issues})"));
        // Node must be an object.
        let kind = self.fresh_tmp();
        self.emit(format!("{kind} = call i32 @__vyrn_vj_kind(ptr {vj})"));
        let isobj = self.fresh_tmp();
        self.emit(format!("{isobj} = icmp eq i32 {kind}, 5"));
        let obj_l = self.fresh_label("dec.rec.obj");
        let bad_l = self.fresh_label("dec.rec.bad");
        let ret_l = self.fresh_label("dec.rec.ret");
        self.emit_term(format!("br i1 {isobj}, label %{obj_l}, label %{bad_l}"));
        self.emit_label(&bad_l);
        self.push_type_issue(issues, path, "object", &kind)?;
        self.emit_term(format!("br label %{ret_l}"));
        self.emit_label(&obj_l);
        for (i, f) in fields.iter().enumerate() {
            self.emit_decode_field(vj, path, issues, &res, &ll, i, f)?;
        }
        // Cross-field predicate, only if the fields decoded cleanly.
        if decl.predicate.is_some() {
            let rec_val = self.fresh_tmp();
            self.emit(format!("{rec_val} = load {ll}, ptr {res}"));
            self.emit_validate_check(issues, path, decl, &rec_val, &before)?;
        }
        self.emit_term(format!("br label %{ret_l}"));
        self.emit_label(&ret_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {ll}, ptr {res}"));
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_decode_field(
        &mut self,
        vj: &str,
        path: &str,
        issues: &str,
        res: &str,
        rec_ll: &str,
        i: usize,
        f: &Field,
    ) -> Result<(), String> {
        let key = self.str_g(&f.name)?;
        let child = self.fresh_tmp();
        self.emit(format!(
            "{child} = call ptr @__vyrn_json_field_path(ptr {path}, ptr {key})"
        ));
        let node = self.fresh_tmp();
        self.emit(format!("{node} = call ptr @__vyrn_vj_get(ptr {vj}, ptr {key})"));
        let absent = self.fresh_tmp();
        self.emit(format!("{absent} = icmp eq ptr {node}, null"));
        let fll = self.llt(&f.ty);
        let fp = self.fresh_tmp();
        self.emit(format!("{fp} = getelementptr {rec_ll}, ptr {res}, i64 0, i32 {i}"));
        if let Type::Option(_) = self.resolve(&f.ty) {
            // Absent -> None; present -> emit_decode (which maps null -> None).
            let none_l = self.fresh_label("dec.fld.none");
            let present_l = self.fresh_label("dec.fld.present");
            let done_l = self.fresh_label("dec.fld.done");
            self.emit_term(format!("br i1 {absent}, label %{none_l}, label %{present_l}"));
            self.emit_label(&none_l);
            self.emit(format!(
                "store {fll} {{ i1 0, i64 0, i64 0 }}, ptr {fp}"
            ));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&present_l);
            let fv = self.emit_decode(&node, &child, issues, &f.ty)?;
            self.emit(format!("store {fll} {fv}, ptr {fp}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&done_l);
        } else {
            let missing_l = self.fresh_label("dec.fld.missing");
            let present_l = self.fresh_label("dec.fld.present");
            let done_l = self.fresh_label("dec.fld.done");
            self.emit_term(format!("br i1 {absent}, label %{missing_l}, label %{present_l}"));
            self.emit_label(&missing_l);
            let msg = vyrn_frontend::codec::missing_message(&f.name);
            self.push_issue(issues, "json.missing", &child, &msg)?;
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&present_l);
            let fv = self.emit_decode(&node, &child, issues, &f.ty)?;
            self.emit(format!("store {fll} {fv}, ptr {fp}"));
            self.emit_term(format!("br label %{done_l}"));
            self.emit_label(&done_l);
        }
        Ok(())
    }

    /// Orchestrate `fromJson`: parse, decode into `tn`, and package the result
    /// as `Validation<T>` — `Valid(T)` when no Issue accumulated, else
    /// `Invalid([Issue])` materialized from the shim's issue list.
    fn gen_from_json(&mut self, tn: &str, s: &str) -> Result<(String, Type), String> {
        let target = Type::Named(tn.to_string());
        let vll = enum_ll(1); // Validation<T> = { i64 tag, i64 payload }
        let empty = self.str_g("")?;
        let valid_tag = self.variants.get("Valid").map(|(t, _)| *t).unwrap_or(0);
        let invalid_tag = self.variants.get("Invalid").map(|(t, _)| *t).unwrap_or(1);

        let issues = self.fresh_tmp();
        self.emit(format!("{issues} = call ptr @__vyrn_issues_new()"));
        let errslot = self.fresh_alloca("ptr");
        let vj = self.fresh_tmp();
        self.emit(format!("{vj} = call ptr @__vyrn_json_parse(ptr {s}, ptr {errslot})"));
        let failed = self.fresh_tmp();
        self.emit(format!("{failed} = icmp eq ptr {vj}, null"));
        let resslot = self.fresh_alloca(&vll);
        let fail_l = self.fresh_label("fj.parsefail");
        let ok_l = self.fresh_label("fj.parseok");
        let valid_l = self.fresh_label("fj.valid");
        let invalid_l = self.fresh_label("fj.invalid");
        let done_l = self.fresh_label("fj.done");
        self.emit_term(format!("br i1 {failed}, label %{fail_l}, label %{ok_l}"));

        // parse failure -> single json.parse Issue (message from the shim).
        self.emit_label(&fail_l);
        let err = self.fresh_tmp();
        self.emit(format!("{err} = load ptr, ptr {errslot}"));
        let pk = self.str_g("json.parse")?;
        self.emit(format!(
            "call void @__vyrn_issues_push(ptr {issues}, ptr {pk}, ptr {empty}, ptr {err})"
        ));
        self.emit_term(format!("br label %{invalid_l}"));

        // parse ok -> decode, then branch on whether any Issue accumulated.
        self.emit_label(&ok_l);
        let val = self.emit_decode(&vj, &empty, &issues, &target)?;
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_issues_len(ptr {issues})"));
        let clean = self.fresh_tmp();
        self.emit(format!("{clean} = icmp eq i64 {n}, 0"));
        self.emit_term(format!("br i1 {clean}, label %{valid_l}, label %{invalid_l}"));

        // Valid(val)
        self.emit_label(&valid_l);
        let boxed = self.box_payload(&val, &target);
        let v0 = self.fresh_tmp();
        let v1 = self.fresh_tmp();
        self.emit(format!("{v0} = insertvalue {vll} undef, i64 {valid_tag}, 0"));
        self.emit(format!("{v1} = insertvalue {vll} {v0}, i64 {boxed}, 1"));
        self.emit(format!("store {vll} {v1}, ptr {resslot}"));
        self.emit_term(format!("br label %{done_l}"));

        // Invalid([Issue]) — build the Vyrn Array<Issue> from the shim's list.
        self.emit_label(&invalid_l);
        let arr = self.build_issue_array(&issues)?;
        let issue_arr_ty = Type::Array(Box::new(Type::Named("Issue".to_string())));
        let boxed_arr = self.box_payload(&arr, &issue_arr_ty);
        let i0 = self.fresh_tmp();
        let i1 = self.fresh_tmp();
        self.emit(format!("{i0} = insertvalue {vll} undef, i64 {invalid_tag}, 0"));
        self.emit(format!("{i1} = insertvalue {vll} {i0}, i64 {boxed_arr}, 1"));
        self.emit(format!("store {vll} {i1}, ptr {resslot}"));
        self.emit_term(format!("br label %{done_l}"));

        self.emit_label(&done_l);
        let r = self.fresh_tmp();
        self.emit(format!("{r} = load {vll}, ptr {resslot}"));
        Ok((r, Type::App("Validation".to_string(), vec![target])))
    }

    /// Materialize the shim's issue list into a Vyrn `Array<Issue>` value
    /// (`{ ptr, i64, i64 }` of `{ ptr, ptr, ptr }` records).
    fn build_issue_array(&mut self, issues: &str) -> Result<String, String> {
        let n = self.fresh_tmp();
        self.emit(format!("{n} = call i64 @__vyrn_issues_len(ptr {issues})"));
        let szp = self.fresh_tmp();
        let bytes = self.fresh_tmp();
        self.emit(format!("{szp} = getelementptr {{ ptr, ptr, ptr }}, ptr null, i64 {n}"));
        self.emit(format!("{bytes} = ptrtoint ptr {szp} to i64"));
        let buf = self.fresh_tmp();
        self.emit(format!("{buf} = call ptr @__vyrn_malloc(i64 {bytes})"));
        let idx = self.fresh_alloca("i64");
        self.emit(format!("store i64 0, ptr {idx}"));
        let cond_l = self.fresh_label("iss.cond");
        let body_l = self.fresh_label("iss.body");
        let done_l = self.fresh_label("iss.done");
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&cond_l);
        let i = self.fresh_tmp();
        self.emit(format!("{i} = load i64, ptr {idx}"));
        let more = self.fresh_tmp();
        self.emit(format!("{more} = icmp slt i64 {i}, {n}"));
        self.emit_term(format!("br i1 {more}, label %{body_l}, label %{done_l}"));
        self.emit_label(&body_l);
        let k = self.fresh_tmp();
        let p = self.fresh_tmp();
        let m = self.fresh_tmp();
        self.emit(format!("{k} = call ptr @__vyrn_issue_key(ptr {issues}, i64 {i})"));
        self.emit(format!("{p} = call ptr @__vyrn_issue_path(ptr {issues}, i64 {i})"));
        self.emit(format!("{m} = call ptr @__vyrn_issue_msg(ptr {issues}, i64 {i})"));
        let is0 = self.fresh_tmp();
        let is1 = self.fresh_tmp();
        let is2 = self.fresh_tmp();
        self.emit(format!("{is0} = insertvalue {{ ptr, ptr, ptr }} undef, ptr {k}, 0"));
        self.emit(format!("{is1} = insertvalue {{ ptr, ptr, ptr }} {is0}, ptr {p}, 1"));
        self.emit(format!("{is2} = insertvalue {{ ptr, ptr, ptr }} {is1}, ptr {m}, 2"));
        let slot = self.fresh_tmp();
        self.emit(format!("{slot} = getelementptr {{ ptr, ptr, ptr }}, ptr {buf}, i64 {i}"));
        self.emit(format!("store {{ ptr, ptr, ptr }} {is2}, ptr {slot}"));
        let ni = self.fresh_tmp();
        self.emit(format!("{ni} = add i64 {i}, 1"));
        self.emit(format!("store i64 {ni}, ptr {idx}"));
        self.emit_term(format!("br label %{cond_l}"));
        self.emit_label(&done_l);
        let a0 = self.fresh_tmp();
        let a1 = self.fresh_tmp();
        let a2 = self.fresh_tmp();
        self.emit(format!("{a0} = insertvalue {{ ptr, i64, i64 }} undef, ptr {buf}, 0"));
        self.emit(format!("{a1} = insertvalue {{ ptr, i64, i64 }} {a0}, i64 {n}, 1"));
        self.emit(format!("{a2} = insertvalue {{ ptr, i64, i64 }} {a1}, i64 {n}, 2"));
        Ok(a2)
    }
}

/// Whether a pattern matches the tag-1 variant (`Some`/`Ok`). Only used on the
/// Option/Result path; user-enum variants go through `gen_match_enum`.
fn pattern_is_one(p: &Pattern) -> bool {
    matches!(p, Pattern::Some(_) | Pattern::Ok(_))
}

/// The name a pattern binds its payload to, if any.
fn pattern_binding(p: &Pattern) -> Option<&str> {
    match p {
        Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => Some(b),
        // Variants route through gen_match_enum, not this Option/Result helper.
        Pattern::Variant(_, b) => b.first().map(|s| s.as_str()),
        Pattern::None => None,
    }
}

/// LLVM byte-string escaping: printable ASCII as-is, everything else `\NN`,
/// plus a trailing NUL. Returns (escaped, total byte length).
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
fn utf8d_table() -> Vec<u8> {
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

/// Collect distinct `=~` pattern literals (first-seen order) from a block.
fn collect_regex_block(b: &Block, out: &mut Vec<String>) {
    for s in &b.stmts {
        collect_regex_stmt(s, out);
    }
}

fn collect_regex_stmt(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. } => collect_regex_expr(value, out),
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_regex_expr(e, out);
            }
        }
        Stmt::If { cond, then_block, else_block, .. } => {
            collect_regex_expr(cond, out);
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
        Stmt::Drop { .. } => {}
        Stmt::Expr(e) => collect_regex_expr(e, out),
        Stmt::Region { body, .. } => collect_regex_block(body, out),
    }
}

fn collect_regex_expr(e: &Expr, out: &mut Vec<String>) {
    match e {
        // A `s =~ "pat"` node contributes its literal pattern.
        Expr::Binary { op: BinOp::Match, lhs, rhs, .. } => {
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
        Expr::Call { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::Spawn { args, .. } => {
            for a in args {
                collect_regex_expr(a, out);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_regex_expr(scrutinee, out);
            for a in arms {
                collect_regex_expr(&a.body, out);
            }
        }
        Expr::IfExpr { cond, then_branch, else_branch, .. } => {
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
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Var { .. } => {}
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
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. } => collect_strings_expr(value, out, types),
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_strings_expr(e, out, types);
            }
        }
        Stmt::If { cond, then_block, else_block, .. } => {
            collect_strings_expr(cond, out, types);
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
        Stmt::Drop { .. } => {}
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
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Var { .. } => {}
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            collect_strings_expr(expr, out, types)
        }
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
        Expr::Match { scrutinee, arms, .. } => {
            collect_strings_expr(scrutinee, out, types);
            for a in arms {
                collect_strings_expr(&a.body, out, types);
            }
        }
        Expr::IfExpr { cond, then_branch, else_branch, .. } => {
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

/// Bind type parameters by matching a (possibly generic) parameter type against
/// a concrete argument type. Mirrors the checker's `unify`, minus error checks
/// (the checker already validated the call).
fn solve_param(pty: &Type, aty: &Type, subst: &mut HashMap<String, Type>) {
    match (pty, aty) {
        (Type::Param(t), _) => {
            subst.entry(t.clone()).or_insert_with(|| aty.clone());
        }
        (Type::Option(p), Type::Option(a)) => solve_param(p, a, subst),
        (Type::Result(p1, p2), Type::Result(a1, a2)) => {
            solve_param(p1, a1, subst);
            solve_param(p2, a2, subst);
        }
        (Type::App(pn, pa), Type::App(an, aa)) if pn == an && pa.len() == aa.len() => {
            for (p, a) in pa.iter().zip(aa) {
                solve_param(p, a, subst);
            }
        }
        // Generic collection/reference element inference (RFC-0023): bind the
        // element type parameter from the concrete argument.
        (Type::Array(p), Type::Array(a)) => solve_param(p, a, subst),
        (Type::ArrayN(p, _), Type::ArrayN(a, _)) => solve_param(p, a, subst),
        (Type::Map(pk, pv), Type::Map(ak, av)) => {
            solve_param(pk, ak, subst);
            solve_param(pv, av, subst);
        }
        (Type::Ref(p), Type::Ref(a)) => solve_param(p, a, subst),
        _ => {}
    }
}

/// The mangled LLVM symbol for a generic instantiation, e.g. `vyrn_id__Int`.
fn mangle_name(name: &str, type_args: &[Type]) -> String {
    let parts: Vec<String> = type_args.iter().map(mangle_ty).collect();
    format!("vyrn_{name}__{}", parts.join("_"))
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
            format!("{}{}", sanitize(n), args.iter().map(mangle_ty).collect::<String>())
        }
        Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..) => "Xf".into(),
        Type::Param(p) => sanitize(p),
        Type::Ref(inner) => format!("Ref{}", mangle_ty(inner)),
        Type::Array(inner) => format!("Arr{}", mangle_ty(inner)),
        Type::ArrayN(inner, n) => format!("Arr{n}{}", mangle_ty(inner)),
        Type::Map(k, v) => format!("Map{}{}", mangle_ty(k), mangle_ty(v)),
        Type::Task(inner) => format!("Task{}", mangle_ty(inner)),
        Type::Logger => "Logger".into(),
        // A function-value type (RFC-0023) mangles by shape — used only when a
        // generic instance's own type argument mentions one (rare); the
        // higher-order specialization keys are formed separately.
        Type::Fn(ps, r) => format!(
            "Fn{}R{}",
            ps.iter().map(mangle_ty).collect::<String>(),
            mangle_ty(r)
        ),
        // Checker recovery sentinel; never reaches codegen in a valid program.
        Type::Err => "Err".into(),
    }
}

/// Every string constant the generated JSON codec functions (RFC-0018) will
/// reference: field keys, enum variant names, `expected <what>` phrases,
/// `json.missing`/`validate` messages, and the fixed Issue keys. Seeded into
/// the module string pool before the functions are emitted so `str_g` resolves.
fn collect_codec_strings(program: &Program, types: &HashMap<String, TypeDecl>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in ["", "json.parse", "json.type", "json.missing", "validate"] {
        out.push(k.to_string());
    }
    let mut seen: Vec<String> = Vec::new();
    for t in &program.type_decls {
        gather_codec_strings(&Type::Named(t.name.clone()), types, &mut out, &mut seen);
    }
    // Also cover types that appear only in a function signature — a bare
    // `Result<T, E>` return (RFC-0024) is `toJson`'d without ever being a named
    // type_decl, so its `Ok`/`Err`/payload strings must still be pooled.
    for f in &program.functions {
        for p in &f.params {
            gather_codec_strings(&p.ty, types, &mut out, &mut seen);
        }
        gather_codec_strings(&f.ret, types, &mut out, &mut seen);
    }
    out.sort();
    out.dedup();
    out
}

fn push_uniq(out: &mut Vec<String>, s: String) {
    if !out.contains(&s) {
        out.push(s);
    }
}

/// Walk a type collecting the codec's constant strings. `seen` breaks cycles
/// (a record reachable through `Array<Self>`).
fn gather_codec_strings(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    out: &mut Vec<String>,
    seen: &mut Vec<String>,
) {
    push_uniq(out, vyrn_frontend::codec::expected_name(ty, types));
    if let Type::Named(n) = ty {
        if seen.contains(n) {
            return;
        }
        if let Some(d) = types.get(n) {
            seen.push(n.clone());
            if d.predicate.is_some() {
                push_uniq(out, vyrn_frontend::codec::validate_message(d));
            }
            gather_codec_strings(&d.base, types, out, seen);
        }
        return;
    }
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Record(fields) => {
            for f in &fields {
                push_uniq(out, f.name.clone());
                push_uniq(out, vyrn_frontend::codec::missing_message(&f.name));
                gather_codec_strings(&f.ty, types, out, seen);
            }
        }
        Type::Enum(vs) => {
            for v in &vs {
                push_uniq(out, v.name.clone());
                // A tuple payload decodes from a JSON array — its mismatch Issue
                // reads `expected array, found <kind>` (RFC-0024).
                if v.payload.len() >= 2 {
                    push_uniq(out, "array".to_string());
                }
                for p in &v.payload {
                    gather_codec_strings(p, types, out, seen);
                }
            }
        }
        // `Result<T, E>` on the wire is a two-variant payload enum (RFC-0024).
        Type::Result(t, e) => {
            push_uniq(out, "Ok".to_string());
            push_uniq(out, "Err".to_string());
            gather_codec_strings(&t, types, out, seen);
            gather_codec_strings(&e, types, out, seen);
        }
        Type::Option(inner) | Type::Array(inner) | Type::ArrayN(inner, _) => {
            gather_codec_strings(&inner, types, out, seen);
        }
        _ => {}
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
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::check;

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

    #[test]
    fn payload_enum_gets_per_type_codec_functions() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Nothing \
                   fn f(s: Shape) -> String { return toJson(s) } \
                   fn g(s: String) -> Validation<Shape> { return fromJson(Shape, s) } \
                   fn main() -> Int64 { return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // A payload enum earns standalone encode/decode functions (recursion-safe)
        // and the call sites route to them.
        assert!(ir.contains("define ptr @__vyrn_enc_Shape("), "enc fn:\n{ir}");
        assert!(ir.contains("@__vyrn_dec_Shape("), "dec fn:\n{ir}");
        // The tuple payload reads a JSON array element.
        assert!(ir.contains("@__vyrn_vj_at_or_null"), "tuple element access:\n{ir}");
    }

    #[test]
    fn pure_nullary_enum_keeps_inline_string_encoding() {
        // Regression pin: a payload-LESS enum must NOT get a codec function — it
        // reads its variant name from the O(1) table (byte-identical to RFC-0018).
        let src = "type Role = | Guest | Admin \
                   fn f(r: Role) -> String { return toJson(r) } \
                   fn main() -> Int64 { return 0 }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(!ir.contains("@__vyrn_enc_Role("), "no codec fn for a nullary enum:\n{ir}");
        assert!(ir.contains("@.enumnames.Role"), "name table present:\n{ir}");
    }

    // ---- function values (RFC-0023) -------------------------------------

    const HO: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n\
         fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
         fn main() -> Int64 {\n\
             let a = twice([1, 2, 3], |x| x * 2)\n\
             let off = 10\n\
             let b = twice([1, 2, 3], |x| x + off)\n\
             let c = twice([1, 2, 3], dbl)\n\
             return 0 }";

    #[test]
    fn lambdas_monomorphize_with_no_indirect_calls() {
        let ir = emit(&check(HO).unwrap()).unwrap();
        // Each lambda literal is lifted to its own top-level function...
        assert!(ir.contains("@__vyrn_lambda_main_"), "lifted lambda missing:\n{ir}");
        // ...and `twice` is specialized per target (three distinct instances).
        assert!(ir.matches("@vyrn_twice__ho").count() >= 3, "specializations missing:\n{ir}");
        // The unspecialized `twice` shell is NEVER emitted (it has a `fn` param).
        assert!(!ir.contains("define { ptr, i64, i64 } @vyrn_twice("), "shell emitted:\n{ir}");
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
        assert!(
            ir.contains("@__vyrn_lambda_main_1_Int64Int64RInt64(i64 %arg0, i64 %arg1)"),
            "captured lambda should take (capture, param):\n{ir}"
        );
    }

    // ---- stored function values: stage gate (RFC-0037) --------------------

    #[test]
    fn stored_fn_values_fail_loudly_until_lowered() {
        // The checker accepts storage (RFC-0037 stage 1); this backend must
        // refuse to compile it with the named stage diagnostic — never
        // miscompile. Each storage form hits the gate.
        let cases: &[&str] = &[
            // `let` annotation.
            "fn main() -> Int64 { let g: fn(Int64) -> Int64 = |x| x * 2  return g(3) }",
            // fn-typed return.
            "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn pick() -> fn(Int64) -> Int64 { return dbl }\n\
             fn main() -> Int64 { let f = pick()  return f(21) }",
            // Record field via type decl.
            "type R = { f: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 { let r = R { f: |x| x + 1 }  let g = r.f  return g(1) }",
            // Module state.
            "type M = fn(Int64) -> Int64\nlet mut chain: Array<M> = []\n\
             fn main() -> Int64 { chain.push(|x| x)  let m = chain[0]  return m(1) }",
            // Bare named-fn composition with no annotation anywhere.
            "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let g = dbl  return g(4) }",
        ];
        for src in cases {
            let e = emit(&check(src).unwrap()).unwrap_err();
            assert!(
                e.contains("not yet supported by the native/wasm backends"),
                "expected the RFC-0037 stage gate, got: {e}\nfor:\n{src}"
            );
        }
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
            ir.matches("define void @__vyrn_task_vyrn_fib(ptr %frame)").count(),
            1,
            "expected exactly one shared thunk:\n{ir}"
        );
        assert!(ir.contains("%r = call i64 @vyrn_fib(i64 %a0)"), "{ir}");
        assert!(ir.contains("store i64 %r, ptr %frame"), "{ir}");
        // join blocks through the shim and loads the result from the frame.
        assert!(ir.contains("call ptr @__vyrn_join(ptr"), "join missing:\n{ir}");
    }

    #[test]
    fn region_arena_is_thread_local() {
        // Isolated tasks may use `region { .. }`; with tasks on real threads
        // the arena stack must be per-thread (single-threaded targets lower
        // TLS to plain globals, so the shared IR is unaffected there).
        let ir = emit(&check(SPAWNY).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_region_sp = thread_local global i64 0"), "{ir}");
        assert!(
            ir.contains("@__vyrn_region_heads = thread_local global [64 x ptr] zeroinitializer"),
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
                assert!(t.contains("@"), "indirect (function-pointer) call emitted:\n  {line}");
            }
        }
    }

    // ---- input I/O (RFC-0014) -------------------------------------------

    #[test]
    fn read_file_lowers_to_shim_call_with_canonical_messages() {
        let src = "fn main() -> Int64 { \
                       let r = readFile(\"cfg.txt\") \
                       return match r { Ok(s) => s.length, Err(e) => e.length } }";
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
                       return match w { Ok(b) => 0, Err(e) => e.length } }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i32 @__vyrn_write_file(ptr"), "{ir}");
        assert!(ir.contains("c\"cannot write `%s`\\00\""), "{ir}");
    }

    #[test]
    fn args_and_read_line_lower_to_runtime_calls() {
        let src = "fn main() -> Int64 { \
                       let a = args() \
                       let l = readLine() \
                       let n = match l { Some(s) => s.length, None => 0 } \
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
                       return match r { Ok(s) => s.length, Err(e) => e.length } }";
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
        let program =
            check("fn main() -> Int64 { let mut d = 3; return 10 / d; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("@.trap.div0"), "zero guard: {ir}");
        assert!(ir.contains("@.trap.divovf"), "MIN/-1 guard: {ir}");
        assert!(ir.contains("icmp eq i64"), "guard compare: {ir}");
        assert!(ir.contains("@fputs"), "stderr write: {ir}");
    }

    #[test]
    fn float_print_selects_nan_literal() {
        // NaN prints as `NaN` (interp's Rust formatting), not UCRT's -nan(ind):
        // the format string is selected on `fcmp uno`.
        let program = check("fn main() -> Int64 { print(0.0 / 0.0); return 0; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("fcmp uno double"), "NaN test: {ir}");
        assert!(ir.contains("@.fmt.nan"), "NaN format: {ir}");
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
        assert!(ir.contains("c\"root\\00\""), "predicate literal missing from pool:\n{ir}");
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
        assert!(ir.contains("load ptr, ptr @__vyrn_log_file"), "logs to the file handle: {ir}");
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
        assert!(ir.contains("@.fmt.log, ptr @.lvl.warn"), "warn should emit: {ir}");
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
        assert!(ir.contains("insertvalue { ptr, i64, i64 }"), "builds arrays: {ir}");
    }

    #[test]
    fn string_interpolation_lowers_to_str_and_concat() {
        // `"n=\{n}"` desugars to `concat("n=", str(n))`; `str(Bool)` selects the
        // no-newline global and copies it into a fresh buffer.
        let src = "fn main() -> Int64 { let n = 7; let ok = true; \
                   let s = \"n=\\{n} ok=\\{ok}\"; return s.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_snprintf"), "str(Int64) -> snprintf: {ir}");
        assert!(ir.contains("select i1"), "str(Bool) -> select true/false: {ir}");
        assert!(ir.contains("@strcpy"), "bool/str render copies: {ir}");
        assert!(ir.contains("@.str.true"), "no-newline bool global: {ir}");
    }

    #[test]
    fn string_plus_lowers_to_concat_runtime() {
        // `a + b` on Strings emits the same strlen/strcpy/strcat sequence `concat`
        // used, and `x.toString()` renders via snprintf.
        let src = "fn main() -> Int64 { let a = \"x\"; let n = (5).toString() + a; return n.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vyrn_strlen"), "concat length: {ir}");
        assert!(ir.contains("@strcpy") && ir.contains("@strcat"), "concat copy: {ir}");
        assert!(ir.contains("@__vyrn_snprintf"), "toString(Int) -> snprintf: {ir}");
    }

    #[test]
    fn contextual_array_literal_lowers_to_heap_triple() {
        // A literal in an `Array<T>` slot is malloc'd into the `{ptr,len,cap}`
        // triple (like `list([..])`), then `.length` reads field 1.
        let src = "fn main() -> Int64 { let a: Array<Int64> = [1, 2, 3]; return a.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vyrn_malloc"), "heap copy: {ir}");
        assert!(ir.contains("insertvalue { ptr, i64, i64 }"), "growable triple: {ir}");
    }

    #[test]
    fn numeric_conversions_lower_to_casts() {
        let src = "fn main() -> Int64 { let f = 3.5; let n = Int64(f); \
                   let g = Float64(n); let s = Int32(5000000000); \
                   print(g); return Int64(s); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("fptosi double"), "float→int: {ir}");
        assert!(ir.contains("sitofp i64"), "int→float: {ir}");
        assert!(ir.contains("trunc i64") && ir.contains("to i32"), "int→i32: {ir}");
        assert!(ir.contains("sext i32 ") && ir.contains("to i64"), "i32→int: {ir}");
    }

    #[test]
    fn sized_ints_lower_to_width_ops() {
        let src = "fn main() -> Int64 { let a: Int32 = 5; let b: Int32 = 3; \
                   let c = a + b; print(c); if c > 0 { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("add i32"), "i32 add: {ir}");
        assert!(ir.contains("icmp sgt i32"), "i32 compare: {ir}");
        // Literals coerce into i32 slots via trunc.
        assert!(ir.contains("trunc i64") && ir.contains("to i32"), "literal→i32: {ir}");
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
        assert!(ir.contains("zext i32") && ir.contains("@.fmt.u"), "unsigned print: {ir}");
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
                   let c = a + b; print(c); if c > 0.0 { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("fadd float"), "f32 add: {ir}");
        assert!(ir.contains("fcmp ogt float"), "f32 compare: {ir}");
        // Literals round into f32 slots via fptrunc, and print promotes back.
        assert!(ir.contains("fptrunc double") && ir.contains("to float"), "literal→f32: {ir}");
        assert!(ir.contains("fpext float") && ir.contains("to double"), "print fpext: {ir}");
    }

    #[test]
    fn float32_conversions_use_fptrunc_and_fpext() {
        let widen = "fn main() -> Int64 { let x: Float32 = 1.5; let d = Float64(x); \
                     if d > 0.0 { return 1; } return 0; }";
        assert!(emit(&check(widen).unwrap()).unwrap().contains("fpext float"), "f32→f64");
        let narrow = "fn main() -> Int64 { let d = 1.5; let x = Float32(d); \
                      if x > 0.0 { return 1; } return 0; }";
        assert!(emit(&check(narrow).unwrap()).unwrap().contains("fptrunc double"), "f64→f32");
    }

    #[test]
    fn exit_code_is_masked_to_low_byte() {
        // `@main` masks vyrn_main's return so it matches the interpreter's
        // `code & 0xff` on values > 255 (POSIX exit convention).
        let ir = emit(&check("fn main() -> Int64 { return 285; }").unwrap()).unwrap();
        assert!(ir.contains("and i64 %r, 255"), "{ir}");
    }

    #[test]
    fn drop_of_array_emits_afree() {
        // `drop a` on a growable array frees its backing buffer (an extra free
        // beyond the region-runtime baseline).
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(1); \
                   drop a; return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(free_calls(&ir) >= 1, "expected an afree from `drop`: {ir}");
    }

    #[test]
    fn length_and_index_surface_lower_to_alen_and_at() {
        // `a.length` -> extractvalue field 1; `a[i]` -> bounds-checked `at`.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(5); \
                   return a.length + a[0]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("extractvalue { ptr, i64, i64 }"), "length -> extractvalue: {ir}");
        assert!(ir.contains("icmp uge i64"), "index -> bounds check: {ir}");
    }

    #[test]
    fn for_loop_lowers_to_indexed_walk() {
        // A `for` over a growable array reads the length once and walks it with a
        // bounds-comparison branch, accumulating into the total.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 3); a = push(a, 4); \
                   let mut s = 0; for x in a { s = s + x; } return s; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("fcond"), "expected a for-loop condition block: {ir}");
        assert!(ir.contains("fbody"), "expected a for-loop body block: {ir}");
        assert!(ir.contains("icmp uge i64"), "expected the length bound check: {ir}");
        // The element type is Int, so the per-iteration element load is `i64`.
        assert!(ir.contains("load i64"), "expected an element load: {ir}");
    }

    // The always-present runtime contributes exactly RUNTIME_FREES `call void
    // @free` occurrences (one in `__vyrn_region_exit`, one in the
    // `__vyrn_bytes_dup` NUL path), so an *auto*-free is a free beyond that
    // baseline.
    const RUNTIME_FREES: usize = 2;
    fn free_calls(ir: &str) -> usize {
        ir.matches("call void @free(ptr").count()
    }

    #[test]
    fn non_escaping_temporary_is_freed() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let n = s.length; return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            free_calls(&ir) > RUNTIME_FREES,
            "expected an auto-free beyond the runtime: {ir}"
        );
    }

    #[test]
    fn escaping_temporary_is_not_freed() {
        // `s` is aliased into `t`, so it must not be auto-freed (would dangle).
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let t = s; return t.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(
            free_calls(&ir),
            RUNTIME_FREES,
            "only the runtime frees should be present: {ir}"
        );
    }

    #[test]
    fn generational_reference_lowers_to_slab_calls() {
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); \
                   let v = get(c); release(c); return v; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @__vyrn_cell_alloc"), "{ir}");
        assert!(ir.contains("call ptr @__vyrn_cell_ptr"), "{ir}");
        assert!(ir.contains("call void @__vyrn_cell_release_slot"), "{ir}");
        // The generation check is what makes a stale reference safe.
        assert!(ir.contains("call void @__vyrn_cell_check"), "{ir}");
    }

    #[test]
    fn non_escaping_cell_is_auto_released() {
        // No explicit `release` in the source, yet the non-escaping cell must be
        // released at block exit (inferred by the ownership analysis).
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); return get(c); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @__vyrn_cell_release"), "expected auto-release: {ir}");
    }

    #[test]
    fn caller_frees_owned_transfer_result() {
        // `make` returns a fresh owned String; `main` must free the result it
        // receives, but `make` must NOT free what it moves out.
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let g = make(a, b); return g.length; }";
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
                       region { let s = a + b; n = s.length; } \
                       return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @__vyrn_region_enter()"), "{ir}");
        assert!(ir.contains("call void @__vyrn_region_exit()"), "{ir}");
        // concat routes through the arena at runtime.
        assert!(ir.contains("@__vyrn_region_alloc"), "{ir}");
        assert!(ir.contains("load i64, ptr @__vyrn_region_sp"), "{ir}");
    }

    // The runtime preamble contains a fixed number of `call void @exit` (the
    // cell slab's trap paths); a validation check is one *beyond* that baseline.
    fn exit_calls(ir: &str) -> usize {
        ir.matches("call void @exit").count()
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

    #[test]
    fn runtime_construction_emits_check() {
        // A non-constant construction (through a parameter) is checked at runtime.
        let src = "type Age = Int64 where value >= 18; \
                   fn mk(n: Int64) -> Age { return Age(n); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(exit_calls(&ir) > exit_baseline(), "expected a runtime check: {ir}");
        assert!(ir.contains("@.trap.verr.Age"), "{ir}");
    }

    #[test]
    fn string_length_lowers_to_strlen() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @__vyrn_strlen"), "str .length → strlen: {ir}");
    }

    #[test]
    fn string_index_lowers_to_byte_load() {
        // `s[i]` is a `UInt8` (RFC-0022): the byte loads as `i8` and stays
        // `i8` (no zero-extension) — an explicit `Int64(..)` is what widens it.
        let src = "fn main() -> Int64 { let s = \"hi\"; return Int64(s[0]); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("load i8"), "loads a byte: {ir}");
        assert!(ir.contains("zext i8") && ir.contains("to i64"), "Int64(..) widens: {ir}");
        assert!(ir.contains("@.trap.soob"), "bounds-checked: {ir}");
    }

    #[test]
    fn encodings_lower_to_runtime() {
        let ir = emit(&check("fn main() -> Int64 { \
            let a = hexEncode(\"x\"); let b = base64Encode(\"x\"); let c = urlEncode(\"x\"); \
            let d = hexDecode(\"41\"); return 0; }").unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vyrn_hex_encode"), "hexEncode: {ir}");
        assert!(ir.contains("call ptr @__vyrn_b64_encode"), "base64Encode: {ir}");
        assert!(ir.contains("call ptr @__vyrn_url_encode"), "urlEncode: {ir}");
        assert!(ir.contains("call { i1, i64, i64 } @__vyrn_hex_decode"), "hexDecode: {ir}");
        // The strict UTF-8 validator DFA + its 364-byte table are present.
        assert!(ir.contains("@__vyrn_utf8valid"), "validator: {ir}");
        assert!(ir.contains("@__vyrn_utf8d = private"), "DFA table: {ir}");
    }

    #[test]
    fn chars_and_bytes_lower_to_runtime() {
        let ir = emit(&check("fn main() -> Int64 { return chars(\"hi\").length + bytes(\"hi\").length; }").unwrap()).unwrap();
        assert!(ir.contains("call { ptr, i64, i64 } @__vyrn_str_chars"), "chars → decoder: {ir}");
        assert!(ir.contains("call { ptr, i64, i64 } @__vyrn_str_bytes"), "bytes → helper: {ir}");
        // The UTF-8 decoder is defined in the module.
        assert!(ir.contains("@__vyrn_str_chars(ptr %s)"), "decoder emitted: {ir}");
    }

    #[test]
    fn string_methods_lower_to_libc() {
        let c = emit(&check("fn f(s: String) -> Bool { return contains(s, \"x\"); } \
                             fn main() -> Int64 { return 0; }").unwrap()).unwrap();
        assert!(c.contains("call ptr @strstr"), "contains → strstr: {c}");
        let s = emit(&check("fn f(s: String) -> Bool { return startsWith(s, \"x\"); } \
                             fn main() -> Int64 { return 0; }").unwrap()).unwrap();
        assert!(s.contains("call i32 @__vyrn_strncmp"), "startsWith → strncmp: {s}");
    }

    #[test]
    fn validated_string_runtime_check_uses_strlen() {
        // A non-constant String construction checks `value.length` via strlen and
        // traps through the same validation-error path.
        let src = "type Name = String where value.length >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @__vyrn_strlen"), "refinement uses strlen: {ir}");
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
        assert!(ir.contains("call i1 @__vyrn_regex_run"), "calls the runner: {ir}");
        assert!(ir.contains("@.rx.0.table"), "emits a transition table: {ir}");
        assert!(ir.contains("@.rx.0.accept"), "emits an accepting array: {ir}");
    }

    #[test]
    fn option_match_lowers_to_aggregate_and_phi() {
        let src = "fn f() -> Option<Int64> { return Some(7); } \
                   fn main() -> Int64 { return match f() { Some(x) => x, None => 0 }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("insertvalue { i1, i64, i64 }"), "Some should build an aggregate: {ir}");
        assert!(ir.contains("extractvalue { i1, i64, i64 }"), "match should extract: {ir}");
        assert!(ir.contains("phi i64"), "match should merge with a phi: {ir}");
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
            "match must unbox the payload as the growable triple:\n{ir}"
        );
    }

    #[test]
    fn result_array_payload_rematerializes_on_coerce() {
        // RFC-0026 regression for the built-in sum types: `Ok([..])` boxes the
        // array as a fixed `[N x T]`, but the declared return type wants the
        // growable `Array<T>`. Coercing the `Result` into the return type must
        // re-materialize the boxed payload in the target representation (a
        // tag-branch rebuild), or `match` decodes it at the wrong width.
        let src = "fn load() -> Result<Array<Int64>, String> { return Ok([1, 2, 3]); } \
                   fn main() -> Int64 { return match load() { Ok(xs) => xs.length, Err(e) => 0 }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("rebox.one") && ir.contains("rebox.zero"),
            "coercing Result<ArrayN,_> into Result<Array,_> must rebuild the \
             payload arm-by-arm:\n{ir}"
        );
        assert!(
            ir.contains("phi { i1, i64, i64 }"),
            "the re-materialized aggregate merges through a phi:\n{ir}"
        );
    }

    #[test]
    fn generic_record_monomorphizes_by_layout() {
        let src = "type Box<T> = { value: T }; \
                   fn main() -> Int64 { let a = Box { value: 5 }; let b = Box { value: true }; \
                                      if b.value { return a.value; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("insertvalue { i64 }"), "Box<Int64> is a 1x i64 struct:\n{ir}");
        assert!(ir.contains("insertvalue { i1 }"), "Box<Bool> is a 1x i1 struct:\n{ir}");
    }

    #[test]
    fn generic_monomorphizes_per_type() {
        let src = "fn id<T>(x: T) -> T { return x; } \
                   fn main() -> Int64 { print(id(\"s\")); return id(1); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("define i64 @vyrn_id__Int"), "Int64 instance:\n{ir}");
        assert!(ir.contains("define ptr @vyrn_id__Str"), "Str instance:\n{ir}");
        assert!(!ir.contains("@vyrn_id("), "no un-instantiated generic body:\n{ir}");
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
        assert!(ir.contains("call i32 @strcmp"), "ordering uses strcmp:\n{ir}");
        assert!(ir.contains("icmp slt i32"), "signed sign-test against 0:\n{ir}");
    }

    #[test]
    fn enum_match_lowers_to_switch() {
        let src = "type E = | A(Int64) | B(Int64) | C; \
                   fn f(e: E) -> Int64 { return match e { A(x) => x, B(y) => y, C => 0 }; } \
                   fn main() -> Int64 { return f(A(5)); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("switch i64"), "enum match uses a switch:\n{ir}");
        assert!(ir.contains("@vyrn_f({ i64, i64 }"), "enum lowers to a 2-word aggregate:\n{ir}");
        assert!(ir.contains("insertvalue { i64, i64 } undef, i64 0"), "variant A has tag 0:\n{ir}");
    }

    #[test]
    fn omit_transformer_lowers_to_narrower_struct() {
        let src = "type User = { id: Int64, name: Int64, pw: Int64 }; type Public = Omit<User, pw>; \
                   fn f(p: Public) -> Int64 { return p.name; } \
                   fn main() -> Int64 { let u = User { id: 1, name: 2, pw: 3 }; return f(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Public resolves to a 2-field struct; User is 3 fields; coercion happens.
        assert!(ir.contains("@vyrn_f({ i64, i64 }"), "Public layout: {ir}");
        assert!(ir.contains("insertvalue { i64, i64, i64 }"), "User is 3 fields: {ir}");
    }

    #[test]
    fn record_width_subtyping_coerces() {
        let src = "type Named = { name: Int64 }; type User = { name: Int64, age: Int64 }; \
                   fn greet(w: Named) -> Int64 { return w.name; } \
                   fn main() -> Int64 { let u = User { name: 7, age: 30 }; return greet(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // greet takes a 1-field record; User is a 2-field record.
        assert!(ir.contains("@vyrn_greet({ i64 }"), "greet param layout: {ir}");
        assert!(ir.contains("insertvalue { i64, i64 }"), "User is built: {ir}");
        // width-subtyping coercion: rebuild a { i64 } from the User's `name`.
        assert!(ir.contains("insertvalue { i64 } undef"), "coercion to Named: {ir}");
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
        assert!(ir.contains("try.prop"), "? should have a propagate block: {ir}");
        assert!(ir.contains("ret { i1, i64, i64 }"), "? should propagate the aggregate: {ir}");
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
                       let n = s.length; \
                       return Ok(x + n); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        let prop = ir.find("try.prop").expect("propagate block present");
        let ret = prop + ir[prop..].find("ret { i1, i64, i64 }").expect("propagate returns");
        assert!(
            ir[prop..ret].contains("call void @free(ptr"),
            "owned string must be freed on the propagate path:\n{}",
            &ir[prop..ret]
        );
    }

    #[test]
    fn region_enter_traps_past_depth_64() {
        // The arena stack is a fixed [64 x ptr]; entering a 65th nested region
        // must trap (stderr + exit 1), not write past the global.
        let src = "fn main() -> Int64 { region { } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("error: region nesting exceeds 64"),
            "region-depth trap message present: {ir}"
        );
        assert!(
            ir.contains("%over = icmp sge i64 %sp, 64"),
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
            ir.contains("\"wasm-import-module\"=\"vyrn\"") &&
            ir.contains("\"wasm-import-name\"=\"jsLog\""),
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
                   export extern fn greet(name: String) -> String { return name } \
                   fn main() -> Int64 { return vyrnAdd(1, 2) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("define i64 @vyrn_vyrnAdd(i64 %arg0, i64 %arg1) \"wasm-export-name\"=\"vyrnAdd\" {"),
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
            ir.contains("define i64 @vyrn_main(") && !ir.contains("@vyrn_main() \"wasm-export-name\""),
            "a plain fn is not exported: {ir}"
        );
    }

    #[test]
    fn mut_array_is_auto_freed() {
        // No explicit `afree`, yet the non-escaping mutable array is freed at
        // scope end (inferred by the ownership analysis).
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); return at(a, 0); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @free(ptr"), "expected auto-afree: {ir}");
    }

    #[test]
    fn afree_frees_the_array_buffer() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); afree(a); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The buffer pointer (aggregate field 0) is freed.
        assert!(ir.contains("extractvalue { ptr, i64, i64 }"), "{ir}");
        assert!(ir.contains("call void @free(ptr"), "afree should free the buffer: {ir}");
    }

    #[test]
    fn option_holds_a_ref_inline() {
        // A `Ref` (two words) fits inline in the widened Option aggregate — no box.
        let src = "fn main() -> Int64 { let r = cell(7); let o = Some(r); \
                   return match o { Some(x) => get(x), None => 0 }; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // The Option is the three-word aggregate, and the Ref is rebuilt inline
        // on match (insertvalue into { i64, i64 }) rather than loaded from a box.
        assert!(ir.contains("insertvalue { i1, i64, i64 }"), "widened aggregate: {ir}");
        assert!(ir.contains("insertvalue { i64, i64 }"), "Ref rebuilt inline: {ir}");
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
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[1] = 9; return a[1]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.trap.aoob"), "reuses the array OOB trap: {ir}");
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
        assert!(ir.contains("@.trap.verr.Age"), "element store validates: {ir}");
    }

    #[test]
    fn pop_emits_none_some_branches_and_writeback() {
        // `pop` len-checks, builds a None/Some aggregate via a phi, and writes
        // the decremented header back to the array slot.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let p = match a.pop() { Some(x) => x, None => -1 }; return p; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("phi { i1, i64, i64 }"), "None/Some merge: {ir}");
        assert!(ir.contains("insertvalue { ptr, i64, i64 }"), "header write-back: {ir}");
        assert!(ir.contains("sub i64"), "length decrement: {ir}");
    }

    #[test]
    fn swapremove_emits_bounds_check_and_swap() {
        // `swapRemove` bounds-checks, loads element i (the result), moves the last
        // element into slot i, and writes the shrunk header back.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g = a.swapRemove(0); return g; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.trap.aoob"), "reuses the array OOB trap: {ir}");
        assert!(ir.contains("insertvalue { ptr, i64, i64 }"), "header write-back: {ir}");
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
        assert!(ir.contains("@g.hits = internal global i64 zeroinitializer"), "{ir}");
        assert!(ir.contains("@g.banner = internal global ptr zeroinitializer"), "{ir}");
        // A synthesized init function, called from `vyrn_entry` before main.
        assert!(ir.contains("define internal void @__vyrn_globals_init()"), "{ir}");
        let init_at = ir.find("call void @__vyrn_globals_init()").expect("init call");
        let main_at = ir.find("call i64 @vyrn_main()").expect("main call");
        assert!(init_at < main_at, "init must run before main");
        // Reads and writes go through the global.
        assert!(ir.contains("load i64, ptr @g.hits"), "read through global: {ir}");
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
        assert!(ir.contains("@.trap.verr.Age"), "per-type validation trap: {ir}");
        assert!(ir.contains("store i64 %") && ir.contains("@g.a"), "store through global: {ir}");
    }

    // ---- RFC-0020 M1: interpolation containment erases the runtime check ----

    #[test]
    fn proven_interpolation_emits_no_validation() {
        // `"nav.\{s}.label"` with s: Section is provably a TransKey, so the
        // containment proof erases the runtime validation entirely — no per-type
        // trap for TransKey is emitted at the `t(..)` argument boundary.
        let src = "type TransKey = String where value =~ \"nav\\\\.(home|about|settings)\\\\.label\" \
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
        let src = "type TransKey = String where value =~ \"nav\\\\.(home|about|settings)\\\\.label\" \
                   fn t(key: TransKey) -> Int64 { return 0 } \
                   fn build(x: String) -> Int64 { return t(\"nav.\\{x}.label\") } \
                   fn main() -> Int64 { return build(\"home\") }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("@.trap.verr.TransKey, ptr"),
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
        assert!(!ir.contains("@.trap.verr.Wide, ptr"), "contained finite var needs no check: {ir}");
    }
}
