// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::Expr;
use crate::codegen::riscv64::codegen::{trunc_i32, Riscv64Codegen};

impl Riscv64Codegen {
    pub(crate) fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => self.li_a0(trunc_i32(*v) as i32),
            Expr::Float { v, .. } => self.load_float_fa0(*v as f32),
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.load_a0(off);
                    // 整数提升: char/short → int
                    let ty = expr.ty();
                    if ty.is_int() {
                        self.gen_int_promote(&ty);
                    }
                } else {
                    self.li_a0(0);
                }
            },
            Expr::Binop { op, l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.mv_a1_a0();
                self.gen_expr_from_ast(r);
                self.binop_a0_a1(*op, Self::is_signed_ast_type(&l.ty()));
            },
            Expr::Call { callee, args, .. } => {
                // GCC 兼容内建函数: AST 路径编译期折叠
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    match name.name.as_str() {
                        "__builtin_expect" | "__builtin_expect_with_probability" => {
                            if let Some(arg0) = args.first() {
                                self.gen_expr_from_ast(arg0);
                            } else {
                                self.li_a0(0);
                            }
                            return;
                        },
                        "__builtin_nonzero_p" => {
                            let v = args
                                .first()
                                .and_then(|a| crate::const_eval::eval(a).ok())
                                .map_or(0, |v| if v != 0 { 1i128 } else { 0i128 });
                            self.li_a0(trunc_i32(v) as i32);
                            return;
                        },
                        "__builtin_constant_p" => {
                            let v = args
                                .first()
                                .map_or(0, |a| crate::const_eval::is_constant(a) as i32);
                            self.li_a0(v);
                            return;
                        },
                        "__builtin_object_size" | "__builtin_dynamic_object_size" => {
                            self.li_a0(-1);
                            return;
                        },
                        "__builtin_mul_loop" => {
                            self.li_a0(0);
                            return;
                        },
                        _ => {},
                    }
                }
                for (i, arg) in args.iter().enumerate().take(8) {
                    self.gen_expr_from_ast(arg);
                    self.mv_a_reg(i as u8);
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_jal_to(&name.name);
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
                    self.li_a0(trunc_i32(v) as i32);
                } else {
                    self.li_a0(0);
                }
            },
        }
    }
}
