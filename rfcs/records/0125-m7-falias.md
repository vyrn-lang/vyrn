#### A store into an element of an Array alias writes the buffer, not a value of the alias's own (2026-09-24, `m7-falias`)
RFC-0125, milestone M7.
Decision: `core_written` does not count a store whose path lands in an element (`kernel::in_element`, now public) of a name of type `Array`, so `Fn_::core_alias` gives such a name the place of its projection. The lead's rule (brief `m7-falias`). It is narrowed to `Array` because the arm's `let` copies a layout's bytes. An element store through the copy of an Array header lands in the buffer that the place owns. An element of a `SmallArray` or an `ArrayN` lives in the copied bytes, so the two walks would write different memory.
The kernel's judgment was already stated, so no paragraph was written: `in_element` says an element store moves no header, and `Alias` says a write through an alias is not a write that the alias or its chain has to end for.
Went: nothing; the rule was unstated in the emitter. Stayed: a whole store into the alias and a `modify` of it, which write the alias's own header.
Lines: `direct.rs` 21,261 to 21,275 (+14: the `names` parameter, the filter, and three doc lines), `kernel.rs` 3,076 to 3,076; one shape file, 8 lines. Refusals: 0 lost / 0 gained. Manifest: 2 rows, below.
Licence:
- `coredrive --ignored`, `e3e6023a` against the tip: taken 20,418 to 20,585 of 21,176, plus 74 judged. 1 byte-identical, 167 run the same, 0 run apart.
- measure-walk instrument, applied uncommitted: 758 refused to 591, and the only body that moved is `num$toFloat`, 167 refused to 167 taken. Census row "names: an Array name with no place": first 244 to 77, met 254 to 87. Row "`let x = read place`": met 233 to 66.
- the 2 moved rows, `jsondecbytes` and `numparse`, were built with `VYRN_WASM_NAMES=1` by `e3e6023a`'s binary and the tip's. `wasm2wat` differs in `$num$toFloat` alone. The 24-byte `memory.copy` of `x.d`'s header into slot +88 (`let mut f = x.d`) is gone, and `f` reads through `x.d`'s address. There are 19 more `local.set`: the rows stage each `&&` and `||` in a local where the arm left it on the stack. The frame stays 176 bytes. Not measured for time.
- the shape `an-alias-of-a-field-written-through`: toFloat's digit doubling, and a String stored into a field of an element. Both binaries print 1182093, with the core walk on and off, under `VYRN_LEAK_CHECK=1`.
- toFloat's callers under `VYRN_LEAK_CHECK=1`, `e3e6023a`'s binary and the tip's: `examples/numparse.vyrn` gave the same 25 lines. A 14-input program covered `parseFloat64`'s and `parseFloat32`'s slow path (17 and 30 digits, 1e400, 4.9e-324, subnormal, ties) and gave the same 14 lines. Both exited 0 with no leak line.
- `residue --ignored`: engine 173 clean / 0 leaking, route 173 clean / 0 leaking, 0 failed.
- `vyrn check` over the corpus roots, `e3e6023a`'s binary against the tip's: the 481 shared roots equal; 1 gained, the shape, accepted.
- pins by `VYRN_PIN=write`: `emitter-census` the mapping 11,682 to 11,696; `emitter-reads` both, for two questions 9,839 to 9,853; `surface` `Type::Array` wasm 40 to 41, all 1,431 to 1,432.
Findings:
- the other 8 field-alias refusals are `@borrow = read s[h].next` (and `.left`, `.right`) in `freelist`, `linkedlist` and `tree`: an enum read out of a `Slots` element, with no store. These are outside this rule.
Left: those 8 bodies. They are blocked by `Fn_::core_place_ty`'s `Elem` arm, which answers `Array`, `ArrayN`, `SmallArray` and `String` but not `Slots`, whose element is a handle lookup. This was read from the code, not traced.
