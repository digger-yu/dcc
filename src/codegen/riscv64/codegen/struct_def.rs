// RISC-V 64 backend - 状态结构体定义.
//
// 与 aarch64 / arm 后端的 struct_def.rs 保持同构: 本文件只承载 codegen 状态
// (结构体字段 + RVV 配置), 构造与 compile/finalize 主干在 pipeline.rs。
// 纯搬迁: 字段顺序、可见性、注释均与原实现一致。
use super::BranchKind;
use crate::ast::Field;
use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ElfSym};
use crate::ir::ValueType;
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// RVV 向量配置参数
#[derive(Clone, Copy, Debug)]
pub(crate) struct RvvConfig {
    vlen: u16,      // 向量寄存器位宽 (128/256/512/1024)
    elen: u16,      // 最大元素位宽
    vtype_val: u32, // vtype 编码值
}

impl Default for RvvConfig {
    fn default() -> Self {
        Self {
            vlen: 128,
            elen: 64,
            vtype_val: 0,
        } // vtype=0: SEW=64, LMUL=1, TA, MA
    }
}

pub struct Riscv64Codegen {
    pub(crate) code: Vec<u8>,
    pub(crate) rodata: Vec<u8>,
    pub(crate) entry: u64,
    pub(crate) relocatable: bool,
    pub(crate) symbols: Vec<ElfSym>,
    pub(crate) relocations: Vec<ElfRela>,
    pub(crate) rodata_relocations: Vec<ElfRela>,
    pub(crate) func_sizes: HashMap<String, u64>,
    /// 迁移期: 写入侧继续用此 HashMap; 读取侧请走 `Self::snapshot_layout()`。
    pub(crate) labels: HashMap<String, u64>,
    pub(crate) locals: HashMap<String, i32>,
    pub(crate) global_rodata: HashMap<String, u64>,
    pub(crate) float_refs: Vec<(u64, u64)>,
    pub(crate) const64_refs: Vec<(u64, u64)>, // (code_pos, rodata_offset) for 64-bit const loads
    pub(crate) sym_index: HashMap<String, u32>,
    pub(crate) stack_bytes: i32,
    pub(crate) frame_bytes: i32,
    /// 帧内保存 ra(x1)/fp(s0) 的偏移; -1 = 本函数为叶子，不保存 (不扰动帧)。
    pub(crate) ret_addr_off: i32,
    pub(crate) emitted_return: bool,
    pub(crate) source_name: String,
    pub(crate) label_serial: u32,
    pub(crate) pending_branches: Vec<(u64, String, BranchKind)>,
    pub(crate) extra_sections: Vec<ElfSection>,
    /// COMDAT group definitions
    pub(crate) comdat_groups: Vec<ComdatGroup>,
    pub(crate) current_return_ty: Option<ValueType>,
    pub(crate) aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    pub(crate) rvv: RvvConfig,
    /// 当前函数是否为 variadic
    pub(crate) is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移 (a0..a7 + fa0..fa7 顺序存 16 槽)
    pub(crate) va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 a0..a7)
    pub(crate) va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 fa0..fa7)
    pub(crate) va_flt_offs: i32,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    pub(crate) raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    pub(crate) raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// dcc 编不出、必须透传给 GAS 的行记录 (TU 级降级判据, 由 asm_source 产出)。
    /// 注意: 这里只记事实, 不在本后端报错也不降级。
    pub(crate) asm_passthrough: Vec<crate::codegen::shared::asm_source::AsmPassthroughRecord>,
    /// 透传语句在 .text 内的起点与其 GAS 行 (降级时按位插入文本)。
    pub(crate) asm_passthrough_regions: Vec<(u64, Vec<String>)>,
    /// 编译诊断
    pub(crate) diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    pub(crate) canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    pub(crate) mcount_loc_entries: Vec<(u64, String)>,
    /// TLS 全局变量集合 (名称 -> 偏移), 用于 load_global_addr_a0 分发 TLS 访问序列
    pub(crate) tls_globals: HashMap<String, u64>,
    pub(crate) _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    pub(crate) break_continue_stack: Vec<(String, String)>,
    /// GNU asm label 映射: C 名 → asm("label") 覆盖名
    pub(crate) asm_label_map: HashMap<String, String>,
    /// DWARF 行号表条目: (pc, source_line)
    pub(crate) line_entries: Vec<(u64, u32)>,
}
