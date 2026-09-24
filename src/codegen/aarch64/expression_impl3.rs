// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{Expr, UnOp};
use crate::codegen::aarch64::codegen::{trunc_i32, Aarch64Codegen};

impl Aarch64Codegen {
    pub(crate) fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.mov_w0_imm(trunc_i32(*v)),
            Expr::Char { v, .. } => self.mov_w0_imm(trunc_i32(*v)),
            Expr::Float { v, .. } => self.load_float_s0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_w0(off);
                    // 整数提升: char/short → int
                    let ty = expr.ty();
                    if ty.is_int() {
                        self.gen_int_promote(&ty);
                    }
                } else if self.global_rodata.contains_key(&name.name) {
                    self.load_global_addr_w0(&name.name);
                } else {
                    self.mov_w0_imm(0);
                }
            },
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mov_w14_w0();
                self.gen_expr_from_ast(r);
                self.mov_w1_w0();
                self.mov_w0_w14();
                self.binop_w0_w1(*op, Self::is_signed_ast_type(&l.ty()));
            },
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => self.neg_w0(),
                    UnOp::Plus => {},
                    UnOp::Not => self.cmp_w0_zero(),
                    UnOp::BitNot => self.mvn_w0(),
                }
            },
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                // GCC 兼容内建函数: AST 路径编译期折叠
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    match name.name.as_str() {
                        "__builtin_expect" | "__builtin_expect_with_probability" => {
                            if let Some(arg0) = args.first() {
                                self.gen_expr_from_ast(arg0);
                            } else {
                                self.mov_w0_imm(0);
                            }
                            return;
                        },
                        "__builtin_nonzero_p" => {
                            let v = args
                                .first()
                                .and_then(|a| crate::const_eval::eval(a).ok())
                                .map_or(0, |v| if v != 0 { 1i128 } else { 0i128 });
                            self.mov_w0_imm(trunc_i32(v));
                            return;
                        },
                        "__builtin_constant_p" => {
                            let v = args
                                .first()
                                .map_or(0u32, |a| crate::const_eval::is_constant(a) as u32);
                            self.mov_w0_imm(v);
                            return;
                        },
                        "__builtin_object_size" | "__builtin_dynamic_object_size" => {
                            self.mov_w0_imm(u32::MAX);
                            return;
                        },
                        "__builtin_mul_loop" => {
                            self.mov_w0_imm(0);
                            return;
                        },
                        _ => {},
                    }
                }
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mov_w_reg_w0(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_bl_to(&name.name);
                }
            },
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    &span.file,
                    span.line,
                    *volatile,
                );
            },
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.mov_w0_imm(trunc_i32(v));
                } else {
                    let span = Self::ast_expr_span(expr);
                    let mut diag = crate::diagnostics::Diagnostic::err(
                        "E0313",
                        "unsupported AST expression on aarch64",
                        span,
                    );
                    diag.hint = Some("add handling in gen_expr_from_ast".to_string());
                    self.diagnostics.push(diag);
                    self.mov_w0_imm(0);
                }
            },
        }
    }
}
