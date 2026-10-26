//! asm/opmap.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
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

    /// 该约束是否要求 x86-64 的 legacy 寄存器类（`q` / `Q`）。
    /// 只服务于「规划器返回 None 之后要不要报 impossible constraint」这一个判定，
    /// 不参与分配本身，所以不必与规划器的字母表严格同步。
    fn constraint_demands_legacy_gp(raw: &str) -> bool {
        let digits = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
        let core = raw
            .trim_start_matches(['=', '+', '&', '%'])
            .trim_start_matches(digits);
        !core.contains('{') && core.chars().any(|c| c == 'q' || c == 'Q')
    }

    /// 本条 asm 实际用过的 callee-saved 号必须登记回占用真源（P12 Phase 6.1/6.2）。
    ///
    /// GP 池现在含 rbx/r12/r14/r15：操作数多于 9 个 GP 号时，过去会**静默复用同一个号**
    /// （实测十个 `"=r"` 输出得 `s=64`，gcc 是 55：第十个输出落在了第一个的家上）。
    /// 而 callee-saved 拿来做操作数，有两件事必须同时成立：
    ///   (a) 序言那条保存不得被 `patch_callee_saves` 改成 nop，否则本函数毁掉调用者存在
    ///       %rbx 里的值（§7.15 实测过同一族：修前 `caller_holds_rbx` 得 1500，gcc 是 500）；
    ///   (b) 表达式溅出池（`alloc_temp`）不得再把同一个号发出去，否则会盖掉已就位的操作数。
    /// `RegBank::claim` 一次做完这两件（它置的就是那两个位）。
    ///
    /// caller-saved 的号不进 claim：它们本来就不在溅出池里，钉住它们只会让后续每条 asm
    /// 可选的号逐步耗空（规划器已在 `finish` 内按池内/池外分流）。
    ///
    /// 历史路径：旧实现靠调用点在「输出分配完后」「整张表建完后」各调一次
    /// `own_asm_operand_gp`（幂等）来登记。Phase 6.2 之后这件事由
    /// `AsmRegisterPlanner::finish(&mut reg_bank)` 一次性完成，旧入口已删除。

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
                                                                  // For non-Ident inout (`+r`) outputs, prepare_asm_binding evaluates the
                                                                  // current value into the bound register (triggering side-effects like
                                                                  // bar() in `*bar()`).  The lvalue address is computed once here and held
                                                                  // in a callee-saved scratch across the asm, so writeback can reuse it
                                                                  // instead of re-running gen_lvalue_addr (which would re-trigger bar()).
        let mut saved_inout_addrs: Vec<Option<u8>> = Vec::new();

        // 解析 clobber 列表, 把被 clobber 的寄存器加入统一规划器的排除集合。
        let (clobber_gp, clobber_xmm, _x87_count) = Self::parse_clobber_registers(clobbers);
        // 寄存器所有权只有一个真源：`RegBank`。构造时一次性把「本条 asm 之外已有主人」
        // 的 GP 号（函数级 pinned + 溅出池正占的号）快照进规划器的排除集合；
        // 规划期不再持有 `&mut RegBank`，因此操作数求值（`prepare_asm_binding` 等
        // `&mut self` 方法）可以与规划器同时活跃。规划结束后由 `planner.finish(&mut reg_bank)`
        // 一次性把 callee-saved 号 claim 回占用真源。
        let owned_gp: Vec<u8> = self.reg_bank.owned_gp();
        let mut planner = AsmRegisterPlanner::new(&owned_gp, &clobber_gp, &clobber_xmm);

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
                    // 字符串字面量立即数: eval_asm_operand 返回 .L.str.const.<hash> label,
                    // 但 codegen 之前没把这个字符串落进 string_pool (g_expr_literal 才会
                    // 落池; 内联汇编里 "%c" 把 label 直接嵌入自定义节, 不经 gen_expr).
                    // 这里补落一份 rodata, 保证自定义节里对 .L.str.const.N 的引用能
                    // 反向定位到 rodata 偏移 (resolve.rs lookup_str_const_label_offset).
                    if let Expr::String { data, .. } = op {
                        if !self.string_pool.contains_key(data) {
                            let o = self.rodata.len() as u64;
                            self.rodata.extend_from_slice(data);
                            self.string_pool.insert(data.clone(), o);
                        }
                    }
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
                let name = self.asm_reg_name_or_report(chosen);
                self.prepare_asm_binding(op, chosen, !is_inout);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            } else if Self::constraint_demands_legacy_gp(raw) {
                // 与输入侧同一条判据：legacy 类耗尽时报 impossible constraint，
                // 不静默换成别的寄存器（那会丢写回，实测第 7 个输出全是 0）。
                self.asm_reject("impossible constraint: 'q' has no free legacy register");
            }

            // 内存地址 (p): lea 到一个 GP 寄存器, 模板里就是 %reg
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Output);
                self.asm_load_address_to_reg(op, chosen);
                let name = self.asm_reg_name_or_report(chosen);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 默认: GP 寄存器
            let chosen = planner.alloc_gp(idx, AsmRegUse::Output);
            let name = self.asm_reg_name_or_report(chosen);
            // 对非 Ident inout (`+r`) 输出 (如 `"+r"(*bar())`): 先把 lvalue 地址
            // 算到 callee-saved scratch (bar() 在此调用一次), 再从该地址 load
            // 旧值到绑定寄存器 (无副作用). writeback 复用 scratch 里的地址, 不再
            // 重新 gen_lvalue_addr —— 否则 bar() 会被调两次 (990130-1.c).
            let saved = if is_inout && !matches!(op, Expr::Ident { .. }) {
                match self.alloc_callee_saved() {
                    Some(scratch) if self.gen_lvalue_addr(op, scratch) => {
                        // mov chosen, [scratch]: 从已算好的地址 load 旧值.
                        let sz = op.ty().size();
                        let mut rex = 0x40u8;
                        if chosen >= 8 {
                            rex |= 0x04;
                        }
                        if scratch >= 8 {
                            rex |= 0x01;
                        }
                        match sz {
                            1 => {
                                // movzx eax, byte [scratch]: 0F B6 /r
                                if rex != 0x40 {
                                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                                }
                                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB6]));
                            },
                            2 => {
                                // movzx eax, word [scratch]: 0F B7 /r
                                self.emit_insn(&Insn::raw_bytes(&[0x66]));
                                if rex != 0x40 {
                                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                                }
                                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB7]));
                            },
                            4 => {
                                if rex != 0x40 {
                                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                                }
                                self.emit_insn(&Insn::raw_bytes(&[0x8B]));
                            },
                            _ => {
                                self.emit_insn(&Insn::raw_bytes(&[rex | 0x08]));
                                self.emit_insn(&Insn::raw_bytes(&[0x8B]));
                            },
                        }
                        self.emit_reg_indirect_modrm(chosen, scratch);
                        Some(scratch)
                    },
                    Some(scratch) => {
                        self.free_callee_saved(scratch);
                        None
                    },
                    None => None,
                }
            } else {
                self.prepare_asm_binding(op, chosen, !is_inout);
                None
            };
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
            saved_inout_addrs.push(saved);
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
                    if let Expr::String { data, .. } = op {
                        if !self.string_pool.contains_key(data) {
                            let o = self.rodata.len() as u64;
                            self.rodata.extend_from_slice(data);
                            self.string_pool.insert(data.clone(), o);
                        }
                    }
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
                // 走到这里：约束只能接立即数，操作数却不是编译期常量（内联没能把实参传进来）。
                // GCC 对这一形状直接报 `impossible constraint in 'asm'`；往下掉去分配寄存器
                // 则把 `%c[feature]` 换成裸寄存器名，数据伪指令 `.4byte rax` 就当符号引用发
                // 一条解不开的重定位（实测 `vmlinux.o` 的 `.altinstructions` 特性字段上 98 条
                // `→ rax`，objtool 报 `special: can't find orig instruction`）。
                //
                // 硬失败而不只报警：当前要求内核全树由 dcc 编出，不允许拿 gcc 兼掉，所以这里
                // 必须把「dcc 还缺什么」当场钉在编译步骤上（也让全树一轮就能把待补站点列齐），
                // 而不是交出一个能编过但值错的产物。先决条件是 `try_stmt_inline_call` 能
                // 展开带 asm 的 `__always_inline` 体（包含 void 体与表达式位置）。
                if Self::asm_constraint_is_immediate_only(raw) {
                    self.warn_asm_immediate_only_operand_runtime(raw, idx);
                    // 把「到底是哪棵表达式没能折成常量」直接写在诊断里：全树跑一轮就能
                    // 把待补的常量求值形态列齐，不必一个个 TU 去回放猜。
                    self.asm_reject(&format!(
                        "impossible constraint in 'asm': operand {idx} with immediate-only \
                         constraint \"{raw}\" has no compile-time constant value \
                         (expression: {})",
                        crate::const_eval::ExprDebug(op)
                    ));
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Imm(String::new()));
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
                        self.emit_insn(&Insn::raw(&[0x66]));
                        if xi >= 8 {
                            self.emit_insn(&Insn::raw(&[0x44]));
                        }
                        self.emit_insn(&Insn::raw(&[0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]));
                    } else {
                        self.emit_insn(&Insn::raw(&[0x66]));
                        let rex = 0x48 | if xi >= 8 { 0x44 } else { 0 };
                        self.emit_insn(&Insn::raw(&[rex, 0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]));
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
                    let name = self.asm_reg_name_or_report(chosen);
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
                        self.gen_expr(op, Self::EVAL_BASE);
                        // xmm0 → GP (按类型大小复制位型)
                        if matches!(op.ty(), Type::Float) {
                            self.emit_insn(&Insn::raw(&[
                                0x66,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]));
                        } else {
                            self.emit_insn(&Insn::raw(&[
                                0x66,
                                0x48,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]));
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
                let name = self.asm_reg_name_or_report(chosen);
                self.prepare_asm_binding(op, chosen, false);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            } else if Self::constraint_demands_legacy_gp(raw) {
                // legacy 类（rax/rbx/rcx/rdx/rsi/rdi）里已经没有空闲号 —— 这就是约束
                // 满足不了。绝不退成「换一个别的寄存器」：实测那样会丢最后一个操作数
                // 的写回，只是换一种错值。报 impossible constraint，出路是既有的
                // --asm-fallback=gas（整 TU 交给 GAS 编码）。
                self.asm_reject("impossible constraint: 'q' has no free legacy register");
            }

            // 内存地址 (p)
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
                self.asm_load_address_to_reg(op, chosen);
                let name = self.asm_reg_name_or_report(chosen);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            }

            // GP 寄存器
            let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
            let name = self.asm_reg_name_or_report(chosen);
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
                let new_name = self.asm_reg_name_or_report(cand);
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

        // x87 FPU 栈加载: 每 fld 都会把栈顶 (st(0)) 压向 st(1), 因此
        // **目标 st 号大的最后加载** 才能让 st(0) = st_idx=0 的操作数.
        // 先加载 st_idx=N (N>=1), 最后加载 st_idx=0; 这样 st(0) 是最后一次
        // fld 的结果 (对应 st_idx=0), st(1)/st(N) 是先前 fld 压下来的旧顶.
        if !pending_x87.is_empty() {
            pending_x87.sort_by(|a, b| b.0.cmp(&a.0)); // st_idx 降序: 大的先 fld
            for (st_idx, expr) in pending_x87 {
                self.prepare_asm_x87_binding(expr, st_idx);
            }
        }

        // 把 meta 转成 AsmOpBinding
        bindings.clear();
        // 本条 asm 收尾：规划期所有 callee-saved 号（rbx/r12/r14/r15）在这里一次性
        // claim 进 RegBank（置 allocated+used_by_self）；caller-saved 号不在溅出池里，
        // 无需登记。取 planner 所有权，因此这是「规划期结束」的类型边界。
        let exhausted = planner.gp_exhausted();
        planner.finish(&mut self.reg_bank);
        // GP 池（含 callee-saved 四个）全被占时 `alloc_gp` 只能复用已发出去的号，那是静默
        // 错值；点名它（与 legacy `q` 池耗空那条同一口径），而不是把一个确定会盖值的
        // 分配交出去。
        if exhausted {
            self.asm_reject(
                "impossible constraint: inline asm has more GP operands than free registers",
            );
        }
        for (i, (reg, xmm_w, is_out, is_mem, flags)) in meta.iter().enumerate() {
            let saved = if i < saved_inout_addrs.len() {
                saved_inout_addrs[i]
            } else {
                None
            };
            bindings.push(AsmOpBinding {
                reg: *reg,
                is_output: *is_out,
                is_mem: *is_mem,
                early_clobber: (flags & 1) != 0,
                xmm_width: *xmm_w,
                saved_inout_addr_reg: saved,
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
    /// Prepare an asm operand.  `is_pure_output` is true when the constraint is
    /// `=r` (write-only — asm will completely overwrite, no need to pre-load
    /// the current value).  It is false for `+r` (inout — must pre-load) and
    /// for pure inputs.
    ///
    /// Returns `Some(addr_reg)` only when the operand is a non-Ident inout
    /// lvalue and the caller should remember this callee-saved scratch across
    /// the asm so writeback can reuse the address instead of re-evaluating
    /// side-effecting sub-expressions (e.g. `"+r"(*bar())` calling `bar()`
    /// twice — see 990130-1.c).
    fn prepare_asm_binding(&mut self, expr: &Expr, reg: u8, is_pure_output: bool) -> Option<u8> {
        if let Expr::Ident { name, .. } = expr {
            if self.is_global_not_local(&name.name) {
                if is_pure_output {
                    self.emit_global_addr(reg, &name.name);
                } else {
                    self.gen_expr(expr, reg);
                }
            } else if is_pure_output {
                let off = self.var_offset(&name.name);
                self.emit_lea_r13(reg, off);
            } else {
                self.gen_expr(expr, reg);
            }
            return None;
        }
        // 非 Ident 表达式
        if is_pure_output {
            // `=r`: asm 完全覆盖, 无需预加载, 也不需要保存地址 (writeback 时再算一次即可).
            return None;
        }
        // inout (`+r`) 或纯输入: 求值当前值到绑定寄存器.
        self.gen_expr(expr, reg);
        // 对 inout (由输出侧调用) 我们再单独在 caller 里计算一次地址并溅到 scratch.
        None
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
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x6E]));
            } else {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x6F]));
            }
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((mm & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((mm & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            }
            return;
        }
        // 非变量: gen_expr 到 rax, 再 movd/mm
        self.gen_expr(expr, Self::EVAL_BASE);
        self.emit_insn(&Insn::raw(&[0x0F, 0x6E, 0xC0 | ((mm & 7) << 3)])); // movd mm, eax
    }
    fn prepare_asm_seg_binding(&mut self, expr: &Expr, seg_code: u8) {
        if let Expr::Ident { name, .. } = expr {
            // 局部优先 (同上): 同名遮蔽时取栈地址而非全局地址
            if self.is_global_not_local(&name.name) {
                // 全局: lea 到 rax, 再 mov Sreg, [rax]
                self.emit_global_addr(0, &name.name);
                // mov Sreg, [rax] = 8E /r (mod=00, reg=seg, r/m=0)
                self.emit_insn(&Insn::raw(&[0x8E, (seg_code & 7) << 3]));
            } else {
                let off = self.var_offset(&name.name);
                // mov Sreg, [r13+off] = 8E /r (REX.B, reg=seg, r/m=5)
                self.emit_insn(&Insn::raw(&[0x41, 0x8E]));
                if off.abs() < 128 {
                    self.emit_insn(&Insn::raw(&[0x45 | ((seg_code & 7) << 3), off as i8 as u8]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x85 | ((seg_code & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                }
            }
            return;
        }
        self.gen_expr(expr, Self::EVAL_BASE);
        self.emit_insn(&Insn::raw(&[0x8E, (seg_code & 7) << 3]));
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
                // 先发射再按 `pos()-4` 取 disp32（发射入口可能在本条之前插字节，
                // 发射前算的绝对位置会整条失效）。与 `mem/access.rs` 同一口径。
                self.emit_insn(&Insn::raw(&[opcode, modrm, 0, 0, 0, 0]));
                self.push_global_float_rel(self.pos() - 4, &name.name);
            } else {
                let off = self.var_offset(&name.name);
                self.emit_x87_fld_at(13, off as i32, &effective_ty);
            }
            return;
        }
        // 非变量表达式: gen_expr 按通道约定求值（ld 在 st0、f32/f64 在
        // xmm0、整数在 rax），再统一搬到 st0；旧代码无条件发 fld m64
        // [rax]，把整数/指针位型当 double 且漏掉 xmm0/st0 两种来源。
        self.gen_expr(expr, Self::EVAL_BASE);
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
        self.gen_expr(expr, Self::EVAL_BASE);
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
    /// 「只能接立即数」的约束拿到了运行时值：当场点名，不静默。
    ///
    /// GCC 对这一形状直接报 `impossible constraint in 'asm'`。dcc 先把内联的
    /// `__always_inline` 展开试一遍（`opt.rs::try_stmt_inline_call`），只有它也没能
    /// 把实参传进来时才会走到这里，此时操作数会落成寄存器，模板里 `%c[op]`/`%P[op]`
    /// 会被换成裸寄存器名，数据伪指令就会发一条指向不存在符号的重定位（实测
    /// `vmlinux.o` 的 `.altinstructions` 特性字段上有 98 条 `→ rax`，objtool 报
    /// `special: can't find orig instruction`）。报而不拒是因为那些体带 `asm goto` 的
    /// 内核分派当前还不能在表达式位置展开，退给宿主编译器会让大半棵树不再由 dcc 编；
    /// 但风险必须在日志里看得见，不能只到链接后才露出来。
    pub(crate) fn warn_asm_immediate_only_operand_runtime(&mut self, constraint: &str, idx: usize) {
        use crate::diagnostics::{Diagnostic, Span};
        let (file, line) = self.asm_src_pos();
        let ordinal = self.current_asm_line;
        let mut diag = Diagnostic::warn(
            "W1016",
            format!(
                "inline asm operand {idx} has an immediate-only constraint \"{constraint}\" but no \
                 compile-time constant value (not inlined); `%c`/`%P` references to it will emit an \
                 unresolved reference"
            ),
            Span::point(if file.is_empty() { "<asm>" } else { &file }, line, 1),
        );
        diag.hint = Some(format!(
            "asm 语句第 {ordinal} 行：把被调函数改成能在表达式位置内联，或给该操作数加上 \
             \"r\" 备选约束"
        ));
        self.diagnostics.push(diag);
    }

    pub(crate) fn warn_unresolved_asm_operand(&mut self, line: &str, asm_line: usize) {
        self.asm_reject(&format!(
            "unsupported inline asm operand pattern '{}' (asm line {})",
            line, asm_line
        ));
    }
    /// 把 asm 输出寄存器写回一个**非 Ident** 左值（`a[i]` / `*p` / `s.f` / `a[i].f`）。
    ///
    /// 为什么需要它：`writeback_asm_operand` 的每个分支都以
    /// `let Expr::Ident { .. } = expr else { return }` 开头，于是这类输出被**静默丢掉**。
    /// 实测 `__asm__ volatile("movq $5, %0" : "=q"(a[1]))` 之后 `a[1]` 仍是 0（gcc 是 5），
    /// `"=r"(a[2])`、`"=q"(gs[0])`（全局数组）同样丢 —— 而「把结果取到数组元素」是
    /// 内核与库代码里的常见写法。
    ///
    /// 返回 `false` 表示这条路走不了（地址算不出，或宽度不是一条 store 能表达的：
    /// 聚合 / 向量 / long double / 128 位整数）。调用方保留原有行为：宁可维持旧状态，
    /// 也不把值写到一个算错的位置。
    ///
    /// scratch 必须来自 callee-saved 池：GP 分配池 (`AsmRegisterPlanner::GP_POOL`) 只含
    /// caller-saved 号，所以这个 scratch 一定不是本句 asm 的任何操作数寄存器；
    /// `alloc_callee_saved` 同时置 `reg_allocated` 与 `callee_saved_used`，因此它也不会
    /// 与外层表达式的溅出槽相撞，并且序言会真的保存它。写回结束立刻归还。
    fn writeback_asm_general_lvalue(
        &mut self,
        expr: &Expr,
        src: u8,
        xmm_width: u8,
        saved_addr_reg: Option<u8>,
    ) -> bool {
        let ty = expr.ty();
        let effective_ty = match &ty {
            Type::Imaginary(inner) => inner.base.clone(),
            other => other.clone(),
        };
        let excluded = matches!(
            &effective_ty,
            Type::Void
                | Type::LongDouble
                | Type::Complex(_)
                | Type::Imaginary(_)
                | Type::Array(..)
                | Type::Vector(..)
                | Type::Struct(..)
                | Type::Union(..)
                | Type::Func(..)
                | Type::Named(_)
                | Type::Int128
                | Type::UInt128
        );
        let sz = effective_ty.size();
        if excluded || sz == 0 || sz > 8 {
            return false;
        }
        // float / double 的输出寄存器是 xmm，存法不同（movss / movsd）。注意
        // `"=x"` 的 `xmm_width` 一律是 128（见 `asm_constraint_wants_xmm`），所以
        // 宽度必须按**类型**判，而不是照抄 xmm_width。这一条现在才敢做：
        // `movsd %xmm1, %xmm0` 以前被编成字符串搬移 0xA5（见 emit_lines.rs 里
        // `movsd` 的守卫），写回还没走到程序就已经崩了。
        let float_bits = match &effective_ty {
            Type::Float => Some(32u8),
            Type::Double => Some(64u8),
            _ => None,
        };
        if xmm_width != 0 && float_bits.is_none() {
            // 操作数在 xmm 里但类型不是 float/double：不知道该怎么存，保持旧行为。
            return false;
        }
        // 调用方已在 prepare 阶段算好 lvalue 地址并溅到一个 callee-saved scratch；
        // 直接用它，避免重跑 gen_lvalue_addr 触发副作用子表达式二次求值
        // (如 `"+r"(*bar())` 里 bar() 被调两次 —— 见 990130-1.c)。
        let scratch = match saved_addr_reg {
            Some(r) => r,
            None => match self.alloc_callee_saved() {
                Some(r) => r,
                None => return false,
            },
        };
        let addr_ready = saved_addr_reg.is_some() || self.gen_lvalue_addr(expr, scratch);
        if !addr_ready {
            if saved_addr_reg.is_none() {
                self.free_callee_saved(scratch);
            }
            return false;
        }
        match float_bits {
            Some(bits) => self.store_xmm_to_ptr(src, scratch, bits),
            None => self.store_reg_to_ptr(src, scratch, sz),
        }
        if saved_addr_reg.is_none() {
            self.free_callee_saved(scratch);
        }
        true
    }

    pub(crate) fn writeback_asm_operand(
        &mut self,
        expr: &Expr,
        reg: u8,
        xmm_width: u8,
        saved_addr_reg: Option<u8>,
    ) {
        // 非 Ident 左值先走通用写回（原因与实测见 `writeback_asm_general_lvalue`）。
        // 必须放在最前面：下面每个分支都以 `Expr::Ident` 为前提，非 Ident 落进去就是
        // 静默丢值。x87 / MMX / 段寄存器 / 条件码（`xmm_width` 的 2xx 编码）仍走各自
        // 的分支 —— 它们的宽度语义与 GP/SSE 两条不同，混进来只会让诊断变模糊。
        if !matches!(expr, Expr::Ident { .. })
            && matches!(xmm_width, 0 | 32 | 64 | 128)
            && self.writeback_asm_general_lvalue(expr, reg, xmm_width, saved_addr_reg)
        {
            return;
        }
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
                // 同上：先发射再取 disp32。
                self.emit_insn(&Insn::raw(&[opcode, modrm, 0, 0, 0, 0]));
                self.push_global_float_rel(self.pos() - 4, &name.name);
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
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x7E]));
            } else {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x7F]));
            }
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((reg & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((reg & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
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
            self.emit_insn(&Insn::raw(&[0x41, 0x8C]));
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((reg & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((reg & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
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
            self.emit_insn(&Insn::raw(&[0x50])); // push %rax (preserve across setcc)
            self.emit_insn(&Insn::raw(&[0x0F, reg, 0xC0])); // setcc %al
            self.emit_insn(&Insn::raw(&[0x0F, 0xB6, 0xC0])); // movzx %al, %eax
            self.emit_insn(&Insn::raw(&[Self::rex(true, 0, 0, 13), 0x89, 0x85])); // mov %rax, [r13+off]
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            self.emit_insn(&Insn::raw(&[0x58])); // pop %rax (restore)
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
                    self.emit_insn(&Insn::raw(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                },
                16 => {
                    self.emit_insn(&Insn::raw(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                },
                32 => {
                    self.emit_insn(&Insn::raw(&[
                        Self::rex(true, reg, 0, 13),
                        0x89,
                        0x85 | ((reg & 7) << 3),
                    ]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
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
        // 全局变量写回：使用 RIP-relative store，不得走 var_offset 栈槽路径
        // (pr98096.c 里 "+r"(i) 全局触发 E1134/E1133)。
        if self.is_global_not_local(&name.name) {
            self.emit_global_store(reg, &name.name);
            return;
        }
        // 变量绑在寄存器里（用户的 `register asm`，或 P12 的自动分配）时，它的家
        // 不是栈槽。旧代码无条件按 `[r13+off]` 写回，于是 asm 的结果落进一个再没
        // 人读的槽里：实测 `register long v asm("rbx")` +
        // `__asm__ volatile("movq $5, %0" : "=r"(v))` 之后 `v` 仍是 0，gcc 是 5。
        // 走 `mov_to_stack` 顺带把宽度口径也统一掉（见 `var_access_shape`）。
        if self.homes.contains_key(&name.name) {
            if xmm_width == 0 {
                self.mov_to_stack(reg, &name.name);
                return;
            }
            // SSE 的输出值在 xmm 里，而变量的家是 GP 寄存器：先按宽度存进栈槽，
            // 再从槽读进一个临时 callee-saved 号，最后仍由 `mov_to_stack` 进绑定
            // 寄存器。这个组合（`register double v asm("rbx")` + `"=x"(v)`）偏到
            // 角落，但没有任何理由让它变成静默错值。
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            if matches!(ty, Type::Float) {
                self.store_xmm32_stack(reg, off);
            } else {
                self.store_xmm64_stack(reg, off);
            }
            if let Some(tmp) = self.alloc_callee_saved() {
                self.load_stack_sized(tmp, off, sz);
                self.mov_to_stack(tmp, &name.name);
                self.free_callee_saved(tmp);
            }
            return;
        }
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
            self.emit_insn(&Insn::raw(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else if sz <= 2 {
            self.emit_insn(&Insn::raw(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else if sz <= 4 {
            // 32-bit 整型写回需零扩展到 64 位槽, 否则后续 64-bit 重载读到栈残留
            // 旧实现用 `mov %eax, [r13+off]` (4 字节), C codegen 后续用 64-bit 读
            // (mov -off(%r13), %rax) 会读到未初始化高 32 位.
            // 改为 `mov %rax, [r13+off]` (REX.W + REX.B, 8 字节), 保证 64-bit 槽整体更新.
            self.emit_insn(&Insn::raw(&[
                Self::rex(true, reg, 0, 13),
                0x89,
                0x85 | ((reg & 7) << 3),
            ]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else {
            self.store_stack(reg, off);
        }
    }
}
