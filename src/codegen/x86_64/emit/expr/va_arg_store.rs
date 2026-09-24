//! emit/expr/va_arg_store.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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
    fn emit_va_arg_int128_from_gp(&mut self, _ap_name: &str, dst: u8, from_reg_save: bool) {
        // rcx = [va_list + gp_offset_off] -> reg_save_area 或 overflow_arg_area
        let gp_offset_off = if from_reg_save { 16 } else { 8 };
        self.emit(&[0x49, 0x8B, 0x4A, gp_offset_off]); // mov rcx, [r10+16/8]

        // 从 rcx 读取低 64 位到 rax
        self.emit(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
                                        // 从 rcx+8 读取高 64 位到 rdx (SysV AMD64: __int128 = rdx:rax)
        self.emit(&[0x48, 0x8B, 0x51, 0x08]); // mov rdx, [rcx+8]

        // 写回 dst (rdx:rax 的低 64 位在 rax，GCC 内部可能只用低 64 位)
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn emit_va_arg_int_load(&mut self, sz: usize, signed: bool, rex: u8, modrm: &[u8]) {
        match (sz, signed) {
            (1, true) => self.emit(&[rex, 0x0F, 0xBE]),  // movsx
            (1, false) => self.emit(&[rex, 0x0F, 0xB6]), // movzx
            (2, true) => self.emit(&[rex, 0x0F, 0xBF]),  // movsx
            (2, false) => self.emit(&[rex, 0x0F, 0xB7]), // movzx
            (4, true) => self.emit(&[rex | 0x48, 0x63]), // movsxd (REX.W)
            (4, false) => self.emit(&[rex, 0x8B]),       // mov eax (零扩展)
            _ => self.emit(&[rex | 0x48, 0x8B]),         // mov rax (REX.W)
        }
        self.emit(modrm);
    }
    pub(crate) fn emit_va_start(&mut self, ap_name: &str) {
        if self.is_global_not_local(ap_name) {
            // 全局 va_list: rip 相对写真全局内存, 与 gen_va_arg_global
            // 的读取路径一致 (跨函数可见, 如 stdarg-1 f2: va_start(gap)
            // 后 bar 内 va_arg(gap,...))。懒分配栈槽是函数私有的, 读写
            // 跨函数不一致。
            self.emit_global_addr(10, ap_name); // r10 = &gap
            self.emit_va_start_at(10);
            return;
        }
        // 局部: 按 va_list 真实大小 (24B=3 槽) 注册栈槽,
        // 避免懒分配 1 槽时四字段写入覆盖相邻变量
        let ap_off = self.register_var(ap_name, &Self::va_list_slot_ty());
        let gp_init = self.variadic_named_gp.min(6) * 8;
        // SSE 命名参数各占 reg_save_area 中 16 字节槽
        let fp_init = 48 + self.variadic_named_fp * 16;
        self.store_stack_imm32(ap_off, gp_init);
        self.store_stack_imm32(ap_off + 4, fp_init);
        // overflow_arg_area: r13 是 push 后的 rsp,
        //   [r13+0]=saved r13, [r13+8]=ret addr, [r13+16]=caller 栈上第 1 个变参.
        // 与 frame_size 无关 — 帧是 sub 出的, 与 caller 栈方向相反.
        let overflow_off: i64 = self.variadic_overflow_offset;
        self.emit_lea_r13(0, overflow_off);
        self.store_stack(0, ap_off + 8);
        self.emit_lea_r13(0, self.variadic_reg_save_offset);
        self.store_stack(0, ap_off + 16);
    }
    pub(crate) fn emit_va_start_at(&mut self, base: u8) {
        let gp_init = self.variadic_named_gp.min(6) * 8;
        let fp_init = 48 + self.variadic_named_fp * 16;
        self.emit_store_imm32_mem(base, 0, gp_init);
        self.emit_store_imm32_mem(base, 4, fp_init);
        self.emit_lea_r13(0, self.variadic_overflow_offset);
        self.emit_store_reg_mem(base, 8, 0);
        self.emit_lea_r13(0, self.variadic_reg_save_offset);
        self.emit_store_reg_mem(base, 16, 0);
    }
    pub(crate) fn gen_va_arg(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        if matches!(arg_ty, Type::Float | Type::Double) && self.target.sse {
            self.gen_va_arg_fp(ap_name, arg_ty, dst);
            return;
        }
        // Long double: SysV MEMORY 类，始终在栈上传递 (overflow_arg_area)
        // 16 字节，不经过寄存器保存区
        if matches!(arg_ty, Type::LongDouble) {
            self.gen_va_arg_long_double(ap_name, dst);
            return;
        }
        // __int128 / unsigned __int128: 三路径 SysV va_arg
        //  - reg2:   gp_offset + 16 <= 48 → 两半均在 reg_save_area
        //  - hybrid: gp_offset == 40 → 低半在 reg_save_area[40], 高半在 overflow[0]
        //  - ovf:    gp_offset >= 48 → 两半均在 overflow_arg_area
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            let ap_off = self.var_offset(ap_name) as i32;
            let _reg2 = self.fresh_label();
            let hybrid = self.fresh_label();
            let ovf = self.fresh_label();
            let done = self.fresh_label();

            // eax = gp_offset
            self.emit(&[0x41, 0x8B, 0x85]);
            self.emit(&ap_off.to_le_bytes());

            // if gp_offset >= 48 → ovf
            self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
            self.jmp_rel32(&ovf, JumpKind::Jge);

            // if gp_offset == 40 → hybrid
            self.emit(&[0x83, 0xF8, 0x28]); // cmp $40, %eax
            self.jmp_rel32(&hybrid, JumpKind::Je);

            // ===== reg2 path: both halves in reg_save_area =====
            // rcx = reg_save_area
            self.emit(&[0x49, 0x8B, 0x8D]);
            self.emit(&(ap_off + 16).to_le_bytes());
            // r10 = low: [rcx + rax*1] — r10 reg2+R, mod=00, rm=100, SIB base=rcx(1), idx=rax(0)
            self.emit(&[0x4C, 0x8B, 0x14, 0x01]); // mov r10, [rcx+rax]
                                                  // r11 = high: [rcx + rax*1 + 8] — r11 reg3+R, mod=01, rm=100, SIB+disp8
            self.emit(&[0x4C, 0x8B, 0x5C, 0x01, 0x08]); // mov r11, [rcx+rax+8]
                                                        // advance gp_offset by 16
            self.emit(&[0x41, 0x83, 0x85]);
            self.emit(&ap_off.to_le_bytes());
            self.emit(&[0x10]);
            self.jmp_rel32(&done, JumpKind::Jmp);

            // ===== hybrid path: low in reg_save_area[40], high in overflow[0] =====
            self.label(&hybrid);
            // rcx = reg_save_area
            self.emit(&[0x49, 0x8B, 0x8D]);
            self.emit(&(ap_off + 16).to_le_bytes());
            // r10 = low: [rcx + 40] (rax already = 40)
            self.emit(&[0x4C, 0x8B, 0x14, 0x01]); // mov r10, [rcx+rax]
                                                  // rax = overflow_arg_area
            self.emit(&[0x49, 0x8B, 0x85]);
            self.emit(&(ap_off + 8).to_le_bytes());
            // r11 = high: [rax] — r11 reg3+R, mod=00, rm=000(rax)
            self.emit(&[0x4C, 0x8B, 0x18]); // mov r11, [rax]
                                            // advance gp_offset by 16
            self.emit(&[0x41, 0x83, 0x85]);
            self.emit(&ap_off.to_le_bytes());
            self.emit(&[0x10]);
            // advance overflow_arg_area by 8
            self.emit(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
            self.emit(&[0x49, 0x89, 0x85]);
            self.emit(&(ap_off + 8).to_le_bytes());
            self.jmp_rel32(&done, JumpKind::Jmp);

            // ===== ovf path: both in overflow_arg_area =====
            self.label(&ovf);
            // eax = gp_offset - 48
            self.emit(&[0x83, 0xE8, 0x30]); // sub eax, 48
                                            // ecx = overflow offset
            self.emit(&[0x89, 0xC1]); // mov ecx, eax
                                      // rax = overflow_arg_area
            self.emit(&[0x49, 0x8B, 0x85]);
            self.emit(&(ap_off + 8).to_le_bytes());
            // r10 = low: [rax + rcx*1] — SIB: base=rax(0), idx=rcx(1) → 0x08
            self.emit(&[0x4C, 0x8B, 0x14, 0x08]); // mov r10, [rax+rcx]
                                                  // r11 = high: [rax + rcx*1 + 8] — r11 reg3+R, mod=01, rm=100
            self.emit(&[0x4C, 0x8B, 0x5C, 0x08, 0x08]); // mov r11, [rax+rcx+8]
                                                        // advance gp_offset by 16
            self.emit(&[0x41, 0x83, 0x85]);
            self.emit(&ap_off.to_le_bytes());
            self.emit(&[0x10]);
            // advance overflow_arg_area by 16
            self.emit(&[0x48, 0x83, 0xC0, 0x10]); // add rax, 16
            self.emit(&[0x49, 0x89, 0x85]);
            self.emit(&(ap_off + 8).to_le_bytes());

            // ===== done: r10=low, r11=high =====
            self.label(&done);
            // __int128 va_arg always returns in rdx:rax (SysV ABI convention).
            // r10 (low) → rax, r11 (high) → rdx.
            self.mov(0, 10); // mov rax, r10
            self.mov(2, 11); // mov rdx, r11
            return;
        }

        // 全局 va_list: 无栈槽，直接 rip 相对访问全局结构 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_global(ap_name, arg_ty, dst);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = gp_offset
        self.emit(&[0x41, 0x8B, 0x85]);
        self.emit(&(ap_off as i32).to_le_bytes());
        self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + gp_offset
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 16) as i32).to_le_bytes());
        let sz = arg_ty.size();
        let signed = Self::va_arg_ty_signed(arg_ty);
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x40, &[0x04, 0x01]),
            2 => self.emit_va_arg_int_load(2, signed, 0x40, &[0x04, 0x01]),
            4 => self.emit_va_arg_int_load(4, signed, 0x40, &[0x04, 0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x04, 0x01]),
        }
        self.emit(&[0x41, 0x83, 0x85]);
        self.emit(&(ap_off as i32).to_le_bytes());
        self.emit(&step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x40, &[0x01]),
            2 => self.emit_va_arg_int_load(2, signed, 0x40, &[0x01]),
            4 => self.emit_va_arg_int_load(4, signed, 0x40, &[0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x01]),
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.emit(&[0x48, 0x83, 0xC1]);
        self.emit(&step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn gen_va_arg_fp(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_fp_global(ap_name, arg_ty, dst);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        let reg_step = 16i32; // reg_save_area 中 XMM 槽为 16 字节对齐
        let overflow_step = 8i32; // overflow_arg_area 中 double/float 8 字节对齐
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = fp_offset (va_list + 4)
        self.emit(&[0x41, 0x8B, 0x85]);
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        // cmp $176, %eax — 176 超出 imm8 范围，须用 32 位立即数
        self.emit(&[0x3D, 0xB0, 0x00, 0x00, 0x00]);
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + fp_offset
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 16) as i32).to_le_bytes());
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x01]); // movss (%rcx,%rax), xmm0
        } else if matches!(arg_ty, Type::LongDouble) {
            // long double: 16 字节，取低 8 字节 double 部分
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x01]); // movsd
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x01]); // movsd (%rcx,%rax), xmm0
        }
        self.emit(&[0x41, 0x83, 0x85]);
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        self.emit(&reg_step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x01]);
        } else {
            // Type::LongDouble 或其他: 统一使用 movsd (long double 调用约定走 SSE 上半部分)
            self.emit(&[0xF2, 0x0F, 0x10, 0x01]);
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        // overflow_arg_area 上 double/float 8 字节步进 (即使 long double 也占用 16 但 movsd 只取 8)
        self.emit(&[0x48, 0x83, 0xC1]);
        self.emit(&overflow_step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    fn gen_va_arg_fp_global(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.emit(&[0x41, 0x8B, 0x42, 0x04]); // mov eax, [r10+4] (fp_offset)
        self.emit(&[0x3D, 0xB0, 0x00, 0x00, 0x00]); // cmp $176, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        self.emit(&[0x49, 0x8B, 0x4A, 0x10]); // mov rcx, [r10+16]
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x01]); // movss xmm0, [rcx+rax]
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x01]); // movsd xmm0, [rcx+rax]
        }
        self.emit(&[0x41, 0x83, 0x42, 0x04, 0x10]); // add dword [r10+4], 16
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x01]);
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x01]);
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]);
        self.emit(&[0x48, 0x83, 0xC1, 0x08]); // add rcx, 8
        self.emit(&[0x49, 0x89, 0x4A, 0x08]); // mov [r10+8], rcx
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    fn gen_va_arg_global(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();

        // __int128 / unsigned __int128: 强制走溢出路径以确保双 64 位加载正确
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            self.emit_va_arg_int128_from_gp(ap_name, dst, true); // 从 reg_save_area 读取
            self.emit(&[0x41, 0x83, 0x02, 0x10]); // add dword [r10], 16 (__int128 占 16 字节)
            self.jmp_rel32(&done, JumpKind::Jmp);
            self.label(&overflow);
            // 溢出区路径同样读取双 64 位
            self.emit_va_arg_int128_from_gp(ap_name, dst, false); // 从 overflow_arg_area 读取
            self.emit(&[0x49, 0x81, 0xC0, 0x10]); // add rax, 16 (推进 overflow_arg_area)
            self.pop(0);
            self.label(&done);
            return;
        }

        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.emit(&[0x41, 0x8B, 0x02]); // mov eax, [r10] (gp_offset)
        self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // 寄存器保存区路径：rcx = reg_save_area
        self.emit(&[0x49, 0x8B, 0x4A, 0x10]); // mov rcx, [r10+16]
        let gsz = arg_ty.size();
        let gsigned = Self::va_arg_ty_signed(arg_ty);
        match gsz {
            1 => self.emit_va_arg_int_load(1, gsigned, 0x40, &[0x04, 0x01]),
            2 => self.emit_va_arg_int_load(2, gsigned, 0x40, &[0x04, 0x01]),
            4 => self.emit_va_arg_int_load(4, gsigned, 0x40, &[0x04, 0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x04, 0x01]),
        }
        self.emit(&[0x41, 0x83, 0x02, 0x08]); // add dword [r10], 8
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
        match gsz {
            1 => self.emit_va_arg_int_load(1, gsigned, 0x40, &[0x01]),
            2 => self.emit_va_arg_int_load(2, gsigned, 0x40, &[0x01]),
            4 => self.emit_va_arg_int_load(4, gsigned, 0x40, &[0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x01]),
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
        self.emit(&[0x48, 0x83, 0xC1]); // add rcx, imm8
        self.emit(&step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x4A, 0x08]); // mov [r10+8], rcx
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn gen_va_arg_long_double(&mut self, ap_name: &str, _dst: u8) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_long_double_global(ap_name);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        // r8 = overflow_arg_area (va_list + 8)
        self.emit(&[0x4D, 0x8B, 0x85]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        // r9 = (r8 + 15) & ~15 (16 字节对齐)
        self.emit(&[0x4D, 0x8D, 0x48, 0x0F]); // lea r9, [r8+15] (REX.WB: B=1 for r8)
        self.emit(&[0x49, 0x81, 0xE1]); // and r9d, imm32
        self.emit(&(!15u32).to_le_bytes());
        // movsd [r9], xmm0: 读取 8 字节 double 到 xmm0 (dcc 内部 long double = double)
        // 循环体后续 movapd xmm0→xmm1 保存参数, movsd [rsp]→xmm0 加载 sum,
        // addsd xmm1+xmm0 = arg+sum.
        // REX.B + F2 0F 10 /r: ModRM=00_000_100(SIB) SIB=00_100_001(idx=none,base=r9)
        self.emit(&[0xF2, 0x41, 0x0F, 0x10, 0x04, 0x21]); // movsd (%r9), %xmm0
                                                          // r9 += 16 (推进到下一个 16 字节对齐位置)
        self.emit(&[0x49, 0x83, 0xC1, 0x10]); // add r9, 16
                                              // 写回 overflow_arg_area: [r13+ap_off+8] = r9
        self.emit(&[0x4D, 0x89, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        // 结果在 xmm0 (dcc 内部 long double = double 约定)
    }
    fn gen_va_arg_long_double_global(&mut self, ap_name: &str) {
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
                                            // rcx = overflow_arg_area ([r10+8]): REX.W+B (0x49) 使 rm=010→r10
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
                                              // r9 = (rcx + 15) & ~15 (16 字节对齐)
        self.emit(&[0x48, 0x8D, 0x49, 0x0F]); // lea rcx, [rcx+15]
        self.emit(&[0x48, 0x83, 0xE1, 0xF0]); // and rcx, ~15
                                              // movsd (%rcx), %xmm0: 读取 8 字节 double (dcc 内部 long double = double)
                                              // F2 0F 10 /r: ModRM = 00_000_001 = 0x01 (reg=xmm0, rm=rcx)
        self.emit(&[0xF2, 0x0F, 0x10, 0x01]); // movsd (%rcx), %xmm0
                                              // rcx += 16 (推进到下一个 16 字节对齐位置)
        self.emit(&[0x48, 0x83, 0xC1, 0x10]); // add rcx, 16
                                              // 写回 [r10+8]: REX.W+B 使 rm=010→r10, reg=001→rcx 源
        self.emit(&[0x49, 0x89, 0x4A, 0x08]); // mov [r10+8], rcx
                                              // 结果在 xmm0 (dcc 内部 long double = double 约定)
    }
    pub(crate) fn gen_va_arg_ptr(&mut self, arg_ty: &Type, dst: u8) {
        if matches!(arg_ty, Type::Float | Type::Double) && self.target.sse {
            self.gen_va_arg_ptr_fp(arg_ty, dst);
            return;
        }
        // Long double: SysV MEMORY 类, 从 overflow_arg_area 读取
        // dcc 内部以 64-bit double 表示 long double (xmm0-double 约定).
        if matches!(arg_ty, Type::LongDouble) {
            self.emit(&[0x49, 0x89, 0xCB]); // mov r11, rcx (回写基址)
            self.emit(&[0x48, 0x8B, 0x49, 0x08]); // mov rcx, [rcx+8] (overflow)
                                                  // rcx = (rcx + 15) & ~15 (16 字节对齐)
            self.emit(&[0x48, 0x8D, 0x49, 0x0F]); // lea rcx, [rcx+15]
            self.emit(&[0x48, 0x83, 0xE1, 0xF0]); // and rcx, ~15
                                                  // movsd (%rcx), %xmm0: 读取 8 字节 double (dcc 内部 long double = double)
            self.emit(&[0xF2, 0x0F, 0x10, 0x01]); // movsd (%rcx), %xmm0
                                                  // rcx += 16
            self.emit(&[0x48, 0x83, 0xC1, 0x10]); // add rcx, 16
                                                  // 写回推进后的 overflow_arg_area 到 [r11+8] (r11 = va_list 地址)。
                                                  // 旧编码 0x4C,0x89,0x4B 实际是 mov [rbx+8],r9 (REX.R=1 无 REX.B,
                                                  // reg→r9, rm→rbx), 写入错误基址导致 stdarg-1 segfault; 正确的
                                                  // mov [r11+8],rcx 需 REX.B (reg=rcx 源, rm=r11 目标)。
            self.emit(&[0x49, 0x89, 0x4B, 0x08]);
            return;
        }
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = gp_offset (rcx + 0)
        self.emit(&[0x8B, 0x01]); // mov eax, (rcx)
        self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + gp_offset: 从 (rcx+16) 加载到 r11
        // REX 0x4C: R=1 使 reg=r11, B=0 使 rm=rcx (rm=001+REX.B 会变 r9)
        self.emit(&[0x4C, 0x8B, 0x59, 0x10]); // mov r11, 16(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        let sz = arg_ty.size();
        let signed = Self::va_arg_ty_signed(arg_ty);
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x41, &[0x04, 0x03]),
            2 => self.emit_va_arg_int_load(2, signed, 0x41, &[0x04, 0x03]),
            4 => self.emit_va_arg_int_load(4, signed, 0x41, &[0x04, 0x03]),
            _ => self.emit_va_arg_int_load(8, signed, 0x41, &[0x04, 0x03]),
        }
        // gp_offset += step
        self.emit(&[0x83, 0x01]); // add (rcx), imm8
        self.emit(&step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        // overflow_arg_area: 从 (rcx+8) 加载
        self.emit(&[0x4C, 0x8B, 0x59, 0x08]); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        match sz {
            1 => self.emit_va_arg_int_load(1, signed, 0x41, &[0x03]),
            2 => self.emit_va_arg_int_load(2, signed, 0x41, &[0x03]),
            4 => self.emit_va_arg_int_load(4, signed, 0x41, &[0x03]),
            _ => self.emit_va_arg_int_load(8, signed, 0x41, &[0x03]),
        }
        self.push(0);
        self.emit(&[0x4C, 0x8B, 0x59, 0x08]); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        self.emit(&[0x49, 0x83, 0xC3]); // add r11, step
        self.emit(&step.to_le_bytes()[..1]);
        self.emit(&[0x4C, 0x89, 0x59, 0x08]); // mov 8(rcx), r11 (REX.R=1: reg=r11)
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    fn gen_va_arg_ptr_fp(&mut self, arg_ty: &Type, dst: u8) {
        let reg_step = 16i32;
        let overflow_step = 8i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = fp_offset (rcx + 4)
        self.emit(&[0x8B, 0x41, 0x04]); // mov eax, 4(rcx)
        self.emit(&[0x3D, 0xB0, 0x00, 0x00, 0x00]); // cmp $176, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + fp_offset
        self.emit(&[0x4C, 0x8B, 0x59, 0x10]); // mov r11, 16(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x41, 0x0F, 0x10, 0x04, 0x03]); // movss (r11,rax), xmm0
        } else {
            self.emit(&[0xF2, 0x41, 0x0F, 0x10, 0x04, 0x03]); // movsd (r11,rax), xmm0
        }
        // fp_offset += reg_step
        self.emit(&[0x83, 0x41, 0x04]); // add 4(rcx), reg_step
        self.emit(&reg_step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        // overflow_arg_area
        self.emit(&[0x4C, 0x8B, 0x59, 0x08]); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x41, 0x0F, 0x10, 0x03]); // movss (r11), xmm0
        } else {
            self.emit(&[0xF2, 0x41, 0x0F, 0x10, 0x03]); // movsd (r11), xmm0
        }
        self.push(0);
        self.emit(&[0x4C, 0x8B, 0x59, 0x08]); // mov r11, 8(rcx) (REX 0x4C=W|R: reg=r11, B=0 使 rm=rcx)
        self.emit(&[0x49, 0x83, 0xC3]); // add r11, overflow_step
        self.emit(&overflow_step.to_le_bytes()[..1]);
        self.emit(&[0x4C, 0x89, 0x59, 0x08]); // mov 8(rcx), r11 (REX.R=1: reg=r11)
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    pub(crate) fn gen_va_arg_ptr_struct(&mut self, arg_ty: &Type) {
        let sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        let n_gp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Integer))
            .count() as i32;
        let n_fp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Sse | AbiClass::SseUp))
            .count() as i32;

        // 从 va_list 结构体 [rcx] 加载字段:
        //   rax = gp_offset, rdx = fp_offset,
        //   r8 = overflow_arg_area, r9 = reg_save_area
        self.emit(&[0x8B, 0x01]); // mov eax, [rcx]
        self.emit(&[0x8B, 0x51, 0x04]); // mov edx, [rcx+4]
        self.emit(&[0x4C, 0x8B, 0x41, 0x08]); // mov r8, [rcx+8]
        self.emit(&[0x4C, 0x8B, 0x49, 0x10]); // mov r9, [rcx+16]

        // 保存 va_list 指针到栈 (rep movsb 会覆盖 rcx)
        self.push(3); // push r11
        self.emit(&[0x49, 0x89, 0xCB]); // mov r11, rcx (保存 va_list 指针)

        let overflow = self.fresh_label();
        let regs = self.fresh_label();
        let done = self.fresh_label();
        // 对高对齐 struct (>16 byte align) 强制走溢出区路径
        // 因为这通常意味着它在 caller 的 stack layout 中有特殊对齐要求
        if has_memory || align >= 16 {
            self.jmp_rel32(&overflow, JumpKind::Jmp);
        } else {
            self.emit(&[0x3D]); // cmp eax, imm32
            self.emit(&(48 - 8 * n_gp).to_le_bytes());
            self.jmp_rel32(&overflow, JumpKind::Jg);
            if n_fp > 0 {
                self.emit(&[0x81, 0xFA]); // cmp edx, imm32
                self.emit(&(176 - 16 * n_fp).to_le_bytes());
                self.jmp_rel32(&overflow, JumpKind::Jg);
            }
            self.jmp_rel32(&regs, JumpKind::Jmp);
        }

        // ---- 溢出区路径 ----
        self.label(&overflow);
        if align > 8 {
            // r8 = (r8 + align-1) & ~(align-1)
            self.emit(&[0x4D, 0x8D, 0x88]); // lea r9, [r8+disp32]
            self.emit(&(align - 1).to_le_bytes());
            self.emit(&[0x4D, 0x89, 0xC8]); // mov r8, r9
            self.emit(&[0x49, 0x81, 0xE0]); // and r8, imm32
            self.emit(&(!(align - 1)).to_le_bytes());
        }
        self.emit(&[0x4C, 0x89, 0xC6]); // mov rsi, r8
        self.emit(&[0xFC]); // cld
        self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, sz
        self.emit(&(sz as u32).to_le_bytes());
        self.emit(&[0xF3, 0xA4]); // rep movsb
        self.emit(&[0x49, 0x81, 0xC0]); // add r8, step
        self.emit(&step.to_le_bytes());
        // 写回 overflow_arg_area 到 va_list 结构体
        self.emit(&[0x4D, 0x89, 0x43, 0x08]); // mov [r11+8], r8
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 寄存器保存区路径 ----
        self.label(&regs);
        for (c, cls) in classes.iter().enumerate() {
            let off8 = (c as i32) * 8;
            match cls {
                AbiClass::Integer => {
                    self.emit(&[0x4C, 0x8B, 0x0C, 0x01]); // mov r9, [r9+rax]
                    if off8 == 0 {
                        self.emit(&[0x4C, 0x89, 0x0F]); // mov [rdi], r9
                    } else if off8 < 128 {
                        self.emit(&[0x4C, 0x89, 0x4F]); // mov [rdi+disp8], r9
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x4C, 0x89, 0x8F]); // mov [rdi+disp32], r9
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
                },
                AbiClass::Sse | AbiClass::SseUp => {
                    self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x11]); // movsd xmm0, [r9+rdx]
                    if off8 == 0 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x07]); // movq [rdi], xmm0
                    } else if off8 < 128 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x47]); // movq [rdi+disp8], xmm0
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x87]); // movq [rdi+disp32], xmm0
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC2, 0x10]); // add rdx, 16
                },
                _ => {},
            }
        }
        // 回写推进后的偏移到 va_list 结构体
        self.emit(&[0x41, 0x89, 0x03]); // mov [r11], eax
        self.emit(&[0x41, 0x89, 0x53, 0x04]); // mov [r11+4], edx

        // 恢复 va_list 指针
        self.label(&done);
        self.pop(3); // pop r11
    }
    pub(crate) fn gen_va_arg_struct(&mut self, ap_name: &str, arg_ty: &Type) {
        // 全局 va_list: 直接 rip 相对访问 (r10 基址)
        if self.is_global_not_local(ap_name) {
            self.gen_va_arg_struct_global(ap_name, arg_ty);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        let sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        let n_gp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Integer))
            .count() as i32;
        let n_fp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Sse | AbiClass::SseUp))
            .count() as i32;

        // 加载 va_list 字段: rax=gp_offset, rdx=fp_offset,
        // rcx=reg_save_area, r8=overflow_arg_area
        self.emit(&[0x41, 0x8B, 0x85]);
        self.emit(&(ap_off as i32).to_le_bytes());
        self.emit(&[0x41, 0x8B, 0x95]);
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 16) as i32).to_le_bytes());
        self.emit(&[0x4D, 0x8B, 0x85]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());

        let overflow = self.fresh_label();
        let regs = self.fresh_label();
        let done = self.fresh_label();
        if has_memory {
            self.jmp_rel32(&overflow, JumpKind::Jmp);
        } else {
            // gp_offset > 48 - 8*ngp 或 fp_offset > 176 - 16*nfp → 溢出区
            self.emit(&[0x3D]); // cmp eax, imm32
            self.emit(&(48 - 8 * n_gp).to_le_bytes());
            self.jmp_rel32(&overflow, JumpKind::Jg);
            if n_fp > 0 {
                self.emit(&[0x81, 0xFA]); // cmp edx, imm32
                self.emit(&(176 - 16 * n_fp).to_le_bytes());
                self.jmp_rel32(&overflow, JumpKind::Jg);
            }
            self.jmp_rel32(&regs, JumpKind::Jmp);
        }

        // ---- 溢出区路径: 对齐 + rep movsb + 推进 ----
        // SysV ABI: caller (emit_sysv_call) force_mem 的参数按其声明对齐放置.
        // callee 必须在 overflow_arg_area 上做相同对齐, 否则中间有 padding
        // 时 (如 align=16 的 agg 前面有 force_mem int) 会错位.
        self.label(&overflow);
        if align > 8 {
            // r8 = (r8 + align - 1) & ~(align - 1)  — 向上对齐到 agg align
            self.emit(&[0x49, 0x81, 0xC0]);
            self.emit(&(align - 1).to_le_bytes());
            let mask: i32 = !(align - 1);
            self.emit(&[0x49, 0x81, 0xE0]);
            self.emit(&mask.to_le_bytes());
        }
        self.emit(&[0x4C, 0x89, 0xC6]); // mov rsi, r8
        self.emit(&[0xFC]); // cld
        self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, sz
        self.emit(&(sz as u32).to_le_bytes());
        self.emit(&[0xF3, 0xA4]); // rep movsb
                                  // 推进 overflow_arg_area: 当前对齐位置 + step, 再对齐到下一步
                                  // 这样 callee 能跳过 caller 可能插入的 padding
        self.emit(&[0x49, 0x81, 0xC0]); // add r8, step
        self.emit(&step.to_le_bytes());
        self.emit(&[0x4D, 0x89, 0x85]); // mov [r13+disp32], r8
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 寄存器保存区路径：逐 eightbyte 搬运，每次读取后推进基址 ----
        self.label(&regs);

        // GCC 调用约定保证 variadic argument buffers (rdi) 至少有 16 字节对齐
        // 对于 >16 字节对齐的结构体（align >= 32），caller 应已正确对齐目标缓冲区
        // 如需显式对齐可添加：rdi = (rdi + align - 1) & ~(align - 1)
        // 此处简化假设 caller 已正确处理

        for (c, cls) in classes.iter().enumerate() {
            let off8 = (c as i32) * 8;
            match cls {
                AbiClass::Integer => {
                    // mov r9, [rcx+rax] → 存 [rdi+8c] → gp_offset += 8
                    self.emit(&[0x4C, 0x8B, 0x0C, 0x01]); // mov r9, [rcx+rax*1]
                    if off8 == 0 {
                        self.emit(&[0x4C, 0x89, 0x0F]); // mov [rdi], r9
                    } else if off8 < 128 {
                        self.emit(&[0x4C, 0x89, 0x4F]); // mov [rdi+disp8], r9
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x4C, 0x89, 0x8F]); // mov [rdi+disp32], r9
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
                },
                AbiClass::Sse | AbiClass::SseUp => {
                    // movsd xmm0, [rcx+rdx] → movq [rdi+8c] → fp_offset += 16
                    self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x11]); // movsd xmm0, [rcx+rdx*1]
                    if off8 == 0 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x07]); // movq [rdi], xmm0
                    } else if off8 < 128 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x47]); // movq [rdi+disp8], xmm0
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x87]); // movq [rdi+disp32], xmm0
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC2, 0x10]); // add rdx, 16
                },
                _ => {},
            }
        }
        // 回写推进后的偏移 (rax/rdx 已累加到最终值)
        self.emit(&[0x41, 0x89, 0x85]); // mov [r13+ap_off], eax
        self.emit(&(ap_off as i32).to_le_bytes());
        self.emit(&[0x41, 0x89, 0x95]); // mov [r13+ap_off+4], edx
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        self.label(&done);
    }
    fn gen_va_arg_struct_global(&mut self, ap_name: &str, arg_ty: &Type) {
        let sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        let n_gp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Integer))
            .count() as i32;
        let n_fp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Sse | AbiClass::SseUp))
            .count() as i32;

        // 加载 va_list 字段: rax=gp_offset, rdx=fp_offset,
        // rcx=reg_save_area, r8=overflow_arg_area, r10=&va_list
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.emit(&[0x41, 0x8B, 0x02]); // mov eax, [r10]
        self.emit(&[0x41, 0x8B, 0x52, 0x04]); // mov edx, [r10+4]
        self.emit(&[0x49, 0x8B, 0x4A, 0x10]); // mov rcx, [r10+16]
        self.emit(&[0x4D, 0x8B, 0x42, 0x08]); // mov r8, [r10+8]

        let overflow = self.fresh_label();
        let regs = self.fresh_label();
        let done = self.fresh_label();
        if has_memory {
            self.jmp_rel32(&overflow, JumpKind::Jmp);
        } else {
            self.emit(&[0x3D]); // cmp eax, imm32
            self.emit(&(48 - 8 * n_gp).to_le_bytes());
            self.jmp_rel32(&overflow, JumpKind::Jg);
            if n_fp > 0 {
                self.emit(&[0x81, 0xFA]); // cmp edx, imm32
                self.emit(&(176 - 16 * n_fp).to_le_bytes());
                self.jmp_rel32(&overflow, JumpKind::Jg);
            }
            self.jmp_rel32(&regs, JumpKind::Jmp);
        }

        // ---- 溢出区路径 ----
        self.label(&overflow);
        if align > 8 {
            self.emit(&[0x4D, 0x8D, 0x88]); // lea r9, [r8+disp32]
            self.emit(&(align - 1).to_le_bytes());
            self.emit(&[0x4D, 0x89, 0xC8]); // mov r8, r9
            self.emit(&[0x49, 0x81, 0xE0]); // and r8, imm32
            self.emit(&(!(align - 1)).to_le_bytes());
        }
        self.emit(&[0x4C, 0x89, 0xC6]); // mov rsi, r8
        self.emit(&[0xFC]); // cld
        self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, sz
        self.emit(&(sz as u32).to_le_bytes());
        self.emit(&[0xF3, 0xA4]); // rep movsb
        self.emit(&[0x49, 0x81, 0xC0]); // add r8, step
        self.emit(&step.to_le_bytes());
        self.emit(&[0x4D, 0x89, 0x42, 0x08]); // mov [r10+8], r8
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 寄存器保存区路径 ----
        self.label(&regs);
        for (c, cls) in classes.iter().enumerate() {
            let off8 = (c as i32) * 8;
            match cls {
                AbiClass::Integer => {
                    self.emit(&[0x4C, 0x8B, 0x0C, 0x01]); // mov r9, [rcx+rax*1]
                    if off8 == 0 {
                        self.emit(&[0x4C, 0x89, 0x0F]); // mov [rdi], r9
                    } else if off8 < 128 {
                        self.emit(&[0x4C, 0x89, 0x4F]);
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x4C, 0x89, 0x8F]);
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
                },
                AbiClass::Sse | AbiClass::SseUp => {
                    self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x11]); // movsd xmm0, [rcx+rdx*1]
                    if off8 == 0 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x07]); // movq [rdi], xmm0
                    } else if off8 < 128 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x47]);
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x87]);
                        self.emit(&off8.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x83, 0xC2, 0x10]); // add rdx, 16
                },
                _ => {},
            }
        }
        // 回写推进后的偏移
        self.emit(&[0x41, 0x89, 0x02]); // mov [r10], eax
        self.emit(&[0x41, 0x89, 0x52, 0x04]); // mov [r10+4], edx
        self.label(&done);
    }
    pub(crate) fn resolve_va_list_addr(&mut self, ap: &Expr) -> VaListAddress {
        let ap_is_ptr = matches!(ap.ty(), Type::Ptr(_));
        if let Expr::Ident { name, .. } = ap {
            if ap_is_ptr {
                // va_list 作为函数参数时退化为指针，加载指针值到 rcx
                self.gen_expr(ap, 0);
                self.mov(1, 0); // rcx = rax
                VaListAddress::InReg
            } else {
                VaListAddress::Named(name.name.clone())
            }
        } else if let Expr::Deref { ref e, .. } = ap {
            // *ptr: 加载指针值到 rcx
            self.gen_expr(e, 0);
            self.mov(1, 0); // rcx = rax
            VaListAddress::InReg
        } else if ap_is_ptr {
            // 指针类型表达式 (如数组元素): 加载指针值到 rcx
            self.gen_expr(ap, 0);
            self.mov(1, 0);
            VaListAddress::InReg
        } else if self.gen_lvalue_addr(ap, 1) {
            // lvalue (成员/下标): gen_lvalue_addr 已将地址放入 rcx
            VaListAddress::InReg
        } else {
            // 无法解析: 返回零地址
            self.mov_imm(1, 0);
            VaListAddress::InReg
        }
    }
    fn va_arg_ty_signed(ty: &Type) -> bool {
        !matches!(
            ty,
            Type::Bool
                | Type::UChar
                | Type::UShort
                | Type::UInt
                | Type::ULong
                | Type::ULongLong
                | Type::UInt128
        )
    }
    fn va_list_slot_ty() -> Type {
        Type::Array(Box::new(Type::ULong.into()), ArraySize::Const(3))
    }
}
