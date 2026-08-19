#!/usr/bin/env python3
"""Compare `p-mandelbrot.vyrn`'s hex rows against the binary `mandelbrot-200.expected`.

The probe cannot print the PBM itself (Vyrn's stdout is UTF-8 text), so it prints
the same packed bytes as hex. This turns the fixture into the same form and
diffs them.

Run from `rfcs/bench-0104`:  vyrn run p-mandelbrot.vyrn | python ref/checkhex.py
"""

import os
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
got = sys.stdin.buffer.read().split(b"\n")
want = open(os.path.join(HERE, "mandelbrot-200.expected"), "rb").read()

nl = want.index(b"\n", want.index(b"\n") + 1) + 1
header, body = want[:nl], want[nl:]

got_header = b"\n".join(got[:2]) + b"\n"
got_body = bytes.fromhex(b"".join(got[2:]).decode("ascii"))

print("header:", "same" if got_header == header else "DIFFERS %r %r" % (got_header, header))
print("body:", "same" if got_body == body else "DIFFERS %d vs %d bytes" % (len(got_body), len(body)))
if got_body != body:
    for i, (a, b) in enumerate(zip(got_body, body)):
        if a != b:
            print("first differing byte at %d: %02x vs %02x" % (i, a, b))
            break
    sys.exit(1)
