// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::shared::elf::{ElfRela, ElfSection, ElfSym};
use super::BranchKind;
use crate::ir::ValueType;
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// ARM AAPCS 后端
pub struct ArmCodegen {
    pub(crate) code: Vec<u8>,
    pub(crate) rodata: Vec<u8>,
    pub(crate) relocatable: bool,
    pub(crate) symbols: Vec<ElfSym>,
    pub(crate) relocations: Vec<ElfRela>,
    pub(crate) rodata_relocations: Vec<ElfRela>,
    pub(crate) func_sizes: HashMap<String, u64>,
    pub(crate) labels: HashMap<String, u64>,
    pub(crate) locals: HashMap<String, i32>,
    pub(crate) global_rodata: HashMap<String, u64>,
    pub(crate) string_pool: HashMap<Vec<u8>, u64>,
    pub(crate) sym_index: HashMap<String, u32>,
    pub(crate) stack_bytes: i32,
    pub(crate) frame_bytes: i32,
    pub(crate) source_name: String,
    pub(crate) extra_sections: Vec<ElfSection>,
    pub(crate) current_return_ty: Option<ValueType>,
    pub(crate) aggregate_fields: HashMap<String, (Arc<Vec<crate::ast::Field>>, bool)>,
    pub(crate) emitted_return: bool,
    pub(crate) label_serial: u32,
    /// 前向 bl 调用: (指令字位置, 目标符号名)
    pub(crate) pending_bl_calls: Vec<(u64, String)>,
    pub(crate) pending_branches: Vec<(u64, String, BranchKind)>,
    pub(crate) break_labels: Vec<String>,
    pub(crate) continue_labels: Vec<String>,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    pub(crate) raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    pub(crate) raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    pub(crate) asm_text_mode: bool,
    /// exec 模式入口地址
    pub(crate) entry: u64,
    /// exec 模式 _start 中 BL main 的指令位置
    pub(crate) start_call_pos: Option<u64>,
    /// 当前函数是否为 variadic
    pub(crate) is_current_func_variadic: bool,
    /// va_save 区在 stack_bytes 中的起始偏移 (plan_locals 阶段预留)
    pub(crate) va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..3, 对应 r0..r3)
    pub(crate) va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..3, 对应 s0..s3 或 d0..d3)
    pub(crate) va_flt_offs: i32,
    /// 浮点字面量池偏移 (rodata 中)
    pub(crate) float_pool: HashMap<u64, u64>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    pub(crate) canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    pub(crate) mcount_loc_entries: Vec<(u64, String)>,
    /// 编译诊断 (未识别内联汇编指令等)
    pub(crate) diagnostics: Vec<crate::diagnostics::Diagnostic>,
    pub(crate) _target: Target,
}
