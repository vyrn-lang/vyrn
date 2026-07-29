//! Lowering Vyrn straight to wasm, with no LLVM in between (RFC-0077 M2).
//!
//! M2a is a vertical slice, not the lowering: enough of the traversal to take
//! one example — `examples/fib.vyrn`: functions, recursion, `if`, comparison,
//! `print`, `return`, an exit code — from AST to a module that runs under
//! wasmtime and matches the interpreter byte for byte. Breadth is M2b's problem,
//! and the ladder in `vyrn-cli/tests/directwasm.rs` is how it gets measured.
//!
//! Everything not yet lowered is [`unsupported`]: a named construct and a source
//! line, never a fallback to the LLVM path. A silent fallback would make the
//! ladder report a number that is not about this backend at all, and the ladder
//! is the milestone's real deliverable.
//!
//! # The three constraints this is built around
//!
//! **Structured control flow, straight from the AST** (M2's pre-flight). wasm has
//! no `goto`, and this needs no relooper because `if`/`while`/`for` map onto
//! `if`/`block`/`loop` and `break`/`continue` onto `br <depth>`. What that costs
//! is bookkeeping: every construct that opens a wasm block pushes one onto
//! [`Fn_::depth`], because a `return` is a `br` past all of them.
//!
//! **A body must not emit `return`** (M1). It would jump past the shadow-stack
//! epilogue `wasm::Module::func` emits and leak the frame for the rest of the
//! program. So a body is wrapped in one `block` whose result is the function's,
//! and `return` is a `br` to it — which is also why `depth` has to be exact
//! rather than approximately right.
//!
//! **Scalars in wasm locals, aggregates in frame slots** (M0). Only the scalar
//! half exists here; the frame is already allocated and addressable
//! ([`wasm::Frame::slot`]) for the aggregate half to land in, and
//! [`print_i64`] uses it, so the convention is exercised rather than merely
//! written down.

use std::collections::HashMap;

use vyrn_frontend::ast::*;

use crate::wasm::{self, BlockType, Instruction, MemArg, Module, ValType};

/// What the direct backend cannot lower yet: the construct, and where.
///
/// One shape for every gap, because the ladder groups its blocker list by the
/// text after the colon — a message that varies by site would report the same
/// gap as several.
fn unsupported<T>(what: &str, line: usize) -> Result<T, String> {
    Err(format!("direct backend: no lowering for {what} at line {line}"))
}

/// Compile a whole program to a standalone `wasm32-wasi` module.
pub fn compile(program: &Program) -> Result<Vec<u8>, String> {
    let mut m = Module::new();
    // Imports first — they share the function index space with definitions, so
    // `wasm::Module` panics if one arrives late.
    let fd_write = m.import(
        "wasi_snapshot_preview1",
        "fd_write",
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
    );
    let proc_exit = m.import("wasi_snapshot_preview1", "proc_exit", &[ValType::I32], &[]);

    // Every function the module will define, in the order they are defined, so a
    // call can name an index before the callee's body exists. Recursion and
    // forward references both need this; there is no fixup pass.
    let print_ix = wasm_index(&m, 0);
    let user: Vec<&Function> = program.functions.iter().filter(|f| !f.is_extern).collect();
    let mut sigs = HashMap::new();
    for (i, f) in user.iter().enumerate() {
        // The return type goes in the table with the index because a call is
        // typed by its callee, and `let x = f(..)` needs that answer before the
        // callee's body has been walked.
        sigs.insert(f.name.clone(), Sig { index: wasm_index(&m, 1 + i as u32), ret: wt(&f.ret).flatten() });
    }
    let entry_ix = wasm_index(&m, 1 + user.len() as u32);

    print_i64(&mut m, fd_write);
    for f in &user {
        lower_fn(&mut m, f, &sigs, print_ix)?;
    }

    // `_start`: WASI's entry point. The exit code is `main & 255`, the same
    // truncation `vyrn_entry` does natively — `vyrn run` and the native binary
    // both give the OS one byte, so wasm has to as well or parity is off by 256.
    let main = sigs
        .get("main")
        .ok_or_else(|| "direct backend: program has no `main`".to_string())?
        .index;
    let start = m.func(&[], &[], &[], 0, |b| {
        b.ins(&Instruction::Call(main))
            .ins(&Instruction::I64Const(255))
            .ins(&Instruction::I64And)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::Call(proc_exit));
    });
    debug_assert_eq!(start, entry_ix);
    m.export("_start", start);
    Ok(m.finish())
}

/// The index the `n`-th function this module defines will get. `Module::func`
/// hands out indices as it is called, and imports sit below them.
fn wasm_index(m: &Module, n: u32) -> u32 {
    m.n_imports() + n
}

/// The wasm type a Vyrn type crosses as, or `None` for `Unit`.
///
/// Only the scalars M2a covers. An aggregate is an `i32` frame address under
/// the M0 convention, but nothing here can produce one yet, so it is a gap
/// rather than a silent `i32`.
fn wt(t: &Type) -> Option<Option<ValType>> {
    Some(match t {
        Type::Unit => None,
        Type::Int | Type::IntN { bits: 64, .. } => Some(ValType::I64),
        Type::IntN { .. } | Type::Bool => Some(ValType::I32),
        Type::Float => Some(ValType::F64),
        Type::Float32 => Some(ValType::F32),
        _ => return None,
    })
}

/// What a call to a function needs to know about it: where it is, and what it
/// leaves on the stack.
#[derive(Clone, Copy)]
struct Sig {
    index: u32,
    ret: Option<ValType>,
}

type Sigs = HashMap<String, Sig>;

fn ty_name(t: &Type) -> String {
    format!("type `{t}`")
}

/// One function being lowered.
struct Fn_<'a> {
    /// Name → (local index, type). A scope stack rather than a map per block:
    /// shadowing pushes, and leaving a block truncates.
    scope: Vec<(String, u32, ValType)>,
    /// Locals declared past the parameters and the frame base, in the order the
    /// pre-pass found them; `let_ix` walks the same order during lowering, which
    /// is what keeps the two passes agreeing without a second data structure.
    let_ix: usize,
    let_slots: Vec<u32>,
    /// wasm blocks open between here and the function's outermost one. A
    /// `return` is `br depth`.
    depth: u32,
    sigs: &'a Sigs,
    print: u32,
    ret: Option<ValType>,
}

fn lower_fn(
    m: &mut Module,
    f: &Function,
    sigs: &Sigs,
    print: u32,
) -> Result<(), String> {
    if !f.type_params.is_empty() {
        return unsupported(&format!("generic function `{}`", f.name), f.line);
    }
    let mut params = Vec::new();
    for p in &f.params {
        match wt(&p.ty).ok_or_else(|| gap(&ty_name(&p.ty), f.line))? {
            Some(v) => params.push(v),
            None => return unsupported("a Unit parameter", f.line),
        }
    }
    let ret = wt(&f.ret).ok_or_else(|| gap(&ty_name(&f.ret), f.line))?;

    // Pre-pass: one local per `let`, in traversal order.
    let mut lets = Vec::new();
    collect_lets(&f.body, sigs, &mut lets)?;
    // Local numbering is params, then the frame base `wasm::Module::func`
    // always declares, then these.
    let first = params.len() as u32 + 1;
    let let_slots: Vec<u32> = (0..lets.len() as u32).map(|i| first + i).collect();

    let scope: Vec<(String, u32, ValType)> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), i as u32, params[i]))
        .collect();

    let mut err = None;
    m.func(&params, &ret.into_iter().collect::<Vec<_>>(), &lets, 0, |b| {
        let mut cx = Fn_ { scope, let_ix: 0, let_slots, depth: 0, sigs, print, ret };
        // The one block every `return` targets. Its result IS the function's, so
        // a `return` leaves the value on the stack and branches; the epilogue
        // `Module::func` emits after this is stack-neutral, which is what lets
        // the value sit under it.
        b.ins(&Instruction::Block(match ret {
            Some(v) => BlockType::Result(v),
            None => BlockType::Empty,
        }));
        err = cx.block(b, &f.body).err();
        // Falling off the end of a value-returning function is unreachable —
        // the checker proves every path returns — but the validator needs to be
        // told, since it cannot see the proof.
        if ret.is_some() {
            b.ins(&Instruction::Unreachable);
        }
        b.ins(&Instruction::End);
    });
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn gap(what: &str, line: usize) -> String {
    format!("direct backend: no lowering for {what} at line {line}")
}

/// Every `let` in a block, in the order the lowering will meet them.
fn collect_lets(b: &Block, sigs: &Sigs, out: &mut Vec<ValType>) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let { ty, value, line, .. } => {
                let t = match ty {
                    Some(t) => wt(t).ok_or_else(|| gap(&ty_name(t), *line))?,
                    None => Some(infer(value, sigs, *line)?),
                };
                out.push(t.ok_or_else(|| gap("a Unit binding", *line))?);
            }
            Stmt::If { then_block, else_block, .. } => {
                collect_lets(then_block, sigs, out)?;
                if let Some(e) = else_block {
                    collect_lets(e, sigs, out)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The type an initializer with no annotation produces.
///
/// Deliberately shallow — the checker already typed the program, but its result
/// does not reach codegen, and M2a needs exactly enough to place a `let`'s
/// local. Anything it cannot see is a gap, not a guess: guessing here would be a
/// silent miscompile of the kind M0 spends a whole clang test avoiding.
fn infer(e: &Expr, sigs: &Sigs, line: usize) -> Result<ValType, String> {
    Ok(match e {
        Expr::Int(_) | Expr::Byte(_) => ValType::I64,
        Expr::Float(_) => ValType::F64,
        Expr::Bool(_) => ValType::I32,
        Expr::Binary { op, lhs, .. } => match op {
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::LtEq
            | BinOp::Gt
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or
            | BinOp::Match => ValType::I32,
            _ => infer(lhs, sigs, line)?,
        },
        Expr::Unary { expr, .. } => infer(expr, sigs, line)?,
        Expr::Call { name, .. } => match sigs.get(name).map(|s| s.ret) {
            Some(Some(t)) => t,
            // A `let` bound to a Unit call is not a program the checker admits,
            // and an unknown name is a builtin this milestone has not reached.
            _ => return unsupported(&format!("an inferred binding from `{name}`"), line),
        },
        _ => return unsupported("an inferred binding of this shape", line),
    })
}

impl Fn_<'_> {
    fn block(&mut self, b: &mut wasm::Frame, blk: &Block) -> Result<(), String> {
        let mark = self.scope.len();
        for s in &blk.stmts {
            self.stmt(b, s)?;
        }
        self.scope.truncate(mark);
        Ok(())
    }

    fn stmt(&mut self, b: &mut wasm::Frame, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let { name, value, .. } => {
                let t = self.expr(b, value)?;
                let slot = self.let_slots[self.let_ix];
                self.let_ix += 1;
                b.ins(&Instruction::LocalSet(slot));
                self.scope.push((name.clone(), slot, t));
            }
            Stmt::Assign { name, value, line } => {
                let (slot, want) = self.lookup(name, *line)?;
                let got = self.expr(b, value)?;
                if got != want {
                    return unsupported("an assignment that changes width", *line);
                }
                b.ins(&Instruction::LocalSet(slot));
            }
            Stmt::Return { value, line } => {
                match (value, self.ret) {
                    (Some(e), Some(want)) => {
                        let got = self.expr(b, e)?;
                        if got != want {
                            return unsupported("a return that changes width", *line);
                        }
                    }
                    (None, None) => {}
                    _ => return unsupported("a return whose value does not match the signature", *line),
                }
                b.ins(&Instruction::Br(self.depth));
            }
            Stmt::If { cond, then_block, else_block, line } => {
                let c = self.expr(b, cond)?;
                if c != ValType::I32 {
                    return unsupported("a non-boolean `if` condition", *line);
                }
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.block(b, then_block)?;
                if let Some(e) = else_block {
                    b.ins(&Instruction::Else);
                    self.block(b, e)?;
                }
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            Stmt::Expr(e) => {
                // A call for its effect leaves its result on the stack; drop it,
                // or the block's type will not check.
                if self.expr_opt(b, e)?.is_some() {
                    b.ins(&Instruction::Drop);
                }
            }
            other => return unsupported(&stmt_name(other), stmt_line(other)),
        }
        Ok(())
    }

    fn lookup(&self, name: &str, line: usize) -> Result<(u32, ValType), String> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|&(_, s, t)| (s, t))
            .ok_or_else(|| gap(&format!("the name `{name}` (not a local)"), line))
    }

    /// An expression that must produce a value.
    fn expr(&mut self, b: &mut wasm::Frame, e: &Expr) -> Result<ValType, String> {
        match self.expr_opt(b, e)? {
            Some(t) => Ok(t),
            None => unsupported("a Unit value in a value position", Expr::line(e)),
        }
    }

    /// An expression, which may be `Unit` (a call to a `-> Unit` function).
    fn expr_opt(&mut self, b: &mut wasm::Frame, e: &Expr) -> Result<Option<ValType>, String> {
        Ok(Some(match e {
            Expr::Int(v) => {
                b.ins(&Instruction::I64Const(*v));
                ValType::I64
            }
            Expr::Bool(v) => {
                b.ins(&Instruction::I32Const(*v as i32));
                ValType::I32
            }
            Expr::Var { name, line } => {
                let (slot, t) = self.lookup(name, *line)?;
                b.ins(&Instruction::LocalGet(slot));
                t
            }
            Expr::Unary { op, expr, line } => match (op, self.expr(b, expr)?) {
                // `0 - x`, which is also what makes `Int64.min` negate to itself
                // — the wrapping the interpreter does, for free.
                (UnOp::Neg, ValType::I64) => {
                    b.ins(&Instruction::I64Const(-1)).ins(&Instruction::I64Mul);
                    ValType::I64
                }
                (UnOp::Not, ValType::I32) => {
                    b.ins(&Instruction::I32Eqz);
                    ValType::I32
                }
                _ => return unsupported("a unary operator on this type", *line),
            },
            Expr::Binary { op, lhs, rhs, line } => self.binary(b, *op, lhs, rhs, *line)?,
            Expr::Call { name, args, line } => return self.call(b, name, args, *line),
            other => return unsupported(&expr_name(other), Expr::line(other)),
        }))
    }

    fn binary(
        &mut self,
        b: &mut wasm::Frame,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        line: usize,
    ) -> Result<ValType, String> {
        let l = self.expr(b, lhs)?;
        let r = self.expr(b, rhs)?;
        if l != r {
            return unsupported("a binary operator over mixed widths", line);
        }
        // Only i64 for now: the sized integers need width-correct wrapping and
        // the unsigned ones need the other half of every comparison, which is
        // M2b's table rather than a case here.
        if l != ValType::I64 {
            return unsupported(&format!("`{op:?}` on a non-Int64 operand"), line);
        }
        let ins = match op {
            BinOp::Add => Instruction::I64Add,
            BinOp::Sub => Instruction::I64Sub,
            BinOp::Mul => Instruction::I64Mul,
            BinOp::Eq => Instruction::I64Eq,
            BinOp::NotEq => Instruction::I64Ne,
            BinOp::Lt => Instruction::I64LtS,
            BinOp::LtEq => Instruction::I64LeS,
            BinOp::Gt => Instruction::I64GtS,
            BinOp::GtEq => Instruction::I64GeS,
            // Division traps on zero AND on INT64_MIN/-1, with the
            // interpreter's exact wording on stderr — that is a runtime string,
            // a data segment and two branches, so it is its own step.
            _ => return unsupported(&format!("`{op:?}`"), line),
        };
        b.ins(&ins);
        Ok(match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => ValType::I64,
            _ => ValType::I32,
        })
    }

    fn call(
        &mut self,
        b: &mut wasm::Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<ValType>, String> {
        if name == "print" {
            if args.len() != 1 {
                return unsupported("`print` with other than one argument", line);
            }
            let t = self.expr(b, &args[0])?;
            if t != ValType::I64 {
                return unsupported("`print` of a non-Int64 value", line);
            }
            b.ins(&Instruction::Call(self.print));
            return Ok(None);
        }
        let Some(&sig) = self.sigs.get(name) else {
            return unsupported(&format!("the call `{name}`"), line);
        };
        for a in args {
            self.expr(b, a)?;
        }
        b.ins(&Instruction::Call(sig.index));
        Ok(sig.ret)
    }
}

/// `print(n: Int64)`: the decimal digits and a newline, straight to fd 1.
///
/// Written as wasm rather than deferred to the shim because `print` is
/// `printf("%lld\n")` today and varargs are M3 — and because it is the one place
/// M2a touches the shadow stack, so the frame convention gets exercised instead
/// of only asserted. Digits go in backwards from the end of the frame's buffer,
/// which is why the iovec's pointer is computed rather than fixed.
///
/// Unsigned division throughout, so `Int64.min` — whose negation is itself —
/// prints its digits rather than wrapping to nothing.
fn print_i64(m: &mut Module, fd_write: u32) -> u32 {
    // [0,32) digits, [32,40) the iovec, [40,44) fd_write's byte count.
    const BUF_END: u32 = 32;
    const IOV: u32 = 32;
    const NWRITTEN: u32 = 40;
    let (v, p, neg) = (0, 2, 3); // param 0, base is 1, then our two
    m.func(&[ValType::I64], &[], &[ValType::I32, ValType::I32], NWRITTEN + 4, |b| {
        // neg = v < 0; v = |v| as unsigned
        b.ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::I64LtS)
            .ins(&Instruction::LocalTee(neg))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Sub)
            .ins(&Instruction::LocalSet(v))
            .ins(&Instruction::End);
        // p = base + BUF_END - 1; *p = '\n'
        b.slot(BUF_END - 1)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Const(b'\n' as i32))
            .ins(&Instruction::I32Store8(byte()));
        // do { *--p = '0' + v % 10; v /= 10 } while (v)
        b.ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(10))
            .ins(&Instruction::I64RemU)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(10))
            .ins(&Instruction::I64DivU)
            .ins(&Instruction::LocalTee(v))
            .ins(&Instruction::I64Eqz)
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::BrIf(0))
            .ins(&Instruction::End);
        // if (neg) *--p = '-'
        b.ins(&Instruction::LocalGet(neg))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Const(b'-' as i32))
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::End);
        // iov = { p, (base + BUF_END) - p }
        b.slot(IOV).ins(&Instruction::LocalGet(p)).ins(&Instruction::I32Store(word()));
        b.slot(IOV + 4)
            .slot(BUF_END)
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Store(word()));
        // fd_write(1, iov, 1, &nwritten) — the errno is dropped, matching
        // `printf`'s return value going unread in the IR backend.
        b.ins(&Instruction::I32Const(1));
        b.slot(IOV);
        b.ins(&Instruction::I32Const(1));
        b.slot(NWRITTEN);
        b.ins(&Instruction::Call(fd_write)).ins(&Instruction::Drop);
    })
}

fn byte() -> MemArg {
    MemArg { offset: 0, align: 0, memory_index: 0 }
}

fn word() -> MemArg {
    MemArg { offset: 0, align: 2, memory_index: 0 }
}

fn stmt_name(s: &Stmt) -> String {
    match s {
        Stmt::Let { .. } => "`let`",
        Stmt::Assign { .. } => "an assignment",
        Stmt::SetField { .. } => "a field assignment",
        Stmt::IndexSet { .. } => "an element assignment",
        Stmt::Return { .. } => "`return`",
        Stmt::Break { .. } => "`break`",
        Stmt::Continue { .. } => "`continue`",
        Stmt::If { .. } => "`if`",
        Stmt::IfLet { .. } => "`if let`",
        Stmt::While { .. } => "`while`",
        Stmt::ForIn { .. } => "`for`",
        Stmt::Drop { .. } => "`drop`",
        Stmt::Expr(_) => "an expression statement",
        Stmt::Region { .. } => "`region`",
    }
    .to_string()
}

fn stmt_line(s: &Stmt) -> usize {
    match s {
        Stmt::Let { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::SetField { line, .. }
        | Stmt::IndexSet { line, .. }
        | Stmt::Return { line, .. }
        | Stmt::Break { line }
        | Stmt::Continue { line }
        | Stmt::If { line, .. }
        | Stmt::IfLet { line, .. }
        | Stmt::While { line, .. }
        | Stmt::ForIn { line, .. }
        | Stmt::Drop { line, .. }
        | Stmt::Region { line, .. } => *line,
        Stmt::Expr(e) => Expr::line(e),
    }
}

fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Float(_) => "a float literal",
        Expr::Byte(_) => "a byte literal",
        Expr::Str(_) => "a string literal",
        Expr::Unary { .. } => "a unary operator",
        Expr::Match { .. } => "`match`",
        Expr::IfExpr { .. } => "`if` as an expression",
        Expr::Try { .. } => "`?`",
        Expr::StructLit { .. } => "a record literal",
        Expr::Field { .. } => "a field access",
        _ => "this expression",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gap message is the ladder's grouping key, so its shape is pinned:
    /// one construct, one line, no site-specific text in between.
    #[test]
    fn a_gap_names_the_construct_and_the_line() {
        let e: Result<(), String> = unsupported("`while`", 12);
        assert_eq!(e.unwrap_err(), "direct backend: no lowering for `while` at line 12");
    }

    /// Widening at the boundary is M0/M1's rule; this is the type side of it.
    #[test]
    fn only_the_scalars_m2a_covers_have_a_wasm_type() {
        assert_eq!(wt(&Type::Int), Some(Some(ValType::I64)));
        assert_eq!(wt(&Type::Bool), Some(Some(ValType::I32)));
        assert_eq!(wt(&Type::IntN { bits: 8, signed: false }), Some(Some(ValType::I32)));
        assert_eq!(wt(&Type::Unit), Some(None));
        // An aggregate is an i32 address under the M0 convention, but nothing
        // here can build one yet, so it reports rather than pretends.
        assert_eq!(wt(&Type::Str), None);
        assert_eq!(wt(&Type::Option(Box::new(Type::Int))), None);
    }
}
