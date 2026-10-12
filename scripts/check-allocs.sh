#!/bin/sh
# Compares the allocations of two `vyrn` binaries built with `--features allocs`
# over the same roots and prints each root's total and the phases that moved.
# Exits 1 when HEAD's total for any root exceeds BASE's by more than THRESHOLD percent.
# The roots are the tree that holds this script. Each binary reads the `std/` beside
# its own `target/`: `std/runtime` changes with the emitter that calls it.
# Usage: scripts/check-allocs.sh BASE_VYRN HEAD_VYRN [THRESHOLD]
# THRESHOLD defaults to $CHECK_ALLOCS_THRESHOLD or 0.1.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
py=$(command -v python3 || command -v python)
exec "$py" - "$1" "$2" "${3:-${CHECK_ALLOCS_THRESHOLD:-0.1}}" "$root" <<'PY'
import glob, os, subprocess, sys, tempfile

base, head, limit = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2]), float(sys.argv[3])
tree = sys.argv[4]
out = os.path.join(tempfile.mkdtemp(), "out.wasm")
env = {k: v for k, v in os.environ.items() if k not in ("VYRN_STD", "VYRN_WEB")}
env.update(VYRN_BUILD_PROFILE="allocs", VYRN_THREADS="1")

# Two builds of the same source differ by up to 10 allocations in a phase (hash
# order and path lengths), so a smaller move is neither listed nor a failure.
NOISE = 16

# A build of site/export.vyrn, then the checker alone on the programs below.
roots = [("build", "site/export.vyrn"), ("check", "site/export.vyrn")] + [
    ("check", r) for r in sorted(
        os.path.relpath(f, tree).replace(os.sep, "/") for f in glob.glob(f"{tree}/examples/*.vyrn"))]


def phases(binary, cache, verb, root):
    """Runs the binary twice and returns the second run's {phase: allocations}, or None on failure."""
    args = [binary, verb, root] if verb == "check" else [binary, "build", "--target", "wasm", root, "-o", out]
    for _ in range(2):
        # The first run fills the generator cache; its counts are not compared.
        p = subprocess.run(args, cwd=tree, env={**env, "VYRN_GEN_CACHE_DIR": cache},
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        if p.returncode:
            return None
    if "alloc phase" not in p.stderr:
        sys.exit(f"{binary} printed no allocation table: build it with --features allocs")
    rows = {}
    for line in p.stderr.split("alloc phase", 1)[1].splitlines()[1:]:
        cells = line.split()
        if len(cells) > 4:
            rows[" ".join(cells[:-4])] = int(cells[-4])
    return rows


caches = {b: tempfile.mkdtemp() for b in (base, head)}
failed = False
skipped = []
for verb, root in roots:
    got = {b: phases(b, caches[b], verb, root) for b in (base, head)}
    name = f"{verb} {root}"
    if None in got.values():
        # The site is the largest root. Both binaries rejecting it means the run is broken, not
        # the root; only base rejecting it means head added what the site uses.
        if root.startswith("site/") and got[head] is None:
            sys.exit(f"{name}: head rejects it")
        skipped.append(name)
        continue
    a, b = got[base], got[head]
    ta, tb = sum(a.values()), sum(b.values())
    pct = (tb - ta) * 100 / ta
    over = tb - ta > max(ta * limit / 100, NOISE)
    failed |= over
    moved = [f"  {p}: only in {'head' if p in b else 'base'}, {a.get(p, b.get(p))}" if p not in a or p not in b
             else f"  {p}: {a[p]} -> {b[p]}" for p in sorted(a.keys() | b.keys()) if abs(a.get(p, 0) - b.get(p, 0)) > NOISE]
    # An example that did not move prints nothing; the two big roots always print.
    if moved or root.startswith("site/"):
        print("\n".join([f"{name}: base {ta}, head {tb}, {pct:+.3f}%{' FAIL' if over else ''}"] + moved))
print(f"compared {len(roots) - len(skipped)} roots, skipped {len(skipped)} that a binary rejects: {', '.join(skipped)}")
print(f"threshold {limit}%: {'FAIL' if failed else 'pass'}")
sys.exit(failed)
PY
