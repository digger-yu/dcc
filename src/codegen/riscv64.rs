// src/codegen/riscv64.rs - RISC-V 64 后端主入口（完整实现从 .bak 迁移）
// 第一性原理：RISC-V 指令编码固定，寄存器按 SysV ABI 选择。

mod backend;

/// RISC-V 分支/跳转指令种类
#[derive(Clone, Copy)]
enum BranchKind {
    Jal,     // Jump and Link (jal)
    Beqz,    // Branch if Equal to Zero (beqz)
    Bne,     // Branch if Not Equal (bne)  
    Beq,     // Branch if Equal (beq)
    Bge,     // Branch if Greater or Equal (bge)
    Blt,     // Branch if Less Than (blt)
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

use crate::ast::{find_field, BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::elf::{
    self, ComdatGroup, ElfRela, ElfSection, ElfSym, R_RISCV_JAL, SHF_ALLOC, SHF_GNU_RETAIN, 
    SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_OBJECT, 
    STT_TLS, STV_DEFAULT, STV_HIDDEN, STV_PROTECTED,
};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

/// RISC-V 64 代码生成器
pub struct Riscv64Codegen {
    code: Vec<u8>,
    rodata: Vec<u8>,
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    func_sizes: HashMap<String, u64>,
    pub labels: HashMap<String, u64>,
    locals: HashMap<String, i32>,
    global_rodata: HashMap<String, u64>,
    float_refs: Vec<(u64, u64)>,
    const64_refs: Vec<(u64, u64)>,
    sym_index: HashMap<String, u32>,
    stack_bytes: i32,
    frame_bytes: i32,
    ret_addr_off: i32,
    emitted_return: bool,
    source_name: String,
    label_serial: u32,
    pending_branches: Vec<(u64, String, BranchKind)>,
    extra_sections: Vec<ElfSection>,
    comdat_groups: Vec<ComdatGroup>,
    current_return_ty: Option<ValueType>,
    aggregate_fields: HashMap<String, (Arc<Vec<Field>>, bool)>,
    rvv: RvvConfig,
    is_current_func_variadic: bool,
    va_save_base_off: i32,
    va_gp_offs: i32,
    va_flt_offs: i32,
    raw_global_asm_lines: Vec<String>,
    raw_func_asm_lines: Vec<(String, Vec<String>)>,
    asm_text_mode: bool,
    diagnostics: Vec<crate::diagnostics::Diagnostic>,
    canary_slot_off: i32,
    mcount_loc_entries: Vec<(u64, String)>,
    tls_globals: HashMap<String, u64>,
    _target: Target,
    break_continue_stack: Vec<(String, String)>,
    asm_label_map: HashMap<String, String>,
    line_entries: Vec<(u64, u32)>,
}

impl Riscv64Codegen {
    /// 创建新的 RISC-V 64 代码生成器实例
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
    
    /// 设置源文件名
    pub fn set_source_name(&mut self, name: &str) {
        self.source_name = name.to_string();
    }
    
    /// 设置汇编文本输出模式 (-S flag)
    pub fn set_asm_text_mode(&mut self, mode: bool) {
        self.asm_text_mode = mode;
    }
    
    /// 主要的代码生成入口（占位 stub）
    pub fn compile(&mut self, module: &Module) {
        // TODO: 从 .bak 文件迁移实际实现
        let _ = module;
        println!("riscv64: placeholder - full implementation to be migrated from riscv64.rs.bak");
    }
    
    /// 启动一个新的函数代码生成（占位 stub）
    pub fn gen_function(&mut self, func: &Function, ast: Option<&FuncDecl>) {
        let _ = func;
        let _ = ast;
        println!("riscv64: placeholder function generation");
    }
    
    /// 获取生成的机器码
    pub fn code(&self) -> &[u8] {
        &self.code
    }
    
    /// 获取 rodata 内容
    pub fn rodata(&self) -> &[u8] {
        &self.rodata
    }
}
