//! The lowered form as text — what `vyrn emit-lowered` prints (RFC-0101 §2.7).
//!
//! Five rules, and each one is there so a question can be answered with `grep`
//! instead of a reading:
//!
//! 1. **One decision per line**, and indentation is the only structure.
//! 2. **A type on every expression**, so `grep ': Array<'` is a whole query.
//!    Two, where the node's own answer and its destination differ: `Int64 =>
//!    Int32` is a value that HAS an `Int64` and must END UP an `Int32`, which is
//!    the pair of RFC-0101 §2.1 item 2 [A16] and the thing `coerce` is for.
//! 3. **The position is the last column**, so a diff that only moves lines looks
//!    different from a diff that changes a decision, at a glance.
//! 4. **Instantiations are spelled, symbols are not** — `map<Int64, String>`,
//!    never `mangle_ty`'s output. Defect #165 was a mangled string used as an
//!    identity, and a dump that shows the mangle invites it into every report.
//! 5. **Deterministic**, and that is a gate rather than an intention:
//!    `vyrn-cli/tests/reproducible.rs` runs seven separate compilers and compares
//!    bytes, because a `HashSet` iterates identically twice inside one process.
//!
//! It promises nothing about its format. Every line above the first `fn` is a
//! comment, the first of them carries a version, and stability is a blessed
//! snapshot rather than a contract (RFC-0101 §6.5, which is rustc's answer).
//!
//! There is no parser. A parser would be a second front end written to test a
//! printer, and it is the largest thing this RFC could accidentally acquire.

use vyrn_frontend::ast::{Expr, Stmt};
use vyrn_frontend::types::substitute;

use crate::{Instance, Lowered, Node, VERSION};

/// The column the position starts at. Wide enough for the corpus's deepest
/// nesting and its longest type; a longer line pushes its own position right
/// rather than truncating, because a truncated type is a lie.
const POS_COLUMN: usize = 72;

/// Render `lowered`'s ROOT-module instances.
///
/// Root-module only, following the rule `vyrn why --memory` already wrote down:
/// "Only the file asked about. A linked program carries every import's
/// functions, and they are another file's answer." The median example is 67
/// lines; its linked program is thousands.
pub fn render(lowered: &Lowered, source: &str) -> String {
    let mut out = format!("; vyrn lowered {VERSION} — {source}\n");
    let roots: Vec<&Instance> = lowered.root().collect();
    if roots.is_empty() {
        out.push_str("; (this module declares no functions)\n");
    }
    for inst in roots {
        out.push('\n');
        out.push_str(&signature(inst));
        for row in &inst.rows {
            let mut line = String::new();
            for _ in 0..=row.depth {
                line.push_str("  ");
            }
            line.push_str(&row_text(row.node, row.ty.as_ref(), row.has.as_ref()));
            pos(&mut out, line, row.line);
        }
        // The releases, in the order they run — RFC-0101 §2.7 sketched these and
        // M4 is where they exist. They follow the rows rather than sitting
        // inside them because a block exit is not a node: it is a point between
        // two of them, and giving it a row would put a decision on a line the
        // source has nothing at.
        for rel in &inst.releases {
            let exit = match rel.exit {
                crate::Exit::Block => "block",
                crate::Exit::Scrutinee => "scrutinee",
                crate::Exit::Break => "break",
                crate::Exit::Continue => "continue",
                crate::Exit::Return => "return",
                crate::Exit::Try => "try",
            };
            pos(
                &mut out,
                format!(
                    "  release {} : {} exit={exit}",
                    rel.name,
                    kind_text(&rel.kind)
                ),
                rel.line,
            );
        }
    }
    out
}

/// A release kind in one token, so `grep 'release .* Release'` lists every place
/// a user's own `release` runs.
fn kind_text(k: &vyrn_frontend::own::DropKind) -> String {
    use vyrn_frontend::own::DropKind as K;
    match k {
        K::FreeStr => "FreeStr".into(),
        K::FreeArr => "FreeArr".into(),
        K::FreeSmallArr => "FreeSmallArr".into(),
        K::FreeMap => "FreeMap".into(),
        K::CloseStream => "CloseStream".into(),
        K::Deep(t) => format!("Deep<{t}>"),
        K::Release(f) => format!("Release {f}"),
    }
}

fn signature(inst: &Instance) -> String {
    let subst = inst.subst.clone().into_iter().collect();
    let params: Vec<String> = inst
        .func
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, substitute(&p.ty, &subst)))
        .collect();
    let mut head = format!(
        "fn {}({}) -> {}",
        inst.spelling(),
        params.join(", "),
        substitute(&inst.func.ret, &subst)
    );
    if let Some(m) = &inst.func.module {
        head.push_str(&format!("  ; from {m}"));
    }
    let mut out = String::new();
    pos(&mut out, head, inst.func.line as u32);
    out
}

/// Append `text`, padded so `@line` lands in the position column.
fn pos(out: &mut String, text: String, line: u32) {
    out.push_str(&text);
    for _ in text.chars().count()..POS_COLUMN {
        out.push(' ');
    }
    out.push_str(&format!(" @{line}\n"));
}

/// One row's head: what the node is, what it names, and what type it has.
fn row_text(
    node: Node,
    ty: Option<&vyrn_frontend::ast::Type>,
    has: Option<&vyrn_frontend::ast::Type>,
) -> String {
    let mut s = node.kind().to_string();
    if let Some(name) = names(node) {
        s.push(' ');
        s.push_str(&name);
    }
    match (has, ty) {
        (Some(h), Some(t)) => s.push_str(&format!(" : {h} => {t}")),
        (None, Some(t)) | (Some(t), None) => s.push_str(&format!(" : {t}")),
        (None, None) => {}
    }
    s
}

/// The one name a row's node carries, where it has one. Deliberately shallow:
/// the operands are their own rows, so repeating them here would be a second
/// rendering of the same decision.
fn names(node: Node) -> Option<String> {
    Some(match node {
        Node::Stmt(s) => match s {
            Stmt::Let { name, mutable, .. } => {
                if *mutable {
                    format!("mut {name}")
                } else {
                    name.clone()
                }
            }
            Stmt::Assign { name, .. } | Stmt::Drop { name, .. } => name.clone(),
            Stmt::SetField { name, field, .. } => format!("{name}.{field}"),
            Stmt::IndexSet { name, .. } => format!("{name}[]"),
            Stmt::ForIn { var, consuming, .. } => {
                if *consuming {
                    format!("{var} in consume")
                } else {
                    var.clone()
                }
            }
            _ => return None,
        },
        Node::Expr(e) => match e {
            Expr::Int(v) => v.to_string(),
            Expr::Byte(v) => format!("{v}"),
            Expr::Float(v) => format!("{v}"),
            Expr::Bool(v) => v.to_string(),
            // A literal is the one place the dump quotes source, because the
            // bytes ARE the decision and nothing else on the line says them.
            Expr::Str(v) => format!("{v:?}"),
            Expr::Var { name, .. } => name.clone(),
            Expr::Call { name, .. } | Expr::Spawn { name, .. } => name.clone(),
            Expr::StructLit { name, .. } | Expr::TryConstruct { name, .. } => name.clone(),
            Expr::Field { field, .. } => format!(".{field}"),
            Expr::Binary { op, .. } => format!("{op:?}"),
            Expr::Unary { op, .. } => format!("{op:?}"),
            _ => return None,
        },
    })
}
