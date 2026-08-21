#!/usr/bin/env python3
"""A minute with Vyrn, recorded by running the real binary.

    python scripts/site-demo.py > site/data/demo.json
    python scripts/site-demo.py --vyrn compiler/target/release/vyrn > site/data/demo.json

WHY A SCRIPT AND NOT THE EXPORT. `site/export.vyrn` reads the working tree with
RFC-0014's `readFile` and `listDir`, and that is all it can do: there is no way
to spawn a process from Vyrn, and there is not going to be one for the sake of a
widget. `scripts/site-history.py` set the pattern and this follows it exactly —
the script writes JSON, the export refuses to publish without it, and a
committed fixture keeps the tests runnable on a clone that has never run either.

WHAT IS RECORDED. Seven steps, in a scratch directory this script makes and
deletes, against the binary it is pointed at:

  1. `vyrn new demo`                     the scaffold, and what the tool says
  2. the files it wrote                  walked, not typed: no command, no
                                         invented output
  3. `vyrn run`                          the scaffold runs with no argument
  4. one edit                            a validated type and a `test` block,
                                         written into `src/main.vyrn`
  5. `vyrn test src/main.vyrn`           the edit's own check
  6. `vyrn build src/main.vyrn -o demo`  a native binary
  7. `./demo`                            and the same two lines out of it

Every `out` below is the process's real stdout and stderr, to the byte, and
every step carries the exit code the process returned. Nothing is transcribed.
A step whose command fails stops the recording: a demo of a broken toolchain is
worse than no demo, and the site would publish it without noticing.

The file is version-stamped from `vyrn --version`, so the page can say which
build produced what it shows.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import date

# The one edit the demo makes: a type that carries its rule, and the `test`
# block that checks it, in the file `vyrn new` just wrote. The block is the
# point of the step it feeds — a test lives in the source file, beside the code
# it is about, and nothing is installed to run it.
#
# It is a PORT and not the index's `Age`, deliberately: the hero editor at the
# top of the same page already shows `Age`, and the same twelve lines twice on
# one page is a page that repeats itself.
EDIT = """type Port = Int64 where value > 0 && value <= 65535

fn describe(n: Int64) -> String {
    return match Port?(n) {
        Some(p) => "listening on \\{p}",
        None => "\\{n} is not a port",
    }
}

fn main() -> Int64 {
    print(describe(8080))
    print(describe(70000))
    return 0
}

test "the rule travels with the type" {
    assertEq(describe(8080), "listening on 8080")
    assertEq(describe(70000), "70000 is not a port")
}
"""


def run(vyrn, args, cwd):
    """One process, as (text, exit code). stdout and stderr in the order a
    terminal shows them, which for these commands is stdout then stderr."""
    p = subprocess.run(
        [vyrn, *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return (p.stdout + p.stderr).replace("\r\n", "\n").rstrip("\n"), p.returncode


def step(steps, vyrn, cwd, sid, title, args, note, shown=None):
    """Run one command and record it. A failure is fatal."""
    out, code = run(vyrn, args, cwd)
    if code != 0:
        sys.exit(
            f"site-demo: `vyrn {' '.join(args)}` exited {code} in {cwd}:\n{out}"
        )
    steps.append(
        {
            "id": sid,
            "title": title,
            "cmd": shown or ("vyrn " + " ".join(args)),
            "out": out,
            "exit": code,
            "note": note,
        }
    )


def tree(root):
    """Every file the scaffold wrote, walked. Directories first, then names,
    both sorted, so a re-run on another machine prints the same listing."""
    rows = []
    for here, dirs, files in os.walk(root):
        dirs.sort()
        rel = os.path.relpath(here, os.path.dirname(root)).replace(os.sep, "/")
        rows.append(rel + "/")
        for name in sorted(files):
            rows.append(rel + "/" + name)
    return "\n".join(rows)


def record(vyrn):
    version, code = run(vyrn, ["--version"], os.getcwd())
    if code != 0:
        sys.exit(f"site-demo: `{vyrn} --version` exited {code}: {version}")

    work = tempfile.mkdtemp(prefix="vyrn-demo-")
    try:
        steps = []
        step(
            steps,
            vyrn,
            work,
            "new",
            "New project",
            ["new", "demo"],
            "One manifest, one source file.",
        )
        proj = os.path.join(work, "demo")
        steps.append(
            {
                "id": "tree",
                "title": "What it wrote",
                "cmd": "",
                "out": tree(proj),
                "exit": 0,
                "note": "Walked, not typed.",
            }
        )
        step(
            steps,
            vyrn,
            proj,
            "run",
            "Run it",
            ["run"],
            "vyrn.json names the entry point.",
        )
        with open(os.path.join(proj, "src", "main.vyrn"), "w", encoding="utf-8", newline="\n") as f:
            f.write(EDIT)
        steps.append(
            {
                "id": "edit",
                "title": "Add a rule",
                "cmd": "",
                "out": EDIT.rstrip("\n"),
                "exit": 0,
                "note": "A rule the compiler enforces.",
            }
        )
        step(
            steps,
            vyrn,
            proj,
            "test",
            "Check it",
            ["test", "src/main.vyrn"],
            "Tests live beside the code.",
        )
        step(
            steps,
            vyrn,
            proj,
            "build",
            "Build a binary",
            ["build", "src/main.vyrn", "-o", "demo"],
            "clang links it; wasm needs nothing.",
        )
        # `-o demo` is what the linker was told, and it is what lands: the
        # compiler does not append `.exe`. The fallback is here because a
        # toolchain that does would otherwise fail with a missing file rather
        # than say which name it wrote.
        exe = os.path.join(proj, "demo")
        if not os.path.exists(exe) and os.path.exists(exe + ".exe"):
            exe += ".exe"
        p = subprocess.run(
            [exe], cwd=proj, capture_output=True, text=True, encoding="utf-8", errors="replace"
        )
        if p.returncode != 0:
            sys.exit(f"site-demo: the built binary exited {p.returncode}:\n{p.stdout}{p.stderr}")
        steps.append(
            {
                "id": "exe",
                "title": "The binary",
                "cmd": "./demo",
                "out": (p.stdout + p.stderr).replace("\r\n", "\n").rstrip("\n"),
                "exit": p.returncode,
                "note": "The interpreter's bytes, from a binary.",
            }
        )
        return {
            "vyrn": version.strip(),
            "recorded": date.today().isoformat(),
            "steps": steps,
        }
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    args = sys.argv[1:]
    vyrn = "vyrn"
    if "--vyrn" in args:
        vyrn = args[args.index("--vyrn") + 1]
    # ABSOLUTE, and this line is load-bearing. Every step below runs in a scratch
    # directory, and on POSIX a relative program path is resolved against the
    # CHILD's working directory — so `--vyrn compiler/target/release/vyrn`, which
    # is what `site.yml` passes, raised `FileNotFoundError` on the first step
    # while the same argument worked on Windows, where `CreateProcess` resolves
    # it against the parent's. `shutil.which` returns a path with a separator in
    # it unchanged, so it does not fix this either.
    vyrn = os.path.abspath(shutil.which(vyrn) or vyrn)
    json.dump(record(vyrn), sys.stdout, separators=(",", ":"), sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
