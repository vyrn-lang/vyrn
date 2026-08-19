// fannkuch-redux, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/fannkuch.vyrn. Same algorithm, same permutation
// order, byte-identical output.
//
//   $ node fannkuch.js       # the census N, order 7
//   $ node fannkuch.js 11    # the bench N
//
// Structural differences from the Vyrn program:
//
//   * flip mutates in place. Vyrn's `consume`-in / owned-out shape exists because
//     a `read` parameter may not be returned; JS has no such rule.
//   * foldCount takes a fresh copy of the permutation, as the Vyrn program does
//     with `perm1.copy()`. Here it is `perm1.slice()`.
//   * Int32Array here, Array<Int64> there. Every value is a small index.

'use strict';
const fs = require('fs');

/// The census N. The work is `n!`, so this is the number the fixture was written
/// at and not a number to raise casually.
const order = 7;

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

/// Reverse `p[0 ..= k]` in place — the flip the benchmark counts.
function flip(a, k) {
    let i = 0;
    let j = k;
    while (i < j) {
        const t = a[i];
        a[i] = a[j];
        a[j] = t;
        i = i + 1;
        j = j - 1;
    }
    return a;
}

/// How many flips `a` takes to bring a 0 to the front.
function foldCount(a) {
    let flips = 0;
    let k = a[0];
    while (k !== 0) {
        a = flip(a, k);
        flips = flips + 1;
        k = a[0];
    }
    return flips;
}

/// The alternating-sign checksum and the deepest fold, over every permutation of
/// `n` elements in the game's prescribed order.
function fannkuch(n) {
    const perm1 = new Int32Array(n);
    for (let i = 0; i < n; i++) perm1[i] = i;
    const count = new Int32Array(n);
    let maxflips = 0;
    let checksum = 0;
    let permcount = 0;
    let r = n;
    let done = false;
    while (!done) {
        while (r !== 1) {
            count[r - 1] = r;
            r = r - 1;
        }
        const flips = foldCount(perm1.slice());
        if (flips > maxflips) {
            maxflips = flips;
        }
        if (permcount % 2 === 0) {
            checksum = checksum + flips;
        } else {
            checksum = checksum - flips;
        }
        permcount = permcount + 1;
        // The next permutation, by rotating the first `r + 1` entries left and
        // carrying into the next position when a rotation runs out.
        let advanced = false;
        while (!advanced && !done) {
            if (r === n) {
                done = true;
            } else {
                const first = perm1[0];
                let m = 0;
                while (m < r) {
                    perm1[m] = perm1[m + 1];
                    m = m + 1;
                }
                perm1[r] = first;
                count[r] = count[r] - 1;
                if (count[r] > 0) {
                    advanced = true;
                } else {
                    r = r + 1;
                }
            }
        }
    }
    return { checksum: checksum, maxflips: maxflips };
}

const n = process.argv[2] === undefined ? order : Number(process.argv[2]);
const f = fannkuch(n);
emit(`${f.checksum}\n`);
emit(`Pfannkuchen(${n}) = ${f.maxflips}\n`);
flush();
