#!/usr/bin/env python3
"""Measure the exported consumer pages: words, blocks, bytes, commands, captions.

RFC-0106 M0. The ceilings that milestone writes are numbers, and a number
nobody can recompute is an adjective with a digit in front of it. So this reads
the tree `site/export.vyrn` wrote and prints the census table the RFC carries.

    python3 scripts/site-census.py out
    python3 scripts/site-census.py out --json     # for a later budget gate

WHAT COUNTS AS A WORD. Prose only: the text a reader reads in sentences. The
contents of `<script>`, `<style>`, `<svg>`, `<template>`, `<pre>`, `<code>`,
`<table>` and `<textarea>` are removed first, with their markup, because a
keyword in a code plate and a cell in a benchmark table are not prose and
shrinking them is not the point of the word budget. Comments go too. What is
left is entity-decoded and split on whitespace; a token counts if it holds a
letter, so `—`, `·` and a bare `2.1` do not inflate the figure.

Attribute text is NOT counted, and that is a decision rather than an oversight:
an `aria-label` is prose a screen reader hears, but it is also the thing the
accessibility checklist requires, so a budget that counted it would reward
deleting it.

WHAT COUNTS AS A BLOCK. Three things, reported separately because the fix for
each is different:

  sections  `<section>` elements — the page's outline, what a reader scans.
  plates    `class="plate"` — the sheet's bordered panel (see style.css).
  widgets   `class="stage"` mounts, `<svg class="chart">`, `<canvas>` — the
            things that draw rather than say.

`blocks` is their sum. Sections contain plates, so the sum is a density
reading and not a count of disjoint objects; it is comparable across pages,
which is all a ceiling needs.

COMMANDS is `class="cmd"` — the copy-button command row, one per command a
reader would type. `code.copyable` (a copiable word inside a sentence) is
counted separately as `copyable`: it is a different affordance and RFC-0106
asks for the first kind, not the second.

CAPTIONS is `class="cap"` — the caption THE RULE targets, the one that says
where a plate's content came from ("read from `rfcs/` while this page was
built"). `meta` is `class="note"` plus `class="notice"`: the other two ways the
sheet says something beside the claim. Both are counted because THE RULE's
ceiling is "at most one disclosure per plate, zero inline", and the second half
of that sentence is about these.
"""

import html
import json
import os
import re
import sys

# The consumer pages, in the order the census table carries them. A pair is
# (label, path under the export root). The representative chapter, module and
# package are the MEDIAN page of their section by byte size, picked once and
# recorded here so a re-run measures the same page:
#   guide/ownership.html  11,592 B, rank 7 of 13
#   docs/std/json.html    14,722 B, rank 19 of 37
#   explore/shelf.html     8,167 B, the upper of the two middle pages of 4
#                          (they run 6,919 to 8,184), and the fullstack dogfood
PAGES = [
    ("index", "index.html"),
    ("install", "install.html"),
    ("philosophy", "philosophy.html"),
    ("compare", "compare.html"),
    ("releases", "releases.html"),
    ("guide (landing)", "guide.html"),
    ("guide/ownership", "guide/ownership.html"),
    ("docs (landing)", "docs.html"),
    ("docs/std/json", "docs/std/json.html"),
    ("explore (landing)", "explore.html"),
    ("explore/shelf", "explore/shelf.html"),
    ("editors", "editors.html"),
    ("play", "play.html"),
]

# Everything whose text is not prose. Removed with its markup, innermost first
# is not needed — none of these nest inside each other in this tree except
# `<code>` inside `<pre>`, and a non-greedy match per tag handles that.
NOT_PROSE = ("script", "style", "svg", "template", "pre", "code", "table", "textarea")


def cls(body, name):
    """How many elements carry `name` as one of their classes."""
    return len(re.findall(r'class="[^"]*(?<![\w-])' + name + r'(?![\w-])', body))


def prose_words(page):
    """The prose word count of a whole HTML document."""
    body = re.sub(r"(?is)^.*?<body[^>]*>", "", page)
    body = re.sub(r"(?s)<!--.*?-->", " ", body)
    for tag in NOT_PROSE:
        body = re.sub(r"(?is)<" + tag + r"\b.*?</" + tag + r"\s*>", " ", body)
        # An unclosed one would swallow the rest of the page; a self-closing or
        # void form of these tags does not exist, so a leftover open tag is a
        # malformed export and is dropped as a tag, not as a region.
    text = html.unescape(re.sub(r"(?s)<[^>]*>", " ", body))
    return sum(1 for w in text.split() if re.search(r"[A-Za-z]", w))


def measure(path):
    raw = open(path, "rb").read()
    page = raw.decode("utf-8")
    body = re.sub(r"(?is)^.*?<body[^>]*>", "", page)
    sections = len(re.findall(r"(?i)<section\b", body))
    plates = cls(body, "plate")
    widgets = (
        cls(body, "stage")
        + len(re.findall(r'(?is)<svg[^>]*class="[^"]*\bchart\b', body))
        + len(re.findall(r"(?i)<canvas\b", body))
    )
    return {
        "words": prose_words(page),
        "sections": sections,
        "plates": plates,
        "widgets": widgets,
        "blocks": sections + plates + widgets,
        "bytes": len(raw),
        "commands": cls(body, "cmd"),
        "copyable": cls(body, "copyable"),
        "captions": cls(body, "cap"),
        "meta": cls(body, "note") + cls(body, "notice"),
    }


COLUMNS = [
    ("words", "Words"),
    ("sections", "Sec"),
    ("plates", "Plates"),
    ("widgets", "Widgets"),
    ("blocks", "Blocks"),
    ("bytes", "Bytes"),
    ("commands", "Cmds"),
    ("copyable", "Copyable"),
    ("captions", "`.cap`"),
    ("meta", "`.note`/`.notice`"),
]


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    root = args[0] if args else "out"
    rows = {}
    for label, rel in PAGES:
        path = os.path.join(root, rel)
        if not os.path.exists(path):
            sys.exit("no such page: " + path + " (has site/export.vyrn run?)")
        rows[label] = measure(path)

    if "--json" in sys.argv[1:]:
        print(json.dumps(rows, indent=2))
        return

    head = "| Page | " + " | ".join(t for _, t in COLUMNS) + " |"
    print(head)
    print("|---|" + "---:|" * len(COLUMNS))
    for label, _ in PAGES:
        r = rows[label]
        cells = [format(r[k], ",") if k == "bytes" else str(r[k]) for k, _ in COLUMNS]
        print("| " + label + " | " + " | ".join(cells) + " |")
    tot = {k: sum(r[k] for r in rows.values()) for k, _ in COLUMNS}
    print(
        "| **all thirteen** | "
        + " | ".join(
            format(tot[k], ",") if k == "bytes" else str(tot[k]) for k, _ in COLUMNS
        )
        + " |"
    )


if __name__ == "__main__":
    main()
