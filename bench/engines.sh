#!/bin/sh
# Time one program on every way this project has of running it, and check that
# they agree.
#
#   sh bench/engines.sh [rounds]
#
# `examples/threeengines.vyrn` is a SHA-1 chain: every round hashes the round
# before it, so nothing can be hoisted or precomputed, and the digest it prints
# is a checksum of the whole run.
#
# WHAT THE ROWS ARE. There is ONE compiled artefact here — the WebAssembly
# module the direct backend emits — and the rows are the three ways it is run.
# The script used to call the first row `interp` and run `vyrn run` in it. The
# interpreter went at RFC-0125 M5 and `vyrn run` has compiled since, so that
# column had been reporting the wasm route under an interpreter's name, and the
# table compared three columns of which two were the same route.
#
#   run      `vyrn run`: the same module, compiled in memory and run in the
#            driver's own wasmtime embedding. This is what CI and every
#            developer actually runs, and its distance from `wasm` is the price
#            of the embedding, not of a second engine
#   wasm     the module written to a file and run by a standalone `wasmtime`
#   native   the same module through wasm2c and clang, as a native binary
#
# So `run` and `wasm` are one route in two hosts, and `native` is the other
# route. Both routes come off the same module, which is why the digests can be
# compared at all.
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
# WHAT IS NOT MEASURED: the build. `vyrn build` runs before the clock starts for
# `wasm` and `native`. `run` compiles inside its own timed process, and its floor
# holds that compile whole — the same compile at one round as at a million — so
# the subtraction takes it out along with process start.
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

# Each row as a command, run as `$(row <rounds>)`.
run() {
  case $1 in
    run)    "$vyrn" run "$src" "$2" ;;
    wasm)   "$wasmtime" "$tmp/three.wasm" "$2" ;;
    native) "$tmp/three" "$2" ;;
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
# rather than assumed, because a speed comparison between rows that disagree is a
# comparison of two different programs. `run` against `wasm` checks the driver's
# embedding; either of them against `native` checks the wasm2c route.
a=$(run run "$rounds")
b=$(run wasm "$rounds")
c=$(run native "$rounds")
[ "$a" = "$b" ] && [ "$b" = "$c" ] || {
  printf 'the three disagree:\n  run    %s\n  wasm   %s\n  native %s\n' "$a" "$b" "$c" >&2
  exit 1
}

echo "$a"
echo "agreed, byte for byte, on all three"
echo
printf '%-8s %11s %9s %11s\n' row "$rounds rounds" 'floor' 'work'
for e in run wasm native; do
  total=$(best "$e" "$rounds")
  floor=$(best "$e" 1)
  printf '%-8s %8s ms %6s ms %8s ms\n' "$e" "$total" "$floor" "$((total - floor))"
done
