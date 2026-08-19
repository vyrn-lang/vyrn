#!/usr/bin/env python3
"""Reference implementations of the ten Benchmarks Game programs, at the small N
the M0 census fixes, writing one `*.expected` file each.

Provenance: every routine here is the game's published algorithm transcribed
from its specification (the constants, the LCG, the output formats), not a copy
of any entry's source. Where the game publishes a known-good number for the N
used, this file's output agrees with it -- nbody at 1000 prints -0.169075164
then -0.169087605, spectral-norm at 100 prints 1.274219991, fannkuch-redux at 7
prints 228 then Pfannkuchen(7) = 16, pidigits at 27 starts 3141592653. Those
four agreements are the check on the transcription.

Run: python gen.py   (writes into the parent directory, LF endings, binary mode)
"""

import os
import sys

OUT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def write(name, data):
    if isinstance(data, str):
        data = data.encode("ascii")
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)
    print("%-28s %d bytes" % (name, len(data)))


# ---------------------------------------------------------------- nbody ----

PI = 3.141592653589793
SOLAR_MASS = 4 * PI * PI
DAYS_PER_YEAR = 365.24

BODIES = [
    ([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], SOLAR_MASS),
    (
        [4.84143144246472090e00, -1.16032004402742839e00, -1.03622044471123109e-01],
        [
            1.66007664274403694e-03 * DAYS_PER_YEAR,
            7.69901118419740425e-03 * DAYS_PER_YEAR,
            -6.90460016972063023e-05 * DAYS_PER_YEAR,
        ],
        9.54791938424326609e-04 * SOLAR_MASS,
    ),
    (
        [8.34336671824457987e00, 4.12479856412430479e00, -4.03523417114321381e-01],
        [
            -2.76742510726862411e-03 * DAYS_PER_YEAR,
            4.99852801234917238e-03 * DAYS_PER_YEAR,
            2.30417297573763929e-05 * DAYS_PER_YEAR,
        ],
        2.85885980666130812e-04 * SOLAR_MASS,
    ),
    (
        [1.28943695621391310e01, -1.51111514016986312e01, -2.23307578892655734e-01],
        [
            2.96460137564761618e-03 * DAYS_PER_YEAR,
            2.37847173959480950e-03 * DAYS_PER_YEAR,
            -2.96589568540237556e-05 * DAYS_PER_YEAR,
        ],
        4.36624404335156298e-05 * SOLAR_MASS,
    ),
    (
        [1.53796971148509165e01, -2.59193146099879641e01, 1.79258772950371181e-01],
        [
            2.68067772490389322e-03 * DAYS_PER_YEAR,
            1.62824170038242295e-03 * DAYS_PER_YEAR,
            -9.51592254519715870e-05 * DAYS_PER_YEAR,
        ],
        5.15138902046611451e-05 * SOLAR_MASS,
    ),
]


def nbody(n):
    b = [(list(p), list(v), m) for (p, v, m) in BODIES]
    px = py = pz = 0.0
    for (p, v, m) in b:
        px += v[0] * m
        py += v[1] * m
        pz += v[2] * m
    b[0][1][0] = -px / SOLAR_MASS
    b[0][1][1] = -py / SOLAR_MASS
    b[0][1][2] = -pz / SOLAR_MASS

    def energy():
        e = 0.0
        for i in range(len(b)):
            (p1, v1, m1) = b[i]
            e += 0.5 * m1 * (v1[0] * v1[0] + v1[1] * v1[1] + v1[2] * v1[2])
            for j in range(i + 1, len(b)):
                (p2, _, m2) = b[j]
                dx = p1[0] - p2[0]
                dy = p1[1] - p2[1]
                dz = p1[2] - p2[2]
                e -= (m1 * m2) / (dx * dx + dy * dy + dz * dz) ** 0.5
        return e

    out = ["%.9f\n" % energy()]
    dt = 0.01
    for _ in range(n):
        for i in range(len(b)):
            (p1, v1, m1) = b[i]
            for j in range(i + 1, len(b)):
                (p2, v2, m2) = b[j]
                dx = p1[0] - p2[0]
                dy = p1[1] - p2[1]
                dz = p1[2] - p2[2]
                mag = dt * (dx * dx + dy * dy + dz * dz) ** -1.5
                v1[0] -= dx * m2 * mag
                v1[1] -= dy * m2 * mag
                v1[2] -= dz * m2 * mag
                v2[0] += dx * m1 * mag
                v2[1] += dy * m1 * mag
                v2[2] += dz * m1 * mag
        for (p, v, _) in b:
            p[0] += dt * v[0]
            p[1] += dt * v[1]
            p[2] += dt * v[2]
    out.append("%.9f\n" % energy())
    return "".join(out)


# -------------------------------------------------------- spectral-norm ----


def spectral_norm(n):
    def a(i, j):
        return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)

    def au(u, out):
        for i in range(n):
            s = 0.0
            for j in range(n):
                s += a(i, j) * u[j]
            out[i] = s

    def atu(u, out):
        for i in range(n):
            s = 0.0
            for j in range(n):
                s += a(j, i) * u[j]
            out[i] = s

    u = [1.0] * n
    v = [0.0] * n
    w = [0.0] * n
    for _ in range(10):
        au(u, w)
        atu(w, v)
        au(v, w)
        atu(w, u)
    vbv = vv = 0.0
    for i in range(n):
        vbv += u[i] * v[i]
        vv += v[i] * v[i]
    return "%.9f\n" % (vbv / vv) ** 0.5


# ------------------------------------------------------- fannkuch-redux ----


def fannkuch(n):
    perm = [0] * n
    perm1 = list(range(n))
    count = [0] * n
    maxflips = 0
    checksum = 0
    r = n
    permcount = 0
    while True:
        while r != 1:
            count[r - 1] = r
            r -= 1
        perm[:] = perm1
        flips = 0
        k = perm[0]
        while k:
            perm[: k + 1] = perm[k::-1]
            flips += 1
            k = perm[0]
        if flips > maxflips:
            maxflips = flips
        checksum += flips if permcount % 2 == 0 else -flips
        permcount += 1
        # next permutation
        while True:
            if r == n:
                return "%d\nPfannkuchen(%d) = %d\n" % (checksum, n, maxflips)
            perm0 = perm1[0]
            i = 0
            while i < r:
                perm1[i] = perm1[i + 1]
                i += 1
            perm1[r] = perm0
            count[r] -= 1
            if count[r] > 0:
                break
            r += 1


# --------------------------------------------------------- binary-trees ----


def binary_trees(n):
    sys.setrecursionlimit(10000)

    def make(d):
        if d == 0:
            return (None, None)
        return (make(d - 1), make(d - 1))

    def check(t):
        (l, r) = t
        if l is None:
            return 1
        return 1 + check(l) + check(r)

    min_depth = 4
    max_depth = max(min_depth + 2, n)
    stretch_depth = max_depth + 1
    out = ["stretch tree of depth %d\t check: %d\n" % (stretch_depth, check(make(stretch_depth)))]
    long_lived = make(max_depth)
    for depth in range(min_depth, stretch_depth, 2):
        iterations = 1 << (max_depth - depth + min_depth)
        c = 0
        for _ in range(iterations):
            c += check(make(depth))
        out.append("%d\t trees of depth %d\t check: %d\n" % (iterations, depth, c))
    out.append("long lived tree of depth %d\t check: %d\n" % (max_depth, check(long_lived)))
    return "".join(out)


# ---------------------------------------------------------------- fasta ----

ALU = (
    "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGGGAGGCCGAGGCGGGCGGA"
    "TCACCTGAGGTCAGGAGTTCGAGACCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACT"
    "AAAAATACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCAGCTACTCGGGAG"
    "GCTGAGGCAGGAGAATCGCTTGAACCCGGGAGGCGGAGGTTGCAGTGAGCCGAGATCGCG"
    "CCACTGCACTCCAGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"
)

IUB = list(
    zip(
        "acgtBDHKMNRSVWY",
        [0.27, 0.12, 0.12, 0.27, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02, 0.02],
    )
)

HOMOSAPIENS = list(
    zip("acgt", [0.3029549426680, 0.1979883004921, 0.1975473066391, 0.3015094502008])
)

IM = 139968
IA = 3877
IC = 29573
LINE = 60


def cumulative(table):
    out = []
    p = 0.0
    for (c, w) in table:
        p += w
        out.append((c, p))
    return out


def fasta(n):
    seed = [42]

    def rnd(mx):
        seed[0] = (seed[0] * IA + IC) % IM
        return mx * seed[0] / IM

    out = []
    out.append(">ONE Homo sapiens alu\n")
    k = 0
    todo = n * 2
    while todo > 0:
        m = LINE if todo > LINE else todo
        line = []
        for _ in range(m):
            line.append(ALU[k])
            k = (k + 1) % len(ALU)
        out.append("".join(line) + "\n")
        todo -= m

    def random_seq(header, table, count):
        out.append(header)
        cum = cumulative(table)
        todo = count
        while todo > 0:
            m = LINE if todo > LINE else todo
            line = []
            for _ in range(m):
                r = rnd(1.0)
                for (c, p) in cum:
                    if p > r:
                        line.append(c)
                        break
                else:
                    line.append(cum[-1][0])
            out.append("".join(line) + "\n")
            todo -= m

    random_seq(">TWO IUB ambiguity codes\n", IUB, n * 3)
    random_seq(">THREE Homo sapiens frequency\n", HOMOSAPIENS, n * 5)
    return "".join(out)


# ----------------------------------------------------- reverse-complement ----

COMP = {}
for (a, b) in zip("ACBDGHKMNSRUTWVYacbdghkmnsrutwvy", "TGVHCDMKNSYAAWBRTGVHCDMKNSYAAWBR"):
    COMP[a] = b


def reverse_complement(text):
    out = []
    for block in text.split(">")[1:]:
        nl = block.index("\n")
        header = block[:nl]
        seq = block[nl + 1 :].replace("\n", "")
        rc = "".join(COMP[c] for c in reversed(seq))
        out.append(">" + header + "\n")
        for i in range(0, len(rc), LINE):
            out.append(rc[i : i + LINE] + "\n")
    return "".join(out)


# -------------------------------------------------------- k-nucleotide ----


def third_sequence(text):
    i = text.index(">THREE")
    body = text[text.index("\n", i) + 1 :]
    return body.replace("\n", "").upper()


def k_nucleotide(text):
    seq = third_sequence(text)
    out = []

    def counts(k):
        d = {}
        for i in range(len(seq) - k + 1):
            key = seq[i : i + k]
            d[key] = d.get(key, 0) + 1
        return d

    for k in (1, 2):
        d = counts(k)
        total = sum(d.values())
        for (key, c) in sorted(d.items(), key=lambda kv: (-kv[1], kv[0])):
            out.append("%s %.3f\n" % (key, 100.0 * c / total))
        out.append("\n")
    for frag in ("GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT"):
        d = counts(len(frag))
        out.append("%d\t%s\n" % (d.get(frag, 0), frag))
    return "".join(out)


# --------------------------------------------------------- regex-redux ----

VARIANTS = [
    "agggtaaa|tttaccct",
    "[cgt]gggtaaa|tttaccc[acg]",
    "a[act]ggtaaa|tttacc[agt]t",
    "ag[act]gtaaa|tttac[agt]ct",
    "agg[act]taaa|ttta[agt]cct",
    "aggg[acg]aaa|ttt[cgt]ccct",
    "agggt[cgt]aa|tt[acg]accct",
    "agggta[cgt]a|t[acg]taccct",
    "agggtaa[cgt]|[acg]ttaccct",
]

SUBST = [
    ("tHa[Nt]", "<4>"),
    ("aND|caN|Ha[DS]|WaS", "<3>"),
    ("a[NSt]|BY", "<2>"),
    ("<[^>]*>", "|"),
    ("\\|[^|][^|]*\\|", "-"),
]


def regex_redux(text):
    import re

    ilen = len(text)
    seq = re.sub(">[^\n]*\n|\n", "", text)
    clen = len(seq)
    out = []
    for v in VARIANTS:
        out.append("%s %d\n" % (v, len(re.findall(v, seq))))
    for (pat, rep) in SUBST:
        seq = re.sub(pat, rep, seq)
    out.append("\n%d\n%d\n%d\n" % (ilen, clen, len(seq)))
    return "".join(out)


# ----------------------------------------------------------- mandelbrot ----


def mandelbrot(n):
    out = bytearray(b"P4\n%d %d\n" % (n, n))
    for y in range(n):
        ci = 2.0 * y / n - 1.0
        bits = 0
        nbits = 0
        for x in range(n):
            cr = 2.0 * x / n - 1.5
            zr = zi = 0.0
            b = 1
            for _ in range(50):
                nzr = zr * zr - zi * zi + cr
                zi = 2.0 * zr * zi + ci
                zr = nzr
                if zr * zr + zi * zi > 4.0:
                    b = 0
                    break
            bits = (bits << 1) | b
            nbits += 1
            if nbits == 8:
                out.append(bits)
                bits = 0
                nbits = 0
        if nbits:
            out.append(bits << (8 - nbits))
    return bytes(out)


# ------------------------------------------------------------- pidigits ----


def pidigits(n):
    q, r, t, k, digit, l = 1, 0, 1, 1, 3, 3
    out = []
    line = []
    i = 0
    while i < n:
        if 4 * q + r - t < digit * t:
            line.append(str(digit))
            i += 1
            if i % 10 == 0:
                out.append("%s\t:%d\n" % ("".join(line), i))
                line = []
            q, r, t, k, digit, l = (
                10 * q,
                10 * (r - digit * t),
                t,
                k,
                (10 * (3 * q + r)) // t - 10 * digit,
                l,
            )
        else:
            q, r, t, k, digit, l = (
                q * k,
                (2 * q + r) * l,
                t * l,
                k + 1,
                (q * (7 * k + 2) + r * l) // (t * l),
                l + 2,
            )
    if line:
        out.append("%s\t:%d\n" % ("".join(line).ljust(10), n))
    return "".join(out)


# ------------------------------------------------------------------ main ----

if __name__ == "__main__":
    write("nbody-1000.expected", nbody(1000))
    write("spectralnorm-100.expected", spectral_norm(100))
    write("fannkuch-7.expected", fannkuch(7))
    write("binarytrees-10.expected", binary_trees(10))
    fa = fasta(1000)
    write("fasta-1000.expected", fa)
    write("revcomp-1000.expected", reverse_complement(fa))
    write("knucleotide-1000.expected", k_nucleotide(fa))
    write("regexredux-1000.expected", regex_redux(fa))
    write("mandelbrot-200.expected", mandelbrot(200))
    write("pidigits-27.expected", pidigits(27))
