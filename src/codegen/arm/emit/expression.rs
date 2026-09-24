// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::{find_field, BinOp, Expr, Type, UnOp};

use crate::codegen::arm::codegen::ArmCodegen;

impl ArmCodegen {
    pub(crate) fn gen_expr_from_ast(&mut self, expr: &Expr) {
        match expr {
            Expr::Int { v, .. } => {
                self.emit_load_imm(0, *v as i32);
            }
            Expr::Char { v, .. } => {
                self.emit_load_imm(0, *v as i32);
            }
            Expr::Float { v, ty, .. } => {
                // 浮点字面量: 加载到 VFP 寄存器 s0/d0
                let is_double = matches!(ty, crate::ast::Type::Double | crate::ast::Type::LongDouble);
                if is_double {
                    self.load_float_d0(*v);
                } else {
                    self.load_float_s0(*v as f32);
                }
            }
            Expr::String { data, .. } => {
                // Emit string to .rodata and load address
                let offset = if let Some(&off) = self.string_pool.get(data) {
                    off
                } else {
                    let off = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(data);
                    // Ensure null terminator
                    if data.last() != Some(&0) {
                        self.rodata.push(0);
                    }
                    self.string_pool.insert(data.clone(), off);
                    off
                };
                // Load string address into r0
                self.emit_load_imm(0, offset as i32);
            }
            Expr::Ident { name, .. } => {
                if let Some(&off) = self.locals.get(&name.name) {
                    self.emit_load_r0(off);
                } else if self.global_rodata.contains_key(&name.name) {
                    // 全局变量地址 (简化)
                    self.emit_load_imm(0, 0);
                } else {
                    self.emit_load_imm(0, 0);
                }
            }
            Expr::Binop { op, l, r, ty, .. } => {
                // 检测浮点运算
                let is_float = matches!(ty, crate::ast::Type::Float | crate::ast::Type::Double | crate::ast::Type::LongDouble);
                let is_float_cmp = is_float && matches!(op, BinOp::EqEq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
                let is_float_arith = is_float && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);

                if is_float_arith || is_float_cmp {
                    // 浮点二元运算: 使用 VFP 指令
                    let is_double = matches!(ty, crate::ast::Type::Double | crate::ast::Type::LongDouble);
                    // 计算左操作数 -> s0/d0
                    self.gen_expr_from_ast(l);
                    // 保存到 s2/d2
                    self.save_s0_to_s2(is_double);
                    // 计算右操作数 -> s0/d0
                    self.gen_expr_from_ast(r);
                    // 执行运算: s0 = s2 op s0
                    if is_float_cmp {
                        self.float_cmp_r0(*op, is_double);
                    } else {
                        self.float_binop_s0(*op, is_double);
                        // 结果在 s0/d0, 需要转回整数寄存器 r0 (如果需要)
                        // 对于纯浮点运算, 结果留在 VFP 寄存器
                    }
                } else {
                    // 整数二元运算
                    self.gen_expr_from_ast(l);
                    self.emit_push(vec![0]); // push r0 (lhs)
                    self.gen_expr_from_ast(r);
                    self.emit_pop(vec![1]); // pop r1 (lhs)
                                            // r0 = rhs, r1 = lhs
                    self.emit_binop(*op);
                }
            }
            Expr::Unary { op, e, .. } => {
                self.gen_expr_from_ast(e);
                match op {
                    UnOp::Neg => self.emit_rsb_imm(0, 0, 0), // rsb r0, r0, #0
                    UnOp::Not => {
                        self.emit_cmp_imm(0, 0);
                        self.emit_mov_imm(0, 0); // mov r0, #0
                        self.emit_moveq(0, 1); // moveq r0, #1
                    }
                    UnOp::BitNot => self.emit_mvn(0, 0), // mvn r0, r0
                    UnOp::Plus => {}
                }
            }
            Expr::Cast { e, .. } => self.gen_expr_from_ast(e),
            Expr::Call { callee, args, .. } => {
                // 参数传递: r0-r3
                for (i, arg) in args.iter().enumerate().take(4) {
                    self.gen_expr_from_ast(arg);
                    if i > 0 {
                        self.emit_push(vec![0]); // push intermediate args
                    }
                }
                // 弹出参数到 r1-r3
                if args.len() > 1 {
                    let mut regs: Vec<u8> = (1..args.len().min(4) as u8).collect();
                    regs.reverse();
                    // Pop in reverse order to r1, r2, r3
                    for j in (1..args.len().min(4)).rev() {
                        self.emit_pop(vec![j as u8]);
                    }
                }
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    self.emit_bl(&name.name);
                }
            }
            Expr::Assign { l, r, .. } => {
                self.gen_expr_from_ast(r);
                if let Expr::Ident { name, .. } = l.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_store_r0(off);
                    }
                }
            }
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
            }
            // M25: __builtin_va_arg(ap, T) — 从保存区顺序读取变参
            Expr::VaArg { ap, ty, .. } => {
                let is_float = matches!(ty, crate::ast::Type::Float | crate::ast::Type::Double | crate::ast::Type::LongDouble);
                let is_double = matches!(ty, crate::ast::Type::Double | crate::ast::Type::LongDouble);
                if is_float {
                    // 浮点变参: 使用 VFP 加载
                    self.gen_va_arg_float("ap", is_double);
                } else {
                    self.gen_va_arg_int("ap");
                }
                let _ = ap;
            }
            // 指针操作: 取地址
            Expr::AddrOf { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        // 加载变量地址到 r0: add r0, fp, #off
                        self.emit_add_imm(0, 11, off as u32);
                    } else {
                        self.emit_load_imm(0, 0);
                    }
                } else {
                    self.emit_load_imm(0, 0);
                }
            }
            // 指针操作: 解引用
            Expr::Deref { e, .. } => {
                self.gen_expr_from_ast(e);
                // r0 现在是指针地址, 加载 [r0] 到 r0
                self.emit_push(vec![0]); // 保存地址
                self.emit_pop(vec![1]); // 弹出到 r1
                // ldr r0, [r1]
                let inst = 0xE5910000u32;
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
            // 数组索引: arr[idx]
            Expr::Index { arr, idx, .. } => {
                self.gen_expr_from_ast(arr);
                self.emit_push(vec![0]); // 保存基地址
                self.gen_expr_from_ast(idx);
                self.emit_pop(vec![1]); // r1 = 基地址
                // r0 = 索引, r1 = 基地址
                // 简化: 假设元素大小为 4 字节
                self.emit_lsl(0, 0, 0); // 先不处理, 假设索引已缩放
                // add r0, r1, r0 (地址 + 偏移)
                self.emit_add(0, 1, 0);
                // 加载 [r0] 到 r0
                self.emit_push(vec![0]);
                self.emit_pop(vec![1]);
                let inst = 0xE5910000u32;
                self.code.extend_from_slice(&inst.to_le_bytes());
            }
            // 结构体成员访问
            Expr::Member { expr, field, ty: _, is_arrow, .. } => {
                self.gen_expr_from_ast(expr);
                // r0 现在是结构体地址 (is_arrow) 或结构体值 (.)
                // 从 aggregate_fields 查找字段实际偏移
                let struct_name = if *is_arrow {
                    // ptr->member: expr.ty() 是指针或数组, 取元素类型
                    match expr.ty() {
                        Type::Ptr(inner) | Type::Array(inner, _) => match &inner.base {
                            Type::Struct(name, _, _, _) | Type::Union(name, _, _, false) => Some(name.clone()),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    // s.member: expr.ty() 是结构体/联合体本身
                    match expr.ty() {
                        Type::Struct(name, _, _, _) | Type::Union(name, _, _, false) => Some(name.clone()),
                        _ => None,
                    }
                };
                let field_off = struct_name.as_ref().and_then(|n| {
                    self.aggregate_fields.get(n).and_then(|(fields, _)| {
                        find_field(fields, &field.name).map(|(f, extra)| f.offset + extra)
                    })
                }).unwrap_or(0);

                if *is_arrow {
                    // ptr->member: r0 已经是指针地址
                    if field_off != 0 {
                        // r0 += field_off
                        self.emit_push(vec![0]); // 保存指针
                        self.emit_pop(vec![1]);  // r1 = 指针
                        self.emit_load_imm(2, field_off as i32); // r2 = 偏移
                        self.emit_add(0, 1, 2);  // r0 = 指针 + 偏移
                        self.emit_push(vec![0]); // 保存地址
                        self.emit_pop(vec![1]);
                    } else {
                        self.emit_push(vec![0]);
                        self.emit_pop(vec![1]);
                    }
                    // ldr r0, [r1] — 加载成员值
                    let inst = 0xE5910000u32;
                    self.code.extend_from_slice(&inst.to_le_bytes());
                } else {
                    // s.member: r0 是结构体值 (已在寄存器中)
                    // 对于值类型, 需要取地址再加偏移
                    // 简化: 如果结构体在栈上, 从栈加载对应偏移字段
                    if field_off != 0 {
                        // 值类型非零偏移: 通过帧偏移加载
                        // 如果 expr 是 Ident, 直接从帧加载字段偏移
                        if let Expr::Ident { name, .. } = expr.as_ref() {
                            if let Some(&base_off) = self.locals.get(&name.name) {
                                let total_off = base_off + field_off as i32;
                                self.emit_ldr_fp_off(0, total_off);
                            }
                        }
                    }
                    // field_off == 0 时 gen_expr_from_ast 已加载第一个成员
                }
            }
            // 前置自增: ++x
            Expr::PreInc { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_r0(off); // 加载当前值
                        self.emit_add_imm(0, 0, 1); // r0 += 1
                        self.emit_store_r0(off); // 存回
                    }
                }
            }
            // 前置自减: --x
            Expr::PreDec { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_r0(off);
                        self.emit_sub_imm(0, 0, 1); // r0 -= 1
                        self.emit_store_r0(off); // 存回
                    }
                }
            }
            // 后置自增: x++
            Expr::PostInc { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_r0(off); // 加载原值 (返回值)
                        self.emit_push(vec![0]); // 保存原值
                        self.emit_add_imm(0, 0, 1); // r0 += 1
                        self.emit_store_r0(off); // 存回新值
                        self.emit_pop(vec![0]); // 恢复原值作为表达式结果
                    }
                }
            }
            // 后置自减: x--
            Expr::PostDec { e, .. } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if let Some(&off) = self.locals.get(&name.name) {
                        self.emit_load_r0(off);
                        self.emit_push(vec![0]);
                        self.emit_sub_imm(0, 0, 1);
                        self.emit_store_r0(off);
                        self.emit_pop(vec![0]);
                    }
                }
            }
            // 条件表达式: cond ? then : else
            Expr::Cond { cond, then_, else_, .. } => {
                let lbl_else = self.fresh_label("Lcond_else");
                let lbl_end = self.fresh_label("Lcond_end");
                self.gen_expr_from_ast(cond);
                self.emit_cmp_imm(0, 0); // cmp r0, #0
                self.emit_beq(&lbl_else);
                self.gen_expr_from_ast(then_);
                self.emit_b(&lbl_end);
                self.set_label(&lbl_else);
                self.gen_expr_from_ast(else_);
                self.set_label(&lbl_end);
            }
            // sizeof(type)
            Expr::SizeofTy { ty, .. } => {
                let size = ty.size() as i32;
                self.emit_load_imm(0, size);
            }
            // sizeof(expr)
            Expr::SizeofExpr { e, .. } => {
                let size = e.ty().size() as i32;
                self.emit_load_imm(0, size);
            }
            // _Alignof(type)
            Expr::AlignofTy { ty, .. } => {
                let align = ty.align() as i32;
                self.emit_load_imm(0, align);
            }
            // _Alignof(expr)
            Expr::AlignofExpr { e, .. } => {
                let align = e.ty().align() as i32;
                self.emit_load_imm(0, align);
            }
            // 逗号表达式: (lhs, rhs) -> 求值 lhs 丢弃, 返回 rhs
            Expr::Comma { l, r, .. } => {
                self.gen_expr_from_ast(l);
                self.gen_expr_from_ast(r);
            }
            // 复合赋值: l += r, l -= r, etc.
            Expr::Assign { op, l, r, .. } => {
                if *op == BinOp::Add {
                    // l += r
                    if let Expr::Ident { name, .. } = l.as_ref() {
                        if let Some(&off) = self.locals.get(&name.name) {
                            self.gen_expr_from_ast(r);
                            self.emit_push(vec![0]); // 保存 r
                            self.emit_load_r0(off); // 加载 l
                            self.emit_pop(vec![1]); // r1 = r
                            self.emit_add(0, 1, 0); // r0 = l + r
                            self.emit_store_r0(off); // 存回 l
                        }
                    }
                } else {
                    // 其他复合赋值: 简化处理
                    self.gen_expr_from_ast(r);
                    if let Expr::Ident { name, .. } = l.as_ref() {
                        if let Some(&off) = self.locals.get(&name.name) {
                            self.emit_store_r0(off);
                        }
                    }
                }
            }
            // _Complex 字面量
            Expr::ComplexLit { real, imag, ty, .. } => {
                // 简化: 将实部和虚部存入栈, 返回地址
                let is_double = matches!(ty, crate::ast::Type::Complex(ref inner) if matches!(&inner.base, crate::ast::Type::Double | crate::ast::Type::LongDouble));
                let size = if is_double { 8 } else { 4 };
                let real_off = self.alloc_local("<cx_r>", size);
                let imag_off = self.alloc_local("<cx_i>", size);
                // 存储实部
                if is_double {
                    self.load_float_d0(*real);
                    self.emit_vstr_d(0, real_off);
                } else {
                    self.emit_load_imm(0, real.to_bits() as i32);
                    self.emit_store_r0(real_off);
                }
                // 存储虚部
                if is_double {
                    self.load_float_d0(*imag);
                    self.emit_vstr_d(0, imag_off);
                } else {
                    self.emit_load_imm(0, imag.to_bits() as i32);
                    self.emit_store_r0(imag_off);
                }
                // 返回复数地址
                self.emit_add_imm(0, 11, real_off as u32);
            }
            // __real__ expr: 取复数实部
            Expr::Real { e, .. } => {
                self.gen_expr_from_ast(e);
                // 简化: 假设 r0 已经是实部
            }
            // __imag__ expr: 取复数虚部
            Expr::Imag { e, .. } => {
                self.gen_expr_from_ast(e);
                // 简化: 返回 0 (标量无虚部)
                self.emit_load_imm(0, 0);
            }
            _ => {
                if let Ok(v) = crate::const_eval::eval(expr) {
                    self.emit_load_imm(0, v as i32);
                } else {
                    self.emit_load_imm(0, 0);
                }
            }
        }
    }
}
