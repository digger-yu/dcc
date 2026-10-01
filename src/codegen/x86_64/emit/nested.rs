//! emit/nested.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    pub(crate) fn collect_all_funcs(prog: &Program, out: &mut Vec<String>) {
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    out.push(f.name.name.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_func_names(&body.items, out);
                    }
                }
            }
        }
    }
    fn collect_nested_func_names(items: &[BlockItem], out: &mut Vec<String>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                if f.body.is_some() {
                    out.push(f.name.name.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_func_names(&body.items, out);
                    }
                }
            }
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_func_names_stmt(s, out);
            }
        }
    }
    fn collect_nested_func_names_stmt(s: &Stmt, out: &mut Vec<String>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_func_names(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_nested_func_names_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_nested_func_names_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_nested_func_names_stmt(body, out);
            },
            Stmt::For { init, body, .. } => {
                if let Some(i) = init {
                    Self::collect_nested_func_names_stmt(i, out);
                }
                Self::collect_nested_func_names_stmt(body, out);
            },
            _ => {},
        }
    }
    pub(crate) fn emit_mprotect_for_stack(&mut self) {
        // mprotect((r13 & ~0xFFF) - 0x1000, 0x2000, PROT_READ|PROT_WRITE|PROT_EXEC)
        // trampoline 槽位于帧底部下方 (r13-256..r13): 当 r13 页内偏移
        // < 0x160 时 trampoline 跨入下一页, 仅保护 r13 所在单页会遗漏
        // 导致跳转到不可执行页 SIGSEGV。覆盖 r13 上方页 + 下方页。
        // rdi = r13 & ~0xFFF
        // 注意: mov rdi, r13 需要 REX.W + REX.B (r13 >= 8)
        // 4C 89 EF = mov rdi, r13 (REX.WR, src=r13 in r/m, dst=rdi in reg)
        self.emit(&[0x4C, 0x89, 0xEF]); // mov rdi, r13
        self.emit(&[0x48, 0x81, 0xE7]); // and rdi, ~0xFFF
        self.emit(&0xFFFFF000u32.to_le_bytes());
        // rdi -= 0x1000 (下方页)
        self.emit(&[0x48, 0x81, 0xEF]); // sub rdi, 0x1000
        self.emit(&0x1000u32.to_le_bytes());
        // rsi = 0x2000 (两页)
        self.emit(&[0x48, 0xC7, 0xC6]); // mov rsi, 0x2000
        self.emit(&0x2000i32.to_le_bytes());
        // rdx = 7 (PROT_READ|PROT_WRITE|PROT_EXEC)
        self.emit(&[0x48, 0xC7, 0xC2]); // mov rdx, 7
        self.emit(&7i32.to_le_bytes());
        // rax = 10 (SYS_mprotect)
        self.emit(&[0x48, 0xC7, 0xC0]); // mov rax, 10
        self.emit(&10i32.to_le_bytes());
        // syscall
        self.emit(&[0x0F, 0x05]);
        // syscall 破坏: rax, rcx, r11
        // 恢复 rax (可能被后续代码使用)
        // 注意: rcx, r11 被 syscall 破坏，但它们是 caller-saved，所以没问题
    }
    pub(crate) fn emit_trampoline_to_stack(&mut self) {
        if self.trampoline_slots.is_empty() {
            return;
        }
        // 收集所有 trampoline 名称，避免借用冲突
        let tramp_names: Vec<String> = self.trampoline_slots.keys().cloned().collect();
        for name in &tramp_names {
            let tramp_off = self.trampoline_slots[name];
            // 加载嵌套函数地址到 rax
            self.emit_func_addr(0, name);
            // rdi = trampoline 目标地址 = r13 + tramp_off
            // lea rdi, [r13 + tramp_off]
            self.emit_lea_r13(7, tramp_off);
            // 写入 trampoline 字节到 [rdi + offset]
            // Trampoline 布局 (32 字节, 与 SysV ABI 嵌套函数约定一致):
            // [0..2)   movabs r10, imm64 = 49 ba (静态链 = 定义父帧指针)
            // [2..10)  <定义父帧 r13 值, 写入时从当前 r13 捕获>
            // [10..12) movabs r11, imm64 = 49 bb
            // [12..20) <func_addr 8 bytes>
            // [20..23) jmp r11           = 41 ff e3
            // [23..32) nop padding (不执行)
            // 关键: 静态链必须在写入时捕获定义父帧的 r13 值烘焙为立即数,
            // 而非调用时读取调用者 r13 — 否则外部函数调用函数指针时
            // 静态链会指向调用者帧, 嵌套函数访问父帧 upvar 时读到垃圾。
            // 写入字节 0-1: movabs r10 = 49 ba
            self.emit(&[0x48, 0xC6, 0x07, 0x49]); // mov BYTE [rdi+0], 0x49
            self.emit(&[0x48, 0xC6, 0x47, 0x01, 0xBA]); // mov BYTE [rdi+1], 0xBA
                                                        // 写入字节 2-9: 定义父帧指针 (当前 r13 值)
                                                        // mov [rdi+2], r13 = 4C 89 6F 02 (REX.WB, r13 为源寄存器)
            self.emit(&[0x4C, 0x89, 0x6F, 0x02]);
            // 写入字节 10-11: movabs r11 = 49 bb
            self.emit(&[0x48, 0xC6, 0x47, 0x0A, 0x49]); // mov BYTE [rdi+10], 0x49
            self.emit(&[0x48, 0xC6, 0x47, 0x0B, 0xBB]); // mov BYTE [rdi+11], 0xBB
                                                        // 写入字节 12-19: 函数地址 (rax 中的 8 字节)
            self.emit(&[0x48, 0x89, 0x47, 0x0C]); // mov [rdi+12], rax
                                                  // 写入字节 20-22: jmp r11 = 41 ff e3
            self.emit(&[0x48, 0xC6, 0x47, 0x14, 0x41]); // mov BYTE [rdi+20], 0x41
            self.emit(&[0x48, 0xC6, 0x47, 0x15, 0xFF]); // mov BYTE [rdi+21], 0xFF
            self.emit(&[0x48, 0xC6, 0x47, 0x16, 0xE3]); // mov BYTE [rdi+22], 0xE3
                                                        // 写入字节 23-31: nop 填充 (不执行)
            self.emit(&[0x48, 0xC6, 0x47, 0x17, 0x90]); // mov BYTE [rdi+23], 0x90
            self.emit(&[0x48, 0xC7, 0x47, 0x18]); // mov QWORD [rdi+24]
            self.emit(&0x9090909090909090u64.to_le_bytes());
        }
    }
    pub(crate) fn gen_trampoline(&mut self, nested_name: &str) -> i64 {
        // 检查是否已为此嵌套函数生成 trampoline
        if let Some(&off) = self.trampoline_slots.get(nested_name) {
            return off;
        }
        // 每个 trampoline 32 字节 (16 字节对齐)
        let tramp_size: i64 = 32;
        // Trampoline 槽位位于帧底部 trampoline reserve (256 字节) 内
        // 从 reserve 顶部向下分配: 第一个槽在 -frame_size + 256 - 32
        let offset = -self.frame_size + (256i64 - self.trampoline_total_size as i64 - tramp_size);
        self.trampoline_total_size += tramp_size as i32;
        self.trampoline_slots
            .insert(nested_name.to_string(), offset);
        offset
    }
    fn load_frame_via_chain(&mut self, dst: u8, hops: i32, offs: &[i64]) {
        for i in 0..hops.max(0) as usize {
            let off = offs.get(i).copied().unwrap_or(self.static_chain_offset);
            if i == 0 {
                self.load_stack(dst, off);
            } else if Self::fits_disp32(off) {
                let rex = 0x48u8 | 0x01 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                self.emit(&[rex, 0x8B, 0x80 | ((dst & 7) << 3) | (dst & 7)]);
                self.emit(&(off as i32).to_le_bytes());
            } else {
                // 罕见: 链槽偏移超出 disp32, 用 r10 作缩放槽
                self.mov_imm(10, off as i128);
                // mov dst, [dst + r10]: REX.X (r10 索引) + REX.R/B (dst 扩展)
                let rex = 0x48u8 | 0x02 | if dst >= 8 { 0x05 } else { 0 };   // 位序: REX.R+B: 自反 ModRM, 同一个寄存器既在 reg 又在 r/m, 判据见 docs/ssa-pass-upgrade-plan.md §7.21
                self.emit(&[rex, 0x8B, 0x80 | ((dst & 7) << 3) | 0x04, 0x10 | (dst & 7)]);
            }
        }
    }
    pub(crate) fn load_static_chain_for_call(&mut self, hops: i32, offs: &[i64]) {
        if hops <= 0 {
            // mov r10, r13 (REX.WRB + 89 /r)
            self.emit(&[0x4F, 0x89, 0xEA]);
            return;
        }
        self.load_frame_via_chain(10, hops, offs);
    }
    pub(crate) fn load_trampoline_addr(&mut self, dst: u8, unique: &str, hops: i32, offs: &[i64]) {
        if hops <= 0 {
            let off = self.gen_trampoline(unique);
            self.emit_lea_r13(dst, off);
            return;
        }
        self.load_frame_via_chain(11, hops, offs);
        let off = self.nested_tramp_offsets.get(unique).copied().unwrap_or(0);
        if Self::fits_disp32(off) {
            let rex = 0x48u8 | 0x01 | if dst >= 8 { 0x04 } else { 0 };
            self.emit(&[rex, 0x8D, 0x80 | ((dst & 7) << 3) | 0x03]);
            self.emit(&(off as i32).to_le_bytes());
        } else {
            self.mov_imm(10, off as i128);
            // lea dst, [r11 + r10]: REX.X (r10) + REX.B (r11) + REX.R (dst 扩展)
            let rex = 0x48u8 | 0x03 | if dst >= 8 { 0x04 } else { 0 };
            self.emit(&[rex, 0x8D]);
            self.emit(&[0x80 | ((dst & 7) << 3) | 0x04, 0x10 | 0x03]);
        }
    }
    pub(crate) fn load_upvar(&mut self, reg: u8, parent_off: i64, size: usize, upvar_name: &str) {
        // r11 = 保存的父级 r13 (立即父帧)
        self.load_stack(11, self.static_chain_offset);
        // 多层静态链上溯: 祖父级 upvar 需要继续上溯中间帧
        let intermediates: Vec<i64> = self
            .upvar_intermediate_offsets
            .get(upvar_name)
            .cloned()
            .unwrap_or_default();
        for &off in &intermediates {
            if Self::fits_disp32(off) {
                // mov r11, [r11 + off]  (REX.W.RB if r11>=8)
                let rex = 0x48u8 | if 11 >= 8 { 0x05u8 } else { 0 };
                let modrm = 0x83 | ((11 & 7) << 3);
                self.emit(&[rex, 0x8B, modrm]);
                self.emit(&(off as i32).to_le_bytes());
            } else {
                self.mov_imm(10, off as i128);
                // lea r11, [r11 + r10]
                self.emit(&[0x4D, 0x8D, 0x1C, 0xD3]);
            }
        }
        if Self::fits_disp32(parent_off) {
            let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
            let modrm = 0x83 | ((reg & 7) << 3);
            match size {
                1 => self.emit(&[rex, 0x0F, 0xB6, modrm]),
                2 => self.emit(&[rex, 0x0F, 0xB7, modrm]),
                4 => self.emit(&[rex, 0x8B, modrm]),
                _ => self.emit(&[rex | 0x08, 0x8B, modrm]),
            }
            self.emit(&(parent_off as i32).to_le_bytes());
        } else {
            self.mov_imm(10, parent_off as i128);
            self.emit(&[0x4D, 0x8D, 0x1C, 0x13]); // lea r11, [r11+r10]
            let rex = match size {
                1 | 2 | 4 => 0x41 | if reg >= 8 { 0x04 } else { 0 },
                _ => 0x49 | if reg >= 8 { 0x04 } else { 0 },
            };
            match size {
                1 => self.emit(&[rex, 0x0F, 0xB6]),
                2 => self.emit(&[rex, 0x0F, 0xB7]),
                4 => self.emit(&[rex, 0x8B]),
                _ => self.emit(&[rex, 0x8B]),
            }
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }
    pub(crate) fn materialize_nested_compound_lits(&mut self, inits: &[InitItem]) {
        for item in inits {
            match item {
                InitItem::Expr(e) => self.materialize_nested_compound_lits_expr(e),
                InitItem::Designated { value, .. } => {
                    self.materialize_nested_compound_lits_expr(value);
                },
            }
        }
    }
    fn materialize_nested_compound_lits_expr(&mut self, e: &Expr) {
        match e {
            Expr::AddrOf { e: inner, .. } => {
                // 直接 AddrOf(CompoundLit) → 物化
                if matches!(inner.as_ref(), Expr::CompoundLit { .. } | Expr::InitList { .. }) {
                    self.materialize_compound_literal_rel(inner);
                }
            },
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                // 嵌套 CompoundLit/InitList: 递归处理子 items
                self.materialize_nested_compound_lits(inits);
            },
            Expr::Cast { e: inner, .. } => {
                self.materialize_nested_compound_lits_expr(inner);
            },
            _ => {},
        }
    }
    pub(crate) fn resolve_nested_func_ref(&self, orig: &str) -> Option<(String, i32, Vec<i64>)> {
        let mut cur = self.current_func_name.clone();
        let mut hops: i32 = 0;
        let mut offs: Vec<i64> = vec![self.static_chain_offset];
        loop {
            if let Some(m) = self.nested_func_renames.get(&cur) {
                if let Some(u) = m.get(orig) {
                    return Some((u.clone(), hops, offs));
                }
            }
            let parent = self.nested_func_parent.get(&cur)?.clone();
            if let Some(&o) = self.nested_chain_offsets.get(&parent) {
                offs.push(o);
            }
            cur = parent;
            hops += 1;
        }
    }
    pub(crate) fn store_upvar(&mut self, reg: u8, parent_off: i64, size: usize, upvar_name: &str) {
        self.load_stack(11, self.static_chain_offset);
        let intermediates: Vec<i64> = self
            .upvar_intermediate_offsets
            .get(upvar_name)
            .cloned()
            .unwrap_or_default();
        for &off in &intermediates {
            if Self::fits_disp32(off) {
                let rex = 0x48u8 | if 11 >= 8 { 0x05u8 } else { 0 };
                let modrm = 0x83 | ((11 & 7) << 3);
                self.emit(&[rex, 0x8B, modrm]);
                self.emit(&(off as i32).to_le_bytes());
            } else {
                self.mov_imm(10, off as i128);
                self.emit(&[0x4D, 0x8D, 0x1C, 0xD3]);
            }
        }
        if Self::fits_disp32(parent_off) {
            let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
            let modrm = 0x83 | ((reg & 7) << 3);
            match size {
                1 => self.emit(&[rex, 0x88, modrm]),
                2 => self.emit(&[0x66, rex, 0x89, modrm]),
                4 => self.emit(&[rex, 0x89, modrm]),
                _ => self.emit(&[rex | 0x08, 0x89, modrm]),
            }
            self.emit(&(parent_off as i32).to_le_bytes());
        } else {
            self.mov_imm(10, parent_off as i128);
            self.emit(&[0x4D, 0x8D, 0x1C, 0x13]); // lea r11, [r11+r10]
            match size {
                1 => {
                    let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
                    self.emit(&[rex, 0x88]);
                    self.emit_reg_indirect_modrm(reg, 11);
                },
                2 => {
                    let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
                    self.emit(&[0x66, rex, 0x89]);
                    self.emit_reg_indirect_modrm(reg, 11);
                },
                4 => {
                    let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
                    self.emit(&[rex, 0x89]);
                    self.emit_reg_indirect_modrm(reg, 11);
                },
                _ => {
                    let rex = 0x49 | if reg >= 8 { 0x04 } else { 0 };
                    self.emit(&[rex, 0x89]);
                    self.emit_reg_indirect_modrm(reg, 11);
                },
            }
        }
    }
}
