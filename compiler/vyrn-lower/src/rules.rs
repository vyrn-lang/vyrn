//! How the ownership and typed judgments state a rule. The rows are
//! [`vyrn_frontend::rules::Rule`]'s; a site decides that its rule holds and
//! builds the row with [`vyrn_frontend::rule!`].
//!
//! The kernel judges the flow rules on its solver: at a use (shape A), where
//! a scope ends (B), where edges join (C) and at a loop's back edge (D).
//! The flow-free rules (E) filter single rows: the kernel's at its use
//! sites, `typed`'s over every row including rows after an ended path, and
//! the builder's at the construct.

use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::own::Exit;
use vyrn_frontend::rules::{Hole, Rule};

/// The `movecheck` error `rule` states at `line`, with the ways out `more`
/// under the row's own. A sentence quotes what the reader wrote, so a
/// compiler temporary (`@t1`, `@p3`) in backticks is a defect in the site
/// that named it; debug builds panic on one.
pub fn refusal(line: usize, rule: Rule, more: Vec<String>) -> Diagnostic {
    let d = Diagnostic::refusal_with(line, 0, "movecheck", rule, more);
    debug_assert!(
        !d.message.contains("`@"),
        "a refusal names a compiler temporary: {}",
        d.message
    );
    d
}

/// The binding a must-use row ([`Rule::NeverDisposed`],
/// [`Rule::DisposedTwice`]) is about; `None` for any other diagnostic. A
/// binding earns one such row per mistake however many instances and paths
/// reach it.
pub fn owed(d: &Diagnostic) -> Option<&str> {
    match &d.rule {
        Some(Rule::NeverDisposed { s, .. } | Rule::DisposedTwice { s, .. }) => match s {
            Hole::Text(s) => Some(s),
            _ => unreachable!("the kernel states a must-use row with the binding as text"),
        },
        _ => None,
    }
}

/// An exit, as [`Rule::HeldAtExit`] names it.
pub fn exit_words(e: Exit) -> &'static str {
    match e {
        Exit::Block => "the end of its scope",
        Exit::Return => "a `return`",
        Exit::Try => "a `?`",
        Exit::Break => "a `break`",
        Exit::Continue => "a `continue`",
        Exit::Scrutinee => "a scrutinee",
    }
}
