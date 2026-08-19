// reverse-complement, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/revcomp.vyrn. Same 256-entry complement table,
// same 60-column wrap, byte-identical output.
//
//   $ node revcomp.js < ../../fasta-1000.expected
//
// Structural differences from the Vyrn program:
//
//   * stdin is read once with fs.readFileSync(0) and split on '\n'. The Vyrn
//     program pulls a line at a time from `readLine()`, so it never holds the
//     whole input; this leg does. The transformation itself is identical, and
//     both hold one whole sequence at a time regardless.
//   * a sequence is a JavaScript string, not an Array<UInt8>, so there is no
//     `bytes(l)` allocation per input line and no `stringFromBytes` per output
//     line. That UTF-8 validation is work this leg does not do.

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

/// The IUB complement table, 256 entries, indexed by the input byte. Bases that
/// are not IUB codes map to themselves, which is what makes the lookup total.
function complementTable() {
    const t = [];
    let i = 0;
    while (i < 256) {
        t.push(String.fromCharCode(i));
        i = i + 1;
    }
    const from = 'ACBDGHKMNSRUTWVYacbdghkmnsrutwvy';
    const to = 'TGVHCDMKNSYAAWBRTGVHCDMKNSYAAWBR';
    let k = 0;
    while (k < from.length) {
        t[from.charCodeAt(k)] = to[k];
        k = k + 1;
    }
    return t;
}

/// `seq` backwards, every base complemented, printed 60 columns to a line with a
/// short last line if it does not divide.
function writeReverseComplement(seq, table) {
    let w = '';
    let i = seq.length - 1;
    while (i >= 0) {
        w += table[seq.charCodeAt(i)];
        if (w.length === 60) {
            emit(w + '\n');
            w = '';
        }
        i = i - 1;
    }
    if (w.length > 0) {
        emit(w + '\n');
    }
}

/// The whole run: one header and one sequence at a time, transformed and written
/// as soon as its last line arrives.
function run() {
    const table = complementTable();
    const lines = fs.readFileSync(0, 'latin1').split('\n');
    let header = '';
    let parts = [];
    for (const l of lines) {
        if (l.length > 0 && l[0] === '>') {
            if (header.length > 0) {
                emit(header + '\n');
                writeReverseComplement(parts.join(''), table);
                parts = [];
            }
            header = l;
        } else {
            parts.push(l);
        }
    }
    if (header.length > 0) {
        emit(header + '\n');
        writeReverseComplement(parts.join(''), table);
    }
}

run();
flush();
