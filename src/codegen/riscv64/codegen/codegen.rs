// src/codegen/riscv64/codegen/codegen.rs - RISC-V 64 代码生成器主结构体
//
// 从 riscv64.rs.bak 提取的主状态容器。所有子模块（emit/stmt, emit/expr/*,
// special/tls, helpers 等）都以 `impl Riscv64Codegen` 的形式为其添加方法。

use crate::ast::{Field, Program};
use crate::codegen::shared::elf::{
    ComdatGroup, ElfRela, ElfSection, ElfSym, STT_TLS,
};
use crate::ir::{Module, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum BranchKind {
    Jal,
    Beqz,
    Bne,
    Beq,
    Bge,
    Blt,
}

/// RVV 向量配置参数
#[derive(Clone, Copy, Debug)]
struct RvvConfig {
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
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub entry: u64,
    pub relocatable: bool,
    pub symbols: Vec<ElfSym>,
    pub relocations: Vec<ElfRela>,
    pub rodata_relocations: Vec<ElfRela>,
    pub func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    pub locals: HashMap<String, i32>,
    pub global_rodata: HashMap<String, u64>,
    pub float_refs: Vec<(u64, u64)>,
    /// (code_pos, rodata_offset) for 64-bit const loads
    pub const64_refs: Vec<(u64, u64)>,
    pub sym_index: HashMap<String, u32>,
    pub stack_bytes: i32,
    pub frame_bytes: i32,
    /// 帧内保存 ra(x1)/fp(s0) 的偏移; -1 = 本函数为叶子，不保存 (不扰动帧)。
    pub ret_addr_off: i32,
    pub emitted_return: bool,
    pub source_name: String,
    pub label_serial: u32,
    pub pending_branches: Vec<(u64, String, BranchKind)>,
    pub extra_sections: Vec<ElfSection>,
    /// COMDAT group definitions
    pub comdat_groups: Vec<ComdatGroup>,
    pub current_return_ty: Option<ValueType>,
    pub aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    pub rvv: RvvConfig,
    /// 当前函数是否为 variadic
    pub is_current_func_variadic: bool,
    /// 寄存器保存区在 frame 内的起始偏移 (a0..a7 + fa0..fa7 顺序存 16 槽)
    pub va_save_base_off: i32,
    /// 下一个 GP va_arg 槽位索引 (0..7, 对应 a0..a7)
    pub va_gp_offs: i32,
    /// 下一个 float va_arg 槽位索引 (0..7, 对应 fa0..fa7)
    pub va_flt_offs: i32,
    /// 文件级 asm 原始文本行 (GlobalAsm)
    pub raw_global_asm_lines: Vec<String>,
    /// 函数级内联 asm 原始文本行: (函数名, 行列表)
    pub raw_func_asm_lines: Vec<(String, Vec<String>)>,
    /// -S 模式标志: true 时保留 asm 文本输出, false 时编码机器码
    pub asm_text_mode: bool,
    /// 编译诊断 (未识别内联汇编指令等)
    pub diagnostics: Vec<crate::diagnostics::Diagnostic>,
    /// Stack protector: 当前函数 canary 槽位偏移, 0 表示未启用
    pub canary_slot_off: i32,
    /// -mrecord-mcount: ftrace call site positions for __mcount_loc section
    pub mcount_loc_entries: Vec<(u64, String)>,
    /// TLS 全局变量集合 (名称 -> 偏移), 用于 load_global_addr_a0 分发 TLS 访问序列
    pub tls_globals: HashMap<String, u64>,
    pub _target: Target,
    /// break/continue 标签栈: (break_label, continue_label)
    pub break_continue_stack: Vec<(String, String)>,
    /// GNU asm label 映射: C 名 → asm("label") 覆盖名
    pub asm_label_map: HashMap<String, String>,
    /// DWARF 行号表条目: (pc, source_line)
    pub line_entries: Vec<(u64, u32)>,
}

impl Riscv64Codegen {
    pub fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: Vec::new(),
            const64_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            ret_addr_off: -1,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_branches: Vec::new(),
            extra_sections: Vec::new(),
            comdat_groups: Vec::new(),
            current_return_ty: None,
            aggregate_fields: HashMap::new(),
            rvv: RvvConfig::default(),
            is_current_func_variadic: false,
            va_save_base_off: 0,
            va_gp_offs: 0,
            va_flt_offs: 0,
            raw_global_asm_lines: Vec::new(),
            raw_func_asm_lines: Vec::new(),
            asm_text_mode: false,
            diagnostics: Vec::new(),
            canary_slot_off: 0,
            mcount_loc_entries: Vec::new(),
            tls_globals: HashMap::new(),
            _target: target,
            break_continue_stack: Vec::new(),
            asm_label_map: HashMap::new(),
            line_entries: Vec::new(),
        }
    }

    pub fn set_source_name(&mut self, name: impl Into<String>) {
        self.source_name = name.into();
    }

    pub fn set_asm_text_mode(&mut self, v: bool) {
        self.asm_text_mode = v;
    }

    /// 解析 GNU asm label: 如果 C 名有对应的 asm 标签则返回 asm 标签名，
    /// 否则返回原始 C 名。用于 ELF 符号名、重定位目标和汇编标签发射。
    pub fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map.get(c_name).map(|s| s.as_str()).unwrap_or(c_name)
    }

    /// 收集 GNU asm label 映射和 TLS 变量访问模型
    pub fn collect_asm_labels(&mut self, prog: &Program) {
        use crate::ast::Decl;
        for decl in &prog.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(ref label) = f.asm_label {
                        self.asm_label_map.insert(f.name.name.clone(), label.clone());
                    }
                }
                Decl::Var(v) => {
                    if let Some(ref label) = v.asm_label {
                        self.asm_label_map.insert(v.name.name.clone(), label.clone());
                    }
                }
                _ => {}
            }
        }
    }

    pub fn push_extra_section(&mut self, sec: ElfSection) {
        self.extra_sections.push(sec);
    }

    pub fn func_frames(&self, module: &Module) -> Vec<crate::debug::FuncFrame> {
        module
            .functions
            .iter()
            .filter_map(|f| {
                self.labels.get(&f.name).and_then(|&pc| {
                    self.func_sizes
                        .get(&f.name)
                        .map(|&sz| crate::debug::FuncFrame::new(pc, sz as u32))
                })
            })
            .collect()
    }

    /// 生成新的唯一标签
    pub fn fresh_label(&mut self, prefix: &str) -> String {
        self.label_serial += 1;
        format!("{}{}", prefix, self.label_serial)
    }

    /// 在当前位置放置标签
    pub fn label(&mut self, name: &str) {
        let pos = self.pos();
        self.labels.insert(name.to_string(), pos);
        // Patch any pending branches to this label
        let pending: Vec<_> = self
            .pending_branches
            .iter()
            .filter(|(_, l, _)| l == name)
            .cloned()
            .collect();
        for (at, _, kind) in pending {
            match kind {
                BranchKind::Jal => {
                    // Placeholder for patching logic
                }
                BranchKind::Beqz => {
                    // Placeholder for patching logic
                }
                BranchKind::Bne | BranchKind::Beq | BranchKind::Bge | BranchKind::Blt => {
                    // Placeholder for patching logic
                }
            }
        }
        self.pending_branches.retain(|(_, l, _)| l != name);
    }

    /// 将立即数加载到 a0 寄存器
    pub fn li_a0(&mut self, imm: i32) {
        self.li_reg(10, imm); // x10 is a0
    }

    /// 将立即数加载到指定寄存器
    pub fn li_reg(&mut self, rd: u8, imm: i32) {
        if (-2048..=2047).contains(&imm) {
            self.emit_addi(rd, 0, imm);
        } else {
            let upper = ((imm as i64 + 0x800) >> 12) as i32;
            let lower = imm - (upper << 12);
            self.emit_lui(rd, upper);
            if lower != 0 {
                self.emit_addi(rd, rd, lower);
            }
        }
    }

    /// 获取当前代码位置
    pub fn pos(&self) -> u64 {
        self.code.len() as u64
    }

    /// 向代码缓冲区发射指令
    pub fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// 发射 addi 指令
    pub fn emit_addi(&mut self, rd: u8, rs1: u8, imm: i32) {
        let insn = (imm & 0xFFF) as u32
            | (rs1 << 15)
            | ((rd & 0x1F) << 7)
            | (0x13 << 12); // OP-IMM opcode
        self.emit(&insn.to_le_bytes());
    }

    /// 发射 lui 指令
    pub fn emit_lui(&mut self, rd: u8, imm: i32) {
        let upper = (imm & 0xFFFFF) as u32;
        let imm_hi = if imm < 0 && (imm & 0x800) != 0 { upper.wrapping_add(1) } else { upper };
        let insn = (imm_hi << 12) | ((rd & 0x1F) << 7) | (0x37 << 20); // LUI opcode
        self.emit(&(insn & 0xFFFFFFFF).to_le_bytes());
    }

    /// 从偏移加载到 a0
    pub fn load_a0(&mut self, off: i32) {
        self.emit_lw(10, 2, off);
    }

    /// 存储 a0 到偏移
    pub fn store_a0(&mut self, off: i32) {
        self.emit_sw(10, 2, off);
    }

    /// 发射 jal 指令
    pub fn j(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_jal_placeholder(1); // ra
        if let Some(&target) = self.labels.get(label) {
            self.patch_jal(pos, target);
        } else {
            self.pending_branches.push((pos, label.to_string(), BranchKind::Jal));
        }
    }

    /// 发射 lw 指令
    pub fn emit_lw(&mut self, rd: u8, rs1: u8, imm: i32) {
        let insn = (imm & 0xFFF) as u32
            | (rs1 << 15)
            | ((rd & 0x1F) << 7)
            | (0x03 << 12); // LW opcode
        self.emit(&insn.to_le_bytes());
    }

    /// 发射 sw 指令
    pub fn emit_sw(&mut self, rs2: u8, rs1: u8, imm: i32) {
        let insn = (imm & 0xFFF) as u32
            | (rs1 << 20)
            | ((rs2 & 0x1F) << 15)
            | (0x03 << 12); // SW opcode
        self.emit(&insn.to_le_bytes());
    }

    /// 发射 jal 占位符
    pub fn emit_jal_placeholder(&mut self, rd: u8) {
        // JAL with zero offset, placeholder for patching
        let insn = ((rd & 0x1F) << 7) | (0x6f << 20); // JAL opcode
        self.emit(&(insn as u32).to_le_bytes());
    }

    /// 检查 jal 偏移是否在范围内 (±1MiB)
    pub fn jal_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    /// 补丁 jal 指令
    pub fn patch_jal(&mut self, pos: u64, target: u64) {
        let diff = target as i64 - (pos + 4) as i64;
        assert!(Self::jal_offset_in_range(diff));
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&self.code[(pos as usize)..(pos as usize + 4)]);
        let mut insn = u32::from_le_bytes(bytes);
        
        let jump_target = (diff >> 2) as u32;
        let bit_31 = ((jump_target & 0x80000000) >> 31) as i32;
        let bit_19_12 = ((jump_target & 0x000FF000) >> 12) as i32;
        let bit_20 = ((jump_target & 0x00100000) >> 20) as i32;
        let bit_9_8 = ((jump_target & 0x00000300) >> 8) as i32;
        let bit_7 = ((jump_target & 0x00000080) >> 7) as i32;
        let bit_6 = ((jump_target & 0x00000040) >> 6) as i32;
        let bit_5_1 = ((jump_target & 0x0000003E) >> 1) as i32;
        
        let new_offset = bit_31 << 31
            | bit_19_12 << 21
            | bit_20 << 20
            | bit_9_8 << 12
            | bit_7 << 11
            | bit_6 << 10
            | bit_5_1 << 15
            | 0x6f << 20; // JAL opcode
        
        self.code[pos as usize..pos as usize + 4].copy_from_slice(&new_offset.to_le_bytes());
    }
}

// ============================================================================
// Module-level utility functions
// ============================================================================

use crate::ast::Decl;

/// 文件作用域 `static` 变量判定。
pub fn var_is_static(prog: &Program, name: &str) -> bool {
    prog.decls.iter().any(|d| match d {
        Decl::Var(v) => v.name.name == name && v.is_static,
        _ => false,
    })
}

/// 递归判定一条 IR 表达式子树里是否含有调用 (Call)。
pub fn expr_has_call(e: &crate::ir::TypedExpr) -> bool {
    if e.kind == crate::ir::ExprKind::Call {
        return true;
    }
    e.children.iter().any(expr_has_call)
}

/// 递归判定一条 IR 语句子树里是否含有调用。
pub fn stmt_has_call(s: &crate::ir::TypedStmt) -> bool {
    s.expressions.iter().any(expr_has_call) || s.nested.iter().any(stmt_has_call)
}
