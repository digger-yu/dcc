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

/// `va_list` 结构体四字段的**落点**（寻址基）。
///
/// 与 `VaListAddress`（回答“寄存器还是内存里的指针”）不同轴：本枚举回答的是
/// “这个 `va_list` 对象本身住在哪种内存里”，因而它的 `gp_offset` /
/// `fp_offset` / `overflow_arg_area` / `reg_save_area` 该怎么读写。三种落点必须
/// 共用同一套 ABI 判定，否则会出现“一侧写真全局内存、另一侧读函数私有
/// 懒分配栈槽”这类落点不一致。
#[derive(Clone, Copy)]
enum VaListLoc {
    /// 命名局部 `va_list`：结构体在 `[r13 + 字段偏移]`（字段基址 = 给定槽偏移）。
    Slot(i32),
    /// 全局 `va_list`：调用点先 `lea r10, [rip+name]`，字段在 `[r10 + ...]`。
    R10,
    /// 已退化为指针的 `va_list`：`rcx` 持指针，字段在 `[rcx + ...]`。
    Rcx,
}

impl Codegen {
    /// va_arg 的结果在 dcc 值模型里是否**以缓冲区地址交付**（而非寄存器值）。
    ///
    /// 这条轴与“字节从哪个区读”（`abi_classes` / `classes_force_mem`）正交：
    /// 前者回答交付约定，后者回答字节来源。旧代码在分发点直接写
    /// `matches!(Struct|Union) && size > 8`，把两轴混成“类型的书写形态”，于是：
    /// - 裸 16 字节向量（`__m128d` = 分类 [Sse, SseUp]）被判为标量，从单
    ///   个 xmm 寄存器交付，而它的值需两个 8 字节块；
    /// - 含多 eightbyte 成员的聚合经 va_list 取参时走不到按分类装载路径。
    ///
    /// 例外是 dcc 特有的**值模型**约定（不是 ABI）：整个对象能被一个寄存器装
    /// 下的纯整数聚合（size <= 8 且分类恰为 `[Integer]`，如
    /// `struct { int a; int b; }` —— 两个 int 落在**同一个** eightbyte）可以按
    /// 寄存器值交付；强行改成缓冲区地址会让通用路径把值当指针解引用。
    pub(crate) fn va_arg_result_in_memory(ty: &Type) -> bool {
        if Self::type_has_vla(ty) {
            return true;
        }
        if !matches!(ty, Type::Struct(..) | Type::Union(..) | Type::Array(..) | Type::Vector(..)) {
            return false;
        }
        !(ty.size() <= 8 && matches!(Self::abi_classes(ty).as_slice(), [AbiClass::Integer]))
    }

    /// eightbyte 分类表 → 本对象实际占用的 `(GP 寄存器数, XMM 寄存器数)`。
    ///
    /// 这两个数是 SysV「寄存器保存区够不够用」判据（`gp_offset > 48 - 8*n_gp`
    /// 与 `fp_offset > 176 - 16*n_xmm`）与游标推进步长的输入。GP 侧一个 eightbyte
    /// 就是一个 8 字节槽（`__int128` = 2 个 eightbyte = 16 字节），数 eightbyte
    /// 与数寄存器一致；**XMM 侧不一致**：`SseUp` 是前一个 `Sse` 所在 16 字节槽的
    /// 高半延续，不另开槽。
    ///
    /// GCC 实测（`-O0` 看 `function_arg_advance` 生成的槽位）：
    /// - `__m128d` = `[Sse, SseUp]` → 只用 `xmm0`（一个槽）；
    /// - `struct {double a; double b;}` = `[Sse, Sse]` → 用 `xmm0` + `xmm1`（两个槽）。
    ///
    /// 旧实现按 SSE 家族 eightbyte 个数算，`[Sse, SseUp]` 被当成 2 个寄存器，上限
    /// 从 `176-16=160` 缩到 `176-32=144`，于是第 8 个向量参数（`fp_offset=160`）
    /// 被误判为溢出、改从 `overflow_arg_area` 取，而 caller 把它留在了 `xmm7` 槽里。
    fn va_arg_reg_counts(classes: &[AbiClass]) -> (i32, i32) {
        let n_gp = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Integer))
            .count() as i32;
        let n_xmm = classes
            .iter()
            .filter(|c| matches!(c, AbiClass::Sse))
            .count() as i32;
        (n_gp, n_xmm)
    }

    /// 第 `c` 个 eightbyte 在 FP 保存区里相对 `fp_offset` 起点的**字节偏移**。
    ///
    /// 分组规则同 [`Self::va_arg_reg_counts`]：`Sse` 开一个新的 16 字节槽取低半，
    /// `SseUp` 续用上一个 `Sse` 的槽取高半（+8）。旧实现用“SSE 家族个数的奇
    /// 偶”近似：对 `[Sse, SseUp]` 恰好对，对 `[Sse, Sse]` 则让第二块从**同一槽
    /// 的 +8** 读（应从下一个槽的 +0），游标也只推 16（应推 32），与 GCC 不同源。
    fn va_arg_sse_disp(classes: &[AbiClass], c: usize) -> i32 {
        let n_sse_before = classes[..c].iter().filter(|k| **k == AbiClass::Sse).count() as i32;
        match classes[c] {
            // 孤立的 SseUp（分类器不会产出）降级到当前槽高半，避免算出负偏移。
            AbiClass::SseUp => (n_sse_before - 1).max(0) * 16 + 8,
            _ => n_sse_before * 16,
        }
    }

    /// `movsd xmm0, disp(%rcx,%rdx)` —— 从 FP 保存区第 `disp` 字节读低 8 字节。
    ///
    /// SIB `0x11` = scale 1 / index=rdx / base=rcx；mod 按偏移宽度选 00/01/10。
    fn va_sse_load_save_disp(disp: i32) -> Vec<u8> {
        if disp == 0 {
            vec![0xF2, 0x0F, 0x10, 0x04, 0x11]
        } else if (-128..=127).contains(&disp) {
            vec![0xF2, 0x0F, 0x10, 0x44, 0x11, disp as u8]
        } else {
            let mut v = vec![0xF2, 0x0F, 0x10, 0x84, 0x11];
            v.extend_from_slice(&disp.to_le_bytes());
            v
        }
    }

    /// FP 槽的读出指令按**对象宽度**归一化：16→movups、4→movss、其余→movsd。
    ///
    /// 旧实现对 `Float` 写 `movss`、其余一律 `movsd`，于是 16 字节的
    /// `__m128d` 只能取到低 8 字节。`base_index` 选基址形态：保存区需加
    /// `fp_offset`（`(%rcx,%rax)`），overflow 游标已在指针里（`(%rcx)`）。
    fn va_sse_load(sz: usize, base_index: bool) -> Vec<u8> {
        let mut v: Vec<u8> = match sz {
            16 => vec![],
            4 => vec![0xF3],
            _ => vec![0xF2],
        };
        v.extend_from_slice(&[0x0F, 0x10]);
        v.extend_from_slice(if base_index { &[0x04, 0x01] } else { &[0x01] });
        v
    }

    /// 寄存器保存区路径的公共装载体：按 ABI 分类逐 eightbyte 把值搬进 `rdi`
    /// 指向的对象。三个调用点（named / ptr / global `va_list`）此前各持一份
    /// 逐字副本，任何单点修复都会另外两处静默失效，现收敛到此处。
    ///
    /// 寄存器约定：`rcx` = reg_save_area、`rax` = gp_offset、`rdx` = fp_offset。
    /// 本函数只推进 `rax`/`rdx` 不回写内存——回写位置随 `va_list` 基址而异
    /// （`[r13+ap_off]` / `[r11]` / `[r10]`），留给调用点完成。
    fn emit_va_arg_reg_save_load(&mut self, classes: &[AbiClass]) {
        for (c, cls) in classes.iter().enumerate() {
            let off8 = (c as i32) * 8;
            match cls {
                AbiClass::Integer => {
                    // 一个 GP eightbyte = 一个 8 字节槽, 逐块读 + 推进。
                    self.emit(&[0x4C, 0x8B, 0x0C, 0x01]); // mov r9, (%rcx,%rax)
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
                    // 槽内位置由分组规则算出(见 `va_arg_sse_disp`), 不能按奇偶近似。
                    let bytes = Self::va_sse_load_save_disp(Self::va_arg_sse_disp(classes, c));
                    self.emit(&bytes); // movsd xmm0, disp(%rcx,%rdx)
                    if off8 == 0 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x07]); // movq [rdi], xmm0
                    } else if off8 < 128 {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x47]); // movq [rdi+disp8], xmm0
                        self.emit(&[off8 as u8]);
                    } else {
                        self.emit(&[0x66, 0x0F, 0xD6, 0x87]); // movq [rdi+disp32], xmm0
                        self.emit(&off8.to_le_bytes());
                    }
                },
                _ => {},
            }
        }
        // FP 游标按**实际占用的寄存器个数**一次性推进 (每寄存器 16 字节),
        // 不再在循环里按奇偶逐次 `add rdx,16`。
        let n_xmm = Self::va_arg_reg_counts(classes).1;
        if n_xmm > 0 {
            self.emit(&[0x48, 0x81, 0xC2]); // add rdx, imm32
            self.emit(&(n_xmm * 16).to_le_bytes());
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
        // 路由到按 16 字节 FP 槽取数的单块路径。判据用 ABI 分类而非类型书写
        // 形态（`float`/`double` 与单 eightbyte SSE 对象同走此路）。
        //
        // 限定 `size <= 8` 是必要的：dcc 内部的**向量值模型是 GP-block**，
        // `va_arg(ap, __m128d)` 的结果需以两个 8 字节 GP 块交付，而本函数把值
        // 送进单个 xmm 寄存器。“字节来源是 FP 槽”与“值表示是 GP block”是两条
        // 正交轴，合并它们会得到错误结果（实测：路由放开到 16 字节后
        // `sseup_shares_one_slot` 仍错，两个向量都读错）。
        // 16 字节 SSE 对象不走本函数：由 `gen_expr_va_arg` 用
        // `va_arg_result_in_memory` 判为“以缓冲区地址交付”，改走
        // `gen_va_arg_struct` / `gen_va_arg_ptr_struct` 的按分类逐块装载路径
        // （消费侧则 destination-direct 写入目标，见 `gen_aggregate_assign`）。
        if self.target.sse
            && arg_ty.size() <= 8
            && Self::classes_all_sse_family(&Self::abi_classes(arg_ty))
        {
            self.gen_va_arg_fp(ap_name, arg_ty, dst);
            return;
        }
        // Long double: SysV MEMORY 类，始终在栈上传递 (overflow_arg_area)
        // 16 字节，不经过寄存器保存区
        if matches!(arg_ty, Type::LongDouble) {
            self.gen_va_arg_long_double(ap_name, dst);
            return;
        }
        // __int128 / unsigned __int128: 占两个 GP eightbyte 的标量。
        // GCC 实测判据 (`cmpl $39, gp_offset; ja 内存路径`)：高半必须仍落在 GP 保存区
        // 最后一个槽内 (gp_offset + 8 <= 47)。装不下则：
        //   1. **不拆分** —— 整个对象从 overflow_arg_area 取（旧实现的 hybrid 路径
        //      把低半留在寄存器保存区 [40]、高半从溢出区取，与 GCC 不同源）；
        //   2. **gp_offset 不推进**（旧实现连内存路径也 `+= 16`，把游标双推进）；
        //   3. 溢出游标先向上对齐到 16 再读，读完推 16。
        // 实测：`gp_offset == 40` 时读 `va_arg(ap, __int128)` 与 GCC 不同源（rc=3）。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            // 落点判据必须在本分支内部同样成立：此前它直用 `var_offset(ap_name)`
            // + r13 相对寻址，于是全局 `va_list` 读的是本函数私有的懒分配栈槽
            // (未初始化) —— `va_start` 却写真全局内存，两侧落点不一致。
            if self.is_global_not_local(ap_name) {
                self.gen_va_arg_int128_global(ap_name);
                return;
            }
            let ap_off = self.var_offset(ap_name) as i32;
            self.gen_va_arg_int128_at(VaListLoc::Slot(ap_off));
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
        let sz = arg_ty.size();
        let reg_step = 16i32; // reg_save_area 中 XMM 槽为 16 字节对齐
                              // overflow_arg_area 按对象自身向上取整到 8 字节：float/double 走 8，
                              // `__m128` 系走 16。写死 8 会让 16 字节向量的后续参数错位。
        let overflow_step = sz.div_ceil(8).max(1) as i32 * 8;
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
        self.emit(&Self::va_sse_load(sz, true));
        self.emit(&[0x41, 0x83, 0x85]);
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        self.emit(&reg_step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.emit(&Self::va_sse_load(sz, false));
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
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
        let sz = arg_ty.size();
        let overflow_step = sz.div_ceil(8).max(1) as i32 * 8;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.emit(&[0x41, 0x8B, 0x42, 0x04]); // mov eax, [r10+4] (fp_offset)
        self.emit(&[0x3D, 0xB0, 0x00, 0x00, 0x00]); // cmp $176, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        self.emit(&[0x49, 0x8B, 0x4A, 0x10]); // mov rcx, [r10+16]
        self.emit(&Self::va_sse_load(sz, true));
        self.emit(&[0x41, 0x83, 0x42, 0x04, 0x10]); // add dword [r10+4], 16
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
        self.emit(&Self::va_sse_load(sz, false));
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]);
        self.emit(&[0x48, 0x83, 0xC1]);
        self.emit(&overflow_step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x4A, 0x08]); // mov [r10+8], rcx
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }
    /// `__int128` 装载体的单一实现：判定与指令完全一致，只有 va_list 四个
    /// 字段的**寻址**随落点而变。三种落点原先各有一份（named 版还带一条与 GCC
    /// 不同源的 hybrid 拆分路径、全局版的 r10 未设置且溢出分支是死代码、
    /// 指针版根本没有 Int128 分支），收敛到此。
    ///
    /// SysV / GCC 真值（由 `cmpl $39, gp_offset; ja 内存路径` 实测得出）：
    /// - `gp_offset <= 39`（高半仍落在 GP 区最后一个槽 40..47 内）→ 两半都在
    ///   寄存器保存区，`gp_offset += 16`；
    /// - 否则**不拆分**：整个对象从 16 字节对齐后的 `overflow_arg_area` 取，
    ///   `gp_offset` **不推进**，溢出游标 += 16。
    ///
    /// 字段偏移：0 = gp_offset(u32)、8 = overflow_arg_area、16 = reg_save_area。
    /// 基址寄存器在本函数内必须不可变（reg_save_area 存于 rdx，不复写基址）。
    fn gen_va_arg_int128_at(&mut self, loc: VaListLoc) {
        let mem = self.fresh_label();
        let done = self.fresh_label();

        // eax = gp_offset (必须 32 位读: 高 4 字节是 fp_offset, 按 64 位读会把它灌进高位)
        match loc {
            VaListLoc::Slot(off) => {
                self.emit(&[0x41, 0x8B, 0x85]); // mov eax, [r13+disp32]
                self.emit(&off.to_le_bytes());
            },
            VaListLoc::R10 => self.emit(&[0x41, 0x8B, 0x02]), // mov eax, [r10]
            VaListLoc::Rcx => self.emit(&[0x8B, 0x01]),       // mov eax, [rcx]
        }
        self.emit(&[0x83, 0xF8, 0x27]); // cmp $39, %eax
        self.jmp_rel32(&mem, JumpKind::Ja);

        // ---- 寄存器保存区路径 ----
        // rdx = reg_save_area; rax = rdx + gp_offset; gp_offset += 16
        match loc {
            VaListLoc::Slot(off) => {
                self.emit(&[0x49, 0x8B, 0x95]); // mov rdx, [r13+disp32]
                self.emit(&(off + 16).to_le_bytes());
            },
            VaListLoc::R10 => self.emit(&[0x49, 0x8B, 0x52, 0x10]), // mov rdx, [r10+16]
            VaListLoc::Rcx => self.emit(&[0x48, 0x8B, 0x51, 0x10]), // mov rdx, [rcx+16]
        }
        self.emit(&[0x48, 0x8D, 0x04, 0x02]); // lea rax, (%rdx,%rax)
        match loc {
            VaListLoc::Slot(off) => {
                self.emit(&[0x41, 0x83, 0x85]); // add dword [r13+disp32], 16
                self.emit(&off.to_le_bytes());
                self.emit(&[0x10]);
            },
            VaListLoc::R10 => self.emit(&[0x41, 0x83, 0x02, 0x10]), // add dword [r10], 16
            VaListLoc::Rcx => self.emit(&[0x83, 0x01, 0x10]),       // add dword [rcx], 16
        }
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 内存路径: rax = 对齐后的溢出区地址, 游标 += 16 (gp_offset 不动) ----
        self.label(&mem);
        match loc {
            VaListLoc::Slot(off) => {
                self.emit(&[0x49, 0x8B, 0x85]); // mov rax, [r13+disp32]
                self.emit(&(off + 8).to_le_bytes());
            },
            VaListLoc::R10 => self.emit(&[0x49, 0x8B, 0x42, 0x08]), // mov rax, [r10+8]
            VaListLoc::Rcx => self.emit(&[0x48, 0x8B, 0x41, 0x08]), // mov rax, [rcx+8]
        }
        self.emit(&[0x48, 0x83, 0xC0, 0x0F]); // add rax, 15
        self.emit(&[0x48, 0x83, 0xE0, 0xF0]); // and rax, -16
        match loc {
            VaListLoc::Slot(off) => {
                self.emit(&[0x49, 0x89, 0x85]); // mov [r13+disp32], rax
                self.emit(&(off + 8).to_le_bytes());
                self.emit(&[0x49, 0x81, 0x85]); // add qword [r13+disp32], 16
                self.emit(&(off + 8).to_le_bytes());
                self.emit(&16i32.to_le_bytes());
            },
            VaListLoc::R10 => {
                self.emit(&[0x49, 0x89, 0x42, 0x08]); // mov [r10+8], rax
                self.emit(&[0x49, 0x81, 0x42, 0x08]); // add qword [r10+8], 16
                self.emit(&16i32.to_le_bytes());
            },
            VaListLoc::Rcx => {
                self.emit(&[0x48, 0x89, 0x41, 0x08]); // mov [rcx+8], rax
                self.emit(&[0x48, 0x81, 0x41, 0x08]); // add qword [rcx+8], 16
                self.emit(&16i32.to_le_bytes());
            },
        }

        // ---- done: __int128 固定以 rdx:rax 交付 (SysV) ----
        self.label(&done);
        self.emit(&[0x48, 0x8B, 0x50, 0x08]); // mov rdx, 8(%rax)
        self.emit(&[0x48, 0x8B, 0x00]); // mov rax, (%rax)
    }

    /// 全局 `va_list` + `__int128`：基址由 `lea r10, [rip+name]` 固定后交给
    /// 共用实现。
    fn gen_va_arg_int128_global(&mut self, ap_name: &str) {
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
        self.gen_va_arg_int128_at(VaListLoc::R10);
    }
    fn gen_va_arg_global(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let step = (arg_ty.size().div_ceil(8) * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();

        // __int128 / unsigned __int128: 与本地版同一判据, 委派给专用的
        // 全局变体。旧副本有三处硬错：它在 `emit_global_addr(10, ..)` **之前**
        // 就进入，所以 `emit_va_arg_int128_from_gp` 解引用的是未设置的 r10；
        // `label(&overflow)` 无人跳入 (整个溢出分支是死代码，且带一个无配对的
        // `pop(0)`)；而且它无条件 `add dword [r10], 16`，没有“装不下就不推进”判定。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            self.gen_va_arg_int128_global(ap_name);
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
    /// D9: 把 `long double` 变参槽 load 到 st(0)。
    ///
    /// SysV AMD64 下 `long double` 属 **X87 类**：变参实参永远放在 16 字节对齐的
    /// `overflow_arg_area`，内容是调用方 `fstpt` 写出的**真 80-bit 编码**（低 10
    /// 字节有效，`+10..16` 是实现定义 padding）。旧代码发 `movsd (%r9), %xmm0`
    /// 只取低 8 字节当成 f64 位型 —— 实际读到的是那 64 位**尾数**，值完全无关
    /// （实测 `(long long)a` 得 LLONG_MIN）。
    ///
    /// `long double` 现在内部也是**真 80-bit 对象 + st(0) 值通道**
    /// （`float_in_st0` 对 `LongDouble` 恒为真，二元算术走 `gen_float_binop` 的
    /// x87 通路，落槽用 `fstpt`），所以此处**不需要任何转换**：一条 `fld tbyte`
    /// 把槽里的 80-bit 压到 st(0)，就是消费端期望的形式。
    ///
    /// 旧实现发的是 `fld tbyte` → `fstp qword`（硬件舍入）→ `movsd`，那是当时
    /// “低 8 字节 = f64”不变量下的**跨界降级垫片**。不变量被取消后，那个
    /// `fstp qword` 成了纯粹的精度损失点（把刚读到的 64 位有效尾数舍回 53 位），
    /// 还额外占用 rsp scratch。删。
    ///
    /// `base_r9` 选基址寄存器：named 变体用 r9，global / ptr 变体用 rcx。
    fn emit_ld80_load_to_st0(&mut self, base_r9: bool) {
        if base_r9 {
            // REX.B=1 使 rm=001→r9; ModRM = 00_101_001 (fld 是 x87 /5)
            self.emit(&[0x41, 0xDB, 0x29]); // fld tbyte (%r9)
        } else {
            self.emit(&[0xDB, 0x29]); // fld tbyte (%rcx)
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
        // 从 overflow_arg_area 把真 80-bit 对象直接 load 到 st(0)（D9）。
        self.emit_ld80_load_to_st0(true); // fld tbyte (%r9) → st(0)，无损
                                          // r9 += 16 (推进到下一个 16 字节对齐位置)
        self.emit(&[0x49, 0x83, 0xC1, 0x10]); // add r9, 16
                                              // 写回 overflow_arg_area: [r13+ap_off+8] = r9
        self.emit(&[0x4D, 0x89, 0x8D]);
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        // 结果在 st(0)（`long double` 的 rvalue 通道）
    }
    fn gen_va_arg_long_double_global(&mut self, ap_name: &str) {
        self.emit_global_addr(10, ap_name); // lea r10, [rip+gap]
                                            // rcx = overflow_arg_area ([r10+8]): REX.W+B (0x49) 使 rm=010→r10
        self.emit(&[0x49, 0x8B, 0x4A, 0x08]); // mov rcx, [r10+8]
                                              // r9 = (rcx + 15) & ~15 (16 字节对齐)
        self.emit(&[0x48, 0x8D, 0x49, 0x0F]); // lea rcx, [rcx+15]
        self.emit(&[0x48, 0x83, 0xE1, 0xF0]); // and rcx, ~15
                                              // 从 overflow_arg_area load 真 80-bit 到 st(0) (D9)。
        self.emit_ld80_load_to_st0(false); // fld tbyte (%rcx) → st(0)，无损
                                           // rcx += 16 (推进到下一个 16 字节对齐位置)
        self.emit(&[0x48, 0x83, 0xC1, 0x10]); // add rcx, 16
                                              // 写回 [r10+8]: REX.W+B 使 rm=010→r10, reg=001→rcx 源
        self.emit(&[0x49, 0x89, 0x4A, 0x08]); // mov [r10+8], rcx
                                              // 结果在 st(0)（`long double` 的 rvalue 通道）
    }
    pub(crate) fn gen_va_arg_ptr(&mut self, arg_ty: &Type, dst: u8) {
        if matches!(arg_ty, Type::Float | Type::Double) && self.target.sse {
            self.gen_va_arg_ptr_fp(arg_ty, dst);
            return;
        }
        // __int128 / unsigned __int128: 本变体以前根本没有这个分支，于是 16 字节的
        // 标量落回单 8 字节通用路径：只取到（并只推进）低半。与 GCC 不同源（实测
        // 指针型 `va_list` 上 `va_arg(ap, __int128)` 及其后一个参数全错）。现与
        // named / global 两变体共用同一判据，基址是 `rcx`（本函数内不得复写）。
        if matches!(arg_ty, Type::Int128 | Type::UInt128) {
            self.gen_va_arg_int128_at(VaListLoc::Rcx);
            return;
        }
        // Long double: SysV MEMORY 类, 从 overflow_arg_area 读取
        // (值通道是 st(0)，与其他 long double rvalue 一致)
        if matches!(arg_ty, Type::LongDouble) {
            self.emit(&[0x49, 0x89, 0xCB]); // mov r11, rcx (回写基址)
            self.emit(&[0x48, 0x8B, 0x49, 0x08]); // mov rcx, [rcx+8] (overflow)
                                                  // rcx = (rcx + 15) & ~15 (16 字节对齐)
            self.emit(&[0x48, 0x8D, 0x49, 0x0F]); // lea rcx, [rcx+15]
            self.emit(&[0x48, 0x83, 0xE1, 0xF0]); // and rcx, ~15
                                                  // 从 overflow_arg_area load 真 80-bit 到 st(0) (D9)。
            self.emit_ld80_load_to_st0(false); // fld tbyte (%rcx) → st(0)，无损
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
        let (n_gp, n_fp) = Self::va_arg_reg_counts(&classes);

        // 从 va_list 结构体 [rcx] 加载字段:
        //   rax = gp_offset, rdx = fp_offset, r8 = overflow_arg_area
        self.emit(&[0x8B, 0x01]); // mov eax, [rcx]
        self.emit(&[0x8B, 0x51, 0x04]); // mov edx, [rcx+4]
        self.emit(&[0x4C, 0x8B, 0x41, 0x08]); // mov r8, [rcx+8]

        // 先把 va_list 指针留在 r11 (溢出区的 `rep movsb` 会毁 rcx, 且下面的
        // 偏移回写要靠 r11), 再把 reg_save_area 装进 rcx —— 装载体
        // `emit_va_arg_reg_save_load` 与 named / global 两个变体共用同一份,
        // 而那份固定以 `(%rcx,%rax)` / `(%rcx,%rdx)` 解引用。此前这里把
        // reg_save_area 装进 r9 却让装载体用 rcx 当基址 (编码 `4C 8B 0C 01`
        // 缺 REX.B)，于是读的是 **va_list 结构体自身 + gp_offset** 的栈垃圾
        // (D1：指针型 va_list 参数读聚合全错)。
        self.push(3); // push r11
        self.emit(&[0x49, 0x89, 0xCB]); // mov r11, rcx (保存 va_list 指针)
        self.emit(&[0x48, 0x8B, 0x49, 0x10]); // mov rcx, [rcx+16] = reg_save_area

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
        self.emit_va_arg_reg_save_load(&classes);
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
        let has_vla = Self::type_has_vla(arg_ty);
        let mut sz = arg_ty.size();
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32; // 计算编译期步长，非VLA使用

        // 处理 VLA: 使用运行时尺寸
        if has_vla {
            self.gen_sizeof_runtime(arg_ty, 1); // 运行时尺寸存入寄存器1 (rcx)
            sz = 0; // 临时占位，后续从 rcx 获取
        }
        
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        let (n_gp, n_fp) = Self::va_arg_reg_counts(&classes);

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
        // 处理 VLA: 运行时尺寸从寄存器1 (rcx) 获取，否则使用编译期尺寸
        if has_vla {
            // 计算 step = (rcx +7) & ~7 (按8字节对齐，与非VLA一致)
            self.emit(&[0x48, 0x8D, 0x41, 0x07]); // lea rax, [rcx+7] (尺寸+7，用于8字节对齐)
            self.emit(&[0x48, 0x83, 0xE0, 0xF8]); // and rax, -8 (对齐到8字节边界)
            // step 是新的对齐后尺寸
            self.emit(&[0x49, 0x89, 0xC3]); // mov r11, rax (暂存step)
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax (rep movsb的计数是对齐后的运行时尺寸)
        } else {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, sz
            self.emit(&(sz as u32).to_le_bytes());
            self.emit(&[0x49, 0x89, 0xC3]); // mov r11, step (暂存已计算的step)
        }
        self.emit(&[0xF3, 0xA4]); // rep movsb
                                    // 推进 overflow_arg_area: 当前对齐位置 + step, 再对齐到下一步
                                    // 这样 callee 能跳过 caller 可能插入的 padding
        // 计算 step: (size +7) & ~7，VLA 使用运行时计算后的尺寸
        self.emit(&[0x49, 0x01, 0xD0]); // add r8, r11 (统一使用暂存的r11，不管是VLA还是非VLA)
        self.emit(&[0x4D, 0x89, 0x85]); // mov [r13+disp32], r8
        self.emit(&((ap_off + 8) as i32).to_le_bytes());
        self.label(&done);

        // ---- 寄存器保存区路径：逐 eightbyte 搬运，每次读取后推进基址 ----
        self.label(&regs);

        // GCC 调用约定保证 variadic argument buffers (rdi) 至少有 16 字节对齐
        // 对于 >16 字节对齐的结构体（align >= 32），caller 应已正确对齐目标缓冲区
        // 如需显式对齐可添加：rdi = (rdi + align - 1) & ~(align - 1)
        // 此处简化假设 caller 已正确处理

        self.emit_va_arg_reg_save_load(&classes);
        // 回写推进后的偏移 (rax/rdx 已累加到最终值)
        self.emit(&[0x41, 0x89, 0x85]); // mov [r13+ap_off], eax
        self.emit(&(ap_off as i32).to_le_bytes());
        self.emit(&[0x41, 0x89, 0x95]); // mov [r13+ap_off+4], edx
        self.emit(&((ap_off + 4) as i32).to_le_bytes());
        self.label(&done);
    }
    fn gen_va_arg_struct_global(&mut self, ap_name: &str, arg_ty: &Type) {
        let has_vla = Self::type_has_vla(arg_ty);
        let mut sz = arg_ty.size();
        if has_vla {
            self.gen_sizeof_runtime(arg_ty, 1); // 运行时尺寸存入 rax
            sz = 0; // 临时占位，后续从 rax 获取
        }
        let align = (arg_ty.align() as i32).max(8);
        let step = ((sz + 7) & !7) as i32;
        let classes = Self::abi_classes(arg_ty);
        let has_memory = classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        });
        let (n_gp, n_fp) = Self::va_arg_reg_counts(&classes);

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
        
        // 处理VLA，与本地函数逻辑一致，尺寸在寄存器1 (rcx)
        if has_vla {
            // 计算step = (rcx +7) & ~7 (按8字节对齐，与非VLA一致)
            self.emit(&[0x48, 0x8D, 0x41, 0x07]); // lea rax, [rcx+7] (尺寸+7，用于8字节对齐)
            self.emit(&[0x48, 0x83, 0xE0, 0xF8]); // and rax, -8 (对齐到8字节边界)
            self.emit(&[0x49, 0x89, 0xC3]); // mov r11, rax (暂存step)
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax (rep movsb计数为对齐后的运行时尺寸)
        } else {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, sz
            self.emit(&(sz as u32).to_le_bytes());
            self.emit(&[0x49, 0x89, 0xC3]); // mov r11, step (暂存已计算step)
        }
        
        self.emit(&[0xF3, 0xA4]); // rep movsb
        // 推进overflow，统一使用暂存的r11
        self.emit(&[0x49, 0x01, 0xD0]); // add r8, r11
        self.emit(&[0x4D, 0x89, 0x42, 0x08]); // mov [r10+8], r8
        self.jmp_rel32(&done, JumpKind::Jmp);

        // ---- 寄存器保存区路径 ----
        self.label(&regs);
        self.emit_va_arg_reg_save_load(&classes);
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
