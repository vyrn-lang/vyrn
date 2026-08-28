# Where binary-trees' native seconds live, per node

Depth 21, single thread, 2026-08-26, after RFC-0114 made the releases real
and the depth counter inlined (`e7d4c973`). The wall was ~23 s and is ~19.4;
C's is low single digits. The allocator was measured out of suspicion first
— a size-class pool bought 4 % and was declined, a no-op free LOSES 20 % to
memory pressure — so the gap is the walks themselves. This page is the
anatomy, read off the emitted IR.

## The three walks, per node

**make**: one recursive call per child, one `__vyrn_malloc(24)` per child
box, a 24-byte aggregate store into each. One malloc and one call per node,
amortized.

**check**: load the tag, load BOTH 24-byte payload aggregates out of their
boxes into allocas, one recursive call per child. Two aggregate copies and
two calls per node — the copies are the by-value enum semantics, and LLVM
keeps them because the recursive calls are opaque.

**release**: the expensive one, and the finding. `release(consume self)`
matches, loads both payloads (two more 24-byte copies), and recurses — but
NOT into itself: into `give(l)` and `give(r)`, and each `give` spills its
argument, calls `release`, and then frees the payload boxes. **Two calls
per node where one would do.**

## Why `give` exists at all

Match arms are single expressions and `drop` is a statement, so
`Node(l, r) => drop l + drop r` cannot be written. The example's own comment
says it: "Take ownership of `v` and let the block end release it." The
trampoline is a LANGUAGE ergonomics gap — block-bodied match arms, or `drop`
as an expression, would let a self-referring release recurse directly and
halve the release walk's call count.

## What this page is not

Not a work item for the allocator (measured, declined) and not one for the
optimizer's alias analysis (measured, wash). The remaining gap is per-node
call count and enum-payload copies — a lowering-and-surface question, and
the give-trampoline above is its most concrete, bounded piece.
