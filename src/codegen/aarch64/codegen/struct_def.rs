// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use super::BranchKind;
use crate::ast::Field;
use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ElfSym};
use crate::ir::ValueType;
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

pub struct Aarch64Codegen {
    pub(crate) code: Vec<u8>,
    pub(crate) rodata: Vec<u8>,
    pub(crate) entry: u64,
    pub(crate) relocatable: bool,
    pub(crate) symbols: Vec<ElfSym>,
    pub(crate) relocations: Vec<ElfRela>,
    pub(crate) rodata_relocations: Vec<ElfRela>,
    pub(crate) func_sizes: HashMap<String, u64>,
    pub(crate) labels: HashMap<String, u64>,
    /// 当前函数局部变量: name -> 栈偏移
    pub(crate) locals: HashMap<String, i32>,
    pub(crate) global_rodata: HashMap<String, u64>,
    pub(crate) float_refs: Vec<(u64, u64)>,
    pub(crate) sym_index: HashMap<String, u32>,
    pub(crate) stack_bytes: i32,
    pub(crate) frame_bytes: i32,
    /// 帧内保存 lr(x30) 的偏移; -1 = 本函数为叶子, 不保存 (不扰动帧)。
    pub(crate) lr_off: i32,
    pub(crate) emitted_return: bool,
    pub(crate) source_name: String,
    pub(crate) label_serial: u32,
    pub(crate) pending_branches: Vec<(u64, String, BranchKind)>,
    /// 前向条件分支: (位置, 标签, 条件码)
    pub(crate) pending_bcond: Vec<(u64, String, u8)>,
    /// 前向 bl 调用: (指令位置, 目标符号/标签)
    pub(crate) pending_bl_calls: Vec<(u64, String)>,
    /// exec 模式 ADR 占位: (位置, rd, 绝对目标地址)
    pub(crate) pending_adr_exec: Vec<(u64, u8, u64)>,
    pub(crate) extra_sections: Vec<ElfSection>,
    /// COMDAT group definitions
    pub(crate) comdat_groups: Vec<ComdatGroup>,
    /// 当前函数返回类型 (sret/HFA 判定)
    pub(crate) current_return_ty: Option<ValueType>,
    /// AST struct/union 字段表 (HFA 分类)
    pub(crate) aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    /// 当前函数是否为 variadic
    pub(crate) is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移
    /// 0 表示未启用 (变参函数中存 x0..x7 + v0..v7, 每 8 字节一个槽)
    pub(crate) va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 x0..x7)
    pub(crate) va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 v0..v7)
    pub(crate) va_flt_offs: i32,
    /// MTE: 是否启用内存标签扩展
    pub(crate) mte_enabled: bool,
    /// MTE: 当前函数的 tag 栈帧大小
    pub(crate) tag_frame_size: i32,
    /// MTE: 帧内保存入参 x19 (callee-saved) 的槽位偏移, 0 表示未启用
    pub(crate) mte_save_off: i32,
    /// 嵌套 sret: 保存的入参 sret 指针栈偏移 (函数返回 >16B 聚合时有效)
    pub(crate) saved_sret_ptr: Option<i32>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    pub(crate) canary_slot_off: i32,
    /// TLS 全局全局变量集合 (名称 -> 偏移), 用于 load_global_addr_w0 分发 TLS 访问序列
    pub(crate) tls_globals: HashMap<String, u64>,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    pub(crate) raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    pub(crate) raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// dcc 编不出、必须透传给 GAS 的行记录 (TU 级降级判据, 由 asm_source 产出)。
    /// 注意: 这里只记事实, 不在本后端报错也不降级。
    pub(crate) asm_passthrough:
        Vec<crate::codegen::shared::asm_source::AsmPassthroughRecord>,
    /// 透传语句在 .text 内的起点与其 GAS 行 (降级时按位插入文本)。
    pub(crate) asm_passthrough_regions: Vec<(u64, Vec<String>)>,
    /// 编译诊断
    pub(crate) diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    pub(crate) mcount_loc_entries: Vec<(u64, String)>,
    pub(crate) _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    pub(crate) break_continue_stack: Vec<(String, String)>,
    /// GNU asm label 映射: C 名 → asm("label") 覆盖名
    pub(crate) asm_label_map: HashMap<String, String>,
    /// DWARF 行号表条目: (pc, source_line)
    pub(crate) line_entries: Vec<(u64, u32)>,
}
