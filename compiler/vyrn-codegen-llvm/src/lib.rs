//! Inkwell (in-memory LLVM) backend for the Vyrn v0 subset.
//!
//! This is the "proper" backend the design chose (Rust + Inkwell). It builds an
//! LLVM module in memory and can emit an object file directly, rather than going
//! through textual IR like [`vyrn_codegen`].
//!
//! **Build requirement:** a matching LLVM toolchain and the right `inkwell`
//! feature (see `Cargo.toml`). This crate is *excluded* from the default
//! workspace so `cargo build`/`cargo test` at the repo root never needs LLVM.
//!
//! **Status:** written to mirror the interpreter's semantics
//! ([`vyrn_frontend::interp`]) and the text emitter, but not yet compiled in the
//! authoring environment (no LLVM was present). Expect to make small
//! version-specific adjustments to the `inkwell` API when you first build it.
//!
//! Semantics implemented: i64/i1/void, `let`/`mut` via alloca, arithmetic and
//! comparisons, `if`/`while`, `return`, calls, and `print` via `printf`. Locals
//! use alloca/load/store so LLVM's mem2reg handles SSA; `&&`/`||` short-circuit.
//! Validated types (RFC-0003) lower to their base type; a construction from a
//! compile-time constant erases to the value, while a non-constant construction
//! emits a runtime predicate check that calls `exit(1)` on failure.

use std::collections::HashMap;
use std::path::Path;

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate, OptimizationLevel};

use vyrn_frontend::ast::*;

/// Compile `program` to a native object file at `out_path`.
pub fn compile_to_object(program: &Program, out_path: &Path) -> Result<(), String> {
    let context = Context::create();
    let module = context.create_module("vyrn");
    let builder = context.create_builder();

    let mut cg = Codegen {
        ctx: &context,
        module: &module,
        builder: &builder,
        functions: HashMap::new(),
        ret_types: HashMap::new(),
        types: HashMap::new(),
        printf: None,
        exit: None,
    };
    cg.declare_runtime();
    cg.declare_all(program);
    for f in &program.functions {
        cg.function(f)?;
    }
    cg.emit_c_main();

    module
        .verify()
        .map_err(|e| format!("LLVM module verification failed: {}", e.to_string()))?;

    // Emit an object file for the host target.
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("target init failed: {e}"))?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let machine = target
        .create_target_machine(
            &triple,
            &TargetMachine::get_host_cpu_name().to_string(),
            &TargetMachine::get_host_cpu_features().to_string(),
            OptimizationLevel::Default,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or("could not create target machine")?;

    machine
        .write_to_file(&module, FileType::Object, out_path)
        .map_err(|e| e.to_string())
}

/// Emit the textual LLVM IR (handy for debugging / comparing with the text backend).
pub fn emit_ir(program: &Program) -> Result<String, String> {
    let context = Context::create();
    let module = context.create_module("vyrn");
    let builder = context.create_builder();
    let mut cg = Codegen {
        ctx: &context,
        module: &module,
        builder: &builder,
        functions: HashMap::new(),
        ret_types: HashMap::new(),
        types: HashMap::new(),
        printf: None,
        exit: None,
    };
    cg.declare_runtime();
    cg.declare_all(program);
    for f in &program.functions {
        cg.function(f)?;
    }
    cg.emit_c_main();
    module.verify().map_err(|e| e.to_string())?;
    Ok(module.print_to_string().to_string())
}

struct Codegen<'ctx, 'a> {
    ctx: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    ret_types: HashMap<String, Type>,
    /// Validated-type declarations, for construction and Named→base resolution.
    types: HashMap<String, TypeDecl>,
    printf: Option<FunctionValue<'ctx>>,
    exit: Option<FunctionValue<'ctx>>,
}

impl<'ctx, 'a> Codegen<'ctx, 'a> {
    /// A validated `Named` type resolves to its base representation.
    fn resolve(&self, t: &Type) -> Type {
        match t {
            Type::Named(n) => self.types.get(n).map(|d| d.base.clone()).unwrap_or(Type::Unit),
            other => other.clone(),
        }
    }

    fn llty(&self, t: &Type) -> Option<inkwell::types::BasicTypeEnum<'ctx>> {
        match self.resolve(t) {
            Type::Int => Some(self.ctx.i64_type().into()),
            Type::Bool => Some(self.ctx.bool_type().into()),
            Type::Str => Some(self.ctx.ptr_type(AddressSpace::default()).into()),
            Type::Unit => None, // void
            Type::Named(_) => None, // unreachable after resolve
            // Option/Result lower to { i1 tag, i64 payload } (payload i64 in native).
            Type::Option(_) | Type::Result(..) => Some(self.option_ty().into()),
            // Records, enums, generics, transformers, references, arrays, and
            // tasks are not supported by this backend (the text-IR backend in
            // `vyrn-codegen` is the feature-complete native path).
            _ => None,
        }
    }

    /// The `{ i1, i64 }` struct type that all Options lower to.
    fn option_ty(&self) -> inkwell::types::StructType<'ctx> {
        self.ctx
            .struct_type(&[self.ctx.bool_type().into(), self.ctx.i64_type().into()], false)
    }

    fn declare_runtime(&mut self) {
        let ptr = self.ctx.ptr_type(AddressSpace::default());
        // i32 @printf(ptr, ...)
        let printf_ty = self.ctx.i32_type().fn_type(&[ptr.into()], true);
        self.printf = Some(self.module.add_function("printf", printf_ty, None));
        // void @exit(i32) — used for validated-type runtime failures
        let exit_ty = self.ctx.void_type().fn_type(&[self.ctx.i32_type().into()], false);
        self.exit = Some(self.module.add_function("exit", exit_ty, None));
    }

    fn sym(name: &str) -> String {
        if name == "main" {
            "vyrn_main".to_string()
        } else {
            format!("vyrn_{name}")
        }
    }

    fn declare_all(&mut self, program: &Program) {
        for t in &program.type_decls {
            self.types.insert(t.name.clone(), t.clone());
        }
        for f in &program.functions {
            let param_types: Vec<BasicMetadataTypeEnum> = f
                .params
                .iter()
                .map(|p| self.llty(&p.ty).unwrap().into())
                .collect();
            let fn_type = match self.resolve(&f.ret) {
                Type::Int => self.ctx.i64_type().fn_type(&param_types, false),
                Type::Bool => self.ctx.bool_type().fn_type(&param_types, false),
                _ => self.ctx.void_type().fn_type(&param_types, false),
            };
            let fv = self.module.add_function(&Self::sym(&f.name), fn_type, None);
            self.functions.insert(f.name.clone(), fv);
            self.ret_types.insert(f.name.clone(), f.ret.clone());
        }
    }

    fn function(&mut self, f: &Function) -> Result<(), String> {
        let fv = self.functions[&f.name];
        let entry = self.ctx.append_basic_block(fv, "entry");
        self.builder.position_at_end(entry);

        // alloca each param and store the incoming argument
        let mut scope: Vec<HashMap<String, (PointerValue, Type)>> = vec![HashMap::new()];
        for (i, p) in f.params.iter().enumerate() {
            let ty = self.llty(&p.ty).unwrap();
            let slot = self.builder.build_alloca(ty, &p.name).map_err(err)?;
            let arg = fv.get_nth_param(i as u32).unwrap();
            self.builder.build_store(slot, arg).map_err(err)?;
            scope[0].insert(p.name.clone(), (slot, p.ty.clone()));
        }

        let terminated = self.gen_block(fv, &f.body, &mut scope)?;
        if !terminated {
            match self.resolve(&f.ret) {
                Type::Int => {
                    let z = self.ctx.i64_type().const_zero();
                    self.builder.build_return(Some(&z)).map_err(err)?;
                }
                Type::Bool => {
                    let z = self.ctx.bool_type().const_zero();
                    self.builder.build_return(Some(&z)).map_err(err)?;
                }
                _ => {
                    self.builder.build_return(None).map_err(err)?;
                }
            }
        }
        Ok(())
    }

    /// Returns true if the block's flow is terminated (returned) on all paths.
    fn gen_block(
        &mut self,
        fv: FunctionValue<'ctx>,
        block: &Block,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<bool, String> {
        scope.push(HashMap::new());
        let mut terminated = false;
        for stmt in &block.stmts {
            if terminated {
                break;
            }
            terminated = self.gen_stmt(fv, stmt, scope)?;
        }
        scope.pop();
        Ok(terminated)
    }

    fn gen_stmt(
        &mut self,
        fv: FunctionValue<'ctx>,
        stmt: &Stmt,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<bool, String> {
        match stmt {
            Stmt::Let { name, ty: _, value, .. } => {
                let (v, ty) = self.gen_expr(fv, value, scope)?;
                let slot = self.builder.build_alloca(v.get_type(), name).map_err(err)?;
                self.builder.build_store(slot, v).map_err(err)?;
                scope.last_mut().unwrap().insert(name.clone(), (slot, ty));
                Ok(false)
            }
            Stmt::Assign { name, value, .. } => {
                let (v, _) = self.gen_expr(fv, value, scope)?;
                let (slot, _) = self.lookup(scope, name).ok_or_else(|| format!("unbound `{name}`"))?;
                self.builder.build_store(slot, v).map_err(err)?;
                Ok(false)
            }
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => {
                        let (v, _) = self.gen_expr(fv, e, scope)?;
                        self.builder.build_return(Some(&v)).map_err(err)?;
                    }
                    None => {
                        self.builder.build_return(None).map_err(err)?;
                    }
                }
                Ok(true)
            }
            Stmt::If { cond, then_block, else_block, .. } => {
                let (c, _) = self.gen_expr(fv, cond, scope)?;
                let cbool = c.into_int_value();
                let then_bb = self.ctx.append_basic_block(fv, "then");
                let end_bb = self.ctx.append_basic_block(fv, "endif");
                let else_bb = if else_block.is_some() {
                    self.ctx.append_basic_block(fv, "else")
                } else {
                    end_bb
                };
                self.builder
                    .build_conditional_branch(cbool, then_bb, else_bb)
                    .map_err(err)?;

                self.builder.position_at_end(then_bb);
                let then_term = self.gen_block(fv, then_block, scope)?;
                if !then_term {
                    self.builder.build_unconditional_branch(end_bb).map_err(err)?;
                }

                let mut else_term = false;
                if let Some(eb) = else_block {
                    self.builder.position_at_end(else_bb);
                    else_term = self.gen_block(fv, eb, scope)?;
                    if !else_term {
                        self.builder.build_unconditional_branch(end_bb).map_err(err)?;
                    }
                }

                self.builder.position_at_end(end_bb);
                // If both branches returned, the end block is unreachable; the
                // caller / function epilogue will terminate it.
                Ok(else_block.is_some() && then_term && else_term)
            }
            Stmt::While { cond, body, .. } => {
                let cond_bb = self.ctx.append_basic_block(fv, "wcond");
                let body_bb = self.ctx.append_basic_block(fv, "wbody");
                let end_bb = self.ctx.append_basic_block(fv, "wend");
                self.builder.build_unconditional_branch(cond_bb).map_err(err)?;

                self.builder.position_at_end(cond_bb);
                let (c, _) = self.gen_expr(fv, cond, scope)?;
                self.builder
                    .build_conditional_branch(c.into_int_value(), body_bb, end_bb)
                    .map_err(err)?;

                self.builder.position_at_end(body_bb);
                let body_term = self.gen_block(fv, body, scope)?;
                if !body_term {
                    self.builder.build_unconditional_branch(cond_bb).map_err(err)?;
                }

                self.builder.position_at_end(end_bb);
                Ok(false)
            }
            Stmt::Expr(e) => {
                self.gen_expr(fv, e, scope)?;
                Ok(false)
            }
            // Region arenas and field mutation are not modelled in this
            // subset backend; the text-IR backend is the feature-complete path.
            Stmt::Region { .. } => {
                Err("Inkwell backend does not support `region`; use the text-IR backend".into())
            }
            Stmt::SetField { .. } => {
                Err("Inkwell backend does not support field mutation; use the text-IR backend".into())
            }
            Stmt::ForIn { .. } => {
                Err("Inkwell backend does not support `for` loops (no arrays); use the text-IR backend".into())
            }
            Stmt::Drop { .. } => {
                Err("Inkwell backend does not support `drop` (no heap types); use the text-IR backend".into())
            }
        }
    }

    fn gen_expr(
        &mut self,
        fv: FunctionValue<'ctx>,
        expr: &Expr,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        match expr {
            Expr::Int(n) => Ok((self.ctx.i64_type().const_int(*n as u64, true).into(), Type::Int)),
            Expr::Bool(b) => Ok((self.ctx.bool_type().const_int(*b as u64, false).into(), Type::Bool)),
            // Strings are not lowered by this backend yet (use the text-IR backend).
            Expr::Str(_) => Err("string literals are not supported by the Inkwell backend".into()),
            Expr::Var { name, .. } => {
                // `None` is a constant Option aggregate, not a variable.
                if name == "None" {
                    let none = self.option_ty().const_named_struct(&[
                        self.ctx.bool_type().const_zero().into(),
                        self.ctx.i64_type().const_zero().into(),
                    ]);
                    // payload type unknown from `None` alone; Int is the native default
                    return Ok((none.into(), Type::Option(Box::new(Type::Int))));
                }
                let (slot, ty) = self.lookup(scope, name).ok_or_else(|| format!("unbound `{name}`"))?;
                let llty = self.llty(&ty).unwrap();
                let v = self.builder.build_load(llty, slot, name).map_err(err)?;
                Ok((v, ty))
            }
            Expr::Unary { op, expr, .. } => {
                let (v, ty) = self.gen_expr(fv, expr, scope)?;
                let iv = v.into_int_value();
                let out = match op {
                    UnOp::Neg => self
                        .builder
                        .build_int_neg(iv, "neg")
                        .map_err(err)?,
                    UnOp::Not => self
                        .builder
                        .build_not(iv, "not")
                        .map_err(err)?,
                };
                Ok((out.into(), ty))
            }
            Expr::Binary { op, lhs, rhs, .. } => self.gen_binary(fv, *op, lhs, rhs, scope),
            Expr::Call { name, args, .. } => self.gen_call(fv, name, args, scope),
            Expr::Match { scrutinee, arms, .. } => self.gen_match(fv, scrutinee, arms, scope),
            Expr::Try { expr, .. } => self.gen_try(fv, expr, scope),
            // Records are not supported by the Inkwell backend yet; use `vyrn run`.
            Expr::StructLit { name, .. } => {
                Err(format!("record literal `{name} {{ .. }}` is not supported by the native backend"))
            }
            Expr::Field { field, .. } => {
                Err(format!("field access `.{field}` is not supported by the native backend"))
            }
            Expr::TryConstruct { name, .. } => {
                Err(format!("`{name}?(..)` is not supported by the Inkwell backend"))
            }
            Expr::ArrayLit { .. } => {
                Err("array literals are not supported by the Inkwell backend".into())
            }
            Expr::Spawn { name, .. } => {
                Err(format!("`spawn {name}(..)` is not supported by the Inkwell backend"))
            }
            // `if` as an expression (RFC-0030) is not lowered by the subset
            // Inkwell backend; use `vyrn run` / the textual-IR backend.
            Expr::IfExpr { .. } => {
                Err("`if` used as an expression is not supported by the Inkwell backend".into())
            }
        }
    }

    /// Lower a `match` over an Option/Result to a tag test + `phi`. The tag-1 arm
    /// is `Some`/`Ok`; the tag-0 arm is `None`/`Err`. Either may bind an i64
    /// payload.
    fn gen_match(
        &mut self,
        fv: FunctionValue<'ctx>,
        scrutinee: &Expr,
        arms: &[MatchArm],
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        let (sv, _) = self.gen_expr(fv, scrutinee, scope)?;
        let agg = sv.into_struct_value();
        let tag = self.builder.build_extract_value(agg, 0, "tag").map_err(err)?.into_int_value();

        let one_bb = self.ctx.append_basic_block(fv, "m.one");
        let zero_bb = self.ctx.append_basic_block(fv, "m.zero");
        let end_bb = self.ctx.append_basic_block(fv, "m.end");
        self.builder.build_conditional_branch(tag, one_bb, zero_bb).map_err(err)?;

        let is_one = |p: &Pattern| matches!(p, Pattern::Some(_) | Pattern::Ok(_));
        let binding = |p: &Pattern| match p {
            Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => Some(b.clone()),
            // User-enum patterns are not lowered by this backend (see llty).
            Pattern::Variant(_, b) => b.first().cloned(),
            Pattern::None => None,
        };

        // tag == 1 arm (Some / Ok)
        self.builder.position_at_end(one_bb);
        let one_arm = arms.iter().find(|a| is_one(&a.pattern)).unwrap();
        let (one_val, ty) = self.gen_match_arm(fv, agg, binding(&one_arm.pattern), &one_arm.body, scope)?;
        let one_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(end_bb).map_err(err)?;

        // tag == 0 arm (None / Err)
        self.builder.position_at_end(zero_bb);
        let zero_arm = arms.iter().find(|a| !is_one(&a.pattern)).unwrap();
        let (zero_val, _) = self.gen_match_arm(fv, agg, binding(&zero_arm.pattern), &zero_arm.body, scope)?;
        let zero_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(end_bb).map_err(err)?;

        // merge
        self.builder.position_at_end(end_bb);
        let phi = self.builder.build_phi(one_val.get_type(), "m").map_err(err)?;
        phi.add_incoming(&[(&one_val, one_end), (&zero_val, zero_end)]);
        Ok((phi.as_basic_value(), ty))
    }

    /// Emit a match arm body, binding the i64 payload if the pattern binds.
    fn gen_match_arm(
        &mut self,
        fv: FunctionValue<'ctx>,
        agg: inkwell::values::StructValue<'ctx>,
        bind: Option<String>,
        body: &Expr,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        scope.push(HashMap::new());
        if let Some(name) = bind {
            let payload = self.builder.build_extract_value(agg, 1, "payload").map_err(err)?;
            let slot = self.builder.build_alloca(self.ctx.i64_type(), &name).map_err(err)?;
            self.builder.build_store(slot, payload).map_err(err)?;
            scope.last_mut().unwrap().insert(name, (slot, Type::Int));
        }
        let out = self.gen_expr(fv, body, scope)?;
        scope.pop();
        Ok(out)
    }

    /// Lower `expr?`: on `None`/`Err` (tag 0) return the aggregate; otherwise
    /// continue with the unwrapped i64 payload.
    fn gen_try(
        &mut self,
        fv: FunctionValue<'ctx>,
        expr: &Expr,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        let (sv, _) = self.gen_expr(fv, expr, scope)?;
        let agg = sv.into_struct_value();
        let tag = self.builder.build_extract_value(agg, 0, "tag").map_err(err)?.into_int_value();
        let ok_bb = self.ctx.append_basic_block(fv, "try.ok");
        let prop_bb = self.ctx.append_basic_block(fv, "try.prop");
        self.builder.build_conditional_branch(tag, ok_bb, prop_bb).map_err(err)?;

        // propagate: the function returns Option/Result ({ i1, i64 }).
        self.builder.position_at_end(prop_bb);
        self.builder.build_return(Some(&agg)).map_err(err)?;

        self.builder.position_at_end(ok_bb);
        let payload = self.builder.build_extract_value(agg, 1, "payload").map_err(err)?;
        Ok((payload, Type::Int))
    }

    fn gen_binary(
        &mut self,
        fv: FunctionValue<'ctx>,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        // Short-circuit && / || via branches + phi.
        if matches!(op, BinOp::And | BinOp::Or) {
            let (l, _) = self.gen_expr(fv, lhs, scope)?;
            let lbool = l.into_int_value();
            let pre_bb = self.builder.get_insert_block().unwrap();
            let rhs_bb = self.ctx.append_basic_block(fv, "sc.rhs");
            let end_bb = self.ctx.append_basic_block(fv, "sc.end");
            match op {
                BinOp::And => self
                    .builder
                    .build_conditional_branch(lbool, rhs_bb, end_bb)
                    .map_err(err)?,
                BinOp::Or => self
                    .builder
                    .build_conditional_branch(lbool, end_bb, rhs_bb)
                    .map_err(err)?,
                _ => unreachable!(),
            };
            self.builder.position_at_end(rhs_bb);
            let (r, _) = self.gen_expr(fv, rhs, scope)?;
            let rbool = r.into_int_value();
            let rhs_end_bb = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(end_bb).map_err(err)?;

            self.builder.position_at_end(end_bb);
            let phi = self.builder.build_phi(self.ctx.bool_type(), "sc").map_err(err)?;
            let short = self
                .ctx
                .bool_type()
                .const_int(if op == BinOp::And { 0 } else { 1 }, false);
            phi.add_incoming(&[(&short, pre_bb), (&rbool, rhs_end_bb)]);
            return Ok((phi.as_basic_value(), Type::Bool));
        }

        let (l, _) = self.gen_expr(fv, lhs, scope)?;
        let (r, _) = self.gen_expr(fv, rhs, scope)?;
        let (li, ri) = (l.into_int_value(), r.into_int_value());

        let arith = |b: &Builder<'ctx>| -> Result<IntValue<'ctx>, String> {
            Ok(match op {
                BinOp::Add => b.build_int_add(li, ri, "add").map_err(err)?,
                BinOp::Sub => b.build_int_sub(li, ri, "sub").map_err(err)?,
                BinOp::Mul => b.build_int_mul(li, ri, "mul").map_err(err)?,
                BinOp::Div => b.build_int_signed_div(li, ri, "div").map_err(err)?,
                BinOp::Rem => b.build_int_signed_rem(li, ri, "rem").map_err(err)?,
                _ => unreachable!(),
            })
        };
        let cmp = |b: &Builder<'ctx>, pred: IntPredicate| -> Result<IntValue<'ctx>, String> {
            b.build_int_compare(pred, li, ri, "cmp").map_err(err)
        };

        let (val, ty) = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                (arith(self.builder)?, Type::Int)
            }
            BinOp::Lt => (cmp(self.builder, IntPredicate::SLT)?, Type::Bool),
            BinOp::LtEq => (cmp(self.builder, IntPredicate::SLE)?, Type::Bool),
            BinOp::Gt => (cmp(self.builder, IntPredicate::SGT)?, Type::Bool),
            BinOp::GtEq => (cmp(self.builder, IntPredicate::SGE)?, Type::Bool),
            BinOp::Eq => (cmp(self.builder, IntPredicate::EQ)?, Type::Bool),
            BinOp::NotEq => (cmp(self.builder, IntPredicate::NE)?, Type::Bool),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        };
        Ok((val.into(), ty))
    }

    fn gen_call(
        &mut self,
        fv: FunctionValue<'ctx>,
        name: &str,
        args: &[Expr],
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        if name == "print" {
            let (v, ty) = self.gen_expr(fv, &args[0], scope)?;
            let printf = self.printf.unwrap();
            match self.resolve(&ty) {
                Type::Int => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%lld\n", "fmt.d")
                        .map_err(err)?
                        .as_pointer_value();
                    self.builder
                        .build_call(printf, &[fmt.into(), v.into()], "printf")
                        .map_err(err)?;
                }
                Type::Bool => {
                    let t = self
                        .builder
                        .build_global_string_ptr("true\n", "fmt.t")
                        .map_err(err)?
                        .as_pointer_value();
                    let f = self
                        .builder
                        .build_global_string_ptr("false\n", "fmt.f")
                        .map_err(err)?
                        .as_pointer_value();
                    let sel = self
                        .builder
                        .build_select(v.into_int_value(), t, f, "fmt.sel")
                        .map_err(err)?;
                    self.builder
                        .build_call(printf, &[sel.into()], "printf")
                        .map_err(err)?;
                }
                _ => return Err("print of a non-scalar".into()),
            }
            // print yields Unit; the checker forbids using it as a value.
            return Ok((self.ctx.i64_type().const_zero().into(), Type::Unit));
        }

        // `Some(x)` / `Ok(x)` / `Err(e)` — build a { i1 tag, i64 payload } value.
        if let Some(tag) = match name {
            "Some" | "Ok" => Some(1u64),
            "Err" => Some(0u64),
            _ => None,
        } {
            let (v, ty) = self.gen_expr(fv, &args[0], scope)?;
            if self.resolve(&ty) != Type::Int {
                return Err(format!(
                    "native backend supports Int payloads only (`{name}` payload is {ty:?}); use `vyrn run`"
                ));
            }
            let undef = self.option_ty().get_undef();
            let a = self
                .builder
                .build_insert_value(undef, self.ctx.bool_type().const_int(tag, false), 0, "tag")
                .map_err(err)?;
            let b = self
                .builder
                .build_insert_value(a, v.into_int_value(), 1, "val")
                .map_err(err)?;
            // The exact Option/Result type doesn't affect the { i1, i64 } repr.
            let out_ty = if name == "Some" {
                Type::Option(Box::new(Type::Int))
            } else {
                Type::Result(Box::new(Type::Int), Box::new(Type::Int))
            };
            return Ok((b.into_struct_value().into(), out_ty));
        }

        // construction of a validated type: `Age(expr)`
        if let Some(decl) = self.types.get(name).cloned() {
            return self.gen_construction(fv, &decl, &args[0], scope);
        }

        let callee = *self
            .functions
            .get(name)
            .ok_or_else(|| format!("call to unknown function `{name}`"))?;
        let mut arg_vals = Vec::with_capacity(args.len());
        for a in args {
            let (v, _) = self.gen_expr(fv, a, scope)?;
            arg_vals.push(v.into());
        }
        let ret = self.ret_types.get(name).cloned().unwrap_or(Type::Int);
        let call = self.builder.build_call(callee, &arg_vals, "call").map_err(err)?;
        match call.try_as_basic_value().basic() {
            Some(v) => Ok((v, ret)),
            None => Ok((self.ctx.i64_type().const_zero().into(), Type::Unit)),
        }
    }

    /// Construct a validated-type value. A compile-time-constant argument (the
    /// checker already proved it valid) erases to the value; otherwise emit a
    /// runtime predicate check that prints and `exit(1)`s on failure.
    fn gen_construction(
        &mut self,
        fv: FunctionValue<'ctx>,
        decl: &TypeDecl,
        arg: &Expr,
        scope: &mut Vec<HashMap<String, (PointerValue<'ctx>, Type)>>,
    ) -> Result<(BasicValueEnum<'ctx>, Type), String> {
        let (v, _) = self.gen_expr(fv, arg, scope)?;
        let named = Type::Named(decl.name.clone());

        let is_const = vyrn_frontend::consteval::eval(arg, &HashMap::new()).is_some();
        let pred = match &decl.predicate {
            Some(p) if !is_const => p,
            _ => return Ok((v, named)), // proven, or no predicate: no runtime check
        };

        // Bind `value` to the argument, evaluate the predicate, branch on it.
        let base_ll = self.llty(&decl.base).unwrap();
        let slot = self.builder.build_alloca(base_ll, "value").map_err(err)?;
        self.builder.build_store(slot, v).map_err(err)?;
        scope.push(HashMap::new());
        scope.last_mut().unwrap().insert("value".into(), (slot, decl.base.clone()));
        let (cond, _) = self.gen_expr(fv, pred, scope)?;
        scope.pop();

        let ok_bb = self.ctx.append_basic_block(fv, "vok");
        let fail_bb = self.ctx.append_basic_block(fv, "vfail");
        self.builder
            .build_conditional_branch(cond.into_int_value(), ok_bb, fail_bb)
            .map_err(err)?;

        self.builder.position_at_end(fail_bb);
        let msg = self
            .builder
            .build_global_string_ptr("Vyrn: validation failed\n", "verr")
            .map_err(err)?
            .as_pointer_value();
        self.builder
            .build_call(self.printf.unwrap(), &[msg.into()], "printf")
            .map_err(err)?;
        let one = self.ctx.i32_type().const_int(1, false);
        self.builder
            .build_call(self.exit.unwrap(), &[one.into()], "")
            .map_err(err)?;
        self.builder.build_unreachable().map_err(err)?;

        self.builder.position_at_end(ok_bb);
        let result = self.builder.build_load(base_ll, slot, "checked").map_err(err)?;
        Ok((result, named))
    }

    fn emit_c_main(&mut self) {
        // define i32 @main() { %r = call i64 @vyrn_main(); ret i32 trunc(%r) }
        let i32t = self.ctx.i32_type();
        let main_ty = i32t.fn_type(&[], false);
        let main_fn = self.module.add_function("main", main_ty, None);
        let bb = self.ctx.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(bb);
        let vyrn_main = self.functions["main"];
        let r = self
            .builder
            .build_call(vyrn_main, &[], "r")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        // Mask to the low 8 bits (POSIX 0–255), matching the interpreter and the
        // text-IR backend, so a return value > 255 doesn't diverge on Windows.
        let mask = self.ctx.i64_type().const_int(255, false);
        let m = self.builder.build_and(r, mask, "m").unwrap();
        let c = self.builder.build_int_truncate(m, i32t, "c").unwrap();
        self.builder.build_return(Some(&c)).unwrap();
    }

    fn lookup(
        &self,
        scope: &[HashMap<String, (PointerValue<'ctx>, Type)>],
        name: &str,
    ) -> Option<(PointerValue<'ctx>, Type)> {
        for frame in scope.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v.clone());
            }
        }
        None
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::check;

    #[test]
    fn minimal_llvm_context() {
        // Smallest possible LLVM-API exercise: create a context + empty module.
        let ctx = Context::create();
        let m = ctx.create_module("t");
        let ir = m.print_to_string().to_string();
        assert!(ir.contains("ModuleID"));
    }

    #[test]
    fn emits_verified_ir_for_fib() {
        let src = "
            fn fib(n: Int) -> Int {
                if n < 2 { return n; }
                return fib(n - 1) + fib(n - 2);
            }
            fn main() -> Int { return fib(10); }
        ";
        let program = check(src).unwrap();
        // This actually drives the LLVM C API via inkwell and verifies the module.
        let ir = emit_ir(&program).unwrap();
        assert!(ir.contains("define i64 @vyrn_main("), "{ir}");
        assert!(ir.contains("define i64 @vyrn_fib("), "{ir}");
        assert!(ir.contains("define i32 @main()"), "{ir}");
    }

    #[test]
    fn emits_object_file() {
        let src = "fn main() -> Int { let mut i = 0; let mut s = 0; \
                   while i < 10 { s = s + i; i = i + 1; } return s; }";
        let program = check(src).unwrap();
        let out = std::env::temp_dir().join("vyrn_inkwell_test.o");
        compile_to_object(&program, &out).unwrap();
        let meta = std::fs::metadata(&out).unwrap();
        assert!(meta.len() > 0, "object file should be non-empty");
    }

    #[test]
    fn object_links_and_runs_matching_interpreter() {
        // Full end-to-end: Inkwell → object → clang link → run → exit code.
        let src = "
            fn fib(n: Int) -> Int {
                if n < 2 { return n; }
                return fib(n - 1) + fib(n - 2);
            }
            fn main() -> Int { return fib(10); }
        ";
        let program = check(src).unwrap();
        let dir = std::env::temp_dir();
        let obj = dir.join("vyrn_ink_e2e.o");
        let exe = dir.join("vyrn_ink_e2e.exe");
        compile_to_object(&program, &obj).unwrap();

        let clang = r"C:\Program Files\LLVM\bin\clang.exe";
        let link = std::process::Command::new(clang)
            .arg(&obj)
            .arg("-o")
            .arg(&exe)
            .status()
            .expect("run clang");
        assert!(link.success(), "clang link failed");

        let run = std::process::Command::new(&exe).status().expect("run exe");
        // fib(10) = 55 — must match the interpreter's exit code.
        assert_eq!(run.code(), Some(55), "native exit code should be fib(10)=55");
    }
}
