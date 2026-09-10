//! The spawn-isolation rule's slot — RFC-0125 §3 M6, the isolation slice.
//!
//! RFC-0004 §Q4 says a task is isolated: it may read immutable data and it may
//! not touch anything a caller or another task can see. That is the effect
//! judgment's question, and the judgment is `vyrn_lower::effects`, which
//! depends on this crate and cannot be named from it. So the shape is the
//! placer's and the floor's ([`crate::own::Placer`],
//! [`crate::floor::Judge`]): a function pointer the CLI and the editor install
//! at start-up, called on a CHECKED program.
//!
//! The rule was a pair of fixpoints inside `checker.rs` until this slice. It
//! could not read the judgment there, because the judgment reads the named core
//! and no core exists while the checker is still deciding what a node's type
//! is. It is stated after the check instead, in the same place and by the same
//! rule as the floor's held decision: a program with errors gets its errors.

use crate::ast::Program;
use crate::diagnostics::Diagnostic;

/// A judgment that says which `spawn` sites of a checked program are not
/// isolated. The module key of each refusal is the file the spawning body came
/// from.
pub type Judge = fn(&Program) -> Vec<Diagnostic>;

static JUDGE: std::sync::OnceLock<Judge> = std::sync::OnceLock::new();

/// Install the judgment. The first installation wins; a second is ignored.
pub fn install_judge(f: Judge) {
    let _ = JUDGE.set(f);
}

/// The isolation refusals of a checked program, or none when no judgment is
/// installed.
pub fn refusals(program: &Program) -> Vec<Diagnostic> {
    match JUDGE.get() {
        Some(j) => j(program),
        None => Vec::new(),
    }
}
