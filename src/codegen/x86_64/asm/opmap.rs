//! asm/opmap.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
impl Codegen {
    fn asm_format_subst(val: &str) -> String {
        // 缺失操作数占位符会被保留为 `%[name]` / `%N`; 这些不是
        // mini-asm 可编码操作数, 让指令整体走 warning + nop fallback。
        if val.starts_with("%[") || val.starts_with('%') {
            return val.to_string();
        }
        // 内存操作数 (含 '('), 立即数 (以 $ 开头), 偏移 (以 - 开头),
        // 纯数字或符号立即数: 直接使用, 不加 % 前缀
        if val.contains('(')
            || val.starts_with('$')
            || val.starts_with('-')
            || val.chars().next().is_some_and(|c| c.is_ascii_digit())
            || Self::asm_symbolic_operand(val)
        {
            val.to_string()
        } else {
            format!("%{val}")
        }
    }
    pub(crate) fn asm_format_subst_safe(val: &str) -> String {
        let base = Self::asm_format_subst(val);
        if val.starts_with('$') && Self::asm_imm_needs_parens(val) {
            // $(expr) 形式: 把 $ 移到括号外
            format!("$({})", &val[1..])
        } else {
            base
        }
    }
    pub(crate) fn asm_has_unresolved_operand(line: &str) -> bool {
        line.contains("%[")
            || line
                .as_bytes()
                .windows(2)
                .any(|w| w[0] == b'%' && w[1].is_ascii_digit())
    }
    fn asm_load_address_to_reg(&mut self, expr: &Expr, reg: u8) {
        // p 约束: 把指针值加载到寄存器 (而不是 lea 变量地址)
        // 对于指针变量, 加载其值; 对于其他表达式, 按值求值
        self.gen_expr(expr, reg);
    }
    pub(crate) fn build_asm_operand_map(
        &mut self,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        bindings: &mut Vec<AsmOpBinding>,
    ) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let mut idx = 0usize;
        // x87 FPU 栈加载需要按 st() 从高到低顺序执行 (fld 推栈,
        // 最后一次 fld 才是 st(0)). 收集所有 x87 operand, 统一在
        // outputs+inputs 都处理完后再排序加载.
        let mut pending_x87: Vec<(u8, &Expr)> = Vec::new();

        // 内部占位: 每个操作数分配状态
        enum Slot {
            /// 立即数 (模板内联)
            Imm(String),
            /// 内存操作数 (模板直接出现)
            Mem(String),
            /// 寄存器 (GP 或 XMM)
            Reg(u8, &'static str),
        }
        let mut slots: Vec<Slot> = Vec::new();
        let mut meta: Vec<(u8, u8, bool, bool, u8)> = Vec::new(); // (reg, xmm_width, is_output, is_mem, flags_raw)
                                                                  // flags_raw: bit 0 = early_clobber

        // 解析 clobber 列表, 把被 clobber 的寄存器加入统一规划器的排除集合。
        let (clobber_gp, clobber_xmm, _x87_count) = Self::parse_clobber_registers(clobbers);
        let mut planner = AsmRegisterPlanner::new(&clobber_gp, &clobber_xmm);

        // 预扫描输出约束: 预留固定寄存器 (a/b/c/d/S/D),
        // 避免 =r 等通用分配抢占后续 =a 等固定约束所需的寄存器。
        for oc in output_constraints.iter() {
            let core = oc.trim_start_matches(['=', '+', '&', '%']);
            if core.contains('{') || core.starts_with("@cc") {
                continue;
            }
            let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
            if letters.iter().any(|c| matches!(c, 'r' | 'R' | 'g')) {
                continue;
            }
            let fixed: Option<u8> = if letters.contains(&'a') {
                Some(0)
            } else if letters.contains(&'b') {
                Some(3)
            } else if letters.contains(&'c') {
                Some(1)
            } else if letters.contains(&'d') {
                Some(2)
            } else if letters.contains(&'S') {
                Some(6)
            } else if letters.contains(&'D') {
                Some(7)
            } else {
                None
            };
            if let Some(reg) = fixed {
                planner.reserve_gp(reg, AsmRegUse::Output);
            }
        }

        // ===== 处理 outputs =====
        for (op_idx, op) in outputs.iter().enumerate() {
            let raw = output_constraints
                .get(op_idx)
                .map(String::as_str)
                .unwrap_or("=r");
            let (is_output, is_inout, early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let letter = Self::asm_pick_sub_constraint(raw, is_inout);
            let _ = is_output; // 已经在 flags 派生

            // match-digit: 复用先前操作数 (仅当 N < 当前全局 idx)
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    match slot {
                        Slot::Imm(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                        },
                        Slot::Mem(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                        },
                        Slot::Reg(r, name) => {
                            let nreg = *r;
                            map.insert(format!("%{idx}"), (*name).to_string());
                            planner.reserve_gp(nreg, AsmRegUse::Output);
                            meta.push((nreg, 0, true, false, 0));
                        },
                    }
                    slots.push(match slot {
                        Slot::Imm(s) => Slot::Imm(s.clone()),
                        Slot::Mem(s) => Slot::Mem(s.clone()),
                        Slot::Reg(r, n) => Slot::Reg(*r, n),
                    });
                    idx += 1;
                    continue;
                }
            }

            // GCC 条件码约束 (@ccz/@ccs/@ccc 等): 不可见操作数, 不在模板中替换
            // 写回时通过 setcc 指令捕获条件标志
            if let Some(cc_letters) = Self::asm_parse_cc_constraint(raw) {
                if let Some(setcc_op) = Self::cc_letter_to_setcc_opcode(cc_letters) {
                    slots.push(Slot::Imm(String::new())); // 占位
                    meta.push((setcc_op, 204, true, false, if early { 1 } else { 0 }));
                    idx += 1;
                    continue;
                }
            }

            // 显式寄存器约束 ({eax}/{cx}/{cl}/{gs}/{fs} 等)
            if let Some(eri) = Self::asm_parse_explicit_reg(raw) {
                let name = eri.att_name;
                match eri.kind {
                    ExplicitRegKind::Seg => {
                        // 段寄存器输出: 模板里用 %gs/%fs 等
                        // 写回时通过 mov %seg, [r13+off] 存储
                        if is_inout {
                            self.prepare_asm_seg_binding(op, eri.reg);
                        }
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, 203, true, false, if early { 1 } else { 0 }));
                        idx += 1;
                        continue;
                    },
                    ExplicitRegKind::Gp => {
                        // 显式 GP 子寄存器: 模板里用 %eax/%cx/%cl 等
                        let xmm_w = match eri.size {
                            8 => 205u8,
                            16 => 206,
                            32 => 207,
                            64 => 208,
                            _ => 0,
                        };
                        // +r (读写): 加载变量值; =r (只写): lea 地址 (将被 asm 覆写)
                        self.prepare_asm_binding(op, eri.reg, !is_inout);
                        planner.reserve_gp(eri.reg, AsmRegUse::Output);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, xmm_w, true, false, if early { 1 } else { 0 }));
                        idx += 1;
                        continue;
                    },
                }
            }

            // x87 FPU 约束 (f/t/u)
            if let Some(st_idx) = Self::asm_constraint_wants_x87(raw) {
                let xmm_w = if st_idx == 0 { 200u8 } else { 201 };
                let name: &'static str = if st_idx == 0 { "st" } else { "%st(1)" };
                if is_inout {
                    pending_x87.push((st_idx, op));
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(st_idx, name));
                meta.push((st_idx, xmm_w, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // MMX 约束 (y)
            if Self::asm_constraint_wants_mmx(raw) {
                let mi = planner.alloc_mmx(idx);
                let name = Self::asm_mmx_name(mi as usize);
                if is_inout {
                    self.prepare_asm_mmx_binding(op, mi);
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(mi, name));
                meta.push((mi, 202, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 立即数约束且操作数可常量求值
            if matches!(letter, "i") && Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = Self::eval_asm_operand(op) {
                    let att_val = if val.starts_with('$') {
                        val.clone()
                    } else if val.starts_with('-')
                        || val.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        format!("${val}")
                    } else {
                        val.clone()
                    };
                    map.insert(format!("%{idx}"), att_val.clone());
                    slots.push(Slot::Imm(att_val));
                    meta.push((0, 0, true, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数 (m 优先)
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                if let Some(mem) = self.asm_memory_operand_str(op) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, true, true, if early { 1 } else { 0 }));
                    idx += 1;
                    continue;
                }
            }

            // XMM 寄存器约束
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                if !Self::asm_type_allows_xmm(&op.ty()) {
                    self.asm_reject(
                        "inconsistent operand constraints in an 'asm': long double needs an x87 constraint (t/u/f)",
                    );
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Reg(0, "st"));
                    meta.push((0, 200, true, false, 0));
                    idx += 1;
                    continue;
                }
                let xi = planner.alloc_xmm(idx, AsmRegUse::Output);
                let name = Self::asm_xmm_name(xi as usize);
                if is_inout {
                    self.prepare_asm_xmm_binding(op, xi, true);
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi, name));
                meta.push((xi, xmm_bits, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 固定 GP 字母约束 (a/b/c/d/S/D/q/Q)
            if let Some((chosen, _fixed)) =
                planner.alloc_gp_for_constraint(raw, idx, AsmRegUse::Output)
            {
                let name = Self::gp_reg_name(chosen as usize);
                self.prepare_asm_binding(op, chosen, !is_inout);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 内存地址 (p): lea 到一个 GP 寄存器, 模板里就是 %reg
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Output);
                self.asm_load_address_to_reg(op, chosen);
                let name = Self::gp_reg_name(chosen as usize);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 默认: GP 寄存器
            let chosen = planner.alloc_gp(idx, AsmRegUse::Output);
            let name = Self::gp_reg_name(chosen as usize);
            // +r (读写): 加载变量值; =r (只写): lea 地址 (将被 asm 覆写)
            self.prepare_asm_binding(op, chosen, !is_inout);
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
            idx += 1;
        }

        // ===== 处理 inputs =====
        for (op_idx, op) in inputs.iter().enumerate() {
            let raw = input_constraints
                .get(op_idx)
                .map(String::as_str)
                .unwrap_or("r");
            let (_is_output, is_inout, _early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let _letter = Self::asm_pick_sub_constraint(raw, is_inout);

            // match-digit: 复用先前操作数的寄存器, 但仍需 prepare_asm_binding
            // 把当前操作数 (作为输入) 的值加载到该寄存器, 不能只复制 slot.
            // 例如 "=r"(b) : "0"(a): 输出准备把 &b 加载到 rax, 但输入 a 应当把
            // a 的值 (42) 重新加载到 rax, 覆盖 &b, 这样 asm "add $1, %0" 才能
            // 在 rax 上做正确算术 (rax = 42 + 1 = 43), 再写回 b.
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    if let Slot::Reg(r, name) = slot {
                        let nreg = *r;
                        let nname = *name;
                        let mwidth = meta.get(n as usize).map(|m| m.1).unwrap_or(0);
                        let is_xmm = mwidth != 0 && mwidth < 200;
                        let is_x87 = mwidth == 200 || mwidth == 201;
                        if is_x87 {
                            // x87 栈寄存器复用: defer fld, 等所有 operand 处理
                            // 完后再按 st() 从高到低统一加载.
                            pending_x87.push((nreg, op));
                        } else if is_xmm {
                            self.prepare_asm_xmm_binding(op, nreg, is_inout);
                        } else {
                            // 对当前操作数 (输入) 调 prepare, 把值加载到复用寄存器
                            self.prepare_asm_binding(op, nreg, false);
                        }
                        map.insert(format!("%{idx}"), nname.to_string());
                        if is_xmm {
                            planner.reserve_xmm(nreg, AsmRegUse::Input);
                        } else if !is_x87 {
                            planner.reserve_gp(nreg, AsmRegUse::Input);
                        }
                        meta.push((nreg, mwidth, false, false, 0));
                        slots.push(Slot::Reg(nreg, nname));
                        idx += 1;
                        continue;
                    }
                    // 非 Reg slot (Imm/Mem): 直接复制即可
                    match slot {
                        Slot::Imm(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                        },
                        Slot::Mem(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                        },
                        Slot::Reg(_, _) => unreachable!(),
                    }
                    slots.push(match slot {
                        Slot::Imm(s) => Slot::Imm(s.clone()),
                        Slot::Mem(s) => Slot::Mem(s.clone()),
                        Slot::Reg(r, n) => Slot::Reg(*r, n),
                    });
                    idx += 1;
                    continue;
                }
            }

            // 显式寄存器约束 ({eax}/{cx}/{cl}/{gs}/{fs} 等)
            if let Some(eri) = Self::asm_parse_explicit_reg(raw) {
                let name = eri.att_name;
                match eri.kind {
                    ExplicitRegKind::Seg => {
                        self.prepare_asm_seg_binding(op, eri.reg);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, 203, false, false, 0));
                        idx += 1;
                        continue;
                    },
                    ExplicitRegKind::Gp => {
                        let xmm_w = match eri.size {
                            8 => 205u8,
                            16 => 206,
                            32 => 207,
                            64 => 208,
                            _ => 0,
                        };
                        self.prepare_asm_binding(op, eri.reg, false);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        planner.reserve_gp(eri.reg, AsmRegUse::Input);
                        meta.push((eri.reg, xmm_w, false, false, 0));
                        idx += 1;
                        continue;
                    },
                }
            }

            // x87 FPU 约束 (f/t/u) — 延迟加载, 等全部 operand 处理完
            // 后按 st() 从高到低顺序统一 fld.
            if let Some(st_idx) = Self::asm_constraint_wants_x87(raw) {
                let xmm_w = if st_idx == 0 { 200u8 } else { 201 };
                let name: &'static str = if st_idx == 0 { "st" } else { "%st(1)" };
                pending_x87.push((st_idx, op));
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(st_idx, name));
                meta.push((st_idx, xmm_w, false, false, 0));
                idx += 1;
                continue;
            }

            // MMX 约束 (y)
            if Self::asm_constraint_wants_mmx(raw) {
                let mi = planner.alloc_mmx(idx);
                let name = Self::asm_mmx_name(mi as usize);
                self.prepare_asm_mmx_binding(op, mi);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(mi, name));
                meta.push((mi, 202, false, false, 0));
                idx += 1;
                continue;
            }

            // 立即数约束且操作数可常量求值
            if Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = Self::eval_asm_operand(op) {
                    let att_val = if val.starts_with('$') {
                        val.clone()
                    } else if val.starts_with('-')
                        || val.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        format!("${val}")
                    } else {
                        val.clone()
                    };
                    map.insert(format!("%{idx}"), att_val.clone());
                    slots.push(Slot::Imm(att_val));
                    meta.push((0, 0, false, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                if let Some(mem) = self.asm_memory_operand_str(op) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, false, true, 0));
                    idx += 1;
                    continue;
                }
            }

            // XMM
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                if !Self::asm_type_allows_xmm(&op.ty()) {
                    self.asm_reject(
                        "inconsistent operand constraints in an 'asm': long double needs an x87 constraint (t/u/f)",
                    );
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Reg(0, "st"));
                    meta.push((0, 200, false, false, 0));
                    idx += 1;
                    continue;
                }
                let xi = planner.alloc_xmm(idx, AsmRegUse::Input);
                let name = Self::asm_xmm_name(xi as usize);
                if matches!(op.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.prepare_asm_xmm_binding(op, xi, is_inout);
                } else {
                    // W0005 兼容路径：约束要求 XMM 但表达式是整数/指针。
                    // 为保持 warning-only 语义，按整数位型加载到 GP 后
                    // movq/movd 进 XMM，避免把 GP 当 XMM 直接使用而崩溃。
                    self.prepare_asm_binding(op, 0, false);
                    if op.ty().size() <= 4 {
                        self.emit(&[0x66]);
                        if xi >= 8 {
                            self.emit(&[0x44]);
                        }
                        self.emit(&[0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]);
                    } else {
                        self.emit(&[0x66]);
                        let rex = 0x48 | if xi >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]);
                    }
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi, name));
                meta.push((xi, xmm_bits, false, false, 0));
                idx += 1;
                continue;
            }

            if matches!(op.ty(), Type::Float | Type::Double | Type::LongDouble) {
                // 浮点值绑到 GP 约束 (如 "r"(1.5F)): 兼容路径.
                // GCC 允许此类绑定 (仅告警), 将浮点位型加载到 GP 寄存器.
                // 920520-1.c: asm("%0"::"r"(1.5F)) 期望 %0 展开为 GP 寄存器名.
                if Self::asm_constraint_is_gp(raw) {
                    // 将浮点位型加载到 GP 寄存器 (而非 XMM).
                    // 常量浮点值: 直接 mov 位型; 非变量: 从内存加载位型.
                    let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
                    let name = Self::gp_reg_name(chosen as usize);
                    // 尝试直接求值浮点常量位型
                    if let Expr::Float { c, ty, .. } = op {
                        // 浮点常量的位型加载到 GP 寄存器。
                        // long double 无法整体放入 GP 寄存器，此处为 GCC
                        // 兼容降级路径：显式转 f64 取其 64 位位型。
                        let bits: u64 = if matches!(ty, Type::Float) {
                            // float: 32 位
                            (c.as_f64() as f32).to_bits() as u64
                        } else {
                            // double / long double 降级: 64 位
                            c.as_f64().to_bits()
                        };
                        self.mov_imm(chosen, bits as i128);
                    } else if let Expr::Ident {
                        name: vname,
                        ty: vty,
                        ..
                    } = op
                    {
                        // 变量: 从内存加载位型 (按类型大小)
                        let off = self.var_offset(&vname.name);
                        let sz = vty.size();
                        if sz <= 4 {
                            self.load_stack_sized(chosen, off, sz);
                        } else {
                            self.gen_expr(op, chosen);
                        }
                    } else {
                        // 其他表达式: 走 gen_expr (可能加载到 xmm0, 再 mov 到 gp)
                        // 对非常量表达式, 从临时区加载位型
                        self.gen_expr(op, 0);
                        // xmm0 → GP (按类型大小复制位型)
                        if matches!(op.ty(), Type::Float) {
                            self.emit(&[
                                0x66,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]);
                        } else {
                            self.emit(&[
                                0x66,
                                0x48,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]);
                        }
                    }
                    map.insert(format!("%{idx}"), name.to_string());
                    slots.push(Slot::Reg(chosen, name));
                    meta.push((chosen, 0, false, false, 0));
                    idx += 1;
                    continue;
                } else {
                    let xi = planner.alloc_xmm(idx, AsmRegUse::Input);
                    let name = Self::asm_xmm_name(xi as usize);
                    self.prepare_asm_xmm_binding(op, xi, is_inout);
                    map.insert(format!("%{idx}"), name.to_string());
                    slots.push(Slot::Reg(xi, name));
                    meta.push((
                        xi,
                        if matches!(op.ty(), Type::Float) { 32 } else { 64 },
                        false,
                        false,
                        0,
                    ));
                    idx += 1;
                    continue;
                }
            }

            // 固定 GP 字母约束 (a/b/c/d/S/D/q/Q)
            if let Some((chosen, _fixed)) =
                planner.alloc_gp_for_constraint(raw, idx, AsmRegUse::Input)
            {
                let name = Self::gp_reg_name(chosen as usize);
                self.prepare_asm_binding(op, chosen, false);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            }

            // 内存地址 (p)
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
                self.asm_load_address_to_reg(op, chosen);
                let name = Self::gp_reg_name(chosen as usize);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            }

            // GP 寄存器
            let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
            let name = Self::gp_reg_name(chosen as usize);
            self.prepare_asm_binding(op, chosen, false);
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, false, false, 0));
            idx += 1;
        }

        // ===== early-clobber: 输出寄存器不能与任何输入寄存器重叠 =====
        let num_outputs = outputs.len();
        for i in 0..num_outputs {
            let (reg, xmm_w, is_out, is_mem, flags) = meta[i];
            if !is_out || is_mem || (flags & 1) == 0 {
                continue;
            }
            // 跳过非 GP/XMM 类型 (x87, MMX, 段寄存器, 条件码, 显式子寄存器)
            // 这些类型不参与 GP/XMM 寄存器池的 early-clobber 重分配
            if xmm_w >= 200 {
                continue;
            }
            if xmm_w != 0 {
                // XMM 输出 + early-clobber: 找一个未被 XMM 输入占用的 XMM
                if !planner.input_xmm_contains(reg) {
                    continue;
                }
                if let Some(cand) = planner.reassign_early_xmm(reg, i) {
                    let new_name = Self::asm_xmm_name(cand as usize);
                    let key = format!("%{i}");
                    if let Some(op) = outputs.get(i) {
                        let raw = output_constraints
                            .get(i)
                            .map(String::as_str)
                            .unwrap_or("=r");
                        let (_, is_inout, _, _) = Self::parse_asm_constraint_flags(raw);
                        if is_inout {
                            self.prepare_asm_xmm_binding(op, cand, true);
                        }
                    }
                    map.insert(key, new_name.to_string());
                    slots[i] = Slot::Reg(cand, new_name);
                    meta[i] = (cand, xmm_w, true, false, flags);
                }
                continue;
            }
            if !planner.input_gp_contains(reg) {
                continue;
            }
            // 找一个不冲突的 GP
            if let Some(cand) = planner.reassign_early_gp(reg, i) {
                let new_name = Self::gp_reg_name(cand as usize);
                let key = format!("%{i}");
                if let Some(op) = outputs.get(i) {
                    let raw = output_constraints
                        .get(i)
                        .map(String::as_str)
                        .unwrap_or("=r");
                    let (_, is_inout, _, _) = Self::parse_asm_constraint_flags(raw);
                    self.prepare_asm_binding(op, cand, !is_inout);
                }
                map.insert(key, new_name.to_string());
                slots[i] = Slot::Reg(cand, new_name);
                meta[i] = (cand, 0, true, false, flags);
            }
        }

        // x87 FPU 栈加载: 所有 operand 已登记到 slots/meta, 现在按
        // st() 从高到低顺序统一 fld — 保证最后一次 fld 落在 st(0),
        // 前一次落在 st(1), 以此类推.
        if !pending_x87.is_empty() {
            pending_x87.sort_by(|a, b| b.0.cmp(&a.0)); // st_idx 降序
            for (st_idx, expr) in pending_x87 {
                self.prepare_asm_x87_binding(expr, st_idx);
            }
        }

        // 把 meta 转成 AsmOpBinding
        bindings.clear();
        for (i, (reg, xmm_w, is_out, is_mem, flags)) in meta.iter().enumerate() {
            let _ = i;
            bindings.push(AsmOpBinding {
                reg: *reg,
                is_output: *is_out,
                is_mem: *is_mem,
                early_clobber: (flags & 1) != 0,
                xmm_width: *xmm_w,
            });
        }

        // name aliases
        for (i, name) in output_names.iter().enumerate() {
            if let Some(n) = name {
                if let Some(reg) = map.get(&format!("%{}", i)) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        for (i, name) in input_names.iter().enumerate() {
            if let Some(n) = name {
                let key = format!("%{}", outputs.len() + i);
                if let Some(reg) = map.get(&key) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        map
    }
    fn prepare_asm_binding(&mut self, expr: &Expr, reg: u8, is_output: bool) {
        if let Expr::Ident { name, .. } = expr {
            // 局部优先: 与全局同名的参数/局部变量不得误入 rip 全局路径
            if self.is_global_not_local(&name.name) {
                if is_output {
                    // 只写输出 (如 "=r"): asm 将完全覆盖, 只需要地址给 writeback
                    self.emit_global_addr(reg, &name.name);
                } else {
                    // 输入或读写 (如 "+r"): 必须加载当前值到寄存器
                    self.gen_expr(expr, reg);
                }
            } else if is_output {
                // 输出: lea 出变量地址, writeback 通过 [r13+off] 写回
                let off = self.var_offset(&name.name);
                self.emit_lea_r13(reg, off);
            } else {
                // 输入: 直接 load 变量值, asm 模板里使用该值
                self.gen_expr(expr, reg);
            }
        } else {
            self.gen_expr(expr, reg);
        }
    }
    fn prepare_asm_mmx_binding(&mut self, expr: &Expr, mm: u8) {
        if let Expr::Ident { name, ty, .. } = expr {
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            // movd/movq [r13+off], mmN
            // movd m32, mm = 0F 7E ; movq m64, mm = 0F 7F
            // 注意: reg=mm_index, r/m=5 (r13)
            // 但方向是内存→寄存器: movd mm, [r13+off] = 0F 6E
            // movq mm, [r13+off] = 0F 6F
            if sz <= 4 {
                self.emit(&[0x41, 0x0F, 0x6E]);
            } else {
                self.emit(&[0x41, 0x0F, 0x6F]);
            }
            if off.abs() < 128 {
                self.emit(&[0x45 | ((mm & 7) << 3), off as i8 as u8]);
            } else {
                self.emit(&[0x85 | ((mm & 7) << 3)]);
                self.emit(&(off as i32).to_le_bytes());
            }
            return;
        }
        // 非变量: gen_expr 到 rax, 再 movd/mm
        self.gen_expr(expr, 0);
        self.emit(&[0x0F, 0x6E, 0xC0 | ((mm & 7) << 3)]); // movd mm, eax
    }
    fn prepare_asm_seg_binding(&mut self, expr: &Expr, seg_code: u8) {
        if let Expr::Ident { name, .. } = expr {
            // 局部优先 (同上): 同名遮蔽时取栈地址而非全局地址
            if self.is_global_not_local(&name.name) {
                // 全局: lea 到 rax, 再 mov Sreg, [rax]
                self.emit_global_addr(0, &name.name);
                // mov Sreg, [rax] = 8E /r (mod=00, reg=seg, r/m=0)
                self.emit(&[0x8E, (seg_code & 7) << 3]);
            } else {
                let off = self.var_offset(&name.name);
                // mov Sreg, [r13+off] = 8E /r (REX.B, reg=seg, r/m=5)
                self.emit(&[0x41, 0x8E]);
                if off.abs() < 128 {
                    self.emit(&[0x45 | ((seg_code & 7) << 3), off as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((seg_code & 7) << 3)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            }
            return;
        }
        self.gen_expr(expr, 0);
        self.emit(&[0x8E, (seg_code & 7) << 3]);
    }
    fn prepare_asm_x87_binding(&mut self, expr: &Expr, st_idx: u8) {
        let _ = st_idx; // 仅作注释, 不影响本函数行为
        if let Expr::Ident { name, ty, .. } = expr {
            // _Imaginary: 按内型
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            // 局部优先: 与全局同名的参数/局部变量必须走栈槽 fld
            if self.is_global_not_local(&name.name) {
                // 全局: RIP 相对 fld，float=D9 /5 double=DD /5 ld=DB /5
                let (opcode, modrm) = match effective_ty {
                    Type::Float => (0xD9u8, 0x05u8),
                    Type::LongDouble => (0xDBu8, 0x2Du8),
                    _ => (0xDDu8, 0x05u8),
                };
                let disp_pos = self.pos() + 2;
                self.emit(&[opcode, modrm, 0, 0, 0, 0]);
                self.push_global_float_rel(disp_pos, &name.name);
            } else {
                let off = self.var_offset(&name.name);
                self.emit_x87_fld_at(13, off as i32, &effective_ty);
            }
            return;
        }
        // 非变量表达式: gen_expr 按通道约定求值（ld 在 st0、f32/f64 在
        // xmm0、整数在 rax），再统一搬到 st0；旧代码无条件发 fld m64
        // [rax]，把整数/指针位型当 double 且漏掉 xmm0/st0 两种来源。
        self.gen_expr(expr, 0);
        self.emit_operand_to_st0(&expr.ty(), 0);
    }
    fn prepare_asm_xmm_binding(&mut self, expr: &Expr, xmm: u8, _is_inout: bool) {
        if let Expr::Ident { name, ty, .. } = expr {
            if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
                || matches!(ty, Type::Imaginary(_))
            {
                // 局部优先: 与全局同名的参数/局部变量 (如 double b 与
                // static float b[]) 必须走栈槽加载, 不得误入全局 rip 路径
                if self.is_global_not_local(&name.name) {
                    self.emit_global_float_to_xmm_typed(xmm, &name.name, ty);
                } else {
                    let off = self.var_offset(&name.name);
                    let effective_ty = if let Type::Imaginary(ref inner) = ty {
                        inner.base.clone()
                    } else {
                        ty.clone()
                    };
                    self.emit_mem_float_to_xmm(13, off as i32, &effective_ty, xmm);
                }
                return;
            }
        }
        self.gen_expr(expr, 0);
        let ty = expr.ty();
        if self.float_in_st0(&ty) {
            // long double 表达式结果在 st0：经栈 scratch 单步舍入到 f64
            self.emit_x87_to_xmm0();
            if xmm != 0 {
                self.mov_xmm_to_xmm(xmm, 0);
            }
        } else if matches!(ty, Type::Float | Type::Double) {
            self.mov_xmm_to_xmm(xmm, 0);
        }
    }
    pub(crate) fn warn_unresolved_asm_operand(&mut self, line: &str, asm_line: usize) {
        self.asm_reject(&format!(
            "unsupported inline asm operand pattern '{}' (asm line {})",
            line, asm_line
        ));
    }
    pub(crate) fn writeback_asm_operand(&mut self, expr: &Expr, reg: u8, xmm_width: u8) {
        // x87 FPU 栈寄存器写回 (f/t/u 约束): fstp 存储 st(0) 到内存
        if xmm_width == 200 || xmm_width == 201 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            if self.is_global_not_local(&name.name) {
                // 全局: RIP 相对 fstp
                // float=D9 /3 double=DD /3 long double=DB /7
                let (opcode, modrm) = match effective_ty {
                    Type::Float => (0xD9u8, 0x1Du8),
                    Type::LongDouble => (0xDBu8, 0x3Du8),
                    _ => (0xDDu8, 0x1Du8),
                };
                let disp_pos = self.pos() + 2;
                self.emit(&[opcode, modrm, 0, 0, 0, 0]);
                self.push_global_float_rel(disp_pos, &name.name);
            } else {
                let off = self.var_offset(&name.name);
                self.emit_x87_fstp_at(13, off as i32, &effective_ty);
            }
            return;
        }
        // MMX 寄存器写回 (y 约束): movq/movd 存储 mm 到内存
        if xmm_width == 202 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            // movq mm, [r13+off] = 0F 7F ; movd mm, [r13+off] = 0F 7E
            // reg 字段 = mm 索引, r/m = 5 (r13)
            if sz <= 4 {
                self.emit(&[0x41, 0x0F, 0x7E]);
            } else {
                self.emit(&[0x41, 0x0F, 0x7F]);
            }
            if off.abs() < 128 {
                self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
            } else {
                self.emit(&[0x85 | ((reg & 7) << 3)]);
                self.emit(&(off as i32).to_le_bytes());
            }
            return;
        }
        // 段寄存器写回 ({gs}/{fs} 约束): mov %seg, [r13+off] (16-bit)
        if xmm_width == 203 {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            // mov [r13+off], Sreg = 8C /r (16-bit)
            // REX.B for r13, reg = seg code, r/m = 5
            self.emit(&[0x41, 0x8C]);
            if off.abs() < 128 {
                self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
            } else {
                self.emit(&[0x85 | ((reg & 7) << 3)]);
                self.emit(&(off as i32).to_le_bytes());
            }
            return;
        }
        // 条件码写回 (@cc 约束): setcc + movzx + store
        // 使用 push/pop rax 保护其他输出操作数的寄存器值
        if xmm_width == 204 {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            // reg 字段 = setcc opcode (0F 90+off)
            self.emit(&[0x50]); // push %rax (preserve across setcc)
            self.emit(&[0x0F, reg, 0xC0]); // setcc %al
            self.emit(&[0x0F, 0xB6, 0xC0]); // movzx %al, %eax
            self.emit(&[Self::rex(true, 0, 0, 13), 0x89, 0x85]); // mov %rax, [r13+off]
            self.emit(&(off as i32).to_le_bytes());
            self.emit(&[0x58]); // pop %rax (restore)
            return;
        }
        // 显式子寄存器写回 ({ecx}/{cx}/{cl} 约束)
        if (205..=208).contains(&xmm_width) {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            let size_bits = (xmm_width - 204) * 8; // 8, 16, 32, 64
            match size_bits {
                8 => {
                    self.emit(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]);
                    self.emit(&(off as i32).to_le_bytes());
                },
                16 => {
                    self.emit(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]);
                    self.emit(&(off as i32).to_le_bytes());
                },
                32 => {
                    self.emit(&[Self::rex(true, reg, 0, 13), 0x89, 0x85 | ((reg & 7) << 3)]);
                    self.emit(&(off as i32).to_le_bytes());
                },
                _ => {
                    self.store_stack(reg, off);
                },
            }
            return;
        }
        if xmm_width != 0 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            if self.is_global_not_local(&name.name) {
                if self.float_in_st0(&effective_ty) {
                    // xmm 约束写回 long double 全局：拓宽后 fstp tbyte
                    let src_ty = if matches!(effective_ty, Type::Float) {
                        Type::Float
                    } else {
                        Type::Double
                    };
                    self.emit_xmm_to_global_float_typed(reg, &name.name, &src_ty, &effective_ty);
                } else {
                    self.emit_global_float_store_from_xmm(reg, &name.name, &effective_ty);
                }
            } else {
                let off = self.var_offset(&name.name);
                if self.float_in_st0(&effective_ty) {
                    let src_ty = if matches!(effective_ty, Type::Float) {
                        Type::Float
                    } else {
                        Type::Double
                    };
                    self.emit_xmm_to_mem_float(reg, &src_ty, 13, off as i32, &effective_ty);
                } else if matches!(effective_ty, Type::Float) || xmm_width == 32 {
                    self.store_xmm32_stack(reg, off);
                } else {
                    self.store_xmm64_stack(reg, off);
                }
            }
            return;
        }
        let Expr::Ident { name, ty, .. } = expr else {
            return;
        };
        if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
            || matches!(ty, Type::Imaginary(_))
        {
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            let off = self.var_offset(&name.name);
            if self.float_in_st0(&effective_ty) {
                // 该兜底分支不可达：long double 输出必走 xmm/x87 绑定
                // （xmm_width != 0），不会落到 GP 写回路径。
                unreachable!("long double writeback must use xmm/x87 binding");
            } else if matches!(effective_ty, Type::Float) {
                self.store_xmm32_stack(reg, off);
            } else {
                self.store_xmm64_stack(reg, off);
            }
            return;
        }
        let off = self.var_offset(&name.name);
        let sz = ty.size();
        if sz <= 1 {
            self.emit(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]);
            self.emit(&(off as i32).to_le_bytes());
        } else if sz <= 2 {
            self.emit(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]);
            self.emit(&(off as i32).to_le_bytes());
        } else if sz <= 4 {
            // 32-bit 整型写回需零扩展到 64 位槽, 否则后续 64-bit 重载读到栈残留
            // 旧实现用 `mov %eax, [r13+off]` (4 字节), C codegen 后续用 64-bit 读
            // (mov -off(%r13), %rax) 会读到未初始化高 32 位.
            // 改为 `mov %rax, [r13+off]` (REX.W + REX.B, 8 字节), 保证 64-bit 槽整体更新.
            self.emit(&[Self::rex(true, reg, 0, 13), 0x89, 0x85 | ((reg & 7) << 3)]);
            self.emit(&(off as i32).to_le_bytes());
        } else {
            self.store_stack(reg, off);
        }
    }
}
