//! Structured diagnostics. Every stage (lex, parse, check, movecheck) reports a
//! [`Diagnostic`] with a position, a severity, its stage and a message;
//! [`Diagnostic::render`] gives the `"line {N}: {message}"` string the CLI and
//! the tests expect, and the LSP reads the ranges.

/// How serious a diagnostic is.
///
/// An `Error` fails the load; a `Warning` rides a load that succeeded.
/// Errors travel in the `Err` arm of [`crate::load`] and warnings
/// beside the program, so a warning never changes an exit code or a byte of
/// output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found in a Vyrn source, with position and provenance.
///
/// Every position is 1-based and counted in Unicode scalar values (`char`s),
/// as the lexer numbers them; the LSP adapter subtracts one. LSP defaults to
/// UTF-16 units, which differ only after a character outside the Basic
/// Multilingual Plane on the same line; one convention for every stage is
/// worth that.
///
/// `col` and `end_col` are `0` when a stage knows only the line (the range is
/// the whole line); an `end_col` of `0` otherwise means a single character.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// The module the problem is in, when it is not the root. `None`
    /// means the file being compiled.
    pub file: Option<String>,
    /// 1-based source line.
    pub line: usize,
    /// 1-based start column, or `0` for the whole line.
    pub col: usize,
    /// 1-based inclusive end column, or `0` for the whole line or one character.
    pub end_col: usize,
    pub severity: Severity,
    /// `"lex"` | `"parse"` | `"check"` | `"movecheck"`.
    pub stage: &'static str,
    pub message: String,
    /// A secondary note: the generated location of a diagnostic
    /// remapped to its origin file, or why an origin directive could not be
    /// followed.
    pub note: Option<String>,
    /// Set once [`crate::origin::OriginMaps::remap`] moved this diagnostic out of
    /// a generated module onto the input file it came from.
    /// `file` then names a real file, and the LSP publishes against its URI.
    pub from_generated: bool,
}

impl Diagnostic {
    /// Builds an error for `stage` at `(line, col)`; `col == 0` means the whole
    /// line.
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

    /// Builds a warning for `stage` at `(line, col)`: advice about a program that
    /// compiled, printed without touching an exit code.
    pub fn warning(line: usize, col: usize, stage: &'static str, message: String) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(line, col, stage, message)
        }
    }

    /// Places the diagnostic in `file`; `None` means the root module.
    pub fn in_file(self, file: Option<String>) -> Self {
        Diagnostic { file, ..self }
    }

    /// Narrows the diagnostic to the 1-based columns `col..=end_col`.
    pub fn at(self, col: usize, end_col: usize) -> Self {
        Diagnostic {
            col,
            end_col,
            ..self
        }
    }

    pub fn with_note(self, note: String) -> Self {
        Diagnostic {
            note: Some(note),
            ..self
        }
    }

    /// Renders `"line {N}: {message}"`, independent of the columns so the CLI
    /// output and the tests stay stable.
    pub fn render(&self) -> String {
        format!("line {}: {}", self.line, self.message)
    }
}

/// Opens a fix line under a refusal's sentence; `vyrn fix` reads a menu by it.
pub const FIX: &str = "fix: ";

/// Appends one `  fix: ...` line per way out of `sentence`, in order.
pub fn menu(sentence: String, fixes: impl IntoIterator<Item = impl std::fmt::Display>) -> String {
    let mut message = sentence;
    for f in fixes {
        message.push_str(&format!("\n  {FIX}{f}"));
    }
    message
}
