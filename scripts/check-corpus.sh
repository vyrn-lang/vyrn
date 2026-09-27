#!/bin/sh
# Runs `vyrn check` over the corpus roots of TREE with TREE's release
# binary, from TREE's root, and writes each root's stdout, stderr and exit code
# under OUT. Two trees' OUT directories compare with `diff -r`.
# Usage: scripts/check-corpus.sh TREE OUT
set -eu
tree=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
cd "$tree"
find examples std site compiler/vyrn-cli/tests -name '*.vyrn' -not -path '*/target/*' | sort |
  xargs -P "$(nproc)" -I{} sh -c '
    f="$1"; o="$2/$(echo "$f" | tr / _)"
    compiler/target/release/vyrn check "$f" >"$o.out" 2>"$o.err"
    echo $? >"$o.code"' _ {} "$out"
echo "roots $(ls "$out"/*.code | wc -l) accepted $(grep -l '^0$' "$out"/*.code | wc -l) refused $(grep -L '^0$' "$out"/*.code | wc -l)"
