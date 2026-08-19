// pidigits, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/pidigits.vyrn: the bounded spigot of Rabinowitz
// and Wagon, `guard = 10`, the nines run held back, digits ten to a line.
// Byte-identical output.
//
//   $ node pidigits.js         # the census N, 27 digits
//   $ node pidigits.js 6000    # the bench N
//
// The Vyrn program uses this spigot because Int64 is the widest integer it has
// and nothing in std exports a big one. JavaScript is in the same position: a
// number is a double, exact to 2^53, and BigInt is deliberately not used here so
// that the two legs run the same algorithm rather than two different ones. The
// spigot's widest intermediate is about (10n/3)^2, which at 6000 digits is about
// 4e8 — far inside 2^53.
//
// Structural difference: the column array is an Int32Array here (every entry is
// below 2k-1) and an Array<Int64> there. Same values.

'use strict';
const fs = require('fs');

/// The census N — 27 digits, the size the fixture was written at.
const order = 27;

/// Extra digits computed and thrown away. A bounded spigot's last few columns are
/// the ones that can be wrong, so the bound is set past the answer.
const guard = 10;

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

/// The first `n` digits of pi, one per entry.
function piDigits(n) {
    const total = n + guard;
    const len = Math.trunc(10 * total / 3) + 1;
    const a = new Int32Array(len).fill(2);
    const out = [];
    let nines = 0;
    let predigit = 0;
    let j = 0;
    while (j <= total) {
        let q = 0;
        let k = len;
        while (k > 0) {
            const x = 10 * a[k - 1] + q * k;
            a[k - 1] = x % (2 * k - 1);
            q = Math.trunc(x / (2 * k - 1));
            k = k - 1;
        }
        a[0] = q % 10;
        q = Math.trunc(q / 10);
        // A run of nines is held back: the carry out of the next column can turn
        // all of them into zeroes and bump the digit before them.
        if (q === 9) {
            nines = nines + 1;
        } else if (q === 10) {
            out.push(predigit + 1);
            let z = 0;
            while (z < nines) {
                out.push(0);
                z = z + 1;
            }
            predigit = 0;
            nines = 0;
        } else {
            out.push(predigit);
            predigit = q;
            let z = 0;
            while (z < nines) {
                out.push(9);
                z = z + 1;
            }
            nines = 0;
        }
        j = j + 1;
    }
    // Entry 0 is the zero that precedes the 3; the guard digits fall off the end.
    const digits = [];
    let d = 1;
    while (d <= n) {
        digits.push(out[d]);
        d = d + 1;
    }
    return digits;
}

/// The digits, ten to a line, each line tagged with how many have been printed.
/// A short last line is padded to ten so the tags stay in one column.
function run(n) {
    const digits = piDigits(n);
    let line = '';
    let i = 0;
    while (i < n) {
        line = `${line}${digits[i]}`;
        if ((i + 1) % 10 === 0) {
            emit(`${line}\t:${i + 1}\n`);
            line = '';
        }
        i = i + 1;
    }
    if (line.length > 0) {
        while (line.length < 10) {
            line = `${line} `;
        }
        emit(`${line}\t:${n}\n`);
    }
}

const n = process.argv[2] === undefined ? order : Number(process.argv[2]);
run(n);
flush();
