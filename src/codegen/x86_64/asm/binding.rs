// src/codegen/x86_64/asm/binding.rs
//
// 内联 asm 操作数绑定所需的共享数据类型。
//
// 这些定义原生于 monolithic x86_64 后端（commit 28a9063）；模块化迁移时
// `asm/bindings.rs` 被删除（见 asm/mod.rs 历史注释），但恢复 asm 操作数
// 绑定路径（build_asm_operand_map / resolve_asm_template）后它们重新被需要，
// 故按职责归位到 asm/ 下。语义与 monolith 一致（零语义 diff）。
#![allow(unused_imports)]

/// 内联 asm 约束绑定结果
pub struct AsmOpBinding {
    /// 分配到的物理寄存器索引 (rax=0 …)
    /// 对于 @cc 约束: 存储条件码字母的 ASCII 值
    /// 对于段寄存器: 存储段码 (0=ES,1=CS,2=SS,3=DS,4=FS,5=GS)
    /// 对于 MMX: 存储 mm 寄存器索引 (0-7)
    pub(crate) reg: u8,
    /// 是否为输出/读写操作数
    pub(crate) is_output: bool,
    /// 模板替换为内存操作数
    pub(crate) is_mem: bool,
    /// early-clobber (`&`): 分配到的寄存器与所有输入都不重叠
    pub(crate) early_clobber: bool,
    /// 寄存器类型/宽度编码:
    ///   0 = GP (64 位), 32 = XMM float, 64 = XMM double, 128 = XMM full
    ///   200 = x87 st(0), 201 = x87 st(1)
    ///   202 = MMX (reg 字段为 mm 索引)
    ///   203 = 段寄存器 (reg 字段为段码)
    ///   204 = 条件码 (reg 字段为 cc ASCII 字母)
    ///   205-208 = 显式子寄存器 (reg 字段为 GP 索引, 205=8b 206=16b 207=32b 208=64b)
    pub(crate) xmm_width: u8,
    /// For inout (`+r`) operands whose lvalue is a non-Ident expression (e.g.
    /// `*bar()`), `prepare_asm_binding` evaluates the expression once to load
    /// the current value into the bound register.  On writeback, re-running
    /// `gen_lvalue_addr` would re-evaluate side-effecting sub-expressions
    /// (e.g. call `bar()` a second time).  When `Some`, writeback uses this
    /// pre-computed address pointer (held in a callee-saved scratch register
    /// across the asm) instead of recomputing it.
    pub(crate) saved_inout_addr_reg: Option<u8>,
}

/// `parse_asm_data_expr` 的求值结果:
/// (数值, 符号引用 (symbol, rel_type, addend), 延迟标签差值 (left, right))
pub type AsmDataExprValue = (i64, Option<(String, u32, i64)>, Option<(String, String)>);
