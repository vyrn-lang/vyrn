# std/grammar

The language of a `.syntax` file: the file format, `std/syntax.syntax`, which
the toolchain's `**/*.syntax` row of the languages map names.

The editor reads a grammar file through this module and its companion,
`std/grammar.editor.vyrn`, to which it exports how a grammar file names its
rules: a definition starts in column 0, and its head is the name that opens
the line, after an optional `nest`.

_No exported declarations._
