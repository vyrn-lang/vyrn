// binary-trees, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/binarytrees.vyrn. Same tree counts, same
// checksums, byte-identical output.
//
//   $ node binarytrees.js       # the census N, depth 10
//   $ node binarytrees.js 18    # the bench N
//
// The one structural difference, and it is the point of this row: Vyrn's tree is
// a recursive enum released by the ownership model at the end of its scope. Here
// it is a plain object with `null` for a Leaf, and the garbage collector decides
// when the memory goes back. The allocation count is the same; who frees it is
// not.

'use strict';
const fs = require('fs');

/// The census N — depth 10, the size the fixture was written at.
const order = 10;

let pending = '';
function emit(s) {
    pending += s;
    if (pending.length >= 1 << 20) flush();
}
function flush() {
    if (pending.length === 0) return;
    const buf = Buffer.from(pending, 'latin1');
    pending = '';
    let off = 0;
    while (off < buf.length) {
        try {
            off += fs.writeSync(1, buf, off, buf.length - off);
        } catch (e) {
            if (e.code !== 'EAGAIN') throw e;
        }
    }
}

/// A complete tree of `depth`. `null` is the game's Leaf.
function make(depth) {
    if (depth === 0) {
        return null;
    }
    return { l: make(depth - 1), r: make(depth - 1) };
}

/// The node count — the game's checksum. A Leaf counts as 1.
function check(t) {
    if (t === null) {
        return 1;
    }
    return 1 + check(t.l) + check(t.r);
}

/// `iterations` trees of `depth`, built, checked and released one at a time.
function checkAll(depth, iterations) {
    let sum = 0;
    let i = 0;
    while (i < iterations) {
        sum = sum + check(make(depth));
        i = i + 1;
    }
    return sum;
}

/// How many trees of `depth` the game asks for at this `maxDepth`.
function iterationsFor(depth, maxDepth, minDepth) {
    let n = 1;
    let s = 0;
    while (s < maxDepth - depth + minDepth) {
        n = n * 2;
        s = s + 1;
    }
    return n;
}

/// The whole run at `n`: the stretch tree, one line per even depth, and the
/// long-lived tree that stays alive across all of them.
function run(n) {
    const minDepth = 4;
    let maxDepth = n;
    if (maxDepth < minDepth + 2) {
        maxDepth = minDepth + 2;
    }
    const stretchDepth = maxDepth + 1;

    emit(`stretch tree of depth ${stretchDepth}\t check: ${check(make(stretchDepth))}\n`);
    const longLived = make(maxDepth);

    let depth = minDepth;
    while (depth < stretchDepth) {
        const iterations = iterationsFor(depth, maxDepth, minDepth);
        emit(`${iterations}\t trees of depth ${depth}\t check: ${checkAll(depth, iterations)}\n`);
        depth = depth + 2;
    }
    emit(`long lived tree of depth ${maxDepth}\t check: ${check(longLived)}\n`);
}

const n = process.argv[2] === undefined ? order : Number(process.argv[2]);
run(n);
flush();
