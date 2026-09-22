// src/codegen/x86_64.rs - x86_64 后端主入口
// 第一性原理：x86_64 指令编码固定，寄存器按 SysV ABI 选择。
// suckless: 直接 emit 字节码，不引入汇编库。

// Backend trait implementation (Phase 3b)
mod backend;

// Re-export submodules for convenience
pub mod abi;
pub mod asm;
pub mod elf_side;
pub mod emit;
pub mod mem;
pub mod parallel;
pub mod special;
pub mod tests;

use crate::ast::{find_field, BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program, Stmt, Type, UnOp};
use crate::codegen::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::elf::{
    self, ComdatGroup, ElfRela, ElfSection, ElfSym, R_X86_64_32, R_X86_64_32S, R_X86_64_64, 
    R_X86_64_PC32, R_X86_64_PLT32, SHF_ALLOC, SHF_GNU_RETAIN, SHF_WRITE, SHN_UNDEF, 
    SHT_PROGBITS, STB_GLOBAL, STB_LOCAL, STB_WEAK, STT_FUNC, STT_GNU_IFUNC, STT_OBJECT, 
    STT_SECTION, STT_TLS, STV_DEFAULT, STV_HIDDEN, STV_PROTECTED,
};
use crate::const_eval::{self, ConstValue};
use crate::diagnostics::{Diagnostic, Span};
use crate::preproc::LineSpan;
use crate::target::Target;
use crate::semantics::{ObjectIdentity, ObjectResolver, StorageClass, StorageInfo, StorageType, storage_type_from_ast};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy)]
enum X8664BranchKind {
    /// x86-64 jump kinds used in control-flow lowering.
    Jmp,
    Jz,
    Jnz,
    Jl,
    Jge,
    Jlth,
    Jgle,
    Jgth,
}

/// x86_64 代码生成器 - 从 IR 翻译成机器码
pub struct Codegen {
    // 代码段和数据段的原始字节码
    code: Vec<u8>,
    rodata: Vec<u8>,
    data: Vec<u8>,
    
    // ELF 相关
    entry: u64,
    relocatable: bool,
    symbols: Vec<ElfSym>,
    relocations: Vec<ElfRela>,
    rodata_relocations: Vec<ElfRela>,
    data_relocations: Vec<ElfRela>,
    
    // 函数信息
    func_sizes: HashMap<String, u64>,
    
    // 标签映射 (位置 -> name)
    pub labels: HashMap<String, u64>,
    
    // 当前函数局部变量
    locals: HashMap<String, i32>,
    
    // 全局 rodata 管理
    global_rodata: HashMap<String, u64>,
    
    // 浮点引用追踪 (用于 .debug_info)
    float_refs: Vec<(u64, u64)>,
    
    // 符号索引 (name -> index)
    sym_index: HashMap<String, u32>,
    
    // 栈帧大小
    stack_bytes: i32,
    frame_bytes: i32,
    
    // 返回标志
    emitted_return: bool,
    
    // 源文件名
    source_name: String,
    
    // 标签序列号 (用于生成唯一标签名)
    label_serial: u32,
    
    // 前向跳转待处理队列
    pending_jumps: Vec<(u64, String, X8664BranchKind)>,
    
    // 条件分支待处理
    pending_bcond: Vec<(u64, String, u8)>,
    
    // TLS 全局变量
    tls_globals: HashMap<String, u64>,
    
    // 当前目标配置
    _target: Target,
    
    // 调试行号信息
    pub line_entries: Vec<(u64, u32)>,
    
    // 并行编译相关
    atomic_counter: AtomicUsize,
    mutex: Mutex<()>,
}

impl Codegen {
    /// 创建新的 x86_64 代码生成器实例
    pub fn new(target: Target, relocatable: bool) -> Self {
        Self {
            code: Vec::new(),
            rodata: Vec::new(),
            data: Vec::new(),
            entry: if relocatable { 0 } else { 0x400000 + 64 + 56 },
            relocatable,
            symbols: Vec::new(),
            relocations: Vec::new(),
            rodata_relocations: Vec::new(),
            data_relocations: Vec::new(),
            func_sizes: HashMap::new(),
            labels: HashMap::new(),
            locals: HashMap::new(),
            global_rodata: HashMap::new(),
            float_refs: Vec::new(),
            sym_index: HashMap::new(),
            stack_bytes: 0,
            frame_bytes: 0,
            emitted_return: false,
            source_name: String::new(),
            label_serial: 0,
            pending_jumps: Vec::new(),
            pending_bcond: Vec::new(),
            tls_globals: HashMap::new(),
            _target: target,
            line_entries: Vec::new(),
            atomic_counter: AtomicUsize::new(0),
            mutex: Mutex::new(()),
        }
    }
    
    /// 设置源文件名
    pub fn set_source_name(&mut self, name: &str) {
        self.source_name = name.to_string();
    }
    
    /// 设置汇编文本输出模式 (-S flag)
    pub fn set_asm_text_mode(&mut self, mode: bool) {
        // TODO: 实现 asm text mode
        let _ = mode;
    }
    
    /// 启动一个新的函数代码生成
    pub fn gen_function(&mut self, func: &crate::ir::Function, ast: Option<&FuncDecl>) {
        // 委托给现有的实现
        self.compile_impl(func, ast);
    }
    
    /// 主要的代码生成入口 (从旧 mod.rs 迁移过来的 compile)
    fn compile_impl(&mut self, module: &crate::ir::Module) {
        // TODO: 这里需要仔细迁移现有 mod.rs 中的核心逻辑
        // 目前先用一个简单的 stub 占位，避免编译失败
        
        let _ = module;
        
        // 临时实现：声明空函数
        println!("x86_64 code generation stub - full implementation to be migrated");
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
