// k-nucleotide, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/knucleotide.vyrn. Same tables, same ordering
// (count descending, ties by fragment ascending), byte-identical output.
//
//   $ node knucleotide.js < ../../fasta-1000.expected
//
// Structural differences from the Vyrn program:
//
//   * a k-mer key is `seq.slice(i, i + k)` on a JavaScript string. The Vyrn
//     program builds an Array<UInt8> window and pays one `stringFromBytes` per
//     position — a UTF-8 validation over ASCII. This leg does not do that.
//   * one `Array.prototype.sort` with a comparator replaces Vyrn's two passes (a
//     hand-written insertion sort by fragment, then a stable `sortBy` on the
//     negated count), because Vyrn's sortBy takes an Int64 key and no comparator
//     crosses the API. Same total order, one pass instead of two.
//   * stdin is read once rather than a line at a time, so this leg holds the
//     whole input; the Vyrn program holds only the THREE sequence.
//   * toUpperCase() is Unicode-aware where std/strings toUpper is ASCII. The
//     input is ASCII, so the two agree here.

'use strict';
const fs = require('fs');

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

/// The five fragments the game asks for by name, after the two frequency tables.
function namedFragments() {
    return ['GGT', 'GGTA', 'GGTATT', 'GGTATTTTAATT', 'GGTATTTTAATTTATAGT'];
}

/// The THREE sequence from FASTA on stdin: uppercased, with the newlines and the
/// other two sequences left out.
function thirdSequence() {
    const parts = [];
    let inThird = false;
    for (const l of fs.readFileSync(0, 'latin1').split('\n')) {
        if (l.startsWith('>')) {
            inThird = l.startsWith('>THREE');
        } else if (inThird) {
            parts.push(l.toUpperCase());
        }
    }
    return parts.join('');
}

/// Every window of width `k`, counted.
function countKmers(seq, k) {
    const m = new Map();
    let i = 0;
    while (i + k <= seq.length) {
        const key = seq.slice(i, i + k);
        const seen = m.get(key);
        m.set(key, seen === undefined ? 1 : seen + 1);
        i = i + 1;
    }
    return m;
}

/// `x` at three decimal places — the game asks for three.
function fixed3(x) {
    const scaled = Math.trunc(x * 1000.0 + 0.5);
    const whole = Math.trunc(scaled / 1000);
    const frac = String(scaled % 1000).padStart(3, '0');
    return `${whole}.${frac}`;
}

/// Count descending, ties by fragment ascending.
function ranked(m) {
    const es = [];
    for (const [frag, count] of m) {
        es.push({ frag: frag, count: count });
    }
    es.sort((a, b) => {
        if (a.count !== b.count) return b.count - a.count;
        return a.frag < b.frag ? -1 : a.frag > b.frag ? 1 : 0;
    });
    return es;
}

/// One frequency table: every fragment of width `k` as a percentage of the
/// windows there are, then a blank line.
function report(seq, k) {
    const m = countKmers(seq, k);
    const total = seq.length - k + 1;
    for (const e of ranked(m)) {
        emit(`${e.frag} ${fixed3(100.0 * e.count / total)}\n`);
    }
    emit('\n');
}

/// The count of one named fragment.
///
/// It builds the whole table for that width and looks the fragment up, rather
/// than scanning for the one string — because the table IS the benchmark.
function countOf(seq, frag) {
    const m = countKmers(seq, frag.length);
    const c = m.get(frag);
    return c === undefined ? 0 : c;
}

const seq = thirdSequence();
report(seq, 1);
report(seq, 2);
for (const frag of namedFragments()) {
    emit(`${countOf(seq, frag)}\t${frag}\n`);
}
flush();
