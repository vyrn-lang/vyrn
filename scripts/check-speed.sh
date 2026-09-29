#!/bin/sh
# Times `vyrn check` of two trees' release binaries over the same roots and
# prints microseconds per source line for each. Exits 1 when HEAD's total is
# more than THRESHOLD times BASE's.
# Usage: scripts/check-speed.sh BASE_TREE HEAD_TREE [RUNS] [THRESHOLD]
# RUNS defaults to 5 and THRESHOLD to $CHECK_SPEED_THRESHOLD or 1.10.
set -eu
py=$(command -v python3 || command -v python)
exec "$py" - "$1" "$2" "${3:-5}" "${4:-${CHECK_SPEED_THRESHOLD:-1.10}}" <<'PY'
import glob, os, re, subprocess, sys, time

base, head, runs, limit = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4])
NULL = subprocess.DEVNULL


def check(tree, root, *flags):
    return subprocess.run([f"{tree}/compiler/target/release/vyrn", "check", *flags, root],
                          cwd=tree, stdout=NULL, stderr=NULL if not flags else subprocess.PIPE, text=True)


def timed(tree, root):
    t = time.perf_counter()
    check(tree, root)
    return time.perf_counter() - t


# The warm-up run fills the derive cache (about 5 s for site/export.vyrn), so it
# is never timed. A root is kept only if both trees accept it.
candidates = ["site/export.vyrn"] + sorted(
    os.path.relpath(f, head).replace(os.sep, "/") for f in glob.glob(f"{head}/examples/*.vyrn"))
roots = [r for r in candidates
         if os.path.exists(f"{base}/{r}") and check(base, r).returncode == 0 == check(head, r).returncode]

best = {base: {}, head: {}}
for i in range(runs):
    # Alternate who goes first, so neither tree always pays for the other's cache misses.
    for r in roots:
        for tree in (base, head) if i % 2 == 0 else (head, base):
            best[tree][r] = min(best[tree].get(r, 1e9), timed(tree, r))

# The line count is the head's: an older binary has no `lines read` row.
lines = 0
for r in roots:
    err = check(head, r, "--profile").stderr
    lines += int(re.search(r"^lines read\s+(\d+)$", err, re.M).group(1))

total = {t: sum(best[t].values()) for t in (base, head)}
for name, t in (("base", base), ("head", head)):
    print(f"{name}: {total[t] * 1e3:9.1f} ms over {len(roots)} roots, {lines} lines, {total[t] * 1e6 / lines:7.3f} us/line")
ratio = total[head] / total[base]
print(f"head/base: {ratio:.3f} (threshold {limit:.2f}, best of {runs})")
worst = sorted(roots, key=lambda r: best[head][r] / best[base][r], reverse=True)[:5]
print("slowest roots, head/base:", ", ".join(f"{r} {best[head][r] / best[base][r]:.2f}" for r in worst))
sys.exit(ratio > limit)
PY
