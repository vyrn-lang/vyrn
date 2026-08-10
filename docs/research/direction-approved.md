# Website direction: approved

The design brief (`website-brief.md` §8.0) requires three rendered directions,
a stop, and a recorded choice. Three drafts were built and are kept in
`hero-drafts/`:

| Draft | Shape |
| --- | --- |
| `a-terminal.html` | A terminal session: the page as a shell transcript |
| `b-specimen.html` | A print specimen sheet: twelve columns, a numbered margin rail, hairline rules, plates with captions |
| `c-instrument.html` | An instrument panel: gauges and readouts around a central display |

## The choice

**Direction B, the specimen sheet.** Its visual language ships unchanged: warm
paper, the twelve-column grid, the numbered rail in the margin, hairline rules
instead of cards, sharp corners, monospace for every label and number, and a
caption under every plate that says where its content came from.

The colour is `editor/vscode/icons/vyrn.svg`, eyedropped and converted to
`oklch` — the four values and the argument for them are a comment at the top of
`site/public/style.css`.

## Two corrections, and what they changed

**1. The messaging was rejected.** Draft B led with *"A language whose three
backends agree to the last bit."* Parity is an internal quality proof, not a
reason anyone adopts a language. The landing page now leads with what Vyrn is
for its reader — the expressiveness of TypeScript, native speed, no collector,
types that carry the rules that make a value valid, ownership written as one
word — all of it drawn from `README.md`. Parity moved to band 04, under the
heading *how we know it works*, where it is genuinely strong.

**2. The animation was rejected.** "Two rotating plane waves" depicts nothing.
The hero must show a Vyrn idea a viewer can name at a glance. Three concepts
were drawn:

1. **The typed gate** — raw glyph noise drifts in from the left, crosses a
   bright vertical seam, and leaves as an ordered lattice.
2. **Three lanes** — one source stream fans into three lanes rendering the same
   pattern in lockstep.
3. **The mark** — drifting characters converge into the vortex logo.

(1) ships. The eye reads disorder, a boundary, and order without a caption, and
what it depicts is the language's first claim: a type carries the rule that makes
a value valid, so untrusted bytes enter and values that satisfy the rule leave.
(2) draws parity, which the correction above demoted. (3) draws a logo and says
nothing about the language.

The field is Vyrn (`examples/herofield.vyrn`), compiled to WebAssembly and
called once per frame from the page; the host only paints. `main` prints the
field's raw IEEE-754 bits, and the parity harness compares all three backends
on them.
