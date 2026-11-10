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
    /// 非 Ident 输出左值（`*p` / `a[i]` / `s.f`，含 `+r` inout）的写回地址驻足号。
    ///
    /// 地址在模板之前算好并整条 asm 期间持有：写回时重跑 `gen_lvalue_addr` 既会
    /// 二次求值副作用子表达式（`+r"(*bar())` 把 bar() 调两次，990130-1.c），
    /// 也会踩掉还没写回的输出寄存器（求值固定用 %rax/%rcx/%rdx 当暂存）。
    pub(crate) saved_addr_reg: Option<u8>,
    /// 驻足池（rbx/r12/r14/r15）被变量的家与本句 asm 的固定操作数占满时，地址改落
    /// 一个 sret 帧槽（`alloc_sret_slot(8)`）。写回只做 `load_stack` + store，不再求值。
    pub(crate) saved_addr_slot: Option<i64>,
    /// 内存操作数（`"m"` / `"+m"`）的地址暂存号：模板里展开成 `(%reg)`，整条 asm
    /// 期间持着地址。asm 编完（含写回）后必须 `free_callee_saved` 归还，否则同一
    /// 函数里每多一条带内存操作数的 asm，就永久少一个可用的 callee-saved 号。
    pub(crate) mem_addr_reg: Option<u8>,
    /// 该地址 scratch 是**借来**的：发号池四个号全被活值占着时，借一个正持有值的
    /// 号并在借出前 `push`、asm 之后 `pop`（见 `alloc_asm_mem_scratch`）。
    ///
    /// 归还路径必须区分：借来的号不能被 `free_callee_saved`（它本来就没被本句
    /// asm 分配），而要先 `pop` 把活值还回去。
    pub(crate) mem_addr_borrowed: bool,
}

/// `parse_asm_data_expr` 的求值结果:
/// (数值, 符号引用 (symbol, rel_type, addend), 延迟标签差值 (left, right))
pub type AsmDataExprValue = (i64, Option<(String, u32, i64)>, Option<(String, String)>);
