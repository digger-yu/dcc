//! emit/expr/aggregate.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::BoolExpect;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{SimdArithOp, SimdWidth};
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
    pub(crate) fn emit_zero_value(&mut self, ty: &Type, dst: u8, sret_off: Option<i64>) {
        if let Some(off) = sret_off {
            self.mov_imm(0, 0);
            self.zero_mem(off, ty.size().max(1));
            self.emit_lea_r13(0, off);
            if dst != 0 {
                self.mov(dst, 0);
            }
            return;
        }
        match ty {
            Type::Void => {},
            Type::Float | Type::Double | Type::LongDouble => {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 0, 0)); // xorps xmm0, xmm0
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Complex(inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 0, 0)); // xorps xmm0, xmm0
                self.emit_insn(&Insn::simd_arith_reg(SimdArithOp::Xorps, SimdWidth::PS, 1, 1)); // xorps xmm1, xmm1
                let result_off = self.complex_result_off();
                let pfx = if matches!(&inner.base, Type::Float) {
                    0xF3u8
                } else {
                    0xF2u8
                };
                let inner_sz = inner.size() as i64;
                self.emit_xmm_r13_disp(pfx, 0x11, 0, result_off);
                self.emit_xmm_r13_disp(pfx, 0x11, 1, result_off + inner_sz);
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Vector(_, _) => {
                let off = self.vector_temp_base(0, ty.size());
                self.mov_imm(0, 0);
                self.mov_imm(2, 0);
                self.zero_mem(off, ty.size().max(1));
                if dst != 0 {
                    self.mov_imm(dst, 0);
                }
            },
            Type::Int128 | Type::UInt128 | Type::Complex(_) => {
                self.mov_imm(dst, 0);
                self.mov_imm(2, 0);
            },
            _ => self.mov_imm(dst, 0),
        }
    }
    pub(crate) fn gen_agg_elem_store(&mut self, r: &Expr, elem_ty: &Type, es: i32, dst: u8) {
        self.push(1); // 保存目标地址 (rcx)
        if !self.gen_lvalue_addr(r, 6) {
            // 非 lvalue 源 (CompoundLit 等): gen_expr 约定返回聚合地址
            self.gen_expr(r, Self::EVAL_BASE);
            self.mov(6, 0);
        }
        self.pop(7); // rdi = 目标地址
                     // 表达式值 = 目标地址 (dst=0 → rax), 必须在 rep movsb 之前取:
                     // rep movsb 会把 rdi 前进到目标末尾, 之后取 rdi 得到的是 &elem+n
                     // (20070919-1: a = ({... p[3] = b;}) 的块值变成 p[3]+8, 源读到
                     // 数组外全 0)。大小读取只用 rcx/r11, 不破坏 rax。
        self.mov(dst, 7);
        // rcx = 拷贝大小: VM struct/union 运行时 (声明点绑定槽优先,
        // C11 6.2.4p7), 其余编译期元素大小
        if elem_ty.has_vla() {
            if let Some(slot) = self.vm_size_slot(elem_ty) {
                self.emit_mov_mem_r13(1, slot);
            } else {
                self.gen_sizeof_runtime(elem_ty, 11);
                self.mov(1, 11);
            }
        } else {
            self.mov_imm(1, es.max(1) as i128);
        }
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb));
        // rep movsb
    }
    /// 聚合**形态**判据: 结构体/联合体/数组/原子/向量。与「值模型」判据
    /// [`Self::value_is_whole_copy`] 分开, 两者一起构成聚合赋值分发的全部知识,
    /// 不允许在调用方重写形状队集。
    pub(crate) fn is_aggregate_shape(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Struct(_, _, _, _)
                | Type::Union(_, _, _, _)
                | Type::Array(_, ArraySize::Const(_))
                | Type::Atomic(_)
                | Type::Vector(_, _)
        )
    }

    /// 聚合**值模型**判据: 该类型的值必须整块拷贝, 不能用恒 8 字节的标量
    /// store 承载 (尺寸装不进一个寄存器, 或者尺寸是运行时值)。这也是
    /// `va_arg` 缓冲区交付 (`va_arg_result_in_memory`) 与内存返回
    /// (`needs_memory_return`) 共用的同一条「装不进寄存器」轴。
    pub(crate) fn value_is_whole_copy(ty: &Type) -> bool {
        Self::is_aggregate_shape(ty) && (ty.size() > 8 || ty.has_vla())
    }

    /// 聚合 **rvalue 的交付形态**: `true` = 求值后 rax 里是**缓冲地址**
    /// (内存交付), `false` = rax(:rdx) 就是**数据** (寄存器交付)。
    ///
    /// 与上述三条轴同源: 返回轴 `needs_memory_return`、`va_arg` 交付轴
    /// `va_arg_result_in_memory`、赋值/语句表达式的「目标地址即值」约定。
    /// 调用侧按值存实参、下标/成员基址、返值转发等所有「拿 rvalue 当源」
    /// 的地方都只准问本函数, 不得各自重写形态队集 —— 否则会出现
    /// 「把缓冲地址当数据存槽」同型错误 (被调方读出指针字节)。
    pub(crate) fn rvalue_is_buffer_addr(e: &Expr) -> bool {
        let e = Self::skip_noop_casts(e);
        let ty = e.ty();
        if !Self::value_is_whole_copy(&ty) {
            return false;
        }
        match e {
            Expr::Call { .. } => Self::needs_memory_return(&ty),
            Expr::VaArg { .. } => Self::va_arg_result_in_memory(&ty),
            Expr::Assign { .. } | Expr::StmtExpr { .. } | Expr::CompoundLit { .. } => true,
            // 聚合三元 `c ? s1 : s2`: 值是选中那一整个对象, `gen_expr_cond`
            // 把两支都物化进一个帧内临时槽后交付其地址 (与上面同一条约定)。
            // 报告成寄存器交付会让消费端把 rax 当数据存槽 —— sqlite3 lemon
            // parser 的 `tok = (a.n==0 ? a : b);` 因此只写对 `Token.z`,
            // `Token.n` 留残值, CREATE TRIGGER 算出 7.9MB 的拷贝长度。
            Expr::Cond { ty, .. } => matches!(ty, Type::Struct(..) | Type::Union(..)),

            // 具名变量 / 成员 / 下标 / 解引用等 lvalue 形态由各自的源分支
            // (取地址后按块装载) 处理, 不走 rvalue 通道。
            _ => false,
        }
    }

    /// 形态分类视图: 剥掉**同类型** (no-op) 的 `Cast` 包装。
    ///
    /// 判据与值模型同源: 类型完全相同的转换既不改变位模式也不改变交付位置
    /// (`gen_expr_cast` 对它是转发内层), 因此「它是地址还是数据」这个问题
    /// 只能问剥壳后的真实形态。C++ 的类拷贝/成员赋值会在 rvalue 上留一层
    /// `Cast{同一类}`, 不剥壳便落入恒 16 字节的通用物化路径, 把 `gen_expr`
    /// 交付的**缓冲地址**当数据写进目标 (`d = s.f` 第一 quad 是指针字节)。
    /// 跨类型的转换 (派生→基类、复合字面量重建) 会另建物化缓冲, 包装必须
    /// 保留 —— 它的地址才是值。
    pub(crate) fn skip_noop_casts(mut e: &Expr) -> &Expr {
        while let Expr::Cast { ty, e: inner, .. } = e {
            if inner.ty() == *ty {
                e = inner;
            } else {
                break;
            }
        }
        e
    }

    /// 聚合目标地址 → 指定寄存器。[`AggDst`] 四种寻址方式的唯一解释器
    /// (旧写法在函数内四处重复 `if is_global_dst { global } else { lea }`)。
    pub(crate) fn emit_agg_dst_addr(&mut self, dst: &AggDst, reg: u8) {
        match dst {
            AggDst::Global(name) => self.emit_global_addr(reg, name),
            // VM struct/VLA: 帧槽存的是数据区基址, 不是数据本体
            AggDst::Local {
                vla: Some(slot), ..
            } => self.load_stack(reg, *slot),
            AggDst::Local { off, .. } => self.emit_lea_r13(reg, *off),
            // 运行时地址: 调用方已把它存进帧槽
            AggDst::Indirect { addr_slot } => self.load_stack(reg, *addr_slot),
        }
    }

    /// 具名变量目标入口 (历史签名)。目标寻址方式一律解析成 [`AggDst`]。
    pub(crate) fn gen_aggregate_assign(&mut self, dst_name: &str, dst_ty: &Type, r: &Expr) {
        let is_global = self.is_global_not_local(dst_name);
        let dst_off = if is_global {
            0 // placeholder, not used for global dest
        } else {
            self.var_offset(dst_name)
        };
        let vla = if is_global {
            None
        } else {
            self.vla_offsets.get(dst_name).copied()
        };
        let dst = if is_global {
            AggDst::Global(dst_name.to_string())
        } else {
            AggDst::Local { off: dst_off, vla }
        };
        self.gen_aggregate_assign_to(&dst, dst_ty, r);
    }

    /// `_Complex` 目标的源是否**不是**同内型的复数对象表示 —— 唯一判据点。
    ///
    /// C6.3.1.8p1/p4: 任何算术类型都可隐式转成复数类型 (实部 = 转换后的标量,
    /// 虚部 = +0), 复数→复数则是**逐分量**的。只有「源已经是目标的对象表示」
    /// (同内型 `_Complex`) 才允许按分量直读或整体字节搬运; 标量源与异内型源
    /// 一律要先经 [`Self::gen_aggregate_assign_to`] 转换。
    /// 赋值 / 实参 / 返回三类站点共用本函数, 不得在站点重写宽度或内型判据
    /// (旧写法在 call.rs 只认 `Complex→Complex` 一条轴, 标量实参 `take(11)`
    /// 于是按复数对直读物化区, 分量全 0; 返回端同类缺失见
    /// [`Codegen::gen_complex_return`])。
    ///
    /// `_Imaginary` 源: 同内型时沿用既有交付约定 (分量路径各自补零实部),
    /// 内型不同时同样需要逐分量转换。
    pub(crate) fn complex_src_needs_convert(dst: &Type, src: Type) -> bool {
        match dst {
            Type::Complex(d) => match src {
                Type::Complex(s) => s.base != d.base,
                Type::Imaginary(s) => s.base != d.base,
                _ => true,
            },
            _ => false,
        }
    }

    /// 把任意表达式按目标类型物化到一个帧内临时槽, 返回槽偏移。
    /// 这是 [`Self::gen_aggregate_assign_to`] 的「目标是帧内临时槽」特例,
    /// 供返回 / 实参这类「先按目标类型落地、再按 SysV 分类装载寄存器」的
    /// 站点复用 —— 转换语义仍然只有 `gen_aggregate_assign_to` 一处。
    pub(crate) fn materialize_value_slot(&mut self, ty: &Type, r: &Expr) -> i64 {
        let slot = self.alloc_sret_slot(ty.size().max(16));
        self.gen_aggregate_assign_to(
            &AggDst::Local {
                off: slot,
                vla: None,
            },
            ty,
            r,
        );
        slot
    }

    pub(crate) fn gen_aggregate_assign_to(&mut self, dst: &AggDst, dst_ty: &Type, r: &Expr) {
        // 分类与求值都按剥掉 no-op `Cast` 后的形态进行 (单一轴, 见
        // [`Self::skip_noop_casts`]): no-op 转换对两者都完全透明。
        let r: &Expr = Self::skip_noop_casts(r);
        let sz = dst_ty.size();
        // 语义与历史 `is_global_dst` 一致; 新增的 `Indirect` (运行时地址) 与全局
        // 同属「不可按帧偏移直存」一类。`dst_off` 对这两类是没意义的占位 0,
        // 只在 `frame_off()` 为 Some 的分支上才会被读。
        let staged_dst = dst.needs_staged();
        let dst_off = dst.frame_off().unwrap_or(0);
        // `_Complex` 目标能否按**对象表示**整体搬运, 只由一个轴决定: 源的值
        // 表示是否与目标同宽同型。判据单点在此, 下面 Ident / Deref / Index /
        // Member 各源形态分支与上方 Call 的 ABI 返回快路径共用同一份
        // (旧写法只拦得住标量 RHS 一种形态):
        //  · 标量 RHS (`c = i`): 从 &i 拷 16 字节会把标量的栈上高位垃圾灌进虚部;
        //  · 内型不同的复数 RHS (`zdd = zff`、`f(zfloat)` 里 `_Complex float` 实参
        //    → `_Complex double` 形参): C6.3.1.8p4 的复数算术转换是**逐分量**的,
        //    元素类型参与值表示。整体字节拷贝 (以及按**源**类型 ABI 分类把返回
        //    寄存器直存) 等于拿源分量的位模式当目标分量的位模式 —— `_Complex
        //    float` 的两个 float 被当成 `_Complex double` 的低那个 double (实部
        //    读出 8.0000018961727619171), 虚部则越界读到隔壁槽。
        // 两者都必须绕到下方 got_src==false 的物化分量转换分支。
        let complex_needs_component_convert = Self::complex_src_needs_convert(dst_ty, r.ty());
        // 特殊处理: 源是函数调用 (走完整 SysV ABI: 参数分类/暂存/返回值)
        let mut call_sret_handled = false;
        if let Expr::Call {
            callee,
            args,
            ty: call_ty,
            ..
        } = r
        {
            if complex_needs_component_convert {
                // 本块的返回端快路径按**源** (`call_ty`) 的 ABI 分类存寄存器,
                // 而复数分量转换需要的是按**目标**内型重新物化: `_Complex float`
                // 的返回值是打包在一个 8 字节 eightbyte 里的两个 float, 与
                // `_Complex double` 的 [double, double] 不是同一对象表示,
                // 直存即位模式误读 (`my_cimag_d(reassemble_fc(7.5f,8.5f))` 得 3
                // 而 gcc 得 8.5)。落到下方分量物化路径 (只求值一次)。
            } else if let Expr::Ident { name, .. } = callee.as_ref() {
                if name.name.starts_with("__builtin_") && !Self::is_fortify_builtin(&name.name) {
                    // 编译器内建不能当外部符号 `call`：一律跳过下方 SysV 调用段，
                    // 落到函数尾部的通用物化路径 —— 那条路径用 `gen_expr` 求 RHS，
                    // 会走 `gen_builtin` 的内建实现（向量 rvalue 交付在
                    // `vector_temp_base(0, size)`，复数在 result_off + xmm0）。
                    //
                    // 旧写法在这里硬列了 conj 三个名字作为唯一例外，其余 `__builtin_*`
                    // 一律当「未实现」直接 `gen_unknown_builtin_fallback` 报错。那份名单
                    // 与 `gen_builtin` 的 `match name` 是同一个事实的两处副本，内建加进了
                    // `gen_builtin` 而这里没同步就会把已实现的内建误判成不支持：实测
                    // `c = __builtin_shuffle(x, y, m)`（返回 16 字节向量 ⇒ 走聚合赋值）报
                    // E0018，而同一内建在标量交付路径上工作正常。真正未实现的内建仍会在
                    // `gen_expr` → `emit_call` 里得到 E0018，不损失任何诊断。
                } else if Self::needs_memory_return(call_ty) {
                    // sret 返回: 分配隐藏指针槽, 调用后从槽拷贝到目标变量
                    let sret_off = self.alloc_sret_slot(call_ty.size());
                    self.emit_sysv_call(Some(&name.name), None, args, Some(sret_off));
                    self.emit_lea_r13(6, sret_off); // rsi = 源 (sret 槽)
                    self.emit_agg_dst_addr(dst, 7); // rdi = 目标地址
                    call_sret_handled = true;
                } else if Self::abi_classes(call_ty).iter().any(|cls| {
                    matches!(
                        cls,
                        AbiClass::X87 | AbiClass::X87Up | AbiClass::ComplexX87 | AbiClass::Memory
                    )
                }) {
                    // 返回值不在 rax/xmm 上: 下面那个按分类存寄存器的循环只
                    // 认识 Integer / Sse / SseUp, 碰上 X87 家族直接 `unreachable!()`
                    // ICE (`_Complex long double g; g = f();` 的分类是
                    // [X87, X87Up, ComplexX87, X87Up], 经 st0/st1 交付而
                    // `needs_memory_return` 对它是 false)。落到函数尾部的通用
                    // 复数物化路径 —— 那边按 x87 st(0)/st(1) → 内存对处理。
                    // 判据只问 `abi_classes` 这个真源, 不在此重写形状枚举。
                } else {
                    // 寄存器返回: 按 abi_classes 存储
                    // (INTEGER→rax:rdx, SSE/SSEUP→xmm0:xmm1)
                    self.emit_sysv_call(Some(&name.name), None, args, None);
                    // 128-bit 单寄存器 SSE 聚合: abi_classes 生成 [Sse, SseUp] 两个
                    // eightbyte, 但 SysV 把完整 16 字节放在**同一个** xmm0 里返回
                    // （`SseUp` 是同一寄存器的高半延续，不是第二个寄存器）。
                    // 不得拆成 store_xmm64(xmm0)+store_xmm64(xmm1) —— 那会从
                    // 从未赋值的 xmm1 取高半（pr92618）。必须用 store_xmm128_stack
                    // 一次性存 128bit。
                    //
                    // 判据直接用被调方 stmt.rs 返回端用的同一个
                    // `is_sseup_128_aggregate`，而不是自己重写形状队集：旧写法限定
                    // `Type::Vector`，于是 `struct { v4f v; }` 这种**含向量的聚合**
                    // 漏判而落入逐 chunk 存储（实测 4 元素返回 `1 2 2 0`）。
                    // 两侧共用单一真源后，这类错配不可能再发生。
                    let is_sse_128_single_reg = Self::is_sseup_128_aggregate(call_ty);
                    if staged_dst {
                        // 不可按帧偏移直存的目标 (全局 / 运行时地址):
                        // dst_off=0 对 store_stack 非法 (会踩返回地址)。
                        // 先存进 sret 临时槽, 再 rep movsb 到真实地址。
                        let temp_off = self.alloc_sret_slot(call_ty.size());
                        if is_sse_128_single_reg {
                            self.store_xmm128_stack(0, temp_off);
                        } else {
                            let ret_classes = Self::abi_classes(call_ty);
                            for (c, cls) in ret_classes.iter().enumerate() {
                                let off = temp_off + (c as i64) * 8;
                                match cls {
                                    AbiClass::Integer => {
                                        let reg = if c == 0 { 0u8 } else { 2u8 };
                                        self.store_stack(reg, off);
                                    },
                                    AbiClass::Sse | AbiClass::SseUp => {
                                        self.store_xmm64_stack(c as u8, off);
                                    },
                                    _ => unreachable!(),
                                }
                            }
                        }
                        self.emit_lea_r13(6, temp_off); // rsi = src
                        self.emit_agg_dst_addr(dst, 7); // rdi = dst
                                                        // rep movsb handled below
                        call_sret_handled = true;
                    } else {
                        if is_sse_128_single_reg {
                            self.store_xmm128_stack(0, dst_off);
                            return;
                        }
                        let ret_classes = Self::abi_classes(call_ty);
                        for (c, cls) in ret_classes.iter().enumerate() {
                            let off = dst_off + (c as i64) * 8;
                            match cls {
                                AbiClass::Integer => {
                                    let reg = if c == 0 { 0u8 } else { 2u8 };
                                    self.store_stack(reg, off);
                                },
                                AbiClass::Sse | AbiClass::SseUp => {
                                    self.store_xmm64_stack(c as u8, off);
                                },
                                AbiClass::Memory
                                | AbiClass::X87
                                | AbiClass::X87Up
                                | AbiClass::ComplexX87 => {
                                    unreachable!()
                                },
                                AbiClass::NoClass => unreachable!(),
                            }
                        }
                        return;
                    }
                }
            }
        }
        // SysV va_arg 聚合读取: 按 abi 分类从寄存器保存区/溢出区读出,
        // 直接写入目标变量缓冲 (rdi)。通用 rax/rdx 物化路径只覆盖 16 字节,
        // >16B 聚合会截断/污染, 必须绕开。
        //
        // 判据只准问 `va_arg_result_in_memory`（交付约定的单一真源），不得在此
        // 重写书写形态枚举：旧式 `Struct|Union && size > 8` 把 16 字节裸向量
        // (`__m128d`) 漏给下方通用 Vector 分支, 而该分支的源固定取
        // `vector_temp_base(0, sz)` —— `gen_expr_va_arg` 却把装载结果写进
        // `alloc_sret_slot` 的槽 (`vec_temp_off + 600`), 两者不是同一处, 于是
        // 装载器填好的值被一个错误源整体覆盖 (sseup_shares_one_slot rc=3)。
        // 走本分支既消除该错配, 也省掉「临时槽 → 目标」的多余一次拷贝。
        if let Expr::VaArg { ap, ty: arg_ty, .. } = r {
            if Self::va_arg_result_in_memory(arg_ty) {
                let va_addr = self.resolve_va_list_addr(ap);
                // 设置 rdi = 目标地址 (resolve 之后, 避免被 scratch 覆盖)
                self.emit_agg_dst_addr(dst, 7);
                if let VaListAddress::Named(ref name) = va_addr {
                    self.gen_va_arg_struct(name, arg_ty);
                } else {
                    // rcx 仍有 va_list 指针 (emit_lea_r13/emit_global_addr
                    // 只写 rdi, 不覆盖 rcx)
                    self.gen_va_arg_ptr_struct(arg_ty);
                }
                return;
            }
        }
        // 1. 获取源地址到 rsi (复数分量转换判据见函数开头的
        // `complex_needs_component_convert`, 与上方 Call 快路径同源)
        let got_src = if call_sret_handled {
            true
        } else if complex_needs_component_convert {
            false
        } else {
            match r {
                Expr::Ident { name: src_name, .. } => {
                    if self.is_global_var(&src_name.name) {
                        self.emit_global_addr(6, &src_name.name); // rsi = 全局变量地址
                    } else if let Some(&off) = self.vla_offsets.get(&src_name.name) {
                        // VM struct/VLA 变量: 源地址 = 数据区基址槽
                        self.load_stack(6, off);
                    } else {
                        let src_off = self.var_offset(&src_name.name);
                        self.emit_lea_r13(6, src_off); // rsi = 源地址 (reg 6 = rsi)
                    }
                    true
                },
                // *ptr: evaluate ptr to get address, use directly as source.
                // This avoids the rax/rdx temp path which only handles 16 bytes.
                Expr::Deref { e: inner, .. } => {
                    self.gen_expr(inner, Self::EVAL_BASE); // rax = ptr
                    self.mov(6, 0); // rsi = rax (source address)
                    true
                },
                // 聚合 lvalue 源 (p[i] / s.f): 直接取地址。通用回退路径
                // 把 rax:rdx 当前 16 字节内容物化, 对聚合 rvalue (地址
                // 约定) 会拷成地址字节 (20070919-1)
                Expr::Index { .. } | Expr::Member { .. } => self.gen_lvalue_addr(r, 6),
                // 链式聚合赋值 / 语句表达式块值: gen_expr 约定返回聚合
                // 目标地址, 直接作源 (20070919-1: a = ({ ...; p[3] = b; }))
                Expr::Assign { .. } | Expr::StmtExpr { .. } => {
                    self.gen_expr(r, Self::EVAL_BASE);
                    self.mov(6, 0); // rsi = rax (聚合源地址)
                    true
                },
                // 聚合三元 `c ? s1 : s2`: `gen_expr_cond` 已按同一条内存交付约定
                // 把选中支整块物化进帧内临时槽, rax = 槽地址, 直接当源拷贝。
                // 落到下面通用物化分支只会拷到 rax:rdx 的前 16 字节 (其中
                // rdx 从未装载), sqlite3 的 `tok = (a.n==0 ? a : b);` 即此。
                Expr::Cond { .. } if Self::rvalue_is_buffer_addr(r) => {
                    self.gen_expr(r, Self::EVAL_BASE);
                    self.mov(6, 0); // rsi = rax (临时槽地址)
                    true
                },
                _ => false,
            }
        };
        if !got_src {
            if matches!(dst_ty, Type::Vector(_, _)) {
                self.gen_expr(r, Self::EVAL_BASE);
                let temp_off = self.vector_temp_base(0, sz);
                self.emit_lea_r13(6, temp_off);
            } else {
                // _Complex 浮点 rvalue (非 Call/Ident): 求值后实部在 xmm0,
                // 虚部在 [r13+result_off+inner_sz] (binop/取负/字面量物化
                // 位置), 直接分存两分量, 不走 rax 单槽 + rep movsb 通用路径
                if let Type::Complex(ref inner) = dst_ty {
                    let dst_float =
                        matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble);
                    let src_complex = matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_));
                    let src_inner = match r.ty() {
                        Type::Complex(ref c) | Type::Imaginary(ref c) => Some(c.base.clone()),
                        Type::Float | Type::Double | Type::LongDouble => Some(r.ty()),
                        _ => None,
                    };
                    let src_float = src_inner.as_ref().is_some_and(|b| {
                        matches!(b, Type::Float | Type::Double | Type::LongDouble)
                    });
                    // ── 整数内型 `_Complex` 源 → float/double 内型目标 ────────
                    // C6.3.1.8p4 的复数算术转换是**逐分量**的, 整数分量必须各自
                    // 转成目标浮点。下面两条通用臂都假定「分量已在 xmm0」
                    // (`emit_scalar_to_xmm0_if_needed` 把任何 `Complex(_)` 都归入
                    // “浮点表达式”直接返回, `gen_complex_imag_into_xmm0` 又按浮点
                    // 位模式读内存对), 对整数内型源整体失效: `zf = zi` 实部恒 0,
                    // `(_Complex float) zs` 的显式转换 (cast.rs 已把异内型
                    // `_Complex→_Complex` 全部委托到这里) 同样失效。分量标量转换
                    // 一律走 `emit_scalar_to_xmm0_if_needed_ty` (整数→浮点的单一
                    // 真源, 含无符号 64 位的 `emit_ulong_to_float` 臂), 本站点只
                    // 负责“逐分量”这件事本身; LongDouble 目标不在这里拓
                    // (需 x87 侧 `fild` 窄宽度支持, 与现有 LD 臂同一缺口)。
                    let int_cx_src = match (&inner.base, r.ty()) {
                        (Type::Float | Type::Double, Type::Complex(ref c))
                            if Self::is_integer_type(&c.base) =>
                        {
                            Some(c.base.clone())
                        },
                        _ => None,
                    };
                    if let Some(si) = int_cx_src {
                        // 源的 (实部, 虚部) 对由 gen_expr 按 complex rvalue 统一
                        // 约定物化在复数结果带, stride = 源内型宽度。
                        let band_off = self.complex_result_off();
                        self.gen_expr(r, Self::EVAL_BASE);
                        let src_isz = si.size() as i64;
                        let d_isz = inner.base.size() as i64;
                        // 目标地址: staged (全局/运行时地址) 走寄存器间接, 局部
                        // 走 r13 相对帧槽。两臂之间没有 call, 故 rcx 无需入栈
                        // 保护 (`store_complex_at_rcx` 的 push/pop 约定只在跨
                        // 嵌套求值时才必需)。
                        if staged_dst {
                            self.emit_agg_dst_addr(dst, 1); // rcx = 目标地址
                        }
                        for part in 0..2i64 {
                            // 每轮重取物化区基址: load 覆盖 rax。
                            self.emit_lea_r13(0, band_off);
                            self.emit_load_mem_typed_disp(si.size(), &si, part * src_isz);
                            self.emit_scalar_to_xmm0_if_needed_ty(&si, &inner.base);
                            if staged_dst {
                                // movss/movsd [rcx + disp8], xmm0
                                let swidth = if matches!(inner.base, Type::Float) {
                                    crate::codegen::x86_64::opcode::SimdWidth::SS
                                } else {
                                    crate::codegen::x86_64::opcode::SimdWidth::SD
                                };
                                self.emit_insn(&Insn::simd_mov_mem_base(
                                    true,
                                    swidth,
                                    0,
                                    1,
                                    (part * d_isz) as i32,
                                ));
                            } else {
                                self.gen_float_store_at(dst_off + part * d_isz, &inner.base);
                            }
                        }
                        self.finish_complex_assign_value(dst, dst_ty);
                        return;
                    }
                    if dst_float || src_float {
                        if staged_dst {
                            // 不可按帧偏移直存的目标: 分量路径的 gen_float_store_at 是
                            // r13 栈槽相对, 必须先取真实地址再按连续布局写入。
                            if matches!(&inner.base, Type::LongDouble) {
                                // LD 不走 SSE: store_complex_at_rcx 内部单次
                                // 物化（这里不预 gen_expr，否则带副作用
                                // rvalue 被求值两次）。
                                self.emit_agg_dst_addr(dst, 1); // rcx = dst
                                self.push(1);
                                self.store_complex_at_rcx(r, &inner.base);
                            } else {
                                // f/d 与 int 截断臂: 先 gen_expr 再寻址。
                                self.gen_expr(r, Self::EVAL_BASE);
                                self.emit_agg_dst_addr(dst, 1); // rcx = dst
                                                                // [`store_complex_at_rcx`] 的交付约定是「调用方把
                                                                // 目标地址 push 入栈, 内部自行 pop」(见其注释) ——
                                                                // 地址不能只在寄存器里。`gf` 这类 **全局** `_Complex
                                                                // float` 目标经 `emit_agg_dst_addr` 用的是 RIP 相对
                                                                // lea, 不碰栈, 少这一次 push 就让 pop 从rsp 上捞出
                                                                // 一个与目标毫无关系的值当地址, `movss (%rcx)` 直接
                                                                // 段错误 (`gf = gf * 2.0f;` 整程序零输出)。
                                self.push(1);
                                if dst_float {
                                    self.store_complex_at_rcx(r, &inner.base);
                                } else {
                                    // 整数目标 + 浮点源 (C99 6.3.1.7 分量
                                    // 截断): cvttsd2si 后写 [rcx]/[rcx+isz]
                                    let isz = inner.base.size() as i64;
                                    let result_off = self.complex_result_off();
                                    self.pop(1); // rcx = dst addr
                                    if src_complex {
                                        let si = src_inner.clone().unwrap();
                                        let rpfx =
                                            if matches!(si, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                                        self.emit_xmm0_r13_disp(
                                            &[rpfx, 0x41, 0x0F, 0x10],
                                            result_off,
                                        );
                                    }
                                    // cvttsd2si rax, xmm0
                                    self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, 0, 0));
                                    if isz == 4 {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                                rip_relative: false,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(1),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                ),
                                            },
                                            &[],
                                        )); // mov [rcx], eax
                                    } else {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                                rip_relative: false,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(1),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                            },
                                            &[],
                                        )); // mov [rcx], rax
                                    }
                                    if src_complex {
                                        let si = src_inner.clone().unwrap();
                                        self.gen_complex_imag_into_xmm0(r, &si);
                                    } else {
                                        self.emit_insn(&Insn::simd_arith_reg(
                                            SimdArithOp::Xorps,
                                            SimdWidth::PS,
                                            0,
                                            0,
                                        )); // xorps xmm0
                                    }
                                    self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, 0, 0));
                                    if isz == 4 {
                                        self.emit_insn(&Insn::mov_rm_mem(
                                            false,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                            0,
                                            1,
                                            4,
                                        ));
                                    } else {
                                        self.emit_insn(&Insn::mov_rm_mem(
                                            false,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                            0,
                                            1,
                                            isz as i32,
                                        ));
                                    }
                                }
                            }
                        } else if matches!(&inner.base, Type::LongDouble) {
                            // 局部 LD: 不走 SSE，也不能对 complex rvalue 双
                            // 求值。单次物化成帧内一对 tbyte，再 fld/fstp
                            // 搬到 real@dst_off、imag@dst_off+16（窄源由
                            // fld 硬件无损拓宽）。
                            let src = self.gen_complex_ld_materialize(r);
                            self.emit_x87_fld_at(13, src as i32, &inner.base);
                            self.emit_x87_fstp_at(13, dst_off as i32, &inner.base);
                            self.emit_x87_fld_at(13, src as i32 + 16, &inner.base);
                            self.emit_x87_fstp_at(
                                13,
                                (dst_off + inner.size() as i64) as i32,
                                &inner.base,
                            );
                        } else {
                            // 局部 f/d 与局部 int 目标
                            self.gen_expr(r, Self::EVAL_BASE);
                            if dst_float {
                                // Convert integer RHS to float if needed
                                // (e.g., c = 0 where c is _Complex double)
                                self.emit_scalar_to_xmm0_if_needed(r, &inner.base);
                                // 实部宽度适配: 产出端 (binop 等) 按运算 inner
                                // 重载 xmm0 实部, dst inner 更窄 (Float) 时
                                // 直接 movss 取低 32 位会丢实部, 必须先
                                // cvtsd2ss (complex-6 test_float)。
                                if let Type::Complex(ref c) = r.ty() {
                                    self.emit_float_convert(&c.base, &inner.base);
                                }
                                self.gen_float_store_at(dst_off, &inner.base);
                                self.gen_complex_imag_into_xmm0(r, &inner.base);
                                let imag_base = dst_off + inner.size() as i64;
                                self.gen_float_store_at(imag_base, &inner.base);
                            } else {
                                // 整数目标 + 浮点源: 分量 cvttsd2si 截断。
                                // 旧路径按 r 自身 inner 读位模式直写整数槽
                                // → 实部取了 1.0d 低 32 位 0 (complex-6
                                // test_int)。
                                let isz = inner.base.size() as i64;
                                let result_off = self.complex_result_off();
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    let rpfx =
                                        if matches!(si, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                                    self.emit_xmm0_r13_disp(&[rpfx, 0x41, 0x0F, 0x10], result_off);
                                }
                                // cvttsd2si rax, xmm0
                                self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, 0, 0));
                                self.emit_store(0, dst_off, isz as usize);
                                if src_complex {
                                    let si = src_inner.clone().unwrap();
                                    self.gen_complex_imag_into_xmm0(r, &si);
                                } else {
                                    self.emit_insn(&Insn::simd_arith_reg(
                                        SimdArithOp::Xorps,
                                        SimdWidth::PS,
                                        0,
                                        0,
                                    )); // xorps xmm0
                                }
                                self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, 0, 0));
                                self.emit_store(0, dst_off + isz, isz as usize);
                            }
                        }
                        // 赋值表达式值 = 左值转换后的值: 按统一 complex
                        // rvalue 约定物化结果 (960512-1: if (c = f()))
                        self.finish_complex_assign_value(dst, dst_ty);
                        return;
                    }
                }
                // 整数内型 _Complex rvalue (非 Call/Ident): gen_expr 已按
                // 统一约定把结果物化到 result_off (rax=地址), 直接从
                // result_off 整体拷贝到目标; 整数标量 RHS (rax=值约定)
                // 先物化 (值, 0) 复用同一路径。
                if let Type::Complex(inner) = dst_ty {
                    if !matches!(&inner.base, Type::Float | Type::Double | Type::LongDouble) {
                        let result_off = self.complex_result_off();
                        self.gen_expr(r, Self::EVAL_BASE);
                        if !matches!(r.ty(), Type::Complex(_) | Type::Imaginary(_)) {
                            // 标量 RHS: 实部 = rax 值, 虚部清零 (物化为同宽
                            // 布局后走下方同宽整体拷贝)。必须按 inner 宽度
                            // 存储: store_stack 恒写 8 字节, _Complex int 的
                            // 标量 -1 会把 0xFFFFFFFF 同时灌进虚部 (c = -1
                            // 得 (-1,-1) 而非 (-1,0), pr56837)
                            let scalar_isz = inner.base.size();
                            if matches!(r.ty(), Type::Float | Type::Double | Type::LongDouble) {
                                // 浮点标量源: gen_expr 后值在 xmm0,
                                // 先 cvttsd2si 截断为整数 (rax 未定义)
                                self.emit_insn(&Insn::cvt2sd_reg(false, SimdWidth::SD, 0, 0));
                            }
                            self.emit_store(0, result_off, scalar_isz);
                            // P11: 收敛到 AluRm{Xor, B64, rax, rax}.
                            {
                                let insn = Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                        op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                    },
                                    &[],
                                );
                                if !self.emit_insn(&insn) {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                0,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                0,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                        },
                                        &[],
                                    )); // xor rax, rax
                                }
                            }
                            self.emit_store(0, result_off + scalar_isz as i64, scalar_isz);
                        }
                        let dst_isz = inner.size();
                        let src_isz = match r.ty() {
                            Type::Complex(c) | Type::Imaginary(c) => c.size(),
                            _ => dst_isz,
                        };
                        if src_isz == dst_isz {
                            // 同宽: 目标地址 → rdi, 源 result_off → rsi, 整体拷贝
                            self.emit_agg_dst_addr(dst, 7);
                            self.emit_lea_r13(6, result_off);
                            let qwords = sz / 8;
                            let remainder = sz % 8;
                            if qwords > 0 {
                                self.emit_insn(&Insn::mov_reg_imm32_b64(1, qwords as i32))
                                    .expect("typed-opcode: mov_reg_imm32_b64"); // mov rcx, imm32
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsq,
                                )); // rep movsq
                            }
                            if remainder > 0 {
                                let _ =
                                    self.emit_insn(&Insn::mov_reg_imm32_b64(1, remainder as i32));
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                )); // rep movsb
                            }
                        } else {
                            // 跨宽分量转换 (C99 6.3.1.7): 实/虚部分别转换后
                            // 物化到 result_off 同宽布局再拷贝到目标。源 4B→
                            // 目标 8B movsxd 符号扩展 (旧实现整体拷贝 sz 字节:
                            // _Complex int 物化 8B 打包位模式直接当
                            // _Complex long 实部 + 越界读 8B 垃圾当虚部,
                            // x = 1 + 2i 得 (0x200000001, 垃圾),
                            // complex-6 test_long_int)。
                            let d_inner = &inner.base;
                            // 倒序处理 (先虚部后实部): 源物化区在 result_off
                            // 且实部写回 8B 会覆盖源虚部 (_Complex int 源总宽
                            // 8B), 先处理虚部可避免自覆盖。
                            for part in [1usize, 0] {
                                // 每轮重取物化区基址: load 会覆盖 rax,
                                // 第二轮的分量偏移不能依赖上一轮的 rax
                                self.emit_lea_r13(0, result_off);
                                self.emit_load_mem_typed_disp(
                                    src_isz,
                                    d_inner,
                                    (part * src_isz) as i64,
                                );
                                self.store_stack(0, result_off + (part * dst_isz) as i64);
                            }
                            self.emit_agg_dst_addr(dst, 7);
                            self.emit_lea_r13(6, result_off);
                            let qwords = sz / 8;
                            let remainder = sz % 8;
                            if qwords > 0 {
                                self.emit_insn(&Insn::mov_reg_imm32_b64(1, qwords as i32))
                                    .expect("typed-opcode: mov_reg_imm32_b64"); // mov rcx, imm32
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsq,
                                )); // rep movsq
                            }
                            if remainder > 0 {
                                let _ =
                                    self.emit_insn(&Insn::mov_reg_imm32_b64(1, remainder as i32));
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                )); // rep movsb
                            }
                        }
                        // 赋值表达式值 = 左值转换后的值: result_off 已含
                        // 结果, 补齐 rax=地址约定 (960512-1: if (c = f()))
                        self.emit_lea_r13(0, result_off);
                        return;
                    }
                }
                // sret 调用返回: gen_expr 后 rax = 结果缓冲地址 (而非数据),
                // 直接以 rax 为源整体拷贝, 不能把 rax/rdx 当作前 16 字节数据物化.
                //
                // 先剥掉 SSA 重建给聚合右值套上的 `+ 0`：那一条不是算术，而是「把聚合值
                // 拷进目标」的 copy 形态（与 `expr_rebuild.rs:95` 注释里那族 `fn + 0` 同一个
                // 写法）。不剥掉它，下面所有分支看到的都不是「一个内存对象」而是「一个
                // 表达式」，于是走通用路：假定 `gen_expr` 后前 16 字节在 `rax:rdx`。而内存
                // 聚合的求值给出的是**地址**，`%rdx` 是从未装载过的垃圾；临时区又按
                // `dst_off - padded` 算，对 `y = x` 恰好落在**源对象自己身上**，于是先把
                // `x.b` 写成垃圾才开始拷贝 —— N13 的实测形状（`struct S{long a,b;} x={1,2};
                // y=x;` gcc 给 3、dcc 给 201/249 且垃圾值随环境变），见设计 §11.40/§11.42。
                let rhs: &Expr = match r {
                    Expr::Binop {
                        op: BinOp::Add,
                        l,
                        r: zero,
                        ..
                    } if matches!(**zero, Expr::Int { v: 0, .. })
                        && matches!(
                            l.ty(),
                            Type::Struct(..) | Type::Union(..) | Type::Array(..)
                        ) =>
                    {
                        l.as_ref()
                    },
                    _ => r,
                };
                if matches!(rhs, Expr::Call { .. }) && Self::needs_memory_return(&rhs.ty()) {
                    self.gen_expr(rhs, Self::EVAL_BASE);
                    self.mov(6, 0); // rsi = rax (sret 缓冲地址)
                } else if matches!(rhs, Expr::CompoundLit { .. }) {
                    // CompoundLit: gen_expr builds the struct on the stack
                    // and returns the address in rax. Use rax directly as
                    // the source address for the copy.
                    self.gen_expr(rhs, Self::EVAL_BASE);
                    self.mov(6, 0); // rsi = rax (compound lit temp address)
                } else if self.gen_lvalue_addr(rhs, 6) {
                    // 右值本身就是一个内存对象（`y = x` 剥掉 `+0` 后就是 `x`）：
                    // `gen_lvalue_addr` 已经把它的地址发到 rsi（同文件第 73 行用的同一个助手），
                    // 不需要任何物化，也就不会碰到上面那个临时区摞到源对象上的问题。
                } else {
                    // 通用路径: 求值表达式, 存到帧临时区, 然后拷贝
                    self.gen_expr(rhs, Self::EVAL_BASE);
                    // 目标有帧槽时用其上方的安全临时区域; 不可直存的目标
                    // (全局 / 运行时地址) 没有可用帧槽, 现分配一个物化槽
                    // (旧写法统一用 `dst_off - padded`, 对全局即 `0 - padded`,
                    // 会踩到帧底的复数物化带)。
                    let temp_off = match dst.frame_off() {
                        Some(off) => off - ((sz + 7) & !7) as i64, // 在目标下方留临时空间
                        None => self.alloc_sret_slot(sz),
                    };
                    self.store_stack(0, temp_off);
                    if sz > 8 {
                        self.store_stack(2, temp_off + 8); // rdx = 第二部分
                    }
                    self.emit_lea_r13(6, temp_off); // rsi = 源 (reg 6 = rsi)
                }
            }
        }
        // 2. 取目标地址到 rdi
        self.emit_agg_dst_addr(dst, 7);
        // 3. rep movsq + rep movsb 拷贝
        if dst_ty.has_vla() {
            // VM struct/union: 拷贝大小是运行时值 (声明点绑定槽优先,
            // C11 6.2.4p7), 不能用编译期 size() 回退值
            if let Some(slot) = self.vm_size_slot(dst_ty) {
                self.emit_mov_mem_r13(1, slot); // rcx = 绑定大小
            } else {
                self.gen_sizeof_runtime(dst_ty, 11);
                self.mov(1, 11);
            }
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld)); // cld
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // rep movsb
            return;
        }
        let qwords = sz / 8;
        let remainder = sz % 8;
        if qwords > 0 {
            self.emit_insn(&Insn::mov_reg_imm32_b64(1, qwords as i32))
                .expect("typed-opcode: mov_reg_imm32_b64"); // mov rcx, imm32 (qwords)
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsq));
            // rep movsq
        }
        if remainder > 0 {
            self.emit_insn(&Insn::mov_reg_imm32_b64(1, remainder as i32))
                .expect("typed-opcode: mov_reg_imm32_b64"); // mov rcx, imm32 (remainder)
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb));
            // rep movsb
        }
    }
    pub(crate) fn gen_init_list(&mut self, var_name: &str, ty: &Type, inits: &[InitItem]) {
        let var_off = self.var_offset(var_name);

        if matches!(
            ty,
            Type::Array(_, _)
                | Type::Vector(_, _)
                | Type::Struct(_, _, _, _)
                | Type::Union(_, _, _, _)
        ) {
            self.gen_init_list_at(var_off, ty, inits);
        } else if let Some(InitItem::Expr(e)) = inits.first() {
            // 标量类型的单元素初始化 (C23 大括号初始化: double x = {1})
            self.gen_expr(e, Self::EVAL_BASE);
            if matches!(ty, Type::Float | Type::Double | Type::LongDouble | Type::Float128) {
                self.emit_scalar_to_xmm0_if_needed(e, ty);
                if matches!(ty, Type::Float) {
                    self.store_xmm32_stack(0, var_off);
                } else if matches!(ty, Type::Float128) {
                    self.store_xmm128_stack(0, var_off);
                } else {
                    self.store_xmm64_stack(0, var_off);
                }
            } else {
                self.mov_to_stack(0, var_name);
            }
        }
    }
    pub(crate) fn gen_init_list_at(&mut self, base: i64, ty: &Type, inits: &[InitItem]) {
        match ty {
            Type::Array(elem_ty, _) => {
                // C11 6.7.9p14 brace elision: 元素类型是聚合体但
                // 初始值不是花括号包裹时, 把标量值分组到子聚合中
                // 如 int a[2][3] = {1,2,3,4,5,6} → {{1,2,3},{4,5,6}}
                let effective_inits;
                if matches!(
                    &elem_ty.base,
                    Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                ) {
                    // 检查是否需要 brace elision: 首个值是标量 (非 InitList)
                    let needs_elision = inits.iter().any(|item| {
                        matches!(item, InitItem::Expr(e) if !matches!(e, Expr::InitList { .. }))
                    });
                    if needs_elision {
                        effective_inits = Self::group_for_brace_elision(&elem_ty.base, inits);
                    } else {
                        effective_inits = inits.to_vec();
                    }
                } else {
                    effective_inits = inits.to_vec();
                }
                let inits = &effective_inits;
                // 数组初始化: int a[] = {1, 2, 3}
                // 数组在栈上布局: arr[0] 在 base, arr[1] 在 base + elem_size, ...
                // 注意: 虽然栈向下增长，但数组元素地址是递增的
                let elem_size = elem_ty.size();
                // 已初始化覆盖到的最大元素下标 (C 标准: 剩余元素必须零填充)
                let mut covered = 0usize;
                for (i, item) in inits.iter().enumerate() {
                    // GNU 范围设计符 [first ... last]: 范围内每个元素写同一值
                    if let InitItem::Designated {
                        designator: Designator::Range(first, last),
                        value,
                    } = item
                    {
                        let (Ok(f), Ok(l)) = (const_eval::eval(first), const_eval::eval(last))
                        else {
                            continue;
                        };
                        let (f, l) = (f as usize, l as usize);
                        if l < f {
                            continue;
                        }
                        let mut k = f;
                        while k <= l {
                            self.gen_expr(value, Self::EVAL_BASE);
                            if matches!(
                                &elem_ty.base,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) {
                                self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                            }
                            let off = base + (k * elem_size) as i64;
                            if matches!(&elem_ty.base, Type::Float) {
                                self.store_xmm32_stack(0, off);
                            } else if matches!(&elem_ty.base, Type::Double | Type::LongDouble) {
                                self.store_xmm64_stack(0, off);
                            } else if matches!(&elem_ty.base, Type::Float128) {
                                self.store_xmm128_stack(0, off);
                            } else {
                                self.emit_store(0, off, elem_size);
                            }
                            covered = covered.max(k + 1);
                            k += 1;
                        }
                        continue;
                    }
                    let (_idx, value) = match item {
                        InitItem::Expr(e) => (i, e),
                        InitItem::Designated { designator, value } => {
                            // 链式 designator 仍可能作用在数组上 (如 [2].field)
                            if let Some((off, _, _)) = resolve_designator(ty, designator) {
                                if off == i * elem_size {
                                    (i, value)
                                } else {
                                    // 跳位设计符 (如 {[3]=7}): 覆盖到 off 之后, 避免补零误清
                                    covered = covered.max(off / elem_size + 1);
                                    continue;
                                }
                            } else {
                                continue;
                            }
                        },
                    };
                    if let Expr::InitList { inits: nested, .. } = value {
                        // 嵌套数组初始化 (如 int m[2][2] = {{1,2},{3,4}}):
                        // 递归展开子数组 (任意深度), 跳过 gen_expr 对 InitList
                        // 的简化处理; 子数组不足的尾部零填充由递归内完成
                        self.gen_init_list_at(base + (i * elem_size) as i64, &elem_ty.base, nested);
                        covered = covered.max(i + 1);
                        continue;
                    }
                    // C11 6.7.9p14 brace elision 已由循环前的
                    // group_for_brace_elision 预处理, 此处不再需要
                    if let Expr::String { data, .. } = value {
                        // 字符串字面量初始化嵌套子对象 (C11 6.7.9p14):
                        // 2D char 数组行 / struct 数组元素。
                        // gen_expr(String) 返回 rodata 指针, 直接
                        // emit_store 会把指针存入数组/字段 (原有缺陷:
                        // char t[2][8] = {"abc"} 存地址)。
                        // 目标块: 2D 时是一行 (elem_size), elision 传入
                        // 的 1D char 数组是整个 ty (ty.size())
                        let blk = if Self::is_char_array(&elem_ty.base) {
                            Some((base + (i * elem_size) as i64, elem_size))
                        } else if Self::is_char_array(ty) {
                            Some((base, ty.size()))
                        } else {
                            None
                        };
                        if let Some((dst_off, blk_sz)) = blk {
                            self.zero_mem(dst_off, blk_sz);
                            for (j, &b) in data.iter().enumerate() {
                                if j < blk_sz {
                                    self.mov_imm(0, b as i128);
                                    self.emit_store(0, dst_off + j as i64, 1);
                                }
                            }
                            // 字符串字面量填充的是 char 数组的多个元素, 不是 1 个.
                            // data 长度含终止符, blk_sz 是 char 数组总字节数 = 元素数.
                            // 例如 char a[3] = "12" 填充 3 个元素 (1,2,\0),
                            // covered 必须反映已覆盖的元素数, 否则 zero_fill_tail
                            // 会错误清零已写入的字符 (本 bug: '2' 被清零).
                            let filled = data.len().min(blk_sz);
                            covered = covered.max(i + filled);
                            continue;
                        }
                        if matches!(
                            &elem_ty.base,
                            Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                        ) {
                            // brace elision (C11 6.7.9p17): 字符串初始化
                            // 聚合元素 → 递归到聚合分支的字符串字段路径
                            // (整元素零填 + 首个 char 数组字段逐字节拷贝)
                            let sub: Vec<InitItem> = vec![InitItem::Expr(value.clone())];
                            self.gen_init_list_at(
                                base + (i * elem_size) as i64,
                                &elem_ty.base,
                                &sub,
                            );
                            covered = covered.max(i + 1);
                            continue;
                        }
                    }
                    self.gen_expr(value, Self::EVAL_BASE);
                    // 浮点数组元素: int 常量需要转换到 xmm0 (double a[] = {1, 2})
                    if matches!(
                        &elem_ty.base,
                        Type::Float | Type::Double | Type::LongDouble | Type::Float128
                    ) {
                        self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                    }
                    let off = base + (i * elem_size) as i64;
                    if matches!(&elem_ty.base, Type::Float) {
                        self.store_xmm32_stack(0, off);
                    } else if matches!(&elem_ty.base, Type::Double | Type::LongDouble) {
                        self.store_xmm64_stack(0, off);
                    } else if matches!(&elem_ty.base, Type::Float128) {
                        self.store_xmm128_stack(0, off);
                    } else {
                        self.emit_store(0, off, elem_size);
                    }
                    covered = covered.max(i + 1);
                }
                // C 标准: 初始化器不足时剩余元素零填充 (int a[4] = {7,8} → a[2]=a[3]=0)
                self.zero_fill_tail(base, ty, covered);
            },
            Type::Vector(elem_ty, n) => {
                // SIMD 向量初始化: v4si a = {1, 2, 3, 4}

                let elem_size = elem_ty.size();
                let mut covered = 0usize;
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n {
                        break;
                    }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    self.gen_expr(value, Self::EVAL_BASE);
                    self.emit_scalar_to_xmm0_if_needed(value, &elem_ty.base);
                    let off = base + (i * elem_size) as i64;
                    if matches!(elem_ty.base, Type::Float) {
                        self.store_xmm32_stack(0, off);
                    } else if matches!(elem_ty.base, Type::Double) {
                        self.store_xmm64_stack(0, off);
                    } else {
                        self.emit_store(0, off, elem_size);
                    }
                    covered = covered.max(i + 1);
                }
                // C 标准: 初始化器不足时剩余元素零填充
                self.zero_fill_tail(base, ty, covered);
            },
            Type::Struct(_, fields, _, reverse_sso) | Type::Union(_, fields, _, reverse_sso) => {
                // struct 初始化: struct Point p = {1, 2} 或 {.x = 1, .y = 2}
                // 或链式: {.u.x = 1} / {.key.dd_key_true = ...}
                // 支持 C99 brace elision: struct { int f[4]; } s = {1,2,3,4}
                // C99: 未显式初始化的字段应零填充
                let reverse_sso = *reverse_sso;
                let struct_size = ty.size();
                if struct_size > 0 && struct_size <= 128 {
                    // 零填充整个结构 (小结构逐字节, 大结构用 rep stosb)
                    for i in 0..struct_size {
                        self.mov_imm(0, 0i128);
                        self.emit_store(0, base + i as i64, 1);
                    }
                }
                let mut field_idx = 0;
                // 跟踪已清零的位域容器偏移, 避免重复清零
                let mut zeroed_containers: std::collections::HashSet<usize> =
                    std::collections::HashSet::new();
                let mut init_idx = 0;
                while init_idx < inits.len() {
                    let item = &inits[init_idx];
                    // 提取字段信息: (offset, size, type, value, bit_width, bit_offset)
                    let (off, sz, field_ty_opt, value, bit_w, bit_o) = match item {
                        InitItem::Expr(e) => {
                            // 跳过匿名位域 (C11 6.7.9p9: 初始化器不作用于
                            // 无名位域, 后续初始值顺延到下一个具名字段;
                            // 与全局初始化路径 gen_global_init_list 保持一致)
                            while field_idx < fields.len()
                                && fields[field_idx].name.is_empty()
                                && fields[field_idx].bit_width.is_some()
                            {
                                field_idx += 1;
                            }
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                // Brace elision: 如果当前字段是聚合类型 (数组/结构体/联合体)
                                // 但初始化值不是 InitList, 收集剩余标量值作为该字段的初始化器
                                let is_agg_field = matches!(
                                    &f.ty,
                                    Type::Array(_, _)
                                        | Type::Struct(_, _, _, _)
                                        | Type::Union(_, _, _, _)
                                );
                                let is_plain_value = !matches!(e, Expr::InitList { .. });
                                // 字符串 ↔ char 数组字段一对一 (C11 6.7.9p14):
                                // 不能走 brace elision 的连续收集 —
                                // struct { char a[8]; char b[8]; } = {"x","y"}
                                // 会把两个字符串都吞给 a, b 全零。
                                if let (true, Expr::String { data, .. }) =
                                    (Self::is_char_array(&f.ty) && is_plain_value, e)
                                {
                                    let dst = base + f.offset as i64;
                                    self.zero_mem(dst, f.ty.size());
                                    for (j, &b) in data.iter().enumerate() {
                                        if j < f.ty.size() {
                                            self.mov_imm(0, b as i128);
                                            self.emit_store(0, dst + j as i64, 1);
                                        }
                                    }
                                    field_idx += 1;
                                    init_idx += 1;
                                    continue;
                                }
                                // 聚合字段被同类型聚合值初始化 (如 `struct A a = {2, b}` 中 b 是 struct B):
                                // 直接复制整个聚合值, 不走 brace elision (后者将 b 误作标量初始化 b.x)
                                let expr_ty = e.ty();
                                let expr_is_agg = matches!(
                                    &expr_ty,
                                    Type::Struct(_, _, _, _)
                                        | Type::Union(_, _, _, _)
                                        | Type::Array(_, _)
                                );
                                if is_agg_field && is_plain_value && !expr_is_agg {
                                    // 收集从这个字段开始的所有剩余标量 InitItem
                                    let mut collected = Vec::new();
                                    let mut j = init_idx;
                                    while j < inits.len() {
                                        if let InitItem::Expr(expr) = &inits[j] {
                                            if !matches!(expr, Expr::InitList { .. }) {
                                                collected.push(inits[j].clone());
                                                j += 1;
                                            } else {
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    // 用收集的初始化器递归初始化该聚合字段
                                    self.gen_init_list_at(
                                        base + f.offset as i64,
                                        &f.ty,
                                        &collected,
                                    );
                                    field_idx += 1;
                                    init_idx = j;
                                    continue;
                                }
                                field_idx += 1;
                                (
                                    f.offset,
                                    f.ty.size(),
                                    Some(f.ty.clone()),
                                    e,
                                    f.bit_width,
                                    f.bit_offset,
                                )
                            } else {
                                init_idx += 1;
                                continue;
                            }
                        },
                        InitItem::Designated { designator, value } => {
                            // 链式 designator: resolve_designator 沿 .a.b.c 累积偏移
                            let resolved = match resolve_designator(ty, designator) {
                                Some(r) => r,
                                None => {
                                    init_idx += 1;
                                    continue;
                                },
                            };
                            // 查找位域信息 (处理直接字段和 designator 路径)
                            let (bf_w, bf_o) =
                                if let InitItem::Designated { designator: d, .. } = item {
                                    match d {
                                        Designator::Field(fname) => find_field(fields, &fname.name)
                                            .map(|(f, _)| (f.bit_width, f.bit_offset))
                                            .unwrap_or((None, 0)),
                                        _ => (None, 0),
                                    }
                                } else {
                                    (None, 0)
                                };
                            (resolved.0, resolved.1, Some(resolved.2), value, bf_w, bf_o)
                        },
                    };
                    if let Expr::InitList { inits: nested, .. } = value {
                        // 嵌套初始化 (数组/结构体字段, 如 { int a[3] } = {{1,2,3}}):
                        // 递归展开字段初始化器, gen_expr 对 InitList 只输出 ud2+0
                        if let Some(ft) = &field_ty_opt {
                            self.gen_init_list_at(base + off as i64, ft, nested);
                            init_idx += 1;
                            continue;
                        }
                    }
                    let store_off = base + off as i64;
                    // 字符串初始化数组字段 (char c[8] = "val" / wchar_t w[4] = L"ab"):
                    // gen_expr(String) 返回 rodata 指针, 不能 emit_store (会存指针值);
                    // 改为整段清零后逐字节写内容 (C 6.7.9p14: 剩余元素补零).
                    if let (Some(Type::Array(_, _)), Expr::String { data, .. }) =
                        (&field_ty_opt, value)
                    {
                        self.zero_mem(store_off, sz.max(1));
                        for (j, &b) in data.iter().enumerate() {
                            if j < sz {
                                self.mov_imm(0, b as i128);
                                self.emit_store(0, store_off + j as i64, 1);
                            }
                        }
                        continue;
                    }
                    if let Some(bw) = bit_w {
                        // ── 位域字段初始化: 读-改-写 ──
                        // 计算容器大小: 扫描同 offset 的所有位域,
                        // 取 ceil(total_bits/8) 与 max_ty_size 的较大值,
                        // 再对齐到 {1,2,4,8} (下游 load/store switch
                        // 只处理这四种大小).
                        let container_size = {
                            let mut max_ty_size = sz;
                            let mut bits_end = bit_o + bw;
                            let mut fi = field_idx + 1;
                            while fi < fields.len() {
                                let nf = &fields[fi];
                                if nf.bit_width.is_some() && nf.offset == off {
                                    let ts = nf.ty.size();
                                    if ts > max_ty_size {
                                        max_ty_size = ts;
                                    }
                                    let end = nf.bit_offset + nf.bit_width.unwrap();
                                    if end > bits_end {
                                        bits_end = end;
                                    }
                                    fi += 1;
                                } else {
                                    break;
                                }
                            }
                            let bytes_for_container = bits_end.div_ceil(8);
                            let raw = (bytes_for_container as usize).max(max_ty_size);
                            if raw <= 1 {
                                1
                            } else if raw <= 2 {
                                2
                            } else if raw <= 4 {
                                4
                            } else {
                                8
                            }
                        };
                        // 首次使用此容器时先清零整个容器 (确保非本字域的位 = 0)
                        if zeroed_containers.insert(off) {
                            self.mov_imm(0, 0); // rax = 0
                            self.emit_store(0, store_off, container_size);
                        }
                        // 生成初始化值 → rax
                        self.gen_expr(value, Self::EVAL_BASE);
                        // 保存值到栈临时槽: [r13-40]
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            13,
                            -40,
                        )); // mov [r13-40], rax
                            // 用 mov rax, r13 + add rax, imm32 计算地址, 保存到 [r13-24]
                        self.emit_insn(&Insn::mov_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            13,
                        )); // mov rax, r13
                        if store_off != 0 {
                            // add rax, imm32: 48 05 id (accumulator-only form)
                            let gas = format!("add ${}, %rax", store_off);
                            self.emit_insn(&Insn::alu_acc_imm32_b64_acc_gas(
                                0x05,
                                store_off as i32,
                                &gas,
                            ));
                        }
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            13,
                            -24,
                        )); // mov [r13-24], rax
                            // 加载容器值 → rax
                        match container_size {
                            4 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B32,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B32,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(0),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ));
                            }, // mov eax, [rax]
                            8 => {
                                self.emit_insn(&Insn::new(
                                    crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                        rip_relative: false,
                                        width: crate::codegen::shared::asm_operand::Width::B64,
                                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                            0,
                                            crate::codegen::shared::asm_operand::Width::B64,
                                        ),
                                        src: crate::codegen::x86_64::opcode::Operand::Mem(
                                            crate::codegen::x86_64::opcode::MemOperand {
                                                disp: 0,
                                                base: Some(0),
                                                index: None,
                                                seg: None,
                                            },
                                        ),
                                    },
                                    &[],
                                ));
                            }, // mov rax, [rax]
                            2 => {
                                self.emit_insn(&Insn::mov_rm_mem(
                                    true,
                                    crate::codegen::shared::asm_operand::Width::B16,
                                    0,
                                    0,
                                    0,
                                ));
                                // movzwq eax, eax
                                self.emit_insn(&Insn::mov_ext(
                                    false,
                                    crate::codegen::shared::asm_operand::Width::B16,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    0,
                                ));
                            },
                            1 => {
                                self.emit_insn(&Insn::mov_rm_mem(
                                    true,
                                    crate::codegen::shared::asm_operand::Width::B8,
                                    0,
                                    0,
                                    0,
                                ));
                            },
                            _ => {},
                        }
                        // 保存容器值 → [r13-32]
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            13,
                            -32,
                        )); // mov [r13-32], rax
                            // 加载保存的新值 → rax
                        self.emit_insn(&Insn::mov_rm_mem(
                            true,
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            13,
                            -40,
                        )); // mov rax, [r13-40]
                            // 掩码新值: and eax, (1<<bw)-1
                        if bw < 64 {
                            let mask: u64 = (1u64 << bw) - 1;
                            if mask <= 0xFFFFFFFF {
                                // and rax, imm32 (zero-extended): 48 25 id. Preserve
                                // G10 byte-exact emission (REX.W forced by codegen even
                                // though upper 32 bits are already 0).
                                self.emit_insn(&Insn::alu_acc_imm32_b64_acc(
                                    0x25,
                                    mask as u32 as i32,
                                ));
                            } else {
                                self.emit_insn(&Insn::movabs_reg_imm64(2, mask));
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    crate::codegen::x86_64::opcode::AluOp::And,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    0,
                                    2,
                                ));
                            }
                        }
                        // 左移到位置: shl rax, bit_o
                        // 对于 reverse_sso: 反转位域在容器内的位置
                        let effective_bit_o = if reverse_sso {
                            let container_bits = (container_size * 8) as u32;
                            container_bits - bit_o - bw
                        } else {
                            bit_o
                        };
                        if effective_bit_o > 0 {
                            // shl rax, imm8: 48 C1 E0 ib (accumulator /4).
                            self.emit_insn(&Insn::shift_reg_imm8_c1_b64(
                                0b100,
                                0,
                                effective_bit_o as u8,
                            ));
                        }
                        // 加载保存的容器值 → rcx
                        self.emit_insn(&Insn::mov_rm_mem(
                            true,
                            crate::codegen::shared::asm_operand::Width::B64,
                            1,
                            13,
                            -32,
                        )); // mov rcx, [r13-32]
                            // 清除目标位: and rcx, inv_mask
                        let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        let inv_mask = !(mask << effective_bit_o);
                        if (inv_mask as u32 as i32 as i64 as u64) == inv_mask {
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, 1, inv_mask as i64));
                        } else {
                            // movabs rdx, inv_mask; and rdx, rcx → wait, need and rcx, rdx.
                            self.emit_insn(&Insn::movabs_reg_imm64(2, inv_mask));
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::And,
                                crate::codegen::shared::asm_operand::Width::B64,
                                1,
                                2,
                            ));
                        }
                        // 合并: or rcx, rax
                        // P11: 收敛到 AluRm{Or, B64, rcx, rax}.
                        {
                            let insn = Insn::new(
                                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                    op: crate::codegen::x86_64::opcode::AluOp::Or,
                                    width: crate::codegen::shared::asm_operand::Width::B64,
                                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                        1,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                                        0,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    ),
                                },
                                &[],
                            );
                            if !self.emit_insn(&insn) {
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    crate::codegen::x86_64::opcode::AluOp::Or,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    1,
                                    0,
                                ));
                            }
                        }
                        // 保存更新后的容器值回 [r13-32]
                        self.emit_insn(&Insn::mov_rm_mem(
                            false,
                            crate::codegen::shared::asm_operand::Width::B64,
                            1,
                            13,
                            -32,
                        )); // mov [r13-32], rcx
                            // 加载地址 → rdx
                        self.emit_insn(&Insn::mov_rm_mem(
                            true,
                            crate::codegen::shared::asm_operand::Width::B64,
                            2,
                            13,
                            -24,
                        )); // mov rdx, [r13-24]
                            // 存储容器 (如果是最后一个位域且 reverse_sso, 需要先字节序反转)
                            // 检查是否还有后续位域在同一容器
                        let has_more_bitfields_in_container = {
                            let mut fi = field_idx;
                            let mut found_more = false;
                            while fi < fields.len() {
                                let f = &fields[fi];
                                if f.bit_width.is_some() && f.offset == off {
                                    // 这是同一容器的位域
                                    if fi >= field_idx {
                                        found_more = true;
                                        break;
                                    }
                                } else if f.offset != off {
                                    break;
                                }
                                fi += 1;
                            }
                            found_more
                        };
                        if reverse_sso && !has_more_bitfields_in_container {
                            // 最后一个位域: 字节序反转整个容器
                            // 注意: reverse_sso 改变位域在容器中的打包方向,
                            // 不仅仅是字节序反转。完整实现需要修改位域布局逻辑。
                            // 当前实现: 反转容器中的字节序 (近似处理)
                            match container_size {
                                2 => {
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        true,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        1,
                                        13,
                                        -32,
                                    )); // mov rcx, [r13-32]
                                    self.emit_insn(&Insn::rol_reg_imm8_b16(1, 8)); // rol cx, 8
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        false,
                                        crate::codegen::shared::asm_operand::Width::B16,
                                        1,
                                        2,
                                        0,
                                    )); // mov [rdx], cx
                                },
                                4 => {
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        true,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        1,
                                        13,
                                        -32,
                                    )); // mov rcx, [r13-32]
                                    self.emit_insn(&Insn::bswap_reg(
                                        crate::codegen::shared::asm_operand::Width::B32,
                                        1,
                                    )); // bswap ecx
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B32,
                                            dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                1,
                                                crate::codegen::shared::asm_operand::Width::B32,
                                            ),
                                        },
                                        &[],
                                    )); // mov [rdx], ecx
                                },
                                8 => {
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        true,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        1,
                                        13,
                                        -32,
                                    )); // mov rcx, [r13-32]
                                    self.emit_insn(&Insn::bswap_reg(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        1,
                                    )); // bswap rcx
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                1,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                        },
                                        &[],
                                    )); // mov [rdx], rcx
                                },
                                _ => {
                                    // 其他大小: 不反转 (1字节无需反转)
                                    match container_size {
                                        4 => {
                                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
        rip_relative: false,
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(2),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B32
                    ),
                },
                &[],
            ));
                                        }, // mov [rdx], ecx
                                        8 => {
                                            self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
        rip_relative: false,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0,
                            base: Some(2),
                            index: None,
                            seg: None,
                        }
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        1, crate::codegen::shared::asm_operand::Width::B64
                    ),
                },
                &[],
            ));
                                        }, // mov [rdx], rcx
                                        2 => {
                                            self.emit_insn(&Insn::mov_rm_mem(
                                                false,
                                                crate::codegen::shared::asm_operand::Width::B16,
                                                1,
                                                2,
                                                0,
                                            ));
                                        },
                                        1 => {
                                            self.emit_insn(&Insn::mov_rm_mem(
                                                false,
                                                crate::codegen::shared::asm_operand::Width::B8,
                                                1,
                                                2,
                                                0,
                                            ));
                                        },
                                        _ => {},
                                    }
                                },
                            }
                        } else {
                            match container_size {
                                4 => {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B32,
                                            dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                1,
                                                crate::codegen::shared::asm_operand::Width::B32,
                                            ),
                                        },
                                        &[],
                                    ));
                                }, // mov [rdx], ecx
                                8 => {
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Mem(
                                                crate::codegen::x86_64::opcode::MemOperand {
                                                    disp: 0,
                                                    base: Some(2),
                                                    index: None,
                                                    seg: None,
                                                },
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                1,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                        },
                                        &[],
                                    ));
                                }, // mov [rdx], rcx
                                2 => {
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        false,
                                        crate::codegen::shared::asm_operand::Width::B16,
                                        1,
                                        2,
                                        0,
                                    ));
                                },
                                1 => {
                                    self.emit_insn(&Insn::mov_rm_mem(
                                        false,
                                        crate::codegen::shared::asm_operand::Width::B8,
                                        1,
                                        2,
                                        0,
                                    ));
                                },
                                _ => {},
                            }
                        }
                    } else {
                        // 聚合字段被聚合值初始化 (struct/union 复制):
                        // 用 rep movsb 整块复制, 避免 gen_expr 只加载前 8 字节
                        let val_ty = value.ty();
                        let val_is_agg =
                            matches!(&val_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _));
                        if val_is_agg && sz > 0 {
                            let mut src_addr_ok = false;
                            if let Expr::Ident { name: src_name, .. } = value {
                                if let Some(src_off) =
                                    self.var_map.get(&src_name.name).map(|(o, _)| *o)
                                {
                                    // rdi = 目标地址
                                    self.emit_lea_r13(7, store_off); // rdi = dest
                                                                     // rsi = 源地址
                                    self.emit_lea_r13(6, src_off); // rsi = src
                                    self.mov_imm(1, sz as i128); // rcx = count
                                    self.emit_insn(&Insn::noarg(
                                        crate::codegen::x86_64::opcode::NoArgOp::Cld,
                                    )); // cld
                                    self.emit_insn(&Insn::noarg(
                                        crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                    )); // rep movsb
                                    src_addr_ok = true;
                                }
                            } else if matches!(value, Expr::CompoundLit { .. }) {
                                // CompoundLit: gen_expr builds on stack,
                                // rax = address of temp
                                self.gen_expr(value, Self::EVAL_BASE);
                                // rdi = dest, rsi = rax (src)
                                self.emit_lea_r13(7, store_off); // rdi = dest
                                self.mov(6, 0); // rsi = rax
                                self.mov_imm(1, sz as i128); // rcx = count
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::Cld,
                                )); // cld
                                self.emit_insn(&Insn::noarg(
                                    crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                )); // rep movsb
                                src_addr_ok = true;
                            } else if matches!(
                                value,
                                Expr::Member { .. } | Expr::Index { .. } | Expr::Deref { .. }
                            ) {
                                // Lvalue: get address directly
                                if self.gen_lvalue_addr(value, Self::EVAL_BASE) {
                                    self.emit_lea_r13(7, store_off); // rdi = dest
                                    self.mov(6, 0); // rsi = rax
                                    self.mov_imm(1, sz as i128); // rcx = count
                                    self.emit_insn(&Insn::noarg(
                                        crate::codegen::x86_64::opcode::NoArgOp::Cld,
                                    )); // cld
                                    self.emit_insn(&Insn::noarg(
                                        crate::codegen::x86_64::opcode::NoArgOp::RepMovsb,
                                    )); // rep movsb
                                    src_addr_ok = true;
                                }
                            }
                            if src_addr_ok {
                                init_idx += 1;
                                continue;
                            }
                        }
                        // 向量字段 (pr94482: `union U z = { .j = <vector> }`):
                        // Struct/Union 字段已由上方 val_is_agg 块处理并 continue,
                        // 旧路径落到 emit_store, 而它对 >8 字节尺寸不发任何指令
                        // —— 整个 store 静默丢失, 字段保留栈残值, pr94482 因此
                        // abort。改走聚合赋值的单一实现, 源侧物化契约
                        // (Ident/Call/CompoundLit) 由 gen_aggregate_assign_to 统一承担。
                        //
                        // 门槛只看类型不看尺寸：向量无论多宽都是同一套值模型（字节在
                        // 向量临时槽），落到下面 `gen_expr + emit_store` 存的是
                        // `gen_vector_load` 逐元素装载后留在 rax 的**最后一个元素**：
                        // 实测 `union { V(uint8_t,8) v; uint64_t x; } b = { .v = s }` 读
                        // `b.x` 得 8 而非 0x0807060504030201（`struct SV a = { .v = s }`
                        // 同样只写对首字节）。那条 `sz > 8` 是 pr94482 当时只遇到宽向量
                        // 面留的口子，不是语义判据。
                        if matches!(&field_ty_opt, Some(Type::Vector(_, _)))
                            || (sz > 8
                                && matches!(
                                    &field_ty_opt,
                                    Some(ft) if Self::is_aggregate_shape(ft)
                                ))
                        {
                            let addr_slot = self.alloc_sret_slot(8);
                            self.emit_lea_r13(0, store_off);
                            self.store_stack(0, addr_slot); // [addr_slot] = &field
                            let fty = field_ty_opt.clone().unwrap();
                            self.gen_aggregate_assign_to(
                                &AggDst::Indirect { addr_slot },
                                &fty,
                                value,
                            );
                            init_idx += 1;
                            continue;
                        }
                        self.gen_expr(value, Self::EVAL_BASE);
                        if let Some(ft) = &field_ty_opt {
                            if matches!(
                                ft,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) {
                                self.emit_scalar_to_xmm0_if_needed(value, ft);
                                if matches!(ft, Type::Float) {
                                    self.store_xmm32_stack(0, store_off);
                                } else if matches!(ft, Type::Float128) {
                                    self.store_xmm128_stack(0, store_off);
                                } else {
                                    self.store_xmm64_stack(0, store_off);
                                }
                                init_idx += 1;
                                continue;
                            }
                        }
                        // SSO reversed: byte-swap scalar value before writing
                        if reverse_sso && sz > 1 {
                            match sz {
                                2 => {
                                    self.emit_insn(&Insn::rol_reg_imm8_b16(0, 8));
                                    // rol ax, 8
                                },
                                4 => {
                                    self.emit_insn(&Insn::bswap_reg(
                                        crate::codegen::shared::asm_operand::Width::B32,
                                        0,
                                    )); // bswap eax
                                },
                                8 => {
                                    self.emit_insn(&Insn::bswap_reg(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        0,
                                    )); // bswap rax
                                },
                                _ => {},
                            }
                        }
                        self.emit_store(0, store_off, sz);
                    }
                    init_idx += 1;
                }
            },
            _ => {
                // 递归路径下不可达 (elem_ty/field_ty 为标量时 value 不会是
                // InitList); 保险: 按标量单元素取首值写内存
                if let Some(InitItem::Expr(e)) = inits.first() {
                    self.gen_expr(e, Self::EVAL_BASE);
                    if matches!(ty, Type::Float | Type::Double | Type::LongDouble | Type::Float128) {
                        self.emit_scalar_to_xmm0_if_needed(e, ty);
                        if matches!(ty, Type::Float) {
                            self.store_xmm32_stack(0, base);
                        } else if matches!(ty, Type::Float128) {
                            self.store_xmm128_stack(0, base);
                        } else {
                            self.store_xmm64_stack(0, base);
                        }
                    } else {
                        self.emit_store(0, base, ty.size().max(1));
                    }
                }
            },
        }
    }
    pub(crate) fn gen_same_size_bitcast(&mut self, from: &Expr, to: &Type, dst: u8) -> bool {
        let src_ty = from.ty();
        if src_ty.size() != to.size() {
            return false;
        }
        if !matches!(src_ty, Type::Vector(_, _) | Type::Complex(_))
            && !matches!(to, Type::Vector(_, _) | Type::Complex(_))
        {
            return false;
        }
        let slot_off = self.vector_temp_base(2, src_ty.size().max(to.size()));
        self.materialize_bits_to_slot(from, slot_off);
        self.load_bits_from_slot(to, slot_off, dst);
        // 向量 bitcast 后, 结果在 slot 0 (temp slot), 需返回地址给 rax —
        // 否则 Index/Call 等后续消费者拿到垃圾 rax 会段错误 (pr70903).
        if matches!(to, Type::Vector(_, _)) {
            let vec_off = self.vector_temp_base(0, to.size());
            self.emit_lea_r13(0, vec_off);
            if dst != 0 {
                self.mov(dst, 0);
            }
        }
        true
    }
    pub(crate) fn gen_sizeof_runtime(&mut self, ty: &Type, dst: u8) {
        match ty {
            Type::Array(elem, ArraySize::Var(n)) => {
                self.gen_expr(n, dst); // dst = 元素个数 (运行时)
                if Self::type_has_vla(&elem.base) {
                    // 元素本身是 VLA: 递归求值 elem_size 到 rcx, dst *= rcx
                    self.gen_sizeof_runtime(&elem.base, 1);
                    self.imul_reg(dst, 1);
                } else {
                    let es = elem.size() as i128;
                    if es != 1 {
                        self.imul_imm(dst, es);
                    }
                }
            },
            Type::Array(elem, ArraySize::Const(c)) => {
                if Self::type_has_vla(&elem.base) {
                    self.gen_sizeof_runtime(&elem.base, 1); // rcx = elem_size
                    self.mov(dst, 1);
                    self.imul_imm(dst, *c as i128);
                } else {
                    self.mov_imm(dst, ty.size() as i128);
                }
            },
            Type::Struct(_, fields, salign, false) => {
                // VM struct (GCC 扩展 variable-sized struct): VLA 字段
                // 之前的偏移是编译期布局 (parser 不受 VLA fallback 影响),
                // 首个 VLA 字段起转入运行时累计: 字段偏移 =
                // align_up(上一字段末尾, 本字段对齐), 字段大小 =
                // sizeof 运行时值 (VLA 字段) 或静态值。最后圆整到
                // struct 对齐。
                // scratch 与 dst 互斥: dst=r11 时 (聚合拷贝/步长回退)
                // 改用 r10, 否则 add dst,scratch 变自加 (r11+=r11)
                let tmp: u8 = if dst == 11 { 10 } else { 11 };
                let mut acc_open = false;
                for f in fields.iter() {
                    let fa = f.field_align.unwrap_or(f.ty.align()).max(1);
                    if !acc_open {
                        if !f.ty.has_vla() {
                            continue; // 静态前缀: f.offset 编译期正确
                        }
                        // 首个 VLA 字段: dst = f.offset + sizeof(f)
                        self.mov_imm(dst, f.offset as i128);
                        self.gen_sizeof_runtime(&f.ty, tmp);
                        // add dst, tmp: AluRm{Add, B64} reg-reg.
                        self.emit_insn(&Insn::alu_rm_reg_reg(
                            crate::codegen::x86_64::opcode::AluOp::Add,
                            crate::codegen::shared::asm_operand::Width::B64,
                            dst,
                            tmp,
                        ));
                        acc_open = true;
                    } else {
                        // 动态区: dst = align_up(dst, fa) + sizeof(f)
                        if fa > 1 {
                            // add dst, (fa-1); and dst, ~(fa-1): AluRm r64, imm32
                            let m = (fa - 1) as i128;
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, m as i64));
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, dst, (!m as u64) as i64));
                        }
                        if f.ty.has_vla() {
                            self.gen_sizeof_runtime(&f.ty, tmp);
                            // add dst, tmp
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::Add,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                tmp,
                            ));
                        } else {
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, f.ty.size() as i64));
                        }
                    }
                }
                if !acc_open {
                    // 无 VLA 字段 (不应发生, has_vla 才进来): 静态大小
                    self.mov_imm(dst, ty.size() as i128);
                } else if *salign > 1 {
                    let m = (*salign - 1) as i128;
                    // add dst, (salign-1); and dst, ~(salign-1)
                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(0, dst, m as i64));
                    self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, dst, (!m as u64) as i64));
                }
            },
            Type::Union(_, fields, _, false) => {
                // VM union: sizeof = max(字段大小)。静态字段取 size(),
                // VLA 字段运行时求值后取 64 位无符号 max。
                // scratch 与 dst 互斥 (同 Struct 分支)
                let tmp: u8 = if dst == 11 { 10 } else { 11 };
                let mut static_max = 0usize;
                let mut acc_open = false;
                for f in fields.iter() {
                    if f.ty.has_vla() {
                        self.gen_sizeof_runtime(&f.ty, tmp);
                        if acc_open {
                            // dst = max(dst, tmp): cmp dst, tmp; cmova dst, tmp
                            // cmp/cmovcc reg-reg: AluRm{Cmp, B64} + typed cmov.
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::Cmp,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                tmp,
                            ));
                            // cmova dst, tmp: 0F 47 /r.
                            self.emit_insn(&Insn::cmovcc_reg_reg_b64(0x47, dst, tmp));
                        } else {
                            self.mov(dst, tmp);
                            acc_open = true;
                        }
                    } else {
                        static_max = static_max.max(f.ty.size());
                    }
                }
                if acc_open && static_max > 0 {
                    // dst = max(dst, static_max)
                    self.mov_imm(tmp, static_max as i128);
                    self.emit_insn(&Insn::alu_rm_reg_reg(
                        crate::codegen::x86_64::opcode::AluOp::Cmp,
                        crate::codegen::shared::asm_operand::Width::B64,
                        dst,
                        tmp,
                    ));
                    self.emit_insn(&Insn::cmovcc_reg_reg_b64(0x47, dst, tmp));
                } else if !acc_open {
                    self.mov_imm(dst, static_max as i128);
                }
            },
            _ => self.mov_imm(dst, ty.size() as i128),
        }
    }
    pub(crate) fn group_for_brace_elision(elem_ty: &Type, inits: &[InitItem]) -> Vec<InitItem> {
        let sub_count = match elem_ty {
            Type::Array(_, ArraySize::Const(n)) => *n,
            Type::Struct(_, fields, _, false) => {
                // 注意: 首元素是结构体名 (String), 第二个才是字段列表;
                // 匿名位域不可初始化, 不计入字段数
                fields
                    .iter()
                    .filter(|f| !(f.name.is_empty() && f.bit_width.is_some()))
                    .count()
            },
            Type::Union(_, _, _, _) => 1,
            _ => return inits.to_vec(),
        };
        if sub_count == 0 {
            return inits.to_vec();
        }
        let mut result = Vec::new();
        let mut idx = 0;
        while idx < inits.len() {
            if let InitItem::Expr(Expr::InitList { .. }) = &inits[idx] {
                // 已有花括号, 保持原样
                result.push(inits[idx].clone());
                idx += 1;
            } else {
                // 收集 sub_count 个标量值到一个 InitList
                let mut group = Vec::new();
                for _ in 0..sub_count {
                    if idx < inits.len() {
                        if let InitItem::Expr(e) = &inits[idx] {
                            group.push(InitItem::Expr(e.clone()));
                        }
                        idx += 1;
                    }
                }
                let span = Span::point("", 0, 0);
                result.push(InitItem::Expr(Expr::InitList {
                    inits: group,
                    span: span.clone(),
                    ty: elem_ty.clone(),
                }));
            }
        }
        result
    }
    fn zero_fill_tail(&mut self, base_off: i64, arr_ty: &Type, init_count: usize) {
        let (elem_ty, n) = match arr_ty {
            Type::Array(elem_ty, ArraySize::Const(n)) => (elem_ty, *n),
            Type::Vector(elem_ty, n) => (elem_ty, *n),
            _ => return,
        };
        if init_count >= n {
            return;
        }
        let elem_size = elem_ty.size();
        self.mov_imm(0, 0); // rax = 0 (补零源)
        for i in init_count..n {
            self.zero_mem(base_off + (i * elem_size) as i64, elem_size);
        }
    }
    fn zero_mem(&mut self, off: i64, size: usize) {
        let mut o = off;
        let mut rem = size;
        while rem >= 8 {
            self.emit_store(0, o, 8);
            o += 8;
            rem -= 8;
        }
        if rem >= 4 {
            self.emit_store(0, o, 4);
            o += 4;
            rem -= 4;
        }
        if rem >= 2 {
            self.emit_store(0, o, 2);
            o += 2;
            rem -= 2;
        }
        if rem >= 1 {
            self.emit_store(0, o, 1);
        }
    }
}
pub(crate) fn resolve_designator(ty: &Type, d: &Designator) -> Option<(usize, usize, Type)> {
    match d {
        Designator::Field(name) => {
            let fields = match ty {
                Type::Struct(_, fs, _, _) | Type::Union(_, fs, _, false) => fs.as_ref(),
                _ => return None,
            };
            // Use find_field for anonymous struct/union穿透 support
            let (f, extra_off) = find_field(fields, &name.name)?;
            Some((f.offset + extra_off, f.ty.size(), f.ty.clone()))
        },
        Designator::Index(idx_expr) => {
            let elem_ty = match ty {
                Type::Array(e, _) => e.base.clone(),
                _ => return None,
            };
            let idx = match idx_expr {
                Expr::Int { v, .. } if *v >= 0 => *v as usize,
                _ => return None,
            };
            let elem_size = elem_ty.size();
            Some((idx * elem_size, elem_size, elem_ty))
        },
        // GNU 范围设计符 [first ... last]: 覆盖多个元素, 无法返回单个偏移,
        // 由 gen_init_list / gen_global_init_list 的数组分支显式展开。
        Designator::Range(_, _) => None,
        Designator::Chain(items) => {
            let mut cur_ty = ty.clone();
            let mut off = 0usize;
            for (i, sub) in items.iter().enumerate() {
                let (d_off, d_sz, new_ty) = resolve_designator(&cur_ty, sub)?;
                off += d_off;
                if i + 1 < items.len() {
                    // 中间环节使用子类型继续往下走
                    let _ = d_sz;
                    cur_ty = new_ty;
                } else {
                    // 最后一段: 返回 (累计偏移, 大小, 目标类型)
                    return Some((off, d_sz, new_ty));
                }
            }
            None
        },
    }
}
