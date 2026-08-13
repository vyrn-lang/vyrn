//! Structured diagnostics for the Vyrn front end.
//!
//! Every stage of the pipeline (lex → parse → check → movecheck) reports problems
//! as a [`Diagnostic`] rather than a free-form string. A diagnostic carries a
//! precise source position (1-based `line`, and a `col`/`end_col` range where the
//! stage knows it), a severity, the stage that produced it, and a message.
//!
//! [`Diagnostic::render`] reproduces the historical `"line {N}: {message}"` string
//! the CLI and the test suite expect, so existing callers see byte-identical
//! output. The structured fields are additive metadata for tooling (the LSP) that
//! wants ranges rather than a single line of text.

/// How serious a diagnostic is.
///
/// An `Error` fails the load; a `Warning` rides a load that SUCCEEDED
/// (RFC-0071 M2b). The two therefore travel on different channels — errors in
/// the `Err` arm of [`crate::load`], warnings alongside the program — because a
/// warning must never change an exit code or a byte of program output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A single problem found in a Vyrn source, with position and provenance.
///
/// Positions are 1-based. `col`/`end_col` are `0` when a stage knows only the
/// line (the whole line is then the natural range); `end_col` of `0` otherwise
/// means "point" (the LSP treats it as a single character).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// The module (file) the problem is in, when it is not the root document —
    /// set by the loader/checker for multi-file programs (RFC-0010). `None`
    /// means "the file being compiled" (single-file programs, or the root).
    pub file: Option<String>,
    /// 1-based source line.
    pub line: usize,
    /// 1-based start column, or `0` for "whole line / unknown column".
    pub col: usize,
    /// 1-based inclusive end column, or `0` for whole-line / single-character.
    pub end_col: usize,
    pub severity: Severity,
    /// `"lex"` | `"parse"` | `"check"` | `"movecheck"`.
    pub stage: &'static str,
    pub message: String,
    /// An optional secondary note (RFC-0033). When a diagnostic in a synthesized
    /// generator module is remapped to its origin input file, the original
    /// generated location is preserved here (`"note: in generated code …"`); a
    /// malformed origin directive records why it could not be followed. `None`
    /// for every ordinary diagnostic.
    pub note: Option<String>,
    /// `true` once [`crate::origin::OriginMaps::remap`] has relocated this
    /// diagnostic out of a synthesized generator module and onto the input file
    /// (`.vyx`, …) the generated text came from (RFC-0033, RFC-0053). `file`
    /// then names a real file the user can open, so the LSP publishes it against
    /// that file's URI instead of adopting it into the root at line 0.
    pub from_generated: bool,
}

impl Diagnostic {
    /// Build an error diagnostic for `stage` at `(line, col)`. `col == 0` means
    /// the whole line is the relevant range.
    pub fn error(line: usize, col: usize, stage: &'static str, message: String) -> Self {
        Diagnostic {
            file: None,
            line,
            col,
            end_col: 0,
            severity: Severity::Error,
            stage,
            message,
            note: None,
            from_generated: false,
        }
    }

    /// Build a WARNING diagnostic for `stage` at `(line, col)` — a problem that
    /// does not fail the load (RFC-0071 M2b).
    ///
    /// A warning is advice about a program that compiled, so it is carried
    /// beside the [`crate::ast::Program`] rather than in place of it, and every
    /// consumer prints it without touching an exit code.
    pub fn warning(line: usize, col: usize, stage: &'static str, message: String) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(line, col, stage, message)
        }
    }

    /// The historical single-line rendering: `"line {N}: {message}"`.
    ///
    /// This is deliberately independent of `col`/`end_col` so the structured
    /// fields can grow without churning the CLI output or the test suite.
    pub fn render(&self) -> String {
        format!("line {}: {}", self.line, self.message)
    }
}
