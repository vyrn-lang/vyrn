#!/usr/bin/env python3
"""Writes `examples/jsondecode.stdin`: the payload `examples/jsondecode.vyrn`
decodes, one order document per line.

The orders hold nested records, arrays of records, optional fields, a map, an
enum, a `where` type, strings with every JSON escape and raw UTF-8, integers,
short decimals and 17-digit floats. Keys come in a shuffled order on a third
of the lines, and unknown keys appear beside known ones. One line in twenty-five
carries a defect: a refused `where`, a missing field, a wrong kind, an unknown
tag, or a syntax error.

The generator is its own SplitMix64, so the bytes do not depend on Python's
`random`. Run: python bench/jsondecode.py
"""

import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "examples", "jsondecode.stdin")
ORDERS = 400
MASK = (1 << 64) - 1


class Rng:
    def __init__(self, seed):
        self.s = seed

    def next(self):
        self.s = (self.s + 0x9E3779B97F4A7C15) & MASK
        z = self.s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        return z ^ (z >> 31)

    def below(self, n):
        return self.next() % n

    def pick(self, xs):
        return xs[self.below(len(xs))]

    def unit(self):
        return (self.next() >> 11) / float(1 << 53)


WORDS = ["lamp", "desk", "cable", "kettle", "chair", "mug", "notebook", "pen",
         "router", "shelf", "blanket", "clock", "plant", "speaker", "tile"]
ODD = ["caf\u00e9", "na\u00efve", "\u65e5\u672c", "\u00fcber", "\U0001f600",
       "say \"hi\"", "back\\slash", "tab\there", "two\nlines", "a/b"]
CITIES = ["Lisbon", "Krak\u00f3w", "S\u00e3o Paulo", "Z\u00fcrich", "Osaka",
          "Austin", "Reykjav\u00edk", "Nairobi"]
COUNTRIES = ["PT", "PL", "BR", "CH", "JP", "US", "IS", "KE"]
STATUS = ["Pending", "Paid", "Shipped", "Delivered", "Returned"]
TAGS = ["new", "sale", "gift", "fragile", "eco", "bulk", "\u2605top"]


def text(r, n):
    words = [r.pick(ODD) if r.below(6) == 0 else r.pick(WORDS) for _ in range(n)]
    return " ".join(words)


def enc(s, r):
    # Half the strings escape every non-ASCII character as `\uXXXX`.
    return json.dumps(s, ensure_ascii=r.below(2) == 0)


def num(r):
    # Short decimals, as a shop writes them, and full 17-digit doubles, as a
    # JavaScript producer writes them.
    if r.below(3) == 0:
        return repr(r.unit() * 1000.0)
    return "%d.%02d" % (r.below(500), r.below(100))


def obj(pairs, r, shuffle):
    if shuffle:
        pairs = pairs[:]
        for i in range(len(pairs) - 1, 0, -1):
            j = r.below(i + 1)
            pairs[i], pairs[j] = pairs[j], pairs[i]
    return "{" + ",".join(json.dumps(k) + ":" + v for k, v in pairs) + "}"


def address(r, shuffle):
    pairs = [
        ("street", enc("%d %s Street" % (1 + r.below(900), r.pick(WORDS).title()), r)),
        ("city", enc(r.pick(CITIES), r)),
        ("zip", json.dumps("%05d" % r.below(100000))),
        ("country", json.dumps(r.pick(COUNTRIES))),
    ]
    if r.below(3) != 0:
        geo = [("lat", repr(r.unit() * 180.0 - 90.0)), ("lon", repr(r.unit() * 360.0 - 180.0))]
        pairs.append(("geo", obj(geo, r, shuffle)))
    elif r.below(2) == 0:
        pairs.append(("geo", "null"))
    return obj(pairs, r, shuffle)


def item(r, shuffle):
    tags = [json.dumps(r.pick(TAGS), ensure_ascii=False) for _ in range(r.below(4))]
    pairs = [
        ("sku", json.dumps("SKU-%06d" % r.below(1000000))),
        ("title", enc(text(r, 2 + r.below(5)), r)),
        ("qty", str(1 + r.below(12))),
        ("price", num(r)),
        ("tags", "[" + ",".join(tags) + "]"),
    ]
    if r.below(4) == 0:
        pairs.append(("note", enc(text(r, 3 + r.below(8)), r)))
    if r.below(5) == 0:
        pairs.append(("warehouse", obj([("bin", str(r.below(99))), ("aisle", json.dumps("A%d" % r.below(9)))], r, shuffle)))
    return obj(pairs, r, shuffle)


def order(r, i, shuffle):
    shuffle = r.below(3) == 0 or shuffle
    customer = [
        ("id", str(100000 + r.below(900000))),
        ("name", enc(text(r, 2), r)),
        ("email", json.dumps("user%d@example.com" % r.below(100000))),
        ("vip", "true" if r.below(5) == 0 else "false"),
        ("address", address(r, shuffle)),
    ]
    items = [item(r, shuffle) for _ in range(1 + r.below(6))]
    attrs = [(r.pick(WORDS), str(r.below(1000))) for _ in range(r.below(4))]
    attrs = list(dict(attrs).items())
    pairs = [
        ("id", str(i + 1)),
        ("placed", json.dumps("2026-%02d-%02dT%02d:%02d:00Z" % (1 + r.below(12), 1 + r.below(28), r.below(24), r.below(60)))),
        ("status", json.dumps(r.pick(STATUS))),
        ("customer", obj(customer, r, shuffle)),
        ("items", "[" + ",".join(items) + "]"),
        ("attrs", obj(attrs, r, False)),
        ("total", num(r)),
        ("_rev", json.dumps("%016x" % r.next())),
    ]
    if r.below(3) == 0:
        pairs.append(("shipping", address(r, shuffle)))
    if r.below(4) == 0:
        pairs.append(("coupon", enc("SAVE" + str(r.below(50)), r)))
    elif r.below(3) == 0:
        pairs.append(("coupon", "null"))
    return obj(pairs, r, shuffle)


# The defects, a list of textual edits of a good line each. The two-edit
# entries put two Issues in one document, whose document order differs from
# the declaration order on a shuffled line.
DEFECTS = [
    [('"qty":', '"qty":0,"was":')],
    [('"email":', '"mail":')],
    [('"id":', '"id":"x","old":')],
    [('"status":"', '"status":"Lost","was":"')],
    [('"vip":', '"vip":null,"was":')],
    [('"tags":[', '"tags":[7],"was":[')],
    [('"total":', '"total":"0","old":')],
    [('"street":', '"street":1,"was":')],
    [('"items":[', '"items":[1,')],
    [('}', ',}')],
    [('"id":', '"id":1,"id":')],
    [('"customer":', '"customer":[],"was":')],
    [('"email":', '"mail":'), ('"qty":', '"qty":1001,"was":')],
    [('"zip":', '"zip":9,"was":'), ('"country":', '"country":false,"old":')],
    [('"sku":', '"sku":null,"was":'), ('"price":', '"price":[],"old":')],
]


def main():
    r = Rng(20261021)
    lines = []
    for i in range(ORDERS):
        broken = i % 25 == 24
        # Every other broken line is shuffled, so its Issues' document order
        # and declaration order differ.
        line = order(r, i, broken and (i // 25) % 2 == 1)
        if broken:
            for old, new in DEFECTS[(i // 25) % len(DEFECTS)]:
                assert old in line, (i, old)
                line = line.replace(old, new, 1)
        lines.append(line)
    data = ("\n".join(lines) + "\n").encode("utf-8")
    with open(OUT, "wb") as f:
        f.write(data)
    print("%s %d bytes, %d lines" % (OUT, len(data), len(lines)))


if __name__ == "__main__":
    main()
