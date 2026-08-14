#!/bin/sh
# Time one program on all three engines, and check that they agree.
#
#   sh bench/engines.sh [rounds]
#
# `examples/threeengines.vyrn` is a SHA-1 chain: every round hashes the round
# before it, so nothing can be hoisted or precomputed, and the digest it prints
# is a checksum of the whole run. This script builds that file twice — once to a
# native binary, once to a WebAssembly module — runs it three ways, refuses to
# print a single number unless all three printed the same bytes, and reports the
# fastest of three runs per engine.
#
# WHAT IS MEASURED. Three columns, and the third is the one to quote:
#
#   total    wall clock of the whole process at <rounds> rounds
#   floor    the same process at ONE round — process start, and for wasm the
#            runtime's compilation of the module, plus whatever this shell costs
#            to fork a child and read a clock
#   work     total - floor, which is the rounds themselves
#
# The floor is not noise to be hidden: on a Windows shell it is 30 ms and on
# Linux it is 2 ms, and a comparison that left it in would be reporting the shell
# it ran under. It is printed rather than absorbed so the subtraction is visible.
#
# WHAT IS NOT MEASURED: the build. `vyrn build` runs before the clock starts.
#
# Environment:
#   VYRN            the compiler to use   (default compiler/target/release/vyrn)
#   VYRN_WASMTIME   the wasm runtime      (default `wasmtime` on PATH)

set -eu

rounds=${1:-100000}
repeats=3
root=$(cd "$(dirname "$0")/.." && pwd)
vyrn=${VYRN:-$root/compiler/target/release/vyrn}
wasmtime=${VYRN_WASMTIME:-wasmtime}
src=$root/examples/threeengines.vyrn

[ -x "$vyrn" ] || command -v "$vyrn" >/dev/null 2>&1 ||
  { echo "no vyrn at $vyrn — build it: (cd compiler && cargo build --release -p vyrn-cli)" >&2; exit 1; }
command -v "$wasmtime" >/dev/null 2>&1 || [ -x "$wasmtime" ] ||
  { echo "no wasmtime — set VYRN_WASMTIME to one" >&2; exit 1; }
case $(date +%s%N) in
  *N) echo "this needs a date(1) that understands %N (GNU coreutils)" >&2; exit 1 ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/vyrn-engines.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

"$vyrn" build "$src" -o "$tmp/three" >/dev/null
"$vyrn" build "$src" --target wasm -o "$tmp/three.wasm" >/dev/null

# Each engine as a command, run as `$(engine <rounds>)`.
run() {
  case $1 in
    interp) "$vyrn" run "$src" "$2" ;;
    native) "$tmp/three" "$2" ;;
    wasm)   "$wasmtime" "$tmp/three.wasm" "$2" ;;
  esac
}

# Milliseconds for the fastest of $repeats runs. A slower run is contention, a
# faster one is not possible, so the minimum is the least noisy estimate here.
best() {
  best=
  i=0
  while [ "$i" -lt "$repeats" ]; do
    t0=$(date +%s%N)
    run "$1" "$2" >/dev/null
    t1=$(date +%s%N)
    ms=$(( (t1 - t0) / 1000000 ))
    [ -n "$best" ] && [ "$best" -le "$ms" ] || best=$ms
    i=$((i + 1))
  done
  echo "$best"
}

# All three must print the same bytes. This is the parity claim, checked here
# rather than assumed, because a speed comparison between engines that disagree
# is a comparison of two different programs.
a=$(run interp "$rounds")
b=$(run native "$rounds")
c=$(run wasm "$rounds")
[ "$a" = "$b" ] && [ "$b" = "$c" ] || {
  printf 'the three engines disagree:\n  interp %s\n  native %s\n  wasm   %s\n' "$a" "$b" "$c" >&2
  exit 1
}

echo "$a"
echo "agreed, byte for byte, on all three"
echo
printf '%-8s %11s %9s %11s\n' engine "$rounds rounds" 'floor' 'work'
for e in interp native wasm; do
  total=$(best "$e" "$rounds")
  floor=$(best "$e" 1)
  printf '%-8s %8s ms %6s ms %8s ms\n' "$e" "$total" "$floor" "$((total - floor))"
done
