//! abi/tyclass.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v2df() -> Type {
        Type::Vector(Box::new(QualType::new(Type::Double)), 2)
    }

    /// 16 字节裸向量的两个 eightbyte 必须是 `[Sse, SseUp]`（与 GCC
    /// `classify_field` 一致）：第二个 eightbyte 是同一寄存器的延续，不是独立
    /// 槽。若退化为 `[Sse, Sse]`，按“消耗几个 XMM 寄存器”推算的
    /// `fp_offset` 上限与推进步长都会翻倍，寄存器保存区将提前误判为溢出。
    #[test]
    fn v2df_classifies_as_sse_sseup() {
        assert_eq!(Codegen::abi_classes(&v2df()), vec![AbiClass::Sse, AbiClass::SseUp]);
        assert!(Codegen::is_sseup_128_aggregate(&v2df()));
    }

    /// 32 字节向量（`vector_size(32)`，自然对齐 32）必须是 `Memory`：GCC 实测
    /// (`gcc -O0 -s` 对 `probe(int, ...)` 传 `v4df`) 在溢出区以
    /// `addq $31; andq $-32` 对齐后整体入栈，不进 XMM 寄存器组。
    /// 旧断言 `[Sse, SseUp, SseUp, SseUp]` 把本应整体入栈的对象当成“一个 XMM
    /// 寄存器 + 三个延续”，调用方只存低 16 字节、`va_arg` 也只读一个槽，
    /// 高半永久丢失。与 `Type::Vector` 的自然对齐修正同属一个根因。
    #[test]
    fn v4df_over_16byte_alignment_classifies_as_memory() {
        let v4df = Type::Vector(Box::new(QualType::new(Type::Double)), 4);
        assert_eq!(v4df.align(), 32);
        assert_eq!(Codegen::abi_classes(&v4df), vec![AbiClass::Memory]);
    }

    /// `double[2]`（与 `struct { double a; double b; }` 同形）与 `__m128d` 同为
    /// 16 字节，但分类不同：前者 `[Sse, Sse]` 占**两个** XMM 寄存器（两个
    /// 16 字节槽），后者 `[Sse, SseUp]` 只占一个。这条差异是
    /// `va_arg_sse_disp` / `va_arg_reg_counts` 的判据来源。
    #[test]
    fn two_independent_doubles_occupy_two_xmm_regs() {
        let d2 = Type::Array(Box::new(QualType::new(Type::Double)), ArraySize::Const(2));
        assert_eq!(Codegen::abi_classes(&d2), vec![AbiClass::Sse, AbiClass::Sse]);
        assert!(!Codegen::is_sseup_128_aggregate(&d2));
    }
}
impl Codegen {
    pub(crate) fn abi_classes(ty: &Type) -> Vec<AbiClass> {
        let size = ty.size();
        if size == 0 {
            return Vec::new();
        }
        // SysV AMD64 ABI: 超过 64 字节的聚合类型无法放入寄存器集
        // (6 GP + 8 XMM 共 64 字节), 直接强制 Memory 类.
        // long double (x86 80-bit) 标量也强制 Memory.
        //
        // 除 `_Complex long double` 之外的所有 _Complex 在旧实现里统一按内存
        // (sret/栈槽) 交付。但 SysV ABI 3.2.3 的真值是：`_Complex float` 是
        // **单个** 8 字节 SSE eightbyte (实部/虚部各 4 字节打包其中)，`_Complex
        // double` 是 `[Sse, Sse]` 两个独立 SSE eightbyte。gcc -O0 实测 (与 dcc
        // 交叉验证的唯一真值)：`f(_Complex float z)` 装 `movq %rax,%xmm0`、
        // `f(_Complex double z)` 装 `xmm0`+`xmm1`，变参则占 1~2 个 16 字节 FP
        // 保存区槽。一刀切成 Memory 使两侧与 gcc 都不互操作 (变参读回垃圾位型)。
        //
        // 整数 _Complex 暂维持既有 Memory 约定：dcc 的整数 complex rvalue 与形参
        // 机制整体按“内存对 + rax/rdx”交付，与本轴的寄存器交付不同轴（见
        // `leaf_abi_class` 的 complex-6 注记），放开需独立改造。
        let is_complex_ld = matches!(
            ty,
            Type::Complex(c) if matches!(&c.base, Type::LongDouble)
        );
        let is_complex_sse = matches!(
            ty,
            Type::Complex(c) if matches!(&c.base, Type::Float | Type::Double)
        );
        // 对齐 **大于 16 字节** 的类型整体走内存: SysV ABI 3.2.3p2 的
        // "如果对齐超过 16 字节, 则归类为 MEMORY" 与 GCC `classify_argument`
        // 里的 `if (align > 128) return CLASS_MEMORY;` (GCC 4.6 起的 ABI 变更)。
        // 产物: `__attribute__((vector_size(32)))` 之类 AVX 向量、含这类向量
        // 成员的聚合、`__attribute__((aligned(32)))` 的结构。把它们分类成
        // `[Sse, SseUp...]` 会让调用方只搬 16 字节、`va_arg` 只消耗一个 XMM 槽,
        // 高 16 字节直接丢失; 正确行为是与 GCC 一致地整体入栈。
        // `_Complex long double` (32B, 对齐 32) 是本规则的例外: 它本就不经
        // SSE/GP 路径 (见下方 X87 分类), 不能让此规则抢先压成 Memory。
        let force_memory = size > 64
            || matches!(ty, Type::LongDouble)
            || (matches!(ty, Type::Complex(_)) && !is_complex_ld && !is_complex_sse)
            || Self::type_has_vla(ty)
            || (ty.align() > 16 && !is_complex_ld);
        if force_memory {
            return vec![AbiClass::Memory];
        }
        // `_Complex long double` (32B): SysV ABI 3.2.3p2 分类为
        // [X87, X87Up, ComplexX87, X87Up] — 实部 tbyte 占前两个
        // eightbyte (X87 + 延续), 虚部 tbyte 占后两个 (ComplexX87
        // 起始 + 延续)。参数侧仍整体走内存栈槽 (classes 含 X87 →
        // force_mem); 返回侧凭此分类走 st0=real / st1=imag, 而非 sret。
        // 旧实现把所有 Complex 一刀切成 Memory, complex LD 返回被迫
        // 走 sret, 与 gcc 不互操作。
        if is_complex_ld {
            return vec![
                AbiClass::X87,
                AbiClass::X87Up,
                AbiClass::ComplexX87,
                AbiClass::X87Up,
            ];
        }
        let num_eightbytes = size.div_ceil(8);
        let mut classes = vec![AbiClass::NoClass; num_eightbytes];
        Self::classify_field(ty, 0, &mut classes);
        // Post-merger pass 1: 结构体**尾部之外**的 NoClass (即超过类型真实
        // 大小、仅因 num_eightbytes 向上取整产生的槽) 保持 NoClass, 不参与
        // 寄存器传递 (SysV ABI: 尾部填充不占 eightbyte)。这里无需转换。
        // 注意: 历史实现把所有 NoClass 统一改成 Memory, 再在 pass 2 里升级为
        // SseUp, 导致 16B 对齐但只含 8B double 的 aligned_d 被误判成
        // [Sse, SseUp], 多出一个本不存在的高 8 字节。
        // 仅当该 eightbyte 确实落在类型大小范围内才补 Memory (正常不会出现,
        // 因为 struct 内的洞由字段重叠分类覆盖)。
        for (i, c) in classes.iter_mut().enumerate() {
            if *c == AbiClass::NoClass && i * 8 < size {
                *c = AbiClass::Memory;
            }
        }
        // Post-merger pass 2: SSE 规则
        // 若首个 eightbyte 为 SSE, 检查其余**有数据的** eightbyte:
        //   - SSE/SSEUP → 保留
        //   - 含非 SSE/SSEUP 数据 → 全部转 MEMORY
        // 结构体尾部填充 (NoClass, i*8 >= size) 不参与判断, 也不升级为 SSEUP。
        if num_eightbytes > 1 && classes[0] == AbiClass::Sse {
            let mut all_sse_compatible = true;
            for (i, class) in classes.iter().enumerate().skip(1) {
                if i * 8 >= size {
                    // 尾部填充: 忽略
                    continue;
                }
                if *class != AbiClass::Sse && *class != AbiClass::SseUp {
                    // 检查该 eightbyte 是否有字段数据
                    let lo = i * 8;
                    let hi = lo + 8;
                    if Self::has_field_data_in_range(ty, lo, hi) {
                        all_sse_compatible = false;
                        break;
                    }
                }
            }
            if all_sse_compatible {
                // 不做任何 NoClass→SseUp 转换: 真正的向量延续已由
                // classify_field 的 Type::Vector 分支标为 SseUp; 尾部填充
                // 必须保持 NoClass。
            } else {
                for c in classes.iter_mut() {
                    *c = AbiClass::Memory;
                }
            }
        }
        // 归一化: 全 Memory 时压缩为单元素, 保证 needs_memory_return 判断正确
        if classes.len() > 1 && classes.iter().all(|c| *c == AbiClass::Memory) {
            classes.truncate(1);
        }
        // 丢弃尾部 NoClass: 那只是 num_eightbytes 向上取整产生的对齐填充,
        // 不构成实际传递的 eightbyte (如 16B 对齐的 8B double → 仅一个 Sse)。
        while classes.last() == Some(&AbiClass::NoClass) {
            classes.pop();
        }
        classes
    }
    pub(crate) fn compute_offset(ty: &Type, member: &str) -> usize {
        let fields = match ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => fields,
            _ => return 0,
        };
        let parts: Vec<&str> = member.split('.').collect();
        let mut offset = 0usize;
        let mut current_fields = fields.as_ref();
        for (i, part) in parts.iter().enumerate() {
            let (name, bracket_part) = if part.contains('[') {
                let pos = part.find('[').unwrap();
                (&part[..pos], &part[pos..])
            } else {
                (*part, "")
            };
            // 使用 find_field 支持匿名 struct/union 成员穿透查找
            if let Some((field, inner_off)) = find_field(current_fields, name) {
                offset += field.offset + inner_off;
                // 处理常量数组下标: "b[2][3]" → 加 2*stride1 + 3*stride2
                if !bracket_part.is_empty() {
                    let mut cur_ty = field.ty.clone();
                    for idx_str in bracket_part.split(['[', ']']) {
                        if idx_str.is_empty() {
                            continue;
                        }
                        if let Ok(idx_val) = idx_str.parse::<usize>() {
                            // stride = 当前数组元素类型的大小
                            let stride = match &cur_ty {
                                Type::Array(elem, _) => elem.size(),
                                _ => break,
                            };
                            offset += idx_val * stride;
                            // 下降到元素类型
                            match &cur_ty {
                                Type::Array(elem, _) => cur_ty = elem.base.clone(),
                                _ => break,
                            };
                        } else {
                            break; // 非常量下标 "?", 由运行时计算
                        }
                    }
                }
                if i < parts.len() - 1 {
                    if let Type::Struct(_, sub, _, _) | Type::Union(_, sub, _, false) = &field.ty {
                        current_fields = sub.as_ref();
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        offset
    }
    pub(crate) fn expr_bitfield_width(e: &Expr) -> Option<u32> {
        if let Expr::Member {
            expr: inner, field, ..
        } = e
        {
            let inner_ty = inner.ty();
            // 指针解引用: 取所指类型 (兼容数组隐式衰减)
            let struct_ty: &Type = match &inner_ty {
                Type::Ptr(p) => &p.base,
                Type::Array(elem, _) => &elem.base,
                other => other,
            };
            match struct_ty {
                Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                    find_field(fields, &field.name).and_then(|(f, _)| f.bit_width)
                },
                _ => None,
            }
        } else {
            None
        }
    }
    pub(crate) fn find_offsetof_field_type(ty: &Type, member: &str) -> Type {
        let fields = match ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => fields,
            _ => return Type::Int,
        };
        let parts: Vec<&str> = member.split('.').collect();
        let mut current_fields = fields.as_ref();
        let mut result_ty = Type::Int;
        for part in &parts {
            let name = if part.contains('[') {
                &part[..part.find('[').unwrap()]
            } else {
                part
            };
            if let Some((field, _)) = find_field(current_fields, name) {
                result_ty = field.ty.clone();
                if let Type::Struct(_, sub, _, _) | Type::Union(_, sub, _, false) = &field.ty {
                    current_fields = sub.as_ref();
                }
            }
        }
        result_ty
    }
    pub(crate) fn fits_disp32(off: i64) -> bool {
        (i32::MIN as i64..=i32::MAX as i64).contains(&off)
    }
    pub(crate) fn fits_disp8(off: i64) -> bool {
        (-128..=127).contains(&off)
    }
    pub(crate) fn gen_sret_return(&mut self, e: &Expr, ret_ty: &Type) {
        let sz = ret_ty.size();
        let sret_off = self
            .current_sret_offset
            .expect("sret return without saved sret pointer");
        // 浮点标量返回 (long double 为 Memory 类): 结果写入 sret 缓冲区.
        // long double 内部以 double 表示: 槽布局 = [double 位模式, 0].
        // 写回格式按声明返回类型判定, 表达式类型可能不同 (隐式转换).
        let declared_ret = self.current_func_ret_ty.clone();
        if matches!(declared_ret, Type::Float | Type::Double | Type::LongDouble) {
            let expr_ty = e.ty();
            if Self::is_integer_type(&expr_ty) {
                // 整数表达式 → rax → xmm0 (cvtsi2sd)
                self.gen_expr(e, 0);
                self.emit_scalar_to_xmm0_if_needed(e, &declared_ret);
            } else {
                // 浮点表达式 → xmm0; float 需提升到 double
                self.gen_expr(e, 0);
                if matches!(expr_ty, Type::Float) && !matches!(declared_ret, Type::Float) {
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd
                }
            }
            self.load_stack(7, sret_off); // rdi = sret 指针
            if matches!(declared_ret, Type::Float) {
                self.emit(&[0xF3, 0x0F, 0x11, 0x07]); // movss [rdi], xmm0
            } else if matches!(declared_ret, Type::LongDouble) {
                // long double: 低 8 字节 double + 高 8 字节零
                self.emit(&[0x66, 0x0F, 0xD6, 0x07]); // movq [rdi], xmm0
                self.emit(&[0x48, 0xC7, 0x47, 0x08, 0, 0, 0, 0]); // mov qword [rdi+8], 0
            } else {
                self.emit(&[0xF2, 0x0F, 0x11, 0x07]); // movsd [rdi], xmm0
            }
            return;
        }
        // 1. 源地址 → rsi (reg 6)
        let mut got_src = false;
        if Self::complex_src_needs_convert(&declared_ret, e.ty()) {
            // 声明返回 `_Complex` 而源不是同内型复数 (标量 / 异内型 `_Complex`):
            // 下面的交付是**对象表示的字节拷贝**, 必须先按声明类型物化出正确的
            // 内存对。转换在此不存在: `ci f(int n){ return n; }` (整数内型复数
            // 被 `leaf_abi_class` force 成 Memory 类) 把 int 的低 4 字节加上栈上
            // 高位垃圾灌进 `_Complex int` 缓冲区, 调用方读出 16/64 这类地址味
            // 道值。物化委派 `materialize_value_slot` (与赋值 / 形参侧同源的
            // `complex_src_needs_convert` + `gen_aggregate_assign_to`), 本站点只
            // 把已落地的对象表示当源地址。
            let slot = self.materialize_value_slot(&declared_ret, e);
            self.emit_lea_r13(6, slot); // rsi = 已按声明类型物化的源
            self.load_stack(7, sret_off);
            self.emit_agg_copy_to_sret(declared_ret.size());
            return;
        } else if matches!(e, Expr::Call { .. }) {
            // sret/寄存器返回调用: gen_expr 后 rax = &内部 sret 槽
            self.gen_expr(e, 0);
            self.mov(6, 0);
            got_src = true;
        } else if self.gen_lvalue_addr(e, 6) {
            // Ident / Member / Index / Deref / Cast: rsi = 源地址
            got_src = true;
        }
        if !got_src {
            // 兜底: 求值后按 rax 指向物化结果 (与既有聚合赋值语义一致)
            self.gen_expr(e, 0);
            self.mov(6, 0);
        }
        // 2. 目标地址 → rdi (reg 7)
        self.load_stack(7, sret_off);
        // 3. rep movsq + rep movsb 拷贝
        self.emit_agg_copy_to_sret(sz);
    }

    /// `rsi` → sret 缓冲区 (`[rdi]`) 的定长拷贝: qword 优先, 尾数回退到
    /// `rep movsb`。`gen_sret_return` 的两条源路径 (直接取址 / 先物化) 共用。
    fn emit_agg_copy_to_sret(&mut self, sz: usize) {
        let qwords = sz / 8;
        let remainder = sz % 8;
        if qwords > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (qwords)
            self.emit(&(qwords as u32).to_le_bytes());
            self.emit(&[0xF3, 0x48, 0xA5]); // rep movsq
        }
        if remainder > 0 {
            self.emit(&[0x48, 0xC7, 0xC1]); // mov rcx, imm32 (remainder)
            self.emit(&(remainder as u32).to_le_bytes());
            self.emit(&[0xF3, 0xA4]); // rep movsb
        }
    }
    pub(crate) fn gp_reg_name(idx: usize) -> &'static str {
        match idx {
            0 => "rax",
            1 => "rcx",
            2 => "rdx",
            3 => "rbx",
            4 => "rsp",
            5 => "rbp",
            6 => "rsi",
            7 => "rdi",
            8 => "r8",
            9 => "r9",
            10 => "r10",
            11 => "r11",
            12 => "r12",
            13 => "r13",
            14 => "r14",
            15 => "r15",
            _ => "rax",
        }
    }
    pub(crate) fn is_integer_type(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Bool
                | Type::Char
                | Type::SChar
                | Type::UChar
                | Type::Short
                | Type::UShort
                | Type::Int
                | Type::UInt
                | Type::Long
                | Type::ULong
                | Type::LongLong
                | Type::ULongLong
                | Type::Int128
                | Type::UInt128
        )
    }
    pub(crate) fn is_int_narrow(src: &Type, dst: &Type) -> bool {
        Self::is_integer_type(src) && Self::is_integer_type(dst) && dst.size() < src.size()
    }
    pub(crate) fn is_signed_int(ty: &Type) -> bool {
        matches!(
            ty,
            Type::SChar | Type::Short | Type::Int | Type::Long | Type::LongLong | Type::Int128
        )
    }
    pub(crate) fn is_sseup_128_aggregate(ty: &Type) -> bool {
        ty.size() == 16 && {
            let c = Self::abi_classes(ty);
            c.len() == 2 && c[0] == AbiClass::Sse && c[1] == AbiClass::SseUp
        }
    }
    pub(crate) fn mask_imm_to_type(v: i128, ty: &Type) -> i128 {
        let (bits, signed) = match ty {
            // 有符号小类型 (< 32 位): 截断 + 符号扩展
            Type::Char | Type::SChar => (8, true),
            Type::Short => (16, true),
            // 无符号小类型: 截断 + 零扩展
            Type::Bool | Type::UChar => (8, false),
            Type::UShort => (16, false),
            // UInt: 截断到 32 位, 零扩展到 64 位.
            // 例如 -1U (i128 = -1) → 截断到 0xFFFFFFFF → 零扩展到 0x00000000FFFFFFFF.
            Type::UInt => (32, false),
            // Int: 64 位寄存器已正确表示, 无需截断.
            // 正值: 0x00000000XXXXXXXX, 负值: 0xFFFFFFFFXXXXXXXX.
            Type::Int => return v,
            // Long/LongLong/Int128: 64 位寄存器已正确表示, 无需截断
            Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Int128
            | Type::UInt128 => return v,
            _ => return v,
        };
        let mask = (1i128 << bits) - 1;
        let truncated = v & mask;
        if signed && (truncated & (1i128 << (bits - 1))) != 0 {
            // 符号位为 1: 符号扩展 (高位全 1)
            truncated | !mask
        } else {
            truncated
        }
    }
    pub(crate) fn member_target_ty(expr_ty: Type, is_arrow: bool) -> Type {
        if !is_arrow {
            return expr_ty;
        }
        match &expr_ty {
            Type::Ptr(p) => p.base.clone(),
            Type::Array(elem, _) => elem.base.clone(),
            _ => expr_ty,
        }
    }
    /// ABI 分类里是否含**强制内存承载**的类（Memory / X87 家族）。
    ///
    /// 这是 SysV 参数传递与返回值交付的根判据，caller 装参、callee 取参和
    /// 返回方式三处原先各自手写一遍同样的 `matches!`，改一处就会漂移。
    pub(crate) fn classes_contain_memory_class(classes: &[AbiClass]) -> bool {
        classes.iter().any(|c| {
            matches!(c, AbiClass::Memory | AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87)
        })
    }

    /// 按 ABI 分类判定一个对象是否**必须整体驻留内存**：含 Memory 类，或
    /// eightbyte 数超过寄存器对能容纳的 2 个。
    ///
    /// 关键判据：`SSE`/`SSEUp` 只是普通向量类（`__m128d` = `[Sse, SseUp]`），
    /// 它们走 xmm 寄存器或 overflow 槽，本身**不**强制内存承载；而共享同一个
    /// 16 字节槽的标量（`struct {__m128d;double}` 的第二 eightbyte = `SseUp`）
    /// 绝不能按整数类从 GP 区取。按分类判定才与对象的**书写形态**无关。
    pub(crate) fn classes_force_mem(classes: &[AbiClass]) -> bool {
        Self::classes_contain_memory_class(classes) || classes.len() > 2
    }

    /// SysV AMD64 ABI 3.5.1 的 **all-or-nothing** 装参判据：一个对象的所有
    /// eightbyte 必须能**同时**放进剩余的参数寄存器；只要有一个放不下，整个
    /// 对象一次性走内存，且它本来会占用的寄存器**留给下一个参数**（不推进游标）。
    ///
    /// 这是 caller (`emit_sysv_call`) 与 callee (`gen_func` 形参采集) 的**唯一**
    /// 拆分真源。两侧曾各写一套判据：caller 用本规则整体入栈、callee 却逐
    /// eightbyte 拆分（“部分进寄存器 + 部分进栈”），于是
    /// `f(int a,..,int e, struct {long,long} p)` 场景 caller 把两个 eightbyte 都
    /// 放进 overflow 区，callee 却从 r9 取低半、从 overflow 首槽取高半 —— 被调方
    /// 读到的 `p.a` 是垃圾、`p.b` 是 `p.a`。gcc 真值（-O0 反汇编）证实整体入栈：
    /// `push q.b; push q.a` 后 callee 从 `0x10(rbp)`/`0x18(rbp)` 两次取字，
    /// 而后续参数 `z` 仍进 r9。
    ///
    /// `gp_avail`/`fp_avail` 是两侧各自的剩余 GP / XMM 寄存器数；判据只看分类，
    /// 不看对象的书写形态（struct / `__int128` / 向量一视同仁）。
    pub(crate) fn aggregate_forces_memory(
        classes: &[AbiClass],
        gp_avail: usize,
        fp_avail: usize,
    ) -> bool {
        let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
        // SseUp 是前一个 Sse 所在 xmm 高 64 位的延续，不单独占寄存器。
        let need_fp = classes.iter().filter(|c| **c == AbiClass::Sse).count();
        Self::classes_contain_memory_class(classes) || gp_avail < need_gp || fp_avail < need_fp
    }

    /// 该类型是否**整体**由 SSE 类构成（可全部从 16 字节 FP 槽按宽度读出）。
    ///
    /// 这是 va_arg 路由的唯一判据：`float`/`double` 与裸 `__m128` 系向量同属
    /// SSE 家族，必须走同一条路径；按类型书写形态枚举会漏掉裸向量，使其误入
    /// GP 区。空分类（size==0）返回 false，交由调用方走通用路径。
    pub(crate) fn classes_all_sse_family(classes: &[AbiClass]) -> bool {
        !classes.is_empty()
            && classes
                .iter()
                .all(|c| matches!(c, AbiClass::Sse | AbiClass::SseUp))
    }

    pub(crate) fn needs_memory_return(ty: &Type) -> bool {
        // 分类强制 Memory/X87，或超过 2 个 eightbyte 放不进 rax+rdx → sret
        //
        // 标量 `long double` 是例外：SysV AMD64 规定 X87 **标量**返回值
        // 放在 **st(0)**（gcc 真值：`fldt <sym>` 后紧跟 `ret`），只有*含*
        // X87 成员的聚合才返回内存。
        //
        // 注意不得误伤**参数传递**侧：X87 类参数确实走内存（16 字节栈槽）。
        // 同一个 `classes_force_mem` 判据在两侧的结论相反，故只在此处特判，
        // 不改 `classes_force_mem` 本身（它还被 caller 与 va_arg 复用）。
        if matches!(ty, Type::LongDouble) {
            return false;
        }
        // `_Complex long double` 同样是例外：SysV ABI 3.2.3p2 规定
        // 其分类 [X87, X87Up, ComplexX87, X87Up] 的返回值经 x87 交付 —
        // st0=实部, st1=虚部 (gcc 真值: 两条 `fldt` 后 `ret`)，不走 sret。
        // 仅返回侧如此；参数侧仍凭 X87 类 force_mem 走内存栈槽。
        if let Type::Complex(c) = ty {
            if matches!(&c.base, Type::LongDouble) {
                return false;
            }
        }
        Self::classes_force_mem(&Self::abi_classes(ty))
    }
    pub(crate) fn ptr_seg_prefix(ty: &Type) -> Option<u8> {
        if let Type::Ptr(qt) = ty {
            qt.addr_space.prefix_byte()
        } else {
            None
        }
    }
    /// 一条**左值表达式**的最终访问需要哪个段前缀：沿 `Deref`/`Index`/`Member`/`Cast`
    /// 的基址链下潜，取第一个 pointee 带 `__seg_gs`/`__seg_fs` 的指针。
    ///
    /// 单一入口。所有「先算地址、再访问内存」的站点（语义层 `materialize_address`、
    /// 解引用、下标/成员的旧路径、inc/dec、聚合写入）都必须走它，而不是各自只瞥
    /// 最外面那一层 —— 只看一层的实测后果是 `p[i]` / `q->a` / `a[i].f` 这类「基址是
    /// 段指针、外面还包一层下标或成员」的访问静默按普通地址读写（与 gcc 产物不一致，
    /// 在内核 per-cpu 场景下是写错内存）。
    pub(crate) fn lvalue_seg_prefix(e: &Expr) -> Option<u8> {
        let base = match e {
            Expr::Deref { e, .. } => Some(&**e),
            Expr::Index { arr, .. } => Some(&**arr),
            Expr::Member { expr, .. } => Some(&**expr),
            Expr::Cast { e, .. } => Some(&**e),
            // 地址算术（`*(p + 1)` / `*(p + i)`）：结果类型已带段，但万一上游只
            // 给了一侧的操作数带段，这里一并下潜。
            Expr::Binop { l, r, .. } => {
                return Self::lvalue_seg_prefix(l).or_else(|| Self::lvalue_seg_prefix(r))
            },
            _ => None,
        };
        match base {
            Some(b) => Self::seg_prefix_of_base(b),
            None => None,
        }
    }
    /// 同上，但入参已经是「被解引用 / 被取成员 / 被下标的**基址表达式**」（`*p` 的 `p`、
    /// `p->a` 的 `p`、`*(pp+1)` 的 `pp`）：先看这一层指针的 pointee 带不带段，
    /// 不带再往下潜。
    pub(crate) fn seg_prefix_of_base(base: &Expr) -> Option<u8> {
        Self::ptr_seg_prefix(&base.ty()).or_else(|| Self::lvalue_seg_prefix(base))
    }
    /// 发一枚已算好的段前缀（`None` 时不发任何东西 —— 误发会污染下一条指令）。
    pub(crate) fn emit_seg(&mut self, seg: Option<u8>) {
        if let Some(seg) = seg {
            self.emit(&[seg]);
        }
    }
    /// 紧接其后的那一笔内存访问要不要带段前缀：就地判、就地发。
    ///
    /// 调用约定：必须**紧贴** load/store 的字节序列，中间不能插别的指令字节 ——
    /// 段前缀只作用于它后面的那一条指令。
    pub(crate) fn emit_seg_for(&mut self, lv: &Expr) {
        if let Some(seg) = Self::lvalue_seg_prefix(lv) {
            self.emit(&[seg]);
        }
    }
    /// `Self::rex()` 恒返回 ≥0x40，因此「一个字段都不用扩」时它给出的是一枚**不扩任何字段
    /// 的裸前缀**。它不改变语义，但会被 objdump 标成 `rex add %ebx,%eax` —— 那是多余位的
    /// 唯一读数，也是下一个人照抄的源头。这个助手把「按需才发」集中到一处。
    ///
    /// 8 位上下文**不走这里**：%spl/%bpl/%sil/%dil（字段值 4..7）需要的是「存在 REX」，
    /// 那些调用点自己显式发 `0x40 | …`（见 enc_alu / enc_mov 的 bits == 8 分支）。
    /// `byte_ctx` 为真表示本条指令可能是 8 位形式，此时一律发前缀（保守，不弱化正确性）。
    pub(crate) fn emit_rex_if_used(&mut self, w: bool, r: u8, x: u8, b: u8, byte_ctx: bool) {
        if byte_ctx || w || r >= 8 || x >= 8 || b >= 8 {
            let v = Self::rex(w, r, x, b);
            self.emit(&[v]);
        }
    }

    pub(crate) fn rex(w: bool, r: u8, x: u8, b: u8) -> u8 {
        let mut v = 0x40u8;
        if w {
            v |= 0x08;
        }
        if r >= 8 {
            v |= 0x04;
        }
        if x >= 8 {
            v |= 0x02;
        }
        if b >= 8 {
            v |= 0x01;
        }
        v
    }
    pub(crate) fn seg_meta(seg: u8) -> (u8, u8) {
        match seg {
            0 => (0, 0x26), // ES
            1 => (1, 0x2E), // CS
            2 => (2, 0x36), // SS
            3 => (3, 0x3E), // DS
            4 => (4, 0x64), // FS
            5 => (5, 0x65), // GS
            _ => (0, 0x3E),
        }
    }
}
