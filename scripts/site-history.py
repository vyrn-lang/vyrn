#!/usr/bin/env python3
"""The repository's own history, as one small JSON file for the site to draw.

    python scripts/site-history.py > site/data/history.json

WHY A SCRIPT AND NOT THE EXPORT. `site/export.vyrn` reads the working tree with
RFC-0014's `readFile` and `listDir`, and that is all it can do: there is no git
in Vyrn, and there is not going to be one for the sake of a chart. The site
workflow therefore runs this before the export, from a checkout with
`fetch-depth: 0` — a shallow clone has no history to count.

Everything here is one `git log` walk or one `git tag` listing, standard library
only, so a local build runs the same command and gets the same file.

WHAT IS IN IT, and why each one:

  days      One entry per calendar day from the first commit to the last, dense,
            including the quiet ones: a pulse with the empty days dropped is not
            a pulse. `c` is commits, `p` is merged pull requests — a commit whose
            subject ends in `(#NNN)`, which is this repository's own merge
            convention (219 of 896 at the time of writing).
  rfcs      Every design record and the day it first appeared, from ONE
            `--diff-filter=A` walk rather than 104 `--follow` calls.
  releases  Every tag, with the day it was made.
  tests     How many `test` blocks the tracked Vyrn source declares, counted the
            way CI's own floor counts them (`^test "`).

The site refuses to export without this file, by name and with this command in
the message, rather than drawing an empty chart.
"""

import json
import re
import subprocess
import sys
from datetime import date, timedelta

PR = re.compile(r"\(#\d+\)\s*$")


def git(*args):
    """One git command, as text. A failure is fatal: a chart drawn from half a
    history is worse than no chart."""
    out = subprocess.run(["git", *args], capture_output=True, text=True, encoding="utf-8")
    if out.returncode != 0:
        sys.exit(f"site-history: git {' '.join(args)} failed: {out.stderr.strip()}")
    return out.stdout


def commits():
    """Every commit, as (day, whether it is a merged pull request)."""
    rows = []
    for line in git("log", "--date=short", "--format=%ad\t%s").splitlines():
        if "\t" not in line:
            continue
        day, subject = line.split("\t", 1)
        rows.append((day, bool(PR.search(subject))))
    return rows


def days(rows):
    """One entry per day from the first commit to the last, quiet days included."""
    if not rows:
        return []
    counted = {}
    for day, is_pr in rows:
        c, p = counted.get(day, (0, 0))
        counted[day] = (c + 1, p + (1 if is_pr else 0))
    first = date.fromisoformat(min(counted))
    last = date.fromisoformat(max(counted))
    out = []
    at = first
    while at <= last:
        c, p = counted.get(at.isoformat(), (0, 0))
        out.append({"d": at.isoformat(), "c": c, "p": p})
        at += timedelta(days=1)
    return out


def rfcs():
    """Every `rfcs/RFC-NNNN-*.md` and the day it was added, from one walk."""
    seen = {}
    day = ""
    for line in git(
        "log", "--reverse", "--date=short", "--diff-filter=A", "--name-status", "--format=%ad", "--", "rfcs/"
    ).splitlines():
        line = line.rstrip()
        if not line:
            continue
        if line[0] == "A" and "\t" in line:
            path = line.split("\t", 1)[1]
            m = re.fullmatch(r"rfcs/RFC-(\d{4})-.*\.md", path)
            if m and m.group(1) not in seen:
                seen[m.group(1)] = day
        elif re.fullmatch(r"\d{4}-\d{2}-\d{2}", line):
            day = line
    return [{"n": int(n), "d": d} for n, d in sorted(seen.items())]


def releases():
    """Every tag, oldest first."""
    out = []
    for line in git("tag", "-l", "--format=%(refname:short)\t%(creatordate:short)").splitlines():
        if "\t" in line:
            tag, when = line.split("\t", 1)
            out.append({"t": tag, "d": when})
    return sorted(out, key=lambda r: r["d"])


def tests():
    """How many `test` blocks the tracked Vyrn source declares."""
    blocks = 0
    files = 0
    for path in git("ls-files", "*.vyrn").splitlines():
        try:
            with open(path, encoding="utf-8") as f:
                n = sum(1 for line in f if line.startswith('test "'))
        except OSError:
            continue
        if n:
            blocks += n
            files += 1
    return {"blocks": blocks, "files": files}


def main():
    rows = commits()
    body = {
        "days": days(rows),
        "rfcs": rfcs(),
        "releases": releases(),
        "tests": tests(),
        "commits": len(rows),
        "prs": sum(1 for _, is_pr in rows if is_pr),
    }
    json.dump(body, sys.stdout, separators=(",", ":"), sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
