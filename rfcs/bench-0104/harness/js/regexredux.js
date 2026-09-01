// regex-redux, the JavaScript leg of RFC-0104 M2.
//
// The specification is examples/regexredux.vyrn: read FASTA on stdin, strip
// the descriptions and the linefeeds, count nine patterns, run five
// substitutions each over the output of the last, then print the counts and
// three lengths. Same order, same bytes.
//
//   $ node regexredux.js < rfcs/bench-0104/fasta-1000.expected
//
// This leg uses the engine's own RegExp — the game's JS entries do, and the
// point of the row is the regex engine each language ships, exactly as the
// Vyrn leg's point is std/regex. Every pattern is a literal of this file.

"use strict";

const fs = require("fs");

function main() {
    const input = fs.readFileSync(0, "latin1");
    const inputLength = input.length;

    // Remove the FASTA descriptions and every linefeed. One pattern, not two,
    // because a description runs to the end of its line and the alternation
    // takes whichever comes first.
    const sequence = input.replace(/>[^\n]*\n|\n/g, "");
    const cleanLength = sequence.length;

    const variants = [
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct",
    ];
    const lines = [];
    for (const pattern of variants) {
        const m = sequence.match(new RegExp(pattern, "g"));
        lines.push(pattern + " " + (m === null ? 0 : m.length));
    }

    // The five rewrites, each over the result of the last.
    const substitutions = [
        [/tHa[Nt]/g, "<4>"],
        [/aND|caN|Ha[DS]|WaS/g, "<3>"],
        [/a[NSt]|BY/g, "<2>"],
        [/<[^>]*>/g, "|"],
        [/\|[^|][^|]*\|/g, "-"],
    ];
    let rewritten = sequence;
    for (const [re, to] of substitutions) {
        rewritten = rewritten.replace(re, to);
    }

    lines.push("");
    lines.push(String(inputLength));
    lines.push(String(cleanLength));
    lines.push(String(rewritten.length));
    process.stdout.write(lines.join("\n") + "\n");
}

main();
