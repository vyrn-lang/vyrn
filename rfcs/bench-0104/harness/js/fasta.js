// fasta, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/fasta.vyrn. Same generator, same weights, same
// single shared random stream across TWO and THREE, byte-identical output.
//
//   $ node fasta.js            # the census N, 1000
//   $ node fasta.js 2000000    # the bench N
//
// Structural differences from the Vyrn program:
//
//   * a line is built as a JavaScript string. The Vyrn program builds an
//     Array<UInt8> and pays one `stringFromBytes` per line — a UTF-8 validation
//     over bases already known to be ASCII. That validation has no counterpart
//     here, so this leg does strictly less work per line.
//   * output is buffered and written to fd 1 in megabyte chunks rather than one
//     `print` per line.

'use strict';
const fs = require('fs');

/// The census N. The game's own N is 25,000,000, which is 250 MB of output.
const order = 1000;

/// The 287-base repeat unit of the ONE sequence, as the game publishes it.
const alu =
    'GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA';

/// The generator's state. It is module state and not a parameter because the
/// game specifies one stream shared by both random sequences: THREE continues
/// where TWO left off.
let seed = 42;

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

/// The game's linear congruential generator.
function nextRandom(max) {
    seed = (seed * 3877 + 29573) % 139968;
    return max * seed / 139968.0;
}

/// The running totals of `ws` — the form the weighted pick reads.
function cumulative(ws) {
    const out = [];
    let p = 0.0;
    for (const w of ws) {
        p = p + w;
        out.push(p);
    }
    return out;
}

/// The first symbol whose running total is above the next random draw. A linear
/// scan, as the game specifies.
function pick(syms, cum) {
    const r = nextRandom(1.0);
    let i = 0;
    while (i < cum.length) {
        if (cum[i] > r) {
            return syms[i];
        }
        i = i + 1;
    }
    return syms[syms.length - 1];
}

/// The width of an output line.
function lineWidth(todo) {
    if (todo < 60) {
        return todo;
    }
    return 60;
}

/// `count` bases taken from `src` cyclically — the ONE sequence.
function repeatFasta(header, src, count) {
    emit(header + '\n');
    let k = 0;
    let todo = count;
    while (todo > 0) {
        const m = lineWidth(todo);
        let w = '';
        let i = 0;
        while (i < m) {
            w += src[k];
            k = (k + 1) % src.length;
            i = i + 1;
        }
        emit(w + '\n');
        todo = todo - m;
    }
}

/// `count` bases drawn from the weighted table — the TWO and THREE sequences.
function randomFasta(header, syms, cum, count) {
    emit(header + '\n');
    let todo = count;
    while (todo > 0) {
        const m = lineWidth(todo);
        let w = '';
        let i = 0;
        while (i < m) {
            w += pick(syms, cum);
            i = i + 1;
        }
        emit(w + '\n');
        todo = todo - m;
    }
}

/// The IUB ambiguity codes and their published weights.
function iubWeights() {
    return [
        0.27, 0.12, 0.12, 0.27, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02,
    ];
}

/// The Homo sapiens base frequencies, as the game publishes them.
function humanWeights() {
    return [0.3029549426680, 0.1979883004921, 0.1975473066391, 0.3015094502008];
}

/// The whole run at `n` — three sequences in the order the generator's single
/// stream requires.
function run(n) {
    repeatFasta('>ONE Homo sapiens alu', alu, n * 2);
    randomFasta('>TWO IUB ambiguity codes', 'acgtBDHKMNRSVWY', cumulative(iubWeights()), n * 3);
    randomFasta('>THREE Homo sapiens frequency', 'acgt', cumulative(humanWeights()), n * 5);
}

const n = process.argv[2] === undefined ? order : Number(process.argv[2]);
run(n);
flush();
