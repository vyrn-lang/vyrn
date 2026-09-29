#!/bin/sh
# The check-elision gate. Runs the examples, the Benchmarks Game programs and
# the site export three ways: with the proved checks removed (the default), with
# every check kept (VYRN_CHECKS=keep), and under the oracle (VYRN_CHECKS=<file>),
# which counts each check row's runs into OUT/counts.tsv and fails a run where a
# proved check would have trapped. The examples and the benchmarks compare with
# their recorded output in every mode; the export's three output trees must be
# equal. Prints the oracle's rows and runs by verdict.
# Usage, from compiler/ after `cargo build --release -p vyrn-cli`:
#   sh ../scripts/check-elision.sh OUT
set -eu
mkdir -p "$1"
out=$(cd "$1" && pwd)
rm -f "$out/counts.tsv"
suites() {
  cargo nextest run --release -p vyrn-cli --no-fail-fast --status-level fail \
    --run-ignored all -E 'binary(fixtures) | binary(benchgame)' >"$out/suites-$1.log" 2>&1 ||
    { echo "suites under $1: FAILED, see $out/suites-$1.log"; status=1; }
}
export_site() {
  # The export writes only below the working directory, into `out/`.
  (cd .. && rm -rf out && mkdir -p out/docs/std out/guide out/web out/tooling out/explore &&
    compiler/target/release/vyrn run site/export.vyrn out >"$out/export-$1.log" 2>&1 &&
    rm -rf "$out/export-$1" && mv out "$out/export-$1") ||
    { echo "export under $1: FAILED, see $out/export-$1.log"; status=1; }
}
status=0
for mode in elide keep oracle; do
  case $mode in
    elide) unset VYRN_CHECKS ;;
    keep) export VYRN_CHECKS=keep ;;
    oracle) export VYRN_CHECKS="$out/counts.tsv" ;;
  esac
  suites $mode
  export_site $mode
done
unset VYRN_CHECKS
for m in keep oracle; do
  diff -r "$out/export-elide" "$out/export-$m" >/dev/null ||
    { echo "export under $m differs from the elided export"; status=1; }
done
if grep -h "a check the compiler proved failed" "$out"/*.log; then
  status=1
fi
awk -F'\t' '{ k = $2 FS $3 FS $4 FS $5; rows[$6, k] = 1; runs[$6] += $7; if ($7 > 0) ran[$6, k] = 1 }
  END { for (x in rows) { split(x, p, SUBSEP); n[p[1]]++ }
        for (x in ran) { split(x, p, SUBSEP); r[p[1]]++ }
        for (v in n) printf "%s: %d rows, %d of them run, %d runs\n", v, n[v], r[v], runs[v] }' "$out/counts.tsv"
exit $status
