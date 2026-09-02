#!/usr/bin/env python3
"""RFC-0104 M2 — the cross-language benchmark runner.

Five contestants per program: C, Rust, JavaScript (node), Vyrn native, Vyrn
wasm. The runner builds every one of them, proves they all print the same
bytes, and only then times anything.

    python run.py                      # build, verify, time, write the record
    python run.py --runs 15
    python run.py --only nbody,fasta
    python run.py --contestants vyrn-native --runs 1 --skip-verify   # calibrate
    python run.py --no-build --out /tmp/scratch.json

Three things about the method, because a benchmark is only worth its
methodology:

* **Whole-process wall time.** The game's own convention. `perf_counter`
  around the child, stdout discarded. So the number carries process
  start-up, and for the wasm leg it carries wasmtime's JIT — that is the
  price of running the thing, and hiding it would be a different measurement.
  `python run.py --floor` prints the empty-program floor for each contestant
  so the reader can subtract it.

  Beside the wall clock the runner records the four other columns the game's
  own per-program pages print, so the record answers the questions a reader
  brings to one of those pages (RFC-0104 M3 amendment):

  - **cpu secs** — user plus kernel time of the child process, from
    `GetProcessTimes` on Windows and `getrusage(RUSAGE_CHILDREN)` elsewhere.
    Every contestant here is single-threaded, so cpu under wall is idle time
    and cpu over wall would be parallelism; neither is hidden.
  - **mem** — peak working set of the child, from
    `GetProcessMemoryInfo(PeakWorkingSetSize)` on Windows and `ru_maxrss`
    elsewhere; the largest of the timed runs, because a peak is a peak.
  - **make secs** — wall time to build that contestant's timed artifact, once
    per program. Zero for node, which has no build step.
  - **gz** — the gzipped size of the single source file, which is the game's
    own stand-in for how much program it took. The two Vyrn legs share one
    source and therefore one figure.

* **N reaches the Vyrn programs through a temp copy.** `examples/*.vyrn` hold
  N as a `let` (RFC-0104 M1 decided that: no `.args` fixtures, one
  deterministic corpus run per program). The runner does NOT edit those files.
  It copies the source to a temp directory and rewrites exactly the one line
  `let <name> = <number>`, then compiles the copy. `--verify-rewrite` (on by
  default) stamps the copy with the FIXTURE N and checks it still prints the
  fixture, which is what proves the rewrite changed nothing but N.

* **Everything is verified before anything is timed.** Each contestant is
  checked against the committed fixture at the fixture N, and then all five
  are checked against EACH OTHER at the timing N. Line endings are normalized
  first: a native Windows build writes `\\r\\n` where the interpreter, wasm,
  Rust and node write `\\n` (RFC-0104 M0 found this).

Not run by CI. `rfcs/**` is CI-ignored, so this is a by-hand runner and the
committed JSON under `results/` is the record.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import date
from pathlib import Path

HARNESS = Path(__file__).resolve().parent
BENCHDIR = HARNESS.parent                      # rfcs/bench-0104
ROOT = BENCHDIR.parent.parent                  # the repo
EXAMPLES = ROOT / "examples"
BUILD = HARNESS / "build"                      # gitignored
RESULTS = BENCHDIR / "results"

# `vyrn-wasm2c` is RFC-0125 §2.5's release route (`vyrn build --route wasm2c`):
# the same wasm the `vyrn-wasm` leg runs, through wasm2c and clang. It needs
# wabt and simde under tools/, so it is not in the default set.
CONTESTANTS = ["c", "rust", "js", "vyrn-native", "vyrn-wasm", "vyrn-wasm2c"]

# The flags, stated rather than implied.
#
# `vyrn build`'s native pipeline passes clang `-O2 -ffp-contract=off
# -Wno-override-module` and no `-march` on the default x86-64 target
# (`add_native_clang_flags` in compiler/vyrn-cli/src/main.rs). The C leg is
# given the same two that affect code: same optimization level, same refusal to
# fuse `a*b+c`, same baseline ISA. Without `-ffp-contract=off` the C numbers
# would differ from every other contestant's in the last printed digit, and the
# cross-check would fail for a reason that has nothing to do with speed.
CFLAGS = ["-O2", "-ffp-contract=off", "-std=c11"]
RUSTFLAGS = ["-C", "opt-level=3"]


@dataclass
class Program:
    """One benchmark row."""

    name: str
    fixture: str
    fixture_n: int
    timing_n: int
    # The `let` in examples/<name>.vyrn that carries N, or None when N does not
    # come from the source at all (revcomp and k-nucleotide take their size
    # from the FASTA on stdin).
    n_let: str | None = None
    # When set, stdin is a FASTA generated at this N. The fixture run uses
    # `fasta-1000.expected`, which is the same generator at n = 1000.
    stdin_fasta_n: int | None = None
    note: str = ""


# Timing N per program. Chosen so the Vyrn NATIVE leg lands roughly 0.5-5 s on
# the recording machine — see "the sizes" in RFC-0104's M2 section. The fixture
# N is the committed corpus size and never moves.
PROGRAMS: list[Program] = [
    Program("nbody", "nbody-1000.expected", 1000, 25_000_000, n_let="steps"),
    Program("spectralnorm", "spectralnorm-100.expected", 100, 5500, n_let="order"),
    Program("fannkuch", "fannkuch-7.expected", 7, 11, n_let="order"),
    Program("binarytrees", "binarytrees-10.expected", 10, 18, n_let="order"),
    Program("fasta", "fasta-1000.expected", 1000, 5_000_000, n_let="order"),
    Program(
        "revcomp",
        "revcomp-1000.expected",
        1000,
        4_000_000,
        stdin_fasta_n=4_000_000,
        note="N is the fasta n; reverse-complement reads all 10n bases",
    ),
    Program(
        "knucleotide",
        "knucleotide-1000.expected",
        1000,
        400_000,
        stdin_fasta_n=400_000,
        note=(
            "N is the fasta n; k-nucleotide reads the 5n-base THREE sequence. "
            "This N was three orders of magnitude below the others while `Map` "
            "lookup was a linear scan and the program was quadratic in the "
            "number of distinct keys. RFC-0116 gave every map a hash index and "
            "RFC-0117 made the key an integer, so the reason is gone — and the "
            "old N left the row at 7 ms, close enough to the process floor "
            "that its cells drifted by multiples between runs while every "
            "other row held inside five per cent"
        ),
    ),
    Program("pidigits", "pidigits-27.expected", 27, 12000, n_let="order"),
    # The two RFC-0104 recorded as boundaries rather than omissions, and that
    # RFC-0111 and RFC-0112 unblocked. Both fixtures had been committed with no
    # program beside them since M1.
    #
    # `mandelbrot` has NO `n_let`: the harness rewrites a `let` to set N, and
    # this program's output is a binary PBM whose header states its own
    # dimensions, so the fixture size and the timing size are the same 200 and
    # the row would have nothing to rewrite.
    Program(
        "mandelbrot",
        "mandelbrot-200.expected",
        200,
        4000,
        n_let="order",
        note=(
            "writes a binary PBM through `writeStdout` (RFC-0111). The fixture "
            "stays at 200 because a PBM states its own dimensions; the timing N "
            "is 4,000 so the row lands in the same 0.5-5 s band as the others "
            "rather than at the process floor, where a scheduler hiccup is a "
            "multiple"
        ),
    ),
    Program(
        "regexredux",
        "regexredux-1000.expected",
        1000,
        100_000,
        stdin_fasta_n=100_000,
        note=(
            "N is the fasta n. The engine is `std/regex`, written in Vyrn and "
            "run by all three backends (RFC-0112). The timing N is 100,000 — a "
            "hundred times the fixture — so the row lands in the same 0.5-5 s "
            "band as the others; at the fixture size the whole run was 17 ms, "
            "close enough to the process floor that a scheduler hiccup showed "
            "up as a multiple"
        ),
    ),
]

BY_NAME = {p.name: p for p in PROGRAMS}


# --------------------------------------------------------------------------
# toolchain


def vyrn_exe() -> Path:
    """The `vyrn` driver. `$VYRN` wins; otherwise the workspace release build."""
    if os.environ.get("VYRN"):
        return Path(os.environ["VYRN"])
    p = ROOT / "compiler" / "target" / "release" / ("vyrn.exe" if os.name == "nt" else "vyrn")
    if not p.exists():
        die(f"no vyrn driver at {p} — `cargo build --release -p vyrn-cli` in compiler/, or set $VYRN")
    return p


def lock_wasmtime() -> tuple[str, str]:
    """The wasmtime the repository pins, as (version, sha256), from vyrn.lock.

    RFC-0102: "measured against wasmtime 46.0.1" is a lock line, not a
    sentence. The runner reads the same line the compiler does, so the
    committed record cannot claim a version the repo does not pin.
    """
    plat = {
        ("nt", "AMD64"): "x86_64-windows",
        ("posix", "x86_64"): "x86_64-linux",
        ("posix", "arm64"): "aarch64-macos",
        ("posix", "aarch64"): "aarch64-linux",
    }.get((os.name, platform.machine()), "x86_64-linux")
    for line in (ROOT / "vyrn.lock").read_text(encoding="utf-8").splitlines():
        parts = line.split("\t")
        if parts and parts[0].startswith("tool:wasmtime@") and parts[0].endswith("/" + plat):
            version = parts[0][len("tool:wasmtime@"):].split("/")[0]
            return version, parts[-1]
    die("vyrn.lock pins no wasmtime for this platform")


def wasmtime_exe() -> Path:
    """The pinned wasmtime binary in the RFC-0102 tool store.

    `$VYRN_WASMTIME` overrides, because that is the variable the rest of the
    project already uses.
    """
    if os.environ.get("VYRN_WASMTIME"):
        return Path(os.environ["VYRN_WASMTIME"])
    version, sha = lock_wasmtime()
    home = Path(os.environ.get("VYRN_HOME", Path.home() / ".vyrn"))
    store = home / "tools" / sha
    # `wasmtime-min` sits beside it in the Windows archive and is a different
    # build, so match the name exactly rather than by prefix.
    for name in ("wasmtime.exe", "wasmtime"):
        for cand in store.rglob(name):
            if cand.is_file():
                return cand
    die(f"pinned wasmtime {version} not unpacked at {store} — run `vyrn update --locked`")


def tool_version(cmd: list[str]) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as e:
        return f"unavailable ({e})"
    return (out.stdout or out.stderr).strip().splitlines()[0] if (out.stdout or out.stderr) else "unknown"


def hardware() -> dict:
    """CPU model, cores and RAM, best effort, stdlib only."""
    cpu, ram = platform.processor() or "unknown", None
    if os.name == "nt":
        try:
            import ctypes
            import winreg

            with winreg.OpenKey(
                winreg.HKEY_LOCAL_MACHINE,
                r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            ) as k:
                cpu = winreg.QueryValueEx(k, "ProcessorNameString")[0].strip()

            class MemStatus(ctypes.Structure):
                _fields_ = [
                    ("dwLength", ctypes.c_ulong),
                    ("dwMemoryLoad", ctypes.c_ulong),
                    ("ullTotalPhys", ctypes.c_ulonglong),
                    ("ullAvailPhys", ctypes.c_ulonglong),
                    ("ullTotalPageFile", ctypes.c_ulonglong),
                    ("ullAvailPageFile", ctypes.c_ulonglong),
                    ("ullTotalVirtual", ctypes.c_ulonglong),
                    ("ullAvailVirtual", ctypes.c_ulonglong),
                    ("ullAvailExtendedVirtual", ctypes.c_ulonglong),
                ]

            ms = MemStatus()
            ms.dwLength = ctypes.sizeof(MemStatus)
            ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(ms))
            ram = ms.ullTotalPhys
        except Exception:  # noqa: BLE001 — an environment record never fails a run
            pass
    else:
        try:
            for line in Path("/proc/meminfo").read_text().splitlines():
                if line.startswith("MemTotal:"):
                    ram = int(line.split()[1]) * 1024
            for line in Path("/proc/cpuinfo").read_text().splitlines():
                if line.startswith("model name"):
                    cpu = line.split(":", 1)[1].strip()
                    break
        except Exception:  # noqa: BLE001
            pass
    return {
        "cpu": cpu,
        "cores": os.cpu_count(),
        "ram_bytes": ram,
        "ram_gib": round(ram / 2**30, 1) if ram else None,
    }


def environment() -> dict:
    version, sha = lock_wasmtime()
    commit = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "-C", str(ROOT), "status", "--porcelain"], capture_output=True, text=True
    ).stdout.strip()
    return {
        # NO HOST NAME. It was `socket.gethostname()`, it was published in every
        # record, and the site printed it in a caption on two pages — a private
        # detail of somebody's desk that adds nothing to a measurement. What
        # makes a number checkable is the CPU, the OS, the toolchain versions and
        # the flags, and all of those stay (RFC-0106 M3, fourth round).
        "os": f"{platform.system()} {platform.release()} {platform.version()}",
        "hardware": hardware(),
        "clang": tool_version(["clang", "--version"]),
        "rustc": tool_version(["rustc", "-V"]),
        "node": tool_version(["node", "-v"]),
        "python": sys.version.split()[0],
        "vyrn": {
            "commit": commit,
            "worktree_clean": not dirty,
            "version": tool_version([str(vyrn_exe()), "--version"]),
        },
        "wasmtime": {
            "version": version,
            "lock_sha256": sha,
            "path": str(wasmtime_exe()),
        },
        "flags": {
            "c": "clang " + " ".join(CFLAGS),
            "rust": "rustc " + " ".join(RUSTFLAGS),
            "js": "node (no build step)",
            "vyrn-native": "vyrn build (clang -O2 -ffp-contract=off -Wno-override-module)",
            "vyrn-wasm": "vyrn build --target wasm (direct backend, no optimizer) + wasmtime run",
            "vyrn-wasm2c": "vyrn build --route wasm2c (the same wasm, wasm2c to C, clang -O2 -ffp-contract=off with wasm-rt)",
        },
    }


# --------------------------------------------------------------------------
# building


def die(msg: str):
    print(f"run.py: {msg}", file=sys.stderr)
    raise SystemExit(1)


def sh(cmd: list, **kw) -> subprocess.CompletedProcess:
    r = subprocess.run([str(c) for c in cmd], capture_output=True, text=True, **kw)
    if r.returncode != 0:
        die(f"failed: {' '.join(str(c) for c in cmd)}\n{r.stdout}\n{r.stderr}")
    return r


# How long each contestant's timed artifact took to build, keyed (program,
# contestant). The game calls this column `make secs`; here it is the wall time
# of the one command that produces the thing that gets timed, so a `--no-build`
# run records no figure rather than a stale one.
MAKE: dict[tuple[str, str], float] = {}


def timed_sh(key: tuple[str, str], cmd: list, **kw):
    t0 = time.perf_counter()
    sh(cmd, **kw)
    MAKE[key] = time.perf_counter() - t0


def source_of(contestant: str, prog: Program) -> Path:
    """The single source file one contestant's entry is written in.

    The game prints one `gz` per entry and this is the file it is of. Both Vyrn
    legs are built from the same `examples/<name>.vyrn`, so they carry the same
    figure: what differs between them is the backend, not the program.
    """
    if contestant == "c":
        return HARNESS / "c" / f"{prog.name}.c"
    if contestant == "rust":
        return HARNESS / "rust" / f"{prog.name}.rs"
    if contestant == "js":
        return HARNESS / "js" / f"{prog.name}.js"
    return EXAMPLES / f"{prog.name}.vyrn"


def gz_bytes(path: Path) -> int:
    """The source gzipped at level 9, in bytes. `mtime=0`, or the header would
    carry the clock and the same unchanged file would measure differently."""
    return len(gzip.compress(path.read_bytes(), 9, mtime=0))


def exe(name: str) -> Path:
    return BUILD / (name + (".exe" if os.name == "nt" else ""))


def stamp(prog: Program, n: int) -> Path:
    """A copy of `examples/<prog>.vyrn` with its N `let` rewritten to `n`.

    The committed file is never touched. Exactly one line may match, and the
    line is rewritten in place, so nothing but the number can move.
    """
    src = EXAMPLES / f"{prog.name}.vyrn"
    text = src.read_text(encoding="utf-8")
    pattern = re.compile(rf"^let {re.escape(prog.n_let)} = \d+$", re.MULTILINE)
    hits = pattern.findall(text)
    if len(hits) != 1:
        die(f"{src.name}: expected one `let {prog.n_let} = <number>` line, found {len(hits)}")
    out = BUILD / "stamped" / f"{prog.name}-{n}.vyrn"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(pattern.sub(f"let {prog.n_let} = {n}", text), encoding="utf-8", newline="")
    return out


def build_all(progs: list[Program], want: list[str]):
    BUILD.mkdir(parents=True, exist_ok=True)
    vyrn = vyrn_exe()
    for p in progs:
        print(f"  build {p.name}", flush=True)
        if "c" in want:
            timed_sh((p.name, "c"), ["clang", *CFLAGS, "-o", exe(f"c-{p.name}"), HARNESS / "c" / f"{p.name}.c"])
        if "rust" in want:
            timed_sh(
                (p.name, "rust"),
                ["rustc", *RUSTFLAGS, "-o", exe(f"rust-{p.name}"), HARNESS / "rust" / f"{p.name}.rs"],
                cwd=BUILD,
            )
        if "js" in want:
            MAKE[(p.name, "js")] = 0.0  # node has no build step, and 0 says so
        # The Vyrn legs. A program whose N is a `let` gets two builds: the
        # committed source (which already carries the fixture N) and a stamped
        # copy at the timing N. A program that reads stdin gets one.
        sources = {"fixture": EXAMPLES / f"{p.name}.vyrn"}
        if p.n_let:
            sources["timing"] = stamp(p, p.timing_n)
            sources["rewritten-fixture"] = stamp(p, p.fixture_n)
        else:
            sources["timing"] = sources["fixture"]
        for tag, src in sources.items():
            # Only the `timing` build is the one that gets timed, so only that
            # one is what `make secs` is of.
            native = (p.name, "vyrn-native") if tag == "timing" else None
            wasm = (p.name, "vyrn-wasm") if tag == "timing" else None
            wasm2c = (p.name, "vyrn-wasm2c") if tag == "timing" else None
            if "vyrn-native" in want:
                cmd = [vyrn, "build", src, "-o", exe(f"vyrn-{p.name}-{tag}")]
                timed_sh(native, cmd) if native else sh(cmd)
            if "vyrn-wasm" in want:
                cmd = [vyrn, "build", src, "--target", "wasm", "-o", BUILD / f"vyrn-{p.name}-{tag}.wasm"]
                timed_sh(wasm, cmd) if wasm else sh(cmd)
            if "vyrn-wasm2c" in want:
                cmd = [vyrn, "build", src, "--route", "wasm2c", "-o", exe(f"vyrn-w2c-{p.name}-{tag}")]
                timed_sh(wasm2c, cmd) if wasm2c else sh(cmd)


def command(contestant: str, prog: Program, n: int, tag: str) -> list:
    """How to invoke one contestant at size `n`.

    C, Rust and node take N on the command line; the Vyrn legs cannot, so they
    take it from the build `tag` — the source was stamped with it.
    """
    argv = [] if prog.n_let is None else [str(n)]
    if contestant == "c":
        return [exe(f"c-{prog.name}"), *argv]
    if contestant == "rust":
        return [exe(f"rust-{prog.name}"), *argv]
    if contestant == "js":
        return ["node", HARNESS / "js" / f"{prog.name}.js", *argv]
    if contestant == "vyrn-native":
        return [exe(f"vyrn-{prog.name}-{tag}")]
    if contestant == "vyrn-wasm":
        return [wasmtime_exe(), "run", BUILD / f"vyrn-{prog.name}-{tag}.wasm"]
    if contestant == "vyrn-wasm2c":
        return [exe(f"vyrn-w2c-{prog.name}-{tag}")]
    die(f"unknown contestant {contestant}")


# --------------------------------------------------------------------------
# verification


def norm(b: bytes) -> bytes:
    """CRLF -> LF, and one trailing newline at most.

    A native Windows build writes `\\r\\n` through the C runtime where every
    other contestant writes `\\n`. That is a line-ending artifact and not a
    difference in what the program computed (RFC-0104 M0).
    """
    return b.replace(b"\r\n", b"\n").rstrip(b"\n") + b"\n"


def capture(cmd: list, stdin_path: Path | None) -> bytes:
    with open(stdin_path, "rb") if stdin_path else open(os.devnull, "rb") as fh:
        r = subprocess.run([str(c) for c in cmd], stdin=fh, capture_output=True)
    if r.returncode != 0:
        die(f"nonzero exit from {' '.join(str(c) for c in cmd)}: {r.returncode}\n{r.stderr[:2000].decode(errors='replace')}")
    return r.stdout


def fasta_input(n: int) -> Path:
    """A FASTA of 10n bases on disk, for the two programs that read one.

    Written by the C fasta, which the fixture check has already proved correct
    at n = 1000 and which the cross-check proves agrees with the other four at
    the timing N. Line endings are normalized, so every contestant reads the
    same bytes whatever platform wrote them.
    """
    path = BUILD / f"fasta-{n}.txt"
    if not path.exists():
        if not exe("c-fasta").exists():
            sh(["clang", *CFLAGS, "-o", exe("c-fasta"), HARNESS / "c" / "fasta.c"])
        print(f"  generating fasta n={n} ({10 * n} bases)", flush=True)
        path.write_bytes(norm(capture([exe("c-fasta"), str(n)], None)))
    return path


def verify(progs: list[Program], want: list[str]) -> dict:
    """Every contestant against the fixture, then all of them against each other."""
    report = {}
    for p in progs:
        print(f"  verify {p.name}", flush=True)
        expected = norm((BENCHDIR / p.fixture).read_bytes())
        fixture_stdin = BENCHDIR / "fasta-1000.expected" if p.stdin_fasta_n else None

        for c in want:
            got = norm(capture(command(c, p, p.fixture_n, "fixture"), fixture_stdin))
            if got != expected:
                die(f"{p.name}/{c} does not print {p.fixture} at N={p.fixture_n}\n{diff(expected, got)}")

        # The rewrite mechanism itself: the stamped copy, at the fixture N,
        # still prints the fixture. If this passes, the only thing the stamp
        # changed is the number.
        if p.n_let and "vyrn-native" in want:
            got = norm(capture(command("vyrn-native", p, p.fixture_n, "rewritten-fixture"), fixture_stdin))
            if got != expected:
                die(f"{p.name}: the stamped copy at N={p.fixture_n} does not print {p.fixture}\n{diff(expected, got)}")

        # And all five against each other at the size that will be timed.
        timing_stdin = fasta_input(p.stdin_fasta_n) if p.stdin_fasta_n else None
        outs = {c: norm(capture(command(c, p, p.timing_n, "timing"), timing_stdin)) for c in want}
        first = want[0]
        for c in want[1:]:
            if outs[c] != outs[first]:
                die(f"{p.name}: {c} and {first} disagree at N={p.timing_n}\n{diff(outs[first], outs[c])}")
        report[p.name] = {
            "fixture": p.fixture,
            "fixture_n": p.fixture_n,
            "timing_n": p.timing_n,
            "contestants_agree": sorted(want),
            "timing_output_bytes": len(outs[first]),
        }
    return report


def diff(want: bytes, got: bytes) -> str:
    w, g = want.split(b"\n"), got.split(b"\n")
    for i in range(max(len(w), len(g))):
        a = w[i] if i < len(w) else b"<end>"
        b = g[i] if i < len(g) else b"<end>"
        if a != b:
            return f"  line {i + 1}\n  want: {a[:200]!r}\n  got:  {b[:200]!r}"
    return "  (lengths differ, no differing line)"


# --------------------------------------------------------------------------
# timing


if os.name == "nt":
    import ctypes
    from ctypes import wintypes

    class _FileTime(ctypes.Structure):
        _fields_ = [("lo", wintypes.DWORD), ("hi", wintypes.DWORD)]

    class _MemCounters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]


def usage_of(proc: subprocess.Popen, before) -> tuple[float | None, int | None]:
    """The cpu time and peak resident memory of one FINISHED child.

    Windows: `GetProcessTimes` and `GetProcessMemoryInfo` on the handle
    `Popen` still holds — both stay readable after the process exits, for as
    long as a handle to it is open, which is what makes this exact per child
    rather than a sampled guess.

    Elsewhere: `getrusage(RUSAGE_CHILDREN)`, differenced across the run for
    the cpu time and read directly for `ru_maxrss`, which is already a
    high-water mark over all reaped children. Stdlib and ctypes only; the
    harness takes no pip dependency.
    """
    if os.name == "nt":
        h = wintypes.HANDLE(int(proc._handle))
        created, exited, kernel, user = _FileTime(), _FileTime(), _FileTime(), _FileTime()
        cpu = None
        if ctypes.windll.kernel32.GetProcessTimes(
            h, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(kernel), ctypes.byref(user)
        ):
            ticks = ((kernel.hi << 32) | kernel.lo) + ((user.hi << 32) | user.lo)
            cpu = ticks / 1e7  # FILETIME counts 100-nanosecond intervals
        mem = _MemCounters()
        mem.cb = ctypes.sizeof(_MemCounters)
        peak = None
        if ctypes.WinDLL("psapi").GetProcessMemoryInfo(h, ctypes.byref(mem), mem.cb):
            peak = int(mem.PeakWorkingSetSize)
        return cpu, peak
    if before is None:
        return None, None
    import resource

    now = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (now.ru_utime - before.ru_utime) + (now.ru_stime - before.ru_stime)
    scale = 1 if sys.platform == "darwin" else 1024  # ru_maxrss is bytes on macOS, KiB on Linux
    return cpu, int(now.ru_maxrss) * scale


def measure(cmd: list, stdin_path: Path | None, runs: int) -> tuple[list[float], list[float], list[int]]:
    """Whole-process wall time, cpu time and peak memory, `runs` times.

    `subprocess.run` became `Popen` plus `wait` so the handle survives long
    enough to be asked what the child cost. With stdout and stderr going to
    the null device there is no pipe to drain, so the two are the same
    sequence of calls and the wall clock is measured exactly as M2 measured it.
    """
    walls: list[float] = []
    cpus: list[float] = []
    peaks: list[int] = []
    for _ in range(runs):
        before = None
        if os.name != "nt":
            import resource

            before = resource.getrusage(resource.RUSAGE_CHILDREN)
        with open(stdin_path, "rb") if stdin_path else open(os.devnull, "rb") as fh:
            t0 = time.perf_counter()
            proc = subprocess.Popen(
                [str(c) for c in cmd],
                stdin=fh,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            rc = proc.wait()
            walls.append(time.perf_counter() - t0)
            cpu, peak = usage_of(proc, before)
        if rc != 0:
            die(f"nonzero exit while timing {' '.join(str(c) for c in cmd)}: {rc}")
        if cpu is not None:
            cpus.append(cpu)
        if peak is not None:
            peaks.append(peak)
    return walls, cpus, peaks


def floor(want: list[str], runs: int) -> dict:
    """What an empty program costs each contestant — the start-up to subtract."""
    tmp = Path(tempfile.mkdtemp(prefix="vyrn-floor-"))
    out = {}
    try:
        (tmp / "e.c").write_text("int main(void){return 0;}\n")
        (tmp / "e.rs").write_text("fn main() {}\n")
        (tmp / "e.js").write_text("\n")
        (tmp / "e.vyrn").write_text("fn main() -> Int64 {\n    return 0\n}\n")
        cmds = {}
        if "c" in want:
            sh(["clang", *CFLAGS, "-o", tmp / "e-c.exe", tmp / "e.c"])
            cmds["c"] = [tmp / "e-c.exe"]
        if "rust" in want:
            sh(["rustc", *RUSTFLAGS, "-o", tmp / "e-rs.exe", tmp / "e.rs"], cwd=tmp)
            cmds["rust"] = [tmp / "e-rs.exe"]
        if "js" in want:
            cmds["js"] = ["node", tmp / "e.js"]
        if "vyrn-native" in want:
            sh([vyrn_exe(), "build", tmp / "e.vyrn", "-o", tmp / "e-v.exe"])
            cmds["vyrn-native"] = [tmp / "e-v.exe"]
        if "vyrn-wasm" in want:
            sh([vyrn_exe(), "build", tmp / "e.vyrn", "--target", "wasm", "-o", tmp / "e.wasm"])
            cmds["vyrn-wasm"] = [wasmtime_exe(), "run", tmp / "e.wasm"]
        for c, cmd in cmds.items():
            ts, cpus, peaks = measure(cmd, None, runs)
            out[c] = {
                "median_s": statistics.median(ts),
                "runs_s": ts,
                "cpu_median_s": statistics.median(cpus) if cpus else None,
                "peak_bytes": max(peaks) if peaks else None,
            }
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    return out


def summarize(ts: list[float], cpus: list[float], peaks: list[int], make: float | None, gz: int) -> dict:
    """One cell of the record: the wall clock first, then the four columns the
    game's own per-program pages print beside it.

    Every key is a scalar or a flat list at one depth, because the site reads
    this file by slicing it at known indentation rather than parsing it.
    """
    m = statistics.median(ts)
    return {
        "median_s": m,
        "min_s": min(ts),
        "max_s": max(ts),
        "spread_pct": round(100.0 * (max(ts) - min(ts)) / m, 2) if m else None,
        "runs_s": ts,
        "cpu_median_s": statistics.median(cpus) if cpus else None,
        "cpu_runs_s": cpus,
        "peak_bytes": max(peaks) if peaks else None,
        "make_s": make,
        "gz_bytes": gz,
    }


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", type=int, default=10, help="timed runs per contestant (default 10)")
    ap.add_argument("--only", default="", help="comma-separated program names")
    ap.add_argument("--contestants", default=",".join(c for c in CONTESTANTS if c != "vyrn-wasm2c"))
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--skip-verify", action="store_true", help="calibration only; never for a committed record")
    ap.add_argument("--floor", action="store_true", help="also measure the empty-program floor")
    ap.add_argument("--out", default="", help="where to write the JSON (default results/<date>-<host>.json)")
    args = ap.parse_args()

    if args.runs < 10 and not args.skip_verify:
        print("run.py: note — a committed record wants at least 10 runs", file=sys.stderr)

    want = [c.strip() for c in args.contestants.split(",") if c.strip()]
    for c in want:
        if c not in CONTESTANTS:
            die(f"unknown contestant {c} (known: {', '.join(CONTESTANTS)})")
    progs = [BY_NAME[n.strip()] for n in args.only.split(",") if n.strip()] if args.only else PROGRAMS

    started = time.time()
    print("toolchain")
    env = environment()
    for k in ("clang", "rustc", "node"):
        print(f"  {k}: {env[k]}")
    print(f"  vyrn: {env['vyrn']['version']} @ {env['vyrn']['commit'][:12]}"
          f"{'' if env['vyrn']['worktree_clean'] else ' (dirty worktree)'}")
    print(f"  wasmtime: {env['wasmtime']['version']} (lock {env['wasmtime']['lock_sha256'][:12]})")

    if not args.no_build:
        print("building")
        build_all(progs, want)
    else:
        BUILD.mkdir(parents=True, exist_ok=True)

    verification = {}
    if args.skip_verify:
        print("verification SKIPPED — this run is not a record")
    else:
        print("verifying")
        verification = verify(progs, want)

    print(f"timing ({args.runs} runs each)")
    results = {}
    for p in progs:
        stdin_path = fasta_input(p.stdin_fasta_n) if p.stdin_fasta_n else None
        row = {}
        for c in want:
            ts, cpus, peaks = measure(command(c, p, p.timing_n, "timing"), stdin_path, args.runs)
            row[c] = summarize(ts, cpus, peaks, MAKE.get((p.name, c)), gz_bytes(source_of(c, p)))
            cell = row[c]
            print(f"  {p.name:<14} {c:<12} {cell['median_s'] * 1000:9.1f} ms"
                  f"  cpu {(cell['cpu_median_s'] or 0) * 1000:8.1f} ms"
                  f"  mem {(cell['peak_bytes'] or 0) // 1024:8d} KB"
                  f"  (spread {cell['spread_pct']}%)", flush=True)
        results[p.name] = {"n": p.timing_n, "note": p.note, "contestants": row}

    record = {
        "rfc": "RFC-0104",
        "milestone": "M2",
        "date": date.today().isoformat(),
        "runs": args.runs,
        "method": "whole-process wall time, stdout discarded, median of the runs",
        "columns": {
            "median_s": "wall clock of the whole process, median of the runs",
            "cpu_median_s": "user plus kernel time of the child, median of the runs",
            "peak_bytes": "peak working set of the child, the largest of the runs",
            "make_s": "wall clock of the one command that builds the timed artifact, 0 for node",
            "gz_bytes": "the contestant's single source file, gzipped at level 9",
        },
        "environment": env,
        "verification": verification,
        "results": results,
        "elapsed_s": round(time.time() - started, 1),
    }
    if args.floor:
        print("floor (empty program)")
        record["floor"] = floor(want, args.runs)
        for c, v in record["floor"].items():
            print(f"  {c:<12} {v['median_s'] * 1000:9.1f} ms")

    RESULTS.mkdir(parents=True, exist_ok=True)
    if args.out:
        out = Path(args.out)
    else:
        # The date and nothing else. This used to append the host name, which is
        # how four committed records ended up with a machine's name in their
        # file names as well as in their bodies.
        base = f"{record['date']}"
        out = RESULTS / f"{base}.json"
        k = 2
        while out.exists():
            out = RESULTS / f"{base}-run{k}.json"
            k += 1
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
