// spectral-norm, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/spectralnorm.vyrn. Same algorithm, same
// expression order, byte-identical output.
//
//   $ node spectralnorm.js         # the census N, order 100
//   $ node spectralnorm.js 3000    # the bench N
//
// Structural differences from the Vyrn program:
//
//   * Math.sqrt is the scalar square root Vyrn reaches through F64x2.sqrt(..).lane(0).
//   * multiplyAv / multiplyAtv write into the destination in place. Vyrn's
//     ownership model makes the destination `consume`-in / owned-out; JS has no
//     such rule, so no move-in-move-out shape is written.
//   * Float64Array here, Array<Float64> there — the same flat run of doubles.

'use strict';
const fs = require('fs');

/// The census N — a 100x100 window of the matrix, the size the fixture was
/// written at.
const order = 100;

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

/// `v` at nine decimal places, the format the game prints.
function fixed9(v) {
    let a = v;
    let sign = '';
    if (a < 0.0) {
        sign = '-';
        a = 0.0 - a;
    }
    const scaled = Math.trunc(a * 1000000000.0 + 0.5);
    const whole = Math.trunc(scaled / 1000000000);
    const frac = String(scaled % 1000000000).padStart(9, '0');
    return `${sign}${whole}.${frac}`;
}

/// `A[i][j]` — the matrix is a formula, so no matrix is ever built.
function cell(i, j) {
    return 1.0 / (Math.trunc((i + j) * (i + j + 1) / 2) + i + 1);
}

/// `out = A v`.
function multiplyAv(n, v, w) {
    let i = 0;
    while (i < n) {
        let sum = 0.0;
        let j = 0;
        while (j < n) {
            sum = sum + cell(i, j) * v[j];
            j = j + 1;
        }
        w[i] = sum;
        i = i + 1;
    }
    return w;
}

/// `out = A-transpose v`, which is multiplyAv with the indices of cell swapped.
function multiplyAtv(n, v, w) {
    let i = 0;
    while (i < n) {
        let sum = 0.0;
        let j = 0;
        while (j < n) {
            sum = sum + cell(j, i) * v[j];
            j = j + 1;
        }
        w[i] = sum;
        i = i + 1;
    }
    return w;
}

/// Ten rounds of `u = A-transpose A u`, then the Rayleigh quotient's square root.
function spectralNorm(n) {
    let u = new Float64Array(n).fill(1.0);
    let v = new Float64Array(n).fill(0.0);
    let w = new Float64Array(n).fill(0.0);
    let round = 0;
    while (round < 10) {
        w = multiplyAv(n, u, w);
        v = multiplyAtv(n, w, v);
        w = multiplyAv(n, v, w);
        u = multiplyAtv(n, w, u);
        round = round + 1;
    }
    let vbv = 0.0;
    let vv = 0.0;
    let k = 0;
    while (k < n) {
        vbv = vbv + u[k] * v[k];
        vv = vv + v[k] * v[k];
        k = k + 1;
    }
    return Math.sqrt(vbv / vv);
}

const n = process.argv[2] === undefined ? order : Number(process.argv[2]);
emit(fixed9(spectralNorm(n)) + '\n');
flush();
