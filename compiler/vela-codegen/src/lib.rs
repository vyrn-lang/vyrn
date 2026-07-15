//! Textual LLVM IR backend for the Vela v0 subset.
//!
//! This emits LLVM IR as a string — no LLVM libraries required to *produce* it.
//! Feed the output to a `clang`/`llc` (LLVM 15+, opaque pointers) to get a
//! native object/executable:
//!
//! ```text
//! velac emit-ir prog.vela > prog.ll
//! clang prog.ll -o prog
//! ```
//!
//! Local variables use `alloca`/`load`/`store` (LLVM's `mem2reg` promotes them
//! to SSA registers), which keeps the emitter simple. `&&`/`||` short-circuit
//! via branches + `phi`, matching the interpreter in [`vela_frontend::interp`].
//!
//! The Inkwell (in-memory LLVM) backend in the excluded `vela-codegen-llvm`
//! crate will eventually replace this; both must agree with the interpreter.

use std::collections::HashMap;
use std::fmt::Write;

use vela_frontend::ast::*;
use vela_frontend::own::DropKind;

/// LLVM IR for the region/arena runtime (see the preamble comment in `emit`).
const REGION_RUNTIME: &str = "\
@__vela_region_sp = global i64 0
@__vela_region_heads = global [64 x ptr] zeroinitializer
@.trap.regiondepth = private unnamed_addr constant [34 x i8] c\"error: region nesting exceeds 64\\0A\\00\"

define void @__vela_region_enter() {
entry:
  %sp = load i64, ptr @__vela_region_sp
  %over = icmp sge i64 %sp, 64
  br i1 %over, label %trap, label %ok
trap:
  %e = call ptr @__vela_stderr()
  %w = call i32 @fputs(ptr @.trap.regiondepth, ptr %e)
  call void @exit(i32 1)
  unreachable
ok:
  %slot = getelementptr [64 x ptr], ptr @__vela_region_heads, i64 0, i64 %sp
  store ptr null, ptr %slot
  %sp1 = add i64 %sp, 1
  store i64 %sp1, ptr @__vela_region_sp
  ret void
}

define ptr @__vela_region_alloc(i64 %n) {
entry:
  %tot = add i64 %n, 8
  %raw = call ptr @__vela_malloc(i64 %tot)
  %sp = load i64, ptr @__vela_region_sp
  %idx = sub i64 %sp, 1
  %slot = getelementptr [64 x ptr], ptr @__vela_region_heads, i64 0, i64 %idx
  %prev = load ptr, ptr %slot
  store ptr %prev, ptr %raw
  store ptr %raw, ptr %slot
  %user = getelementptr i8, ptr %raw, i64 8
  ret ptr %user
}

define void @__vela_region_exit() {
entry:
  %sp = load i64, ptr @__vela_region_sp
  %idx = sub i64 %sp, 1
  store i64 %idx, ptr @__vela_region_sp
  %slot = getelementptr [64 x ptr], ptr @__vela_region_heads, i64 0, i64 %idx
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
@__vela_cell_gen = global [65536 x i64] zeroinitializer
@__vela_cell_ptr_arr = global [65536 x ptr] zeroinitializer
@__vela_cell_top = global i64 0
@__vela_cell_free = global [65536 x i64] zeroinitializer
@__vela_cell_freetop = global i64 0
@.fmt.uaf = private unnamed_addr constant [37 x i8] c\"error: reference used after release\\0A\\00\"
@.fmt.oom = private unnamed_addr constant [31 x i8] c\"error: out of reference cells\\0A\\00\"

define void @__vela_cell_trap() {
entry:
  %e = call ptr @__vela_stderr()
  %r = call i32 @fputs(ptr @.fmt.uaf, ptr %e)
  call void @exit(i32 1)
  unreachable
}

define i64 @__vela_cell_alloc(ptr %p) {
entry:
  %ft = load i64, ptr @__vela_cell_freetop
  %hasfree = icmp sgt i64 %ft, 0
  br i1 %hasfree, label %reuse, label %fresh
reuse:
  %ft1 = sub i64 %ft, 1
  store i64 %ft1, ptr @__vela_cell_freetop
  %fp = getelementptr [65536 x i64], ptr @__vela_cell_free, i64 0, i64 %ft1
  %rslot = load i64, ptr %fp
  br label %done
fresh:
  %top = load i64, ptr @__vela_cell_top
  %oob = icmp sge i64 %top, 65536
  br i1 %oob, label %overflow, label %ok
overflow:
  %eo = call ptr @__vela_stderr()
  %ro = call i32 @fputs(ptr @.fmt.oom, ptr %eo)
  call void @exit(i32 1)
  unreachable
ok:
  %top1 = add i64 %top, 1
  store i64 %top1, ptr @__vela_cell_top
  br label %done
done:
  %slot = phi i64 [ %rslot, %reuse ], [ %top, %ok ]
  %pp = getelementptr [65536 x ptr], ptr @__vela_cell_ptr_arr, i64 0, i64 %slot
  store ptr %p, ptr %pp
  ret i64 %slot
}

define i64 @__vela_cell_getgen(i64 %slot) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vela_cell_gen, i64 0, i64 %slot
  %g = load i64, ptr %gp
  ret i64 %g
}

define ptr @__vela_cell_ptr(i64 %slot) {
entry:
  %pp = getelementptr [65536 x ptr], ptr @__vela_cell_ptr_arr, i64 0, i64 %slot
  %p = load ptr, ptr %pp
  ret ptr %p
}

define void @__vela_cell_check(i64 %slot, i64 %gen) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vela_cell_gen, i64 0, i64 %slot
  %cur = load i64, ptr %gp
  %ok = icmp eq i64 %cur, %gen
  br i1 %ok, label %pass, label %fail
fail:
  call void @__vela_cell_trap()
  unreachable
pass:
  ret void
}

define void @__vela_cell_release_slot(i64 %slot) {
entry:
  %gp = getelementptr [65536 x i64], ptr @__vela_cell_gen, i64 0, i64 %slot
  %g = load i64, ptr %gp
  %g1 = add i64 %g, 1
  store i64 %g1, ptr %gp
  %ft = load i64, ptr @__vela_cell_freetop
  %fp = getelementptr [65536 x i64], ptr @__vela_cell_free, i64 0, i64 %ft
  store i64 %slot, ptr %fp
  %ft1 = add i64 %ft, 1
  store i64 %ft1, ptr @__vela_cell_freetop
  ret void
}

";

/// Text-encoding runtime (hex / base64 / url) plus the shared helpers: a strict
/// UTF-8 validator (Björn Höhrmann's DFA — matches Rust's `from_utf8`) used by the
/// decoders, and hex-digit conversions. The `@__vela_utf8d` and `@__vela_b64alpha`
/// tables are emitted separately (generated in `emit`). Decoders return the
/// Option aggregate `{ i1 tag, i64 word0, i64 word1 }` (word0 = `ptrtoint` of the
/// result string on `Some`; all-zero on `None`).
const ENCODING_RUNTIME: &str = "\
define i8 @__vela_hexdigit(i8 %n) {
  %lt = icmp ult i8 %n, 10
  %d0 = add i8 %n, 48
  %da = add i8 %n, 87
  %r = select i1 %lt, i8 %d0, i8 %da
  ret i8 %r
}

define i8 @__vela_hexdigit_uc(i8 %n) {
  %lt = icmp ult i8 %n, 10
  %d0 = add i8 %n, 48
  %da = add i8 %n, 55
  %r = select i1 %lt, i8 %d0, i8 %da
  ret i8 %r
}

define i32 @__vela_hexval(i8 %c) {
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

define i1 @__vela_utf8valid(ptr %s, i64 %len) {
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
  %tp = getelementptr i8, ptr @__vela_utf8d, i64 %bz
  %ty = load i8, ptr %tp
  %tyz = zext i8 %ty to i64
  %a = add i64 256, %st
  %idx = add i64 %a, %tyz
  %sp = getelementptr i8, ptr @__vela_utf8d, i64 %idx
  %sv = load i8, ptr %sp
  %st2 = zext i8 %sv to i64
  %i2 = add i64 %i, 1
  br label %loop
fin:
  %ok = icmp eq i64 %st, 0
  ret i1 %ok
}

define ptr @__vela_hex_encode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %outlen = mul i64 %len, 2
  %sz = add i64 %outlen, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %hc = call i8 @__vela_hexdigit(i8 %hi)
  %lc = call i8 @__vela_hexdigit(i8 %lo)
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

define {i1, i64, i64} @__vela_hex_decode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %odd = and i64 %len, 1
  %isodd = icmp ne i64 %odd, 0
  br i1 %isodd, label %none, label %ok0
ok0:
  %outlen = lshr i64 %len, 1
  %sz = add i64 %outlen, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %hv = call i32 @__vela_hexval(i8 %hc)
  %lv = call i32 @__vela_hexval(i8 %lc)
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
  %v = call i1 @__vela_utf8valid(ptr %out, i64 %outlen)
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

define ptr @__vela_url_encode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %cap = mul i64 %len, 3
  %sz = add i64 %cap, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %hc = call i8 @__vela_hexdigit_uc(i8 %hi)
  %lc = call i8 @__vela_hexdigit_uc(i8 %lo)
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

define {i1, i64, i64} @__vela_url_decode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %sz = add i64 %len, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %hv = call i32 @__vela_hexval(i8 %hc)
  %lv = call i32 @__vela_hexval(i8 %lc)
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
  %v = call i1 @__vela_utf8valid(ptr %out, i64 %o)
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

define i8 @__vela_b64char(i64 %idx) {
  %p = getelementptr i8, ptr @__vela_b64alpha, i64 %idx
  %c = load i8, ptr %p
  ret i8 %c
}

define ptr @__vela_b64_encode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %p2 = add i64 %len, 2
  %grp = udiv i64 %p2, 3
  %outlen = mul i64 %grp, 4
  %sz = add i64 %outlen, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %c0 = call i8 @__vela_b64char(i64 %d0m)
  %c1 = call i8 @__vela_b64char(i64 %d1m)
  %c2 = call i8 @__vela_b64char(i64 %d2m)
  %c3 = call i8 @__vela_b64char(i64 %d3m)
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
  %ec0 = call i8 @__vela_b64char(i64 %e0m)
  %ec1 = call i8 @__vela_b64char(i64 %e1m)
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
  %gc0 = call i8 @__vela_b64char(i64 %g0m)
  %gc1 = call i8 @__vela_b64char(i64 %g1m)
  %gc2 = call i8 @__vela_b64char(i64 %g2m)
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

define i32 @__vela_b64val(i8 %c) {
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

define {i1, i64, i64} @__vela_b64_decode(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %m4 = and i64 %len, 3
  %notmul4 = icmp ne i64 %m4, 0
  %empty = icmp eq i64 %len, 0
  br i1 %notmul4, label %none, label %ok0
ok0:
  %cap = mul i64 %len, 1
  %sz = add i64 %cap, 1
  %out = call ptr @__vela_malloc(i64 %sz)
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
  %v0 = call i32 @__vela_b64val(i8 %c0)
  %v1 = call i32 @__vela_b64val(i8 %c1)
  %v2raw = call i32 @__vela_b64val(i8 %c2)
  %v3raw = call i32 @__vela_b64val(i8 %c3)
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
  %v = call i1 @__vela_utf8valid(ptr %out, i64 %o)
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

/// `bytes(s)` / `chars(s)`: build an `Array<Int>` ({ptr,len,cap}) of a string's
/// raw UTF-8 bytes, or of its decoded Unicode code points (a two-pass UTF-8 decode
/// — count leaders, then decode each 1–4 byte sequence).
const STRING_RUNTIME: &str = "\
define {ptr, i64, i64} @__vela_str_bytes(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
  %sz = mul i64 %len, 8
  %data = call ptr @__vela_malloc(i64 %sz)
  br label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %i2, %body ]
  %done = icmp uge i64 %i, %len
  br i1 %done, label %ret, label %body
body:
  %sp = getelementptr i8, ptr %s, i64 %i
  %b = load i8, ptr %sp
  %v = zext i8 %b to i64
  %dp = getelementptr i64, ptr %data, i64 %i
  store i64 %v, ptr %dp
  %i2 = add i64 %i, 1
  br label %loop
ret:
  %r0 = insertvalue {ptr, i64, i64} undef, ptr %data, 0
  %r1 = insertvalue {ptr, i64, i64} %r0, i64 %len, 1
  %r2 = insertvalue {ptr, i64, i64} %r1, i64 %len, 2
  ret {ptr, i64, i64} %r2
}

define {ptr, i64, i64} @__vela_str_chars(ptr %s) {
entry:
  %len = call i64 @__vela_strlen(ptr %s)
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
  %data = call ptr @__vela_malloc(i64 %sz)
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
define i1 @__vela_regex_run(ptr %s, ptr %table, i64 %start, ptr %accept) {
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

/// The private LLVM symbol for an `extern` import (RFC-0012). Prefixed so it
/// cannot collide with a real C symbol on the native target: the generated C
/// trap stub defines exactly this name, and the wasm import name is carried
/// separately by the `wasm-import-name` attribute (the raw Vela name).
fn extern_symbol(name: &str) -> String {
    format!("__vela_extern_{name}")
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

/// Emit a complete LLVM IR module for `program`.
pub fn emit(program: &Program) -> Result<String, String> {
    let mut out = String::new();
    // module preamble: printf/abort + format strings (opaque-pointer style)
    out.push_str("; Vela v0.1 — generated LLVM IR (target: LLVM 15+)\n");
    out.push_str("declare i32 @printf(ptr, ...)\n");
    // exit() (not abort()) so stdio buffers flush and the exit code is a clean 1,
    // matching the interpreter.
    out.push_str("declare void @exit(i32)\n");
    out.push_str("declare i32 @strcmp(ptr, ptr)\n");
    out.push_str("declare i32 @__vela_strncmp(ptr, ptr, i64)\n");
    out.push_str("declare ptr @strstr(ptr, ptr)\n");
    // Heap + string runtime (dynamic strings). Allocations are not yet freed —
    // the reclamation strategy is RFC-0004's open question.
    out.push_str("declare i64 @__vela_strlen(ptr)\n");
    out.push_str("declare ptr @__vela_malloc(i64)\n");
    out.push_str("declare ptr @__vela_realloc(ptr, i64)\n");
    out.push_str("declare void @free(ptr)\n");
    out.push_str("declare ptr @strcpy(ptr, ptr)\n");
    out.push_str("declare ptr @strcat(ptr, ptr)\n");
    out.push_str("declare i32 @__vela_snprintf(ptr, i64, ptr, ...)\n");
    // Logging (RFC-0008) and traps: fprintf/fputs to stderr. `stderr` is a C
    // macro with no portable symbol, so the stream handles come from a tiny C
    // shim (`__vela_stderr`/`__vela_stdout`, embedded in vela-cli and compiled
    // by clang alongside this IR) that works on every libc (MSVC, glibc,
    // wasi-libc).
    out.push_str("declare i32 @fprintf(ptr, ptr, ...)\n");
    out.push_str("declare ptr @__vela_stderr()\n");
    out.push_str("declare ptr @__vela_stdout()\n");
    // Runtime traps (division, and eventually every trap) fputs to stderr with
    // the interpreter's exact `error: ...` wording, then exit(1).
    out.push_str("declare i32 @fputs(ptr, ptr)\n");
    out.push_str("declare ptr @fopen(ptr, ptr)\n");
    out.push_str("declare i32 @fclose(ptr)\n");
    // `extern` imports (RFC-0012): each body-less `extern fn` becomes a wasm
    // import from the fixed `vela` namespace. We emit ONE target-neutral IR —
    // a `declare` carrying the wasm-import attributes plus a real `call` at each
    // use site (see `gen_extern_call`). On the wasm target the import resolves
    // against the host page's `vela` object; on native the symbol is satisfied
    // by a per-extern C trap stub that vela-cli links in (printing the canonical
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
            "attributes #{grp} = {{ \"wasm-import-module\"=\"vela\" \"wasm-import-name\"=\"{}\" }}\n",
            f.name
        ));
    }
    // For a `file(..)` sink: a global stream handle plus the path/mode constants.
    if let LogSink::File(path) = &program.log_sink {
        out.push_str("@__vela_log_file = global ptr null\n");
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
        "@__vela_utf8d = private unnamed_addr constant [364 x i8] [{table_body}]\n"
    ));
    out.push_str(
        "@__vela_b64alpha = private unnamed_addr constant [64 x i8] \
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
    // Module-state initializers (RFC-0013) are lowered in `@__vela_globals_init`,
    // so any string literal they mention must be pooled too.
    for g in &program.globals {
        collect_strings_expr(&g.init, &mut literals, &type_map);
    }
    for (i, s) in literals.iter().enumerate() {
        let name = format!("@.str.{i}");
        let (escaped, len) = llvm_str(s);
        out.push_str(&format!(
            "{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\"\n"
        ));
        str_globals.insert(s.clone(), name);
    }
    out.push('\n');

    // Compile every distinct `=~` pattern to a DFA and emit its transition table
    // and accepting-state array as globals (the runner `@__vela_regex_run` walks
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
        let dfa = vela_frontend::regex::compile(pat).expect("regex validated by checker");
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
    let ownership = vela_frontend::own::analyze(program);
    let droppable_map = &ownership.droppable;

    let protocol_methods: HashMap<String, String> = program
        .protocols
        .iter()
        .flat_map(|p| p.methods.iter().map(|m| (m.name.clone(), p.name.clone())))
        .collect();

    // ---- module state (RFC-0013) ----------------------------------------
    // One LLVM global per binding (`@g.<name>`, `zeroinitializer`), plus a
    // synthesized `@__vela_globals_init()` that runs every initializer's stores
    // in declaration order (heap-valued inits — arrays, strings — work because
    // this runs at runtime). It is called from `vela_entry` BEFORE `main`. Reads
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
        globals_init_ir.push_str("define internal void @__vela_globals_init() {\n");
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
        let sym = if f.name == "main" { "vela_main".to_string() } else { format!("vela_{}", f.name) };
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
    }

    // 2. Generic instantiations, transitively.
    while let Some((name, type_args)) = queue.pop() {
        let sym = mangle_name(&name, &type_args);
        if !emitted.insert(sym.clone()) {
            continue;
        }
        let f = funcs[&name];
        let subst: HashMap<String, Type> =
            f.type_params.iter().cloned().zip(type_args.iter().cloned()).collect();
        let mut gen = Gen::new(
            &ret_types, &param_types, &param_caps, &types, &variants, &str_globals, &subst, &funcs,
            droppable_map, &regex_globals,
        );
        gen.log_level = program.log_level;
        gen.log_sink = program.log_sink.clone();
        gen.protocol_methods = protocol_methods.clone();
        gen.globals = globals_map.clone();
        gen.function(f, &sym, &mut out)?;
        out.push('\n');
        let insts = std::mem::take(&mut gen.instantiations);
        enqueue(&emitted, &mut queue, insts);
    }

    // The module-state initializer function (RFC-0013), defined after the user
    // functions (textual order is immaterial to LLVM).
    out.push_str(&globals_init_ir);

    // C entry point: call Vela's main and reduce its i64 to a process exit code.
    // Mask to the low 8 bits so the result matches the interpreter (which does
    // `code & 0xff`) and the POSIX 0–255 exit-status convention — otherwise a
    // return value > 255 would diverge on Windows, which preserves the full i32.
    out.push_str("define i32 @vela_entry() {\n");
    out.push_str("entry:\n");
    // Open the log file before running, if the program logs to one.
    let file_sink = matches!(program.log_sink, LogSink::File(_));
    if file_sink {
        out.push_str("  %lf = call ptr @fopen(ptr @.logpath, ptr @.logmode)\n");
        out.push_str("  store ptr %lf, ptr @__vela_log_file\n");
    }
    // Initialize module state (RFC-0013) before `main` runs — and therefore
    // before any exported extern handler the host calls afterward.
    if !program.globals.is_empty() {
        out.push_str("  call void @__vela_globals_init()\n");
    }
    out.push_str("  %r = call i64 @vela_main()\n");
    // Flush and close the log file after running (before returning the code).
    if file_sink {
        out.push_str("  %lfc = load ptr, ptr @__vela_log_file\n");
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
    /// how), for the function currently being emitted (from `vela_frontend::own`).
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
        }
    }

    /// Resolve a type to its structural form: substitute generic parameters for
    /// this instantiation, then delegate to the shared resolver (which also
    /// evaluates the `Omit`/`Pick`/`Merge` transformers).
    fn resolve(&self, ty: &Type) -> Type {
        let t = vela_frontend::types::substitute(ty, self.subst);
        vela_frontend::types::resolve(&t, self.types)
    }

    /// The fields of `ty` if it is (resolves to) a record.
    fn record_fields(&self, ty: &Type) -> Option<Vec<Field>> {
        let t = vela_frontend::types::substitute(ty, self.subst);
        vela_frontend::types::record_fields(&t, self.types)
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
            // A fixed-size array lowers to the LLVM value aggregate [N x T].
            Type::ArrayN(inner, n) => format!("[{n} x {}]", self.llt(&inner)),
            // A task's result handle is just its result value (deterministic
            // fork-join needs no boxing).
            Type::Task(inner) => self.llt(&inner),
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
            // `Err` is the checker's recovery sentinel; a program with any `Err`
            // already has diagnostics and never reaches codegen. Lower to void
            // as a defensive fallback (never observed in practice).
            Type::Err => "void".into(),
        }
    }

    /// Coerce a value of type `from` to type `to`, emitting a field-by-field
    /// rebuild for structural record width subtyping (RFC-0002). For everything
    /// else the bit pattern is unchanged and only the reported type differs.
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
            self.emit(format!("{buf} = call ptr @__vela_region_alloc(i64 {size})"));
        } else {
            self.emit(format!("{buf} = call ptr @__vela_malloc(i64 {size})"));
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
        self.emit(format!("{la} = call i64 @__vela_strlen(ptr {a})"));
        self.emit(format!("{lb} = call i64 @__vela_strlen(ptr {b})"));
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
        self.emit(format!("{buf} = call ptr @__vela_malloc(i64 {sz})"));
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
        self.emit(format!("{e} = call ptr @__vela_stderr()"));
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
        // Vela name (not the internal `vela_<name>` symbol). The attribute is a
        // GC root, so no `-Wl,--export` flag is needed for the function itself;
        // on native targets LLVM simply ignores the string attribute. Note the
        // String ABI asymmetry vs. an import (M1): an exported fn's `String`
        // parameter is a single `ptr` (the normal lowering) because the JS caller
        // CAN allocate — it grabs `__vela_malloc`, copies UTF-8 + a NUL, and
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
                self.emit(format!("call void @__vela_cell_check(i64 {s}, i64 {g})"));
                self.emit(format!("{p} = call ptr @__vela_cell_ptr(i64 {s})"));
                self.emit(format!("call void @free(ptr {p})"));
                self.emit(format!("call void @__vela_cell_release_slot(i64 {s})"));
            }
            DropKind::AfreeArr => {
                // Auto-afree: free the array's final backing buffer (field 0).
                let a = self.fresh_tmp();
                let d = self.fresh_tmp();
                self.emit(format!("{a} = load {{ ptr, i64, i64 }}, ptr {slot}"));
                self.emit(format!("{d} = extractvalue {{ ptr, i64, i64 }} {a}, 0"));
                self.emit(format!("call void @free(ptr {d})"));
            }
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, value, ty: decl_ty, .. } => {
                // Node-address identity — must match `vela_frontend::own`, which
                // ran on this same borrowed AST.
                let key = stmt as *const Stmt as usize;
                let (v, vty) = self.gen_expr(value)?;
                // Coerce to the annotation if present (record width subtyping).
                let (v, bty) = match decl_ty {
                    Some(t) => self.coerce(v, &vty, t)?,
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
                    other => Err(format!("`{name}[i] = ..` needs an Array, found {other:?}")),
                }
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => {
                        let (v, vty) = self.gen_expr(e)?;
                        let ret = self.fn_ret.clone();
                        let (v, _) = self.coerce(v, &vty, &ret)?;
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
                        self.emit(format!("{len} = call i64 @__vela_strlen(ptr {av})"));
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
                self.emit("call void @__vela_region_enter()".into());
                self.region_depth += 1;
                self.gen_block(body)?;
                self.region_depth -= 1;
                if !self.terminated {
                    self.emit("call void @__vela_region_exit()".into());
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
                let (slot, ty) = self.lookup(name).ok_or_else(|| format!("unbound `{name}`"))?;
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
                        // `str.length` is the byte length via `strlen`.
                        Type::Str => {
                            let len = self.fresh_tmp();
                            self.emit(format!("{len} = call i64 @__vela_strlen(ptr {v})"));
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
            // A spawned task: run the (pure) callee. Execution is eager and
            // deterministic; the type is `Task<ret>` so a `join` is required.
            Expr::Spawn { name, args, .. } => {
                let (v, ret) = self.gen_call(name, args)?;
                Ok((v, Type::Task(Box::new(ret))))
            }
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
                "native fallible construction supports Int64-based types only (`{name}`); use `velac run`"
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
            let field_ty = vela_frontend::types::substitute(&decl_f.ty, &solved);
            let (v, _) = self.coerce(v, &vty, &field_ty)?;
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
                    vela_frontend::consteval::eval(e, &HashMap::new()).is_some()
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
        self.emit(format!("{p} = call ptr @__vela_malloc(i64 {size})"));
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
                "{t} = call i1 @__vela_regex_run(ptr {s}, ptr {table}, i64 {start}, ptr {accept})"
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

        // String equality compares contents via strcmp, not pointers.
        if matches!(op, BinOp::Eq | BinOp::NotEq) && self.resolve(&lty) == Type::Str {
            let c = self.fresh_tmp();
            self.emit(format!("{c} = call i32 @strcmp(ptr {l}, ptr {r})"));
            let t = self.fresh_tmp();
            let pred = if op == BinOp::Eq { "eq" } else { "ne" };
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
        self.emit(format!("{e} = call ptr @__vela_stderr()"));
        self.emit(format!(
            "call i32 (ptr, ptr, ...) @fprintf(ptr {e}, ptr @.trap.aoob, i64 {iv})"
        ));
        self.emit("call void @exit(i32 1)".into());
        self.emit_term("unreachable".into());
    }

    fn gen_call(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
        // `schemaOf(TypeName)` reflects a type at compile time — build its Schema
        // literal from the type declaration and lower that (identical to interp).
        if name == "schemaOf" {
            let sl = match args.first() {
                Some(Expr::Var { name: tn, .. }) if self.types.contains_key(tn) => {
                    vela_frontend::types::schema_struct_lit(&self.types[tn])
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
                    vela_frontend::types::json_schema_string(&self.types[tn], self.types)
                }
                _ => return Err("`jsonSchema` needs a declared type name".to_string()),
            };
            return self.gen_expr(&Expr::Str(json));
        }
        // Numeric conversion `Int32(x)`, `Float64(x)`, ...
        if let Some(target) = vela_frontend::types::numeric_conv_target(name) {
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
                        self.emit(format!("{stream} = call ptr @__vela_stderr()"))
                    }
                    LogSink::Stdout => {
                        self.emit(format!("{stream} = call ptr @__vela_stdout()"))
                    }
                    // The file is opened once in `@main` (below).
                    LogSink::File(_) => {
                        self.emit(format!("{stream} = load ptr, ptr @__vela_log_file"))
                    }
                }
                self.emit(format!(
                    "call i32 (ptr, ptr, ...) @fprintf(ptr {stream}, ptr @.fmt.log, ptr {lvl}, ptr {logv}, ptr {msgv})"
                ));
            }
            return Ok(("".into(), Type::Unit));
        }

        // (`len(String)` was removed; a String's byte length is the `.length`
        // field, lowered at `Expr::Field` via `@__vela_strlen`.)
        // Text encodings. Encoders return a fresh String; decoders return the
        // Option<String> aggregate (runtime helpers do the work + UTF-8 checking).
        if matches!(name, "hexEncode" | "base64Encode" | "urlEncode") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper = match name {
                "hexEncode" => "@__vela_hex_encode",
                "base64Encode" => "@__vela_b64_encode",
                _ => "@__vela_url_encode",
            };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call ptr {helper}(ptr {v})"));
            return Ok((t, Type::Str));
        }
        if matches!(name, "hexDecode" | "base64Decode" | "urlDecode") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper = match name {
                "hexDecode" => "@__vela_hex_decode",
                "base64Decode" => "@__vela_b64_decode",
                _ => "@__vela_url_decode",
            };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {{ i1, i64, i64 }} {helper}(ptr {v})"));
            return Ok((t, Type::Option(Box::new(Type::Str))));
        }
        // bytes(s) / chars(s): decode a string into an Array<Int> of bytes or of
        // Unicode code points (runtime helpers do the UTF-8 work).
        if matches!(name, "bytes" | "chars") {
            let (v, _) = self.gen_expr(&args[0])?;
            let helper =
                if name == "bytes" { "@__vela_str_bytes" } else { "@__vela_str_chars" };
            let t = self.fresh_tmp();
            self.emit(format!("{t} = call {{ ptr, i64, i64 }} {helper}(ptr {v})"));
            return Ok((t, Type::Array(Box::new(Type::Int))));
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
            self.emit(format!("{lb} = call i64 @__vela_strlen(ptr {b})"));
            self.emit(format!("{c} = call i32 @__vela_strncmp(ptr {a}, ptr {b}, i64 {lb})"));
            self.emit(format!("{r} = icmp eq i32 {c}, 0"));
            return Ok((r, Type::Bool));
        }
        // endsWith(a, b): b fits in a AND strncmp(a + (|a|-|b|), b, |b|) == 0.
        if name == "endsWith" {
            let (a, _) = self.gen_expr(&args[0])?;
            let (b, _) = self.gen_expr(&args[1])?;
            let la = self.fresh_tmp();
            let lb = self.fresh_tmp();
            self.emit(format!("{la} = call i64 @__vela_strlen(ptr {a})"));
            self.emit(format!("{lb} = call i64 @__vela_strlen(ptr {b})"));
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
            self.emit(format!("{c} = call i32 @__vela_strncmp(ptr {p}, ptr {b}, i64 {lb})"));
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
                        "call i32 (ptr, i64, ptr, ...) @__vela_snprintf(ptr {buf}, i64 24, ptr @.fmt.ld, i64 {v})"
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
                        "call i32 (ptr, i64, ptr, ...) @__vela_snprintf(ptr {buf}, i64 24, ptr {fmt}, i64 {w})"
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
                        "call i32 (ptr, i64, ptr, ...) @__vela_snprintf(ptr {buf}, i64 512, ptr {fmt}, double {v})"
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
                        "call i32 (ptr, i64, ptr, ...) @__vela_snprintf(ptr {buf}, i64 512, ptr {fmt}, double {d})"
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
                    self.emit(format!("{len} = call i64 @__vela_strlen(ptr {v})"));
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
            self.emit(format!("{payload} = call ptr @__vela_malloc(i64 {size})"));
            self.emit(format!("store {ll} {v}, ptr {payload}"));
            let slot = self.fresh_tmp();
            self.emit(format!("{slot} = call i64 @__vela_cell_alloc(ptr {payload})"));
            let g = self.fresh_tmp();
            self.emit(format!("{g} = call i64 @__vela_cell_getgen(i64 {slot})"));
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
            self.emit(format!("call void @__vela_cell_check(i64 {slot}, i64 {g})"));
            let payload = self.fresh_tmp();
            self.emit(format!("{payload} = call ptr @__vela_cell_ptr(i64 {slot})"));
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
                    self.emit(format!("call void @__vela_cell_release_slot(i64 {slot})"));
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
            self.emit(format!("{nd} = call ptr @__vela_realloc(ptr {data}, i64 {nb})"));
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
                g.emit(format!("{e} = call ptr @__vela_stderr()"));
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
                // byte and zero-extend to i64 (the byte's value).
                Type::Str => {
                    let len = self.fresh_tmp();
                    self.emit(format!("{len} = call i64 @__vela_strlen(ptr {av})"));
                    let oob = self.fresh_tmp();
                    self.emit(format!("{oob} = icmp uge i64 {iv}, {len}"));
                    self.emit_term(format!("br i1 {oob}, label %{bad_l}, label %{ok_l}"));
                    emit_trap(self, "@.trap.soob");
                    self.emit_label(&ok_l);
                    let ep = self.fresh_tmp();
                    let byte = self.fresh_tmp();
                    let v = self.fresh_tmp();
                    self.emit(format!("{ep} = getelementptr i8, ptr {av}, i64 {iv}"));
                    self.emit(format!("{byte} = load i8, ptr {ep}"));
                    self.emit(format!("{v} = zext i8 {byte} to i64"));
                    return Ok((v, Type::Int));
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
        // @join (`t.join()`): with eager tasks the result is already computed.
        if name == "@join" {
            let (v, ty) = self.gen_expr(&args[0])?;
            let inner = match self.resolve(&ty) {
                Type::Task(inner) => *inner,
                other => other,
            };
            return Ok((v, inner));
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
            // gen each payload, boxing any wider than a word.
            let mut payloads = Vec::new();
            for a in args {
                let (v, ty) = self.gen_expr(a)?;
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
            let concrete = vela_frontend::types::substitute(&recv_ty, self.subst);
            let key = vela_frontend::types::type_key(&concrete)
                .ok_or_else(|| format!("cannot dispatch `{name}` on {recv_ty:?}"))?;
            let mangled = vela_frontend::types::impl_method_name(&proto, &key, name);
            return self.gen_call(&mangled, args);
        }

        // `extern` call (RFC-0012): emit the real host call. This is the one
        // call whose behavior differs by target — the shared IR carries the
        // import, and the C trap stub (native) vs the `vela` namespace (wasm)
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
                arg_tys.push(vela_frontend::types::substitute(&vty, self.subst));
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
                let pty = vela_frontend::types::substitute(&p.ty, &call_subst);
                let (v, cty) = self.coerce(v, aty, &pty)?;
                arg_ops.push(format!("{} {v}", self.llt(&cty)));
            }
            self.instantiations.push((name.to_string(), type_args));

            let ret_ty = vela_frontend::types::substitute(&callee.ret, &call_subst);
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
                Some(p) => self.coerce(v, &vty, p)?,
                None => (v, vty),
            };
            arg_ops.push(format!("{} {v}", self.llt(&pty)));
        }
        let sym = format!("vela_{name}");
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

    /// Emit a real call to an `extern` import (RFC-0012). Each argument is
    /// coerced to its declared parameter type, then to the ABI value type; a
    /// `String` crosses as a `(ptr, strlen)` pair. The result is converted from
    /// the ABI type back to the value's Vela representation. The callee symbol
    /// (`@__vela_extern_<name>`) resolves to the host import (wasm) or the linked
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
                self.emit(format!("{len} = call i64 @__vela_strlen(ptr {v})"));
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
        let is_const = vela_frontend::consteval::eval(arg, &HashMap::new()).is_some();
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
/// 12 is REJECT. Used by `@__vela_utf8valid` so the native decoders reject exactly
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
                        let sl = vela_frontend::types::schema_struct_lit(decl);
                        collect_strings_expr(&sl, out, types);
                    }
                }
            }
            // `jsonSchema(TypeName)` lowers to a single computed JSON string literal;
            // seed the exact string the code generator will emit (see `gen_call`).
            if name == "jsonSchema" {
                if let Some(Expr::Var { name: tn, .. }) = args.first() {
                    if let Some(decl) = types.get(tn) {
                        let js = vela_frontend::types::json_schema_string(decl, types);
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
        Expr::Spawn { args, .. } => {
            for e in args {
                collect_strings_expr(e, out, types);
            }
        }
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
        _ => {}
    }
}

/// The mangled LLVM symbol for a generic instantiation, e.g. `vela_id__Int`.
fn mangle_name(name: &str, type_args: &[Type]) -> String {
    let parts: Vec<String> = type_args.iter().map(mangle_ty).collect();
    format!("vela_{name}__{}", parts.join("_"))
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
        Type::Task(inner) => format!("Task{}", mangle_ty(inner)),
        Type::Logger => "Logger".into(),
        // Checker recovery sentinel; never reaches codegen in a valid program.
        Type::Err => "Err".into(),
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
    use vela_frontend::check;

    #[test]
    fn emits_module_with_main_wrapper() {
        let program = check("fn main() -> Int64 { let x = 2 + 3; print(x); return x; }").unwrap();
        let ir = emit(&program).unwrap();
        assert!(ir.contains("define i64 @vela_main("));
        assert!(ir.contains("define i32 @vela_entry()"));
        assert!(ir.contains("@printf"));
        assert!(ir.contains("add i64"));
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
        assert!(ir.contains("@__vela_stderr()"), "stderr handle: {ir}");
        assert!(ir.contains("@fprintf"), "fprintf: {ir}");
        assert!(ir.contains("@.lvl.info"), "level name global: {ir}");
    }

    #[test]
    fn stdout_sink_selects_stream_1() {
        let src = "logging { sink: stdout } \
                   fn main() -> Int64 { let l = logger(\"m\"); l.error(\"x\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@__vela_stdout()"), "stdout via the shim: {ir}");
    }

    #[test]
    fn file_sink_opens_and_closes_in_main() {
        let src = "logging { sink: file(\"a.log\") } \
                   fn main() -> Int64 { let l = logger(\"m\"); l.error(\"x\"); return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@fopen(ptr @.logpath"), "opens the file: {ir}");
        assert!(ir.contains("@fclose"), "closes the file: {ir}");
        assert!(ir.contains("load ptr, ptr @__vela_log_file"), "logs to the file handle: {ir}");
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
        assert!(ir.contains("call i64 @vela_sql("), "calls the tag: {ir}");
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
        assert!(ir.contains("@__vela_snprintf"), "str(Int64) -> snprintf: {ir}");
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
        assert!(ir.contains("@__vela_strlen"), "concat length: {ir}");
        assert!(ir.contains("@strcpy") && ir.contains("@strcat"), "concat copy: {ir}");
        assert!(ir.contains("@__vela_snprintf"), "toString(Int) -> snprintf: {ir}");
    }

    #[test]
    fn contextual_array_literal_lowers_to_heap_triple() {
        // A literal in an `Array<T>` slot is malloc'd into the `{ptr,len,cap}`
        // triple (like `list([..])`), then `.length` reads field 1.
        let src = "fn main() -> Int64 { let a: Array<Int64> = [1, 2, 3]; return a.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vela_malloc"), "heap copy: {ir}");
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
        // `@main` masks vela_main's return so it matches the interpreter's
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

    // The region runtime (`__vela_region_exit`) always contributes exactly one
    // `call void @free`, so an *auto*-free is a free beyond that baseline.
    fn free_calls(ir: &str) -> usize {
        ir.matches("call void @free(ptr").count()
    }

    #[test]
    fn non_escaping_temporary_is_freed() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let n = s.length; return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(free_calls(&ir) > 1, "expected an auto-free beyond the runtime: {ir}");
    }

    #[test]
    fn escaping_temporary_is_not_freed() {
        // `s` is aliased into `t`, so it must not be auto-freed (would dangle).
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let t = s; return t.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert_eq!(free_calls(&ir), 1, "only the runtime free should be present: {ir}");
    }

    #[test]
    fn generational_reference_lowers_to_slab_calls() {
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); \
                   let v = get(c); release(c); return v; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @__vela_cell_alloc"), "{ir}");
        assert!(ir.contains("call ptr @__vela_cell_ptr"), "{ir}");
        assert!(ir.contains("call void @__vela_cell_release_slot"), "{ir}");
        // The generation check is what makes a stale reference safe.
        assert!(ir.contains("call void @__vela_cell_check"), "{ir}");
    }

    #[test]
    fn non_escaping_cell_is_auto_released() {
        // No explicit `release` in the source, yet the non-escaping cell must be
        // released at block exit (inferred by the ownership analysis).
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); return get(c); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @__vela_cell_release"), "expected auto-release: {ir}");
    }

    #[test]
    fn caller_frees_owned_transfer_result() {
        // `make` returns a fresh owned String; `main` must free the result it
        // receives, but `make` must NOT free what it moves out.
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let g = make(a, b); return g.length; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // One runtime free + exactly one auto-free (in `main`, for `g`).
        assert_eq!(free_calls(&ir), 2, "caller should free the owned result once: {ir}");
    }

    #[test]
    fn region_brackets_body_with_enter_and_exit() {
        let src = "fn main() -> Int64 { \
                       let a = \"x\"; let b = \"y\"; let mut n = 0; \
                       region { let s = a + b; n = s.length; } \
                       return n; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call void @__vela_region_enter()"), "{ir}");
        assert!(ir.contains("call void @__vela_region_exit()"), "{ir}");
        // concat routes through the arena at runtime.
        assert!(ir.contains("@__vela_region_alloc"), "{ir}");
        assert!(ir.contains("load i64, ptr @__vela_region_sp"), "{ir}");
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
        assert!(ir.contains("call i64 @__vela_strlen"), "str .length → strlen: {ir}");
    }

    #[test]
    fn string_index_lowers_to_byte_load() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return s[0]; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("load i8"), "loads a byte: {ir}");
        assert!(ir.contains("zext i8") && ir.contains("to i64"), "zero-extends: {ir}");
        assert!(ir.contains("@.trap.soob"), "bounds-checked: {ir}");
    }

    #[test]
    fn encodings_lower_to_runtime() {
        let ir = emit(&check("fn main() -> Int64 { \
            let a = hexEncode(\"x\"); let b = base64Encode(\"x\"); let c = urlEncode(\"x\"); \
            let d = hexDecode(\"41\"); return 0; }").unwrap()).unwrap();
        assert!(ir.contains("call ptr @__vela_hex_encode"), "hexEncode: {ir}");
        assert!(ir.contains("call ptr @__vela_b64_encode"), "base64Encode: {ir}");
        assert!(ir.contains("call ptr @__vela_url_encode"), "urlEncode: {ir}");
        assert!(ir.contains("call { i1, i64, i64 } @__vela_hex_decode"), "hexDecode: {ir}");
        // The strict UTF-8 validator DFA + its 364-byte table are present.
        assert!(ir.contains("@__vela_utf8valid"), "validator: {ir}");
        assert!(ir.contains("@__vela_utf8d = private"), "DFA table: {ir}");
    }

    #[test]
    fn chars_and_bytes_lower_to_runtime() {
        let ir = emit(&check("fn main() -> Int64 { return chars(\"hi\").length + bytes(\"hi\").length; }").unwrap()).unwrap();
        assert!(ir.contains("call { ptr, i64, i64 } @__vela_str_chars"), "chars → decoder: {ir}");
        assert!(ir.contains("call { ptr, i64, i64 } @__vela_str_bytes"), "bytes → helper: {ir}");
        // The UTF-8 decoder is defined in the module.
        assert!(ir.contains("@__vela_str_chars(ptr %s)"), "decoder emitted: {ir}");
    }

    #[test]
    fn string_methods_lower_to_libc() {
        let c = emit(&check("fn f(s: String) -> Bool { return contains(s, \"x\"); } \
                             fn main() -> Int64 { return 0; }").unwrap()).unwrap();
        assert!(c.contains("call ptr @strstr"), "contains → strstr: {c}");
        let s = emit(&check("fn f(s: String) -> Bool { return startsWith(s, \"x\"); } \
                             fn main() -> Int64 { return 0; }").unwrap()).unwrap();
        assert!(s.contains("call i32 @__vela_strncmp"), "startsWith → strncmp: {s}");
    }

    #[test]
    fn validated_string_runtime_check_uses_strlen() {
        // A non-constant String construction checks `value.length` via strlen and
        // traps through the same validation-error path.
        let src = "type Name = String where value.length >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("call i64 @__vela_strlen"), "refinement uses strlen: {ir}");
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
        assert!(ir.contains("call i1 @__vela_regex_run"), "calls the runner: {ir}");
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
        assert!(ir.contains("define i64 @vela_id__Int"), "Int64 instance:\n{ir}");
        assert!(ir.contains("define ptr @vela_id__Str"), "Str instance:\n{ir}");
        assert!(!ir.contains("@vela_id("), "no un-instantiated generic body:\n{ir}");
    }

    #[test]
    fn string_lowers_to_global_and_strcmp() {
        let src = "fn main() -> Int64 { if \"a\" == \"a\" { return 1; } return 0; }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("@.str.0 = private"), "string global:\n{ir}");
        assert!(ir.contains("call i32 @strcmp"), "== uses strcmp:\n{ir}");
    }

    #[test]
    fn enum_match_lowers_to_switch() {
        let src = "type E = | A(Int64) | B(Int64) | C; \
                   fn f(e: E) -> Int64 { return match e { A(x) => x, B(y) => y, C => 0 }; } \
                   fn main() -> Int64 { return f(A(5)); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(ir.contains("switch i64"), "enum match uses a switch:\n{ir}");
        assert!(ir.contains("@vela_f({ i64, i64 }"), "enum lowers to a 2-word aggregate:\n{ir}");
        assert!(ir.contains("insertvalue { i64, i64 } undef, i64 0"), "variant A has tag 0:\n{ir}");
    }

    #[test]
    fn omit_transformer_lowers_to_narrower_struct() {
        let src = "type User = { id: Int64, name: Int64, pw: Int64 }; type Public = Omit<User, pw>; \
                   fn f(p: Public) -> Int64 { return p.name; } \
                   fn main() -> Int64 { let u = User { id: 1, name: 2, pw: 3 }; return f(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // Public resolves to a 2-field struct; User is 3 fields; coercion happens.
        assert!(ir.contains("@vela_f({ i64, i64 }"), "Public layout: {ir}");
        assert!(ir.contains("insertvalue { i64, i64, i64 }"), "User is 3 fields: {ir}");
    }

    #[test]
    fn record_width_subtyping_coerces() {
        let src = "type Named = { name: Int64 }; type User = { name: Int64, age: Int64 }; \
                   fn greet(w: Named) -> Int64 { return w.name; } \
                   fn main() -> Int64 { let u = User { name: 7, age: 30 }; return greet(u); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        // greet takes a 1-field record; User is a 2-field record.
        assert!(ir.contains("@vela_greet({ i64 }"), "greet param layout: {ir}");
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
        // wasm-import attributes (namespace `vela`, field = the Vela name) on
        // the prefixed symbol; a String parameter flattens to a (ptr, i64)
        // pair; the call site passes the pointer plus a computed length.
        let src = "extern fn jsLog(msg: String) \
                   extern fn jsAdd(a: Int64, b: Int64) -> Int64 \
                   fn main() -> Int64 { jsLog(\"hi\"); return jsAdd(1, 2); }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("declare void @__vela_extern_jsLog(ptr, i64)"),
            "String param flattens to (ptr, i64): {ir}"
        );
        assert!(
            ir.contains("declare i64 @__vela_extern_jsAdd(i64, i64)"),
            "scalar extern declared with ABI types: {ir}"
        );
        assert!(
            ir.contains("\"wasm-import-module\"=\"vela\"") &&
            ir.contains("\"wasm-import-name\"=\"jsLog\""),
            "wasm import attributes present: {ir}"
        );
        assert!(
            ir.contains("call i64 @__vela_extern_jsAdd(i64 1, i64 2)"),
            "extern call emitted at the use site: {ir}"
        );
    }

    #[test]
    fn export_extern_emits_a_normal_define_with_the_export_attribute() {
        // RFC-0012 M2: an `export extern fn` is a normal `define` under the
        // internal `vela_<name>` symbol, carrying an inline `wasm-export-name`
        // attribute so wasm-ld exports it under the bare Vela name. A `String`
        // parameter is a SINGLE `ptr` (not the import's (ptr,len) pair) — the JS
        // caller allocates the buffer, so decode-side length is a NUL scan.
        let src = "export extern fn velaAdd(a: Int64, b: Int64) -> Int64 { return a + b } \
                   export extern fn greet(name: String) -> String { return name } \
                   fn main() -> Int64 { return velaAdd(1, 2) }";
        let ir = emit(&check(src).unwrap()).unwrap();
        assert!(
            ir.contains("define i64 @vela_velaAdd(i64 %arg0, i64 %arg1) \"wasm-export-name\"=\"velaAdd\" {"),
            "scalar export extern is a normal define with the export attr: {ir}"
        );
        assert!(
            ir.contains("define ptr @vela_greet(ptr %arg0) \"wasm-export-name\"=\"greet\" {"),
            "String param/return are single ptrs; export attr present: {ir}"
        );
        // It is NOT a body-less import: no declare, no import attributes for it.
        assert!(
            !ir.contains("@__vela_extern_velaAdd"),
            "an export extern is not a wasm import: {ir}"
        );
        // A plain fn keeps no export attribute.
        assert!(
            ir.contains("define i64 @vela_main(") && !ir.contains("@vela_main() \"wasm-export-name\""),
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
        assert!(ir.contains("call i1 @vela_t()"), "{ir}");
        // and the branch consumes an i1, never an i64 call result
        assert!(!ir.contains("call i64 @vela_t()"), "{ir}");
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
        // A synthesized init function, called from `vela_entry` before main.
        assert!(ir.contains("define internal void @__vela_globals_init()"), "{ir}");
        let init_at = ir.find("call void @__vela_globals_init()").expect("init call");
        let main_at = ir.find("call i64 @vela_main()").expect("main call");
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
}
