#!/usr/bin/env python3
"""Writes `results/history.json`: Vyrn's p50 per program in every record git holds.

    python history.py

It walks the history of HEAD for every version of every `results/*.json`
record and keeps the last version of each file. Records are ordered by date,
then file name. It writes three lists, one entry per record: the file, the
date and the Vyrn commit timed. Then, per program and Vyrn contestant, one
list of p50 in seconds, null where the record has no such contestant.

A record from before p50 holds `median_s`. For an odd run count that is the
same statistic; for an even one it is the mean of the two middle runs. The
list takes it as p50. A record that is not committed is not read, so the
commit that adds a record runs this script again.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

RESULTS = Path(__file__).resolve().parent.parent / "results"
OUT = RESULTS / "history.json"
VYRN = ("vyrn-native", "vyrn-wasm", "vyrn-wasm-opt")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=RESULTS, capture_output=True, text=True, check=True).stdout


def main() -> int:
    latest: dict[str, str] = {}
    log = git("log", "--format=%H", "--name-only", "--diff-filter=AM", "--", ".")
    commit = ""
    for line in log.splitlines():
        if len(line) == 40 and "/" not in line:
            commit = line
        elif line.endswith(".json") and not line.endswith(OUT.name):
            latest.setdefault(line, commit)  # the log is newest first
    records = sorted(
        ((Path(path).stem, json.loads(git("show", f"{c}:{path}"))) for path, c in latest.items()),
        key=lambda r: (r[1]["date"], r[0]),
    )
    p50: dict[str, dict[str, list]] = {}
    for i, (_, rec) in enumerate(records):
        for prog, cell in rec["results"].items():
            for c in VYRN:
                m = cell["contestants"].get(c)
                v = m.get("p50_s", m.get("median_s")) if m else None
                p50.setdefault(prog, {k: [None] * len(records) for k in VYRN})[c][i] = None if v is None else round(v, 6)
    # One list per line, so the site can cut a row out by its indentation.
    lines = [
        "{",
        f'  "files": {json.dumps([n + ".json" for n, _ in records])},',
        f'  "dates": {json.dumps([r["date"] for _, r in records])},',
        f'  "commits": {json.dumps([r["environment"]["vyrn"]["commit"] for _, r in records])},',
        '  "p50_s": {',
    ]
    for j, (prog, row) in enumerate(p50.items()):
        lines.append(f'    "{prog}": {{')
        lines += [f'      "{c}": {json.dumps(row[c])}' + ("," if k < len(VYRN) - 1 else "") for k, c in enumerate(VYRN)]
        lines.append("    }" + ("," if j < len(p50) - 1 else ""))
    lines += ["  }", "}"]
    OUT.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    json.loads(OUT.read_text())
    print(f"wrote {OUT}: {len(records)} records")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
